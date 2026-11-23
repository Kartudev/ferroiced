#![allow(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]
pub mod window;

mod engine;
mod layer;
mod primitive;
mod settings;
mod text;

#[cfg(feature = "image")]
mod raster;

#[cfg(feature = "svg")]
mod vector;

#[cfg(feature = "geometry")]
pub mod geometry;

use iced_debug as debug;
pub use iced_graphics as graphics;
pub use iced_graphics::core;

pub use layer::Layer;
pub use primitive::Primitive;
pub use settings::Settings;

#[cfg(feature = "geometry")]
pub use geometry::Geometry;

use crate::core::renderer;
use crate::core::{
    Background, Color, Font, Pixels, Point, Rectangle, Size, Transformation,
};
use crate::engine::Engine;
use crate::graphics::Viewport;
use crate::graphics::compositor;
use crate::graphics::text::{Editor, Paragraph};

/// A [`tiny-skia`] graphics renderer for [`iced`].
///
/// [`tiny-skia`]: https://github.com/RazrFalcon/tiny-skia
/// [`iced`]: https://github.com/iced-rs/iced
#[derive(Debug)]
pub struct Renderer {
    default_font: Font,
    default_text_size: Pixels,
    layers: layer::Stack,
    engine: Engine, // TODO: Shared engine
}

impl Renderer {
    pub fn new(default_font: Font, default_text_size: Pixels) -> Self {
        Self {
            default_font,
            default_text_size,
            layers: layer::Stack::new(),
            engine: Engine::new(),
        }
    }

    pub fn layers(&mut self) -> &[Layer] {
        self.layers.flush();
        self.layers.as_slice()
    }

    pub fn draw(
        &mut self,
        pixels: &mut tiny_skia::PixmapMut<'_>,
        clip_mask: &mut tiny_skia::Mask,
        viewport: &Viewport,
        damage: &[Rectangle],
        background_color: Color,
    ) {
        let scale_factor = viewport.scale_factor();

        self.layers.flush();

        for &damage_bounds in damage {
            let damage_bounds = damage_bounds * scale_factor;

            // Skip clearing what an opaque quad of the bottom layer will
            // cover anyway (a panel background under a scrolled chat, say):
            // those pixels end up the quad's colour whatever was there.
            if !self.covered_by_opaque_quad(damage_bounds, scale_factor, pixels)
            {
                clear(pixels, damage_bounds, background_color);
            }

            for layer in self.layers.iter() {
                let Some(layer_bounds) =
                    damage_bounds.intersection(&(layer.bounds * scale_factor))
                else {
                    continue;
                };

                engine::adjust_clip_mask(clip_mask, layer_bounds);

                if !layer.quads.is_empty() {
                    let render_span = debug::render(debug::Primitive::Quad);
                    for (quad, background) in &layer.quads {
                        self.engine.draw_quad(
                            quad,
                            background,
                            Transformation::scale(scale_factor),
                            pixels,
                            clip_mask,
                            layer_bounds,
                        );
                    }
                    render_span.finish();
                }

                if !layer.primitives.is_empty() {
                    let render_span = debug::render(debug::Primitive::Triangle);

                    for group in &layer.primitives {
                        let Some(group_bounds) = (group.clip_bounds()
                            * scale_factor)
                            .intersection(&layer_bounds)
                        else {
                            continue;
                        };

                        engine::adjust_clip_mask(clip_mask, group_bounds);

                        for primitive in group.as_slice() {
                            self.engine.draw_primitive(
                                primitive,
                                Transformation::scale(scale_factor)
                                    * group.transformation(),
                                pixels,
                                clip_mask,
                                group_bounds,
                            );
                        }

                        engine::adjust_clip_mask(clip_mask, layer_bounds);
                    }

                    render_span.finish();
                }

                if !layer.images.is_empty() {
                    let render_span = debug::render(debug::Primitive::Image);

                    for image in &layer.images {
                        self.engine.draw_image(
                            image,
                            Transformation::scale(scale_factor),
                            pixels,
                            clip_mask,
                            layer_bounds,
                        );
                    }

                    render_span.finish();
                }

                if !layer.text.is_empty() {
                    let render_span = debug::render(debug::Primitive::Image);

                    for group in &layer.text {
                        for text in group.as_slice() {
                            self.engine.draw_text(
                                text,
                                Transformation::scale(scale_factor)
                                    * group.transformation(),
                                pixels,
                                clip_mask,
                                layer_bounds,
                            );
                        }
                    }

                    render_span.finish();
                }
            }
        }

        self.engine.trim();
    }

