//! Bench-test input simulation (toggled from the config screen). Generates an
//! animated telemetry sweep so the whole dash + LEDs run without SimHub. Compact
//! port of the legacy sim_fill. Writes TELEM only while state.sim_on is set;
//! the USB dispatcher suppresses real telemetry while sim is on.

use std::thread;
use std::time::Duration;

use pith_core::simhub::Telemetry;

use crate::{state, usb};

pub fn spawn() {
    thread::Builder::new()
        .stack_size(3072)
        .name("sim".into())
        .spawn(|| {
            let (mut phase, mut gear, mut rpm, mut accel, mut lap) = (0i32, 1i32, 1500i32, true, 1i32);
            loop {
                if state::with(|s| s.sim_on) {
                    phase += 1;
                    let shift = 8100;
                    if accel {
                        rpm += 220;
                        if rpm >= shift {
                            if gear < 6 {
                                gear += 1;
                                rpm = 5200;
                            } else {
                                accel = false;
                            }
                        }
                    } else {
                        rpm -= 180;
                        if rpm <= 1500 {
                            gear = 1;
                            rpm = 1500;
                            accel = true;
                            lap += 1;
                        }
                    }
                    let trel = (rpm - 1500) / 60;
                    let mut t = Telemetry::idle();
                    t.gear = b'0' + gear as u8;
                    t.speed_kmh = gear * 38 + trel;
                    t.rpm = rpm;
                    t.max_rpm = 8800;
                    t.shift_rpm = shift;
                    t.cur_lap_ms = (phase * 50) % 95000;
                    t.last_lap_ms = 84012;
                    t.best_lap_ms = 82900;
                    t.pb_lap_ms = 82500;
                    t.est_lap_ms = 83100;
                    t.delta_ms = -3000 + (phase % 60) * 100;
                    t.position = 4;
                    t.field_size = 20;
                    t.laps_done = lap;
                    t.total_laps = 30;
                    t.laps_left = 30 - (lap % 30);
                    t.water_c = 90;
                    t.oil_c = 105;
                    t.tc = 4;
                    t.abs = 2;
                    t.brake_bias_x10 = 565;
                    t.fuel_dl = 700 - (lap % 80) * 8;
                    t.fuel_cap_dl = 750;
                    let tb = 80 + (phase / 8) % 30;
                    t.tt_fl_m = tb + 4;
                    t.tt_fr_m = tb + 5;
                    t.tt_rl_m = tb + 2;
                    t.tt_rr_m = tb + 3;
                    t.throttle = if accel { 100 } else { 0 };
                    t.brake = if accel { 0 } else { 85 };
                    t.tc_active = if accel && rpm > 7400 { 1 } else { 0 };
                    t.abs_active = if !accel && rpm < 4000 { 1 } else { 0 };
                    t.s1_ms = 28000;
                    t.s2_ms = 30900;
                    t.s3_ms = 25600;
                    t.bs1_ms = 28100;
                    t.bs2_ms = 30900;
                    t.bs3_ms = 25600;
                    *usb::TELEM.lock().unwrap() = t;
                    usb::note_telem(); // sim frames keep the screens awake too
                }
                thread::sleep(Duration::from_millis(50));
            }
        })
        .expect("spawn sim task");
}
