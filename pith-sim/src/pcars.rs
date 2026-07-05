//! SMS UDP telemetry decoder — Project CARS 2 and Automobilista 2 (AMS2 reuses
//! the identical `SMS_UDP_Definitions.hpp` layout).
//!
//! Fire-and-forget UDP (default port 5606). Many packet types share a 12-byte
//! `PacketBase`; the one we want is the **Telemetry** packet (`mPacketType == 0`,
//! `sTelemetryData`), a 559-byte datagram carrying the player car's physics in a
//! single self-contained packet. Everything we read lives in the stable head
//! region (offsets 12..344, before the trailing participant/compound strings),
//! so this is a clean stateless decode. Offsets follow the pack(1)
//! `sTelemetryData` in the public SMS_UDP_Definitions.hpp.

use super::decoders::{Decoded, GameDecoder};
use super::le;
use pith_core::simhub::Telemetry;

pub struct PCarsDecoder;

impl GameDecoder for PCarsDecoder {
    fn name(&self) -> &'static str {
        "Project CARS 2 / AMS2"
    }

    fn decode(&self, b: &[u8]) -> Option<Decoded> {
        // The telemetry datagram is 559 bytes on the wire; mPacketType (offset 10)
        // == 0 (eCarPhysics). Gate on both to avoid claiming another game's packet.
        if !(555..=562).contains(&b.len()) {
            return None;
        }
        if le::u8(b, 10) != 0 {
            return None;
        }

        let mut t = Telemetry::idle();
        // sCarFlags bitfield @17: HEADLIGHT=1, ENGINE_ACTIVE=2, ENGINE_WARNING=4,
        // SPEED_LIMITER=8, ABS=16, HANDBRAKE=32.
        let flags = le::u8(b, 17);
        t.headlights = (flags & 0x01 != 0) as i32;
        t.ignition = (flags & 0x02 != 0) as i32;
        t.pit_limiter = (flags & 0x08 != 0) as i32;
        t.abs_active = (flags & 0x10 != 0) as i32;
        t.steer = le::i8(b, 44) as i32 * 100 / 127; // filtered steering -127..127
        t.boost_kpa = le::u8(b, 46) as i32; // sBoostAmount — unit undocumented; treat as approximate
        t.throttle = le::u8(b, 13) as i32 * 100 / 255; // unfiltered throttle 0..255
        t.brake = le::u8(b, 14) as i32 * 100 / 255;
        t.clutch = le::u8(b, 16) as i32 * 100 / 255;
        t.oil_c = le::i16(b, 18) as i32;
        t.water_c = le::i16(b, 22) as i32;
        let fuel_cap_l = le::u8(b, 28) as f32; // litres (integer)
        let fuel_frac = le::f32(b, 32); // 0..1 of capacity
        t.fuel_cap_dl = (fuel_cap_l * 10.0).round() as i32;
        t.fuel_dl = (fuel_frac * fuel_cap_l * 10.0).round() as i32;
        t.speed_kmh = (le::f32(b, 36) * 3.6).round().max(0.0) as i32; // m/s → km/h
        t.rpm = le::u16(b, 40) as i32;
        t.max_rpm = le::u16(b, 42) as i32;
        t.shift_rpm = t.max_rpm;
        // Gear packed in one byte: low nibble = gear (0 N, 15 = reverse), high
        // nibble = number of gears.
        let g = (le::u8(b, 45) & 0x0F) as i32;
        let gear = if g == 15 { -1 } else { g };
        t.gear = le::gear_byte(gear);
        // Tyre surface temps °C, order [FL, FR, RL, RR].
        let (fl, fr, rl, rr) = (
            le::u8(b, 176) as i32,
            le::u8(b, 177) as i32,
            le::u8(b, 178) as i32,
            le::u8(b, 179) as i32,
        );
        set_tyre(&mut t, fl, fr, rl, rr);

        // Chassis G-forces from sLocalAcceleration[3] @100 (float x3, m/s²).
        // Offsets computed from the pack(1) `sTelemetryData` in the public
        // SMS_UDP_Definitions.hpp (Patch-5 UDP layout, shared verbatim by PC2 and
        // AMS2): …sOdometerKM@48, sOrientation@52, sLocalVelocity@64,
        // sWorldVelocity@76, sAngularVelocity@88, sLocalAcceleration@100.
        // The Madness engine keeps the ISI/gMotor local vehicle frame (+x left,
        // +y up, +z out the BACK of the car — the frame rF2's InternalsPlugin.hpp
        // and r3e.h document for their shared ancestry), confirmed for this very
        // packet by lmirel/mfc's pcars client notes ("sLocalAcceleration[2]:
        // -ACC, +BRK"; "[0]: +LT, -RT"). Negate both so g_long is +accel/−brake
        // and g_lat is positive-right (matching Forza's documented X=right).
        let (ax, az) = (le::f32(b, 100), le::f32(b, 108));
        if ax.is_finite() && az.is_finite() {
            t.g_lat_x100 = (-ax / 9.81 * 100.0).round() as i32;
            t.g_long_x100 = (-az / 9.81 * 100.0).round() as i32;
        }
        // Wheel slip: the packet carries wheel rotation sTyreRPS[4] @160 (rad/s
        // per the field's shared-memory twin) but NO tyre radius — mTyreRadius is
        // shared-memory-only — so rad/s can't be compared against sSpeed (m/s) to
        // form a slip ratio. wheel_slip stays 0 rather than guessing a radius.
        // Suspension VELOCITY sSuspensionVelocity[4] @328 (float x4; "rate of
        // change of pushrod deflection" — sSuspensionTravel@312 is metres, so
        // m/s) → impact proxy: peak |v| normalized to 0..1000 against the same
        // 2.0 m/s hard-bottom-out cap the Codemasters decoder documents.
        t.susp_impact = crate::ffb::susp_impact_from_velocity(
            [
                le::f32(b, 328),
                le::f32(b, 332),
                le::f32(b, 336),
                le::f32(b, 340),
            ],
            crate::ffb::SUSP_V_CAP_HARD_BOTTOM_OUT,
        );

        Some(Decoded {
            telem: t,
            car: None,
        })
    }
}