    /// Whether an opaque, square quad of the bottom layer paints every pixel
    /// of `damage` (physical) itself, so clearing it first is wasted work.
    fn covered_by_opaque_quad(
        &self,
        damage: Rectangle,
        scale_factor: f32,
        pixels: &tiny_skia::PixmapMut<'_>,
    ) -> bool {
        #[cfg(test)]
        if tests::NO_SKIP.get() {
            return false;
        }

        let Some(layer) = self.layers.iter().next() else {
            return false;
        };

        // The layer clips its quads: it must not cut into the damage.
        if !damage.is_within(&(layer.bounds * scale_factor)) {
            return false;
        }

        // The clear never paints outside these pixels (checked in
        // `the_clear_stays_inside_its_pixel_rect`).
        let [x0, y0, x1, y1] =
            engine::pixel_rect(damage, pixels.width(), pixels.height());

        if x0 >= x1 || y0 >= y1 {
            return true;
        }

        let transformation = Transformation::scale(scale_factor);

        layer.quads.iter().any(|(quad, background)| {
            engine::opaque_interior(quad, background, transformation)
                .is_some_and(|[ix0, iy0, ix1, iy1]| {
                    ix0 <= x0 && iy0 <= y0 && ix1 >= x1 && iy1 >= y1
                })
        })
    }
}

impl core::Renderer for Renderer {
    fn start_layer(&mut self, bounds: Rectangle) {
        self.layers.push_clip(bounds);
    }

    fn end_layer(&mut self) {
        self.layers.pop_clip();
    }

    fn start_transformation(&mut self, transformation: Transformation) {
        self.layers.push_transformation(transformation);
    }

    fn end_transformation(&mut self) {
        self.layers.pop_transformation();
    }

    fn fill_quad(
        &mut self,
        quad: renderer::Quad,
        background: impl Into<Background>,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_quad(quad, background.into(), transformation);
    }

    fn reset(&mut self, new_bounds: Rectangle) {
        self.layers.reset(new_bounds);
    }

    fn allocate_image(
        &mut self,
        _handle: &core::image::Handle,
        callback: impl FnOnce(Result<core::image::Allocation, core::image::Error>)
        + Send
        + 'static,
    ) {
        #[cfg(feature = "image")]
        #[allow(unsafe_code)]
        // TODO: Concurrency
        callback(self.engine.raster_pipeline.load(_handle));

        #[cfg(not(feature = "image"))]
        callback(Err(core::image::Error::Unsupported));
    }
}

impl core::text::Renderer for Renderer {
    type Font = Font;
    type Paragraph = Paragraph;
    type Editor = Editor;

    const ICON_FONT: Font = Font::with_name("Iced-Icons");
    const CHECKMARK_ICON: char = '\u{f00c}';
    const ARROW_DOWN_ICON: char = '\u{e800}';
    const ICED_LOGO: char = '\u{e801}';
    const SCROLL_UP_ICON: char = '\u{e802}';
    const SCROLL_DOWN_ICON: char = '\u{e803}';
    const SCROLL_LEFT_ICON: char = '\u{e804}';
    const SCROLL_RIGHT_ICON: char = '\u{e805}';

    fn default_font(&self) -> Self::Font {
        self.default_font
    }

    fn default_size(&self) -> Pixels {
        self.default_text_size
    }

    fn fill_paragraph(
        &mut self,
        text: &Self::Paragraph,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();

        layer.draw_paragraph(
            text,
            position,
            color,
            clip_bounds,
            transformation,
        );
    }

    fn fill_editor(
        &mut self,
        editor: &Self::Editor,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_editor(editor, position, color, clip_bounds, transformation);
    }

    fn fill_text(
        &mut self,
        text: core::Text,
        position: Point,
        color: Color,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text(text, position, color, clip_bounds, transformation);
    }
}

impl graphics::text::Renderer for Renderer {
    fn fill_raw(&mut self, raw: graphics::text::Raw) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_text_raw(raw, transformation);
    }
}

#[cfg(feature = "geometry")]
impl graphics::geometry::Renderer for Renderer {
    type Geometry = Geometry;
    type Frame = geometry::Frame;

    fn new_frame(&self, bounds: Rectangle) -> Self::Frame {
        geometry::Frame::new(bounds)
    }

