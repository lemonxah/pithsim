//! The complete active-pedal control pipeline, tying every other module in
//! this crate into one per-tick step — the host-tested equivalent of the
//! reference project's `pedalUpdateTask` inner loop (`ESP32/src/Main.cpp`).
//! The firmware (`firmware/pedals`) is then trivial glue: read the ADS1256,
//! call [`Controller::tick`], write the joystick axis and the servo target.
//!
//! Per tick (mirrors the reference's ordering):
//! 1. raw ADC code → kg via [`crate::loadcell::LoadcellScale`]
//! 2. lever correction → force at the pedal face ([`crate::kinematics`])
//! 3. denoise ([`crate::filter::ForceFilter`], selectable Kalman/exp/raw)
//! 4. effect oscillators ([`crate::effects`]) → force + position offsets
//! 5. admittance integration ([`crate::admittance`]) → target step position
//! 6. joystick output (force- or travel-based) → 0..65535 axis
//!
//! All state lives on the struct (no globals), so a future multi-pedal build
//! just holds one [`Controller`] per pedal.

use crate::admittance::{Admittance, Inputs, Params};
use crate::curve::{ForceCurve, ScaledCurve};
use crate::effects::{
    AbsOscillator, AbsParams, BitePointOscillator, CustomVibrationOscillator, GForceEffect,
    RoadImpactEffect, RpmOscillator, RpmParams, WheelSlipOscillator,
};
use crate::filter::ForceFilter;
use crate::kinematics::Linkage;
use crate::loadcell::LoadcellScale;
use crate::protocol::{PedalAction, PedalConfig};

const GRAVITY_N_KG: f32 = 9.81;
/// Reference's `EFFECT_POSITION_SCALING_FACTOR_FL32` — position-offset
/// oscillator outputs (in mm-ish units) are scaled by this to become steps.
const EFFECT_POSITION_SCALING: f32 = 0.1;
const JOYSTICK_MAX: u16 = 0xFFFF;

/// One tick's outputs.
#[derive(Debug, Clone, Copy, Default)]
pub struct Output {
    /// 0..65535 game-facing joystick axis value.
    pub joystick: u16,
    /// Absolute target position for the servo, in steps.
    pub target_steps: f32,
    /// Filtered force at the pedal face (kg) — for the `?` status/telemetry.
    pub force_kg: f32,
    /// Virtual model position 0..1 — for telemetry.
    pub position_01: f32,
    /// True if the admittance oscillation detector fired this tick.
    pub oscillating: bool,
}

/// Static parameters derived once per config change (nothing per-tick here).
struct Derived {
    curve: ScaledCurve,
    linkage: Linkage,
    admittance_params: Params,
    loadcell: LoadcellScale,
    max_force_kg: f32,
    min_force_kg: f32,
    // effect param scratch
    abs_freq_hz: f32,
    abs_amplitude_01: f32,
    abs_sawtooth: bool,
    abs_affects_travel: bool,
    rpm_min_freq: f32,
    rpm_max_freq: f32,
    rpm_amplitude_01: f32,
    bite_freq: f32,
    bite_amplitude_01: f32,
    bite_trigger_pct: f32,
    bite_enabled: bool,
    g_multiplier: u8,
    ws_freq: f32,
    ws_amplitude_01: f32,
    road_multiplier: u8,
    cv_freq: [f32; 4],
    cv_amplitude_pct: [f32; 4],
    travel_as_joystick: bool,
    max_game_output_pct: u8,
    min_force_for_effects_kg: f32,
    steps_per_rev: u32,
    pitch_mm: f32,
    kf_force_model_order: u8,
    kf_force_model_noise: u8,
}

/// The whole control pipeline for one pedal.
pub struct Controller {
    derived: Derived,
    filter: ForceFilter,
    admittance: Admittance,
    // effect oscillators
    abs: AbsOscillator,
    rpm: RpmOscillator,
    bite: BitePointOscillator,
    gforce: GForceEffect,
    wheel_slip: WheelSlipOscillator,
    road: RoadImpactEffect,
    cv: [CustomVibrationOscillator; 4],
    // latest action from the host effects engine
    action: PedalAction,
    // homing / travel envelope (set by the firmware once homed)
    soft_min_steps: i32,
    soft_max_steps: i32,
    hard_min_steps: i32,
    hard_max_steps: i32,
}

