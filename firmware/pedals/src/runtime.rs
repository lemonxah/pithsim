//! Firmware runtime: owns the live [`PedalConfig`] and the host-tested
//! [`pith_pedals_core::controller::Controller`] that turns a loadcell reading
//! into a joystick axis + servo target. All the real control math lives in
//! that crate (and is unit-tested on the host); this struct is the thin glue
//! the USB/OTA/main loop drives.
//!
//! Motor output is **disarmed by default**. `tick` always computes the
//! target position (so telemetry/tuning work immediately), but `main` only
//! forwards it to the servo once the pedal is homed AND the user has armed it
//! (`@ARM`) after the bench validation in `docs/pedals.md` §0. Until a real
//! ADS1256 reading and homing are wired by `main`, `tick` runs on a
//! placeholder code of 0 → the axis reads 0% and no motor command is sent.

use pith_pedals_core::controller::{Controller, Output};
use pith_pedals_core::protocol::{PedalAction, PedalConfig, PedalId, PedalState};

pub struct Runtime {
    pub config: PedalConfig,
    pub controller: Controller,
    pub state: PedalState,
    /// Latest control output (axis/target) for `main` + the `?` status reply.
    pub last: Output,
    /// True once `main` has homed the pedal (travel envelope known).
    pub homed: bool,
    /// True once the user has armed motor output post-validation (`@ARM`).
    pub armed: bool,
    soft_min_steps: i32,
}

impl Runtime {
    pub fn new() -> Self {
        // Brake is a safe starting default; the wizard sends the real
        // pedal_type + geometry via @CFG before the pedal is used.
        let config = PedalConfig::defaults(PedalId::Brake);
        let controller = Controller::new(&config);
        Runtime {
            config,
            controller,
            state: PedalState::default(),
            last: Output::default(),
            homed: false,
            armed: false,
            soft_min_steps: 0,
        }
    }

    /// Absolute step index of the soft-min endstop (0 until homed).
    pub fn soft_min(&self) -> i32 {
        self.soft_min_steps
    }

    /// Install the homed travel envelope (absolute steps) and mark the pedal
    /// homed. Driven by the `@HOME` command with bench-measured endstop
    /// values — NOT run automatically, since a real sweep drives the motor
    /// into its endstops and must be operator-supervised.
    pub fn home(&mut self, soft_min: i32, soft_max: i32, hard_min: i32, hard_max: i32) {
        self.soft_min_steps = soft_min;
        self.controller
            .set_travel(soft_min, soft_max, hard_min, hard_max);
        self.homed = true;
    }

    /// Replace the config and rebuild the controller's derived parameters.
    pub fn apply_config(&mut self, config: PedalConfig) {
        self.controller.apply_config(&config);
        self.config = config;
    }

    /// Feed the latest effect action from the dashboard's effects engine.
    pub fn apply_action(&mut self, action: PedalAction, now_ms: i64) {
        self.controller.apply_action(action, now_ms);
    }

    /// Run one control step. `raw_code` is the signed 24-bit ADS1256 reading
    /// (0 when no loadcell is present yet), `phys_steps_from_min` the measured
    /// servo position. Updates `self.last` + `self.state` and returns the
    /// output so `main` can push the axis and (if armed+homed) the target.
    pub fn tick(
        &mut self,
        raw_code: i32,
        phys_steps_from_min: f32,
        tracking_error_steps: i32,
        now_ms: i64,
        dt_us: u32,
    ) -> Output {
        let out = self.controller.tick(
            raw_code,
            phys_steps_from_min,
            tracking_error_steps,
            now_ms,
            dt_us,
        );
        self.last = out;
        self.state.position_pct_x10 = (out.position_01 * 1000.0).clamp(0.0, 1000.0) as u16;
        self.state.force_n_x10 = (out.force_kg * 9.81 * 10.0).clamp(0.0, 65535.0) as u16;
        self.state.joystick_output = out.joystick;
        self.state.servo_on = self.armed;
        out
    }

    /// The 0..=65535 axis value from the last tick, for the HID report.
    pub fn output(&self) -> u16 {
        self.last.joystick
    }
}
