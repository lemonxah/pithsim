//! Effect math: (1) host-side helpers that turn live telemetry into a
//! [`crate::protocol::PedalAction`] — this is what the dashboard's effects
//! engine does every tick, replacing the SimHub plugin's `DIYFFBPedal.cs`
//! telemetry glue; (2) [`AbsOscillator`], a faithful port of the reference
//! firmware's ABS pulsation waveform generator, for use once `firmware/pedals`
//! grows an actuator driver (see docs/pedals.md §3, Phase 2) — ported now
//! because it's a small, bounded, fully-specified signal generator with no
//! motor-safety implications on its own (it only produces a decaying
//! sine/sawtooth offset; nothing here drives a motor).
//!
//! The other reference oscillators (RPM/G/wheel-slip/road-impact/custom
//! vibration) are NOT ported yet — their class bodies weren't read as part
//! of this pass, and fabricating their internals from the field names alone
//! would risk silently wrong effect behavior. Port them the same way once
//! read from source.

/// Rising-edge trigger: fires once when `active` transitions false -> true.
/// Mirrors the reference plugin calling the firmware's `oscillator.trigger()`
/// once per ABS/TC activation rather than every tick it's active.
#[derive(Default)]
pub struct EdgeTrigger {
    was_active: bool,
}

impl EdgeTrigger {
    pub fn update(&mut self, active: bool) -> bool {
        let fired = active && !self.was_active;
        self.was_active = active;
        fired
    }
}

/// 0..255 fraction of `value / max` (both same units), clamped.
pub fn pct_byte(value: f32, max: f32) -> u8 {
    if max <= 0.0 {
        return 0;
    }
    ((value / max).clamp(0.0, 1.0) * 255.0).round() as u8
}

/// ABS pulsation waveform generator — verbatim port of `ABSOscillation` from
/// `ESP32/include/ABSOscillation.h`. `trigger()` (rising edge) restarts the
/// active window; `force_offset()` is called every control-loop tick and
/// returns `(force_offset_n, position_offset_mm)` — exactly one of the pair
/// is non-zero depending on `affects_travel`, matching the reference's
/// `absForceOrTarvelBit`. After ~100ms with no trigger it decays smoothly to
/// zero rather than snapping off.
pub struct AbsOscillator {
    last_trigger_ms: i64,
    active_ms: f32,
    last_call_ms: i64,
    force_offset_n: f32,
    position_offset_mm: f32,
}

/// Tunables for one [`AbsOscillator::force_offset`] tick, taken straight from
/// the matching `PedalConfig` fields (+ live track-wetness estimate).
pub struct AbsParams {
    pub freq_hz: f32,
    pub amplitude_01: f32,
    pub force_range_n: f32,
    pub position_range_mm: f32,
    /// false = sine, true = sawtooth (reference `absPattern_u8`).
    pub sawtooth: bool,
    /// false = force offset, true = position offset (reference
    /// `absForceOrTarvelBit_u8`).
    pub affects_travel: bool,
    /// Reference's 0..6 wetness scale — widens the pulsation frequency by up
    /// to +60% at the wettest setting.
    pub track_condition_0_6: u8,
}

const ACTIVE_WINDOW_MS: f32 = 100.0;
const POS_DECAY_STEP: f32 = 10.0;
const FORCE_DECAY_STEP: f32 = 0.1;

impl Default for AbsOscillator {
    fn default() -> Self {
        AbsOscillator {
            last_trigger_ms: 0,
            active_ms: 0.0,
            last_call_ms: 0,
            force_offset_n: 0.0,
            position_offset_mm: 0.0,
        }
    }
}

impl AbsOscillator {
    pub fn trigger(&mut self, now_ms: i64) {
        self.last_trigger_ms = now_ms;
    }

