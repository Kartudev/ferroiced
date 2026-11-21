use crate::core::alignment;
use crate::core::text::{Alignment, Shaping};
use crate::core::{Color, Font, Pixels, Point, Rectangle, Transformation};
use crate::graphics::text::cache::{self, Cache};
use crate::graphics::text::editor;
use crate::graphics::text::font_system;
use crate::graphics::text::paragraph;

use rustc_hash::{FxHashMap, FxHashSet};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::hash_map;

#[derive(Debug)]
pub struct Pipeline {
    glyph_cache: GlyphCache,
    cache: RefCell<Cache>,
}

impl Pipeline {
    pub fn new() -> Self {
        Pipeline {
            glyph_cache: GlyphCache::new(),
            cache: RefCell::new(Cache::new()),
        }
    }

    // TODO: Shared engine
    #[allow(dead_code)]
    pub fn load_font(&mut self, bytes: Cow<'static, [u8]>) {
        font_system()
            .write()
            .expect("Write font system")
            .load_font(bytes);

        self.cache = RefCell::new(Cache::new());
    }

    pub fn draw_paragraph(
        &mut self,
        paragraph: &paragraph::Weak,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip: Option<Rectangle>,
        transformation: Transformation,
    ) {
        let Some(paragraph) = paragraph.upgrade() else {
            return;
        };

        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            paragraph.buffer(),
            position,
            color,
            pixels,
            clip,
            transformation,
        );
    }

    pub fn draw_editor(
        &mut self,
        editor: &editor::Weak,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip: Option<Rectangle>,
        transformation: Transformation,
    ) {
        let Some(editor) = editor.upgrade() else {
            return;
        };

        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            editor.buffer(),
            position,
            color,
            pixels,
            clip,
            transformation,
        );
    }

    pub fn draw_cached(
        &mut self,
        content: &str,
        bounds: Rectangle,
        color: Color,
        size: Pixels,
        line_height: Pixels,
        font: Font,
        align_x: Alignment,
        align_y: alignment::Vertical,
        shaping: Shaping,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip: Option<Rectangle>,
        transformation: Transformation,
    ) {
        let line_height = f32::from(line_height);

        let mut font_system = font_system().write().expect("Write font system");
        let font_system = font_system.raw();

        let key = cache::Key {
            bounds: bounds.size(),
            content,
            font,
            size: size.into(),
            line_height,
            shaping,
            align_x,
        };

        let (_, entry) = self.cache.get_mut().allocate(font_system, key);

        let width = entry.min_bounds.width;
        let height = entry.min_bounds.height;

        let x = match align_x {
            Alignment::Default | Alignment::Left | Alignment::Justified => {
                bounds.x
            }
            Alignment::Center => bounds.x - width / 2.0,
            Alignment::Right => bounds.x - width,
        };

        let y = match align_y {
            alignment::Vertical::Top => bounds.y,
            alignment::Vertical::Center => bounds.y - height / 2.0,
            alignment::Vertical::Bottom => bounds.y - height,
        };

        draw(
            font_system,
            &mut self.glyph_cache,
            &entry.buffer,
            Point::new(x, y),
            color,
            pixels,
            clip,
            transformation,
        );
    }

    pub fn draw_raw(
        &mut self,
        buffer: &cosmic_text::Buffer,
        position: Point,
        color: Color,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip: Option<Rectangle>,
        transformation: Transformation,
    ) {
        let mut font_system = font_system().write().expect("Write font system");

        draw(
            font_system.raw(),
            &mut self.glyph_cache,
            buffer,
            position,
            color,
            pixels,
            clip,
            transformation,
        );
    }

    pub fn trim_cache(&mut self) {
        self.cache.get_mut().trim();
        self.glyph_cache.trim();
    }
}

fn draw(
    font_system: &mut cosmic_text::FontSystem,
    glyph_cache: &mut GlyphCache,
    buffer: &cosmic_text::Buffer,
    position: Point,
    color: Color,
    pixels: &mut tiny_skia::PixmapMut<'_>,
    clip: Option<Rectangle>,
    transformation: Transformation,
) {
    let position = position * transformation;

    // The same pixels the clip mask would cover (see `pixel_rect`), so
    // glyphs can be clipped, culled and blended without the mask.
    let clip = clip.map_or(
        [0, 0, pixels.width() as i32, pixels.height() as i32],
        |bounds| {
            crate::engine::pixel_rect(bounds, pixels.width(), pixels.height())
        },
    );

    if clip[0] >= clip[2] || clip[1] >= clip[3] {
        return;
    }

    let mut swash = cosmic_text::SwashCache::new();

    for run in buffer.layout_runs() {
        let line_y =
            (run.line_y * transformation.scale_factor()).round() as i32;

        for glyph in run.glyphs {
            let physical_glyph = glyph.physical(
                (position.x, position.y),
                transformation.scale_factor(),
            );

            if let Some((buffer, placement)) = glyph_cache.allocate(
                physical_glyph.cache_key,
                glyph.color_opt.map(from_color).unwrap_or(color),
                font_system,
                &mut swash,
            ) {
                let opacity = color.a
                    * glyph
                        .color_opt
                        .map(|c| c.a() as f32 / 255.0)
                        .unwrap_or(1.0);

                blend_glyph(
                    pixels,
                    buffer,
                    placement.width as i32,
                    placement.height as i32,
                    physical_glyph.x + placement.left,
                    physical_glyph.y - placement.top + line_y,
                    opacity,
                    clip,
                );
            }
        }
    }
}

