//! EA SPORTS WRC (2023, EA/Codemasters) native UDP telemetry decoder.
//!
//! EA WRC does NOT reuse the legacy DiRT "extradata" array (see
//! `codemasters.rs`) — it has a new, JSON-configurable UDP system. This
//! decoder parses the game's DEFAULT packet: structure `"wrc"`, packet
//! `"session_update"` — 237 bytes, little-endian, tightly packed, no header.
//! The user enables it by setting `"bEnabled": true` (and, for a two-PC
//! setup, the Linux host's IP) in `Documents/My Games/WRC/telemetry/
//! config.json`; the default target is `127.0.0.1:20777`.
//!
//! Layout per the official EA SPORTS WRC UDP Telemetry Guide v1.3 and two
//! independent copies of the game-generated `readme/udp/wrc.json` (schema 1 /
//! data 3, 59 channels; size corroborated by tm-bt-led's wrc.js "Packed size
//! is 237 bytes"). Wheel order is **BL, BR, FL, FR**. The car-local frame is
//! x = +LEFT, y = up, z = +forward; acceleration is m/s² (not g).
//!
//! Note: since game patch v1.9 EA's anticheat prevents the game itself from
//! running under Proton/Wine — this decoder mainly serves a two-PC setup
//! (Windows game box streaming UDP to the Linux host) or pre-1.9 installs.

use super::decoders::{Decoded, GameDecoder};
use super::le;
use pith_core::simhub::Telemetry;

const PACKET_LEN: usize = 237;
const G_MS2: f32 = 9.80665;
/// Suspension-velocity full-scale for the 0..1000 impact proxy — same 2.0 m/s
/// hard-bottom-out cap the Codemasters/PCARS/R3E decoders document.
const SUSP_V_CAP: f32 = 2.0;

// Byte offsets from the packed channel order in `wrc.json`.
const OFF_GEAR_INDEX: usize = 37; // u8
const OFF_GEAR_NEUTRAL: usize = 38; // u8
const OFF_GEAR_REVERSE: usize = 39; // u8
const OFF_SPEED: usize = 41; // f32 m/s (body speed)
const OFF_ACCEL_X: usize = 73; // f32 m/s², +left
const OFF_ACCEL_Z: usize = 81; // f32 m/s², +forward
const OFF_HUB_VEL: usize = 137; // f32×4 m/s, BL BR FL FR (suspension velocity)
const OFF_CP_FWD_SPEED: usize = 153; // f32×4 m/s, contact-patch forward speed
const OFF_BRAKE_TEMP: usize = 169; // f32×4 °C, BL BR FL FR
const OFF_RPM_MAX: usize = 185; // f32, true rpm
const OFF_RPM_CURRENT: usize = 193; // f32, true rpm
const OFF_SHIFT_RPM_END: usize = 32; // f32, shiftlights_rpm_end
const OFF_SHIFT_RPM_VALID: usize = 36; // bool (1 byte)
const OFF_THROTTLE: usize = 197; // f32 0..1
const OFF_BRAKE: usize = 201; // f32 0..1
const OFF_CLUTCH: usize = 205; // f32 0..1
const OFF_STEERING: usize = 209; // f32 -1..1
const OFF_STAGE_TIME: usize = 217; // f32 s
const OFF_STAGE_DIST: usize = 221; // f64 m
const OFF_STAGE_LEN: usize = 229; // f64 m

pub struct EaWrcDecoder;

