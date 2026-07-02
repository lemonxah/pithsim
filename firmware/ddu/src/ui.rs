//! On-screen UI: parses the pushed @RS race layout + @BS button pages and renders
//! them with embedded-graphics. Render fns are generic over DrawTarget so the
//! display task can pass each ST7796 panel. The on-screen RPM strip reuses
//! pith_core::shift so it matches the physical LEDs.

use embedded_graphics::{
    pixelcolor::Rgb565,
    prelude::*,
    primitives::{Circle, PrimitiveStyle, Rectangle, RoundedRectangle, Triangle},
};
use u8g2_fonts::{
    fonts,
    types::{FontColor, HorizontalAlignment, VerticalPosition},
    FontRenderer,
};

use pith_core::format::{self, Fmt, Pal, RuleOp};
use pith_core::registry::{field_def, field_id_from_str, field_value};
use pith_core::shift::{segment_rgb, CarData, RevCfg};
use pith_core::simhub::Telemetry;

pub const W: i32 = 480;
pub const H: i32 = 320;

// ---- palette (Pal token -> RGB565) ----
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
pub fn rgb(r: u8, g: u8, b: u8) -> Rgb565 {
    Rgb565::new(r >> 3, g >> 2, b >> 3)
}
fn rgb888(c: u32) -> Rgb565 {
    rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

pub const C_BG: Rgb565 = Rgb565::new(1, 2, 1);

// ---- text helper (picks a u8g2 font by requested pixel height) ----
#[allow(clippy::too_many_arguments)]
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
        _ => draw!(fonts::u8g2_font_logisoso32_tf),
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

// ---- race layout model (parsed from @RS) ----
#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Stat, Gear, GearSpeed, RpmStrip, TyreGrid, TcDual, Sectors, LapPair, Bar, Map, Flag, Position,
}
fn kind_of(s: &str) -> Kind {
    match s {
        "gear" => Kind::Gear,
        "gearSpeed" => Kind::GearSpeed,
        "rpmStrip" => Kind::RpmStrip,
        "tyreGrid" => Kind::TyreGrid,
        "tcDual" => Kind::TcDual,
        "sectors" => Kind::Sectors,
        "lapPair" => Kind::LapPair,
        "bar" => Kind::Bar,
        "map" => Kind::Map,
        "flag" => Kind::Flag,
        "position" => Kind::Position,
        _ => Kind::Stat,
    }
}

pub struct Module {
    pub kind: Kind,
    pub field: usize,
    pub label: String,
    pub fmt: Fmt,
    pub unit: String,
    pub scale: i32,
    pub base: Pal,
    pub rules: Vec<(RuleOp, i32, Pal)>,
    pub zone: usize, // 0=TOP 1=LEFT 2=CENTER 3=RIGHT 4=BOTTOM
    pub order: i32,
}

#[derive(Default)]
pub struct RaceLayout {
    pub mods: Vec<Module>,
}

pub fn parse_race(json: &str) -> Option<RaceLayout> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let arr = v.get("mods")?.as_array()?;
    let mut out = RaceLayout::default();
    for m in arr {
        let field = m
            .get("f")
            .and_then(|x| x.as_str())
            .map(field_id_from_str)
            .unwrap_or(0);
        let def = field_def(field);
        let fmt = m
            .get("fmt")
            .and_then(|f| f.get("t"))
            .and_then(|x| x.as_str())
            .map(Fmt::from_str)
            .unwrap_or_else(|| def.map(|d| d.fmt).unwrap_or(Fmt::Int));
        let scale = m
            .get("fmt")
            .and_then(|f| f.get("sc"))
            .and_then(|x| x.as_i64())
            .map(|s| s as i32)
            .filter(|&s| s > 0)
            .unwrap_or_else(|| def.map(|d| d.scale).unwrap_or(1));
        let unit = m
            .get("fmt")
            .and_then(|f| f.get("u"))
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let label = m
            .get("l")
            .and_then(|x| x.as_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| def.map(|d| d.label.to_string()).unwrap_or_default());
        let base = Pal::from_str(m.get("b").and_then(|x| x.as_str()).unwrap_or("white"));
        let mut rules = Vec::new();
        if let Some(rs) = m.get("r").and_then(|x| x.as_array()) {
            for r in rs.iter().take(4) {
                let op = RuleOp::from_str(r.get("op").and_then(|x| x.as_str()).unwrap_or(">"));
                let val = r.get("v").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                let col = Pal::from_str(r.get("c").and_then(|x| x.as_str()).unwrap_or("red"));
                rules.push((op, val, col));
            }
        }
        out.mods.push(Module {
            kind: kind_of(m.get("k").and_then(|x| x.as_str()).unwrap_or("stat")),
            field,
            label,
            fmt,
            unit,
            scale,
            base,
            rules,
            zone: m.get("z").and_then(|x| x.as_i64()).unwrap_or(0).clamp(0, 4) as usize,
            order: m.get("o").and_then(|x| x.as_i64()).unwrap_or(0) as i32,
        });
    }
    Some(out)
}

