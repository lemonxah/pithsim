//! pith-ui — a pure-Rust, runtime-interpreted UI engine.
//!
//! A screen is described by a [`UiDoc`]: a serializable tree of [`Node`]s,
//! serialized with `postcard` and **interpreted + rendered at runtime** against any
//! `embedded_graphics::DrawTarget` — so the device changes screens by loading a new
//! blob from flash or the wire, with no recompile. The same engine renders on the
//! ESP32 panels and in the desktop preview (the `std`-gated [`Framebuffer`]).
//!
//! `Stat` values can be **bound to live telemetry fields** ([`Bind::Field`]),
//! resolved + formatted at render time via `pith-core` (the shared, byte-compatible
//! formatting + field registry). The device target is esp-idf, which provides std.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use embedded_graphics::{
    mono_font::{ascii, MonoTextStyle},
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{PrimitiveStyle, Rectangle, RoundedRectangle},
    text::{Alignment, Baseline, Text, TextStyleBuilder},
};
use serde::{Deserialize, Serialize};

use pith_core::format::format as fmt_value;
pub use pith_core::format::{Fmt, RuleOp};
use pith_core::registry::field_def;

/// 8-bit RGB; converted to the device's RGB565 at render time.
pub type Color = (u8, u8, u8);

fn col(c: Color) -> Rgb565 {
    Rgb565::new(c.0 >> 3, c.1 >> 2, c.2 >> 3)
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    fn eg(&self) -> Rectangle {
        Rectangle::new(Point::new(self.x, self.y), Size::new(self.w, self.h))
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    Small,
    Medium,
    Large,
}

impl Font {
    fn style(self, c: Color) -> MonoTextStyle<'static, Rgb565> {
        let f = match self {
            // embedded-graphics' built-in ASCII monospace fonts. Bigger/glyph fonts
            // (e.g. a large 7-seg gear) are a follow-up via u8g2-fonts/profont.
            Font::Small => &ascii::FONT_6X10,
            Font::Medium => &ascii::FONT_9X15,
            Font::Large => &ascii::FONT_10X20,
        };
        MonoTextStyle::new(f, col(c))
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Halign {
    Left,
    Center,
    Right,
}

/// A stat's value: static text, or a live telemetry field formatted at render time.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Bind {
    /// Fixed text.
    Text(String),
    /// Live telemetry: 1-based field-registry id. `fmt`/`scale`/`unit` override the
    /// registry defaults (None / 0 / "" => use the field's default).
    Field { id: u8, fmt: Option<Fmt>, scale: i32, unit: String },
}

/// A colour rule: when `op(value, threshold)` holds, the value is drawn in `color`
/// (first matching rule wins; otherwise the stat's base colour).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub op: RuleOp,
    pub threshold: i32,
    pub color: Color,
}

/// What a node draws. New kinds are added here (and in [`draw_kind`]); the wire
/// format stays the same shape, so adding a widget needs no protocol change.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Kind {
    /// Filled (optionally rounded) background panel.
    Panel { color: Color, radius: u32 },
    /// A single line of text.
    Label { text: String, color: Color, font: Font, align: Halign },
    /// A small caption above a large value (the workhorse "stat" tile). The value is
    /// static text or a live field; `color` is the base value colour, overridden by
    /// the first matching `rule`.
    Stat { label: String, value: Bind, label_color: Color, color: Color, rules: Vec<Rule> },
    /// Horizontal progress/level bar, 0..=100.
    Bar { pct: u8, fill: Color, track: Color },
}

/// A positioned node: a rectangle + what to draw in it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Node {
    pub rect: Rect,
    pub kind: Kind,
}

/// One screen targeting one display.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Screen {
    pub display: u8,
    pub w: u32,
    pub h: u32,
    pub bg: Color,
    pub nodes: Vec<Node>,
}

/// A complete UI: one or more screens (one per display).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UiDoc {
    pub version: u16,
    pub screens: Vec<Screen>,
}

impl UiDoc {
    /// Serialize to a compact postcard blob (what gets stored in flash / sent over
    /// the wire).
    pub fn to_postcard(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("serialize UiDoc")
    }