impl Controller {
    /// Build from a config, seeding the loadcell variance with a nominal
    /// estimate (the firmware replaces it after the boot bias sweep).
    pub fn new(config: &PedalConfig) -> Self {
        let variance = crate::loadcell::VARIANCE_DEFAULT_KG2;
        Controller {
            derived: Self::derive(config),
            filter: ForceFilter::new(variance),
            admittance: Admittance::new(),
            abs: AbsOscillator::default(),
            rpm: RpmOscillator::default(),
            bite: BitePointOscillator::default(),
            gforce: GForceEffect::default(),
            wheel_slip: WheelSlipOscillator::default(),
            road: RoadImpactEffect::default(),
            cv: Default::default(),
            action: PedalAction::default(),
            soft_min_steps: 0,
            soft_max_steps: 0,
            hard_min_steps: 0,
            hard_max_steps: 0,
        }
    }

    /// Rebuild the static derived parameters after a config edit. Preserves
    /// the running filter/admittance/oscillator state so a live tweak doesn't
    /// glitch the pedal.
    pub fn apply_config(&mut self, config: &PedalConfig) {
        self.derived = Self::derive(config);
    }

    /// Install the boot-time loadcell bias/variance estimate.
    pub fn set_loadcell_bias(&mut self, zero_kg: f32, sigma_kg: f32, variance_kg2: f32) {
        self.derived.loadcell.set_bias(zero_kg, sigma_kg);
        self.filter = ForceFilter::new(variance_kg2);
    }

    /// Set the homed travel envelope (absolute steps). Until this is called
    /// the controller has no range and holds output at the min.
    pub fn set_travel(&mut self, soft_min: i32, soft_max: i32, hard_min: i32, hard_max: i32) {
        self.soft_min_steps = soft_min;
        self.soft_max_steps = soft_max;
        self.hard_min_steps = hard_min;
        self.hard_max_steps = hard_max;
    }

    /// Store the latest effect action from the host and fire trigger-based
    /// oscillators — mirrors the reference's serial-rx task writing the
    /// action struct + calling `.trigger()`. ABS forwards the CURRENT level
    /// every truthy tick (not an edge), per the reference plugin semantics.
    pub fn apply_action(&mut self, action: PedalAction, now_ms: i64) {
        if action.trigger_abs {
            self.abs.trigger(now_ms);
        }
        self.rpm.update_rpm(now_ms);
        if self.derived.bite_enabled {
            // bite fires from position vs trigger point in tick(); nothing here
        }
        if action.wheel_slip > 0 {
            self.wheel_slip.trigger(now_ms);
        }
        for (i, &cv_on) in action.trigger_cv.iter().enumerate() {
            if cv_on {
                self.cv[i].trigger(now_ms);
            }
        }
        self.action = action;
    }

    /// The virtual model position 0..1 (for telemetry between ticks).
    pub fn position_01(&self) -> f32 {
        self.admittance.model_pos_01()
    }