fn set_tyre(t: &mut Telemetry, fl: i32, fr: i32, rl: i32, rr: i32) {
    let (fl, fr, rl, rr) = (fl * 10, fr * 10, rl * 10, rr * 10); // °C → 0.1°C
    t.tt_fl_i = fl;
    t.tt_fl_m = fl;
    t.tt_fl_o = fl;
    t.tt_fr_i = fr;
    t.tt_fr_m = fr;
    t.tt_fr_o = fr;
    t.tt_rl_i = rl;
    t.tt_rl_m = rl;
    t.tt_rl_o = rl;
    t.tt_rr_i = rr;
    t.tt_rr_m = rr;
    t.tt_rr_o = rr;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn telem_packet() -> Vec<u8> {
        let mut b = vec![0u8; 559];
        b[10] = 0; // mPacketType = eCarPhysics
        b[11] = 2; // version
        b[36..40].copy_from_slice(&55.0f32.to_le_bytes()); // speed m/s
        b[40..42].copy_from_slice(&7200u16.to_le_bytes()); // rpm
        b[42..44].copy_from_slice(&8000u16.to_le_bytes()); // max rpm
        b[45] = 0x63; // numGears=6, gear=3
        b[13] = 255; // throttle
        b
    }

    #[test]
    fn decodes_core() {
        let dec = PCarsDecoder.decode(&telem_packet()).unwrap();
        assert_eq!(dec.telem.speed_kmh, 198); // 55*3.6
        assert_eq!(dec.telem.rpm, 7200);
        assert_eq!(dec.telem.max_rpm, 8000);
        assert_eq!(dec.telem.gear, b'3');
        assert_eq!(dec.telem.throttle, 100);
    }

    #[test]
    fn reverse_gear() {
        let mut b = telem_packet();
        b[45] = 0x6F; // gear nibble = 15 → reverse
        assert_eq!(PCarsDecoder.decode(&b).unwrap().telem.gear, b'R');
    }

    /// G-forces come from sLocalAcceleration@100 in the gMotor frame (+x left,
    /// +z back → both negated), susp_impact from sSuspensionVelocity@328
    /// (2.0 m/s cap); slip stays 0 (no tyre radius in the packet).
    #[test]
    fn g_forces_and_suspension_impact() {
        let mut b = telem_packet();
        b[100..104].copy_from_slice(&(-9.81f32).to_le_bytes()); // ax: left −1 g → right +1 g
        b[108..112].copy_from_slice(&9.81f32.to_le_bytes()); // az: back +1 g → braking
        b[328..332].copy_from_slice(&1.0f32.to_le_bytes()); // FL susp vel 1 m/s
        b[336..340].copy_from_slice(&(-3.0f32).to_le_bytes()); // RL −3 m/s → |v| caps
        let t = PCarsDecoder.decode(&b).unwrap().telem;
        assert_eq!(t.g_lat_x100, 100);
        assert_eq!(t.g_long_x100, -100); // braking
        assert_eq!(t.susp_impact, 1000); // 3 m/s > 2 m/s cap
        assert_eq!(t.wheel_slip, 0); // not derivable from this packet
    }

    #[test]
    fn rejects_wrong_type_or_size() {
        let mut b = telem_packet();
        b[10] = 3; // timings packet
        assert!(PCarsDecoder.decode(&b).is_none());
        assert!(PCarsDecoder.decode(&[0u8; 100]).is_none());
    }
}
