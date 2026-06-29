//! pith-ui — the shared, runtime-interpreted UI engine for the Pith DDU.
//!
//! A screen is a [`UiDoc`]: a serializable, **freeform** tree of [`Node`]s (each an
//! absolute [`Rect`] + a [`Kind`]). The doc is serialized with `postcard` and
//! **interpreted + rendered at runtime** against any `embedded_graphics::DrawTarget`
//! — so screens change by loading a new blob from flash or the wire, no recompile,
//! with full layout control.
//!
//! The renderer uses the *same* `u8g2-fonts`, palette and `pith-core` formatting /
//! shift-light logic the firmware uses, so the desktop preview is **pixel-identical**
//! to the device. [`render_screen_diff`] does **dirty-rect** redraws — only the nodes
//! whose telemetry changed repaint, so the device only pushes changed pixels over SPI.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle, RoundedRectangle},
};
use serde::{Deserialize, Serialize};
use u8g2_fonts::{
    fonts,
    types::{FontColor, HorizontalAlignment, VerticalPosition},
    FontRenderer,
};

use pith_core::format::{self, Fmt, RuleOp};
pub use pith_core::format::{Fmt as ValueFmt, Pal, RuleOp as Op};
use pith_core::registry::{field_def, field_value};
pub use pith_core::relatives::Relatives;
use pith_core::shift::{segment_rgb, RevCfg};
pub use pith_core::shift::CarData;
use pith_core::simhub::Telemetry;

// ---- palette (Pal token -> RGB565), identical to the firmware ----
pub fn pal(p: Pal) -> Rgb565 {
    match p {
        Pal::Bg => rgb(8, 10, 14),
        Pal::Panel => rgb(28, 32, 40),
        Pal::White => rgb(235, 238, 245),
        Pal::Dim => rgb(120, 128, 140),
        Pal::Green => rgb(40, 220, 90),
        Pal::Amber => rgb(255, 180, 40),
        Pal::Red => rgb(240, 60, 60),
        Pal::Cyan => rgb(40, 210, 230),
        Pal::Blue => rgb(60, 130, 255),
        Pal::Purple => rgb(180, 110, 255),
        Pal::Rgb(r, g, b) => rgb(r, g, b),
    }
}
fn rgb(r: u8, g: u8, b: u8) -> Rgb565 {
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}
fn rgb888(c: u32) -> Rgb565 {
    rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

// ============ model ============

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

impl Align {
    fn h(self) -> HorizontalAlignment {
        match self {
            Align::Left => HorizontalAlignment::Left,
            Align::Center => HorizontalAlignment::Center,
            Align::Right => HorizontalAlignment::Right,
        }
    }
}

/// Vertical placement of text/content within an element's box.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VAlign {
    Top,
    #[default]
    Center,
    Bottom,
}

impl VAlign {
    /// The baseline y + u8g2 vertical anchor for a box [y, y+h).
    fn place(self, y: i32, h: i32) -> (i32, VerticalPosition) {
        match self {
            VAlign::Top => (y + 2, VerticalPosition::Top),
            VAlign::Center => (y + h / 2, VerticalPosition::Center),
            VAlign::Bottom => (y + h - 2, VerticalPosition::Bottom),
        }
    }
}

/// A colour rule: when `op(value, threshold)` holds, use `color` (first match wins).
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Rule {
    pub op: RuleOp,
    pub threshold: i32,
    pub color: Pal,
}

/// What a node draws. `field` is a 1-based telemetry field id (0 = none). Composite
/// kinds read a fixed set of fields; `Stat`/`Bar`/`Flag` are data-bound.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum Kind {
    /// Filled (optionally rounded) background panel.
    Panel { color: Pal, radius: u8 },
    /// Static text.
    Label { text: String, color: Pal, size: u8, align: Align, valign: VAlign },
    /// Caption + a live value formatted via the field registry (overridable).
    Stat { field: u8, label: String, fmt: Option<Fmt>, scale: i32, unit: String, base: Pal, rules: Vec<Rule>, size: u8 },
    /// Horizontal level bar; value/scale -> 0..=100%.
    Bar { field: u8, label: String, scale: i32, base: Pal, rules: Vec<Rule> },
    /// Big gear glyph, optionally with speed below.
    GearSpeed { speed: bool },
    /// Rev/shift strip (uses the shared shift-light colours). `count` is the number
    /// of segments — set to the physically installed rev-LED count so the on-screen
    /// strip matches the real strip (0 = legacy default of 12).
    RpmStrip {
        #[serde(default)]
        count: u8,
    },
    /// 2x2 tyre-temperature grid.
    TyreGrid,
    /// TC / ABS levels side by side.
    TcDual,
    /// S1/S2/S3 sector times (green if <= personal best).
    Sectors,
    /// Current + best lap times.
    LapPair,
    /// Race position P{pos}/{field}.
    Position { label: String },
    /// Solid flag-colour panel (driven by `field` + rules).
    Flag { field: u8, base: Pal, rules: Vec<Rule> },
    /// Track map: an outline polyline (`pts` = flat x,y pairs normalized to
    /// 0..=1000, pushed from the app's track DB) plus a position dot placed along
    /// the path by the `track_pct` telemetry. Empty `pts` draws a placeholder.
    Map {
        #[serde(default)]
        pts: Vec<u16>,
    },
    /// A live value with no caption (the decomposed half of `Stat`); align within
    /// its box. The composition engine pairs this with a `Label`.
    Value { field: u8, fmt: Option<Fmt>, scale: i32, unit: String, base: Pal, rules: Vec<Rule>, size: u8, align: Align, valign: VAlign },
    /// A touch button: a filled rounded panel + centred label. The device sends HID
    /// joystick button `hid` (1..=32) on tap — momentary (down on touch, up on
    /// release) unless `toggle`. An optional bound `field` + `rules` colour the
    /// button from live game state and (when set) show its value below the label —
    /// e.g. a wiper level or a toggle reflecting the sim's actual on/off state.
    /// `action` is a legacy semantic name kept for back-compat. New fields default
    /// so older serialized docs still decode.
    Button {
        label: String,
        /// OFF-state fill colour.
        color: Pal,
        action: String,
        toggle: bool,
        #[serde(default)]
        hid: u8,
        /// Optional bound field: when set, its value (>0 = on) drives the on/off
        /// state instead of the local toggle latch. The value itself is NOT shown.
        #[serde(default)]
        field: u8,
        #[serde(default)]
        rules: Vec<Rule>,
        /// ON-state fill colour (state on = bound field > 0, or the local toggle).
        #[serde(default = "on_color_default")]
        on_color: Pal,
    },
    /// A composable widget: an element tree laid out inside the node's rect. This
    /// is how built-in and custom widgets are expressed (label vs value position,
    /// row/column arrangement, …) without new Rust per widget.
    Widget(alloc::boxed::Box<El>),
    /// Multi-car relatives / standings table. Rows come from the side-channel
    /// [`Relatives`] (the `@REL` line), NOT the single-car telemetry frame —
    /// it's the only non-single-car widget. `mode`: 0 = relative (cars nearest
    /// on track, signed gap to you), 1 = standings (by position, gap to leader).
    /// `rows` caps visible rows (0 = 6). Appended last so postcard indices of the
    /// existing variants are unchanged.
    Relatives {
        #[serde(default)]
        mode: u8,
        #[serde(default)]
        rows: u8,
    },
}

