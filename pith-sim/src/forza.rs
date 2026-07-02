//! Forza "Data Out" UDP telemetry decoder — the first direct game decoder.
//!
//! Forza titles (Forza Motorsport, Forza Horizon 4/5/6) emit a fixed,
//! little-endian, positional binary struct via their "Data Out" setting,
//! fire-and-forget to a user-configured IP:port. There is no header/magic — the
//! **datagram length** identifies the format:
//!
//! | len      | format                              | dash block | dash shift |
//! |----------|-------------------------------------|-----------|-----------|
//! | 232      | Sled (v1)                           | no        | —         |
//! | 311      | FM7 "Car Dash"                      | yes       | +0        |
//! | 331      | Forza Motorsport 2023 "Data Out V2" | yes       | +0        |
//! | 323/324  | Forza Horizon 4 / 5 / 6               | yes       | +12       |
//!
//! There is deliberately no open-ended `len >= 323` catch-all: an unbounded
//! length-only match here would swallow every OTHER decoder's larger packets
//! before they get a chance to run (PCARS2/AMS2 is 559 bytes, F1 23-25's
//! multi-car packets are 1200+ bytes — both comfortably clear 323 and would
//! get misidentified as Forza with garbage field values). A confirmed future
//! Horizon title gets its own explicit length arm, same as every entry above.
//!
//! The Horizon family inserts a 12-byte block (CarGroup / SmashableVelDiff /
//! SmashableMass) between the Sled and the dash section, so every dash field
//! shifts by +12 vs the Forza Motorsport layout. The Sled block (offsets 0..231,
//! including CarOrdinal at 212) is identical across every title.
//!
//! Only the channels the Pith dash actually uses are pulled out; the rest of the
//! packet (G-forces, suspension, slip, etc.) is ignored.

use super::decoders::{Decoded, GameDecoder};
use pith_core::simhub::Telemetry;

