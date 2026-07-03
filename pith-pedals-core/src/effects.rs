//! Effect math: (1) host-side helpers that turn live telemetry into a
//! [`crate::protocol::PedalAction`] — this is what the dashboard's effects
//! engine does every tick, replacing the SimHub plugin's `DIYFFBPedal.cs`
//! telemetry glue; (2) every effect oscillator from the reference firmware's
//! `ESP32/include/ABSOscillation.h` (despite the file's name, it holds ALL
//! of them — ABS, RPM, bite-point, G-force, wheel-slip, road-impact, custom
//! vibration ×4), faithfully ported for full feature parity with that
//! project, for use once `firmware/pedals` grows an actuator driver (see
//! docs/pedals.md §3, Phase 2) — ported now because these are small,
//! bounded, fully-specified signal generators/filters with no motor-safety
//! implications on their own (they only compute an offset number; nothing
//! here drives a motor). One exact-math difference: the reference uses
//! `isin()`, a fast sine approximation (`FastTrig.h`) tuned for its MCU;
//! this uses `f32::sin()` (exact, and the Xtensa LX7 has a hardware FPU),
//! which can only make the waveform *more* accurate, not change its shape.

/// Fixed-size circular-buffer simple moving average — verbatim port of
/// `Common_Libs/MovingAverageFilter` (100-sample default in the reference,
/// used by [`GForceEffect`] and [`RoadImpactEffect`]).
pub struct MovingAverageFilter<const N: usize> {
    values: [f32; N],
    index: usize,
}

impl<const N: usize> Default for MovingAverageFilter<N> {
    fn default() -> Self {
        MovingAverageFilter {
            values: [0.0; N],
            index: 0,
        }
    }
}

impl<const N: usize> MovingAverageFilter<N> {
    pub fn process(&mut self, input: f32) -> f32 {
        self.values[self.index] = input;
        self.index = (self.index + 1) % N;
        self.values.iter().sum::<f32>() / N as f32
    }
}

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

/// RPM vibration — verbatim port of `RPMOscillation`. Unlike the trigger-
/// based oscillators, this reads a continuously-updated `rpm_pct` (0..100,
/// percent of redline) each tick rather than a discrete trigger event; the
/// 100ms "active window" instead means "no fresh rpm_pct update arrived" —
/// the offset holds at its last value in that gap rather than restarting.
/// Frequency interpolates `rpm_min_freq_hz..rpm_max_freq_hz` across
/// `rpm_pct`; amplitude gets up to +30% boosted at rpm_pct=100.
#[derive(Default)]
pub struct RpmOscillator {
    last_update_ms: i64,
    active_ms: f32,
    last_call_ms: i64,
    last_offset: f32,
}

pub struct RpmParams {
    pub rpm_pct: f32, // 0..100
    pub min_freq_hz: f32,
    pub max_freq_hz: f32,
    pub amplitude_01: f32,
    pub position_range_mm: f32,
}

impl RpmOscillator {
    /// Call whenever a fresh `rpm_pct` sample arrives (mirrors the reference
    /// writing `rpmValue_fl32` from the serial-rx task).
    pub fn update_rpm(&mut self, now_ms: i64) {
        self.last_update_ms = now_ms;
    }

    pub fn force_offset(&mut self, now_ms: i64, p: &RpmParams) -> f32 {
        let since_update_ms = (now_ms - self.last_update_ms) as f32;
        let offset = if since_update_ms > ACTIVE_WINDOW_MS {
            self.active_ms = 0.0;
            self.last_offset // holds, does not decay
        } else if p.rpm_pct == 0.0 {
            0.0
        } else {
            let amp = p.amplitude_01 * (1.0 + 0.3 * p.rpm_pct * 0.01);
            let freq_hz = (p.rpm_pct * (p.max_freq_hz - p.min_freq_hz) * 0.01)
                .clamp(p.min_freq_hz, p.max_freq_hz);
            self.active_ms += (now_ms - self.last_call_ms) as f32;
            let t_s = self.active_ms * 0.001;
            p.position_range_mm * amp * (360.0 * freq_hz * t_s).to_radians().sin()
        };
        self.last_call_ms = now_ms;
        self.last_offset = offset;
        offset
    }
}

/// Clutch bite-point vibration — verbatim port of `BitePointOscillation`:
/// trigger-based, fixed 100ms window, sine only, snaps to 0 (no smooth
/// decay) once the window elapses.
#[derive(Default)]
pub struct BitePointOscillator {
    last_trigger_ms: i64,
    active_ms: f32,
    last_call_ms: i64,
}

impl BitePointOscillator {
    pub fn trigger(&mut self, now_ms: i64) {
        self.last_trigger_ms = now_ms;
    }

