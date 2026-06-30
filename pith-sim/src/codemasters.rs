//! Codemasters "extradata" UDP telemetry decoder — DiRT Rally, DiRT Rally 2.0,
//! DiRT 4, the GRID series, and EA SPORTS WRC (which reuses the format).
//!
//! The packet is a flat little-endian `f32` array (no header/magic), so
//! `byte_offset = index * 4`. We require the full **extradata=3** layout, whose
//! wire size is 264 bytes — distinct from every other decoder's packet length,
//! which is how we avoid mis-claiming another game's datagram. RPM fields are
//! reported as rpm/10, so we scale ×10.

use super::decoders::{Decoded, GameDecoder};
use super::le;
use pith_core::simhub::Telemetry;

/// Read float at array index `i` (byte offset i*4).
#[inline]
fn fi(b: &[u8], i: usize) -> f32 {
    le::f32(b, i * 4)
}

pub struct CodemastersDecoder;

impl GameDecoder for CodemastersDecoder {
    fn name(&self) -> &'static str {
        "DiRT / GRID / WRC (Codemasters)"
    }

    fn decode(&self, b: &[u8]) -> Option<Decoded> {
        // extradata=3 emits a 264-byte datagram (DiRT Rally 2.0 / EA WRC). This
        // length is unique among our decoders.
        if b.len() != 264 {
            return None;
        }
        // Sanity-gate the float content so a same-sized blob from elsewhere can't
        // masquerade: speed and rpm must be finite and in a plausible range.
        let speed_ms = fi(b, 7);
        let rpm10 = fi(b, 37);
        if !speed_ms.is_finite() || !rpm10.is_finite() || speed_ms < 0.0 || speed_ms > 200.0 {
            return None;
        }

        let mut t = Telemetry::idle();
        t.speed_kmh = (speed_ms * 3.6).round() as i32;
        t.rpm = (rpm10 * 10.0).round().max(0.0) as i32; // reported as rpm/10
        t.max_rpm = (fi(b, 63) * 10.0).round().max(0.0) as i32;
        t.shift_rpm = t.max_rpm;
        t.throttle = (fi(b, 29) * 100.0).round() as i32;
        t.steer = (fi(b, 30) * 100.0).round().clamp(-100.0, 100.0) as i32; // -1..1
        t.brake = (fi(b, 31) * 100.0).round() as i32;
        t.clutch = (fi(b, 32) * 100.0).round() as i32;
        // Gear: official 0=N, 1..n forward, 10=reverse; some titles use <0 for
        // reverse. Be defensive about both.
        let gf = fi(b, 33);
        let gear = if gf < 0.0 || gf >= 9.5 {
            -1
        } else if gf < 0.5 {
            0
        } else {
            gf.round() as i32
        };
        t.gear = le::gear_byte(gear);
        t.cur_lap_ms = (fi(b, 1) * 1000.0).round().max(0.0) as i32;
        t.last_lap_ms = (fi(b, 62) * 1000.0).round().max(0.0) as i32;
        t.laps_done = fi(b, 36).round() as i32;
        t.position = fi(b, 39).round().max(0.0) as i32;
        // Lap progress for the track map: lap distance / track length.
        let track_len = fi(b, 61);
        if track_len > 1.0 {
            t.track_pct = ((fi(b, 2) / track_len) * 1000.0).clamp(0.0, 1000.0) as i32;
        }
        // World position (metres) for the self-learned map.
        t.pos_x = fi(b, 4).round() as i32;
        t.pos_z = fi(b, 6).round() as i32;

        Some(Decoded {
            telem: t,
            car: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt() -> Vec<u8> {
        let mut b = vec![0u8; 264];
        let put = |b: &mut [u8], i: usize, v: f32| b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        put(&mut b, 7, 50.0); // speed m/s
        put(&mut b, 37, 750.0); // rpm/10 → 7500
        put(&mut b, 63, 800.0); // max rpm/10 → 8000
        put(&mut b, 33, 4.0); // gear 4
        put(&mut b, 29, 1.0); // throttle
        b
    }

    #[test]
    fn decodes_core() {
        let dec = CodemastersDecoder.decode(&pkt()).unwrap();
        assert_eq!(dec.telem.speed_kmh, 180); // 50*3.6
        assert_eq!(dec.telem.rpm, 7500);
        assert_eq!(dec.telem.max_rpm, 8000);
        assert_eq!(dec.telem.gear, b'4');
        assert_eq!(dec.telem.throttle, 100);
    }

    #[test]
    fn rejects_wrong_size() {
        assert!(CodemastersDecoder.decode(&[0u8; 232]).is_none());
    }
}
