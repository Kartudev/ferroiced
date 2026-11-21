use crate::core::image as raster;
use crate::core::{Rectangle, Size, border};
use crate::graphics;

use rustc_hash::{FxHashMap, FxHashSet};
use std::cell::RefCell;
use std::collections::hash_map;

#[derive(Debug)]
pub struct Pipeline {
    cache: RefCell<Cache>,
}

impl Pipeline {
    pub fn new() -> Self {
        Self {
            cache: RefCell::new(Cache::default()),
        }
    }

    pub fn load(
        &self,
        handle: &raster::Handle,
    ) -> Result<raster::Allocation, raster::Error> {
        let mut cache = self.cache.borrow_mut();
        let image = cache.allocate(handle)?;

        #[allow(unsafe_code)]
        Ok(unsafe {
            raster::allocate(handle, Size::new(image.width(), image.height()))
        })
    }

    pub fn dimensions(&self, handle: &raster::Handle) -> Option<Size<u32>> {
        let mut cache = self.cache.borrow_mut();
        let image = cache.allocate(handle).ok()?;

        Some(Size::new(image.width(), image.height()))
    }

    pub fn draw(
        &mut self,
        handle: &raster::Handle,
        filter_method: raster::FilterMethod,
        bounds: Rectangle,
        clip_bounds: Rectangle,
        border_radius: border::Radius,
        opacity: f32,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        transform: tiny_skia::Transform,
        clip_mask: Option<&tiny_skia::Mask>,
        clip_rect: Option<[i32; 4]>,
    ) {
        let mut cache = self.cache.borrow_mut();

        let Ok(image) = cache.allocate(handle) else {
            return;
        };

        let width_scale = bounds.width / image.width() as f32;
        let height_scale = bounds.height / image.height() as f32;

        // Like `iced_wgpu`, the radius rounds the visible area: the image
        // bounds cut by its clip bounds (the widget bounds).
        let shape = bounds.intersection(&clip_bounds).unwrap_or(bounds);
        let mut radii = <[f32; 4]>::from(border_radius);
        for radius in &mut radii {
            *radius = radius
                .max(0.0)
                .min(shape.width / 2.0)
                .min(shape.height / 2.0);
        }

        let quality = match filter_method {
            raster::FilterMethod::Linear => tiny_skia::FilterQuality::Bilinear,
            raster::FilterMethod::Nearest => tiny_skia::FilterQuality::Nearest,
        };

        // Rounded corners: fill the rounded rectangle with the image as a
        // pattern, like `draw_pixmap` does with a plain rectangle.
        if radii.iter().any(|radius| *radius > 0.0) {
            let path = crate::engine::rounded_rectangle(shape, radii);
            let pattern = tiny_skia::Pattern::new(
                image,
                tiny_skia::SpreadMode::Pad,
                quality,
                opacity,
                tiny_skia::Transform::from_row(
                    width_scale,
                    0.0,
                    0.0,
                    height_scale,
                    bounds.x,
                    bounds.y,
                ),
            );

            pixels.fill_path(
                &path,
                &tiny_skia::Paint {
                    shader: pattern,
                    anti_alias: true,
                    ..Default::default()
                },
                tiny_skia::FillRule::Winding,
                transform,
                clip_mask,
            );

            return;
        }

        let transform = transform.pre_scale(width_scale, height_scale);
        let x = (bounds.x / width_scale) as i32;
        let y = (bounds.y / height_scale) as i32;

        // Fast path (Ferrocord video): an opaque image drawn 1:1, unrotated
        // and fully opaque is a plain row copy, clipped to the same pixel
        // rectangle as the clip mask.
        if opacity >= 1.0
            && clip_mask.is_none() == clip_rect.is_none()
            && transform.kx == 0.0
            && transform.ky == 0.0
            && (transform.sx - 1.0).abs() < 1e-3
            && (transform.sy - 1.0).abs() < 1e-3
        {
            let dx = x as f32 * transform.sx + transform.tx;
            let dy = y as f32 * transform.sy + transform.ty;
            if (dx - dx.round()).abs() < 1e-2 && (dy - dy.round()).abs() < 1e-2
            {
                if let Some(Some(entry)) = cache.entries.get(&handle.id()) {
                    if entry.opaque
                        && blit(
                            entry,
                            dx.round() as i64,
                            dy.round() as i64,
                            pixels,
                            clip_rect,
                        )
                    {
                        return;
                    }
                }
            }
        }

        let Ok(image) = cache.allocate(handle) else {
            return;
        };

        pixels.draw_pixmap(
            x,
            y,
            image,
            &tiny_skia::PixmapPaint {
                quality,
                opacity,
                ..Default::default()
            },
            transform,
            clip_mask,
        );
    }