    /// Load a UiDoc from a postcard blob at runtime (no recompile).
    pub fn from_postcard(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// Resolve + format a bound value, returning (text, colour).
fn resolve_bind(value: &Bind, base: Color, rules: &[Rule], telem: &impl Fn(u8) -> i32) -> (String, Color) {
    match value {
        Bind::Text(t) => (t.clone(), base),
        Bind::Field { id, fmt, scale, unit } => {
            let iv = telem(*id);
            let def = field_def(*id as usize);
            let f = fmt.unwrap_or_else(|| def.map(|d| d.fmt).unwrap_or(Fmt::Int));
            let sc = if *scale > 0 { *scale } else { def.map(|d| d.scale).unwrap_or(1) };
            let text = fmt_value(iv, f, sc, unit);
            let color = rules
                .iter()
                .find(|r| r.op.matches(iv, r.threshold))
                .map(|r| r.color)
                .unwrap_or(base);
            (text, color)
        }
    }
}

/// Render a screen with no live telemetry (bound fields resolve to 0).
pub fn render_screen<D>(s: &Screen, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    render_screen_with(s, &|_| 0, target)
}

/// Render a screen, resolving [`Bind::Field`] values through `telem` (a
/// `field id -> raw value` lookup). The firmware passes `field_value` over its
/// telemetry struct; the dashboard passes its telemetry mirror.
pub fn render_screen_with<D, F>(s: &Screen, telem: &F, target: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
    F: Fn(u8) -> i32,
{
    Rectangle::new(Point::zero(), Size::new(s.w, s.h))
        .into_styled(PrimitiveStyle::with_fill(col(s.bg)))
        .draw(target)?;
    for node in &s.nodes {
        draw_kind(&node.rect, &node.kind, telem, target)?;
    }
    Ok(())
}

fn draw_kind<D, F>(r: &Rect, kind: &Kind, telem: &F, t: &mut D) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
    F: Fn(u8) -> i32,
{
    match kind {
        Kind::Panel { color, radius } => {
            RoundedRectangle::with_equal_corners(r.eg(), Size::new(*radius, *radius))
                .into_styled(PrimitiveStyle::with_fill(col(*color)))
                .draw(t)?;
        }
        Kind::Label { text, color, font, align } => {
            draw_text(t, text, r, font.style(*color), *align)?;
        }
        Kind::Stat { label, value, label_color, color, rules } => {
            let (text, vcolor) = resolve_bind(value, *color, rules, telem);
            let cap = Rect { x: r.x, y: r.y, w: r.w, h: r.h / 3 };
            let val = Rect { x: r.x, y: r.y + (r.h / 3) as i32, w: r.w, h: r.h - r.h / 3 };
            draw_text(t, label, &cap, Font::Small.style(*label_color), Halign::Center)?;
            draw_text(t, &text, &val, Font::Large.style(vcolor), Halign::Center)?;
        }
        Kind::Bar { pct, fill, track } => {
            r.eg().into_styled(PrimitiveStyle::with_fill(col(*track))).draw(t)?;
            let fw = r.w * (*pct).min(100) as u32 / 100;
            Rectangle::new(Point::new(r.x, r.y), Size::new(fw, r.h))
                .into_styled(PrimitiveStyle::with_fill(col(*fill)))
                .draw(t)?;
        }
    }
    Ok(())
}

fn draw_text<D>(
    t: &mut D,
    s: &str,
    r: &Rect,
    style: MonoTextStyle<'static, Rgb565>,
    align: Halign,
) -> Result<(), D::Error>
where
    D: DrawTarget<Color = Rgb565>,
{
    let (x, eg_align) = match align {
        Halign::Left => (r.x, Alignment::Left),
        Halign::Center => (r.x + r.w as i32 / 2, Alignment::Center),
        Halign::Right => (r.x + r.w as i32, Alignment::Right),
    };
    let y = r.y + r.h as i32 / 2;
    let ts = TextStyleBuilder::new().alignment(eg_align).baseline(Baseline::Middle).build();
    Text::with_text_style(s, Point::new(x, y), style, ts).draw(t)?;
    Ok(())
}

/// A built-in sample race screen (480x320 main display) with **live-bound** GEAR /
/// speed / delta / fuel — for previews, tests, and bootstrapping editors. Feed it
/// [`demo_telem`] (or any resolver) via [`render_screen_with`] to see live values.
pub fn demo_doc() -> UiDoc {
    const BG: Color = (6, 7, 8);
    const PANEL: Color = (19, 21, 25);
    const WHITE: Color = (232, 234, 237);
    const DIM: Color = (99, 106, 116);
    const GREEN: Color = (0, 230, 118);
    const AMBER: Color = (255, 179, 0);
    const RED: Color = (255, 59, 48);

    let text_stat = |x: i32, y: i32, w: u32, h: u32, label: &str, value: &str, vc: Color| Node {
        rect: Rect { x, y, w, h },
        kind: Kind::Stat {
            label: label.into(),
            value: Bind::Text(value.into()),
            label_color: DIM,
            color: vc,
            rules: alloc::vec![],
        },
    };
    let field_stat = |x: i32, y: i32, w: u32, h: u32, label: &str, id: u8, vc: Color, rules: Vec<Rule>| Node {
        rect: Rect { x, y, w, h },
        kind: Kind::Stat {
            label: label.into(),
            value: Bind::Field { id, fmt: None, scale: 0, unit: String::new() },
            label_color: DIM,
            color: vc,
            rules,
        },
    };
    let panel = |x: i32, y: i32, w: u32, h: u32| Node {
        rect: Rect { x, y, w, h },
        kind: Kind::Panel { color: PANEL, radius: 12 },
    };

    let nodes = alloc::vec![
        Node { rect: Rect { x: 8, y: 8, w: 464, h: 22 }, kind: Kind::Bar { pct: 82, fill: GREEN, track: PANEL } },
        panel(150, 56, 180, 178),
        text_stat(150, 60, 180, 120, "GEAR", "4", WHITE),
        field_stat(150, 176, 180, 54, "KM/H", 1, GREEN, alloc::vec![]), // speed_kmh
        panel(8, 56, 130, 84),
        // delta_ms: green if ahead (<0), red if behind (>0)
        field_stat(8, 60, 130, 80, "DELTA", 10, AMBER, alloc::vec![
            Rule { op: RuleOp::Lt, threshold: 0, color: GREEN },
            Rule { op: RuleOp::Gt, threshold: 0, color: RED },
        ]),
        panel(342, 56, 130, 84),
        field_stat(342, 60, 130, 80, "FUEL", 23, AMBER, alloc::vec![]), // fuel_dl
        Node {
            rect: Rect { x: 8, y: 284, w: 464, h: 28 },
            kind: Kind::Label { text: "LAP  1:24.318".into(), color: WHITE, font: Font::Medium, align: Halign::Center },
        },
    ];
    UiDoc { version: 1, screens: alloc::vec![Screen { display: 0, w: 480, h: 320, bg: BG, nodes }] }
}

/// Demo telemetry resolver matching [`demo_doc`]'s bound fields (so previews show
/// believable live values without a connected device).
pub fn demo_telem(id: u8) -> i32 {
    match id {
        1 => 212,    // speed_kmh
        10 => -3000, // delta_ms -> -0.3000 s
        23 => 486,   // fuel_dl  -> 48.6 L
        _ => 0,
    }
}

/// A heap-backed RGB565 framebuffer + RGBA8 export — the desktop preview target
/// (and what the dashboard blits into a Slint image). Device builds don't need it,
/// so it's behind the `std` feature.
#[cfg(feature = "std")]
mod framebuffer {
    use super::*;

    pub struct Framebuffer {
        pub w: u32,
        pub h: u32,
        pub buf: Vec<Rgb565>,
    }

    impl Framebuffer {
        pub fn new(w: u32, h: u32) -> Self {
            Self { w, h, buf: alloc::vec![Rgb565::BLACK; (w * h) as usize] }
        }

        /// Expand to 8-bit RGBA (for PNG export / Slint's Rgba8 image buffer).
        pub fn to_rgba8(&self) -> Vec<u8> {
            let mut out = Vec::with_capacity((self.w * self.h * 4) as usize);
            for px in &self.buf {
                let (r, g, b) = (px.r(), px.g(), px.b());
                out.push((r << 3) | (r >> 2)); // 5 -> 8 bit
                out.push((g << 2) | (g >> 4)); // 6 -> 8 bit
                out.push((b << 3) | (b >> 2)); // 5 -> 8 bit
                out.push(255);
            }
            out
        }
    }

    impl OriginDimensions for Framebuffer {
        fn size(&self) -> Size {
            Size::new(self.w, self.h)
        }
    }

    impl DrawTarget for Framebuffer {
        type Color = Rgb565;
        type Error = core::convert::Infallible;

        fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
        where
            I: IntoIterator<Item = Pixel<Self::Color>>,
        {
            for Pixel(p, c) in pixels {
                if p.x >= 0 && p.y >= 0 && (p.x as u32) < self.w && (p.y as u32) < self.h {
                    self.buf[(p.y as u32 * self.w + p.x as u32) as usize] = c;
                }
            }
            Ok(())
        }
    }
}

#[cfg(feature = "std")]
pub use framebuffer::Framebuffer;
