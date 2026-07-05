//! Gran Turismo 7 / GT Sport UDP telemetry — Salsa20-encrypted.
//!
//! Active client: send a heartbeat byte `'A'` to the console on UDP :33739 and
//! receive encrypted 296-byte datagrams on local :33740. Each datagram is
//! Salsa20-encrypted with a fixed key; the nonce is derived from a u32 seed at
//! offset 0x40. After decryption the packet starts with magic `0x47375330`.
//! GT7 exposes engine RPM + a rev-limiter alert RPM, so shift-lights work.

use salsa20::cipher::{KeyIvInit, StreamCipher};
use salsa20::Salsa20;

use super::le;
use pith_core::simhub::Telemetry;

pub const HEARTBEAT: &[u8] = b"A";
pub const SEND_PORT: u16 = 33739; // we send the heartbeat here
pub const RECV_PORT: u16 = 33740; // we bind here to receive

/// Car code (`CarCode` i32 @0x124, the packet's last field) from a decrypted
/// packet. GT7 sends no car NAME — this numeric id keys [`car_name`]. `None`
/// when the packet is short or the value is implausible (cars with >8 gears
/// overflow the preceding gear-ratio array into this field — PDTools caveat).
pub fn car_code(b: &[u8]) -> Option<i32> {
    if b.len() < 0x128 {
        return None;
    }
    let id = le::i32(b, 0x124);
    (0..500_000).contains(&id).then_some(id)
}

/// Car name for a GT7 car code, from the embedded community DB
/// (ddm999/gt7info `cars.csv`, `ID,ShortName,Maker`).
pub fn car_name(code: i32) -> Option<&'static str> {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static DB: OnceLock<HashMap<i32, &'static str>> = OnceLock::new();
    let db = DB.get_or_init(|| {
        let mut m = HashMap::new();
        for line in include_str!("gt7_cars.csv").lines().skip(1) {
            let mut it = line.splitn(3, ',');
            if let (Some(id), Some(name)) = (it.next().and_then(|s| s.parse().ok()), it.next()) {
                if !name.is_empty() {
                    m.insert(id, name);
                }
            }
        }
        m
    });
    db.get(&code).copied()
}

const MAGIC: u32 = 0x4737_5330;
// Salsa20 key = first 32 bytes of this 38-byte string.
const KEY_SRC: &[u8] = b"Simulator Interface Packet GT7 ver 0.0";

/// Decrypt a received datagram in place and verify the magic. Returns the
/// plaintext on success.
pub fn decrypt(packet: &[u8]) -> Option<Vec<u8>> {
    if packet.len() < 0x44 {
        return None;
    }
    let iv1 = le::u32(packet, 0x40);
    let iv2 = iv1 ^ 0xDEAD_BEAF;
    let mut nonce = [0u8; 8];
    nonce[0..4].copy_from_slice(&iv2.to_le_bytes());
    nonce[4..8].copy_from_slice(&iv1.to_le_bytes());
    let mut key = [0u8; 32];
    key.copy_from_slice(&KEY_SRC[..32]);

    let mut buf = packet.to_vec();
    let mut cipher = Salsa20::new(&key.into(), &nonce.into());
    cipher.apply_keystream(&mut buf);

    if le::u32(&buf, 0) != MAGIC {
        return None;
    }
    Some(buf)
}

