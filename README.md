> **ferroiced** — fork of [iced](https://github.com/iced-rs/iced) maintained for
> [Ferrocord](https://github.com/Dasemu/Ferrocord). It is iced **0.14.1**
> (release commit `df71aa93b`) plus changes to `iced_tiny_skia` only: a fast
> path for opaque raster images (used for in-app video), redraws that only
> touch what can be seen (6-180x less CPU per frame in a chat-like UI, same
> pixels), and two rendering fixes (clipped shadows, rounded raster images). Crate names are unchanged so it can be
> used via `[patch.crates-io]`. Not affiliated with or endorsed by the iced
> project; all credit for iced goes to its authors (MIT, see `LICENSE`).
>
> ### What the fork changes, and what it buys
>
> **Fast path for opaque images** (`30bd32b`, `tiny_skia/src/raster.rs`):
>
> - `Handle::Rgba` pixels are converted directly, skipping `image::load` and
>   its extra copy.
> - Fully opaque images skip premultiplication: a plain RGBA→BGRA swizzle the
>   compiler vectorises, instead of a per-pixel premultiply loop.
> - Opaque images drawn 1:1 (unscaled, unrotated, full opacity, no clip mask)
>   are copied row by row into the target instead of going through
>   `draw_pixmap`.
>
> Measured in Ferrocord's video prototype (Ryzen 9 7940HS, 1920×1080 at 125%
> scaling, worst-case 33 Mbps H.264, release build, full frame rate kept in
> every run; % of one CPU core):
>
> | Scenario (1080p60 fullscreen, 1:1) | stock `iced_tiny_skia` 0.14.1 | ferroiced |
> |---|---|---|
> | App + decoder, hardware decode (D3D11VA) | ~160% | **~108%** |
> | App + decoder, software decode | ~180% | **~121%** |
> | App / renderer thread alone | ~100% | **~50%** |
>
> Output is pixel-identical to stock (checked on Ferrocord's mock
> screenshots). Small images gain little: a 270p15 clip in chat costs ~7% and
> 1080p60 scaled to 400×225 ~30% either way, since the 1:1 row-copy path only
> applies to unscaled draws. To hit it, scale frames to physical pixels
> (logical bounds × scale factor) and draw with `FilterMethod::Nearest`.
>
> **Redraw only what can be seen.** Measured on a Ferrocord-like scene
> (server rail, channel list, 30-message chat, member list, composer;
> 1920×1080 at 125%, Ryzen 9 7940HS, release, CPU time of one
> `Renderer::draw`, mean of 300 frames):
>
> | Frame | before | ferroiced |
> |---|---|---|
> | Cursor blink (3×22 damage) | 1.46 ms | **0.008 ms** |
> | Hover one message | 2.17 ms | **0.08 ms** |
> | GIF frame 300×200 | 3.25 ms | **0.17 ms** |
> | Scrolling the chat | 11.3 ms | **1.13 ms** |
> | Full redraw | 15.6 ms | **2.5 ms** |
> | 540p video in the chat | 7.0 ms | **0.55 ms** |
> | 540p video cut by the chat edge | 10.5 ms | **0.49 ms** |
>
> Every frame of that scene is pixel-identical before and after, and pixel
> tests compare each new path with what tiny-skia draws.
>
> - **Empty glyphs are cached.** The glyph cache never stored glyphs
>   without pixels (spaces), so every space went through swash again on
>   every frame: about 15% of the glyphs and most of the text time.
> - **Quads are only rasterized where they can be seen.** A quad was filled
>   over its whole area and only then cut by the clip mask, so a blinking
>   cursor repainted the full-window backgrounds. Square quads (and rounded
>   ones whose corners lie outside the clip) are cut to the clip, and opaque
>   solid ones are written directly, with only their anti-aliased edges
>   going through tiny-skia.
> - **Glyphs are blended directly**, with the same f32 arithmetic and
>   rounding as `draw_pixmap`, clipped and culled against the clip rectangle
>   instead of the mask (the mask is no longer rebuilt per text).
> - **The clip mask is updated incrementally**: only the previous and the new
>   rectangle are written, instead of clearing the whole window-sized mask
>   several times per frame.
> - **Opaque 1:1 images use the row-copy path when clipped too** (a video cut
>   by a scrollable's edge), since the clip is always a rectangle.
> - **Small images stay cached while idle** (up to 32 MiB, 1 MiB each, 600
>   frames), so animation frames are not converted again on every loop.
>   Large images, like video frames, are still dropped after one idle frame.
> - **Only damaged regions are sent to the window** (`present_with_damage`,
>   honoured by softbuffer on Win32, X11 SHM and Wayland). On Win32 a full
>   1080p present costs 0.86 ms, a cursor-sized one 0.01 ms. A redraw with
>   no damage (an expose) still presents everything; set
>   `FERROICED_FULL_PRESENT=1` to always present the whole window (X11 without
>   a compositor, if exposed areas are not repainted).
>
> One stock tiny-skia quirk is not reproduced: for an image or glyph hanging
> off the top/left edge of the window, tiny-skia also paints one padded
> row/column past its bottom/right edge.
>
> **Bug fixes over stock `iced_tiny_skia` 0.14.1** (both covered by pixel
> tests in `tiny_skia/src/engine.rs`):
>
> - **Shadows no longer darken to black on partial redraws.** Stock iced
>   draws a quad's shadow without the clip mask, so each partial redraw
>   (scrolling, hover; visible on Windows, where softbuffer keeps the back
>   buffer and only the damaged region is repainted) blends the
>   semi-transparent shadow again over pixels outside the damage region until
>   they turn black. The shadow is now clipped to the damage region and only
>   its visible part is computed; it is drawn even when only the shadow (not
>   the quad) is damaged; and damage tracking includes the shadow's extent,
>   so a moving shadowed quad leaves no stale shadow behind.
> - **`image(..).border_radius(..)` works for raster images.** Stock
>   `iced_tiny_skia` ignores it and draws them square. The rounded rectangle
>   is now filled with the image as a pattern (anti-aliased, honours opacity,
>   filter method and the clip mask). Like `iced_wgpu`, the radius rounds the
>   visible area: the image bounds cut by the widget bounds. Images without a
>   radius take the unchanged path, including the fast path above.

<div align="center">

<img src="docs/logo.svg" width="140px" />

# Iced

[![Documentation](https://docs.rs/iced/badge.svg)][documentation]
[![Crates.io](https://img.shields.io/crates/v/iced.svg)](https://crates.io/crates/iced)
[![License](https://img.shields.io/crates/l/iced.svg)](https://github.com/iced-rs/iced/blob/master/LICENSE)
[![Downloads](https://img.shields.io/crates/d/iced.svg)](https://crates.io/crates/iced)
[![Test Status](https://img.shields.io/github/actions/workflow/status/iced-rs/iced/test.yml?branch=master&event=push&label=test)](https://github.com/iced-rs/iced/actions)
[![Discourse](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fdiscourse.iced.rs%2Fsite%2Fstatistics.json&query=%24.users_count&suffix=%20users&label=discourse&color=5e7ce2)](https://discourse.iced.rs/)
[![Discord Server](https://img.shields.io/discord/628993209984614400?label=&labelColor=6A7EC2&logo=discord&logoColor=ffffff&color=7389D8)](https://discord.gg/3xZJ65GAhd)

A cross-platform GUI library for Rust focused on simplicity and type-safety.
Inspired by [Elm].

<a href="https://github.com/squidowl/halloy">
  <img src="https://iced.rs/showcase/halloy.gif" width="460px">
</a>
<a href="https://github.com/hecrj/icebreaker">
  <img src="https://iced.rs/showcase/icebreaker.gif" width="360px">
</a>

</div>

## Features

* Simple, easy-to-use, batteries-included API
* Type-safe, reactive programming model
* [Cross-platform support] (Windows, macOS, Linux, and the Web)
* Responsive layout
* Built-in widgets (including [text inputs], [scrollables], and more!)
* Custom widget support (create your own!)
* [Debug tooling with performance metrics and time traveling]
* First-class support for async actions (use futures!)
* Modular ecosystem split into reusable parts:
  * A [renderer-agnostic native runtime] enabling integration with existing systems
  * Two built-in renderers leveraging [`wgpu`] and [`tiny-skia`]
    * [`iced_wgpu`] supporting Vulkan, Metal and DX12
    * [`iced_tiny_skia`] offering a software alternative as a fallback
  * A [windowing shell]

__Iced is currently experimental software.__ [Take a look at the roadmap] and
[check out the issues].

[Cross-platform support]: https://raw.githubusercontent.com/iced-rs/iced/master/docs/images/todos_desktop.jpg
[text inputs]: https://iced.rs/examples/text_input.mp4
[scrollables]: https://iced.rs/examples/scrollable.mp4
[Debug tooling with performance metrics and time traveling]: https://github.com/user-attachments/assets/2e49695c-0261-4b43-ac2e-8d7da5454c4b
[renderer-agnostic native runtime]: runtime/
[`wgpu`]: https://github.com/gfx-rs/wgpu
[`tiny-skia`]: https://github.com/RazrFalcon/tiny-skia
[`iced_wgpu`]: wgpu/
[`iced_tiny_skia`]: tiny_skia/
[windowing shell]: winit/
[Take a look at the roadmap]: ROADMAP.md
[check out the issues]: https://github.com/iced-rs/iced/issues

## Overview

Inspired by [The Elm Architecture], Iced expects you to split user interfaces
into four different concepts:

* __State__ — the state of your application
* __Messages__ — user interactions or meaningful events that you care
  about
* __View logic__ — a way to display your __state__ as widgets that
  may produce __messages__ on user interaction
* __Update logic__ — a way to react to __messages__ and update your
  __state__

We can build something to see how this works! Let's say we want a simple counter
that can be incremented and decremented using two buttons.

We start by modelling the __state__ of our application:

```rust
#[derive(Default)]
struct Counter {
    value: i32,
}
```

Next, we need to define the possible user interactions of our counter:
the button presses. These interactions are our __messages__:

```rust
#[derive(Debug, Clone, Copy)]
pub enum Message {
    Increment,
    Decrement,
}
```

Now, let's show the actual counter by putting it all together in our
__view logic__:

```rust
use iced::widget::{button, column, text, Column};

impl Counter {
    pub fn view(&self) -> Column<Message> {
        // We use a column: a simple vertical layout
        column![
            // The increment button. We tell it to produce an
            // `Increment` message when pressed
            button("+").on_press(Message::Increment),

            // We show the value of the counter here
            text(self.value).size(50),

            // The decrement button. We tell it to produce a
            // `Decrement` message when pressed
            button("-").on_press(Message::Decrement),
        ]
    }
}
```

Finally, we need to be able to react to any produced __messages__ and change our
__state__ accordingly in our __update logic__:

```rust
impl Counter {
    // ...

    pub fn update(&mut self, message: Message) {
        match message {
            Message::Increment => {
                self.value += 1;
            }
            Message::Decrement => {
                self.value -= 1;
            }
        }
    }
}
```

And that's everything! We just wrote a whole user interface. Let's run it:

```rust
fn main() -> iced::Result {
    iced::run(Counter::update, Counter::view)
}
```

Iced will automatically:

  1. Take the result of our __view logic__ and layout its widgets.
  1. Process events from our system and produce __messages__ for our
     __update logic__.
  1. Draw the resulting user interface.

Read the [book], the [documentation], and the [examples] to learn more!

## Implementation details

Iced was originally born as an attempt at bringing the simplicity of [Elm] and
[The Elm Architecture] into [Coffee], a 2D game library I am working on.

The core of the library was implemented during May 2019 in [this pull request].
[The first alpha version] was eventually released as
[a renderer-agnostic GUI library]. The library did not provide a renderer and
implemented the current [tour example] on top of [`ggez`], a game library.

Since then, the focus has shifted towards providing a batteries-included,
end-user-oriented GUI library, while keeping the ecosystem modular.

[this pull request]: https://github.com/hecrj/coffee/pull/35
[The first alpha version]: https://github.com/iced-rs/iced/tree/0.1.0-alpha
[a renderer-agnostic GUI library]: https://www.reddit.com/r/rust/comments/czzjnv/iced_a_rendereragnostic_gui_library_focused_on/
[tour example]: examples/README.md#tour
[`ggez`]: https://github.com/ggez/ggez

## Contributing / Feedback

If you want to contribute, please read our [contributing guidelines] for more details.

Feedback is also welcome! You can create a new topic in [our Discourse forum] or
come chat to [our Discord server].

## Sponsors

The development of Iced is sponsored by the [Cryptowatch] team at [Kraken.com]

[book]: https://book.iced.rs/
[documentation]: https://docs.rs/iced/
[examples]: https://github.com/iced-rs/iced/tree/0.14/examples#examples
[Coffee]: https://github.com/hecrj/coffee
[Elm]: https://elm-lang.org/
[The Elm Architecture]: https://guide.elm-lang.org/architecture/
[the current issues]: https://github.com/iced-rs/iced/issues
[contributing guidelines]: https://github.com/iced-rs/iced/blob/master/CONTRIBUTING.md
[our Discourse forum]: https://discourse.iced.rs/
[our Discord server]: https://discord.gg/3xZJ65GAhd
[Cryptowatch]: https://cryptowat.ch/charts
[Kraken.com]: https://kraken.com/