    /// One control step. `raw_code` is the signed 24-bit ADS1256 reading,
    /// `physical_steps_from_min` the measured servo position relative to the
    /// soft-min endstop, `now_ms`/`dt_us` the loop clock. Returns the
    /// joystick axis + servo target + telemetry.
    pub fn tick(
        &mut self,
        raw_code: i32,
        physical_steps_from_min: f32,
        servo_tracking_error_steps: i32,
        now_ms: i64,
        dt_us: u32,
    ) -> Output {
        let d = &self.derived;

        // 1. raw code -> kg
        let raw_kg = d.loadcell.weight_kg(raw_code);

        // 2. lever correction: needs the sled position in mm
        let travel_steps = (self.soft_max_steps - self.soft_min_steps) as f32;
        let rev_per_step = if d.steps_per_rev > 0 {
            1.0 / d.steps_per_rev as f32
        } else {
            0.0
        };
        let sled_mm = physical_steps_from_min * rev_per_step * d.pitch_mm;
        let pedal_force_kg = d.linkage.pedal_force(raw_kg, sled_mm);

        // 3. denoise — force filter selected/tuned by the pushed config
        // (0 = const-velocity KF, 1 = const-accel, 2 = exponential, else raw).
        let (filtered_kg, _velocity) = self.filter.update(
            pedal_force_kg,
            dt_us,
            d.kf_force_model_order,
            d.kf_force_model_noise,
        );

        // 4. effects -> force (kg) + position (steps) offsets
        let max_force_n = d.max_force_kg * GRAVITY_N_KG;
        let force_range_n = (d.max_force_kg - d.min_force_kg) * GRAVITY_N_KG;

        let mut effect_force_kg = 0.0f32;
        let mut effect_pos_units = 0.0f32;

        let effects_allowed =
            d.min_force_for_effects_kg <= 0.0 || filtered_kg >= d.min_force_for_effects_kg;

        if effects_allowed {
            // ABS (force + position components)
            let (abs_force_n, abs_pos_mm) = self.abs.force_offset(
                now_ms,
                &AbsParams {
                    freq_hz: d.abs_freq_hz,
                    amplitude_01: d.abs_amplitude_01,
                    force_range_n,
                    position_range_mm: d.pitch_mm.max(1.0) * 4.0,
                    sawtooth: d.abs_sawtooth,
                    affects_travel: d.abs_affects_travel,
                    track_condition_0_6: self.action.track_condition.min(6),
                },
            );
            effect_force_kg += abs_force_n / GRAVITY_N_KG;
            effect_pos_units += abs_pos_mm;

            // RPM (position)
            effect_pos_units += self.rpm.force_offset(
                now_ms,
                &RpmParams {
                    rpm_pct: self.action.rpm_pct as f32 / 255.0 * 100.0,
                    min_freq_hz: d.rpm_min_freq,
                    max_freq_hz: d.rpm_max_freq,
                    amplitude_01: d.rpm_amplitude_01,
                    position_range_mm: d.pitch_mm.max(1.0) * 4.0,
                },
            );

            // Bite point: fires when the pedal crosses the configured trigger %
            if d.bite_enabled && self.position_01() * 100.0 >= d.bite_trigger_pct {
                self.bite.trigger(now_ms);
            }
            effect_pos_units += self.bite.force_offset(
                now_ms,
                d.bite_freq,
                d.bite_amplitude_01,
                d.pitch_mm.max(1.0) * 4.0,
            );

            // Wheel slip (position)
            effect_pos_units += self.wheel_slip.force_offset(
                now_ms,
                d.ws_freq,
                d.ws_amplitude_01 * (self.action.wheel_slip as f32 / 255.0),
                d.pitch_mm.max(1.0) * 4.0,
            );

            // Custom vibration slots (position)
            for i in 0..4 {
                effect_pos_units += self.cv[i].force_offset(
                    now_ms,
                    d.cv_freq[i],
                    d.cv_amplitude_pct[i],
                    d.pitch_mm.max(1.0) * 4.0,
                );
            }

            // G-force + road impact -> additional force (kg). The reference
            // folds these into the max force; adding them as a force offset
            // keeps the pedal firming under load with less bookkeeping.
            let g_signed = self.action.g_value as f32 - 128.0;
            effect_force_kg += self.gforce.update(g_signed, d.g_multiplier) / GRAVITY_N_KG;
            effect_force_kg += self.road.update(
                self.action.impact_value as f32 / 255.0 * 100.0,
                d.road_multiplier,
                force_range_n,
            ) / GRAVITY_N_KG;
        }

        let effect_offset_steps = effect_pos_units * EFFECT_POSITION_SCALING;

        // 5. admittance -> target steps
        let (target_steps, dbg) = self.admittance.step(
            &d.admittance_params,
            &Inputs {
                loadcell_kg: filtered_kg,
                effect_force_kg,
                effect_offset_steps,
                curve: &d.curve,
                linkage: d.linkage,
                physical_steps_from_min,
                travel_steps,
                soft_min_steps: self.soft_min_steps,
                servo_tracking_error_steps,
                steps_per_rev: d.steps_per_rev,
                pitch_mm_per_rev: d.pitch_mm,
                hard_min_steps: self.hard_min_steps,
                hard_max_steps: self.hard_max_steps,
                dt_s: (dt_us.max(1) as f32) * 1e-6,
            },
        );

        // 6. joystick output
        let joystick = if d.travel_as_joystick {
            self.axis_from_fraction(self.admittance.model_pos_01())
        } else {
            let frac = if force_range_n > 0.0 {
                ((filtered_kg - d.min_force_kg) * GRAVITY_N_KG / force_range_n).clamp(0.0, 1.0)
            } else {
                0.0
            };
            self.axis_from_fraction(frac)
        };
        let _ = max_force_n;

        Output {
            joystick,
            target_steps,
            force_kg: filtered_kg,
            position_01: self.admittance.model_pos_01(),
            oscillating: dbg.is_oscillating,
        }
    }

