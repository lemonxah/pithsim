//! Codemasters / EA "F1" UDP telemetry decoder (F1 23 / F1 24 / F1 25).
//!
//! F1 streams several packet types (fire-and-forget UDP, default port 20777),
//! each a `#pragma pack(1)` little-endian struct led by a 29-byte `PacketHeader`.
//! The dash data we want is spread across three packet types, so this decoder
//! accumulates them into one [`Telemetry`] (keyed on the header's player car
//! index) and returns the merged snapshot on every handled packet:
//!   * CarTelemetry (id 6): speed, rpm, gear, throttle/brake/clutch, tyre temps
//!   * LapData      (id 2): lap times, position, lap number, sectors
//!   * CarStatus    (id 7): max rpm, fuel
//!
//! Only F1 23+ (29-byte header) is supported; F1 22's 24-byte header + different
//! LapData layout is rejected to avoid mis-parsing.

use std::sync::{Mutex, OnceLock};

use super::decoders::{Decoded, GameDecoder};
use super::le;
use pith_core::simhub::Telemetry;

const HEADER: usize = 29;
const CARS: usize = 22;

fn state() -> &'static Mutex<Telemetry> {
    static S: OnceLock<Mutex<Telemetry>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Telemetry::idle()))
}

pub struct F1Decoder;

impl GameDecoder for F1Decoder {
    fn name(&self) -> &'static str {
        "F1 (Codemasters)"
    }

    fn decode(&self, b: &[u8]) -> Option<Decoded> {
        if b.len() < HEADER {
            return None;
        }
        // Signature: packetFormat is the year; F1 23+ uses the 29-byte header.
        let fmt = le::u16(b, 0);
        if !(2023..=2030).contains(&fmt) {
            return None;
        }
        let packet_id = le::u8(b, 6);
        let player = le::u8(b, 27) as usize;
        if player >= CARS {
            return None;
        }

        let mut t = state().lock().unwrap();
        match packet_id {
            // ---- CarTelemetry: 60-byte elements ----
            6 => {
                let base = HEADER + 60 * player;
                if b.len() < base + 60 {
                    return None;
                }
                t.speed_kmh = le::u16(b, base) as i32;
                t.throttle = (le::f32(b, base + 2) * 100.0).round() as i32;
                t.steer = (le::f32(b, base + 6) * 100.0).round().clamp(-100.0, 100.0) as i32;
                t.brake = (le::f32(b, base + 10) * 100.0).round() as i32;
                t.clutch = le::u8(b, base + 14) as i32;
                t.gear = le::gear_byte(le::i8(b, base + 15) as i32);
                t.rpm = le::u16(b, base + 16) as i32;
                t.ignition = 1; // engine running while telemetry streams
                t.water_c = le::u16(b, base + 38) as i32; // engine temperature
                // Tyre surface temps, order [RL, RR, FL, FR] → all three zones.
                let (rl, rr, fl, fr) = (
                    le::u8(b, base + 30) as i32,
                    le::u8(b, base + 31) as i32,
                    le::u8(b, base + 32) as i32,
                    le::u8(b, base + 33) as i32,
                );
                set_tyre(&mut t, fl, fr, rl, rr);
                // Brake temps (u16, °C) and tyre pressures (f32, PSI), same
                // [RL, RR, FL, FR] order.
                t.bt_rl = le::u16(b, base + 22) as i32;
                t.bt_rr = le::u16(b, base + 24) as i32;
                t.bt_fl = le::u16(b, base + 26) as i32;
                t.bt_fr = le::u16(b, base + 28) as i32;
                t.tp_rl = le::f32(b, base + 40).round() as i32;
                t.tp_rr = le::f32(b, base + 44).round() as i32;
                t.tp_fl = le::f32(b, base + 48).round() as i32;
                t.tp_fr = le::f32(b, base + 52).round() as i32;
            }
            // ---- LapData: 57-byte elements ----
            2 => {
                let base = HEADER + 57 * player;
                if b.len() < base + 57 {
                    return None;
                }
                t.last_lap_ms = le::u32(b, base) as i32;
                t.cur_lap_ms = le::u32(b, base + 4) as i32;
                // Sector times split into ms part (u16) + minutes part (u8).
                t.s1_ms = le::u8(b, base + 10) as i32 * 60000 + le::u16(b, base + 8) as i32;
                t.s2_ms = le::u8(b, base + 13) as i32 * 60000 + le::u16(b, base + 11) as i32;
                t.position = le::u8(b, base + 32) as i32;
                t.laps_done = le::u8(b, base + 33) as i32;
            }
            // ---- CarStatus: 55-byte elements ----
            7 => {
                let base = HEADER + 55 * player;
                if b.len() < base + 55 {
                    return None;
                }
                // Fuel is in kg; convert to litres (petrol ≈ 0.75 kg/L) → decilitres.
                let fuel_l = le::f32(b, base + 5) / 0.75;
                let cap_l = le::f32(b, base + 9) / 0.75;
                t.fuel_dl = (fuel_l * 10.0).round() as i32;
                t.fuel_cap_dl = (cap_l * 10.0).round() as i32;
                t.fuel_laps_x10 = (le::f32(b, base + 13) * 10.0).round() as i32;
                t.max_rpm = le::u16(b, base + 17) as i32;
                t.shift_rpm = t.max_rpm; // F1 has no explicit shift point
                t.tc = le::u8(b, base) as i32; // traction control (0..2)
                t.abs = le::u8(b, base + 1) as i32; // anti-lock brakes (0/1)
                t.brake_bias_x10 = le::u8(b, base + 3) as i32 * 10; // front brake bias %
                t.pit_limiter = le::u8(b, base + 4) as i32; // pit limiter status
            }
            _ => return None, // packet type we don't use
        }
        Some(Decoded {
            telem: *t,
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

    fn pkt(id: u8, player: u8, len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0..2].copy_from_slice(&2024u16.to_le_bytes()); // packetFormat
        b[6] = id;
        b[27] = player;
        b
    }

    #[test]
    fn car_telemetry_player_fields() {
        let p = 5usize;
        let mut b = pkt(6, p as u8, HEADER + 60 * CARS);
        let base = HEADER + 60 * p;
        b[base..base + 2].copy_from_slice(&8000u16.to_le_bytes()); // speed
        b[base + 15] = 0xFFu8; // gear = -1 (reverse) as i8
        b[base + 16..base + 18].copy_from_slice(&10500u16.to_le_bytes()); // rpm
        let dec = F1Decoder.decode(&b).unwrap();
        assert_eq!(dec.telem.speed_kmh, 8000);
        assert_eq!(dec.telem.rpm, 10500);
        assert_eq!(dec.telem.gear, b'R');
    }

    #[test]
    fn rejects_non_f1() {
        // A 324-byte Forza-ish buffer: packetFormat bytes won't be a plausible year.
        let mut b = vec![0u8; 324];
        b[0] = 1; // format = 1 → not a year
        assert!(F1Decoder.decode(&b).is_none());
    }
}