/// Parse a decrypted GT7 packet into `Telemetry`.
pub fn parse(b: &[u8]) -> Option<Telemetry> {
    if b.len() < 0x94 || le::u32(b, 0) != MAGIC {
        return None;
    }
    let mut t = Telemetry::idle();
    t.rpm = le::f32(b, 0x3C).round().max(0.0) as i32;
    t.speed_kmh = (le::f32(b, 0x4C) * 3.6).round().max(0.0) as i32;
    // Rev-limiter alert max → treat as redline so the shift strip lights up.
    let max_alert = le::i16(b, 0x8A) as i32;
    if max_alert > 0 {
        t.max_rpm = max_alert;
        t.shift_rpm = max_alert;
    }
    t.laps_done = le::i16(b, 0x74) as i32;
    t.best_lap_ms = le::i32(b, 0x78).max(0);
    t.last_lap_ms = le::i32(b, 0x7C).max(0);
    // Fuel: GasLevel / GasCapacity (litres) → decilitres.
    let fuel_l = le::f32(b, 0x44);
    let cap_l = le::f32(b, 0x48);
    t.fuel_dl = (fuel_l * 10.0).round().max(0.0) as i32;
    t.fuel_cap_dl = (cap_l * 10.0).round().max(0.0) as i32;
    t.throttle = le::u8(b, 0x91) as i32 * 100 / 255;
    t.brake = le::u8(b, 0x92) as i32 * 100 / 255;
    // CAVEAT: GT7 sends static dummies here — water is always 85, oil always
    // 110 (confirmed by the PDTools reference). Kept so the widgets show
    // *something*, but they never move.
    t.water_c = le::f32(b, 0x58).round() as i32;
    t.oil_c = le::f32(b, 0x5C).round() as i32;
    t.boost_kpa = ((le::f32(b, 0x50) - 1.0) * 100.0).round() as i32; // 1.0 = 0 kPa
                                                                     // Tyre surface temps °C: FL/FR/RL/RR @ 0x60/0x64/0x68/0x6C → all three zones.
    let (fl, fr, rl, rr) = (
        (le::f32(b, 0x60) * 10.0).round() as i32,
        (le::f32(b, 0x64) * 10.0).round() as i32,
        (le::f32(b, 0x68) * 10.0).round() as i32,
        (le::f32(b, 0x6C) * 10.0).round() as i32,
    );
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
    // Flags bitfield @0x8E: CarOnTrack=1, Lights=128, TCSActive=2048.
    let flags = le::u16(b, 0x8E);
    t.ignition = (flags & 0x0001 != 0) as i32;
    t.headlights = (flags & 0x0080 != 0) as i32;
    t.tc_active = (flags & 0x0800 != 0) as i32;
    // Gear: low nibble = current. 0 = REVERSE, 15 = NEUTRAL (all bits set =
    // clutch disengaged), else the forward gear — per the GTPlanet reverse-
    // engineering + Bornhall/Nenkai reference decoders. (We shipped this
    // inverted for a while: R showed as N and vice versa.)
    let cur = (le::u8(b, 0x90) & 0x0F) as i32;
    let gear = match cur {
        0 => -1,
        15 => 0,
        g => g,
    };
    t.gear = le::gear_byte(gear);
    // Wheel slip: per-wheel linear speed = wheelAngularSpeed (@0xA4, rad/s) ×
    // tyreRadius (@0xB4, m) vs body speed (@0x4C, m/s). slip = |wheel − body| /
    // max(body, 1). Max across wheels, ×100. Needs the packet through 0xC4.
    if b.len() >= 0xC4 {
        let mut wheels = [0.0f32; 4];
        for (i, w) in wheels.iter_mut().enumerate() {
            *w = le::f32(b, 0xA4 + i * 4) * le::f32(b, 0xB4 + i * 4);
        }
        t.wheel_slip = crate::ffb::body_relative_slip(le::f32(b, 0x4C), wheels);
    }
    // GT7 exposes no chassis G-force channel (g_long_x100 / g_lat_x100 left 0)
    // and only suspension POSITION (TireSusHeight, no velocity), so susp_impact
    // stays 0.
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use salsa20::cipher::{KeyIvInit, StreamCipher};

    fn make_encrypted() -> Vec<u8> {
        // Build a plaintext packet, then Salsa20-encrypt it the same way the
        // game would (encryption == decryption for a stream cipher).
        let mut p = vec![0u8; 296];
        p[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        p[0x3C..0x40].copy_from_slice(&7400.0f32.to_le_bytes()); // rpm
        p[0x4C..0x50].copy_from_slice(&50.0f32.to_le_bytes()); // speed m/s
        p[0x8A..0x8C].copy_from_slice(&8000i16.to_le_bytes()); // max alert rpm
        p[0x90] = 0x04; // gear 4
        p[0x91] = 255; // throttle
                       // Choose an iv1, encrypt with the matching nonce, then write iv1 @0x40
                       // into the ciphertext (the game writes the seed in the clear region).
        let iv1: u32 = 0x1234_5678;
        let iv2 = iv1 ^ 0xDEAD_BEAF;
        let mut nonce = [0u8; 8];
        nonce[0..4].copy_from_slice(&iv2.to_le_bytes());
        nonce[4..8].copy_from_slice(&iv1.to_le_bytes());
        let mut key = [0u8; 32];
        key.copy_from_slice(&KEY_SRC[..32]);
        let mut ct = p.clone();
        let mut c = Salsa20::new(&key.into(), &nonce.into());
        c.apply_keystream(&mut ct);
        ct[0x40..0x44].copy_from_slice(&iv1.to_le_bytes()); // seed survives in clear
        ct
    }

    #[test]
    fn decrypt_and_parse() {
        let ct = make_encrypted();
        let pt = decrypt(&ct).expect("decrypts + magic ok");
        let t = parse(&pt).unwrap();
        assert_eq!(t.rpm, 7400);
        assert_eq!(t.speed_kmh, 180); // 50 * 3.6
        assert_eq!(t.max_rpm, 8000);
        assert_eq!(t.gear, b'4');
        assert_eq!(t.throttle, 100);
    }

    /// Regression: the gear nibble is 0 = REVERSE and 15 = NEUTRAL (we shipped
    /// this inverted for a while).
    #[test]
    fn gear_nibble_reverse_and_neutral() {
        let mut p = vec![0u8; 296];
        p[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        p[0x90] = 0x00;
        assert_eq!(parse(&p).unwrap().gear, b'R');
        p[0x90] = 0x0F;
        assert_eq!(parse(&p).unwrap().gear, b'N');
    }
}
