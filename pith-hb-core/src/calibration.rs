//! Idle/max/deadzone calibration for a single load-cell axis. All integer math
//! (the ESP32-S2 has no hardware FPU) — `apply()` maps a raw HX711 reading to a
//! 0..=65535 output the firmware pushes as the HID axis value.

/// Persisted + in-flight calibration state. `idle_raw`/`max_raw` don't need
/// `idle_raw < max_raw` — whichever direction the load cell is wired, `apply()`
/// normalizes it; `inverted` is a separate, explicit "flip the output" choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Calibration {
    pub idle_raw: i32,
    pub max_raw: i32,
    pub deadzone_lo_pct: u8,
    pub deadzone_hi_pct: u8,
    pub inverted: bool,
    /// False until a `@SAVE` has committed a real idle/max pair — lets the
    /// firmware/wizard distinguish "freshly flashed" from "user zeroed it out".
    pub calibrated: bool,
}

impl Default for Calibration {
    fn default() -> Self {
        Calibration {
            idle_raw: 0,
            max_raw: 0,
            deadzone_lo_pct: 2,
            deadzone_hi_pct: 2,
            inverted: false,
            calibrated: false,
        }
    }
}

/// Minimum |max_raw - idle_raw| to accept a `@MAXC` capture — below this the
/// two points are indistinguishable from load-cell/ADC noise. Tune against a
/// real cell; this is a conservative starting guess for a 24-bit HX711 reading.
pub const MIN_SPAN: i32 = 2000;

/// Fixed on-wire/NVS blob size: idle_raw(4) + max_raw(4) + dz_lo(1) + dz_hi(1)
/// + inverted(1) + calibrated(1).
pub const BLOB_LEN: usize = 12;

impl Calibration {
    /// Whether `idle` and `max` are far enough apart to be a usable calibration.
    pub fn span_ok(idle: i32, max: i32) -> bool {
        (max as i64 - idle as i64).abs() >= MIN_SPAN as i64
    }

    pub fn to_bytes(&self) -> [u8; BLOB_LEN] {
        let mut b = [0u8; BLOB_LEN];
        b[0..4].copy_from_slice(&self.idle_raw.to_le_bytes());
        b[4..8].copy_from_slice(&self.max_raw.to_le_bytes());
        b[8] = self.deadzone_lo_pct;
        b[9] = self.deadzone_hi_pct;
        b[10] = self.inverted as u8;
        b[11] = self.calibrated as u8;
        b
    }

    pub fn from_bytes(b: &[u8]) -> Option<Self> {
        if b.len() < BLOB_LEN {
            return None;
        }
        Some(Calibration {
            idle_raw: i32::from_le_bytes(b[0..4].try_into().ok()?),
            max_raw: i32::from_le_bytes(b[4..8].try_into().ok()?),
            deadzone_lo_pct: b[8].min(100),
            deadzone_hi_pct: b[9].min(100),
            inverted: b[10] != 0,
            calibrated: b[11] != 0,
        })
    }

