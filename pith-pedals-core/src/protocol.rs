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
///
/// Every field is an integer, including ones the reference stores as
/// `float` (force, curve points) — scaled by 10 (`_x10` fields) instead.
/// This isn't a style preference: `serde_json` deserializing an `f32`/
/// `Vec<f32>` field on this crate's `xtensa-esp32s3-espidf` target hits a
/// genuine LLVM backend bug (`Cannot select: ... XtensaISD::PCREL_WRAPPER
/// TargetConstantPool ... [2 x float] [-1.0, 1.0]`, reproduced in both
/// debug and release — not an optimization artifact) inside its float
/// parser's codegen. Scaled integers sidestep it entirely, and match every
/// other pith wire struct (the DDU's are 100% integer fields already, for
/// what may well be the same reason).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
// Per-field fallback for JSON written by an older build: any field a saved
// config predates is filled from `PedalConfig::default()` instead of failing
// the whole parse. Without this, adding/renaming one field (e.g. the
// `step_loss_detection` → `step_loss_recovery`/`crash_detection` split) makes
// every stored profile un-deserializable — and the dashboard's whole-map
// `from_str(..).unwrap_or_default()` then silently drops ALL saved profiles.
#[serde(default)]
pub struct PedalConfig {
    // ---- pedal start/end travel, in percent of full physical range ----
    pub pedal_start_pct: u8,
    pub pedal_end_pct: u8,

    // ---- force range (Newtons x10, one implied decimal) ----
    pub max_force_n_x10: u16,
    pub preload_force_n_x10: u16,

    // ---- force-vs-travel curve: up to 11 points, travel/force in percent x10 ----
    pub curve_travel_pct_x10: Vec<u16>,
    pub curve_force_pct_x10: Vec<u16>,

    // ---- joystick output remap curve (also up to 11 points, percent x10) ----
    pub joystick_map_orig_pct_x10: Vec<u16>,
    pub joystick_map_mapped_pct_x10: Vec<u16>,

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
    // The reference packs these two as bits 0/1 of `stepLossFunctionFlags_u8`
    // (independent toggles in its "General" settings tab); kept as two bools.
    pub step_loss_recovery: bool,
    pub crash_detection: bool,
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

impl Default for PedalConfig {
    /// Only used as serde's per-field fallback (see `#[serde(default)]` above)
    /// when loading a config written before a field existed. The pedal id is a
    /// placeholder — real stored configs always carry their own `pedal_type`.
    fn default() -> Self {
        Self::defaults(PedalId::Brake)
    }
}

impl PedalConfig {
    /// A conservative default: mid-travel curve, all effects near-zero
    /// amplitude. Effects should be turned up deliberately per profile, not
    /// default to "on" for a device that pushes against a foot.
    pub fn defaults(pedal_type: PedalId) -> Self {
        PedalConfig {
            pedal_start_pct: 0,
            pedal_end_pct: 100,
            max_force_n_x10: match pedal_type {
                PedalId::Brake => 6000,
                PedalId::Clutch => 2000,
                PedalId::Throttle => 500,
            },
            preload_force_n_x10: 0,
            curve_travel_pct_x10: vec![0, 1000],
            curve_force_pct_x10: vec![0, 1000],
            joystick_map_orig_pct_x10: vec![0, 1000],
            joystick_map_mapped_pct_x10: vec![0, 1000],
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
            loadcell_rating_kg: 50, // DYLY-107 mini S-type, the cell on this build
            travel_as_joystick_output: false,
            invert_loadcell: false,
            invert_motor_direction: false,
            spindle_pitch_mm_per_rev: 4,
            pedal_type,
            step_loss_recovery: true,
            crash_detection: true,
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
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PedalAction {
    pub trigger_abs: bool,
    /// Track/surface condition, the side channel the reference smuggles
    /// through `triggerAbs_u8 > 1` (`trackCondition = triggerAbs - 1`);
    /// carried as its own field here. 0 = unknown/none.
    pub track_condition: u8,
    pub rpm_pct: u8, // 0..255 = 0..100% of redline
    /// SIGNED around 128, exactly like the reference's `gValue_u8 - 128`:
    /// 128 = 0 G, <128 braking/negative, >128 accelerating/positive. A
    /// sender with no G data must send 128, not 0 (0 means hard braking).
    pub g_value: u8,
    pub wheel_slip: u8,        // 0..255 = 0..100% slip ratio
    pub impact_value: u8,      // 0..255 = road/kerb impact magnitude
    pub trigger_cv: [bool; 4], // custom vibration slots 1-4
}

impl Default for PedalAction {
    fn default() -> Self {
        PedalAction {
            trigger_abs: false,
            track_condition: 0,
            rpm_pct: 0,
            g_value: 128, // 128-centered: this is "no G", 0 would be -max
            wheel_slip: 0,
            impact_value: 0,
            trigger_cv: [false; 4],
        }
    }
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
    fn config_survives_older_format_missing_and_renamed_fields() {
        // A config saved by a build that predates the
        // `step_loss_detection` → `step_loss_recovery`/`crash_detection` split:
        // it carries the OLD (now-removed) field and lacks both new ones. It
        // must still deserialize (from field defaults), not error — otherwise
        // the dashboard's whole-map `from_str(..).unwrap_or_default()` drops
        // every saved profile at once.
        let mut cfg = PedalConfig::defaults(PedalId::Brake);
        cfg.max_force_n_x10 = 987; // a value we expect to survive the migration
        let mut v = serde_json::to_value(&cfg).unwrap();
        let obj = v.as_object_mut().unwrap();
        obj.remove("step_loss_recovery");
        obj.remove("crash_detection");
        obj.insert("step_loss_detection".into(), serde_json::json!(true));
        let migrated: PedalConfig = serde_json::from_value(v).unwrap();
        assert_eq!(migrated.max_force_n_x10, 987, "existing fields preserved");
        // The renamed fields fall back to their defaults rather than failing.
        let d = PedalConfig::defaults(PedalId::Brake);
        assert_eq!(migrated.step_loss_recovery, d.step_loss_recovery);
        assert_eq!(migrated.crash_detection, d.crash_detection);
    }

    #[test]
    fn action_round_trips_through_json() {
        let act = PedalAction {
            trigger_abs: true,
            track_condition: 2,
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