    pub fn force_offset(
        &mut self,
        now_ms: i64,
        freq_hz: f32,
        amplitude_01: f32,
        position_range_mm: f32,
    ) -> f32 {
        let since_trigger_ms = (now_ms - self.last_trigger_ms) as f32;
        let offset = if since_trigger_ms > ACTIVE_WINDOW_MS {
            self.active_ms = 0.0;
            0.0
        } else {
            self.active_ms += (now_ms - self.last_call_ms) as f32;
            let t_s = self.active_ms * 0.001;
            position_range_mm * amplitude_01 * (360.0 * freq_hz * t_s).to_radians().sin()
        };
        self.last_call_ms = now_ms;
        offset
    }
}

/// G-force / weight-transfer effect — verbatim port of `GForceEffect`: not
/// trigger-based, a continuous value proportional to the live G reading,
/// scaled by the configured multiplier and smoothed with a 100-sample
/// moving average. `g_value` uses the reference's sentinel: exactly -128.0
/// means "no data" and reads as zero force.
#[derive(Default)]
pub struct GForceEffect {
    filter: MovingAverageFilter<100>,
}

/// 1/9.81, the reference's g-normalization constant (not `1.0 / 9.81`
/// directly, to match its literal to the last bit).
const G_NORM_INVERSE: f32 = 0.101_936_8;

impl GForceEffect {
    pub fn update(&mut self, g_value: f32, g_multiplier_pct: u8) -> f32 {
        let raw = if g_value == -128.0 {
            0.0
        } else {
            10.0 * g_value * (g_multiplier_pct as f32 * 0.01) * G_NORM_INVERSE
        };
        self.filter.process(raw)
    }
}

/// Wheel-slip vibration — verbatim port of `WSOscillation`: trigger-based,
/// fixed 100ms window, sine only, snaps to 0 (no smooth decay).
#[derive(Default)]
pub struct WheelSlipOscillator {
    last_trigger_ms: i64,
    active_ms: f32,
    last_call_ms: i64,
}

impl WheelSlipOscillator {
    pub fn trigger(&mut self, now_ms: i64) {
        self.last_trigger_ms = now_ms;
    }

    pub fn force_offset(
        &mut self,
        now_ms: i64,
        freq_hz: f32,
        amplitude_01: f32,
        position_range_mm: f32,
    ) -> f32 {
        let since_trigger_ms = (now_ms - self.last_trigger_ms) as f32;
        let offset = if since_trigger_ms > ACTIVE_WINDOW_MS {
            self.active_ms = 0.0;
            0.0
        } else {
            self.active_ms += (now_ms - self.last_call_ms) as f32;
            let t_s = self.active_ms * 0.001;
            position_range_mm * amplitude_01 * (360.0 * freq_hz * t_s).to_radians().sin()
        };
        self.last_call_ms = now_ms;
        offset
    }
}

/// Road/kerb impact effect — verbatim port of `RoadImpactEffect`: not
/// trigger-based, a continuous value proportional to the live impact
/// reading (0..100), scaled by the configured multiplier and a fixed 0.3
/// factor from the reference, smoothed with a 100-sample moving average.
#[derive(Default)]
pub struct RoadImpactEffect {
    filter: MovingAverageFilter<100>,
}

impl RoadImpactEffect {
    pub fn update(&mut self, impact_value_pct: f32, multiplier_pct: u8, force_range_n: f32) -> f32 {
        let raw = 0.3 * (multiplier_pct as f32 * 0.01) * force_range_n * (impact_value_pct * 0.01);
        self.filter.process(raw)
    }
}

/// One of the 4 general-purpose custom-vibration slots — verbatim port of
/// `CustomVibration`: trigger-based, fixed 100ms window, sine only, snaps
/// to 0. Frequency/amplitude/travel-range are passed per-call (the
/// reference computes them from config fresh each tick too, rather than
/// storing them on the instance).
#[derive(Default)]
pub struct CustomVibrationOscillator {
    last_trigger_ms: i64,
    active_ms: f32,
    last_call_ms: i64,
}

impl CustomVibrationOscillator {
    pub fn trigger(&mut self, now_ms: i64) {
        self.last_trigger_ms = now_ms;
    }

