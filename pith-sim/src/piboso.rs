//! PiBoSo "UDP Proxy" decoder — one decoder for the whole PiBoSo family
//! (MX Bikes, Kart Racing Pro; GP Bikes / World Racing Series stream the same
//! framing and are accepted head-only).
//!
//! Enable in-game by editing `proxy_udp.ini` in the sim's install folder:
//! `[params] enable=1  ip=<dashboard ip>:<port>  delay=1  info=1`
//! (`info=1` adds the event/lap/split packets — needed for max/shift RPM and
//! the bike/kart name.)
//!
//! Wire format (little-endian, packed): every datagram is a 5-byte ASCII tag
//! (`data\0`, `evnt\0`, `sesn\0`, `lap \0`, `splt\0`) + state `i32` + time `i32`
//! (13-byte header), then the plugin-interface struct verbatim:
//!
//! | tag    | struct                                   | MXB pkt | KRP pkt |
//! |--------|------------------------------------------|---------|---------|
//! | `data` | `SPluginsBikeData_t` / `SPluginsKartData_t` | 201  | 189     |
//! | `evnt` | `SPluginsBikeEvent_t` / `SPluginsKartEvent_t` | 665 | 761    |
//! | `lap ` | `SPlugins*Lap_t` (4×i32)                 | 29      | 29      |
//! | `splt` | `SPlugins*Split_t` (3×i32)               | 25      | 25      |
//!
//! The `data` HEAD is identical across the family (rpm i32 @13, engine-temp
//! f32 @17, water f32 @21, gear i32 @25 (0 = N), fuel litres f32 @29,
//! speed m/s f32 @33) — verified against the official `mxb_example.c` and
//! `krp_example.c` plugin headers. Only the tails diverge (bike suspension vs
//! 4-wheel kart data), so input fields are read per known packet size and an
//! unknown-size `data` packet still yields the head fields.

use std::sync::{Mutex, OnceLock};

use super::decoders::{Decoded, GameDecoder};
use super::le;
use pith_core::simhub::Telemetry;

const HEADER: usize = 13;

struct Accum {
    t: Telemetry,
    car: Option<String>,
    /// Cumulative time at split 0 of the current lap (ms) — turns the `splt`
    /// packets' cumulative stamps into s1/s2 sector times.
    split0_ms: i32,
}

fn state() -> &'static Mutex<Accum> {
    static S: OnceLock<Mutex<Accum>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Accum { t: Telemetry::idle(), car: None, split0_ms: 0 }))
}

/// NUL-terminated ASCII string from a fixed 100-byte field.
fn cstr(b: &[u8], off: usize) -> String {
    let end = (off + 100).min(b.len());
    let s = &b[off.min(end)..end];
    let n = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..n]).trim().to_string()
}

pub struct PiBoSoDecoder;

