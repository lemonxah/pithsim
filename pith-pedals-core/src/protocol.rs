//! Config/action/state data model for the Pith active pedal, ported field-
//! for-field (same meaning, same tunables) from the reference project's
//! `PayloadPedalConfig_t` / `PayloadPedalAction_t` / `PayloadPedalStateBasic_t`
//! (`Common_Libs/DiyActivePedal_types/src/*.h` in
//! github.com/ChrGri/DIY-Sim-Racing-FFB-Pedal).
//!
//! Wire ENCODING deliberately does NOT copy the reference's byte-packed
//! struct + Fletcher-16 framing — that scheme exists to survive a noisy
//! Arduino softserial link, which the pith HID transport (fixed 64-byte
//! reports, no byte loss) doesn't need. Instead this follows the pith
//! convention already used by the DDU (`@C{json}`, `@UI{json}`) and shared
//! by `pith-ui`: plain structs, `serde` JSON over the `@`-command channel.
//! The *data model* (every field, its meaning and units) is faithful; the
//! *encoding* is pith's own.

use serde::{Deserialize, Serialize};

/// Which pedal this device is (mirrors `PedalId_t`); a single board may
/// serve any one of these depending on which pedal it's mounted in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PedalId {
    Clutch,
    Brake,
    Throttle,
}

/// One-time (or edited-in-the-wizard) pedal configuration: start/end travel,
/// force range, the 11-point force curve, effect tunables, and calibration/
/// geometry. Ported from `PayloadPedalConfig_t`; grouped with comments
/// matching the source's sections. All the `u8`-percent/`u8`-scaled fields
/// keep the reference's scale (documented per-field) so behavior matches
/// the project this is ported from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PedalConfig {
    // ---- pedal start/end travel, in percent of full physical range ----
    pub pedal_start_pct: u8,
    pub pedal_end_pct: u8,

    // ---- force range ----
    pub max_force_n: f32,
    pub preload_force_n: f32,

    // ---- force-vs-travel curve: up to 11 points, travel/force in percent ----
    pub curve_travel_pct: Vec<f32>,
    pub curve_force_pct: Vec<f32>,

    // ---- joystick output remap curve (also up to 11 points) ----
    pub joystick_map_orig_pct: Vec<f32>,
    pub joystick_map_mapped_pct: Vec<f32>,

    // ---- ABS pulsation effect ----
    pub abs_frequency_hz: u8,
    pub abs_amplitude_kg20: u8,   // amplitude in kg/20 (reference's unit)
    pub abs_sawtooth: bool,       // false = sine, true = sawtooth
    pub abs_affects_travel: bool, // false = force, true = travel

    // ---- pedal linkage geometry, mm (lengths per the reference's a/b/c/d model) ----
    pub length_a_mm: i16,
    pub length_b_mm: i16,
    pub length_d_mm: i16,
    pub length_c_horizontal_mm: i16,
    pub length_c_vertical_mm: i16,
    pub length_travel_mm: i16,

    // ---- ABS simulation (test button in the dashboard) ----
    pub simulate_abs: bool,
    pub simulate_abs_value: u8,

    // ---- RPM vibration effect ----
    pub rpm_max_freq_hz: u8,
    pub rpm_min_freq_hz: u8,
    pub rpm_amplitude_kg: u8,

    // ---- clutch bite-point effect ----
    pub bite_point_trigger_pct: u8,
    pub bite_point_amplitude: u8,
    pub bite_point_freq_hz: u8,
    pub bite_point_enabled: bool,

    // ---- G-force / weight-transfer effect ----
    pub g_multiplier: u8,
    pub g_window: u8,

    // ---- wheel-slip vibration ----
    pub wheel_slip_amplitude: u8,
    pub wheel_slip_freq_hz: u8,

    // ---- road/impact effect (kerbs, bumps) ----
    pub road_impact_multiplier: u8,
    pub road_impact_window: u8,

    // ---- 4 general-purpose custom vibration slots ----
    pub custom_vibration: [CustomVibration; 4],

    pub max_game_output_pct: u8,

    // ---- Kalman filters (force estimate + joystick output smoothing) ----
    pub kf_force_model_noise: u8,
    pub kf_force_model_order: u8,
    pub kf_joystick_enabled: bool,
    pub kf_joystick_model_noise: u8,

    pub debug_flags: u8,

    // ---- loadcell / calibration ----
    pub loadcell_rating_kg: u8, // reference stores kg/2; this field is the real kg value
    pub travel_as_joystick_output: bool, // false = force is the joystick axis, true = travel
    pub invert_loadcell: bool,
    pub invert_motor_direction: bool,
    pub spindle_pitch_mm_per_rev: u8,

    pub pedal_type: PedalId,
    pub step_loss_detection: bool,
    pub servo_idle_timeout_s: u8,
    pub min_force_for_effects_n: u8,
    pub config_hash: u32,
    pub endstop_detection_threshold: u8,

    // ---- virtual pedal (admittance model) ----
    pub virtual_mass_pct: u8,
    pub virtual_damping_pct: u8,

    // ---- endstop feel ----
    pub endstop_stiffness_kg_per_mm: u8,
    pub endstop_travel_range_mm: u8,

    // ---- elastomere/spring progression + static friction ----
    pub damping_progression: u8,
    pub coulomb_friction_0p1n: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CustomVibration {
    pub amplitude: u8,
    pub frequency_hz: u8,
}