    /// Map a raw HX711 reading to a 0..=65535 axis value: normalize against
    /// idle/max (clamped to the endpoints, direction-agnostic), clip the
    /// deadzones at each end and rescale the remainder to fill 0..=65535, then
    /// flip if `inverted`. Returns 0 while uncalibrated or if idle == max.
    pub fn apply(&self, raw: i32) -> u16 {
        if !self.calibrated {
            return 0;
        }
        let span = self.max_raw as i64 - self.idle_raw as i64;
        if span == 0 {
            return 0;
        }
        let diff = raw as i64 - self.idle_raw as i64;
        let ratio = ((diff * 65535) / span).clamp(0, 65535);

        let dz_lo = (self.deadzone_lo_pct as i64) * 65535 / 100;
        let dz_hi = (self.deadzone_hi_pct as i64) * 65535 / 100;
        let hi_cut = 65535 - dz_hi;

        let out = if dz_lo + dz_hi >= 65535 {
            // Degenerate config (deadzones eat the whole range) — off, not a panic.
            0
        } else if ratio <= dz_lo {
            0
        } else if ratio >= hi_cut {
            65535
        } else {
            ((ratio - dz_lo) * 65535) / (hi_cut - dz_lo)
        };
        let out = out.clamp(0, 65535) as u16;

        if self.inverted {
            65535 - out
        } else {
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(idle: i32, max: i32, dz_lo: u8, dz_hi: u8, inverted: bool) -> Calibration {
        Calibration {
            idle_raw: idle,
            max_raw: max,
            deadzone_lo_pct: dz_lo,
            deadzone_hi_pct: dz_hi,
            inverted,
            calibrated: true,
        }
    }

    #[test]
    fn uncalibrated_is_zero() {
        let c = Calibration::default();
        assert_eq!(c.apply(50_000), 0);
    }

    #[test]
    fn zero_span_is_zero() {
        let c = cal(1000, 1000, 0, 0, false);
        assert_eq!(c.apply(50_000), 0);
    }

    #[test]
    fn below_idle_clamps_to_zero() {
        let c = cal(1000, 9000, 0, 0, false);
        assert_eq!(c.apply(0), 0);
    }

    #[test]
    fn above_max_clamps_to_max() {
        let c = cal(1000, 9000, 0, 0, false);
        assert_eq!(c.apply(50_000), 65535);
    }

    #[test]
    fn midpoint_is_half_scale() {
        let c = cal(0, 10_000, 0, 0, false);
        let out = c.apply(5_000);
        assert!((32000..=33500).contains(&out), "got {out}");
    }

    #[test]
    fn deadzone_lo_clamps_small_pulls_to_zero() {
        let c = cal(0, 10_000, 10, 0, false); // bottom 10% dead
        assert_eq!(c.apply(500), 0); // 5% pull, inside the deadzone
        assert!(c.apply(2_000) > 0); // 20% pull, past it
    }

    #[test]
    fn deadzone_hi_clamps_near_max_to_full_scale() {
        let c = cal(0, 10_000, 0, 10, false); // top 10% saturates early
        assert_eq!(c.apply(9_500), 65535); // 95% pull, inside the top deadzone
        assert!(c.apply(8_000) < 65535); // 80% pull, before it
    }

    #[test]
    fn inverted_flips_output() {
        let normal = cal(0, 10_000, 0, 0, false);
        let inv = cal(0, 10_000, 0, 0, true);
        assert_eq!(normal.apply(10_000), 65535);
        assert_eq!(inv.apply(10_000), 0);
        assert_eq!(normal.apply(0), 0);
        assert_eq!(inv.apply(0), 65535);
    }

    #[test]
    fn reversed_wiring_still_ramps_up_on_pull() {
        // max_raw < idle_raw: raw counts DOWN as the handbrake is pulled.
        let c = cal(10_000, 0, 0, 0, false);
        assert_eq!(c.apply(10_000), 0);
        assert_eq!(c.apply(0), 65535);
        let mid = c.apply(5_000);
        assert!((32000..=33500).contains(&mid), "got {mid}");
    }

    #[test]
    fn degenerate_deadzone_config_is_always_off() {
        let c = cal(0, 10_000, 60, 60, false); // lo+hi > 100% — nothing usable
        assert_eq!(c.apply(5_000), 0);
    }

    #[test]
    fn bytes_roundtrip() {
        let c = cal(-1234, 987_654, 5, 12, true);
        let b = c.to_bytes();
        assert_eq!(Calibration::from_bytes(&b), Some(c));
    }

    #[test]
    fn from_bytes_rejects_short_buffer() {
        assert_eq!(Calibration::from_bytes(&[0u8; 4]), None);
    }

    #[test]
    fn span_ok_thresholds() {
        assert!(!Calibration::span_ok(0, MIN_SPAN - 1));
        assert!(Calibration::span_ok(0, MIN_SPAN));
        assert!(Calibration::span_ok(MIN_SPAN, 0)); // direction-agnostic
    }
}