    pub fn trim_cache(&mut self) {
        self.cache.borrow_mut().trim();
    }
}

#[derive(Debug, Default)]
struct Cache {
    entries: FxHashMap<raster::Id, Option<Entry>>,
    hits: FxHashSet<raster::Id>,
    frame: u64,
}

/// Small images not drawn in the last frame are kept (most recent first)
/// up to this many bytes, so animation frames are not converted again on
/// every loop. Larger images, like video frames, are dropped right away.
const IDLE_BUDGET: usize = 32 * 1024 * 1024;
const MAX_IDLE_ENTRY: usize = 1024 * 1024;
/// Idle images are dropped after this many frames regardless.
const MAX_IDLE_FRAMES: u64 = 600;

impl Cache {
    pub fn allocate(
        &mut self,
        handle: &raster::Handle,
    ) -> Result<tiny_skia::PixmapRef<'_>, raster::Error> {
        let id = handle.id();

        if let hash_map::Entry::Vacant(entry) = self.entries.entry(id) {
            if let raster::Handle::Rgba {
                width,
                height,
                pixels,
                ..
            } = handle
            {
                if *width == 0 || *height == 0 {
                    return Err(raster::Error::Empty);
                }
                let (buffer, opaque) = convert(pixels);
                let _ = entry.insert(Some(Entry {
                    width: *width,
                    height: *height,
                    pixels: buffer,
                    opaque,
                    last_used: self.frame,
                }));
            } else {
                let image = match graphics::image::load(handle) {
                    Ok(image) => image,
                    Err(error) => {
                        let _ = entry.insert(None);

                        return Err(error);
                    }
                };

                if image.width() == 0 || image.height() == 0 {
                    return Err(raster::Error::Empty);
                }

                let (buffer, opaque) = convert(image.as_raw());

                let _ = entry.insert(Some(Entry {
                    width: image.width(),
                    height: image.height(),
                    pixels: buffer,
                    opaque,
                    last_used: self.frame,
                }));
            }
        }

        let _ = self.hits.insert(id);

        Ok(self
            .entries
            .get(&id)
            .unwrap()
            .as_ref()
            .map(|entry| {
                tiny_skia::PixmapRef::from_bytes(
                    bytemuck::cast_slice(&entry.pixels),
                    entry.width,
                    entry.height,
                )
                .expect("Build pixmap from image bytes")
            })
            .expect("Image should be allocated"))
    }

    fn trim(&mut self) {
        self.frame += 1;
        let frame = self.frame;

        for id in &self.hits {
            if let Some(Some(entry)) = self.entries.get_mut(id) {
                entry.last_used = frame;
            }
        }

        let mut idle: Vec<(u64, raster::Id, usize)> = self
            .entries
            .iter()
            .filter(|(id, _)| !self.hits.contains(*id))
            .filter_map(|(id, entry)| {
                let entry = entry.as_ref()?;
                let bytes = entry.pixels.len() * 4;

                (bytes <= MAX_IDLE_ENTRY
                    && frame - entry.last_used <= MAX_IDLE_FRAMES)
                    .then_some((entry.last_used, *id, bytes))
            })
            .collect();

        idle.sort_unstable_by_key(|(last_used, _, _)| {
            std::cmp::Reverse(*last_used)
        });

        let mut budget = IDLE_BUDGET;
        let keep: FxHashSet<raster::Id> = idle
            .into_iter()
            .filter(|(_, _, bytes)| {
                let fits = *bytes <= budget;
                if fits {
                    budget -= bytes;
                }
                fits
            })
            .map(|(_, id, _)| id)
            .collect();

        self.entries
            .retain(|id, _| self.hits.contains(id) || keep.contains(id));
        self.hits.clear();
    }
}

#[derive(Debug)]
struct Entry {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    opaque: bool,
    last_used: u64,
}

/// RGBA bytes to tiny-skia's premultiplied BGRA words. Opaque images (video
/// frames, most photos) skip premultiplication: a pure swizzle the compiler
/// vectorises.
fn convert(rgba: &[u8]) -> (Vec<u32>, bool) {
    let opaque = rgba.chunks_exact(4).all(|p| p[3] == 255);
    let buffer = if opaque {
        rgba.chunks_exact(4)
            .map(|p| u32::from_le_bytes([p[2], p[1], p[0], 255]))
            .collect()
    } else {
        rgba.chunks_exact(4)
            .map(|p| {
                bytemuck::cast(
                    tiny_skia::ColorU8::from_rgba(p[2], p[1], p[0], p[3])
                        .premultiply(),
                )
            })
            .collect()
    };
    (buffer, opaque)
}

