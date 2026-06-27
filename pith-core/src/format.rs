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
    Int = 0,  // integer, value/scale
    Fixed1,   // 1 decimal,  value/scale
    Fixed2,   // 2 decimals, value/scale
    Time,     // lap time  m:ss.mmm (value = ms)
    Sector,   // sector    s.mmm    (value = ms)
    Delta,    // signed seconds, 4 decimals (value = 0.1 ms units)
    Str,      // plain integer (no scaling), for counts/codes ("string")
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
    Bg = 0,
    Panel,
    White,
    Dim,
    Green,
    Amber,
    Red,
    Cyan,
    Blue,
    Purple,
}

pub const FMT_NAMES: [&str; 7] =
    ["int", "fixed1", "fixed2", "time", "sector", "delta", "string"];
pub const OP_NAMES: [&str; 5] = ["<", "<=", "==", ">=", ">"];
pub const PAL_NAMES: [&str; 10] = [
    "bg", "panel", "white", "dim", "green", "amber", "red", "cyan", "blue", "purple",
];

impl Fmt {
    /// Parse a format token; defaults to `Int` (matches `fmtc_fmt_from_str`).
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
            _ => Pal::White,
        }
    }
    pub fn as_str(self) -> &'static str {
        PAL_NAMES[self as usize]
    }
}

/// Format a raw telemetry int into a display string — the exact port of
/// `fmtc_format`. `scale` divides the raw value; `unit` is the verbatim suffix
/// (pass `""` to omit — the device passes `""` for the degree ring). The output
/// must be byte-identical to the C version.
pub fn format(v: i32, fmt: Fmt, scale: i32, unit: &str) -> String {
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