/// Draws a premultiplied glyph sprite at (`x`, `y`) with source-over,
/// clipped to `clip` (`[x0, y0, x1, y1]`). This is the arithmetic of
/// tiny-skia's `draw_pixmap` with an identity transform (its f32 pipeline:
/// `Gather` has no low-precision stage), so the pixels are the same; it
/// just avoids building a raster pipeline for every glyph.
#[allow(clippy::too_many_arguments)]
fn blend_glyph(
    pixels: &mut tiny_skia::PixmapMut<'_>,
    sprite: &[u8],
    width: i32,
    height: i32,
    x: i32,
    y: i32,
    opacity: f32,
    clip: [i32; 4],
) {
    // tiny-skia draws nothing for a transparent paint.
    if opacity <= 0.0 {
        return;
    }

    let x0 = x.max(clip[0]);
    let y0 = y.max(clip[1]);
    let x1 = (x + width).min(clip[2]);
    let y1 = (y + height).min(clip[3]);

    if x0 >= x1 || y0 >= y1 {
        return;
    }

    const FACTOR: f32 = 1.0 / 255.0;
    let opacity = opacity.min(1.0);
    let stride = pixels.width() as usize;
    let data = pixels.data_mut();

    for row in y0..y1 {
        let source = ((row - y) * width + (x0 - x)) as usize * 4;
        let target = (row as usize * stride + x0 as usize) * 4;
        let len = (x1 - x0) as usize * 4;

        let source = &sprite[source..source + len];
        let target = &mut data[target..target + len];

        for (src, dst) in source
            .as_chunks::<4>()
            .0
            .iter()
            .zip(target.as_chunks_mut::<4>().0)
        {
            if src[3] == 0 {
                continue;
            }

            let mut s = [
                f32::from(src[0]) * FACTOR,
                f32::from(src[1]) * FACTOR,
                f32::from(src[2]) * FACTOR,
                f32::from(src[3]) * FACTOR,
            ];

            if opacity < 1.0 {
                for channel in &mut s {
                    *channel *= opacity;
                }
            }

            let inverse = 1.0 - s[3];

            for (&s, d) in s.iter().zip(dst.iter_mut()) {
                let value = (f32::from(*d) * FACTOR) * inverse + s;
                *d = (value.clamp(0.0, 1.0) * 255.0).round_ties_even() as u8;
            }
        }
    }
}

fn from_color(color: cosmic_text::Color) -> Color {
    let [r, g, b, a] = color.as_rgba();

    Color::from_rgba8(r, g, b, a as f32 / 255.0)
}

const EMPTY_PLACEMENT: cosmic_text::Placement = cosmic_text::Placement {
    left: 0,
    top: 0,
    width: 0,
    height: 0,
};

#[derive(Debug, Clone, Default)]
struct GlyphCache {
    entries: FxHashMap<
        (cosmic_text::CacheKey, [u8; 3]),
        (Vec<u32>, cosmic_text::Placement),
    >,
    recently_used: FxHashSet<(cosmic_text::CacheKey, [u8; 3])>,
    trim_count: usize,
}

impl GlyphCache {
    const TRIM_INTERVAL: usize = 300;
    const CAPACITY_LIMIT: usize = 16 * 1024;

    fn new() -> Self {
        GlyphCache::default()
    }

    fn allocate(
        &mut self,
        cache_key: cosmic_text::CacheKey,
        color: Color,
        font_system: &mut cosmic_text::FontSystem,
        swash: &mut cosmic_text::SwashCache,
    ) -> Option<(&[u8], cosmic_text::Placement)> {
        let [r, g, b, _a] = color.into_rgba8();
        let key = (cache_key, [r, g, b]);

        if let hash_map::Entry::Vacant(entry) = self.entries.entry(key) {
            // TODO: Outline support
            let Some(image) = swash.get_image_uncached(font_system, cache_key)
            else {
                // Remember glyphs without an image too: otherwise they are
                // rasterized again on every frame.
                let _ = entry.insert((Vec::new(), EMPTY_PLACEMENT));
                let _ = self.recently_used.insert(key);
                return None;
            };

            let glyph_size = image.placement.width as usize
                * image.placement.height as usize;

            if glyph_size == 0 {
                // Same for empty glyphs (spaces).
                let _ = entry.insert((Vec::new(), image.placement));
                let _ = self.recently_used.insert(key);
                return None;
            }

            let mut buffer = vec![0u32; glyph_size];

            match image.content {
                cosmic_text::SwashContent::Mask => {
                    let mut i = 0;

                    // TODO: Blend alpha

                    for _y in 0..image.placement.height {
                        for _x in 0..image.placement.width {
                            buffer[i] = bytemuck::cast(
                                tiny_skia::ColorU8::from_rgba(
                                    b,
                                    g,
                                    r,
                                    image.data[i],
                                )
                                .premultiply(),
                            );

                            i += 1;
                        }
                    }
                }
                cosmic_text::SwashContent::Color => {
                    let mut i = 0;

                    for _y in 0..image.placement.height {
                        for _x in 0..image.placement.width {
                            // TODO: Blend alpha
                            buffer[i >> 2] = bytemuck::cast(
                                tiny_skia::ColorU8::from_rgba(
                                    image.data[i + 2],
                                    image.data[i + 1],
                                    image.data[i],
                                    image.data[i + 3],
                                )
                                .premultiply(),
                            );

                            i += 4;
                        }
                    }
                }
                cosmic_text::SwashContent::SubpixelMask => {
                    // TODO
                }
            }

            let _ = entry.insert((buffer, image.placement));
        }

        let _ = self.recently_used.insert(key);

        self.entries
            .get(&key)
            .filter(|(buffer, _)| !buffer.is_empty())
            .map(|(buffer, placement)| {
                (bytemuck::cast_slice(buffer.as_slice()), *placement)
            })
    }

