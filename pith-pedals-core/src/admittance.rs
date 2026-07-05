//! Admittance (mass–spring–damper) force-control model — the port of the
//! reference project's `MoveByAdmittanceStrategy` (+ its helper functions)
//! from `ESP32/include/StepperMovementStrategy.h`. This is what makes the
//! pedal an *active* pedal: it reads the user's force and integrates a 1-DOF
//! virtual mass through a Tustin (bilinear) IIR, then commands the motor to
//! track the virtual position.
//!
//! The reference keeps its integrator state in file-global `g_*` variables
//! and warns (in a long comment) that this makes it single-pedal-per-MCU.
//! This port encapsulates every one of those globals in [`Admittance`] so a
//! future multi-pedal build just holds one per pedal. Everything else — the
//! Tustin coefficients, soft endstop spring, Coulomb-friction blend, regen
//! power clamp, velocity choke, soft leash, the Landi-et-al oscillation
//! detector and position-gated virtual-mass adaptation — is ported
//! faithfully, with the reference's constants kept verbatim.
//!
//! Runs natively in *pedal task space* (arc length along the pedal face),
//! not actuator space, exactly like the reference — forward kinematics maps
//! the sled position to a pedal angle, the physics integrates in arc-length
//! metres, and analytical inverse kinematics ([`crate::kinematics`]) maps the
//! virtual position back to a target sled position in steps.

use crate::curve::ScaledCurve;
use crate::kinematics::Linkage;

const GRAVITY_N_KG: f32 = 9.81;
const MAX_ACCEL_MPS2: f32 = 50.0;
const MAX_REGEN_POWER_W: f32 = 1.0;
const MAX_ELASTOMER_MULTIPLIER: f32 = 4.0;
const LEASH_RATE: f32 = 0.5;
const LEASH_DEADBAND_01: f32 = 0.005;
const FRICTION_VELOCITY_BAND_MPS: f32 = 0.030;
// Oscillation detector (Landi et al.).
const OSC_PSI_THRESHOLD_N: f32 = 25.0;
const MASS_MAX_KG: f32 = 2.5;
const MASS_INCREASE_RATE_KG_S: f32 = 15.0;
const MASS_DECREASE_RATE_KG_S: f32 = 3.0;

/// The static + dynamic inputs to one control step. Grouped so the call site
/// (the firmware's control task) reads clearly; all of these come straight
/// from the live config, the force curve, and the measured servo state.
pub struct Inputs<'a> {
    /// Filtered loadcell force at the pedal face (kg) — already lever-
    /// corrected via [`Linkage::pedal_force`] and denoised.
    pub loadcell_kg: f32,
    /// Additive effect force offset (kg) from the oscillators (ABS force
    /// component). Reference's `effectOffsets_st.forceOffset_kg_fl32`.
    pub effect_force_kg: f32,
    /// Additive effect position offset (steps) injected straight into
    /// actuator space (ABS/vibration position component).
    pub effect_offset_steps: f32,
    /// The absolute force curve (preload..max kg) for spring + stiffness.
    pub curve: &'a ScaledCurve,
    /// Pedal linkage geometry.
    pub linkage: Linkage,
    /// Current measured physical sled position, in steps from the soft-min
    /// endstop (i.e. `getCurrentPosition() - softEndstopMin`).
    pub physical_steps_from_min: f32,
    /// Total travel between soft endstops, in steps.
    pub travel_steps: f32,
    /// Absolute step index of the soft-min endstop (added back to produce an
    /// absolute target).
    pub soft_min_steps: i32,
    /// Servo's own reported position/tracking error (steps) — feeds the
    /// tracking-error-dependent damping. 0 if unavailable.
    pub servo_tracking_error_steps: i32,
    pub steps_per_rev: u32,
    pub pitch_mm_per_rev: f32,
    /// Hard mechanical limits (absolute steps) — the final clamp.
    pub hard_min_steps: i32,
    pub hard_max_steps: i32,
    /// Integration timestep (seconds) — the control task interval.
    pub dt_s: f32,
}