/// One child of a `Row`/`Col`, with a flex weight (share of the main axis).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Slot {
    pub flex: u16,
    pub el: El,
}

/// An element in a widget tree: a layout container or a leaf (any [`Kind`]).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum El {
    /// Lay children left-to-right, splitting width by flex weight.
    Row { gap: u8, pad: u8, children: Vec<Slot> },
    /// Lay children top-to-bottom, splitting height by flex weight.
    Col { gap: u8, pad: u8, children: Vec<Slot> },
    /// Overlay all children in the same (padded) box.
    Stack { pad: u8, children: Vec<El> },
    /// A tab strip + the active page (one of `pages`). The device switches `active`
    /// on a tap in the strip; the desktop preview shows `active`.
    Tabs { titles: Vec<String>, active: u8, pages: Vec<El> },
    /// A drawable leaf — any widget kind, rendered in its computed rect.
    Leaf(Kind),
}

/// A positioned node: an absolute rectangle + what to draw in it. `page` is the
/// tab page it belongs to when the screen is tabbed (0 otherwise / untabbed).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Node {
    pub rect: Rect,
    pub kind: Kind,
    #[serde(default)]
    pub page: u8,
}

/// One screen, targeting one display. When `tabs` is non-empty the screen is
/// tabbed: a tab strip is drawn across the top and only nodes whose `page` matches
/// the active tab are shown (used for paged button banks on the side display).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Screen {
    pub display: u8,
    pub w: u32,
    pub h: u32,
    pub bg: Pal,
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub tabs: Vec<String>,
}

/// A complete UI: one or more screens (one per display).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct UiDoc {
    pub version: u16,
    pub screens: Vec<Screen>,
}

impl UiDoc {
    pub fn to_postcard(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("serialize UiDoc")
    }
    pub fn from_postcard(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

/// The default content for a built-in widget kind. Decomposable widgets (e.g.
/// `stat`) come back as a composable [`Kind::Widget`] element tree (so the editor
/// can rearrange label vs value); the rest stay as their primitive kind. This is
/// the single source of built-in widget definitions, shared by device + desktop.
#[allow(clippy::too_many_arguments)]
pub fn builtin(
    kind: &str,
    field: u8,
    label: &str,
    fmt: Option<Fmt>,
    scale: i32,
    unit: &str,
    base: Pal,
    rules: Vec<Rule>,
    size: u8,
) -> Kind {
    use alloc::boxed::Box;
    match kind {
        // Caption above value — a Col of a Label + a value-only leaf, so the editor
        // can flip it to a Row, swap order, realign, etc.
        "stat" => Kind::Widget(Box::new(El::Col {
            gap: 2,
            pad: 2,
            children: Vec::from([
                Slot {
                    flex: 1,
                    el: El::Leaf(Kind::Label {
                        text: label.to_string(),
                        color: Pal::Dim,
                        size: 11,
                        align: Align::Center,
                        valign: VAlign::Center,
                    }),
                },
                Slot {
                    flex: 2,
                    el: El::Leaf(Kind::Value { field, fmt, scale, unit: unit.to_string(), base, rules, size, align: Align::Center, valign: VAlign::Center }),
                },
            ]),
        })),
        "bar" => Kind::Bar { field, label: label.to_string(), scale, base, rules },
        "gearSpeed" => Kind::GearSpeed { speed: true },
        "gear" => Kind::GearSpeed { speed: false },
        "rpmStrip" => Kind::RpmStrip { count: 0 },
        "tyreGrid" => Kind::TyreGrid,
        "tcDual" => Kind::TcDual,
        "sectors" => Kind::Sectors,
        "lapPair" => Kind::LapPair,
        "position" => Kind::Position { label: label.to_string() },
        "flag" => Kind::Flag { field, base, rules },
        "map" => Kind::Map { pts: Vec::new() },
        "button" => Kind::Button { label: label.to_string(), color: base, action: String::new(), toggle: false, hid: 0, field, rules, on_color: Pal::Green },
        _ => Kind::Stat { field, label: label.to_string(), fmt, scale, unit: unit.to_string(), base, rules, size },
    }
}

// ============ render primitives (ported from firmware ui.rs) ============

#[allow(clippy::too_many_arguments)]
fn text<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    x: i32,
    y: i32,
    size: u32,
    color: Rgb565,
    h: HorizontalAlignment,
    v: VerticalPosition,
) {
    let p = Point::new(x, y);
    let fc = FontColor::Transparent(color);
    macro_rules! draw {
        ($f:ty) => {{
            let _ = FontRenderer::new::<$f>().render_aligned(s, p, v, h, fc, d);
        }};
    }
    match size {
        0..=11 => draw!(fonts::u8g2_font_6x13_tf),
        12..=15 => draw!(fonts::u8g2_font_helvB12_tf),
        16..=23 => draw!(fonts::u8g2_font_helvB18_tf),
        24..=33 => draw!(fonts::u8g2_font_helvB24_tf),
        34..=40 => draw!(fonts::u8g2_font_logisoso32_tf),
        41..=48 => draw!(fonts::u8g2_font_logisoso38_tf),
        49..=56 => draw!(fonts::u8g2_font_logisoso46_tf),
        _ => draw!(fonts::u8g2_font_logisoso58_tf), // ~58px, the largest available
    }
}

/// Like [`text`] but with a MONOSPACE font (fixed-width logisoso) — so right-
/// aligned numbers (lap times, deltas, sectors) don't jiggle horizontally as the
/// digit widths change. The big tiers are already logisoso (monospace); this only
/// swaps the small/mid proportional helv tiers for fixed-width equivalents.
fn text_mono<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    s: &str,
    x: i32,
    y: i32,
    size: u32,
    color: Rgb565,
    h: HorizontalAlignment,
    v: VerticalPosition,
) {
    let p = Point::new(x, y);
    let fc = FontColor::Transparent(color);
    macro_rules! draw {
        ($f:ty) => {{
            let _ = FontRenderer::new::<$f>().render_aligned(s, p, v, h, fc, d);
        }};
    }
    match size {
        0..=13 => draw!(fonts::u8g2_font_6x13_tf),
        14..=19 => draw!(fonts::u8g2_font_logisoso18_tf),
        20..=27 => draw!(fonts::u8g2_font_logisoso24_tf),
        28..=40 => draw!(fonts::u8g2_font_logisoso32_tf),
        41..=48 => draw!(fonts::u8g2_font_logisoso38_tf),
        49..=56 => draw!(fonts::u8g2_font_logisoso46_tf),
        _ => draw!(fonts::u8g2_font_logisoso58_tf),
    }
}