impl GameDecoder for EaWrcDecoder {
    fn name(&self) -> &'static str {
        "EA SPORTS WRC"
    }

    fn decode(&self, b: &[u8]) -> Option<Decoded> {
        // The default `wrc`/`session_update` packet is exactly 237 bytes —
        // unique among our decoders (92/96, 232/311/324+, 264, 555–562,
        // magic-gated F1/PiBoSo).
        if b.len() != PACKET_LEN {
            return None;
        }
        // Content sanity gates so a random 237-byte blob can't masquerade.
        let speed_ms = le::f32(b, OFF_SPEED);
        let rpm = le::f32(b, OFF_RPM_CURRENT);
        let rpm_max = le::f32(b, OFF_RPM_MAX);
        if !speed_ms.is_finite()
            || !rpm.is_finite()
            || !rpm_max.is_finite()
            || !(0.0..=150.0).contains(&speed_ms.abs())
            || !(0.0..=30_000.0).contains(&rpm)
            || !(0.0..=30_000.0).contains(&rpm_max)
        {
            return None;
        }

        let mut t = Telemetry::idle();
        t.speed_kmh = (speed_ms.abs() * 3.6).round() as i32;
        t.rpm = rpm.round() as i32;
        t.max_rpm = rpm_max.round() as i32;
        // Shift point: the game's own shift-light top when flagged valid.
        let shift_end = le::f32(b, OFF_SHIFT_RPM_END);
        t.shift_rpm = if le::u8(b, OFF_SHIFT_RPM_VALID) != 0 && shift_end.is_finite() {
            shift_end.round() as i32
        } else {
            t.max_rpm
        };

        // Gear: an index compared against the packet's own neutral/reverse
        // indices; forward gears display as their distance past neutral.
        let idx = le::u8(b, OFF_GEAR_INDEX);
        let neutral = le::u8(b, OFF_GEAR_NEUTRAL);
        let reverse = le::u8(b, OFF_GEAR_REVERSE);
        t.gear = if idx == reverse {
            b'R'
        } else if idx == neutral {
            b'N'
        } else if idx > neutral {
            le::gear_byte((idx - neutral) as i32)
        } else {
            le::gear_byte(idx as i32)
        };

        t.throttle = (le::f32(b, OFF_THROTTLE) * 100.0).round().clamp(0.0, 100.0) as i32;
        t.brake = (le::f32(b, OFF_BRAKE) * 100.0).round().clamp(0.0, 100.0) as i32;
        t.clutch = (le::f32(b, OFF_CLUTCH) * 100.0).round().clamp(0.0, 100.0) as i32;
        t.steer = (le::f32(b, OFF_STEERING) * 100.0)
            .round()
            .clamp(-100.0, 100.0) as i32;

        t.cur_lap_ms = (le::f32(b, OFF_STAGE_TIME) * 1000.0).round().max(0.0) as i32;
        // Stage progress for the track map.
        let dist = le::f64(b, OFF_STAGE_DIST);
        let len = le::f64(b, OFF_STAGE_LEN);
        if len > 1.0 {
            t.track_pct = ((dist / len) * 1000.0).clamp(0.0, 1000.0) as i32;
        }

        // Brake temps, wheel order BL BR FL FR → our per-corner fields.
        t.bt_rl = le::f32(b, OFF_BRAKE_TEMP).round() as i32;
        t.bt_rr = le::f32(b, OFF_BRAKE_TEMP + 4).round() as i32;
        t.bt_fl = le::f32(b, OFF_BRAKE_TEMP + 8).round() as i32;
        t.bt_fr = le::f32(b, OFF_BRAKE_TEMP + 12).round() as i32;

        // FFB channels. Acceleration is m/s² in the car frame (x +left,
        // z +forward): g_long +accel/−brake needs no flip; g_lat is
        // positive-right by our convention, so negate the +left axis.
        t.g_long_x100 = (le::f32(b, OFF_ACCEL_Z) / G_MS2 * 100.0).round() as i32;
        t.g_lat_x100 = (-le::f32(b, OFF_ACCEL_X) / G_MS2 * 100.0).round() as i32;
        // Wheel slip: contact-patch forward speed vs body speed, max across
        // wheels (|cp − body| / max(body, 1) ×100 — same shape as the other
        // decoders).
        t.wheel_slip = crate::ffb::body_relative_slip(
            speed_ms,
            [
                le::f32(b, OFF_CP_FWD_SPEED),
                le::f32(b, OFF_CP_FWD_SPEED + 4),
                le::f32(b, OFF_CP_FWD_SPEED + 8),
                le::f32(b, OFF_CP_FWD_SPEED + 12),
            ],
        );
        // Suspension velocity → impact proxy: peak |hub velocity|, 2 m/s cap.
        t.susp_impact = crate::ffb::susp_impact_from_velocity(
            [
                le::f32(b, OFF_HUB_VEL),
                le::f32(b, OFF_HUB_VEL + 4),
                le::f32(b, OFF_HUB_VEL + 8),
                le::f32(b, OFF_HUB_VEL + 12),
            ],
            SUSP_V_CAP,
        );

        Some(Decoded {
            telem: t,
            car: None, // vehicle identity isn't in the default packet
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_f32(b: &mut [u8], off: usize, v: f32) {
        b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    fn put_f64(b: &mut [u8], off: usize, v: f64) {
        b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }

    /// A plausible mid-stage packet: 30 m/s, 5000 rpm, 3rd gear (neutral
    /// index 1 → forward gear index 4 displays as 3), braking at 0.8 with a
    /// 1 g decel and a solid suspension hit on the front-left.
    fn pkt() -> Vec<u8> {
        let mut b = vec![0u8; PACKET_LEN];
        b[OFF_GEAR_INDEX] = 4;
        b[OFF_GEAR_NEUTRAL] = 1;
        b[OFF_GEAR_REVERSE] = 0;
        put_f32(&mut b, OFF_SPEED, 30.0);
        put_f32(&mut b, OFF_RPM_CURRENT, 5000.0);
        put_f32(&mut b, OFF_RPM_MAX, 7500.0);
        b[OFF_SHIFT_RPM_VALID] = 1;
        put_f32(&mut b, OFF_SHIFT_RPM_END, 7000.0);
        put_f32(&mut b, OFF_THROTTLE, 0.1);
        put_f32(&mut b, OFF_BRAKE, 0.8);
        put_f32(&mut b, OFF_ACCEL_Z, -G_MS2); // 1 g braking
        put_f32(&mut b, OFF_ACCEL_X, 0.5 * G_MS2); // 0.5 g to the LEFT
                                                   // Front-left hub velocity spike (order BL BR FL FR → index 2).
        put_f32(&mut b, OFF_HUB_VEL + 8, 1.0);
        // One locked wheel: contact patch at 15 m/s vs body 30 m/s.
        put_f32(&mut b, OFF_CP_FWD_SPEED, 15.0);
        put_f32(&mut b, OFF_CP_FWD_SPEED + 4, 30.0);
        put_f32(&mut b, OFF_CP_FWD_SPEED + 8, 30.0);
        put_f32(&mut b, OFF_CP_FWD_SPEED + 12, 30.0);
        put_f32(&mut b, OFF_STAGE_TIME, 83.5);
        put_f64(&mut b, OFF_STAGE_DIST, 2500.0);
        put_f64(&mut b, OFF_STAGE_LEN, 10_000.0);
        b
    }

    #[test]
    fn decodes_core() {
        let d = EaWrcDecoder.decode(&pkt()).unwrap();
        assert_eq!(d.telem.speed_kmh, 108); // 30 m/s
        assert_eq!(d.telem.rpm, 5000);
        assert_eq!(d.telem.max_rpm, 7500);
        assert_eq!(d.telem.shift_rpm, 7000);
        assert_eq!(d.telem.gear, b'3'); // index 4, neutral 1
        assert_eq!(d.telem.brake, 80);
        assert_eq!(d.telem.cur_lap_ms, 83_500);
        assert_eq!(d.telem.track_pct, 250);
    }

    #[test]
    fn decodes_ffb_channels() {
        let d = EaWrcDecoder.decode(&pkt()).unwrap();
        assert_eq!(d.telem.g_long_x100, -100); // 1 g braking
        assert_eq!(d.telem.g_lat_x100, -50); // +left accel = negative (right-positive)
        assert_eq!(d.telem.wheel_slip, 50); // |15-30|/30
        assert_eq!(d.telem.susp_impact, 500); // 1.0 / 2.0 cap
    }

    #[test]
    fn gear_special_indices() {
        let mut b = pkt();
        b[OFF_GEAR_INDEX] = 1; // == neutral
        assert_eq!(EaWrcDecoder.decode(&b).unwrap().telem.gear, b'N');
        b[OFF_GEAR_INDEX] = 0; // == reverse
        assert_eq!(EaWrcDecoder.decode(&b).unwrap().telem.gear, b'R');
    }

    #[test]
    fn rejects_wrong_size_and_garbage() {
        assert!(EaWrcDecoder.decode(&[0u8; 264]).is_none());
        assert!(EaWrcDecoder.decode(&[0u8; 236]).is_none());
        // Right size, absurd rpm → rejected by the sanity gate.
        let mut b = pkt();
        put_f32(&mut b, OFF_RPM_CURRENT, 1.0e9);
        assert!(EaWrcDecoder.decode(&b).is_none());
    }
}