    pub fn force_offset(&mut self, now_ms: i64, p: &AbsParams) -> (f32, f32) {
        let since_trigger_ms = (now_ms - self.last_trigger_ms) as f32;

        if since_trigger_ms > ACTIVE_WINDOW_MS {
            self.active_ms = 0.0;
            // Decay both offsets smoothly toward zero rather than snapping.
            self.position_offset_mm = decay_toward_zero(self.position_offset_mm, POS_DECAY_STEP);
            self.force_offset_n = decay_toward_zero(self.force_offset_n, FORCE_DECAY_STEP);
        } else {
            let freq_hz = (p.freq_hz * (1.0 + p.track_condition_0_6 as f32 * 0.1)).clamp(0.0, 50.0);
            self.active_ms += (now_ms - self.last_call_ms) as f32;
            let t_s = self.active_ms * 0.001;

            let amp_force_n = p.amplitude_01 * p.force_range_n;
            let amp_pos_mm = p.amplitude_01 * p.position_range_mm;

            let wave = if p.sawtooth {
                if freq_hz > 0.0 {
                    (t_s % (1.0 / freq_hz)) * freq_hz - 0.5
                } else {
                    0.0
                }
            } else {
                (360.0 * freq_hz * t_s).to_radians().sin()
            };

            if p.affects_travel {
                self.position_offset_mm = amp_pos_mm * wave;
                self.force_offset_n = 0.0;
            } else {
                self.force_offset_n = amp_force_n * wave;
                self.position_offset_mm = 0.0;
            }
        }
        self.last_call_ms = now_ms;
        (self.force_offset_n, self.position_offset_mm)
    }
}

fn decay_toward_zero(v: f32, step: f32) -> f32 {
    if v > step {
        v - step
    } else if v < -step {
        v + step
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_trigger_fires_once_per_activation() {
        let mut e = EdgeTrigger::default();
        assert!(!e.update(false));
        assert!(e.update(true)); // rising edge
        assert!(!e.update(true)); // still active, no re-fire
        assert!(!e.update(false));
        assert!(e.update(true)); // fires again on the next rising edge
    }

    #[test]
    fn pct_byte_clamps() {
        assert_eq!(pct_byte(-5.0, 100.0), 0);
        assert_eq!(pct_byte(0.0, 100.0), 0);
        assert_eq!(pct_byte(50.0, 100.0), 128);
        assert_eq!(pct_byte(100.0, 100.0), 255);
        assert_eq!(pct_byte(500.0, 100.0), 255);
        assert_eq!(pct_byte(1.0, 0.0), 0); // avoid div-by-zero
    }

    fn force_params() -> AbsParams {
        AbsParams {
            freq_hz: 12.0,
            amplitude_01: 0.5,
            force_range_n: 100.0,
            position_range_mm: 10.0,
            sawtooth: false,
            affects_travel: false,
            track_condition_0_6: 0,
        }
    }

    #[test]
    fn abs_oscillator_produces_bounded_force_and_decays() {
        let mut o = AbsOscillator::default();
        let p = force_params();
        o.trigger(0);
        let (f0, p0) = o.force_offset(0, &p);
        assert_eq!(p0, 0.0); // force mode: position stays zero
        assert!(f0.abs() <= 50.0 + 0.001); // amplitude_01 * force_range_n bound

        // Well past the active window with no new trigger: decays to zero.
        let (f1, _) = o.force_offset(500, &p);
        assert!(f1.abs() < f0.abs().max(0.001) || f1 == 0.0);
        for ms in (600..2000).step_by(20) {
            o.force_offset(ms, &p);
        }
        let (f_final, p_final) = o.force_offset(2000, &p);
        assert_eq!(f_final, 0.0);
        assert_eq!(p_final, 0.0);
    }

    #[test]
    fn abs_oscillator_travel_mode_only_moves_position() {
        let mut o = AbsOscillator::default();
        o.trigger(0);
        let p = AbsParams {
            freq_hz: 10.0,
            amplitude_01: 1.0,
            force_range_n: 100.0,
            position_range_mm: 5.0,
            sawtooth: false,
            affects_travel: true,
            track_condition_0_6: 0,
        };
        let (f, p_off) = o.force_offset(10, &p);
        assert_eq!(f, 0.0);
        assert!(p_off.abs() <= 5.0 + 0.001);
    }
}
