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

// Render primitives intentionally take flat geometry + palette + doc context
// (mirrors the firmware's C-style draw calls); grouping into structs would
// churn both sides for no runtime win.
#![allow(clippy::too_many_arguments)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, Line, PrimitiveStyle, Rectangle, RoundedRectangle},
};
use serde::{Deserialize, Serialize};
use u8g2_fonts::{fonts, types::FontColor, FontRenderer};
// Re-exported so sibling crates (e.g. pith-bios) can call the public draw
// primitives ([`text`], [`fill_round`]) without a direct u8g2-fonts dependency.
pub use u8g2_fonts::types::{HorizontalAlignment, VerticalPosition};

use pith_core::format::{self, Fmt, RuleOp};
pub use pith_core::format::{Fmt as ValueFmt, Pal, RuleOp as Op};
use pith_core::registry::{field_def, field_value};
pub use pith_core::relatives::Relatives;
pub use pith_core::shift::CarData;
use pith_core::shift::{self, segment_rgb, RevCfg};
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
    Label {
        text: String,
        color: Pal,
        size: u8,
        align: Align,
        valign: VAlign,
    },
    /// Caption + a live value formatted via the field registry (overridable).
    Stat {
        field: u8,
        label: String,
        fmt: Option<Fmt>,
        scale: i32,
        unit: String,
        base: Pal,
        rules: Vec<Rule>,
        size: u8,
    },
    /// Horizontal level bar; value/scale -> 0..=100%.
    Bar {
        field: u8,
        label: String,
        scale: i32,
        base: Pal,
        rules: Vec<Rule>,
    },
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
    Flag {
        field: u8,
        base: Pal,
        rules: Vec<Rule>,
    },
    /// Track map: an outline polyline (`pts` = flat x,y pairs normalized to
    /// 0..=1000, pushed from the app's track DB) plus a position dot placed along
    /// the path by the `track_pct` telemetry. Empty `pts` draws a placeholder.
    Map {
        #[serde(default)]
        pts: Vec<u16>,
    },
    /// A live value with no caption (the decomposed half of `Stat`); align within
    /// its box. The composition engine pairs this with a `Label`.
    Value {
        field: u8,
        fmt: Option<Fmt>,
        scale: i32,
        unit: String,
        base: Pal,
        rules: Vec<Rule>,
        size: u8,
        align: Align,
        valign: VAlign,
    },
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
    /// Fullscreen "all tyres" panel: a 2x2 car layout (FL/FR over RL/RR), each
    /// corner showing the surface tread gradient (inner/mid/outer temps), plus
    /// pressure, brake temp, wear % and compound. Single-car telemetry only.
    /// Appended last so existing postcard variant indices are unchanged.
    TyrePanel,
    /// Three TC channels side by side: level / slip / cut (LMU). Appended last.
    TcTriple,
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
    Row {
        gap: u8,
        pad: u8,
        children: Vec<Slot>,
    },
    /// Lay children top-to-bottom, splitting height by flex weight.
    Col {
        gap: u8,
        pad: u8,
        children: Vec<Slot>,
    },
    /// Overlay all children in the same (padded) box.
    Stack { pad: u8, children: Vec<El> },
    /// A tab strip + the active page (one of `pages`). The device switches `active`
    /// on a tap in the strip; the desktop preview shows `active`.
    Tabs {
        titles: Vec<String>,
        active: u8,
        pages: Vec<El>,
    },
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
                    el: El::Leaf(Kind::Value {
                        field,
                        fmt,
                        scale,
                        unit: unit.to_string(),
                        base,
                        rules,
                        size,
                        align: Align::Center,
                        valign: VAlign::Center,
                    }),
                },
            ]),
        })),
        "bar" => Kind::Bar {
            field,
            label: label.to_string(),
            scale,
            base,
            rules,
        },
        "gearSpeed" => Kind::GearSpeed { speed: true },
        "gear" => Kind::GearSpeed { speed: false },
        "rpmStrip" => Kind::RpmStrip { count: 0 },
        "tyreGrid" => Kind::TyreGrid,
        "tyrePanel" => Kind::TyrePanel,
        "tcTriple" => Kind::TcTriple,
        "tcDual" => Kind::TcDual,
        "sectors" => Kind::Sectors,
        "lapPair" => Kind::LapPair,
        "position" => Kind::Position {
            label: label.to_string(),
        },
        "flag" => Kind::Flag { field, base, rules },
        "map" => Kind::Map { pts: Vec::new() },
        "button" => Kind::Button {
            label: label.to_string(),
            color: base,
            action: String::new(),
            toggle: false,
            hid: 0,
            field,
            rules,
            on_color: Pal::Green,
        },
        _ => Kind::Stat {
            field,
            label: label.to_string(),
            fmt,
            scale,
            unit: unit.to_string(),
            base,
            rules,
            size,
        },
    }
}

// ============ render primitives (ported from firmware ui.rs) ============

/// Shared text primitive: pick a u8g2 font by requested pixel height and render
/// `s` aligned at (x,y). Public so sibling crates (pith-bios) draw with the same
/// fonts as the rest of the UI.
pub fn text<D: DrawTarget<Color = Rgb565>>(
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
    let _ = Rectangle::new(
        Point::new(x, y),
        Size::new(w.max(0) as u32, h.max(0) as u32),
    )
    .into_styled(PrimitiveStyle::with_fill(c))
    .draw(d);
}
/// Shared filled-rounded-rect primitive. Public so sibling crates (pith-bios)
/// draw panels/buttons identically to the rest of the UI.
pub fn fill_round<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    r: i32,
    c: Rgb565,
) {
    let _ = RoundedRectangle::with_equal_corners(
        Rectangle::new(
            Point::new(x, y),
            Size::new(w.max(0) as u32, h.max(0) as u32),
        ),
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

/// Inflate a dirty rect by 1px each side, clamped to the screen. Tall glyphs can
/// paint a row just outside their cell; a too-tight blit would leave that edge row
/// stale. Lives in the lib so the platform blits returned rects verbatim.
fn inflate(c: (i32, i32, i32, i32), sw: i32, sh: i32) -> (i32, i32, i32, i32) {
    (
        (c.0 - 1).max(0),
        (c.1 - 1).max(0),
        (c.2 + 1).min(sw - 1),
        (c.3 + 1).min(sh - 1),
    )
}

/// Reusable per-cell dirty painter. A widget emits its dynamic pieces as [`cell`]s
/// (each with a content signature) and its fixed pieces via [`chrome`]; the painter
/// caches each cell's signature across frames and, on an incremental pass, erases +
/// redraws + records the (inflated) dirty rect ONLY for cells whose signature
/// changed. This is how any multi-value widget gets fine-grained dirty for free —
/// no bespoke per-widget bookkeeping. On `full` it paints everything and reseeds.
///
/// [`cell`]: Painter::cell
/// [`chrome`]: Painter::chrome
pub struct Painter<'a, D: DrawTarget<Color = Rgb565>> {
    d: &'a mut D,
    cache: &'a mut Vec<u64>,
    rects: &'a mut Vec<(i32, i32, i32, i32)>,
    dirty: Option<(i32, i32, i32, i32)>,
    full: bool,
    bg: Rgb565,
    sw: i32,
    sh: i32,
    idx: usize,
}

impl<'a, D: DrawTarget<Color = Rgb565>> Painter<'a, D> {
    fn new(
        d: &'a mut D,
        cache: &'a mut Vec<u64>,
        rects: &'a mut Vec<(i32, i32, i32, i32)>,
        full: bool,
        bg: Rgb565,
        sw: i32,
        sh: i32,
    ) -> Self {
        Self {
            d,
            cache,
            rects,
            dirty: None,
            full,
            bg,
            sw,
            sh,
            idx: 0,
        }
    }

    /// Static content — painted once on a full repaint, skipped on incremental passes.
    pub fn chrome(&mut self, draw: impl FnOnce(&mut D)) {
        if self.full {
            draw(self.d);
        }
    }

    /// A dynamic cell bounded by `rect`. `sig` is its content signature (e.g. the
    /// value it shows). Redraws + records a dirty rect only when `sig` changed.
    pub fn cell(&mut self, rect: (i32, i32, i32, i32), sig: u64, draw: impl FnOnce(&mut D)) {
        let i = self.idx;
        self.idx += 1;
        if self.cache.len() <= i {
            self.cache.resize(i + 1, !sig); // guarantee a first-pass redraw
        }
        if self.full || self.cache[i] != sig {
            if !self.full {
                let (x0, y0, x1, y1) = rect;
                fill_rect(self.d, x0, y0, x1 - x0 + 1, y1 - y0 + 1, self.bg);
            }
            draw(self.d);
            self.cache[i] = sig;
            if !self.full {
                let r = inflate(rect, self.sw, self.sh);
                self.rects.push(r);
                self.dirty = Some(match self.dirty {
                    None => r,
                    Some((a0, b0, a1, b1)) => (a0.min(r.0), b0.min(r.1), a1.max(r.2), b1.max(r.3)),
                });
            }
        }
    }

    fn finish(self) -> Option<(i32, i32, i32, i32)> {
        self.dirty
    }
}

fn tyre_tcol(v: i32) -> Rgb565 {
    if v == format::NA {
        pal(Pal::Dim)
    } else if v > 950 {
        pal(Pal::Red)
    } else if v > 800 {
        pal(Pal::Amber)
    } else {
        pal(Pal::Green)
    }
}

fn tyre_compound(c: i32) -> &'static str {
    match c {
        0 => "S",
        1 => "M",
        2 => "H",
        3 => "W",
        _ => "--",
    }
}