impl GameDecoder for PiBoSoDecoder {
    fn name(&self) -> &'static str {
        "PiBoSo (MX Bikes / KRP)"
    }

    fn decode(&self, b: &[u8]) -> Option<Decoded> {
        if b.len() < HEADER || b[4] != 0 {
            return None;
        }
        let tag = &b[0..4];
        let mut a = state().lock().unwrap();
        match tag {
            b"data" if b.len() >= HEADER + 36 => {
                a.t.rpm = le::i32(b, 13).max(0);
                a.t.water_c = le::f32(b, 21).round() as i32;
                a.t.gear = le::gear_byte(le::i32(b, 25)); // 0 = N, 1.. up
                a.t.fuel_dl = (le::f32(b, 29) * 10.0).round().max(0.0) as i32;
                a.t.speed_kmh = (le::f32(b, 33) * 3.6).round().max(0.0) as i32;
                a.t.ignition = 1;
                // Inputs live in the game-specific tail — keyed by packet size.
                match b.len() {
                    201 => {
                        // MX Bikes / GP Bikes-style: steer@153 throttle@157
                        // frontBrake@161 rearBrake@165 clutch@169 (0..1)
                        a.t.throttle = (le::f32(b, 157) * 100.0).round().clamp(0.0, 100.0) as i32;
                        a.t.brake = (le::f32(b, 161).max(le::f32(b, 165)) * 100.0).round().clamp(0.0, 100.0) as i32;
                        a.t.clutch = (le::f32(b, 169) * 100.0).round().clamp(0.0, 100.0) as i32;
                    }
                    189 => {
                        // Kart Racing Pro: steer@133 throttle@137 brake@141
                        // frontBrakes@145 clutch@149 (0..1)
                        a.t.throttle = (le::f32(b, 137) * 100.0).round().clamp(0.0, 100.0) as i32;
                        a.t.brake = (le::f32(b, 141) * 100.0).round().clamp(0.0, 100.0) as i32;
                        a.t.clutch = (le::f32(b, 149) * 100.0).round().clamp(0.0, 100.0) as i32;
                    }
                    _ => {} // unknown family member — head fields only
                }
            }
            b"evnt" => {
                // Name block is common: [rider 100][id 100][name 100] from 13.
                let name = cstr(b, 13 + 200);
                if !name.is_empty() {
                    a.car = Some(name);
                }
                // Gearing block placement differs per game — key on packet size.
                let (max_off, shift_off) = match b.len() {
                    665 => (13 + 304, 13 + 312), // MXB: gears@300 max@304 limiter@308 shift@312
                    761 => (13 + 308, 13 + 316), // KRP: driveType@300 gears@304 max@308 limiter@312 shift@316
                    _ => return None,            // unknown event layout — skip, keep stream alive
                };
                let max = le::i32(b, max_off);
                let shift = le::i32(b, shift_off);
                if (1000..40000).contains(&max) {
                    a.t.max_rpm = max;
                    a.t.shift_rpm = if (1000..=max).contains(&shift) { shift } else { max };
                }
            }
            b"lap " if b.len() >= HEADER + 16 => {
                let lap_num = le::i32(b, 13);
                let lap_ms = le::i32(b, 21);
                let best = le::i32(b, 25);
                if lap_ms > 0 {
                    a.t.last_lap_ms = lap_ms;
                    if best == 1 || (a.t.best_lap_ms == 0 || lap_ms < a.t.best_lap_ms) {
                        a.t.best_lap_ms = lap_ms;
                    }
                }
                a.t.laps_done = lap_num.max(0);
                a.split0_ms = 0;
            }
            b"splt" if b.len() >= HEADER + 12 => {
                // Cumulative time at split line `idx` → sector times.
                let idx = le::i32(b, 13);
                let ms = le::i32(b, 17);
                if ms > 0 {
                    if idx == 0 {
                        a.t.s1_ms = ms;
                        a.split0_ms = ms;
                    } else if idx == 1 && a.split0_ms > 0 {
                        a.t.s2_ms = ms - a.split0_ms;
                    }
                }
            }
            b"sesn" => {} // session info — nothing the dash shows yet
            _ => return None,
        }
        Some(Decoded { telem: a.t, car: a.car.clone() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(tag: &[u8; 4], len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[0..4].copy_from_slice(tag);
        b[4] = 0;
        b
    }

    #[test]
    fn decodes_mxb_data_head_and_inputs() {
        let mut b = pkt(b"data", 201);
        b[13..17].copy_from_slice(&9500i32.to_le_bytes()); // rpm
        b[21..25].copy_from_slice(&78.0f32.to_le_bytes()); // water
        b[25..29].copy_from_slice(&3i32.to_le_bytes()); // gear
        b[29..33].copy_from_slice(&6.5f32.to_le_bytes()); // fuel litres
        b[33..37].copy_from_slice(&25.0f32.to_le_bytes()); // 25 m/s = 90 km/h
        b[157..161].copy_from_slice(&0.8f32.to_le_bytes()); // throttle
        let d = PiBoSoDecoder.decode(&b).unwrap();
        assert_eq!(d.telem.rpm, 9500);
        assert_eq!(d.telem.water_c, 78);
        assert_eq!(d.telem.gear, b'3');
        assert_eq!(d.telem.fuel_dl, 65);
        assert_eq!(d.telem.speed_kmh, 90);
        assert_eq!(d.telem.throttle, 80);
    }

    #[test]
    fn event_sets_max_and_shift_rpm_per_variant() {
        // MXB event (665): max@317, shift@325 (13+304 / 13+312).
        let mut e = pkt(b"evnt", 665);
        e[13 + 200..13 + 209].copy_from_slice(b"KTM 450\0\0");
        e[13 + 304..13 + 308].copy_from_slice(&11800i32.to_le_bytes());
        e[13 + 312..13 + 316].copy_from_slice(&11000i32.to_le_bytes());
        let d = PiBoSoDecoder.decode(&e).unwrap();
        assert_eq!(d.telem.max_rpm, 11800);
        assert_eq!(d.telem.shift_rpm, 11000);
        assert_eq!(d.car.as_deref(), Some("KTM 450"));

        // KRP event (761): max@321, shift@329 (13+308 / 13+316).
        let mut e = pkt(b"evnt", 761);
        e[13 + 308..13 + 312].copy_from_slice(&16000i32.to_le_bytes());
        e[13 + 316..13 + 320].copy_from_slice(&15000i32.to_le_bytes());
        let d = PiBoSoDecoder.decode(&e).unwrap();
        assert_eq!(d.telem.max_rpm, 16000);
        assert_eq!(d.telem.shift_rpm, 15000);
    }

    #[test]
    fn lap_and_splits() {
        let mut s0 = pkt(b"splt", 25);
        s0[13..17].copy_from_slice(&0i32.to_le_bytes());
        s0[17..21].copy_from_slice(&30_500i32.to_le_bytes());
        PiBoSoDecoder.decode(&s0).unwrap();
        let mut s1 = pkt(b"splt", 25);
        s1[13..17].copy_from_slice(&1i32.to_le_bytes());
        s1[17..21].copy_from_slice(&62_100i32.to_le_bytes());
        let d = PiBoSoDecoder.decode(&s1).unwrap();
        assert_eq!(d.telem.s1_ms, 30_500);
        assert_eq!(d.telem.s2_ms, 31_600);

        let mut l = pkt(b"lap ", 29);
        l[13..17].copy_from_slice(&4i32.to_le_bytes());
        l[21..25].copy_from_slice(&95_000i32.to_le_bytes());
        l[25..29].copy_from_slice(&1i32.to_le_bytes());
        let d = PiBoSoDecoder.decode(&l).unwrap();
        assert_eq!(d.telem.last_lap_ms, 95_000);
        assert_eq!(d.telem.best_lap_ms, 95_000);
        assert_eq!(d.telem.laps_done, 4);
    }

    #[test]
    fn rejects_non_piboso() {
        assert!(PiBoSoDecoder.decode(&[0u8; 92]).is_none()); // OutGauge-sized
        let mut b = vec![0u8; 201];
        b[0..4].copy_from_slice(b"noPe");
        assert!(PiBoSoDecoder.decode(&b).is_none());
        // right tag, but no NUL terminator at byte 4 → not PiBoSo framing
        let mut b = vec![0u8; 201];
        b[0..5].copy_from_slice(b"dataX");
        assert!(PiBoSoDecoder.decode(&b).is_none());
    }
}
