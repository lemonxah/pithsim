//! pith-core — pure-logic core for the pithddu firmware.
//!
//! Dependency-free (no esp / no_std-friendly std) so it compiles and unit-tests
//! on the host. Holds the wire-format primitives, the SimHub telemetry parser,
//! and the generated telemetry field registry — the parts that must stay
//! byte-compatible with the pithddu-dashboard app.

pub mod format;
pub mod le;
pub mod net;
pub mod relatives;
pub mod shift;
pub mod simhub;

/// Telemetry field registry, generated from `main/field_registry.json`.
pub mod registry {
    use crate::format::Fmt;
    use crate::simhub::Telemetry;

    include!(concat!(env!("OUT_DIR"), "/field_registry.rs"));

    /// Resolve a field name to its 1-based id; 0 if unknown (port of
    /// `field_id_from_str`).
    pub fn field_id_from_str(name: &str) -> usize {
        for (i, f) in FIELDS.iter().enumerate() {
            if f.name == name {
                return i + 1;
            }
        }
        0
    }

    /// The default format/scale/label a module inherits for a field id (1-based).
    pub fn field_def(id: usize) -> Option<&'static FieldDef> {
        if id == 0 {
            None
        } else {
            FIELDS.get(id - 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::format::{format, Fmt, Pal, RuleOp};
    use crate::registry::{field_id_from_str, field_value, FIELDS, FIELD_COUNT};
    use crate::simhub::{parse_line, Telemetry};

    // ---- format (must match the C fmtc_format byte-for-byte) ----
    #[test]
    fn fmt_int_and_string() {
        assert_eq!(format(212, Fmt::Int, 1, ""), "212");
        assert_eq!(format(212, Fmt::Int, 1, "KM/H"), "212KM/H");
        assert_eq!(format(2500, Fmt::Int, 1, ""), "2500");
        assert_eq!(format(7, Fmt::Str, 1, ""), "7");
    }

    #[test]
    fn fmt_fixed() {
        assert_eq!(format(65, Fmt::Fixed1, 10, "L"), "6.5L");
        assert_eq!(format(-35, Fmt::Fixed1, 10, ""), "-3.5");
        assert_eq!(format(565, Fmt::Fixed1, 10, ""), "56.5");
        assert_eq!(format(1234, Fmt::Fixed2, 100, ""), "12.34");
    }

    #[test]
    fn fmt_time_and_sector() {
        assert_eq!(format(0, Fmt::Time, 1, ""), "--:--.---");
        assert_eq!(format(-5, Fmt::Time, 1, ""), "--:--.---");
        assert_eq!(format(84012, Fmt::Time, 1, ""), "1:24.012");
        assert_eq!(format(95000, Fmt::Time, 1, ""), "1:35.000");
        assert_eq!(format(0, Fmt::Sector, 1, ""), "--.---");
        assert_eq!(format(28456, Fmt::Sector, 1, ""), "28.456");
    }

    #[test]
    fn fmt_delta() {
        // 0.1 ms units: 10000 = 1.0000 s; always 4 decimals, signed, no unit.
        assert_eq!(format(0, Fmt::Delta, 1, ""), "+0.0000");
        assert_eq!(format(-3000, Fmt::Delta, 1, ""), "-0.3000");
        assert_eq!(format(12340, Fmt::Delta, 1, ""), "+1.2340");
        assert_eq!(format(999999, Fmt::Delta, 1, ""), "+9.9999"); // clamped
        assert_eq!(format(-999999, Fmt::Delta, 1, ""), "-9.9999"); // clamped
    }

    #[test]
    fn rule_ops_and_palette_roundtrip() {
        assert!(RuleOp::Gt.matches(10, 5));
        assert!(!RuleOp::Lt.matches(10, 5));
        assert!(RuleOp::Eq.matches(5, 5));
        assert_eq!(RuleOp::from_str(">="), RuleOp::Ge);
        assert_eq!(RuleOp::from_str("bogus"), RuleOp::Gt); // default
        assert_eq!(Fmt::from_str("delta"), Fmt::Delta);
        assert_eq!(Fmt::from_str("bogus"), Fmt::Int); // default
        assert_eq!(Pal::from_str("amber"), Pal::Amber);
        assert_eq!(Pal::from_str("bogus"), Pal::White); // default
        assert_eq!(Fmt::Delta.as_str(), "delta");
        assert_eq!(Pal::Cyan.as_str(), "cyan");
    }

    // ---- field registry ----
    #[test]
    fn registry_ids_and_count() {
        // id = index + 1; 0 = none. The dashboard relies on this exact ordering.
        assert_eq!(field_id_from_str("speed_kmh"), 1);
        assert_eq!(field_id_from_str("rpm"), 2);
        assert_eq!(field_id_from_str("delta_ms"), 10);
        // Registry order now mirrors the $-frame exactly (incl. inner/outer tyre
        // temps, world pos, best sectors) so field id == frame token position.
        assert_eq!(field_id_from_str("ignition"), 60);
        assert_eq!(field_id_from_str("flag"), 61);
        assert_eq!(field_id_from_str("track_pct"), 62);
        assert_eq!(field_id_from_str("battery_pct"), 71);
        assert_eq!(field_id_from_str("ers_state"), 72);
        // Tyre surface-average (×4) + carcass-core (×4) appended after ers_state.
        assert_eq!(field_id_from_str("tt_avg_fl"), 73);
        assert_eq!(field_id_from_str("tt_carc_fl"), 77);
        assert_eq!(field_id_from_str("tt_carc_rr"), 80);
        // pith-ui's ve_swap hardcodes these ids — keep them stable.
        assert_eq!(field_id_from_str("fuel_dl"), 23);
        assert_eq!(field_id_from_str("fuel_per_lap_ml"), 25);
        assert_eq!(field_id_from_str("comp_fl"), 81);
        assert_eq!(field_id_from_str("comp_rr"), 84);
        assert_eq!(field_id_from_str("tc_slip"), 85);
        assert_eq!(field_id_from_str("virtual_energy"), 87);
        assert_eq!(field_id_from_str("fuel_is_ve"), 89);
        // Chassis G-forces + grip diagnostics appended after fuel_is_ve.
        assert_eq!(field_id_from_str("g_long_x100"), 90);
        assert_eq!(field_id_from_str("g_lat_x100"), 91);
        assert_eq!(field_id_from_str("wheel_slip"), 92);
        assert_eq!(field_id_from_str("susp_impact"), 93);
        assert_eq!(field_id_from_str("nope"), 0);
        assert_eq!(FIELDS.len(), 93);
        assert_eq!(FIELD_COUNT, 94);
    }

    #[test]
    fn field_value_maps_to_struct() {
        let mut t = Telemetry::idle();
        t.speed_kmh = 212;
        t.rpm = 6800;
        t.ignition = 1;
        assert_eq!(field_value(&t, 1), 212);
        assert_eq!(field_value(&t, 2), 6800);
        assert_eq!(field_value(&t, 60), 1); // ignition
        assert_eq!(field_value(&t, 0), 0);
        assert_eq!(field_value(&t, 999), 0);
    }

    // ---- simhub parser ----
    #[test]
    fn parse_minimal_required() {
        let t = parse_line("$3;212;6800;7500").unwrap();
        assert_eq!(t.gear, b'3');
        assert_eq!(t.speed_kmh, 212);
        assert_eq!(t.rpm, 6800);
        assert_eq!(t.max_rpm, 7500);
        // Absent optional fields keep defaults; tyre wear defaults to 100.
        assert_eq!(t.shift_rpm, 0);
        assert_eq!(t.tw_fl, 100);
        assert_eq!(t.tw_rr, 100);
    }

    #[test]
    fn parse_gear_forms() {
        assert_eq!(parse_line("$N;0;0;0").unwrap().gear, b'N');
        assert_eq!(parse_line("$R;0;0;0").unwrap().gear, b'R');
        assert_eq!(parse_line("$0;0;0;0").unwrap().gear, b'N'); // numeric neutral
        assert_eq!(parse_line("$-1;0;0;0").unwrap().gear, b'R'); // numeric reverse
        assert_eq!(parse_line("$7;0;0;0").unwrap().gear, b'7');
    }

    #[test]
    fn parse_resync_and_reject() {
        // bytes before '$' are ignored
        assert!(parse_line("junk$2;100;5000;7000").is_some());
        // no sentinel -> reject
        assert!(parse_line("2;100;5000;7000").is_none());
        // missing required field -> reject
        assert!(parse_line("$2;100").is_none());
        // bad gear -> reject
        assert!(parse_line("$X;1;2;3").is_none());
        // trailing junk -> reject
        assert!(parse_line("$2;1;2;3;garbage").is_none());
    }

    #[test]
    fn parse_optional_and_empty_fields() {
        // empty fields keep defaults but are still consumed (;;), later fields set
        let t = parse_line("$2;100;5000;7000;;500;;;;;-3000").unwrap();
        assert_eq!(t.shift_rpm, 0); // empty
        assert_eq!(t.cur_lap_ms, 500);
        assert_eq!(t.delta_ms, -3000); // 7th optional field, negative
    }

    #[test]
    fn parse_trailing_separator_ok() {
        // trailing ';' and whitespace tolerated
        assert!(parse_line("$2;100;5000;7000;").is_some());
        assert!(parse_line("$2;100;5000;7000\r\n").is_some());
    }
}
