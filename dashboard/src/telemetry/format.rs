//! Telemetry value formatting + UI option lists.
//!
//! The byte-compatible formatting and the `Fmt`/`RuleOp` enums + their name tables
//! now come from `pith-core` — the single source of truth shared with the firmware
//! (its host tests lock the output to the C `fmtc_format`). Only the app-side
//! palette colours and the authoring dropdown lists live here.

pub use pith_core::format::{format as fmtc_format, Fmt, RuleOp as Op, FMT_NAMES, OP_NAMES};

/// UI dropdown palette tokens (the editor's 8-token subset — no bg/panel).
pub const PALETTE_TOKENS: [&str; 8] =
    ["white", "dim", "green", "amber", "red", "cyan", "blue", "purple"];

/// Authoring kind options (the race-screen module palette).
pub const KIND_OPTIONS: [&str; 12] = [
    "stat", "bar", "gear", "gearSpeed", "rpmStrip", "tyreGrid", "tcDual", "sectors", "lapPair",
    "map", "flag", "position",
];

pub fn fmt_from_str(s: &str) -> Fmt {
    Fmt::from_str(s)
}
pub fn op_from_str(s: &str) -> Op {
    Op::from_str(s)
}
pub fn rule_match(v: i32, op: Op, rule_v: i32) -> bool {
    op.matches(v, rule_v)
}

/// Index of `v` in a fixed token list, or -1 (mirrors C++ idxOf, for ComboBox
/// current-index).
pub fn idx_of(list: &[&str], v: &str) -> i32 {
    list.iter().position(|&x| x == v).map(|i| i as i32).unwrap_or(-1)
}

/// App-side palette token -> 0xRRGGBB (the dashboard's colour space; the device
/// maps the same `Pal` tokens to RGB565).
pub fn palette_color(tok: &str) -> u32 {
    match tok {
        "bg" => 0x060708,
        "panel" => 0x131519,
        "dim" => 0x636A74,
        "green" => 0x00E676,
        "amber" => 0xFFB300,
        "red" => 0xFF3B30,
        "cyan" => 0x7FC9B1,
        "blue" => 0x2E9DFF,
        "purple" => 0xD500F9,
        _ => 0xE8EAED, // white / default text
    }
}
