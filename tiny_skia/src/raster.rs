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

        // Fast path (Ferrocord video): an opaque image drawn 1:1, unrotated,
        // fully opaque and unmasked is a plain row copy.
        if opacity >= 1.0
            && clip_mask.is_none()
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
}

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
        self.entries.retain(|key, _| self.hits.contains(key));
        self.hits.clear();
    }
}

#[derive(Debug)]
struct Entry {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
    opaque: bool,
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
) -> bool {
    let (tw, th) = (i64::from(pixels.width()), i64::from(pixels.height()));
    let Ok(dst) = bytemuck::try_cast_slice_mut::<u8, u32>(pixels.data_mut())
    else {
        return false;
    };
    let (w, h) = (i64::from(entry.width), i64::from(entry.height));
    let (x0, x1) = (x.max(0), (x + w).min(tw));
    let (y0, y1) = (y.max(0), (y + h).min(th));
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
