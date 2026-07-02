//! Shared, dependency-free formatting + rule/palette primitives for the
//! data-driven module system. This is the Rust port of the C `format_common.h`
//! that is compiled into BOTH the firmware and (by path) the pithddu-dashboard
//! app. It is the single source of truth that guarantees the app preview and the
//! device format values + evaluate color rules **byte-identically**.
//!
//! Any change here must keep `fmtc_format` / `fmtc_rule_match` output identical
//! to the C version — the host tests in this crate lock that down.

/// Value format types. Order matches the C `fmt_type_t` enum and `FMT_NAMES`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Fmt {
    Int = 0, // integer, value/scale
    Fixed1,  // 1 decimal,  value/scale
    Fixed2,  // 2 decimals, value/scale
    Time,    // lap time  m:ss.mmm (value = ms)
    Sector,  // sector    s.mmm    (value = ms)
    Delta,   // signed seconds, 4 decimals (value = 0.1 ms units)
    Str,     // plain integer (no scaling), for counts/codes ("string")
}

/// Color-rule comparison ops. Order matches the C `rule_op_t` enum and `OP_NAMES`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum RuleOp {
    Lt = 0, // <
    Le,     // <=
    Eq,     // ==
    Ge,     // >=
    Gt,     // >
}

/// Shared palette tokens. Order matches the C `pal_token_t` enum and `PAL_NAMES`.
/// Each side maps these to its own color space.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum Pal {
    Bg,
    Panel,
    White,
    Dim,
    Green,
    Amber,
    Red,
    Cyan,
    Blue,
    Purple,
    /// Arbitrary custom colour (from the GUI colour picker / theme palette).
    Rgb(u8, u8, u8),
}

pub const FMT_NAMES: [&str; 7] = [
    "int", "fixed1", "fixed2", "time", "sector", "delta", "string",
];
pub const OP_NAMES: [&str; 5] = ["<", "<=", "==", ">=", ">"];
pub const PAL_NAMES: [&str; 10] = [
    "bg", "panel", "white", "dim", "green", "amber", "red", "cyan", "blue", "purple",
];

impl Fmt {
    /// Parse a format token; defaults to `Int` (matches `fmtc_fmt_from_str`).
    // Not the std FromStr trait: infallible by design (unknown input maps
    // to a sensible default so a hand-edited UiDoc never fails to load).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Fmt {
        match s {
            "fixed1" => Fmt::Fixed1,
            "fixed2" => Fmt::Fixed2,
            "time" => Fmt::Time,
            "sector" => Fmt::Sector,
            "delta" => Fmt::Delta,
            "string" => Fmt::Str,
            _ => Fmt::Int,
        }
    }
    pub fn as_str(self) -> &'static str {
        FMT_NAMES[self as usize]
    }
}

impl RuleOp {
    /// Parse an op token; defaults to `Gt` (matches `fmtc_op_from_str`).
    // Not the std FromStr trait: infallible by design (unknown input maps
    // to a sensible default so a hand-edited UiDoc never fails to load).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> RuleOp {
        match s {
            "<" => RuleOp::Lt,
            "<=" => RuleOp::Le,
            "==" => RuleOp::Eq,
            ">=" => RuleOp::Ge,
            _ => RuleOp::Gt,
        }
    }
    pub fn as_str(self) -> &'static str {
        OP_NAMES[self as usize]
    }
    /// Evaluate one color-rule comparison against the raw value
    /// (port of `fmtc_rule_match`).
    pub fn matches(self, v: i32, rule_v: i32) -> bool {
        match self {
            RuleOp::Lt => v < rule_v,
            RuleOp::Le => v <= rule_v,
            RuleOp::Eq => v == rule_v,
            RuleOp::Ge => v >= rule_v,
            RuleOp::Gt => v > rule_v,
        }
    }
}

