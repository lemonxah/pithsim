//! OutGauge UDP telemetry decoder — Live for Speed's `OutGaugePack`, also emitted
//! natively by BeamNG.drive and various OutGauge-compatible tools.
//!
//! Fixed `#pragma pack` struct, little-endian, 92 bytes (or 96 with the optional
//! trailing OutGauge ID). Provides gear/speed/rpm/fuel/pedals but NO redline, so
//! the shift strip uses the device's configured redline for these titles.

use super::decoders::{Decoded, GameDecoder};
use super::le;
use pith_core::simhub::Telemetry;

pub struct OutGaugeDecoder;

impl GameDecoder for OutGaugeDecoder {
    fn name(&self) -> &'static str {
        "OutGauge (LFS / BeamNG)"
    }

    fn decode(&self, b: &[u8]) -> Option<Decoded> {
        // 92 = no ID field, 96 = trailing int32 ID present.
        if b.len() != 92 && b.len() != 96 {
            return None;
        }
        let speed = le::f32(b, 12);
        let rpm = le::f32(b, 16);
        let gear_raw = le::u8(b, 10);
        // Sanity-gate so a same-sized blob elsewhere can't masquerade.
        if !speed.is_finite() || !rpm.is_finite() || speed < 0.0 || rpm < 0.0 || gear_raw > 10 {
            return None;
        }

        let mut t = Telemetry::idle();
        t.speed_kmh = (speed * 3.6).round() as i32; // m/s → km/h
        t.rpm = rpm.round() as i32; // direct engine rpm (no redline in OutGauge)
        // OutGauge gear: 0 = reverse, 1 = neutral, 2 = 1st … → numeric gear = raw-1.
        t.gear = le::gear_byte(gear_raw as i32 - 1);
        t.throttle = (le::f32(b, 48) * 100.0).round() as i32;
        t.brake = (le::f32(b, 52) * 100.0).round() as i32;
        t.clutch = (le::f32(b, 56) * 100.0).round() as i32;
        t.ignition = 1; // engine running while OutGauge streams
        // Extra channels: turbo (bar→kPa), engine + oil temps.
        t.boost_kpa = (le::f32(b, 20) * 100.0).round() as i32; // turbo bar → kPa
        t.water_c = le::f32(b, 24).round() as i32; // EngTemp
        t.oil_c = le::f32(b, 36).round() as i32; // OilTemp
        // ShowLights (DL_* bits) @44: FULLBEAM=2, PITSPEED=8, TC=16, ABS=1024.
        let dl = le::u32(b, 44);
        t.headlights = (dl & 0x0002 != 0) as i32;
        t.pit_limiter = (dl & 0x0008 != 0) as i32;
        t.tc_active = (dl & 0x0010 != 0) as i32;
        t.abs_active = (dl & 0x0400 != 0) as i32;

        Some(Decoded {
            telem: t,
            car: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkt(len: usize) -> Vec<u8> {
        let mut b = vec![0u8; len];
        b[10] = 4; // gear raw 4 → 3rd gear
        b[12..16].copy_from_slice(&30.0f32.to_le_bytes()); // speed m/s
        b[16..20].copy_from_slice(&5400.0f32.to_le_bytes()); // rpm
        b[48..52].copy_from_slice(&1.0f32.to_le_bytes()); // throttle
        b
    }

    #[test]
    fn decodes_92_and_96() {
        for len in [92usize, 96] {
            let dec = OutGaugeDecoder.decode(&pkt(len)).unwrap();
            assert_eq!(dec.telem.speed_kmh, 108); // 30*3.6
            assert_eq!(dec.telem.rpm, 5400);
            assert_eq!(dec.telem.gear, b'3'); // raw 4 → gear 3
            assert_eq!(dec.telem.throttle, 100);
        }
    }

    #[test]
    fn reverse_and_neutral() {
        let mut b = pkt(92);
        b[10] = 0; // reverse
        assert_eq!(OutGaugeDecoder.decode(&b).unwrap().telem.gear, b'R');
        b[10] = 1; // neutral
        assert_eq!(OutGaugeDecoder.decode(&b).unwrap().telem.gear, b'N');
    }

    #[test]
    fn rejects_wrong_size() {
        assert!(OutGaugeDecoder.decode(&[0u8; 64]).is_none());
    }
}