impl Module {
    fn color(&self, raw: i32) -> Rgb565 {
        for (op, v, c) in &self.rules {
            if op.matches(raw, *v) {
                return pal(*c);
            }
        }
        pal(self.base)
    }
}

// Zone rects (480x320), matching the legacy layout.
const ZONES: [(i32, i32, i32, i32, bool); 5] = [
    (0, 2, 480, 42, true),    // TOP (horizontal)
    (4, 50, 128, 220, false), // LEFT (vertical)
    (136, 50, 208, 220, false), // CENTER (vertical)
    (348, 50, 128, 220, false), // RIGHT (vertical)
    (0, 276, 480, 42, true),  // BOTTOM (horizontal)
];

pub fn render_race<D: DrawTarget<Color = Rgb565>>(d: &mut D, layout: &RaceLayout, t: &Telemetry, now_ms: i64) {
    let _ = d.clear(C_BG);
    for (zi, &(zx, zy, zw, zh, horiz)) in ZONES.iter().enumerate() {
        let mut zmods: Vec<&Module> = layout.mods.iter().filter(|m| m.zone == zi).collect();
        zmods.sort_by_key(|m| m.order);
        let n = zmods.len() as i32;
        if n == 0 {
            continue;
        }
        for (i, m) in zmods.iter().enumerate() {
            let i = i as i32;
            let (x, y, w, h) = if horiz {
                (zx + zw * i / n, zy, zw / n, zh)
            } else {
                (zx, zy + zh * i / n, zw, zh / n)
            };
            draw_module(d, x, y, w, h, m, t, now_ms);
        }
    }
}