    fn draw_geometry(&mut self, geometry: Self::Geometry) {
        let (layer, transformation) = self.layers.current_mut();

        match geometry {
            Geometry::Live {
                primitives,
                images,
                text,
                clip_bounds,
            } => {
                layer.draw_primitive_group(
                    primitives,
                    clip_bounds,
                    transformation,
                );

                for image in images {
                    layer.draw_image(image, transformation);
                }

                layer.draw_text_group(text, clip_bounds, transformation);
            }
            Geometry::Cache(cache) => {
                layer.draw_primitive_cache(
                    cache.primitives,
                    cache.clip_bounds,
                    transformation,
                );

                for image in cache.images.iter() {
                    layer.draw_image(image.clone(), transformation);
                }

                layer.draw_text_cache(
                    cache.text,
                    cache.clip_bounds,
                    transformation,
                );
            }
        }
    }
}

impl graphics::mesh::Renderer for Renderer {
    fn draw_mesh(&mut self, _mesh: graphics::Mesh) {
        log::warn!("iced_tiny_skia does not support drawing meshes");
    }

    fn draw_mesh_cache(&mut self, _cache: iced_graphics::mesh::Cache) {
        log::warn!("iced_tiny_skia does not support drawing meshes");
    }
}

#[cfg(feature = "image")]
impl core::image::Renderer for Renderer {
    type Handle = core::image::Handle;

    fn load_image(
        &self,
        handle: &Self::Handle,
    ) -> Result<core::image::Allocation, core::image::Error> {
        self.engine.raster_pipeline.load(handle)
    }

    fn measure_image(
        &self,
        handle: &Self::Handle,
    ) -> Option<crate::core::Size<u32>> {
        self.engine.raster_pipeline.dimensions(handle)
    }

    fn draw_image(
        &mut self,
        image: core::Image,
        bounds: Rectangle,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_raster(image, bounds, clip_bounds, transformation);
    }
}

#[cfg(feature = "svg")]
impl core::svg::Renderer for Renderer {
    fn measure_svg(
        &self,
        handle: &core::svg::Handle,
    ) -> crate::core::Size<u32> {
        self.engine.vector_pipeline.viewport_dimensions(handle)
    }

    fn draw_svg(
        &mut self,
        svg: core::Svg,
        bounds: Rectangle,
        clip_bounds: Rectangle,
    ) {
        let (layer, transformation) = self.layers.current_mut();
        layer.draw_svg(svg, bounds, clip_bounds, transformation);
    }
}

impl compositor::Default for Renderer {
    type Compositor = window::Compositor;
}

impl renderer::Headless for Renderer {
    async fn new(
        default_font: Font,
        default_text_size: Pixels,
        backend: Option<&str>,
    ) -> Option<Self> {
        if backend.is_some_and(|backend| {
            !["tiny-skia", "tiny_skia"].contains(&backend)
        }) {
            return None;
        }

        Some(Self::new(default_font, default_text_size))
    }

    fn name(&self) -> String {
        "tiny-skia".to_owned()
    }

    fn screenshot(
        &mut self,
        size: Size<u32>,
        scale_factor: f32,
        background_color: Color,
    ) -> Vec<u8> {
        let viewport = Viewport::with_physical_size(size, scale_factor);

        window::compositor::screenshot(self, &viewport, background_color)
    }
}