#[inline]
fn f32le(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
fn s32le(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub struct ForzaDecoder;

impl GameDecoder for ForzaDecoder {
    fn name(&self) -> &'static str {
        "Forza Horizon 6"
    }

    fn decode(&self, b: &[u8]) -> Option<Decoded> {
        // Classify by length → (has dash section, dash field shift).
        let (has_dash, d) = match b.len() {
            232 => (false, 0usize),
            311 => (true, 0),
            331 => (true, 0),
            323 | 324 => (true, 12), // FH4/5/6 — 324 confirmed for FH6's "Data Out"
            _ => return None,
        };
        // Need the full Sled at minimum.
        if b.len() < 232 {
            return None;
        }
        // IsRaceOn == 0 → paused / in a menu: emit nothing so the device idles.
        if s32le(b, 0) == 0 {
            return None;
        }

        let mut t = Telemetry::idle();

        // ---- Sled (absolute offsets, all titles) ----
        t.max_rpm = f32le(b, 8).round().max(0.0) as i32;
        t.rpm = f32le(b, 16).round().max(0.0) as i32;
        // Forza exposes no shift-point channel; drive the strip toward redline.
        t.shift_rpm = t.max_rpm;
        // We only get here while IsRaceOn (engine running) → ignition on.
        t.ignition = 1;

        // CarOrdinal (Sled tail) → a stable, if numeric, car identity.
        let mut car = None;
        let ordinal = s32le(b, 212);
        if ordinal > 0 {
            car = Some(format!("Forza #{ordinal}"));
        }

        if has_dash {
            // Guard: the furthest field we read is Steer (s8) at 308+d.
            if b.len() < 309 + d {
                return None;
            }
            // Speed is m/s → km/h.
            t.speed_kmh = (f32le(b, 244 + d) * 3.6).round().max(0.0) as i32;
            // World position (metres) for the self-learned track map.
            t.pos_x = f32le(b, 232 + d).round() as i32;
            t.pos_z = f32le(b, 240 + d).round() as i32;
            // Tyre surface temps: one value per corner → fill all three zones so
            // whichever zone a widget binds to shows the reading. Forza reports
            // these in Fahrenheit (the protocol's native temp unit) → convert to
            // °C, then ×10 for the deci-degree integer the pipeline expects.
            let f2c10 = |f: f32| (((f - 32.0) * 5.0 / 9.0) * 10.0).round() as i32;
            let ttfl = f2c10(f32le(b, 256 + d));
            let ttfr = f2c10(f32le(b, 260 + d));
            let ttrl = f2c10(f32le(b, 264 + d));
            let ttrr = f2c10(f32le(b, 268 + d));
            t.tt_fl_i = ttfl;
            t.tt_fl_m = ttfl;
            t.tt_fl_o = ttfl;
            t.tt_fr_i = ttfr;
            t.tt_fr_m = ttfr;
            t.tt_fr_o = ttfr;
            t.tt_rl_i = ttrl;
            t.tt_rl_m = ttrl;
            t.tt_rl_o = ttrl;
            t.tt_rr_i = ttrr;
            t.tt_rr_m = ttrr;
            t.tt_rr_o = ttrr;
            // Lap times: Forza reports seconds → ms.
            t.best_lap_ms = (f32le(b, 284 + d) * 1000.0).round().max(0.0) as i32;
            t.last_lap_ms = (f32le(b, 288 + d) * 1000.0).round().max(0.0) as i32;
            t.cur_lap_ms = (f32le(b, 292 + d) * 1000.0).round().max(0.0) as i32;
            t.laps_done = u16le(b, 300 + d) as i32;
            t.position = b[302 + d] as i32;
            // Inputs are 0..255 → 0..100.
            t.throttle = b[303 + d] as i32 * 100 / 255;
            t.brake = b[304 + d] as i32 * 100 / 255;
            t.clutch = b[305 + d] as i32 * 100 / 255;
            // Gear: 0 = reverse, 1..n forward (Forza has no explicit neutral).
            let g = b[307 + d];
            t.gear = match g {
                0 => b'R',
                1..=9 => b'0' + g,
                _ => b'9', // clamp ≥10 to the single-digit frame field
            };
            t.steer = (b[308 + d] as i8) as i32 * 100 / 127; // -100..100
            t.boost_kpa = (f32le(b, 272 + d) * 6.895).round() as i32; // psi → kPa
            // Fuel is a 0..1 tank fraction with no capacity channel → can't map to
            // the device's decilitre field, so it's left unset for now.
        }

        Some(Decoded { telem: t, car })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Build a Horizon (324-byte, +12) packet with a few known fields set.
    fn horizon_packet() -> Vec<u8> {
        let mut b = vec![0u8; 324];
        let put_f32 = |b: &mut [u8], o: usize, v: f32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let put_s32 = |b: &mut [u8], o: usize, v: i32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let put_u16 = |b: &mut [u8], o: usize, v: u16| b[o..o + 2].copy_from_slice(&v.to_le_bytes());
        put_s32(&mut b, 0, 1); // IsRaceOn
        put_f32(&mut b, 8, 7500.0); // EngineMaxRpm
        put_f32(&mut b, 16, 6800.0); // CurrentEngineRpm
        put_s32(&mut b, 212, 4242); // CarOrdinal
        let d = 12;
        put_f32(&mut b, 244 + d, 55.0); // Speed m/s (≈198 km/h)
        put_f32(&mut b, 292 + d, 84.012); // CurrentLap seconds
        put_u16(&mut b, 300 + d, 7); // LapNumber
        b[302 + d] = 3; // RacePosition
        b[303 + d] = 255; // Accel
        b[307 + d] = 4; // Gear
        b
    }

    #[test]
    fn decodes_horizon_core_fields() {
        let dec = ForzaDecoder.decode(&horizon_packet()).expect("decodes");
        let t = dec.telem;
        assert_eq!(t.max_rpm, 7500);
        assert_eq!(t.rpm, 6800);
        assert_eq!(t.shift_rpm, 7500);
        assert_eq!(t.speed_kmh, 198); // 55 * 3.6 = 198
        assert_eq!(t.gear, b'4');
        assert_eq!(t.position, 3);
        assert_eq!(t.laps_done, 7);
        assert_eq!(t.cur_lap_ms, 84012);
        assert_eq!(t.throttle, 100);
        assert_eq!(dec.car.as_deref(), Some("Forza #4242"));
    }

    #[test]
    fn rejects_when_race_off() {
        let mut b = horizon_packet();
        b[0..4].copy_from_slice(&0i32.to_le_bytes()); // IsRaceOn = 0
        assert!(ForzaDecoder.decode(&b).is_none());
    }

    #[test]
    fn rejects_unknown_length() {
        assert!(ForzaDecoder.decode(&[0u8; 64]).is_none());
    }

    /// Regression: an open-ended `len >= 323` arm used to swallow every larger
    /// game's packets (F1's multi-car packets, PCARS2/AMS2's 559-byte packet)
    /// before their own decoders ran, misreporting them as Forza Horizon 6.
    #[test]
    fn rejects_oversized_non_forza_packets() {
        assert!(ForzaDecoder.decode(&[0u8; 559]).is_none()); // PCARS2/AMS2
        assert!(ForzaDecoder.decode(&[0u8; 1349]).is_none()); // F1 CarTelemetry (22 cars)
    }

    #[test]
    fn gear_zero_is_reverse() {
        let mut b = horizon_packet();
        b[307 + 12] = 0;
        assert_eq!(ForzaDecoder.decode(&b).unwrap().telem.gear, b'R');
    }
}