fn draw_module<D: DrawTarget<Color = Rgb565>>(
    d: &mut D, x: i32, y: i32, w: i32, h: i32, m: &Module, t: &Telemetry, now_ms: i64,
) {
    let cx = x + w / 2;
    let raw = field_value(t, m.field);
    match m.kind {
        Kind::RpmStrip => {
            let seg = 12;
            let sw = w / seg;
            for i in 0..seg {
                let c = segment_rgb(t, i, seg, &RevCfg::default(), &CarData::default(), now_ms);
                let col = if c == 0 { pal(Pal::Panel) } else { rgb888(c) };
                fill_round(d, x + i * sw + 1, y + 4, sw - 2, h - 8, 3, col);
            }
        }
        Kind::Gear | Kind::GearSpeed => {
            let g = if t.gear == 0 { 'N' } else { t.gear as char };
            text(d, &g.to_string(), cx, y + h / 2 - 6, 40, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
            if m.kind == Kind::GearSpeed {
                text(d, &t.speed_kmh.to_string(), cx, y + h - 26, 24, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
                text(d, "KM/H", cx, y + h - 8, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            }
        }
        Kind::Position => {
            text(d, &m.label, cx, y + 12, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            let s = format!("P{}/{}", t.position, t.field_size);
            text(d, &s, cx, y + h / 2 + 4, 22, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
        }
        Kind::TcDual => {
            text(d, "TC", x + w / 4, y + 12, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, "ABS", x + 3 * w / 4, y + 12, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, &t.tc.to_string(), x + w / 4, y + h / 2 + 6, 22, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, &t.abs.to_string(), x + 3 * w / 4, y + h / 2 + 6, 22, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
        }
        Kind::LapPair => {
            let cur = format::format(t.cur_lap_ms, Fmt::Time, 1, "");
            let best = format::format(t.best_lap_ms, Fmt::Time, 1, "");
            text(d, "CURRENT", cx, y + 10, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, &cur, cx, y + h / 4 + 6, 18, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, "BEST", cx, y + h / 2 + 8, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            text(d, &best, cx, y + 3 * h / 4 + 4, 18, pal(Pal::Cyan), HorizontalAlignment::Center, VerticalPosition::Center);
        }
        Kind::Sectors => {
            let secs = [t.s1_ms, t.s2_ms, t.s3_ms];
            let bs = [t.bs1_ms, t.bs2_ms, t.bs3_ms];
            let sw = w / 3;
            for i in 0..3 {
                let col = if secs[i] > 0 && bs[i] > 0 && secs[i] <= bs[i] { pal(Pal::Green) } else { pal(Pal::Amber) };
                let s = format::format(secs[i], Fmt::Sector, 1, "");
                text(d, &s, x + i as i32 * sw + sw / 2, y + h / 2, 12, col, HorizontalAlignment::Center, VerticalPosition::Center);
            }
        }
        Kind::Bar => {
            let pct = if m.scale > 0 { (raw * 100 / m.scale).clamp(0, 100) } else { 0 };
            text(d, &m.label, x + 4, y + 10, 11, pal(Pal::Dim), HorizontalAlignment::Left, VerticalPosition::Center);
            fill_rect(d, x + 4, y + h / 2, w - 8, h / 3, pal(Pal::Panel));
            fill_rect(d, x + 4, y + h / 2, (w - 8) * pct / 100, h / 3, m.color(raw));
        }
        Kind::TyreGrid => {
            let temps = [t.tt_fl_m, t.tt_fr_m, t.tt_rl_m, t.tt_rr_m];
            let bw = w / 2;
            let bh = h / 2;
            for i in 0..4 {
                let (cxx, cyy) = (x + (i as i32 % 2) * bw, y + (i as i32 / 2) * bh);
                let col = if temps[i] > 95 { pal(Pal::Red) } else if temps[i] > 80 { pal(Pal::Amber) } else { pal(Pal::Green) };
                fill_round(d, cxx + 2, cyy + 2, bw - 4, bh - 4, 4, pal(Pal::Panel));
                text(d, &temps[i].to_string(), cxx + bw / 2, cyy + bh / 2, 14, col, HorizontalAlignment::Center, VerticalPosition::Center);
            }
        }
        Kind::Flag => {
            fill_round(d, x + 4, y + 4, w - 8, h - 8, 4, m.color(raw));
        }
        Kind::Map => {
            let _ = Circle::new(Point::new(cx - h / 3, y + h / 6), (h / 3).max(1) as u32)
                .into_styled(PrimitiveStyle::with_stroke(pal(Pal::Dim), 1))
                .draw(d);
        }
        Kind::Stat => {
            text(d, &m.label, cx, y + 11, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
            let s = format::format(raw, m.fmt, m.scale, &m.unit);
            text(d, &s, cx, y + h / 2 + 6, 22, m.color(raw), HorizontalAlignment::Center, VerticalPosition::Center);
        }
    }
}


// ---- "no screen yet" placeholder ----
/// Drawn on a panel with no pith-ui screen yet — prompts the user to upload one
/// for this display in the editor (the legacy @BS button box was removed).
pub fn render_no_screen<D: DrawTarget<Color = Rgb565>>(d: &mut D) {
    let _ = d.clear(C_BG);
    text(d, "No screen", W / 2, H / 2 - 12, 20, pal(Pal::Dim),
         HorizontalAlignment::Center, VerticalPosition::Center);
    text(d, "Add a layout for this display in the editor", W / 2, H / 2 + 14, 12, pal(Pal::Dim),
         HorizontalAlignment::Center, VerticalPosition::Center);
}

// ---- config screen ----
// A settings-cog affordance in the BOTTOM-LEFT opens the on-device config screen.
pub const CONFIG_HOTSPOT: (i32, i32, i32, i32) = (0, H - 56, 86, 56);
pub const SLD: (i32, i32, i32, i32) = (40, 205, 400, 36);    // brightness slider (below the info block)
pub const SIM_BTN: (i32, i32, i32, i32) = (288, 56, 172, 42); // right column, top
pub const RBT_BTN: (i32, i32, i32, i32) = (288, 104, 172, 42);// right column, below SIM
pub const SLP_BTN: (i32, i32, i32, i32) = (288, 152, 172, 42);// right column, sleep now
pub const SLP_TO_BTN: (i32, i32, i32, i32) = (40, 140, 200, 36); // auto-sleep timeout cycler (left, under info)
pub const BACK_BTN: (i32, i32, i32, i32) = (20, 262, 60, 46); // bottom-left back-arrow icon

/// Auto-sleep timeout choices (seconds; 0 = never). Tapping the config-screen
/// button steps to the next one.
pub const SLEEP_PRESETS: [u16; 7] = [0, 15, 30, 60, 120, 300, 600];

pub fn sleep_label(secs: u16) -> String {
    match secs {
        0 => "OFF".into(),
        s if s < 60 => format!("{s}S"),
        s => format!("{}M", s / 60),
    }
}

/// Stats shown on the device config screen.
pub struct ConfigInfo<'a> {
    pub fw: &'a str,
    pub board: &'a str,
    pub serial: &'a str,
    pub car: &'a str,
    pub heap_kb: i32,
    pub uptime_s: i64,
    pub brightness: u8,
    pub sim: bool,
    pub sleep_timeout_s: u16,
}

/// A small settings-cog affordance in the race panel's BOTTOM-LEFT corner so the
/// tap-to-open config gesture is discoverable. Drawn over the race render each frame.
pub fn render_config_hint<D: DrawTarget<Color = Rgb565>>(d: &mut D) {
    let cx = 24;
    let cy = H - 24;
    let body = pal(Pal::Dim);
    // eight teeth around the rim
    for i in 0..8 {
        let a = i as f32 * core::f32::consts::FRAC_PI_4;
        let tx = cx + (a.cos() * 13.0) as i32;
        let ty = cy + (a.sin() * 13.0) as i32;
        fill_round(d, tx - 3, ty - 3, 6, 6, 1, body);
    }
    // gear body + centre hole
    let _ = Circle::new(Point::new(cx - 10, cy - 10), 20)
        .into_styled(PrimitiveStyle::with_fill(body))
        .draw(d);
    let _ = Circle::new(Point::new(cx - 4, cy - 4), 8)
        .into_styled(PrimitiveStyle::with_fill(C_BG))
        .draw(d);
}

pub fn render_config<D: DrawTarget<Color = Rgb565>>(d: &mut D, info: &ConfigInfo) {
    let _ = d.clear(C_BG);
    text(d, "PITH DDU - CONFIG", W / 2, 26, 16, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
    // back-arrow icon button at the bottom-left to exit config
    fill_round(d, BACK_BTN.0, BACK_BTN.1, BACK_BTN.2, BACK_BTN.3, 6, pal(Pal::Panel));
    {
        let cy = BACK_BTN.1 + BACK_BTN.3 / 2;
        let cx = BACK_BTN.0 + BACK_BTN.2 / 2;
        let _ = Triangle::new(
            Point::new(cx - 9, cy),
            Point::new(cx + 6, cy - 11),
            Point::new(cx + 6, cy + 11),
        )
        .into_styled(PrimitiveStyle::with_fill(pal(Pal::White)))
        .draw(d);
    }

    // --- device stats panel ---
    let mins = info.uptime_s / 60;
    let secs = info.uptime_s % 60;
    let line1 = format!("FW {}   BOARD {}", info.fw, info.board);
    let line2 = format!("S/N {}", info.serial);
    let line3 = format!("CAR {}", if info.car.is_empty() { "—" } else { info.car });
    let line4 = format!("HEAP {} KB   UP {}:{:02}", info.heap_kb, mins, secs);
    text(d, &line1, 40, 74, 12, pal(Pal::Dim), HorizontalAlignment::Left, VerticalPosition::Center);
    text(d, &line2, 40, 92, 12, pal(Pal::Dim), HorizontalAlignment::Left, VerticalPosition::Center);
    text(d, &line3, 40, 110, 12, pal(Pal::Dim), HorizontalAlignment::Left, VerticalPosition::Center);
    text(d, &line4, 40, 128, 12, pal(Pal::Dim), HorizontalAlignment::Left, VerticalPosition::Center);

    // auto-sleep timeout cycler (tap to step through the presets)
    fill_round(d, SLP_TO_BTN.0, SLP_TO_BTN.1, SLP_TO_BTN.2, SLP_TO_BTN.3, 6, pal(Pal::Panel));
    text(d, &format!("AUTO SLEEP: {}", sleep_label(info.sleep_timeout_s)),
         SLP_TO_BTN.0 + SLP_TO_BTN.2 / 2, SLP_TO_BTN.1 + SLP_TO_BTN.3 / 2, 13,
         pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);

    let b = info.brightness;
    text(d, "SHIFT LED BRIGHTNESS", SLD.0, SLD.1 - 18, 12, pal(Pal::Dim), HorizontalAlignment::Left, VerticalPosition::Center);
    fill_round(d, SLD.0, SLD.1, SLD.2, SLD.3, 6, pal(Pal::Panel));
    fill_round(d, SLD.0, SLD.1, SLD.2 * b as i32 / 100, SLD.3, 6, pal(Pal::Cyan));
    text(d, &format!("{b}%"), SLD.0 + SLD.2 / 2, SLD.1 + SLD.3 / 2, 14, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);

    fill_round(d, SIM_BTN.0, SIM_BTN.1, SIM_BTN.2, SIM_BTN.3, 6, if info.sim { pal(Pal::Green) } else { pal(Pal::Panel) });
    text(d, if info.sim { "SIM: ON" } else { "RUN SIM" }, SIM_BTN.0 + SIM_BTN.2 / 2, SIM_BTN.1 + SIM_BTN.3 / 2, 14, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
    fill_round(d, RBT_BTN.0, RBT_BTN.1, RBT_BTN.2, RBT_BTN.3, 6, pal(Pal::Red));
    text(d, "RESTART", RBT_BTN.0 + RBT_BTN.2 / 2, RBT_BTN.1 + RBT_BTN.3 / 2, 14, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
    fill_round(d, SLP_BTN.0, SLP_BTN.1, SLP_BTN.2, SLP_BTN.3, 6, pal(Pal::Panel));
    text(d, "SLEEP NOW", SLP_BTN.0 + SLP_BTN.2 / 2, SLP_BTN.1 + SLP_BTN.3 / 2, 14, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
}

pub fn render_ota<D: DrawTarget<Color = Rgb565>>(d: &mut D, pct: i32) {
    let _ = d.clear(C_BG);
    text(d, "FIRMWARE UPDATE", W / 2, 110, 18, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
    text(d, "Flashing — do not disconnect", W / 2, 145, 12, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
    fill_round(d, 60, 175, 360, 30, 6, pal(Pal::Panel));
    fill_round(d, 60, 175, 360 * pct.clamp(0, 100) / 100, 30, 6, pal(Pal::Cyan));
    text(d, &format!("{pct}%"), W / 2, 220, 14, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
}

pub fn hit(p: (i32, i32, i32, i32), tx: i32, ty: i32) -> bool {
    tx >= p.0 && tx < p.0 + p.2 && ty >= p.1 && ty < p.1 + p.3
}
