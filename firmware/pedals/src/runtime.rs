//! Shared runtime state: the active [`PedalConfig`], the latest
//! [`PedalAction`] from the dashboard's effects engine, and the device's
//! live [`PedalState`].
//!
//! **Phase 1 scope** (see `docs/pedals.md`): [`Runtime::sample`] is a
//! placeholder — it does NOT read a real loadcell or drive a real motor.
//! It produces a bounded, deterministic value purely so the USB/JSON/OTA
//! pipeline is provable end-to-end on real hardware before any actuator
//! code exists. Wiring an ADS1220 loadcell and a stepper/servo driver is
//! Phase 2, bench-validated before it touches a physical actuator.

use pith_pedals_core::protocol::{PedalAction, PedalConfig, PedalId, PedalState};

pub struct Runtime {
    pub config: PedalConfig,
    pub action: PedalAction,
    pub state: PedalState,
}

impl Runtime {
    pub fn new() -> Self {
        Runtime {
            // Real hardware should call @CFG with its actual pedal_type once
            // the wizard runs; Brake is just a starting default.
            config: PedalConfig::defaults(PedalId::Brake),
            action: PedalAction::default(),
            state: PedalState::default(),
        }
    }

    /// PLACEHOLDER — no sensor/actuator yet (Phase 1, see module docs). Holds
    /// position/force at zero so the joystick axis and `?` status are well-
    /// defined while the rest of the pipeline (USB, config round-trip, OTA)
    /// gets proven out.
    pub fn sample(&mut self) {
        self.state.position_pct_x10 = 0;
        self.state.force_n_x10 = 0;
        self.state.error_code = 0;
        self.state.servo_on = false;
    }

    /// The 0..=65535 axis value for the current state, honoring
    /// `travel_as_joystick_output` (force vs. travel as the game-facing axis).
    pub fn output(&self) -> u16 {
        let val_x10 = if self.config.travel_as_joystick_output {
            self.state.position_pct_x10
        } else {
            // Force is only meaningful relative to max_force; without a real
            // loadcell this is always 0 in Phase 1.
            self.state.force_n_x10
        };
        ((val_x10 as u32 * 65535) / 1000).min(65535) as u16
    }
}