/// Fills `bounds` (physical) with the background colour.
fn clear(
    pixels: &mut tiny_skia::PixmapMut<'_>,
    bounds: Rectangle,
    background_color: Color,
) {
    let path = tiny_skia::PathBuilder::from_rect(
        tiny_skia::Rect::from_xywh(
            bounds.x,
            bounds.y,
            bounds.width,
            bounds.height,
        )
        .expect("Create damage rectangle"),
    );

    pixels.fill_path(
        &path,
        &tiny_skia::Paint {
            shader: tiny_skia::Shader::SolidColor(engine::into_color(
                background_color,
            )),
            anti_alias: false,
            blend_mode: tiny_skia::BlendMode::Source,
            ..Default::default()
        },
        tiny_skia::FillRule::default(),
        tiny_skia::Transform::identity(),
        None,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::renderer::{Quad, Renderer as _};
    use crate::core::{Border, Point};
    use std::cell::Cell;

    thread_local! {
        /// Disables the skipped clear, to compare with stock behaviour.
        pub(super) static NO_SKIP: Cell<bool> = const { Cell::new(false) };
    }

    const BACKGROUND: Color = Color::from_rgb(0.1, 0.2, 0.3);

    /// Draws `quads` (bottom layer) over `canvas`, redrawing only `damage`
    /// (logical) at a 1.25 scale factor.
    fn draw(
        quads: &[(Rectangle, Color, f32)],
        damage: Rectangle,
        mut canvas: tiny_skia::Pixmap,
    ) -> tiny_skia::Pixmap {
        let mut renderer = Renderer::new(Font::DEFAULT, Pixels(16.0));
        let viewport = Viewport::with_physical_size(
            Size::new(canvas.width(), canvas.height()),
            1.25,
        );
        renderer.reset(Rectangle::with_size(viewport.logical_size()));

        for &(bounds, color, radius) in quads {
            renderer.fill_quad(
                Quad {
                    bounds,
                    border: Border {
                        radius: radius.into(),
                        ..Border::default()
                    },
                    ..Quad::default()
                },
                color,
            );
        }

        let mut mask =
            tiny_skia::Mask::new(canvas.width(), canvas.height()).unwrap();
        renderer.draw(
            &mut canvas.as_mut(),
            &mut mask,
            &viewport,
            &[damage],
            BACKGROUND,
        );
        canvas
    }

    #[test]
    fn the_clear_stays_inside_its_pixel_rect() {
        let mut rng = engine::tests::Lcg(5);
        for _ in 0..2000 {
            let bounds = Rectangle {
                x: rng.next() * 60.0 - 5.0,
                y: rng.next() * 50.0 - 5.0,
                width: rng.next() * 30.0 + 0.01,
                height: rng.next() * 25.0 + 0.01,
            };
            let mut pixmap = tiny_skia::Pixmap::new(64, 48).unwrap();
            clear(&mut pixmap.as_mut(), bounds, Color::WHITE);

            let [x0, y0, x1, y1] = engine::pixel_rect(bounds, 64, 48);
            for (i, pixel) in pixmap.pixels().iter().enumerate() {
                let (x, y) = ((i % 64) as i32, (i / 64) as i32);
                if pixel.alpha() != 0 {
                    assert!(
                        x >= x0 && x < x1 && y >= y0 && y < y1,
                        "{bounds:?} paints ({x}, {y}) outside {:?}",
                        [x0, y0, x1, y1]
                    );
                }
            }
        }
    }

    #[test]
    fn skipping_the_clear_changes_no_pixel() {
        let mut rng = engine::tests::Lcg(11);
        let mut skipped = 0;
        for case in 0..400 {
            // Mostly damage inside the opaque quad (the case that skips),
            // sometimes crossing its edges.
            let quad = Rectangle::new(
                Point::new(rng.next() * 10.0, rng.next() * 8.0),
                Size::new(rng.next() * 50.0 + 5.0, rng.next() * 38.0 + 5.0),
            );
            let damage = if case % 3 == 0 {
                // Damage exactly on the quad, like a scrolled panel.
                quad
            } else {
                Rectangle::new(
                    Point::new(
                        quad.x + rng.next() * quad.width * 0.6 - 2.0,
                        quad.y + rng.next() * quad.height * 0.6 - 2.0,
                    ),
                    Size::new(rng.next() * 20.0 + 0.3, rng.next() * 15.0 + 0.3),
                )
            };
            let quads = [
                (
                    quad,
                    Color::from_rgb(rng.next(), rng.next(), rng.next()),
                    if case % 7 == 0 { 4.0 } else { 0.0 },
                ),
                (
                    Rectangle::new(
                        Point::new(10.0, 10.0),
                        Size::new(9.5, 7.25),
                    ),
                    Color::from_rgba(0.9, 0.5, 0.1, 0.5),
                    3.0,
                ),
            ];
            let canvas = engine::tests::noise(80, 60, case);

            NO_SKIP.set(true);
            let stock = draw(&quads, damage, canvas.clone());
            NO_SKIP.set(false);
            let actual = draw(&quads, damage, canvas.clone());

            if stock.data() != canvas.data() && actual.data() == stock.data() {
                skipped += 1;
            }
            engine::tests::assert_same_pixels(
                &stock,
                &actual,
                &format!("quad {quad:?} damage {damage:?}"),
            );
        }
        assert!(skipped > 100, "the skip path ran in only {skipped} cases");
    }
}