/// Copies an opaque entry into `pixels` at (`x`, `y`), clipped to the
/// target. Returns false if the target cannot be viewed as words.
fn blit(
    entry: &Entry,
    x: i64,
    y: i64,
    pixels: &mut tiny_skia::PixmapMut<'_>,
    clip: Option<[i32; 4]>,
) -> bool {
    let (tw, th) = (i64::from(pixels.width()), i64::from(pixels.height()));
    let [cx0, cy0, cx1, cy1] =
        clip.map_or([0, 0, tw, th], |rect| rect.map(i64::from));
    let Ok(dst) = bytemuck::try_cast_slice_mut::<u8, u32>(pixels.data_mut())
    else {
        return false;
    };
    let (w, h) = (i64::from(entry.width), i64::from(entry.height));
    let (x0, x1) = (x.max(cx0).max(0), (x + w).min(cx1).min(tw));
    let (y0, y1) = (y.max(cy0).max(0), (y + h).min(cy1).min(th));
    if x0 >= x1 || y0 >= y1 {
        return true;
    }
    let len = (x1 - x0) as usize;
    for row in y0..y1 {
        let src = ((row - y) * w + (x0 - x)) as usize;
        let d = (row * tw + x0) as usize;
        dst[d..d + len].copy_from_slice(&entry.pixels[src..src + len]);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{Point, border};
    use crate::engine::tests::reference_mask;

    fn opaque(width: u32, height: u32, seed: u8) -> raster::Handle {
        let pixels = (0..width * height)
            .flat_map(|i| [seed, (i % 251) as u8, (i / width % 253) as u8, 255])
            .collect::<Vec<u8>>();
        raster::Handle::from_rgba(width, height, pixels)
    }

    #[test]
    fn clipped_blit_matches_draw_pixmap() {
        let handle = opaque(40, 30, 9);
        // On the canvas (see the note in `text::tests`).
        let bounds =
            Rectangle::new(Point::new(5.0, 4.0), Size::new(40.0, 30.0));
        for clip in [
            Rectangle::new(Point::new(10.3, 2.6), Size::new(20.2, 13.7)),
            Rectangle::new(Point::new(0.0, 0.0), Size::new(64.0, 48.0)),
            Rectangle::new(Point::new(30.5, 20.5), Size::new(100.0, 100.0)),
        ] {
            let mask = reference_mask(64, 48, clip);

            let mut expected = crate::engine::tests::noise(64, 48, 5);
            let mut actual = expected.clone();

            let mut pipeline = Pipeline::new();
            {
                let mut cache = pipeline.cache.borrow_mut();
                let image = cache.allocate(&handle).unwrap();
                expected.draw_pixmap(
                    5,
                    4,
                    image,
                    &tiny_skia::PixmapPaint::default(),
                    tiny_skia::Transform::identity(),
                    Some(&mask),
                );
            }

            pipeline.draw(
                &handle,
                raster::FilterMethod::Nearest,
                bounds,
                bounds,
                border::Radius::default(),
                1.0,
                &mut actual.as_mut(),
                tiny_skia::Transform::identity(),
                Some(&mask),
                Some(crate::engine::pixel_rect(clip, 64, 48)),
            );

            crate::engine::tests::assert_same_pixels(
                &expected,
                &actual,
                &format!("clip {clip:?}"),
            );
        }
    }

    #[test]
    fn small_idle_images_survive_a_trim() {
        let mut cache = Cache::default();
        let small = opaque(32, 32, 1);
        let other = opaque(32, 32, 2);
        let large = opaque(1024, 512, 3);

        for handle in [&small, &other, &large] {
            let _ = cache.allocate(handle);
        }
        cache.trim();

        // Only `small` is drawn in the next frame.
        let _ = cache.allocate(&small);
        cache.trim();

        assert!(cache.entries.contains_key(&small.id()));
        assert!(cache.entries.contains_key(&other.id()), "small idle kept");
        assert!(
            !cache.entries.contains_key(&large.id()),
            "large idle dropped"
        );

        for _ in 0..=MAX_IDLE_FRAMES {
            let _ = cache.allocate(&small);
            cache.trim();
        }
        assert!(
            !cache.entries.contains_key(&other.id()),
            "dropped after the idle limit"
        );
    }
}