    /// `amplitude_pct_x10`: the reference takes amplitude as a percent
    /// scaled by 0.001 (not 0.01) against `travel_range_mm` — reproduced
    /// verbatim; callers should pass their percent value directly, this
    /// function applies the same 0.001 scale the reference does.
    pub fn force_offset(
        &mut self,
        now_ms: i64,
        freq_hz: f32,
        amplitude_pct: f32,
        travel_range_mm: f32,
    ) -> f32 {
        let since_trigger_ms = (now_ms - self.last_trigger_ms) as f32;
        let amp = amplitude_pct * 0.001 * travel_range_mm;
        let offset = if since_trigger_ms > ACTIVE_WINDOW_MS {
            self.active_ms = 0.0;
            0.0
        } else {
            self.active_ms += (now_ms - self.last_call_ms) as f32;
            let t_s = self.active_ms * 0.001;
            amp * (360.0 * freq_hz * t_s).to_radians().sin()
        };
        self.last_call_ms = now_ms;
        offset
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

    #[test]
    fn moving_average_converges_to_a_constant_input() {
        let mut f: MovingAverageFilter<4> = MovingAverageFilter::default();
        // Starts at 0 in every slot, so a constant input ramps up over N calls.
        assert_eq!(f.process(8.0), 2.0); // (8+0+0+0)/4
        assert_eq!(f.process(8.0), 4.0); // (8+8+0+0)/4
        assert_eq!(f.process(8.0), 6.0);
        assert_eq!(f.process(8.0), 8.0); // fully converged
        assert_eq!(f.process(8.0), 8.0); // stays there
    }

    #[test]
    fn rpm_oscillator_silent_at_zero_rpm_and_bounded_otherwise() {
        let mut o = RpmOscillator::default();
        o.update_rpm(0);
        let zero_p = RpmParams {
            rpm_pct: 0.0,
            min_freq_hz: 5.0,
            max_freq_hz: 40.0,
            amplitude_01: 0.5,
            position_range_mm: 10.0,
        };
        assert_eq!(o.force_offset(0, &zero_p), 0.0);

        let mut o2 = RpmOscillator::default();
        o2.update_rpm(0);
        let p = RpmParams {
            rpm_pct: 100.0,
            min_freq_hz: 5.0,
            max_freq_hz: 40.0,
            amplitude_01: 0.5,
            position_range_mm: 10.0,
        };
        let v = o2.force_offset(0, &p);
        // amplitude_01 * (1 + 0.3) * position_range_mm = 0.5*1.3*10 = 6.5
        assert!(v.abs() <= 6.5 + 0.001);
    }

    #[test]
    fn rpm_oscillator_holds_last_value_when_updates_stop() {
        let mut o = RpmOscillator::default();
        o.update_rpm(0);
        let p = RpmParams {
            rpm_pct: 50.0,
            min_freq_hz: 5.0,
            max_freq_hz: 40.0,
            amplitude_01: 0.5,
            position_range_mm: 10.0,
        };
        let v0 = o.force_offset(0, &p);
        // No update_rpm() called again — after the active window, holds v0
        // rather than decaying to zero (unlike the trigger-based oscillators).
        let v1 = o.force_offset(500, &p);
        assert_eq!(v1, v0);
    }

    #[test]
    fn bite_point_oscillator_snaps_to_zero_after_window() {
        let mut o = BitePointOscillator::default();
        o.trigger(0);
        let v0 = o.force_offset(0, 20.0, 0.5, 5.0);
        assert!(v0.abs() <= 2.5 + 0.001);
        assert_eq!(o.force_offset(500, 20.0, 0.5, 5.0), 0.0);
    }

    #[test]
    fn g_force_effect_zero_at_sentinel_and_smooths() {
        let mut g = GForceEffect::default();
        assert_eq!(g.update(-128.0, 100), 0.0);
        // A constant non-sentinel input should converge toward a nonzero,
        // correctly-signed average rather than jumping there immediately.
        let mut g2 = GForceEffect::default();
        let first = g2.update(5.0, 100);
        let mut last = first;
        for _ in 0..200 {
            last = g2.update(5.0, 100);
        }
        assert!(last > first); // ramped up from the initial (zero-padded) average
        assert!(last > 0.0);
    }

    #[test]
    fn wheel_slip_oscillator_snaps_to_zero_after_window() {
        let mut o = WheelSlipOscillator::default();
        o.trigger(0);
        let v0 = o.force_offset(0, 15.0, 1.0, 8.0);
        assert!(v0.abs() <= 8.0 + 0.001);
        assert_eq!(o.force_offset(500, 15.0, 1.0, 8.0), 0.0);
    }

    #[test]
    fn road_impact_effect_scales_and_smooths() {
        let mut r = RoadImpactEffect::default();
        assert_eq!(r.update(0.0, 100, 100.0), 0.0);
        let mut r2 = RoadImpactEffect::default();
        let first = r2.update(100.0, 100, 100.0);
        let mut last = first;
        for _ in 0..200 {
            last = r2.update(100.0, 100, 100.0);
        }
        // Converges to 0.3 * 1.0 * 100.0 * 1.0 = 30.0
        assert!((last - 30.0).abs() < 0.01);
    }

    #[test]
    fn custom_vibration_snaps_to_zero_after_window() {
        let mut o = CustomVibrationOscillator::default();
        o.trigger(0);
        let v0 = o.force_offset(0, 10.0, 50.0, 20.0);
        // amp = 50 * 0.001 * 20 = 1.0
        assert!(v0.abs() <= 1.0 + 0.001);
        assert_eq!(o.force_offset(500, 10.0, 50.0, 20.0), 0.0);
    }
}