/// Tunables mirrored from `PedalConfig`, already decoded to the reference's
/// working units/ranges.
pub struct Params {
    pub virtual_mass_pct: u8,
    pub virtual_damping_pct: u8,
    pub damping_progression: u8,
    pub coulomb_friction_n: f32, // coulombFrictionIn0p1N_u8 * 0.1
    pub max_force_kg: f32,
    pub endstop_stiffness_kg_per_mm: u8,
    pub endstop_travel_range_mm: u8,
}

/// Per-pedal admittance integrator state (everything the reference kept in
/// `g_*` globals).
#[derive(Debug, Clone)]
pub struct Admittance {
    v_model_pos_01: f32,
    v_model_vel_mps: f32,
    mass_adaptation_offset_kg: f32,
    last_net_force_tustin_n: f32,
    // Oscillation-detector filter state.
    psi_initialized: bool,
    prev_physical_pos_m: f32,
    filtered_physical_vel_mps: f32,
    filtered_physical_acc_mps2: f32,
    prev_filtered_physical_vel_mps: f32,
    psi_lowpass: f32,
    power_envelope_w: f32,
}

impl Default for Admittance {
    fn default() -> Self {
        Admittance {
            v_model_pos_01: 0.0,
            v_model_vel_mps: 0.0,
            mass_adaptation_offset_kg: 0.0,
            last_net_force_tustin_n: 0.0,
            psi_initialized: false,
            prev_physical_pos_m: 0.0,
            filtered_physical_vel_mps: 0.0,
            filtered_physical_acc_mps2: 0.0,
            prev_filtered_physical_vel_mps: 0.0,
            psi_lowpass: 0.0,
            power_envelope_w: 0.0,
        }
    }
}

/// Diagnostics from one step (the reference's `AdmittanceDebugState_t`
/// subset), useful for the dashboard's tuning scope later.
#[derive(Debug, Clone, Copy, Default)]
pub struct Debug {
    pub virtual_pos_01: f32,
    pub virtual_vel_mps: f32,
    pub virtual_acc_mps2: f32,
    pub active_mass_kg: f32,
    pub active_damping_ns_m: f32,
    pub is_oscillating: bool,
}

impl Admittance {
    pub fn new() -> Self {
        Self::default()
    }

    /// The virtual model position (0..1), for telemetry.
    pub fn model_pos_01(&self) -> f32 {
        self.v_model_pos_01
    }