impl Pal {
    /// Parse a palette token; defaults to `White` (matches `fmtc_pal_from_str`).
    // Not the std FromStr trait: infallible by design (unknown input maps
    // to a sensible default so a hand-edited UiDoc never fails to load).
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Pal {
        match s {
            "bg" => Pal::Bg,
            "panel" => Pal::Panel,
            "dim" => Pal::Dim,
            "green" => Pal::Green,
            "amber" => Pal::Amber,
            "red" => Pal::Red,
            "cyan" => Pal::Cyan,
            "blue" => Pal::Blue,
            "purple" => Pal::Purple,
            "white" => Pal::White,
            // "#rrggbb" -> a custom colour; anything else -> White.
            _ => parse_hex(s)
                .map(|(r, g, b)| Pal::Rgb(r, g, b))
                .unwrap_or(Pal::White),
        }
    }
    /// Lowercase token for the named colours; "custom" for an Rgb (use [`Pal::to_token`]
    /// for the round-trippable form).
    pub fn as_str(self) -> &'static str {
        match self {
            Pal::Bg => "bg",
            Pal::Panel => "panel",
            Pal::White => "white",
            Pal::Dim => "dim",
            Pal::Green => "green",
            Pal::Amber => "amber",
            Pal::Red => "red",
            Pal::Cyan => "cyan",
            Pal::Blue => "blue",
            Pal::Purple => "purple",
            Pal::Rgb(..) => "custom",
        }
    }
    /// 8-bit sRGB for this colour (matches the device `pal()` mapping). Lets the
    /// GUI render an exact swatch for any palette entry, named or custom.
    pub fn rgb888(self) -> (u8, u8, u8) {
        match self {
            Pal::Bg => (8, 10, 14),
            Pal::Panel => (28, 32, 40),
            Pal::White => (235, 238, 245),
            Pal::Dim => (120, 128, 140),
            Pal::Green => (40, 220, 90),
            Pal::Amber => (255, 180, 40),
            Pal::Red => (240, 60, 60),
            Pal::Cyan => (40, 210, 230),
            Pal::Blue => (60, 130, 255),
            Pal::Purple => (180, 110, 255),
            Pal::Rgb(r, g, b) => (r, g, b),
        }
    }
}

/// Parse "#rrggbb" (or bare "rrggbb") into (r, g, b). None if malformed.
pub fn parse_hex(s: &str) -> Option<(u8, u8, u8)> {
    let h = s.strip_prefix('#').unwrap_or(s);
    if h.len() != 6 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some((r, g, b))
}

/// "No reading" sentinel: a decoder writes this when a channel is absent or
/// garbage (e.g. an uninitialised tyre at 0 K → -273°C). `format` renders it as
/// `--`. Far outside any real 0.1°/0.1ms range, so it never collides with data —
/// but NOT `i32::MIN`: its magnitude (2147483648) overflows the base-10 frame
/// parser (`parse_int_opt`), so it must stay inside `±i32::MAX`.
pub const NA: i32 = -2_000_000_000;

/// Format a raw telemetry int into a display string — the exact port of
/// `fmtc_format`. `scale` divides the raw value; `unit` is the verbatim suffix
/// (pass `""` to omit — the device passes `""` for the degree ring). The output
/// must be byte-identical to the C version (the `NA` sentinel is host-only).
pub fn format(v: i32, fmt: Fmt, scale: i32, unit: &str) -> String {
    if v == NA {
        return "--".to_string();
    }
    let scale = if scale <= 0 { 1 } else { scale };
    match fmt {
        Fmt::Time => {
            if v <= 0 {
                return "--:--.---".to_string();
            }
            let mut s = format!("{}:{:02}.{:03}", v / 60000, (v / 1000) % 60, v % 1000);
            if !unit.is_empty() {
                s.push_str(unit);
            }
            s
        }
        Fmt::Sector => {
            if v <= 0 {
                return "--.---".to_string();
            }
            let mut s = format!("{}.{:03}", v / 1000, v % 1000);
            if !unit.is_empty() {
                s.push_str(unit);
            }
            s
        }
        Fmt::Delta => {
            // Signed seconds, ALWAYS 4 decimals, clamped to +/-9.9999. `v` is in
            // 0.1 ms units (10000 = 1.0000 s). Never carries a unit.
            let v = v.clamp(-99999, 99999);
            let sign = if v >= 0 { '+' } else { '-' };
            let a = v.abs();
            format!("{}{}.{:04}", sign, a / 10000, a % 10000)
        }
        Fmt::Fixed1 => {
            let whole = v / scale;
            let frac = (v.abs() * 10 / scale) % 10;
            format!("{}.{}{}", whole, frac, unit)
        }
        Fmt::Fixed2 => {
            let whole = v / scale;
            let frac = (v.abs() * 100 / scale) % 100;
            format!("{}.{:02}{}", whole, frac, unit)
        }
        Fmt::Int | Fmt::Str => format!("{}{}", v / scale, unit),
    }
}