impl PedalConfig {
    /// A conservative default: mid-travel curve, all effects near-zero
    /// amplitude. Effects should be turned up deliberately per profile, not
    /// default to "on" for a device that pushes against a foot.
    pub fn defaults(pedal_type: PedalId) -> Self {
        PedalConfig {
            pedal_start_pct: 0,
            pedal_end_pct: 100,
            max_force_n: match pedal_type {
                PedalId::Brake => 600.0,
                PedalId::Clutch => 200.0,
                PedalId::Throttle => 50.0,
            },
            preload_force_n: 0.0,
            curve_travel_pct: vec![0.0, 100.0],
            curve_force_pct: vec![0.0, 100.0],
            joystick_map_orig_pct: vec![0.0, 100.0],
            joystick_map_mapped_pct: vec![0.0, 100.0],
            abs_frequency_hz: 12,
            abs_amplitude_kg20: 0,
            abs_sawtooth: false,
            abs_affects_travel: false,
            length_a_mm: 0,
            length_b_mm: 0,
            length_d_mm: 0,
            length_c_horizontal_mm: 0,
            length_c_vertical_mm: 0,
            length_travel_mm: 0,
            simulate_abs: false,
            simulate_abs_value: 0,
            rpm_max_freq_hz: 0,
            rpm_min_freq_hz: 0,
            rpm_amplitude_kg: 0,
            bite_point_trigger_pct: 0,
            bite_point_amplitude: 0,
            bite_point_freq_hz: 0,
            bite_point_enabled: false,
            g_multiplier: 0,
            g_window: 0,
            wheel_slip_amplitude: 0,
            wheel_slip_freq_hz: 0,
            road_impact_multiplier: 0,
            road_impact_window: 0,
            custom_vibration: Default::default(),
            max_game_output_pct: 100,
            kf_force_model_noise: 10,
            kf_force_model_order: 1,
            kf_joystick_enabled: false,
            kf_joystick_model_noise: 10,
            debug_flags: 0,
            loadcell_rating_kg: 100,
            travel_as_joystick_output: false,
            invert_loadcell: false,
            invert_motor_direction: false,
            spindle_pitch_mm_per_rev: 4,
            pedal_type,
            step_loss_detection: true,
            servo_idle_timeout_s: 30,
            min_force_for_effects_n: 5,
            config_hash: 0,
            endstop_detection_threshold: 10,
            virtual_mass_pct: 20,
            virtual_damping_pct: 50,
            endstop_stiffness_kg_per_mm: 10,
            endstop_travel_range_mm: 2,
            damping_progression: 0,
            coulomb_friction_0p1n: 0,
        }
    }
}

/// Real-time effect triggers/magnitudes, computed from the live telemetry
/// merge (this is what SimHub's plugin used to send every tick; here it's
/// the dashboard's effects engine). Ported from `PayloadPedalAction_t` — one
/// magnitude byte (0..255) per effect, scaled against the corresponding
/// `PedalConfig` amplitude/frequency by the FIRMWARE's oscillator (the
/// waveform generator stays on-device; see docs/pedals.md §1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PedalAction {
    pub trigger_abs: bool,
    pub rpm_pct: u8,           // 0..255 = 0..100% of redline
    pub g_value: u8,           // 0..255 = 0..configured G window
    pub wheel_slip: u8,        // 0..255 = 0..100% slip ratio
    pub impact_value: u8,      // 0..255 = road/kerb impact magnitude
    pub trigger_cv: [bool; 4], // custom vibration slots 1-4
}

/// Live device state (position/force/joystick output + health), the
/// pedal's analogue of the handbrake's `?` status reply. Ported from
/// `PayloadPedalStateBasic_t`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PedalState {
    pub position_pct_x10: u16, // 0..1000, one implied decimal of percent
    pub force_n_x10: u16,      // 0.., one implied decimal of Newtons
    pub joystick_output: u16,  // raw 0..65535 axis value
    pub error_code: u8,
    pub servo_on: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trips_through_json() {
        let cfg = PedalConfig::defaults(PedalId::Brake);
        let json = serde_json::to_string(&cfg).unwrap();
        let back: PedalConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn action_round_trips_through_json() {
        let act = PedalAction {
            trigger_abs: true,
            rpm_pct: 200,
            g_value: 40,
            wheel_slip: 12,
            impact_value: 0,
            trigger_cv: [true, false, false, true],
        };
        let json = serde_json::to_string(&act).unwrap();
        let back: PedalAction = serde_json::from_str(&json).unwrap();
        assert_eq!(act, back);
    }
}