    fn axis_from_fraction(&self, frac: f32) -> u16 {
        let capped = frac.clamp(0.0, 1.0) * (self.derived.max_game_output_pct as f32 / 100.0);
        (capped * JOYSTICK_MAX as f32) as u16
    }

    fn derive(config: &PedalConfig) -> Derived {
        // Build the force curve from the config's travel/force points.
        let curve = build_curve(config);
        let max_force_kg = config.max_force_n_x10 as f32 / 10.0 / GRAVITY_N_KG;
        let min_force_kg = config.preload_force_n_x10 as f32 / 10.0 / GRAVITY_N_KG;

        Derived {
            curve: ScaledCurve {
                curve,
                min_kg: min_force_kg,
                max_kg: max_force_kg,
            },
            linkage: Linkage {
                a_mm: config.length_a_mm as f32,
                b_mm: config.length_b_mm as f32,
                d_mm: config.length_d_mm as f32,
                c_horizontal_mm: config.length_c_horizontal_mm as f32,
                c_vertical_mm: config.length_c_vertical_mm as f32,
            },
            admittance_params: Params {
                virtual_mass_pct: config.virtual_mass_pct,
                virtual_damping_pct: config.virtual_damping_pct,
                damping_progression: config.damping_progression,
                coulomb_friction_n: config.coulomb_friction_0p1n as f32 * 0.1,
                max_force_kg,
                endstop_stiffness_kg_per_mm: config.endstop_stiffness_kg_per_mm,
                endstop_travel_range_mm: config.endstop_travel_range_mm,
            },
            loadcell: LoadcellScale::new(config.loadcell_rating_kg, config.invert_loadcell),
            max_force_kg,
            min_force_kg,
            abs_freq_hz: config.abs_frequency_hz as f32,
            abs_amplitude_01: config.abs_amplitude_kg20 as f32 / 20.0 / max_force_kg.max(1.0),
            abs_sawtooth: config.abs_sawtooth,
            abs_affects_travel: config.abs_affects_travel,
            rpm_min_freq: config.rpm_min_freq_hz as f32,
            rpm_max_freq: config.rpm_max_freq_hz as f32,
            rpm_amplitude_01: config.rpm_amplitude_kg as f32 / max_force_kg.max(1.0),
            bite_freq: config.bite_point_freq_hz as f32,
            bite_amplitude_01: config.bite_point_amplitude as f32 / max_force_kg.max(1.0),
            bite_trigger_pct: config.bite_point_trigger_pct as f32,
            bite_enabled: config.bite_point_enabled,
            g_multiplier: config.g_multiplier,
            ws_freq: config.wheel_slip_freq_hz as f32,
            ws_amplitude_01: config.wheel_slip_amplitude as f32 / max_force_kg.max(1.0),
            road_multiplier: config.road_impact_multiplier,
            cv_freq: [
                config.custom_vibration[0].frequency_hz as f32,
                config.custom_vibration[1].frequency_hz as f32,
                config.custom_vibration[2].frequency_hz as f32,
                config.custom_vibration[3].frequency_hz as f32,
            ],
            cv_amplitude_pct: [
                config.custom_vibration[0].amplitude as f32,
                config.custom_vibration[1].amplitude as f32,
                config.custom_vibration[2].amplitude as f32,
                config.custom_vibration[3].amplitude as f32,
            ],
            travel_as_joystick: config.travel_as_joystick_output,
            max_game_output_pct: config.max_game_output_pct,
            min_force_for_effects_kg: config.min_force_for_effects_n as f32 / GRAVITY_N_KG,
            // JSS57P factory microsteps/rev (reg 8), not the old iSV57T's 3750 —
            // hardcoding 3750 mis-scaled every step→mm conversion ~9×.
            steps_per_rev: crate::servo_jss57p::DEFAULT_MICROSTEPS_PER_REV as u32,
            pitch_mm: config.spindle_pitch_mm_per_rev.max(1) as f32,
            kf_force_model_order: config.kf_force_model_order,
            kf_force_model_noise: config.kf_force_model_noise,
        }
    }
}