    pub fn trim(&mut self) {
        if self.trim_count > Self::TRIM_INTERVAL
            || self.recently_used.len() >= Self::CAPACITY_LIMIT
        {
            self.entries
                .retain(|key, _| self.recently_used.contains(key));

            self.recently_used.clear();

            self.entries.shrink_to(Self::CAPACITY_LIMIT);
            self.recently_used.shrink_to(Self::CAPACITY_LIMIT);

            self.trim_count = 0;
        } else {
            self.trim_count += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::{Lcg, reference_mask};

    #[test]
    fn glyph_blending_matches_draw_pixmap() {
        let mut rng = Lcg(7);
        for case in 0..300 {
            let gw = 1 + (rng.next() * 14.0) as u32;
            let gh = 1 + (rng.next() * 14.0) as u32;
            let sprite: Vec<u8> = (0..gw * gh)
                .flat_map(|_| match case % 3 {
                    0 if rng.next() < 0.7 => {
                        let [r, g, b, _] = rng.pixel();
                        [r, g, b, 255]
                    }
                    1 if rng.next() < 0.7 => [0, 0, 0, 0],
                    _ => rng.pixel(),
                })
                .collect();
            let glyph =
                tiny_skia::PixmapRef::from_bytes(&sprite, gw, gh).unwrap();

            // On the canvas: for sprites hanging off its top/left edge
            // tiny-skia also paints one padded row/column past the sprite,
            // which the direct blend does not reproduce.
            let x = (rng.next() * 34.0) as i32;
            let y = (rng.next() * 26.0) as i32;
            let opacity = [1.0, 0.6, 0.2, 0.0][case % 4];
            let clip = Rectangle {
                x: rng.next() * 30.0,
                y: rng.next() * 20.0,
                width: rng.next() * 30.0 + 0.5,
                height: rng.next() * 25.0 + 0.5,
            };

            let mut expected = crate::engine::tests::noise(32, 24, case as u64);
            let mut actual = expected.clone();

            let mask = reference_mask(32, 24, clip);
            expected.draw_pixmap(
                x,
                y,
                glyph,
                &tiny_skia::PixmapPaint {
                    opacity,
                    ..tiny_skia::PixmapPaint::default()
                },
                tiny_skia::Transform::identity(),
                Some(&mask),
            );

            blend_glyph(
                &mut actual.as_mut(),
                &sprite,
                gw as i32,
                gh as i32,
                x,
                y,
                opacity,
                crate::engine::pixel_rect(clip, 32, 24),
            );

            crate::engine::tests::assert_same_pixels(
                &expected,
                &actual,
                &format!(
                    "case {case}: {gw}x{gh} at ({x}, {y}) opacity {opacity} clip {clip:?}"
                ),
            );
        }
    }

    #[test]
    fn empty_glyphs_are_cached() {
        let mut font_system = font_system().write().expect("Write font system");
        let font_system = font_system.raw();
        let mut buffer = cosmic_text::Buffer::new(
            font_system,
            cosmic_text::Metrics::new(16.0, 20.0),
        );
        buffer.set_text(
            font_system,
            "a b  c",
            &cosmic_text::Attrs::new(),
            cosmic_text::Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(font_system, false);

        let mut cache = GlyphCache::new();
        let mut swash = cosmic_text::SwashCache::new();
        let mut keys = FxHashSet::default();
        for run in buffer.layout_runs() {
            for glyph in run.glyphs {
                let key = glyph.physical((0.0, 0.0), 1.0).cache_key;
                let _ = keys.insert(key);
                let _ =
                    cache.allocate(key, Color::WHITE, font_system, &mut swash);
            }
        }

        // Every distinct glyph, spaces included, now has an entry.
        assert_eq!(cache.entries.len(), keys.len());
        assert!(cache.entries.values().any(|(buffer, _)| buffer.is_empty()));
    }
}