fn fill_rect<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: i32, y: i32, w: i32, h: i32, c: Rgb565) {
    let _ = Rectangle::new(Point::new(x, y), Size::new(w.max(0) as u32, h.max(0) as u32))
        .into_styled(PrimitiveStyle::with_fill(c))
        .draw(d);
}
fn fill_round<D: DrawTarget<Color = Rgb565>>(d: &mut D, x: i32, y: i32, w: i32, h: i32, r: i32, c: Rgb565) {
    let _ = RoundedRectangle::with_equal_corners(
        Rectangle::new(Point::new(x, y), Size::new(w.max(0) as u32, h.max(0) as u32)),
        Size::new(r as u32, r as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(c))
    .draw(d);
}

fn rule_color(raw: i32, base: Pal, rules: &[Rule]) -> Rgb565 {
    for r in rules {
        if r.op.matches(raw, r.threshold) {
            return pal(r.color);
        }
    }
    pal(base)
}

// ============ widget rendering ============

/// Blend a colour toward white by `amt` (0..256) — used to "light up" a pressed button.
fn lighten(c: Rgb565, amt: u16) -> Rgb565 {
    let inv = 256 - amt;
    Rgb565::new(
        ((c.r() as u16 * inv + 31 * amt) >> 8) as u8,
        ((c.g() as u16 * inv + 63 * amt) >> 8) as u8,
        ((c.b() as u16 * inv + 31 * amt) >> 8) as u8,
    )
}

/// Default ON-state colour for buttons in docs serialized before `on_color` existed.
fn on_color_default() -> Pal {
    Pal::Green
}

/// Black on light backgrounds, white on dark — a readable label over any fill.
fn contrast(bg: Rgb565) -> Rgb565 {
    // perceived luminance on a 0..255 scale (5/6/5 channels expanded to 8-bit)
    let r = (bg.r() as u32) << 3;
    let g = (bg.g() as u32) << 2;
    let b = (bg.b() as u32) << 3;
    let lum = (r * 299 + g * 587 + b * 114) / 1000;
    if lum > 140 {
        Rgb565::BLACK
    } else {
        Rgb565::WHITE
    }
}

/// `active` is a bitmask of HID buttons (bit = hid-1) currently pressed/toggled-on,
/// so a button can light up while you're touching it. 0 = nothing active.
fn draw_kind<D: DrawTarget<Color = Rgb565>>(d: &mut D, r: &Rect, kind: &Kind, t: &Telemetry, now_ms: i64, active: u32, car: &CarData, rel: &Relatives) {
    let (x, y, w, h) = (r.x, r.y, r.w as i32, r.h as i32);
    let cx = x + w / 2;
    match kind {
        Kind::Panel { color, radius } => {
            fill_round(d, x, y, w, h, *radius as i32, pal(*color));
        }
        Kind::Label { text: s, color, size, align, valign } => {
            let sz = if *size == 0 { 14 } else { *size as u32 };
            let ax = match align {
                Align::Left => x + 2,
                Align::Center => cx,
                Align::Right => x + w - 2,
            };
            let (ay, vp) = valign.place(y, h);
            text(d, s, ax, ay, sz, pal(*color), align.h(), vp);
        }
        Kind::Stat { field, label, fmt, scale, unit, base, rules, size } => {
            let raw = field_value(t, *field as usize);
            let def = field_def(*field as usize);
            let f = fmt.unwrap_or_else(|| def.map(|d| d.fmt).unwrap_or(Fmt::Int));
            let sc = if *scale > 0 { *scale } else { def.map(|d| d.scale).unwrap_or(1) };
            let sz = if *size == 0 { 22 } else { *size as u32 };
            text(d, label, cx, y + 11, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            let s = format::format(raw, f, sc, unit);
            text(d, &s, cx, y + h / 2 + 6, sz, rule_color(raw, *base, rules), HorizontalAlignment::Center, VerticalPosition::Center);
        }
        Kind::Bar { field, label, scale, base, rules } => {
            let raw = field_value(t, *field as usize);
            let pct = if *scale > 0 { (raw * 100 / *scale).clamp(0, 100) } else { 0 };
            text(d, label, x + 4, y + 10, 11, pal(Pal::Dim), HorizontalAlignment::Left, VerticalPosition::Center);
            fill_rect(d, x + 4, y + h / 2, w - 8, h / 3, pal(Pal::Panel));
            fill_rect(d, x + 4, y + h / 2, (w - 8) * pct / 100, h / 3, rule_color(raw, *base, rules));
        }
        Kind::GearSpeed { speed } => {
            let g = if t.gear == 0 { 'N' } else { t.gear as char };
            // Clip to the widget rect: the big gear font can extend past the box top,
            // and the dirty-rect blit only clears within the rect — so any overflow
            // would never be erased (ghost trails). Clipping keeps it inside the rect.
            let area = Rectangle::new(Point::new(x, y), Size::new(w.max(0) as u32, h.max(0) as u32));
            let mut cd = d.clipped(&area);
            if *speed {
                // gear in the upper area, speed + unit along the bottom
                let gsz = (h * 5 / 10).clamp(11, 46) as u32;
                text(&mut cd, &g.to_string(), cx, y + h * 4 / 10, gsz, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
                text(&mut cd, &t.speed_kmh.to_string(), cx, y + h - 26, 24, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
                text(&mut cd, "KM/H", cx, y + h - 8, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            } else {
                // gear only: centred in the box, scaled to the largest font that fits
                let gsz = (h - 14).clamp(11, 58) as u32;
                text(&mut cd, &g.to_string(), cx, y + h / 2, gsz, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
            }
        }
        Kind::RpmStrip { count } => {
            let seg = if *count == 0 { 12 } else { (*count as i32).clamp(1, 48) };
            let sw = w / seg;
            for i in 0..seg {
                let c = segment_rgb(t, i, seg, &RevCfg::default(), car, now_ms);
                let col = if c == 0 { pal(Pal::Panel) } else { rgb888(c) };
                fill_round(d, x + i * sw + 1, y + 4, sw - 2, h - 8, 3, col);
            }
        }
        Kind::TyreGrid => {
            let temps = [t.tt_fl_m, t.tt_fr_m, t.tt_rl_m, t.tt_rr_m];
            let (bw, bh) = (w / 2, h / 2);
            for i in 0..4 {
                let (cxx, cyy) = (x + (i as i32 % 2) * bw, y + (i as i32 / 2) * bh);
                let col = if temps[i] > 95 { pal(Pal::Red) } else if temps[i] > 80 { pal(Pal::Amber) } else { pal(Pal::Green) };
                fill_round(d, cxx + 2, cyy + 2, bw - 4, bh - 4, 4, pal(Pal::Panel));
                text(d, &temps[i].to_string(), cxx + bw / 2, cyy + bh / 2, 14, col, HorizontalAlignment::Center, VerticalPosition::Center);
            }
        }
        Kind::TcDual => {
            text(d, "TC", x + w / 4, y + 12, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, "ABS", x + 3 * w / 4, y + 12, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, &t.tc.to_string(), x + w / 4, y + h / 2 + 6, 22, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, &t.abs.to_string(), x + 3 * w / 4, y + h / 2 + 6, 22, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
        }
        Kind::Sectors => {
            let secs = [t.s1_ms, t.s2_ms, t.s3_ms];
            let bs = [t.bs1_ms, t.bs2_ms, t.bs3_ms];
            let sw = w / 3;
            for i in 0..3 {
                let col = if secs[i] > 0 && bs[i] > 0 && secs[i] <= bs[i] { pal(Pal::Green) } else { pal(Pal::Amber) };
                let s = format::format(secs[i], Fmt::Sector, 1, "");
                text_mono(d, &s, x + i as i32 * sw + sw / 2, y + h / 2, 12, col, HorizontalAlignment::Center, VerticalPosition::Center);
            }
        }
        Kind::LapPair => {
            let cur = format::format(t.cur_lap_ms, Fmt::Time, 1, "");
            let best = format::format(t.best_lap_ms, Fmt::Time, 1, "");
            text(d, "CURRENT", cx, y + 10, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            text_mono(d, &cur, cx, y + h / 4 + 6, 18, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, "BEST", cx, y + h / 2 + 8, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            text_mono(d, &best, cx, y + 3 * h / 4 + 4, 18, pal(Pal::Cyan), HorizontalAlignment::Center, VerticalPosition::Center);
        }
        Kind::Position { label } => {
            text(d, label, cx, y + 12, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            let s = alloc::format!("P{}/{}", t.position, t.field_size);
            text(d, &s, cx, y + h / 2 + 4, 22, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
        }
        Kind::Flag { field, base, rules } => {
            let raw = field_value(t, *field as usize);
            fill_round(d, x + 4, y + 4, w - 8, h - 8, 4, rule_color(raw, *base, rules));
        }
        Kind::Map { pts } => {
            let n = pts.len() / 2;
            if n >= 2 {
                // map normalized (0..=1000) coords into the widget box with a margin
                let mg = 6;
                let bx = x + mg;
                let by = y + mg;
                let bw = (w - 2 * mg).max(1);
                let bh = (h - 2 * mg).max(1);
                let to_screen = |i: usize| -> Point {
                    let px = pts[2 * i].min(1000) as i32;
                    let py = pts[2 * i + 1].min(1000) as i32;
                    Point::new(bx + px * bw / 1000, by + py * bh / 1000)
                };
                // outline as a closed polyline
                for i in 0..n {
                    let a = to_screen(i);
                    let b = to_screen((i + 1) % n);
                    let _ = Line::new(a, b)
                        .into_styled(PrimitiveStyle::with_stroke(pal(Pal::Dim), 1))
                        .draw(d);
                }
                // position dot placed by lap progress (index along the polyline)
                let tp = t.track_pct.clamp(0, 1000) as i64;
                let seg = ((tp * n as i64 / 1000) as usize).min(n - 1);
                let dot = to_screen(seg);
                let _ = Circle::new(Point::new(dot.x - 3, dot.y - 3), 7)
                    .into_styled(PrimitiveStyle::with_fill(pal(Pal::Red)))
                    .draw(d);
            } else {
                let _ = Circle::new(Point::new(cx - h / 3, y + h / 6), (h / 3).max(1) as u32)
                    .into_styled(PrimitiveStyle::with_stroke(pal(Pal::Dim), 1))
                    .draw(d);
            }
        }
        Kind::Value { field, fmt, scale, unit, base, rules, size, align, valign } => {
            let raw = field_value(t, *field as usize);
            let def = field_def(*field as usize);
            let f = fmt.unwrap_or_else(|| def.map(|d| d.fmt).unwrap_or(Fmt::Int));
            let sc = if *scale > 0 { *scale } else { def.map(|d| d.scale).unwrap_or(1) };
            let sz = if *size == 0 { 22 } else { *size as u32 };
            let ax = match align {
                Align::Left => x + 2,
                Align::Center => cx,
                Align::Right => x + w - 2,
            };
            let (ay, vp) = valign.place(y, h);
            let s = format::format(raw, f, sc, unit);
            let col = rule_color(raw, *base, rules);
            // Numbers use a fixed-width font so right-aligned values (lap times,
            // deltas, sectors) don't jiggle as digit widths change; free text stays
            // proportional (logisoso has no proper letterforms).
            if matches!(f, Fmt::Str) {
                text(d, &s, ax, ay, sz, col, align.h(), vp);
            } else {
                text_mono(d, &s, ax, ay, sz, col, align.h(), vp);
            }
        }
        Kind::Button { label, color, toggle, field, hid, on_color, .. } => {
            // Toggles reflect game state ONLY (the bound field, >0 = on) — no latch,
            // no press overlay, no outline. Momentary (push) buttons glow while the
            // button is physically pressed (the `active` HID bit). Value is never shown.
            let bit_on = *hid > 0 && (active >> (*hid as u32 - 1)) & 1 == 1;
            let field_on = *field > 0 && field_value(t, *field as usize) > 0;
            let on = if *toggle || *field > 0 { field_on } else { bit_on };
            let base = if on { pal(*on_color) } else { pal(*color) };
            // Momentary "touch registered" glow + bright border on ANY button while
            // it's physically pressed (the `active` bit is the finger-down button,
            // not the toggle latch — so it flashes on press and clears on release,
            // leaving the toggle's on/off colour purely game-data driven).
            let pressed = bit_on;
            let bg = if pressed { lighten(base, 90) } else { base };
            fill_round(d, x + 1, y + 1, w - 2, h - 2, 6, bg);
            if pressed {
                let _ = RoundedRectangle::with_equal_corners(
                    Rectangle::new(Point::new(x + 1, y + 1), Size::new((w - 2).max(0) as u32, (h - 2).max(0) as u32)),
                    Size::new(6, 6),
                )
                .into_styled(PrimitiveStyle::with_stroke(pal(Pal::White), 2))
                .draw(d);
            }
            // label only (no value), in a colour that contrasts the fill
            text(d, label, cx, y + h / 2, 14, contrast(bg), HorizontalAlignment::Center, VerticalPosition::Center);
        }
        Kind::Widget(el) => layout_draw(d, r, el, t, now_ms, car, rel),
        Kind::Relatives { mode, rows } => draw_relatives(d, r, *mode, *rows, rel),
    }
    let _ = active;
}

/// Draw the multi-car relatives/standings table. `mode` 0 = relative (cars nearest
/// the player on track, signed track gap), 1 = standings (by position, gap to
/// leader). The host already selected the cars; the widget sorts + windows them.
fn draw_relatives<D: DrawTarget<Color = Rgb565>>(d: &mut D, r: &Rect, mode: u8, rows: u8, rel: &Relatives) {
    let (x, y, w, h) = (r.x, r.y, r.w as i32, r.h as i32);
    let cx = x + w / 2;
    let entries = rel.entries();
    if entries.is_empty() {
        text(d, "no cars", cx, y + h / 2, 12, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
        return;
    }
    let want = if rows == 0 { 6 } else { rows as usize };

    // Order: standings by position; relative by track gap (ahead at top).
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    if mode == 1 {
        idx.sort_by_key(|&i| entries[i].place);
        idx.truncate(want);
    } else {
        idx.sort_by(|&a, &b| entries[b].gap_rel_ms.cmp(&entries[a].gap_rel_ms));
        let pp = idx.iter().position(|&i| entries[i].is_player()).unwrap_or(0);
        let half = want / 2;
        let start = pp.saturating_sub(half).min(idx.len().saturating_sub(want.min(idx.len())));
        let end = (start + want).min(idx.len());
        idx = idx[start..end].to_vec();
    }

    let n = idx.len().max(1) as i32;
    let row_h = (h / n).max(1);
    let tsz = (row_h - 4).clamp(9, 16) as u32;
    for (row, &i) in idx.iter().enumerate() {
        let c = &entries[i];
        let ry = y + row as i32 * row_h;
        let player = c.is_player();
        if player {
            fill_round(d, x + 1, ry + 1, w - 2, row_h - 2, 3, pal(Pal::Panel));
        }
        let fg = if c.in_pits() { pal(Pal::Dim) } else { pal(Pal::White) };
        let label = alloc::format!("P{} {}", c.place, c.name_str());
        text(d, &label, x + 4, ry + row_h / 2, tsz, fg, HorizontalAlignment::Left, VerticalPosition::Center);
        let (gap, signed) = if mode == 1 { (c.gap_leader_ms, false) } else { (c.gap_rel_ms, true) };
        let gs = if player && signed { alloc::string::String::from("--") } else { fmt_gap(gap, signed) };
        let gcol = if signed {
            if gap > 0 { pal(Pal::Red) } else if gap < 0 { pal(Pal::Green) } else { pal(Pal::Cyan) }
        } else {
            pal(Pal::Cyan)
        };
        text_mono(d, &gs, x + w - 4, ry + row_h / 2, tsz, gcol, HorizontalAlignment::Right, VerticalPosition::Center);
    }
}

/// Format a gap in ms as `S.s` (relative gaps signed, standings gaps unsigned),
/// integer-only so columns stay fixed-width under `text_mono`.
fn fmt_gap(ms: i32, signed: bool) -> String {
    let a = ms.unsigned_abs();
    let (whole, tenths) = (a / 1000, (a % 1000) / 100);
    let sign = if !signed || ms == 0 { "" } else if ms > 0 { "+" } else { "-" };
    alloc::format!("{}{}.{}", sign, whole, tenths)
}

/// Inset a rect by `pad` on all sides (clamped to non-negative size).
fn inset(r: &Rect, pad: i32) -> Rect {
    Rect {
        x: r.x + pad,
        y: r.y + pad,
        w: (r.w as i32 - 2 * pad).max(0) as u32,
        h: (r.h as i32 - 2 * pad).max(0) as u32,
    }
}

/// Lay out + draw an element tree inside `r`. Row/Col split the main axis by flex
/// weight; Stack overlays; Leaf draws via [`draw_kind`] (so every existing widget
/// is reusable as a building block).
fn layout_draw<D: DrawTarget<Color = Rgb565>>(d: &mut D, r: &Rect, el: &El, t: &Telemetry, now_ms: i64, car: &CarData, rel: &Relatives) {
    match el {
        El::Leaf(k) => draw_kind(d, r, k, t, now_ms, 0, car, rel),
        El::Stack { pad, children } => {
            let inner = inset(r, *pad as i32);
            for c in children {
                layout_draw(d, &inner, c, t, now_ms, car, rel);
            }
        }
        El::Tabs { titles, active, pages } => {
            // tab strip across the top, active page fills the rest
            let strip_h = 22.min(r.h as i32 / 4).max(14);
            let n = titles.len().max(1) as i32;
            let tw = r.w as i32 / n;
            for (i, title) in titles.iter().enumerate() {
                let tx = r.x + i as i32 * tw;
                let on = i as u8 == *active;
                fill_round(d, tx + 1, r.y + 1, tw - 2, strip_h - 2, 3, pal(if on { Pal::Panel } else { Pal::Bg }));
                text(d, title, tx + tw / 2, r.y + strip_h / 2, 11, pal(if on { Pal::White } else { Pal::Dim }), HorizontalAlignment::Center, VerticalPosition::Center);
            }
            if let Some(page) = pages.get(*active as usize) {
                let body = Rect { x: r.x, y: r.y + strip_h, w: r.w, h: (r.h as i32 - strip_h).max(0) as u32 };
                layout_draw(d, &body, page, t, now_ms, car, rel);
            }
        }
        El::Row { gap, pad, children } => {
            let inner = inset(r, *pad as i32);
            let n = children.len() as i32;
            if n == 0 {
                return;
            }
            let total: i32 = children.iter().map(|s| s.flex.max(1) as i32).sum();
            let avail = (inner.w as i32 - *gap as i32 * (n - 1).max(0)).max(0);
            let mut cx = inner.x;
            for s in children {
                let cw = avail * s.flex.max(1) as i32 / total;
                let cr = Rect { x: cx, y: inner.y, w: cw.max(0) as u32, h: inner.h };
                layout_draw(d, &cr, &s.el, t, now_ms, car, rel);
                cx += cw + *gap as i32;
            }
        }
        El::Col { gap, pad, children } => {
            let inner = inset(r, *pad as i32);
            let n = children.len() as i32;
            if n == 0 {
                return;
            }
            let total: i32 = children.iter().map(|s| s.flex.max(1) as i32).sum();
            let avail = (inner.h as i32 - *gap as i32 * (n - 1).max(0)).max(0);
            let mut cy = inner.y;
            for s in children {
                let ch = avail * s.flex.max(1) as i32 / total;
                let cr = Rect { x: inner.x, y: cy, w: inner.w, h: ch.max(0) as u32 };
                layout_draw(d, &cr, &s.el, t, now_ms, car, rel);
                cy += ch + *gap as i32;
            }
        }
    }
}

// ============ rendering: full + dirty-rect ============

/// Per-node content signature (FNV-1a of the telemetry that affects the node's
/// pixels). Static kinds hash to a constant so they draw once and never repaint.
fn node_sig(kind: &Kind, t: &Telemetry, now_ms: i64, active: u32) -> u64 {
    fn h(vals: &[i64]) -> u64 {
        let mut x: u64 = 0xcbf29ce484222325;
        for &v in vals {
            x ^= v as u64;
            x = x.wrapping_mul(0x100000001b3);
        }
        x
    }
    let fv = |id: u8| field_value(t, id as usize) as i64;
    match kind {
        Kind::Panel { .. } | Kind::Label { .. } => 0,
        Kind::Map { .. } => h(&[t.track_pct as i64]),
        Kind::Stat { field, .. }
        | Kind::Bar { field, .. }
        | Kind::Flag { field, .. }
        | Kind::Value { field, .. } => h(&[fv(*field)]),
        // include the press/toggle state so a button repaints on press + release
        Kind::Button { field, hid, .. } => {
            let on = if *hid > 0 { (active >> (*hid as u32 - 1)) & 1 } else { 0 };
            h(&[fv(*field), on as i64])
        }
        Kind::GearSpeed { speed } => h(&[t.gear as i64, if *speed { t.speed_kmh as i64 } else { 0 }]),
        // blink phase keeps the rev strip live
        Kind::RpmStrip { .. } => h(&[t.rpm as i64, t.max_rpm as i64, t.shift_rpm as i64, now_ms / 80]),
        Kind::TyreGrid => h(&[t.tt_fl_m as i64, t.tt_fr_m as i64, t.tt_rl_m as i64, t.tt_rr_m as i64]),
        Kind::TcDual => h(&[t.tc as i64, t.abs as i64]),
        Kind::Sectors => h(&[t.s1_ms as i64, t.s2_ms as i64, t.s3_ms as i64, t.bs1_ms as i64, t.bs2_ms as i64, t.bs3_ms as i64]),
        Kind::LapPair => h(&[t.cur_lap_ms as i64, t.best_lap_ms as i64]),
        Kind::Position { .. } => h(&[t.position as i64, t.field_size as i64]),
        Kind::Widget(el) => el_sig(el, t, now_ms),
        // Relatives data rides a side channel node_sig can't see; repaint on a
        // ~4 Hz tick so live gaps refresh without threading the list in here.
        Kind::Relatives { mode, rows } => h(&[*mode as i64, *rows as i64, now_ms / 250]),
    }
}

/// Combine the signatures of an element tree (so a composed widget repaints iff
/// any of its leaves' telemetry changed).
fn el_sig(el: &El, t: &Telemetry, now_ms: i64) -> u64 {
    fn mix(a: u64, b: u64) -> u64 {
        (a ^ b).wrapping_mul(0x100000001b3)
    }
    match el {
        El::Leaf(k) => node_sig(k, t, now_ms, 0),
        El::Stack { children, .. } => children.iter().fold(0, |a, c| mix(a, el_sig(c, t, now_ms))),
        El::Row { children, .. } | El::Col { children, .. } => {
            children.iter().fold(0, |a, s| mix(a, el_sig(&s.el, t, now_ms)))
        }
        El::Tabs { active, pages, .. } => {
            let base = mix(0x9e3779b9, *active as u64);
            match pages.get(*active as usize) {
                Some(p) => mix(base, el_sig(p, t, now_ms)),
                None => base,
            }
        }
    }
}

/// Cache of last-drawn node signatures for [`render_screen_diff`].
#[derive(Default)]
pub struct RenderCache {
    sigs: Vec<u64>,
    last_tab: i32, // active tab last painted (tabbed screens) — switch forces a full repaint
}

impl RenderCache {
    pub fn new() -> Self {
        Self { sigs: Vec::new(), last_tab: -1 }
    }
    /// Force a full repaint on the next [`render_screen_diff`] (e.g. after a layout
    /// swap or display wake).
    pub fn invalidate(&mut self) {
        self.sigs.clear();
        self.last_tab = -1;
    }
}

/// Full repaint: clear to the screen background and draw every node.
pub fn render_screen<D: DrawTarget<Color = Rgb565>>(s: &Screen, t: &Telemetry, now_ms: i64, active: u32, car: &CarData, rel: &Relatives, d: &mut D) {
    let _ = d.clear(pal(s.bg));
    for node in &s.nodes {
        draw_kind(d, &node.rect, &node.kind, t, now_ms, active, car, rel);
    }
}

/// Dirty-rect repaint: only redraw nodes whose telemetry changed since last call
/// (the rest of the panel — static chrome — is left untouched, so the device only
/// pushes the changed pixels over SPI). Pass a fresh [`RenderCache`] the first time.
/// Returns the bounding box `(x0, y0, x1, y1)` (inclusive) of everything that was
/// redrawn this call — so the caller can push ONLY those pixels over SPI. `None`
/// means nothing changed (skip the blit entirely). A full repaint returns the whole
/// screen. This is what keeps the LCD fast: blit the changed region, not the frame.
pub fn render_screen_diff<D: DrawTarget<Color = Rgb565>>(
    s: &Screen,
    t: &Telemetry,
    now_ms: i64,
    active: u32,
    car: &CarData,
    rel: &Relatives,
    cache: &mut RenderCache,
    d: &mut D,
) -> Option<(i32, i32, i32, i32)> {
    let mut scratch = Vec::new();
    render_screen_dirty(s, t, now_ms, active, car, rel, cache, d, &mut scratch)
}

/// Like [`render_screen_diff`], but also fills `rects` with the bounding box of
/// EACH node that was redrawn (cleared first). The caller should blit those rects
/// INDIVIDUALLY rather than the returned union — under live telemetry the changed
/// widgets are scattered, so the union is nearly the whole screen while the actual
/// changed pixels are a fraction of it. Per-rect blits keep the SPI cost (and the
/// frame rate, since touch shares this loop) proportional to what really changed.
pub fn render_screen_dirty<D: DrawTarget<Color = Rgb565>>(
    s: &Screen,
    t: &Telemetry,
    now_ms: i64,
    active: u32,
    car: &CarData,
    rel: &Relatives,
    cache: &mut RenderCache,
    d: &mut D,
    rects: &mut Vec<(i32, i32, i32, i32)>,
) -> Option<(i32, i32, i32, i32)> {
    rects.clear();
    let full = cache.sigs.len() != s.nodes.len();
    if full {
        let _ = d.clear(pal(s.bg));
        cache.sigs.clear();
        cache.sigs.resize(s.nodes.len(), 0);
    }
    let mut dirty: Option<(i32, i32, i32, i32)> = None;
    for (i, node) in s.nodes.iter().enumerate() {
        let sig = node_sig(&node.kind, t, now_ms, active);
        if full || cache.sigs[i] != sig {
            let r = &node.rect;
            if !full {
                // erase the node's rect with the screen background before repaint
                fill_rect(d, r.x, r.y, r.w as i32, r.h as i32, pal(s.bg));
            }
            draw_kind(d, r, &node.kind, t, now_ms, active, car, rel);
            cache.sigs[i] = sig;
            let (x0, y0) = (r.x, r.y);
            let (x1, y1) = (r.x + r.w as i32 - 1, r.y + r.h as i32 - 1);
            if !full {
                rects.push((x0, y0, x1, y1));
            }
            dirty = Some(match dirty {
                None => (x0, y0, x1, y1),
                Some((ax0, ay0, ax1, ay1)) => (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1)),
            });
        }
    }
    if full {
        let whole = (0, 0, s.w as i32 - 1, s.h as i32 - 1);
        rects.push(whole);
        Some(whole)
    } else {
        dirty
    }
}

/// Height of the tab strip on a tabbed screen.
pub const TAB_STRIP_H: i32 = 26;

/// Which tab a tap at (tx,ty) lands on, if it's in the strip of an `n`-tab screen
/// of width `w`. None if the tap is below the strip (i.e. in the page body).
pub fn tab_at(w: u32, n: usize, tx: i32, ty: i32) -> Option<u8> {
    if n == 0 || ty < 0 || ty >= TAB_STRIP_H || tx < 0 || tx >= w as i32 {
        return None;
    }
    let tw = (w as i32 / n as i32).max(1);
    Some(((tx / tw) as usize).min(n - 1) as u8)
}

/// Full repaint of a tabbed screen: a tab strip across the top (active highlighted)
/// plus only the nodes on the active page. Used for the side button banks; a full
/// repaint each frame is fine for the simple button screen and sidesteps dirty-rect
/// bookkeeping across tab switches.
pub fn render_tabbed<D: DrawTarget<Color = Rgb565>>(
    s: &Screen,
    active: u8,
    t: &Telemetry,
    now_ms: i64,
    pressed: u32,
    car: &CarData,
    rel: &Relatives,
    d: &mut D,
) {
    let _ = d.clear(pal(s.bg));
    let n = s.tabs.len().max(1) as i32;
    let tw = s.w as i32 / n;
    for (i, title) in s.tabs.iter().enumerate() {
        let tx = i as i32 * tw;
        let on = i as u8 == active;
        fill_round(d, tx + 1, 1, tw - 2, TAB_STRIP_H - 2, 3, pal(if on { Pal::Panel } else { Pal::Bg }));
        text(
            d,
            title,
            tx + tw / 2,
            TAB_STRIP_H / 2,
            12,
            pal(if on { Pal::White } else { Pal::Dim }),
            HorizontalAlignment::Center,
            VerticalPosition::Center,
        );
    }
    for node in s.nodes.iter().filter(|n| n.page == active) {
        draw_kind(d, &node.rect, &node.kind, t, now_ms, pressed, car, rel);
    }
}

/// Draw just the tab strip across the top (active highlighted).
fn draw_tab_strip<D: DrawTarget<Color = Rgb565>>(s: &Screen, active: u8, d: &mut D) {
    let n = s.tabs.len().max(1) as i32;
    let tw = s.w as i32 / n;
    for (i, title) in s.tabs.iter().enumerate() {
        let tx = i as i32 * tw;
        let on = i as u8 == active;
        fill_round(d, tx + 1, 1, tw - 2, TAB_STRIP_H - 2, 3, pal(if on { Pal::Panel } else { Pal::Bg }));
        text(d, title, tx + tw / 2, TAB_STRIP_H / 2, 12, pal(if on { Pal::White } else { Pal::Dim }), HorizontalAlignment::Center, VerticalPosition::Center);
    }
}

/// Dirty-rect repaint of a tabbed screen: a full repaint on a tab switch (or first
/// paint), then only the active page's changed nodes — same per-node blit model as
/// [`render_screen_dirty`], so a tabbed layout with live widgets stays as cheap as a
/// plain one. Fills `rects` with the per-node boxes to blit; returns their union.
pub fn render_tabbed_dirty<D: DrawTarget<Color = Rgb565>>(
    s: &Screen,
    active: u8,
    t: &Telemetry,
    now_ms: i64,
    pressed: u32,
    car: &CarData,
    rel: &Relatives,
    cache: &mut RenderCache,
    d: &mut D,
    rects: &mut Vec<(i32, i32, i32, i32)>,
) -> Option<(i32, i32, i32, i32)> {
    rects.clear();
    // Index the active page's nodes (the only ones drawn).
    let page: Vec<&Node> = s.nodes.iter().filter(|n| n.page == active).collect();
    let full = cache.last_tab != active as i32 || cache.sigs.len() != page.len();
    if full {
        let _ = d.clear(pal(s.bg));
        draw_tab_strip(s, active, d);
        cache.sigs.clear();
        cache.sigs.resize(page.len(), 0);
        cache.last_tab = active as i32;
        for (i, node) in page.iter().enumerate() {
            draw_kind(d, &node.rect, &node.kind, t, now_ms, pressed, car, rel);
            cache.sigs[i] = node_sig(&node.kind, t, now_ms, pressed);
        }
        let whole = (0, 0, s.w as i32 - 1, s.h as i32 - 1);
        rects.push(whole);
        return Some(whole);
    }
    let mut dirty: Option<(i32, i32, i32, i32)> = None;
    for (i, node) in page.iter().enumerate() {
        let sig = node_sig(&node.kind, t, now_ms, pressed);
        if cache.sigs[i] != sig {
            let r = &node.rect;
            fill_rect(d, r.x, r.y, r.w as i32, r.h as i32, pal(s.bg));
            draw_kind(d, r, &node.kind, t, now_ms, pressed, car, rel);
            cache.sigs[i] = sig;
            let rect = (r.x, r.y, r.x + r.w as i32 - 1, r.y + r.h as i32 - 1);
            rects.push(rect);
            dirty = Some(match dirty {
                None => rect,
                Some((ax0, ay0, ax1, ay1)) => (ax0.min(rect.0), ay0.min(rect.1), ax1.max(rect.2), ay1.max(rect.3)),
            });
        }
    }
    dirty
}

// ============ desktop preview ============

/// A heap-backed RGB565 framebuffer + RGBA8 export — the desktop preview target
/// (what the dashboard blits into a Slint image). Behind the `std` feature.
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
        pub fn to_rgba8(&self) -> Vec<u8> {
            let mut out = Vec::with_capacity((self.w * self.h * 4) as usize);
            for px in &self.buf {
                let (r, g, b) = (px.r(), px.g(), px.b());
                out.push((r << 3) | (r >> 2));
                out.push((g << 2) | (g >> 4));
                out.push((b << 3) | (b >> 2));
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

// ============ demo ============

/// A built-in sample race screen (480x320) exercising the widget set, for previews,
/// tests and bootstrapping the editor.
pub fn demo_doc() -> UiDoc {
    let stat = |x: i32, y: i32, w: u32, h: u32, label: &str, field: u8, base: Pal, rules: Vec<Rule>| Node {
        rect: Rect { x, y, w, h },
        kind: Kind::Stat { field, label: label.into(), fmt: None, scale: 0, unit: String::new(), base, rules, size: 0 },
        page: 0,
    };
    let panel = |x: i32, y: i32, w: u32, h: u32| Node {
        rect: Rect { x, y, w, h },
        kind: Kind::Panel { color: Pal::Panel, radius: 12 },
        page: 0,
    };
    let leaf = |x: i32, y: i32, w: u32, h: u32, kind: Kind| Node { rect: Rect { x, y, w, h }, kind, page: 0 };
    let nodes = alloc::vec![
        leaf(0, 2, 480, 42, Kind::RpmStrip { count: 0 }),
        panel(136, 50, 208, 200),
        leaf(136, 50, 208, 200, Kind::GearSpeed { speed: true }),
        panel(4, 50, 128, 90),
        stat(4, 50, 128, 90, "DELTA", 10 /*delta_ms*/, Pal::Amber, alloc::vec![
            Rule { op: RuleOp::Lt, threshold: 0, color: Pal::Green },
            Rule { op: RuleOp::Gt, threshold: 0, color: Pal::Red },
        ]),
        panel(4, 150, 128, 120),
        leaf(4, 150, 128, 120, Kind::TyreGrid),
        panel(348, 50, 128, 90),
        stat(348, 50, 128, 90, "FUEL", 23 /*fuel_dl*/, Pal::Amber, alloc::vec![]),
        panel(348, 150, 128, 120),
        leaf(348, 150, 128, 120, Kind::Position { label: "POS".into() }),
        leaf(0, 276, 480, 42, Kind::LapPair),
    ];
    UiDoc { version: 1, screens: alloc::vec![Screen { display: 0, w: 480, h: 320, bg: Pal::Bg, nodes, tabs: Vec::new() }] }
}

/// Demo telemetry (a believable race frame) for previews without a device.
pub fn demo_telem() -> Telemetry {
    let mut t = Telemetry::idle();
    t.gear = b'4';
    t.speed_kmh = 212;
    t.rpm = 7100;
    t.max_rpm = 8200;
    t.shift_rpm = 7800;
    t.delta_ms = -3000;
    t.fuel_dl = 486;
    t.position = 4;
    t.field_size = 20;
    t.cur_lap_ms = 84318;
    t.best_lap_ms = 82900;
    t.tt_fl_m = 88;
    t.tt_fr_m = 90;
    t.tt_rl_m = 97;
    t.tt_rr_m = 86;
    t
}

#[cfg(all(test, feature = "std"))]
mod engine_tests {
    use super::*;

    // The composition engine lays out + draws a Widget(Col[Label, Value]) without
    // panicking and paints pixels (the built-in `stat` is now this tree).
    #[test]
    fn widget_col_renders() {
        let k = builtin("stat", 23, "FUEL", None, 10, "L", Pal::Cyan, Vec::new(), 24);
        assert!(matches!(k, Kind::Widget(_)));
        let screen = Screen {
            display: 0,
            w: 120,
            h: 80,
            bg: Pal::Bg,
            nodes: Vec::from([Node { rect: Rect { x: 0, y: 0, w: 120, h: 80 }, kind: k, page: 0 }]),
            tabs: Vec::new(),
        };
        let t = demo_telem();
        let mut fb = Framebuffer::new(120, 80);
        render_screen(&screen, &t, 0, 0, &CarData::default(), &Relatives::default(), &mut fb);
        // some non-background pixels were drawn
        let bg = pal(Pal::Bg);
        assert!(fb_any_non_bg(&fb, bg), "widget drew nothing");
    }

    fn fb_any_non_bg(fb: &Framebuffer, bg: Rgb565) -> bool {
        let rgba = fb.to_rgba8();
        let bgr = ((bg.r() as u32) << 3, (bg.g() as u32) << 2, (bg.b() as u32) << 3);
        let _ = bgr;
        // any pixel whose alpha row differs from a flat fill -> just check variance
        rgba.chunks(4).any(|p| p[0] > 40 || p[1] > 40 || p[2] > 40)
    }
}