fn build_curve(config: &PedalConfig) -> ForceCurve {
    let n = config
        .curve_travel_pct_x10
        .len()
        .min(config.curve_force_pct_x10.len());
    if n >= 2 {
        let pts: Vec<(f32, f32)> = (0..n)
            .map(|i| {
                (
                    config.curve_travel_pct_x10[i] as f32 / 10.0,
                    config.curve_force_pct_x10[i] as f32 / 10.0,
                )
            })
            .collect();
        // strictly-increasing travel is required; fall back to linear if the
        // config's points are malformed rather than panicking on-device.
        if let Some(c) = ForceCurve::from_points(&pts) {
            return c;
        }
    }
    ForceCurve::linear_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::PedalId;

    fn homed_controller() -> Controller {
        let mut cfg = PedalConfig::defaults(PedalId::Brake);
        // Give it real geometry so kinematics is well-defined.
        cfg.length_a_mm = 205;
        cfg.length_b_mm = 220;
        cfg.length_d_mm = 60;
        cfg.length_c_horizontal_mm = 215;
        cfg.length_c_vertical_mm = 60;
        cfg.length_travel_mm = 100;
        let mut c = Controller::new(&cfg);
        c.set_travel(1000, 16000, 0, 20000);
        c
    }

    #[test]
    fn rest_produces_zero_ish_axis() {
        let mut c = homed_controller();
        let mut out = Output::default();
        for _ in 0..200 {
            out = c.tick(0, 0.0, 0, 0, 500);
        }
        // No force -> joystick near zero, target near the min endstop.
        assert!(out.joystick < 3000, "rest axis too high: {}", out.joystick);
    }

    #[test]
    fn output_stays_within_hard_limits() {
        let mut c = homed_controller();
        let mut phys = 0.0f32;
        for i in 0..3000 {
            // Positive ADC code -> positive force. Simulate a servo that
            // tracks the last target.
            let code = if i > 500 { 200_000 } else { 0 };
            let out = c.tick(code, phys, 0, i as i64, 500);
            assert!(
                (0.0..=20000.0).contains(&out.target_steps),
                "target escaped hard limits: {}",
                out.target_steps
            );
            assert!(out.target_steps.is_finite());
            phys = (out.target_steps - 1000.0).clamp(0.0, 15000.0);
        }
    }

    #[test]
    fn force_reads_back_through_loadcell_scale() {
        let mut c = homed_controller();
        // A large positive code should produce a positive filtered force.
        let mut force = 0.0;
        for i in 0..500 {
            force = c.tick(500_000, 2000.0, 0, i as i64, 500).force_kg;
        }
        assert!(force > 0.0, "expected positive force, got {force}");
    }

    #[test]
    fn config_change_does_not_panic_or_nan() {
        let mut c = homed_controller();
        let mut cfg = PedalConfig::defaults(PedalId::Clutch);
        cfg.length_a_mm = 205;
        cfg.length_b_mm = 220;
        cfg.length_d_mm = 60;
        cfg.length_c_horizontal_mm = 215;
        cfg.length_c_vertical_mm = 60;
        c.apply_config(&cfg);
        let out = c.tick(100_000, 3000.0, 0, 10, 500);
        assert!(out.target_steps.is_finite());
    }

    #[test]
    fn effects_action_is_accepted() {
        let mut c = homed_controller();
        let action = PedalAction {
            trigger_abs: true,
            rpm_pct: 200,
            wheel_slip: 100,
            trigger_cv: [true, false, false, false],
            ..PedalAction::default()
        };
        c.apply_action(action, 5);
        // Should run without panicking and stay bounded.
        for i in 0..300 {
            let out = c.tick(150_000, 4000.0, 0, i as i64, 500);
            assert!(out.target_steps.is_finite());
        }
    }
}