    /// Runs one admittance step. Returns the absolute target position in
    /// steps for the motor, plus diagnostics. Direct port of
    /// `MoveByAdmittanceStrategy` (non-rudder path).
    pub fn step(&mut self, p: &Params, i: &Inputs) -> (f32, Debug) {
        let dt_s = i.dt_s;

        // --- 1. Parameters ---
        let virtual_mass_base = (p.virtual_mass_pct as f32 * 0.01).clamp(0.2, 5.0);
        let damping_zeta = (p.virtual_damping_pct as f32 * 0.01).clamp(0.5, 5.0);

        // --- 4. Forward kinematics (task space) ---
        let rev_per_step = if i.steps_per_rev > 0 {
            1.0 / i.steps_per_rev as f32
        } else {
            0.0
        };
        let max_sled_mm = i.travel_steps * rev_per_step * i.pitch_mm_per_rev;
        let actual_sled_frac = if i.travel_steps > 0.0 {
            (i.physical_steps_from_min / i.travel_steps).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let actual_sled_mm = actual_sled_frac * max_sled_mm;

        let angle_min = i.linkage.incline_angle_deg(0.0);
        let angle_max = i.linkage.incline_angle_deg(max_sled_mm);
        let angle_cur = i.linkage.incline_angle_deg(actual_sled_mm);

        let lever_arm_m = (i.linkage.b_mm + i.linkage.d_mm) * 0.001;
        let total_travel_m =
            (angle_max - angle_min).abs() * std::f32::consts::PI / 180.0 * lever_arm_m;

        let actual_pos_frac = if (angle_max - angle_min).abs() > 0.001 {
            ((angle_cur - angle_min) / (angle_max - angle_min)).clamp(0.0, 1.0)
        } else {
            0.0
        };

        // --- 5. Spring + local stiffness (from the cubic spline) ---
        let displacement_01 = self.v_model_pos_01.clamp(0.0, 1.0);
        let spring_force_kg = i.curve.force_kg(displacement_01);
        let spring_force_n = (spring_force_kg * GRAVITY_N_KG).max(0.0);

        // Local stiffness: kg per unit pos_01 → N/m over the arc.
        let stiffness_kg_per_unit = i.curve.gradient_kg_per_unit(displacement_01);
        let stiffness_kg_per_step = if i.travel_steps > 0.0 {
            stiffness_kg_per_unit / i.travel_steps
        } else {
            0.0
        };
        let local_stiffness_n_m =
            (stiffness_kg_per_unit / total_travel_m.max(0.0001) * GRAVITY_N_KG).max(1.0);

        // --- 6. External force ---
        let external_force_n = (i.loadcell_kg + i.effect_force_kg) * GRAVITY_N_KG;

        // --- 7. Dynamic travel limits (effects can push past soft endstops) ---
        let (lower_limit_01, mut upper_limit_01) = calc_dynamic_travel_limits(
            i.travel_steps,
            stiffness_kg_per_step,
            i.effect_offset_steps,
            i.effect_force_kg,
        );

        // --- 8. Soft endstop ---
        let mut current_stiffness_n_m = local_stiffness_n_m;
        let soft_endstop_force_n = calc_soft_endstop_force(
            self.v_model_pos_01,
            total_travel_m,
            p.endstop_travel_range_mm,
            p.endstop_stiffness_kg_per_mm,
            &mut current_stiffness_n_m,
            &mut upper_limit_01,
        );

        // --- 9. Oscillation detector ---
        let ideal_base_damping =
            damping_zeta * 2.0 * (virtual_mass_base * current_stiffness_n_m).sqrt();
        let total_spring_reaction_n = spring_force_n + soft_endstop_force_n;
        let has_active_effect = effect_is_active(i.effect_force_kg, i.effect_offset_steps);

        let is_oscillating = self.detect_oscillation(
            external_force_n,
            actual_pos_frac,
            total_travel_m,
            total_spring_reaction_n,
            ideal_base_damping,
            virtual_mass_base,
            dt_s,
            has_active_effect,
        );

        // --- 10. Position-gated mass adaptation ---
        let virtual_mass = self.adapt_virtual_mass(
            is_oscillating,
            dt_s,
            virtual_mass_base,
            has_active_effect,
            actual_pos_frac,
        );

        // --- 11. Active damping (base + elastomer hysteresis) ---
        let active_damping_ns_m = calc_active_damping(
            damping_zeta,
            virtual_mass,
            current_stiffness_n_m,
            self.v_model_pos_01,
            p.damping_progression,
        );

        // --- 12. Coulomb friction blend ---
        let friction_n = p.coulomb_friction_n;
        let friction_blend = (self.v_model_vel_mps / FRICTION_VELOCITY_BAND_MPS).clamp(-1.0, 1.0);

        // --- Tustin (bilinear) integration of M a + C v = F_net ---
        let net_force_without_damping_n =
            external_force_n - spring_force_n - soft_endstop_force_n - friction_n * friction_blend;

        let c_tustin = 2.0 / dt_s;
        let a0 = virtual_mass * c_tustin + active_damping_ns_m;
        let a1 = active_damping_ns_m - virtual_mass * c_tustin;
        let mut new_vel = self.v_model_vel_mps;
        if a0.abs() > 1e-5 {
            let b0 = 1.0 / a0;
            let b1 = 1.0 / a0;
            let a1n = a1 / a0;
            new_vel = b0 * net_force_without_damping_n + b1 * self.last_net_force_tustin_n
                - a1n * self.v_model_vel_mps;
        }
        self.last_net_force_tustin_n = net_force_without_damping_n;

        let mut acceleration_mps2 =
            ((new_vel - self.v_model_vel_mps) / dt_s).clamp(-MAX_ACCEL_MPS2, MAX_ACCEL_MPS2);

        // Regen power clamp.
        apply_regen_power_clamping(virtual_mass, self.v_model_vel_mps, &mut acceleration_mps2);

        // Velocity integration.
        self.v_model_vel_mps += acceleration_mps2 * dt_s;

        // --- 13. Velocity choke ---
        let max_physical_sled_vel_mps = if i.steps_per_rev > 0 {
            // MAXIMUM_STEPPER_SPEED (250k steps/s) * pitch / steps_per_rev, in m/s.
            250_000.0 * i.pitch_mm_per_rev / i.steps_per_rev as f32 * 0.001
        } else {
            0.8
        };
        let max_sled_pos_m = (max_sled_mm * 0.001).max(0.0001);
        let max_pedal_arc_vel_mps = max_physical_sled_vel_mps * (total_travel_m / max_sled_pos_m);
        self.v_model_vel_mps = self
            .v_model_vel_mps
            .clamp(-max_pedal_arc_vel_mps, max_pedal_arc_vel_mps);

        // --- 14. Position integration + boundary constraints ---
        let current_pos_m = self.v_model_pos_01 * total_travel_m + self.v_model_vel_mps * dt_s;
        self.v_model_pos_01 = if total_travel_m > 0.0001 {
            current_pos_m / total_travel_m
        } else {
            0.0
        };
        if self.v_model_pos_01 <= lower_limit_01 {
            self.v_model_pos_01 = lower_limit_01;
            if self.v_model_vel_mps < 0.0 {
                self.v_model_vel_mps = 0.0;
            }
        } else if self.v_model_pos_01 >= upper_limit_01 {
            self.v_model_pos_01 = upper_limit_01;
            if self.v_model_vel_mps > 0.0 {
                self.v_model_vel_mps = 0.0;
            }
        }

        // Soft leash (drift correction toward the physical position).
        let mut divergence_01 = actual_pos_frac - self.v_model_pos_01;
        if divergence_01.abs() < LEASH_DEADBAND_01 {
            divergence_01 = 0.0;
        } else {
            divergence_01 += if divergence_01 > 0.0 {
                -LEASH_DEADBAND_01
            } else {
                LEASH_DEADBAND_01
            };
        }
        self.v_model_pos_01 += divergence_01 * (LEASH_RATE * dt_s);

        // Silence unused-in-non-tracking-error-path warnings while keeping
        // the reference's inputs available for future tracking-error damping.
        let _ = (i.servo_tracking_error_steps, p.max_force_kg);

        // --- 15. Inverse kinematics → target steps ---
        let target_angle_deg = angle_min + self.v_model_pos_01 * (angle_max - angle_min);
        let target_sled_mm = i
            .linkage
            .sled_mm_for_angle(target_angle_deg)
            .unwrap_or(actual_sled_mm)
            .clamp(0.0, max_sled_mm);

        let steps_to_mm = rev_per_step * i.pitch_mm_per_rev;
        let mut target_steps = if steps_to_mm.abs() > 1e-9 {
            target_sled_mm / steps_to_mm + i.soft_min_steps as f32
        } else {
            i.soft_min_steps as f32
        };
        target_steps += i.effect_offset_steps;
        let target_steps = target_steps.clamp(i.hard_min_steps as f32, i.hard_max_steps as f32);

        let dbg = Debug {
            virtual_pos_01: self.v_model_pos_01,
            virtual_vel_mps: self.v_model_vel_mps,
            virtual_acc_mps2: acceleration_mps2,
            active_mass_kg: virtual_mass,
            active_damping_ns_m,
            is_oscillating,
        };
        (target_steps, dbg)
    }

    /// Landi-et-al oscillation detector — `DetectAdmittanceOscillation`.
    #[allow(clippy::too_many_arguments)]
    fn detect_oscillation(
        &mut self,
        external_force_n: f32,
        actual_pos_frac: f32,
        total_travel_m: f32,
        total_spring_reaction_n: f32,
        base_damping_ns_m: f32,
        current_mass_kg: f32,
        dt_s: f32,
        has_active_effect: bool,
    ) -> bool {
        let physical_pos_m = actual_pos_frac * total_travel_m;

        if !self.psi_initialized {
            self.prev_physical_pos_m = physical_pos_m;
            self.filtered_physical_vel_mps = 0.0;
            self.filtered_physical_acc_mps2 = 0.0;
            self.prev_filtered_physical_vel_mps = 0.0;
            self.psi_lowpass = 0.0;
            self.power_envelope_w = 0.0;
            self.psi_initialized = true;
            return false;
        }
        if dt_s < 0.0001 {
            return false;
        }

        let alpha_vel = 1.0 - (-dt_s / 0.002).exp();
        let alpha_acc = 1.0 - (-dt_s / 0.006).exp();

        let raw_vel = (physical_pos_m - self.prev_physical_pos_m) / dt_s;
        self.prev_physical_pos_m = physical_pos_m;
        self.filtered_physical_vel_mps =
            alpha_vel * raw_vel + (1.0 - alpha_vel) * self.filtered_physical_vel_mps;
        let raw_acc = (self.filtered_physical_vel_mps - self.prev_filtered_physical_vel_mps) / dt_s;
        self.prev_filtered_physical_vel_mps = self.filtered_physical_vel_mps;
        self.filtered_physical_acc_mps2 =
            alpha_acc * raw_acc + (1.0 - alpha_acc) * self.filtered_physical_acc_mps2;

        let mut expected_force_n = current_mass_kg * self.filtered_physical_acc_mps2
            + base_damping_ns_m * self.filtered_physical_vel_mps
            + total_spring_reaction_n;
        if !(0.05..=0.95).contains(&actual_pos_frac) {
            expected_force_n = external_force_n;
        }

        let mut psi_raw = (external_force_n - expected_force_n).abs();
        if has_active_effect {
            psi_raw = 0.0;
        }

        let alpha_hp = 1.0 - (-dt_s / 0.05).exp();
        self.psi_lowpass = alpha_hp * psi_raw + (1.0 - alpha_hp) * self.psi_lowpass;
        let psi_high_freq = (psi_raw - self.psi_lowpass).abs();

        let mechanical_power_w = external_force_n * self.filtered_physical_vel_mps;
        let power_to_user_w = if mechanical_power_w < 0.0 {
            -mechanical_power_w
        } else {
            0.0
        };
        if power_to_user_w > self.power_envelope_w {
            self.power_envelope_w = power_to_user_w; // instant attack
        } else {
            let release_alpha = 1.0 - (-dt_s / 0.100).exp();
            self.power_envelope_w -= self.power_envelope_w * release_alpha;
        }
        let power_weight = (self.power_envelope_w / 1.5).clamp(0.0, 1.0);
        let psi_final = psi_high_freq * power_weight;

        psi_final > OSC_PSI_THRESHOLD_N
    }

    /// Position-gated virtual-mass adaptation — `AdaptVirtualMass`.
    fn adapt_virtual_mass(
        &mut self,
        is_oscillating: bool,
        dt_s: f32,
        base_mass_kg: f32,
        has_active_effect: bool,
        actual_pos_frac: f32,
    ) -> f32 {
        if has_active_effect {
            return base_mass_kg + self.mass_adaptation_offset_kg;
        }
        if is_oscillating {
            self.mass_adaptation_offset_kg += MASS_INCREASE_RATE_KG_S * dt_s;
        } else if !(0.05..=0.95).contains(&actual_pos_frac) {
            self.mass_adaptation_offset_kg -= MASS_DECREASE_RATE_KG_S * dt_s;
        }
        if self.mass_adaptation_offset_kg < 0.0 {
            self.mass_adaptation_offset_kg = 0.0;
        }
        if base_mass_kg + self.mass_adaptation_offset_kg > MASS_MAX_KG {
            self.mass_adaptation_offset_kg = MASS_MAX_KG - base_mass_kg;
        }
        base_mass_kg + self.mass_adaptation_offset_kg
    }
}

fn calc_dynamic_travel_limits(
    travel_steps: f32,
    local_stiffness_kg_step: f32,
    effect_offset_steps: f32,
    effect_force_kg: f32,
) -> (f32, f32) {
    let additional_force_steps = if local_stiffness_kg_step > 0.0001 {
        effect_force_kg / local_stiffness_kg_step
    } else {
        0.0
    };
    if travel_steps > 0.0 {
        let ext = (effect_offset_steps + additional_force_steps) / travel_steps;
        (ext.min(0.0), 1.0 + ext.max(0.0))
    } else {
        (0.0, 1.0)
    }
}

fn calc_soft_endstop_force(
    v_model_pos_01: f32,
    total_travel_m: f32,
    travel_range_mm: u8,
    stiffness_kg_per_mm: u8,
    current_stiffness_n_m: &mut f32,
    upper_limit_01: &mut f32,
) -> f32 {
    let mut force_n = 0.0;
    if travel_range_mm as f32 > 0.01 {
        if v_model_pos_01 > 1.0 {
            let stiffness_n_m = stiffness_kg_per_mm as f32 * GRAVITY_N_KG * 1000.0;
            let deflection_m = (v_model_pos_01 - 1.0) * total_travel_m;
            force_n = stiffness_n_m * deflection_m;
            *current_stiffness_n_m = stiffness_n_m;
        }
        if total_travel_m > 0.0001 {
            *upper_limit_01 += (travel_range_mm as f32 / 1000.0) / total_travel_m;
        }
    }
    force_n
}

fn calc_active_damping(
    damping_zeta: f32,
    virtual_mass_kg: f32,
    current_stiffness_n_m: f32,
    v_model_pos_01: f32,
    damping_progression: u8,
) -> f32 {
    let critical = 2.0 * (virtual_mass_kg * current_stiffness_n_m).sqrt();
    let base = damping_zeta * critical;

    // Hunt-Crossley elastomer hysteresis (the default model).
    let ratio = damping_progression.clamp(0, 100) as f32 / 100.0;
    let progression_01 = ratio * ratio;
    let coeff = progression_01 * (MAX_ELASTOMER_MULTIPLIER * critical);
    let displacement_01 = v_model_pos_01.clamp(0.0, 1.0);
    base + coeff * displacement_01
}

fn apply_regen_power_clamping(virtual_mass_kg: f32, v_model_vel_mps: f32, acceleration: &mut f32) {
    let braking = (*acceleration > 0.0 && v_model_vel_mps < 0.0)
        || (*acceleration < 0.0 && v_model_vel_mps > 0.0);
    if braking {
        let power_w = (virtual_mass_kg * *acceleration * v_model_vel_mps).abs();
        if power_w > MAX_REGEN_POWER_W {
            *acceleration *= MAX_REGEN_POWER_W / power_w;
        }
    }
}

/// Whether an effect is influencing the pedal this tick. EITHER channel counts:
/// a force-only effect (ABS / G-force with `affects_travel = false`) leaves
/// `effect_offset_steps` at 0, and a position-only effect leaves
/// `effect_force_kg` at 0. Treating either alone as "no active effect" (an AND)
/// let the oscillation detector and psi/mass adaptation react to an injected
/// effect force as if it were user input, distorting the pedal during effects.
fn effect_is_active(effect_force_kg: f32, effect_offset_steps: f32) -> bool {
    effect_force_kg != 0.0 || effect_offset_steps != 0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::ForceCurve;

    #[test]
    fn effect_active_when_either_channel_nonzero() {
        assert!(effect_is_active(2.0, 0.0), "force-only effect (ABS/G-force)");
        assert!(effect_is_active(0.0, 5.0), "position-only effect");
        assert!(effect_is_active(2.0, 5.0), "both channels active");
        assert!(!effect_is_active(0.0, 0.0), "no effect at all");
    }

    fn test_curve() -> ScaledCurve {
        ScaledCurve {
            curve: ForceCurve::linear_default(),
            min_kg: 0.0,
            max_kg: 60.0,
        }
    }

    fn test_linkage() -> Linkage {
        Linkage {
            a_mm: 205.0,
            b_mm: 220.0,
            d_mm: 60.0,
            c_horizontal_mm: 215.0,
            c_vertical_mm: 60.0,
        }
    }

    fn params() -> Params {
        Params {
            virtual_mass_pct: 60,
            virtual_damping_pct: 100,
            damping_progression: 0,
            coulomb_friction_n: 0.0,
            max_force_kg: 60.0,
            endstop_stiffness_kg_per_mm: 50,
            endstop_travel_range_mm: 10,
        }
    }

    fn inputs<'a>(curve: &'a ScaledCurve, loadcell_kg: f32, physical_steps: f32) -> Inputs<'a> {
        Inputs {
            loadcell_kg,
            effect_force_kg: 0.0,
            effect_offset_steps: 0.0,
            curve,
            linkage: test_linkage(),
            physical_steps_from_min: physical_steps,
            travel_steps: 15000.0,
            soft_min_steps: 1000,
            servo_tracking_error_steps: 0,
            steps_per_rev: 3750,
            pitch_mm_per_rev: 5.0,
            hard_min_steps: 0,
            hard_max_steps: 20000,
            dt_s: 0.0005, // 2 kHz
        }
    }

    #[test]
    fn rest_state_stays_put() {
        let curve = test_curve();
        let p = params();
        let mut adm = Admittance::new();
        // No force, at rest — target should stay near the min endstop.
        let mut target = 0.0;
        for _ in 0..200 {
            let (t, _) = adm.step(&p, &inputs(&curve, 0.0, 0.0));
            target = t;
        }
        assert!(
            (target - 1000.0).abs() < 500.0,
            "rest target drifted to {target}"
        );
        assert!(adm.model_pos_01() < 0.1);
    }

    #[test]
    fn pushing_moves_pedal_forward() {
        let curve = test_curve();
        let p = params();
        let mut adm = Admittance::new();
        // Sustained force above the spring should advance the virtual model.
        let mut phys = 0.0f32;
        for _ in 0..2000 {
            let (t, _) = adm.step(&p, &inputs(&curve, 30.0, phys));
            // Simulate a perfect servo that instantly reaches the target.
            phys = (t - 1000.0).clamp(0.0, 15000.0);
        }
        assert!(
            adm.model_pos_01() > 0.2,
            "pedal didn't advance under load: pos={}",
            adm.model_pos_01()
        );
    }

    #[test]
    fn output_is_always_within_hard_limits() {
        let curve = test_curve();
        let p = params();
        let mut adm = Admittance::new();
        for f in [0.0f32, 100.0, -50.0, 500.0] {
            for _ in 0..200 {
                let (t, _) = adm.step(&p, &inputs(&curve, f, 7000.0));
                assert!(
                    (0.0..=20000.0).contains(&t),
                    "target {t} escaped hard limits for force {f}"
                );
            }
        }
    }

    #[test]
    fn does_not_produce_nan() {
        let curve = test_curve();
        let p = params();
        let mut adm = Admittance::new();
        for _ in 0..500 {
            let (t, d) = adm.step(&p, &inputs(&curve, 25.0, 3000.0));
            assert!(t.is_finite(), "target became NaN/inf");
            assert!(d.virtual_vel_mps.is_finite());
            assert!(d.active_damping_ns_m.is_finite());
        }
    }
}