/// The All-Tyres panel, expressed purely as `Painter` cells — so per-cell dirty is
/// the lib's job, not this widget's. 4 corners (FL FR / RL RR): small i/m/o tread
/// zones on top, the average as the big focus number, then PRESS/BRAKE/WEAR/COMP.
fn paint_tyre_panel<D: DrawTarget<Color = Rgb565>>(p: &mut Painter<D>, r: &Rect, t: &Telemetry) {
    let (x, y, w, h) = (r.x, r.y, r.w as i32, r.h as i32);
    let avg = [t.tt_avg_fl, t.tt_avg_fr, t.tt_avg_rl, t.tt_avg_rr];
    let zi = [t.tt_fl_i, t.tt_fr_i, t.tt_rl_i, t.tt_rr_i];
    let zm = [t.tt_fl_m, t.tt_fr_m, t.tt_rl_m, t.tt_rr_m];
    let zo = [t.tt_fl_o, t.tt_fr_o, t.tt_rl_o, t.tt_rr_o];
    let press = [t.tp_fl, t.tp_fr, t.tp_rl, t.tp_rr];
    let wear = [t.tw_fl, t.tw_fr, t.tw_rl, t.tw_rr];
    let brake = [t.bt_fl, t.bt_fr, t.bt_rl, t.bt_rr];
    let comp = [t.comp_fl, t.comp_fr, t.comp_rl, t.comp_rr];
    let labels = ["FL", "FR", "RL", "RR"];
    let stat_labels = ["PRESS", "BRAKE", "WEAR", "COMP"];
    let gut = (w / 10).clamp(8, 48);
    let cw = (w - gut) / 2;
    let chh = h / 2;
    for i in 0..4 {
        let px = x + (i as i32 % 2) * (cw + gut);
        let py = y + (i as i32 / 2) * chh;
        let zw = (cw - 16) / 3;
        let zy = py + 22;
        let zh = (chh / 7).clamp(14, 28);
        let avg_sz = (chh / 5).clamp(20, 34);
        let avg_top = zy + zh + 4;
        let avg_cy = avg_top + avg_sz / 2;
        let sy = avg_top + avg_sz + 8;
        let lh = ((py + chh - 6 - sy) / 4).clamp(12, 40);
        // static chrome
        p.chrome(move |d| {
            fill_round(d, px + 3, py + 3, cw - 6, chh - 6, 6, pal(Pal::Panel));
            text(
                d,
                labels[i],
                px + 10,
                py + 12,
                11,
                pal(Pal::Dim),
                HorizontalAlignment::Left,
                VerticalPosition::Center,
            );
            for (ri, lbl) in stat_labels.iter().enumerate() {
                let ry = sy + ri as i32 * lh + lh / 2;
                text(
                    d,
                    lbl,
                    px + 12,
                    ry,
                    11,
                    pal(Pal::Dim),
                    HorizontalAlignment::Left,
                    VerticalPosition::Center,
                );
            }
        });
        // small i/m/o tread zones on top
        let zones = [zi[i], zm[i], zo[i]];
        for (k, &v) in zones.iter().enumerate() {
            let zx = px + 8 + k as i32 * zw;
            p.cell((zx + 1, zy, zx + zw - 2, zy + zh - 1), v as u64, move |d| {
                fill_round(d, zx + 1, zy, zw - 2, zh, 3, tyre_tcol(v));
                let ts = format::format(v, Fmt::Fixed1, 10, "");
                text(
                    d,
                    &ts,
                    zx + zw / 2,
                    zy + zh / 2,
                    10,
                    pal(Pal::Bg),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
            });
        }
        // average — the big focus number. The cell rect gets 3px of headroom top +
        // 2px bottom: this big glyph overhangs a tight box, and the erase/blit must
        // cover the overhang or a stale row lingers at the top. The 4px gap above is
        // panel bg, so erasing into it is safe.
        let av = avg[i];
        p.cell(
            (px + 8, avg_top - 3, px + cw - 8, avg_top + avg_sz + 1),
            av as u64,
            move |d| {
                let ts = format::format(av, Fmt::Fixed1, 10, "°");
                text(
                    d,
                    &ts,
                    px + cw / 2,
                    avg_cy,
                    avg_sz as u32,
                    tyre_tcol(av),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
            },
        );
        // stat values
        for ri in 0..4 {
            let (val, raw) = match ri {
                0 => (format::format(press[i], Fmt::Fixed1, 10, " kPa"), press[i]),
                1 => (format::format(brake[i], Fmt::Fixed1, 10, "°C"), brake[i]),
                2 => (format::format(wear[i], Fmt::Int, 1, "%"), wear[i]),
                _ => (String::from(tyre_compound(comp[i])), comp[i]),
            };
            let ry = sy + ri * lh + lh / 2;
            let vx0 = px + cw * 2 / 5;
            let vx1 = px + cw - 6;
            p.cell(
                (vx0, ry - lh / 2, vx1, ry + lh / 2 - 1),
                raw as u64,
                move |d| {
                    text(
                        d,
                        &val,
                        px + cw - 12,
                        ry,
                        13,
                        pal(Pal::White),
                        HorizontalAlignment::Right,
                        VerticalPosition::Center,
                    );
                },
            );
        }
    }
}

/// LMU energy-regulated cars (Hypercar/LMDh) show fuel as Virtual Energy **%**,
/// not litres. When `fuel_is_ve` is set and the bound field is a fuel channel,
/// substitute the VE channel + a "%" unit so an ordinary Fuel / Fuel-per-lap
/// widget reads correctly without a separate layout. Field ids are registry
/// positions (append-only, asserted in a test): fuel_dl=23, fuel_per_lap_ml=25,
/// virtual_energy=87, ve_per_lap=88. Returns (field, fmt, scale, unit) overrides.
fn ve_swap(field: u8, t: &Telemetry) -> Option<(usize, Fmt, i32, &'static str)> {
    if t.fuel_is_ve == 0 {
        return None;
    }
    let sub = match field {
        23 => 87, // fuel_dl → virtual_energy
        25 => 88, // fuel_per_lap_ml → ve_per_lap
        _ => return None,
    };
    let def = field_def(sub);
    Some((
        sub,
        def.map(|d| d.fmt).unwrap_or(Fmt::Fixed1),
        def.map(|d| d.scale).unwrap_or(10),
        "%",
    ))
}

/// `active` is a bitmask of HID buttons (bit = hid-1) currently pressed/toggled-on,
/// so a button can light up while you're touching it. 0 = nothing active.
fn draw_kind<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    r: &Rect,
    kind: &Kind,
    t: &Telemetry,
    now_ms: i64,
    active: u32,
    car: &CarData,
    rel: &Relatives,
) {
    let (x, y, w, h) = (r.x, r.y, r.w as i32, r.h as i32);
    let cx = x + w / 2;
    match kind {
        Kind::Panel { color, radius } => {
            fill_round(d, x, y, w, h, *radius as i32, pal(*color));
        }
        Kind::Label {
            text: s,
            color,
            size,
            align,
            valign,
        } => {
            let sz = if *size == 0 { 14 } else { *size as u32 };
            let ax = match align {
                Align::Left => x + 2,
                Align::Center => cx,
                Align::Right => x + w - 2,
            };
            let (ay, vp) = valign.place(y, h);
            text(d, s, ax, ay, sz, pal(*color), align.h(), vp);
        }
        Kind::Stat {
            field,
            label,
            fmt,
            scale,
            unit,
            base,
            rules,
            size,
        } => {
            let (raw, f, sc, unit): (i32, Fmt, i32, &str) =
                if let Some((fid, vf, vsc, vu)) = ve_swap(*field, t) {
                    (field_value(t, fid), vf, vsc, vu)
                } else {
                    let def = field_def(*field as usize);
                    let f = fmt.unwrap_or_else(|| def.map(|d| d.fmt).unwrap_or(Fmt::Int));
                    let sc = if *scale > 0 {
                        *scale
                    } else {
                        def.map(|d| d.scale).unwrap_or(1)
                    };
                    (field_value(t, *field as usize), f, sc, unit.as_str())
                };
            let sz = if *size == 0 { 22 } else { *size as u32 };
            text(
                d,
                label,
                cx,
                y + 11,
                11,
                pal(Pal::Dim),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
            let s = format::format(raw, f, sc, unit);
            text(
                d,
                &s,
                cx,
                y + h / 2 + 6,
                sz,
                rule_color(raw, *base, rules),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
        }
        Kind::Bar {
            field,
            label,
            scale,
            base,
            rules,
        } => {
            // VE cars: a fuel bar represents Virtual Energy 0..100% (full-scale 1000).
            let (fid, scale) = if t.fuel_is_ve != 0 && (*field == 23 || *field == 25) {
                (87usize, 1000)
            } else {
                (*field as usize, *scale)
            };
            let raw = field_value(t, fid);
            let pct = if scale > 0 {
                (raw * 100 / scale).clamp(0, 100)
            } else {
                0
            };
            text(
                d,
                label,
                x + 4,
                y + 10,
                11,
                pal(Pal::Dim),
                HorizontalAlignment::Left,
                VerticalPosition::Center,
            );
            fill_rect(d, x + 4, y + h / 2, w - 8, h / 3, pal(Pal::Panel));
            fill_rect(
                d,
                x + 4,
                y + h / 2,
                (w - 8) * pct / 100,
                h / 3,
                rule_color(raw, *base, rules),
            );
        }
        Kind::GearSpeed { speed } => {
            let g = if t.gear == 0 { 'N' } else { t.gear as char };
            // Clip to the widget rect: the big gear font can extend past the box top,
            // and the dirty-rect blit only clears within the rect — so any overflow
            // would never be erased (ghost trails). Clipping keeps it inside the rect.
            let area = Rectangle::new(
                Point::new(x, y),
                Size::new(w.max(0) as u32, h.max(0) as u32),
            );
            let mut cd = d.clipped(&area);
            if *speed {
                // gear in the upper area, speed + unit along the bottom
                let gsz = (h * 5 / 10).clamp(11, 46) as u32;
                text(
                    &mut cd,
                    &g.to_string(),
                    cx,
                    y + h * 4 / 10,
                    gsz,
                    pal(Pal::White),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
                text(
                    &mut cd,
                    &t.speed_kmh.to_string(),
                    cx,
                    y + h - 26,
                    24,
                    pal(Pal::White),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
                text(
                    &mut cd,
                    "KM/H",
                    cx,
                    y + h - 8,
                    11,
                    pal(Pal::Dim),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
            } else {
                // gear only: centred in the box, scaled to the largest font that fits
                let gsz = (h - 14).clamp(11, 58) as u32;
                text(
                    &mut cd,
                    &g.to_string(),
                    cx,
                    y + h / 2,
                    gsz,
                    pal(Pal::White),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
            }
        }
        Kind::RpmStrip { count } => {
            let seg = if *count == 0 {
                12
            } else {
                (*count as i32).clamp(1, 48)
            };
            let sw = w / seg;
            for i in 0..seg {
                let c = segment_rgb(t, i, seg, &RevCfg::default(), car, now_ms);
                let col = if c == 0 { pal(Pal::Panel) } else { rgb888(c) };
                fill_round(d, x + i * sw + 1, y + 4, sw - 2, h - 8, 3, col);
            }
        }
        Kind::TyreGrid => {
            // Temps are 0.1°C — the per-tyre AVERAGE (mean of the inner/mid/outer
            // zones), matching the in-game HUD readout. Thresholds: >95°C / >80°C.
            // NA (no reading, e.g. uninitialised wheel) renders dim "--".
            let temps = [t.tt_avg_fl, t.tt_avg_fr, t.tt_avg_rl, t.tt_avg_rr];
            let (bw, bh) = (w / 2, h / 2);
            for (i, &tv) in temps.iter().enumerate() {
                let (cxx, cyy) = (x + (i as i32 % 2) * bw, y + (i as i32 / 2) * bh);
                let col = if tv == format::NA {
                    pal(Pal::Dim)
                } else if tv > 950 {
                    pal(Pal::Red)
                } else if tv > 800 {
                    pal(Pal::Amber)
                } else {
                    pal(Pal::Green)
                };
                fill_round(d, cxx + 2, cyy + 2, bw - 4, bh - 4, 4, pal(Pal::Panel));
                let s = format::format(tv, Fmt::Fixed1, 10, "°C");
                text(
                    d,
                    &s,
                    cxx + bw / 2,
                    cyy + bh / 2,
                    13,
                    col,
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
            }
        }
        Kind::TyrePanel => {
            // Standalone nodes use the per-cell dirty path (render_screen_dirty);
            // here (composed widgets / preview) just do a full paint.
            let mut cache = Vec::new();
            let mut tr = Vec::new();
            let mut p = Painter::new(d, &mut cache, &mut tr, true, pal(Pal::Bg), w, h);
            paint_tyre_panel(&mut p, r, t);
        }
        Kind::TcDual => {
            text(
                d,
                "TC",
                x + w / 4,
                y + 12,
                11,
                pal(Pal::Dim),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
            text(
                d,
                "ABS",
                x + 3 * w / 4,
                y + 12,
                11,
                pal(Pal::Dim),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
            text(
                d,
                &t.tc.to_string(),
                x + w / 4,
                y + h / 2 + 6,
                22,
                pal(Pal::White),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
            text(
                d,
                &t.abs.to_string(),
                x + 3 * w / 4,
                y + h / 2 + 6,
                22,
                pal(Pal::White),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
        }
        Kind::TcTriple => {
            // Three TC channels: level / slip / cut.
            let cols = [("TC", t.tc), ("SLIP", t.tc_slip), ("CUT", t.tc_cut)];
            for (k, (lbl, v)) in cols.iter().enumerate() {
                let cx = x + w * (2 * k as i32 + 1) / 6;
                text(
                    d,
                    lbl,
                    cx,
                    y + 12,
                    11,
                    pal(Pal::Dim),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
                text(
                    d,
                    &v.to_string(),
                    cx,
                    y + h / 2 + 6,
                    20,
                    pal(Pal::White),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
            }
        }
        Kind::Sectors => {
            let secs = [t.s1_ms, t.s2_ms, t.s3_ms];
            let bs = [t.bs1_ms, t.bs2_ms, t.bs3_ms];
            let sw = w / 3;
            for i in 0..3 {
                let col = if secs[i] > 0 && bs[i] > 0 && secs[i] <= bs[i] {
                    pal(Pal::Green)
                } else {
                    pal(Pal::Amber)
                };
                let s = format::format(secs[i], Fmt::Sector, 1, "");
                text_mono(
                    d,
                    &s,
                    x + i as i32 * sw + sw / 2,
                    y + h / 2,
                    12,
                    col,
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
            }
        }
        Kind::LapPair => {
            let cur = format::format(t.cur_lap_ms, Fmt::Time, 1, "");
            let best = format::format(t.best_lap_ms, Fmt::Time, 1, "");
            text(
                d,
                "CURRENT",
                cx,
                y + 10,
                11,
                pal(Pal::Dim),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
            text_mono(
                d,
                &cur,
                cx,
                y + h / 4 + 6,
                18,
                pal(Pal::White),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
            text(
                d,
                "BEST",
                cx,
                y + h / 2 + 8,
                11,
                pal(Pal::Dim),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
            text_mono(
                d,
                &best,
                cx,
                y + 3 * h / 4 + 4,
                18,
                pal(Pal::Cyan),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
        }
        Kind::Position { label } => {
            text(
                d,
                label,
                cx,
                y + 12,
                11,
                pal(Pal::Dim),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
            let s = alloc::format!("P{}/{}", t.position, t.field_size);
            text(
                d,
                &s,
                cx,
                y + h / 2 + 4,
                22,
                pal(Pal::White),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
        }
        Kind::Flag { field, base, rules } => {
            let raw = field_value(t, *field as usize);
            fill_round(
                d,
                x + 4,
                y + 4,
                w - 8,
                h - 8,
                4,
                rule_color(raw, *base, rules),
            );
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
        Kind::Value {
            field,
            fmt,
            scale,
            unit,
            base,
            rules,
            size,
            align,
            valign,
        } => {
            let (raw, f, sc, unit): (i32, Fmt, i32, &str) =
                if let Some((fid, vf, vsc, vu)) = ve_swap(*field, t) {
                    (field_value(t, fid), vf, vsc, vu)
                } else {
                    let def = field_def(*field as usize);
                    let f = fmt.unwrap_or_else(|| def.map(|d| d.fmt).unwrap_or(Fmt::Int));
                    let sc = if *scale > 0 {
                        *scale
                    } else {
                        def.map(|d| d.scale).unwrap_or(1)
                    };
                    (field_value(t, *field as usize), f, sc, unit.as_str())
                };
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
        Kind::Button {
            label,
            color,
            toggle,
            field,
            hid,
            on_color,
            ..
        } => {
            // Toggles reflect game state ONLY (the bound field, >0 = on) — no latch,
            // no press overlay, no outline. Momentary (push) buttons glow while the
            // button is physically pressed (the `active` HID bit). Value is never shown.
            let bit_on = *hid > 0 && (active >> (*hid as u32 - 1)) & 1 == 1;
            let field_on = *field > 0 && field_value(t, *field as usize) > 0;
            let on = if *toggle || *field > 0 {
                field_on
            } else {
                bit_on
            };
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
                    Rectangle::new(
                        Point::new(x + 1, y + 1),
                        Size::new((w - 2).max(0) as u32, (h - 2).max(0) as u32),
                    ),
                    Size::new(6, 6),
                )
                .into_styled(PrimitiveStyle::with_stroke(pal(Pal::White), 2))
                .draw(d);
            }
            // label only (no value), in a colour that contrasts the fill
            text(
                d,
                label,
                cx,
                y + h / 2,
                14,
                contrast(bg),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
        }
        Kind::Widget(el) => layout_draw(d, r, el, t, now_ms, car, rel),
        Kind::Relatives { mode, rows } => draw_relatives(d, r, *mode, *rows, rel),
    }
    let _ = active;
}

/// Draw the multi-car relatives/standings table. `mode` 0 = relative (cars nearest
/// the player on track, signed track gap), 1 = standings (by position, gap to
/// leader). The host already selected the cars; the widget sorts + windows them.
fn draw_relatives<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    r: &Rect,
    mode: u8,
    rows: u8,
    rel: &Relatives,
) {
    let (x, y, w, h) = (r.x, r.y, r.w as i32, r.h as i32);
    let cx = x + w / 2;
    let entries = rel.entries();
    if entries.is_empty() {
        text(
            d,
            "no cars",
            cx,
            y + h / 2,
            12,
            pal(Pal::Dim),
            HorizontalAlignment::Center,
            VerticalPosition::Center,
        );
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
        let pp = idx
            .iter()
            .position(|&i| entries[i].is_player())
            .unwrap_or(0);
        let half = want / 2;
        let start = pp
            .saturating_sub(half)
            .min(idx.len().saturating_sub(want.min(idx.len())));
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
        let fg = if c.in_pits() {
            pal(Pal::Dim)
        } else {
            pal(Pal::White)
        };
        let label = alloc::format!("P{} {}", c.place, c.name_str());
        text(
            d,
            &label,
            x + 4,
            ry + row_h / 2,
            tsz,
            fg,
            HorizontalAlignment::Left,
            VerticalPosition::Center,
        );
        let (gap, signed) = if mode == 1 {
            (c.gap_leader_ms, false)
        } else {
            (c.gap_rel_ms, true)
        };
        let gs = if player && signed {
            alloc::string::String::from("--")
        } else {
            fmt_gap(gap, signed)
        };
        let gcol = if signed {
            if gap > 0 {
                pal(Pal::Red)
            } else if gap < 0 {
                pal(Pal::Green)
            } else {
                pal(Pal::Cyan)
            }
        } else {
            pal(Pal::Cyan)
        };
        text_mono(
            d,
            &gs,
            x + w - 4,
            ry + row_h / 2,
            tsz,
            gcol,
            HorizontalAlignment::Right,
            VerticalPosition::Center,
        );
    }
}

/// FNV-1a over i64s — a cheap content signature for Painter cells.
fn fnv64(vals: &[i64]) -> u64 {
    let mut x: u64 = 0xcbf29ce484222325;
    for &v in vals {
        x ^= v as u64;
        x = x.wrapping_mul(0x100000001b3);
    }
    x
}

fn name_sig(s: &str) -> i64 {
    let mut x: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        x ^= b as u64;
        x = x.wrapping_mul(0x100000001b3);
    }
    x as i64
}

/// Relatives/Standings via `Painter` — one cell per ROW (plus a structural cell
/// that clears the whole node when the row count/mode changes so vacated rows
/// don't ghost). Each row only redraws when its own data (place/name/gap/pit/
/// player) changes, so a busy standings table no longer reblits every row every
/// tick — that's the slowness you saw on those pages.
fn paint_relatives<D: DrawTarget<Color = Rgb565>>(
    p: &mut Painter<D>,
    r: &Rect,
    mode: u8,
    rows: u8,
    rel: &Relatives,
) {
    let (x, y, w, h) = (r.x, r.y, r.w as i32, r.h as i32);
    let node_rect = (x, y, x + w - 1, y + h - 1);
    let entries = rel.entries();
    if entries.is_empty() {
        p.cell(node_rect, 1, move |d| {
            text(
                d,
                "no cars",
                x + w / 2,
                y + h / 2,
                12,
                pal(Pal::Dim),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
        });
        return;
    }
    let want = if rows == 0 { 6 } else { rows as usize };
    let mut idx: Vec<usize> = (0..entries.len()).collect();
    if mode == 1 {
        idx.sort_by_key(|&i| entries[i].place);
        idx.truncate(want);
    } else {
        idx.sort_by(|&a, &b| entries[b].gap_rel_ms.cmp(&entries[a].gap_rel_ms));
        let pp = idx
            .iter()
            .position(|&i| entries[i].is_player())
            .unwrap_or(0);
        let half = want / 2;
        let start = pp
            .saturating_sub(half)
            .min(idx.len().saturating_sub(want.min(idx.len())));
        let end = (start + want).min(idx.len());
        idx = idx[start..end].to_vec();
    }
    let n = idx.len().max(1) as i32;
    let row_h = (h / n).max(1);
    let tsz = (row_h - 4).clamp(9, 16) as u32;
    // Structural cell: clears the whole node when the row count or mode changes.
    p.cell(node_rect, fnv64(&[idx.len() as i64, mode as i64]), |_d| {});
    for (row, &i) in idx.iter().enumerate() {
        let c = &entries[i];
        let ry = y + row as i32 * row_h;
        let player = c.is_player();
        let in_pits = c.in_pits();
        let (gap, signed) = if mode == 1 {
            (c.gap_leader_ms, false)
        } else {
            (c.gap_rel_ms, true)
        };
        let sig = fnv64(&[
            c.place as i64,
            name_sig(c.name_str()),
            in_pits as i64,
            gap as i64,
            player as i64,
            idx.len() as i64,
            mode as i64,
        ]);
        let label = alloc::format!("P{} {}", c.place, c.name_str());
        let gs = if player && signed {
            alloc::string::String::from("--")
        } else {
            fmt_gap(gap, signed)
        };
        let gcol = if signed {
            if gap > 0 {
                pal(Pal::Red)
            } else if gap < 0 {
                pal(Pal::Green)
            } else {
                pal(Pal::Cyan)
            }
        } else {
            pal(Pal::Cyan)
        };
        p.cell((x, ry, x + w - 1, ry + row_h - 1), sig, move |d| {
            if player {
                fill_round(d, x + 1, ry + 1, w - 2, row_h - 2, 3, pal(Pal::Panel));
            }
            let fg = if in_pits {
                pal(Pal::Dim)
            } else {
                pal(Pal::White)
            };
            text(
                d,
                &label,
                x + 4,
                ry + row_h / 2,
                tsz,
                fg,
                HorizontalAlignment::Left,
                VerticalPosition::Center,
            );
            text_mono(
                d,
                &gs,
                x + w - 4,
                ry + row_h / 2,
                tsz,
                gcol,
                HorizontalAlignment::Right,
                VerticalPosition::Center,
            );
        });
    }
}

/// Paint a per-cell (Painter-based) multi-value widget. Returns `Some(dirty)` when
/// `node` is one of these widgets (handled here), or `None` for ordinary node-level
/// widgets the caller should draw the normal way. New multi-value widgets get
/// fine-grained dirty just by adding an arm here.
fn paint_node<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    node: &Node,
    t: &Telemetry,
    rel: &Relatives,
    cache: &mut Vec<u64>,
    rects: &mut Vec<(i32, i32, i32, i32)>,
    full: bool,
    bg: Rgb565,
    sw: i32,
    sh: i32,
) -> Option<Option<(i32, i32, i32, i32)>> {
    let mut p = Painter::new(d, cache, rects, full, bg, sw, sh);
    match &node.kind {
        Kind::TyrePanel => {
            paint_tyre_panel(&mut p, &node.rect, t);
            Some(p.finish())
        }
        Kind::Relatives { mode, rows } => {
            paint_relatives(&mut p, &node.rect, *mode, *rows, rel);
            Some(p.finish())
        }
        // Tall gear+speed widgets split the huge gear glyph and the speed text
        // into separate cells: at speed the km/h ticks every frame, and without
        // the split each tick erased + redrew + re-blitted the whole widget —
        // by far the largest glyph on the screen — for a one-digit change.
        // Short widgets keep the single-node path (the two texts sit too close
        // there for per-cell erases not to chew into each other).
        Kind::GearSpeed { speed: true } if node.rect.h >= 110 => {
            paint_gear_speed(&mut p, &node.rect, t);
            Some(p.finish())
        }
        _ => None,
    }
}

/// Per-cell gear+speed panel (tall variant only — see the caller). Geometry is
/// identical to `draw_kind`'s `Kind::GearSpeed { speed: true }` arm; each text
/// is clipped to its own cell so an erase can never bleed into the other's.
fn paint_gear_speed<D: DrawTarget<Color = Rgb565>>(p: &mut Painter<D>, r: &Rect, t: &Telemetry) {
    let (x, y, w, h) = (r.x, r.y, r.w as i32, r.h as i32);
    let cx = x + w / 2;
    let split = y + h - 40; // bottom band = speed value + "KM/H" unit
    let g = if t.gear == 0 { 'N' } else { t.gear as char };
    let gsz = (h * 5 / 10).clamp(11, 46) as u32;
    p.cell((x, y, x + w - 1, split - 1), t.gear as u64, |d| {
        let area = Rectangle::new(
            Point::new(x, y),
            Size::new(w.max(0) as u32, (split - y).max(0) as u32),
        );
        let mut cd = d.clipped(&area);
        text(
            &mut cd,
            &g.to_string(),
            cx,
            y + h * 4 / 10,
            gsz,
            pal(Pal::White),
            HorizontalAlignment::Center,
            VerticalPosition::Center,
        );
    });
    p.cell(
        (x, y + h - 39, x + w - 1, y + h - 14),
        t.speed_kmh as u64,
        |d| {
            let area = Rectangle::new(Point::new(x, y + h - 39), Size::new(w.max(0) as u32, 26));
            let mut cd = d.clipped(&area);
            text(
                &mut cd,
                &t.speed_kmh.to_string(),
                cx,
                y + h - 26,
                24,
                pal(Pal::White),
                HorizontalAlignment::Center,
                VerticalPosition::Center,
            );
        },
    );
    p.chrome(|d| {
        text(
            d,
            "KM/H",
            cx,
            y + h - 8,
            11,
            pal(Pal::Dim),
            HorizontalAlignment::Center,
            VerticalPosition::Center,
        );
    });
}

/// Format a gap in ms as `S.s` (relative gaps signed, standings gaps unsigned),
/// integer-only so columns stay fixed-width under `text_mono`.
fn fmt_gap(ms: i32, signed: bool) -> String {
    let a = ms.unsigned_abs();
    let (whole, tenths) = (a / 1000, (a % 1000) / 100);
    let sign = if !signed || ms == 0 {
        ""
    } else if ms > 0 {
        "+"
    } else {
        "-"
    };
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
fn layout_draw<D: DrawTarget<Color = Rgb565>>(
    d: &mut D,
    r: &Rect,
    el: &El,
    t: &Telemetry,
    now_ms: i64,
    car: &CarData,
    rel: &Relatives,
) {
    match el {
        El::Leaf(k) => draw_kind(d, r, k, t, now_ms, 0, car, rel),
        El::Stack { pad, children } => {
            let inner = inset(r, *pad as i32);
            for c in children {
                layout_draw(d, &inner, c, t, now_ms, car, rel);
            }
        }
        El::Tabs {
            titles,
            active,
            pages,
        } => {
            // tab strip across the top, active page fills the rest
            let strip_h = 22.min(r.h as i32 / 4).max(14);
            let n = titles.len().max(1) as i32;
            let tw = r.w as i32 / n;
            for (i, title) in titles.iter().enumerate() {
                let tx = r.x + i as i32 * tw;
                let on = i as u8 == *active;
                fill_round(
                    d,
                    tx + 1,
                    r.y + 1,
                    tw - 2,
                    strip_h - 2,
                    3,
                    pal(if on { Pal::Panel } else { Pal::Bg }),
                );
                text(
                    d,
                    title,
                    tx + tw / 2,
                    r.y + strip_h / 2,
                    11,
                    pal(if on { Pal::White } else { Pal::Dim }),
                    HorizontalAlignment::Center,
                    VerticalPosition::Center,
                );
            }
            if let Some(page) = pages.get(*active as usize) {
                let body = Rect {
                    x: r.x,
                    y: r.y + strip_h,
                    w: r.w,
                    h: (r.h as i32 - strip_h).max(0) as u32,
                };
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
                let cr = Rect {
                    x: cx,
                    y: inner.y,
                    w: cw.max(0) as u32,
                    h: inner.h,
                };
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
                let cr = Rect {
                    x: inner.x,
                    y: cy,
                    w: inner.w,
                    h: ch.max(0) as u32,
                };
                layout_draw(d, &cr, &s.el, t, now_ms, car, rel);
                cy += ch + *gap as i32;
            }
        }
    }
}

// ============ rendering: full + dirty-rect ============

/// Per-node content signature (FNV-1a of the telemetry that affects the node's
/// pixels). Static kinds hash to a constant so they draw once and never repaint.
/// The goal is to hash the node's VISUAL state, not its raw inputs — a raw value
/// change that doesn't move a pixel (a bar whose integer percent is unchanged, a
/// rev strip where no LED toggled) must NOT dirty the node, or busy telemetry
/// repaints + re-blits the whole screen every frame for nothing.
fn node_sig(kind: &Kind, t: &Telemetry, now_ms: i64, active: u32, car: &CarData) -> u64 {
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
        // Hash the field the draw path actually shows — on VE cars Stat/Value
        // swap fuel fields for virtual-energy (ve_swap), so hashing the raw
        // field would leave the swapped value STALE on screen.
        Kind::Stat { field, .. } | Kind::Value { field, .. } => {
            let fid = ve_swap(*field, t)
                .map(|(id, ..)| id)
                .unwrap_or(*field as usize);
            h(&[field_value(t, fid) as i64])
        }
        Kind::Flag { field, .. } => h(&[fv(*field)]),
        // A bar's fill is quantised to integer percent and its colour to the
        // matched rule band — hash those, not the raw value (an RPM-bound bar
        // otherwise repaints on every single-RPM tick that moves no pixels).
        Kind::Bar {
            field,
            scale,
            rules,
            ..
        } => {
            let (fid, scale) = if t.fuel_is_ve != 0 && (*field == 23 || *field == 25) {
                (87usize, 1000)
            } else {
                (*field as usize, *scale)
            };
            let raw = field_value(t, fid);
            let pct = if scale > 0 {
                (raw * 100 / scale).clamp(0, 100)
            } else {
                0
            };
            let band = rules
                .iter()
                .position(|r| r.op.matches(raw, r.threshold))
                .map(|i| i as i64 + 1)
                .unwrap_or(0);
            h(&[pct as i64, band])
        }
        // include the press/toggle state so a button repaints on press + release
        Kind::Button { field, hid, .. } => {
            let on = if *hid > 0 {
                (active >> (*hid as u32 - 1)) & 1
            } else {
                0
            };
            h(&[fv(*field), on as i64])
        }
        Kind::GearSpeed { speed } => {
            h(&[t.gear as i64, if *speed { t.speed_kmh as i64 } else { 0 }])
        }
        // Hash the strip's exact visual state (which LEDs are lit + the flash
        // phase, from the same math segment_rgb draws with). The old sig hashed
        // raw rpm + a free-running 80 ms tick, so every strip repainted 12×/s
        // even parked in the pits with the engine off.
        Kind::RpmStrip { count } => {
            let seg = if *count == 0 {
                12
            } else {
                (*count as i32).clamp(1, 48)
            };
            if car.valid {
                let gi = shift::gear_index(t.gear);
                let rl = car.redline[gi] as i32;
                let over = rl > 0 && t.rpm >= rl;
                let mut mask: i64 = 0;
                for i in 0..car.led_count.min(shift::CAR_LED_MAX) {
                    if car.thresh[gi][i] > 0 && t.rpm >= car.thresh[gi][i] as i32 {
                        mask |= 1 << i;
                    }
                }
                let phase = match (over, car.blink_ms) {
                    (false, _) => 0,
                    (true, 0) => 1, // hold solid at redline (no strobe)
                    (true, ms) => 2 + ((now_ms / ms as i64) & 1),
                };
                h(&[mask, phase, seg as i64])
            } else {
                let cfg = RevCfg::default();
                let shift = shift::shift_rpm_of(t);
                let start = shift * cfg.start_pct as i32 / 100;
                let span = shift - start;
                let lit = if span > 0 {
                    ((t.rpm - start) * seg / span).clamp(0, seg)
                } else {
                    0
                };
                let flashing = shift > 0 && t.rpm * 100 / shift >= cfg.flash_pct as i32;
                let phase = if flashing {
                    1 + ((now_ms / shift::FLASH_MS) & 1)
                } else {
                    0
                };
                h(&[lit as i64, phase, seg as i64])
            }
        }
        Kind::TyreGrid => h(&[
            t.tt_avg_fl as i64,
            t.tt_avg_fr as i64,
            t.tt_avg_rl as i64,
            t.tt_avg_rr as i64,
        ]),
        Kind::TyrePanel => h(&[
            t.tt_fl_i as i64,
            t.tt_fl_m as i64,
            t.tt_fl_o as i64,
            t.tt_fr_i as i64,
            t.tt_fr_m as i64,
            t.tt_fr_o as i64,
            t.tt_rl_i as i64,
            t.tt_rl_m as i64,
            t.tt_rl_o as i64,
            t.tt_rr_i as i64,
            t.tt_rr_m as i64,
            t.tt_rr_o as i64,
            t.tp_fl as i64,
            t.tp_fr as i64,
            t.tp_rl as i64,
            t.tp_rr as i64,
            t.tw_fl as i64,
            t.tw_fr as i64,
            t.tw_rl as i64,
            t.tw_rr as i64,
            t.bt_fl as i64,
            t.bt_fr as i64,
            t.bt_rl as i64,
            t.bt_rr as i64,
            t.comp_fl as i64,
            t.comp_fr as i64,
            t.comp_rl as i64,
            t.comp_rr as i64,
        ]),
        Kind::TcDual => h(&[t.tc as i64, t.abs as i64]),
        Kind::TcTriple => h(&[t.tc as i64, t.tc_slip as i64, t.tc_cut as i64]),
        Kind::Sectors => h(&[
            t.s1_ms as i64,
            t.s2_ms as i64,
            t.s3_ms as i64,
            t.bs1_ms as i64,
            t.bs2_ms as i64,
            t.bs3_ms as i64,
        ]),
        Kind::LapPair => h(&[t.cur_lap_ms as i64, t.best_lap_ms as i64]),
        Kind::Position { .. } => h(&[t.position as i64, t.field_size as i64]),
        Kind::Widget(el) => el_sig(el, t, now_ms, car),
        // Relatives data rides a side channel node_sig can't see; repaint on a
        // ~4 Hz tick so live gaps refresh without threading the list in here.
        Kind::Relatives { mode, rows } => h(&[*mode as i64, *rows as i64, now_ms / 250]),
    }
}

/// Combine the signatures of an element tree (so a composed widget repaints iff
/// any of its leaves' telemetry changed).
fn el_sig(el: &El, t: &Telemetry, now_ms: i64, car: &CarData) -> u64 {
    fn mix(a: u64, b: u64) -> u64 {
        (a ^ b).wrapping_mul(0x100000001b3)
    }
    match el {
        El::Leaf(k) => node_sig(k, t, now_ms, 0, car),
        El::Stack { children, .. } => children
            .iter()
            .fold(0, |a, c| mix(a, el_sig(c, t, now_ms, car))),
        El::Row { children, .. } | El::Col { children, .. } => children
            .iter()
            .fold(0, |a, s| mix(a, el_sig(&s.el, t, now_ms, car))),
        El::Tabs { active, pages, .. } => {
            let base = mix(0x9e3779b9, *active as u64);
            match pages.get(*active as usize) {
                Some(p) => mix(base, el_sig(p, t, now_ms, car)),
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
    cells: Vec<Vec<u64>>, // per-node Painter cell-signature cache (multi-value widgets)
    next_ms: Vec<i64>, // per-node earliest time it may be reconsidered (throttled kinds only)
}

impl RenderCache {
    pub fn new() -> Self {
        Self {
            sigs: Vec::new(),
            last_tab: -1,
            cells: Vec::new(),
            next_ms: Vec::new(),
        }
    }
    /// Force a full repaint on the next [`render_screen_diff`] (e.g. after a layout
    /// swap or display wake).
    pub fn invalidate(&mut self) {
        self.sigs.clear();
        self.cells.clear();
        self.next_ms.clear();
        self.last_tab = -1;
    }
}

/// Minimum time between dirty-checks for widgets that don't need to track live
/// telemetry every frame. Decouples them from the "race screen" gauges (RPM,
/// gear, shift lights, ...) which always check every frame — a busy map or
/// standings redraw should never eat into their SPI/blit budget. 0 = no
/// throttle (current every-frame behaviour).
fn throttle_ms(kind: &Kind) -> i64 {
    match kind {
        Kind::Map { .. } => 200,       // car dot creeping around a lap outline
        Kind::Relatives { .. } => 400, // standings/gaps don't need to be that live
        _ => 0,
    }
}

/// Full repaint: clear to the screen background and draw every node.
pub fn render_screen<D: DrawTarget<Color = Rgb565>>(
    s: &Screen,
    t: &Telemetry,
    now_ms: i64,
    active: u32,
    car: &CarData,
    rel: &Relatives,
    d: &mut D,
) {
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
        cache.cells.clear();
        cache.cells.resize(s.nodes.len(), Vec::new());
        cache.next_ms.clear();
        cache.next_ms.resize(s.nodes.len(), 0);
    }
    let mut dirty: Option<(i32, i32, i32, i32)> = None;
    for (i, node) in s.nodes.iter().enumerate() {
        // Throttled kinds (Map, Relatives) get checked on their own slower cadence —
        // skip entirely until due, so they never compete with the live gauges.
        let thr = throttle_ms(&node.kind);
        if thr > 0 && !full && now_ms < cache.next_ms[i] {
            continue;
        }
        if thr > 0 {
            cache.next_ms[i] = now_ms + thr;
        }
        // Multi-value widgets (TyrePanel, Relatives) self-manage per-cell dirty via
        // the Painter, so one changing value doesn't reblit the whole widget.
        if let Some(d2opt) = paint_node(
            d,
            node,
            t,
            rel,
            &mut cache.cells[i],
            rects,
            full,
            pal(s.bg),
            s.w as i32,
            s.h as i32,
        ) {
            if let Some(d2) = d2opt {
                dirty = Some(match dirty {
                    None => d2,
                    Some((a0, b0, a1, b1)) => {
                        (a0.min(d2.0), b0.min(d2.1), a1.max(d2.2), b1.max(d2.3))
                    }
                });
            }
            cache.sigs[i] = node_sig(&node.kind, t, now_ms, active, car);
            continue;
        }
        let sig = node_sig(&node.kind, t, now_ms, active, car);
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
                let rr = inflate((x0, y0, x1, y1), s.w as i32, s.h as i32);
                rects.push(rr);
                dirty = Some(match dirty {
                    None => rr,
                    Some((ax0, ay0, ax1, ay1)) => {
                        (ax0.min(rr.0), ay0.min(rr.1), ax1.max(rr.2), ay1.max(rr.3))
                    }
                });
            } else {
                dirty = Some(match dirty {
                    None => (x0, y0, x1, y1),
                    Some((ax0, ay0, ax1, ay1)) => {
                        (ax0.min(x0), ay0.min(y0), ax1.max(x1), ay1.max(y1))
                    }
                });
            }
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

/// Height of the tab strip on a tabbed screen. Tall enough to be an easy touch
/// target (the tab row is the only fixed-position control on a tabbed screen).
pub const TAB_STRIP_H: i32 = 40;

/// Which tab a tap at (tx,ty) lands on, if it's in the strip of an `n`-tab screen
/// of width `w`. None if the tap is below the strip (i.e. in the page body).
pub fn tab_at(w: u32, n: usize, tx: i32, ty: i32) -> Option<u8> {
    if n == 0 || !(0..TAB_STRIP_H).contains(&ty) || tx < 0 || tx >= w as i32 {
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
        fill_round(
            d,
            tx + 1,
            1,
            tw - 2,
            TAB_STRIP_H - 2,
            3,
            pal(if on { Pal::Panel } else { Pal::Bg }),
        );
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
        fill_round(
            d,
            tx + 1,
            1,
            tw - 2,
            TAB_STRIP_H - 2,
            3,
            pal(if on { Pal::Panel } else { Pal::Bg }),
        );
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
        cache.cells.clear();
        cache.cells.resize(page.len(), Vec::new());
        cache.next_ms.clear();
        cache.next_ms.resize(page.len(), 0);
        cache.last_tab = active as i32;
        for (i, node) in page.iter().enumerate() {
            let thr = throttle_ms(&node.kind);
            if thr > 0 {
                cache.next_ms[i] = now_ms + thr;
            }
            if paint_node(
                d,
                node,
                t,
                rel,
                &mut cache.cells[i],
                rects,
                true,
                pal(s.bg),
                s.w as i32,
                s.h as i32,
            )
            .is_none()
            {
                draw_kind(d, &node.rect, &node.kind, t, now_ms, pressed, car, rel);
            }
            cache.sigs[i] = node_sig(&node.kind, t, now_ms, pressed, car);
        }
        let whole = (0, 0, s.w as i32 - 1, s.h as i32 - 1);
        rects.push(whole);
        return Some(whole);
    }
    let mut dirty: Option<(i32, i32, i32, i32)> = None;
    for (i, node) in page.iter().enumerate() {
        // Throttled kinds (Map, Relatives) get checked on their own slower cadence —
        // skip entirely until due, so they never compete with the live gauges.
        let thr = throttle_ms(&node.kind);
        if thr > 0 && now_ms < cache.next_ms[i] {
            continue;
        }
        if thr > 0 {
            cache.next_ms[i] = now_ms + thr;
        }
        if let Some(d2opt) = paint_node(
            d,
            node,
            t,
            rel,
            &mut cache.cells[i],
            rects,
            false,
            pal(s.bg),
            s.w as i32,
            s.h as i32,
        ) {
            if let Some(d2) = d2opt {
                dirty = Some(match dirty {
                    None => d2,
                    Some((a0, b0, a1, b1)) => {
                        (a0.min(d2.0), b0.min(d2.1), a1.max(d2.2), b1.max(d2.3))
                    }
                });
            }
            cache.sigs[i] = node_sig(&node.kind, t, now_ms, pressed, car);
            continue;
        }
        let sig = node_sig(&node.kind, t, now_ms, pressed, car);
        if cache.sigs[i] != sig {
            let r = &node.rect;
            fill_rect(d, r.x, r.y, r.w as i32, r.h as i32, pal(s.bg));
            draw_kind(d, r, &node.kind, t, now_ms, pressed, car, rel);
            cache.sigs[i] = sig;
            let rect = inflate(
                (r.x, r.y, r.x + r.w as i32 - 1, r.y + r.h as i32 - 1),
                s.w as i32,
                s.h as i32,
            );
            rects.push(rect);
            dirty = Some(match dirty {
                None => rect,
                Some((ax0, ay0, ax1, ay1)) => (
                    ax0.min(rect.0),
                    ay0.min(rect.1),
                    ax1.max(rect.2),
                    ay1.max(rect.3),
                ),
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
            Self {
                w,
                h,
                buf: alloc::vec![Rgb565::BLACK; (w * h) as usize],
            }
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
    let stat =
        |x: i32, y: i32, w: u32, h: u32, label: &str, field: u8, base: Pal, rules: Vec<Rule>| {
            Node {
                rect: Rect { x, y, w, h },
                kind: Kind::Stat {
                    field,
                    label: label.into(),
                    fmt: None,
                    scale: 0,
                    unit: String::new(),
                    base,
                    rules,
                    size: 0,
                },
                page: 0,
            }
        };
    let panel = |x: i32, y: i32, w: u32, h: u32| Node {
        rect: Rect { x, y, w, h },
        kind: Kind::Panel {
            color: Pal::Panel,
            radius: 12,
        },
        page: 0,
    };
    let leaf = |x: i32, y: i32, w: u32, h: u32, kind: Kind| Node {
        rect: Rect { x, y, w, h },
        kind,
        page: 0,
    };
    let nodes = alloc::vec![
        leaf(0, 2, 480, 42, Kind::RpmStrip { count: 0 }),
        panel(136, 50, 208, 200),
        leaf(136, 50, 208, 200, Kind::GearSpeed { speed: true }),
        panel(4, 50, 128, 90),
        stat(
            4,
            50,
            128,
            90,
            "DELTA",
            10, /*delta_ms*/
            Pal::Amber,
            alloc::vec![
                Rule {
                    op: RuleOp::Lt,
                    threshold: 0,
                    color: Pal::Green
                },
                Rule {
                    op: RuleOp::Gt,
                    threshold: 0,
                    color: Pal::Red
                },
            ]
        ),
        panel(4, 150, 128, 120),
        leaf(4, 150, 128, 120, Kind::TyreGrid),
        panel(348, 50, 128, 90),
        stat(
            348,
            50,
            128,
            90,
            "FUEL",
            23, /*fuel_dl*/
            Pal::Amber,
            alloc::vec![]
        ),
        panel(348, 150, 128, 120),
        leaf(
            348,
            150,
            128,
            120,
            Kind::Position {
                label: "POS".into()
            }
        ),
        leaf(0, 276, 480, 42, Kind::LapPair),
    ];
    UiDoc {
        version: 1,
        screens: alloc::vec![Screen {
            display: 0,
            w: 480,
            h: 320,
            bg: Pal::Bg,
            nodes,
            tabs: Vec::new()
        }],
    }
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
    t.tt_fl_m = 880; // 0.1°C
    t.tt_fr_m = 900;
    t.tt_rl_m = 970;
    t.tt_rr_m = 860;
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
            nodes: Vec::from([Node {
                rect: Rect {
                    x: 0,
                    y: 0,
                    w: 120,
                    h: 80,
                },
                kind: k,
                page: 0,
            }]),
            tabs: Vec::new(),
        };
        let t = demo_telem();
        let mut fb = Framebuffer::new(120, 80);
        render_screen(
            &screen,
            &t,
            0,
            0,
            &CarData::default(),
            &Relatives::default(),
            &mut fb,
        );
        // some non-background pixels were drawn
        let bg = pal(Pal::Bg);
        assert!(fb_any_non_bg(&fb, bg), "widget drew nothing");
    }

    fn fb_any_non_bg(fb: &Framebuffer, bg: Rgb565) -> bool {
        let rgba = fb.to_rgba8();
        let bgr = (
            (bg.r() as u32) << 3,
            (bg.g() as u32) << 2,
            (bg.b() as u32) << 3,
        );
        let _ = bgr;
        // any pixel whose alpha row differs from a flat fill -> just check variance
        rgba.chunks(4).any(|p| p[0] > 40 || p[1] > 40 || p[2] > 40)
    }
}
