//! Continuous idle drift compensation: while the handbrake sits still near
//! the calibrated idle point, slowly nudge `idle_raw` to track thermal/
//! mechanical drift — so it still reads exactly 0% at rest, without needing
//! the PC app or a manual recalibration. Runs standalone in the firmware, so
//! it keeps working even when nothing's connected over USB.
//!
//! Safety gate: a stable reading only counts as drift if it's already close
//! to the current idle point. A steady reading far from idle (someone
//! holding a partial or full pull, e.g. trail-braking) is never treated as
//! "at rest" — it just doesn't move the anchor at all.

use pith_hb_core::Calibration;

/// Consecutive stable samples required before accepting a drift nudge.
/// HX711 samples arrive at 10-80 SPS, so this is roughly 1-1.5s of stillness.
const STABLE_SAMPLES: u32 = 15;

pub struct AutoZero {
    anchor: i32,
    run: u32,
}

impl AutoZero {
    pub fn new() -> Self {
        AutoZero { anchor: 0, run: 0 }
    }

    /// Feed one filtered raw sample; may nudge `cal.idle_raw` toward it.
    /// Thresholds scale with the calibrated idle..max span rather than being
    /// fixed raw-count constants, since that span varies a lot by load cell.
    pub fn observe(&mut self, raw: i32, cal: &mut Calibration) {
        if !cal.calibrated {
            self.run = 0;
            return;
        }
        let span = (cal.max_raw - cal.idle_raw).unsigned_abs() as i32;
        if span == 0 {
            self.run = 0;
            return;
        }
        // ~0.5% of span counts as "the same reading" (floored so it still
        // rejects real ADC noise on a very small/degenerate span).
        let quiet_band = (span / 200).max(50);
        if self.run == 0 || (raw - self.anchor).abs() > quiet_band {
            self.anchor = raw;
            self.run = 1;
            return;
        }
        self.run += 1;
        if self.run < STABLE_SAMPLES {
            return;
        }
        self.run = 0; // require a fresh stable run before nudging again

        // Reject anything more than ~8% of span from the current idle point —
        // that's a held pull, not drift.
        if (raw - cal.idle_raw).abs() > span / 12 {
            return;
        }
        // Gentle nudge (a quarter of the residual gap), not a hard snap, so a
        // reading never visibly jumps mid-session.
        cal.idle_raw += (raw - cal.idle_raw) / 4;
    }
}
