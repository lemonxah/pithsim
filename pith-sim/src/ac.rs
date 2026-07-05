//! Assetto Corsa (original, 2014) UDP remote telemetry — port 9996 handshake.
//!
//! Active client: send a 12-byte handshake (3×i32: id, version, operationId),
//! the game replies with a 408-byte HandshakerResponse; then send op=1
//! (SUBSCRIBE_UPDATE) and it streams 328-byte `RTCarInfo` packets. Unlike ACC's
//! broadcasting feed, original AC **does** carry engine RPM, so shift-lights work
//! (using the device's configured redline, since the packet has no max-RPM).
//! ACC does not implement this protocol — original AC only.

use super::le;
use pith_core::simhub::Telemetry;

// operationId values.
pub const OP_HANDSHAKE: i32 = 0;
pub const OP_SUBSCRIBE_UPDATE: i32 = 1;
pub const OP_DISMISS: i32 = 3;

const RTCARINFO_SIZE: usize = 328;
const HANDSHAKE_RESPONSE_SIZE: usize = 408;

/// Build a handshake/subscribe/dismiss datagram (`[id i32][version i32][op i32]`).
pub fn encode_op(op: i32) -> Vec<u8> {
    let mut b = Vec::with_capacity(12);
    b.extend_from_slice(&1i32.to_le_bytes()); // identifier
    b.extend_from_slice(&1i32.to_le_bytes()); // version
    b.extend_from_slice(&op.to_le_bytes());
    b
}

/// Is this datagram the 408-byte handshake response (→ time to subscribe)?
pub fn is_handshake_response(b: &[u8]) -> bool {
    b.len() == HANDSHAKE_RESPONSE_SIZE
}

/// Parse (carName, trackName) from the handshake response. Both are UTF-16LE
/// `wchar[50]` (100 bytes): carName@0, trackName@208.
pub fn parse_handshake(b: &[u8]) -> Option<(String, String)> {
    if b.len() < HANDSHAKE_RESPONSE_SIZE {
        return None;
    }
    Some((utf16_str(&b[0..100]), utf16_str(&b[208..308])))
}

/// Decode a NUL-terminated UTF-16LE string from a byte slice.
fn utf16_str(b: &[u8]) -> String {
    let units: Vec<u16> = b
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Parse a 328-byte `RTCarInfo` into a `Telemetry`. Returns None on a wrong size.
pub fn parse_rtcarinfo(b: &[u8]) -> Option<Telemetry> {
    if b.len() != RTCARINFO_SIZE {
        return None;
    }
    let mut t = Telemetry::idle();
    t.speed_kmh = le::f32(b, 8).round().max(0.0) as i32;
    t.cur_lap_ms = le::i32(b, 40);
    t.last_lap_ms = le::i32(b, 44);
    t.best_lap_ms = le::i32(b, 48);
    t.laps_done = le::i32(b, 52);
    t.throttle = (le::f32(b, 56) * 100.0).round() as i32; // gas 0..1
    t.brake = (le::f32(b, 60) * 100.0).round() as i32;
    t.clutch = (le::f32(b, 64) * 100.0).round() as i32;
    t.rpm = le::f32(b, 68).round().max(0.0) as i32;
    // gear: 0 = reverse, 1 = neutral, 2 = 1st … → numeric gear = raw - 1.
    t.gear = le::gear_byte(le::i32(b, 76) - 1);
    // Aid-engagement flags (1 byte each): isAbsInAction@21, isTcInAction@22.
    t.abs_active = (le::u8(b, 21) != 0) as i32;
    t.tc_active = (le::u8(b, 22) != 0) as i32;
    t.ignition = 1; // engine running while telemetry streams
                    // Chassis G-forces (g units) — RTCarInfo accG @28 vertical / @32 horizontal
                    // (lateral) / @36 frontal (longitudinal). Offsets verified against the
                    // public RTCarInfo layout (e.g. rickwest/ac-remote-telemetry-client's
                    // RTCarInfoParser), which also anchors speed@8, gas@56, gear@76 below.
                    // g_lat from accG_horizontal, g_long from accG_frontal (+accel / −brake).
    t.g_lat_x100 = (le::f32(b, 32) * 100.0).round() as i32; // accG_horizontal
    t.g_long_x100 = (le::f32(b, 36) * 100.0).round() as i32; // accG_frontal
                                                             // Per-wheel slip DOES exist here: after cgHeight@80 the packet runs
                                                             // wheelAngularSpeed[4]@84, slipAngle[4]@100, slipAngle_ContactPatch[4]
                                                             // @116, slipRatio[4]@132, tyreSlip[4]@148, ndSlip[4]@164 (same public
                                                             // layout as above). wheel_slip = max |slipRatio| ×100, order FL,FR,RL,RR.
    t.wheel_slip = crate::ffb::slip_from_ratios([
        le::f32(b, 132),
        le::f32(b, 136),
        le::f32(b, 140),
        le::f32(b, 144),
    ]);
    // suspensionHeight[4] @292 is a POSITION — RTCarInfo has no per-wheel
    // suspension-velocity channel, so susp_impact stays 0 (no offsets guessed).
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rtcarinfo() -> Vec<u8> {
        let mut b = vec![0u8; RTCARINFO_SIZE];
        b[8..12].copy_from_slice(&210.0f32.to_le_bytes()); // speed kmh
        b[68..72].copy_from_slice(&7300.0f32.to_le_bytes()); // rpm
        b[76..80].copy_from_slice(&4i32.to_le_bytes()); // gear raw 4 → 3rd
        b[56..60].copy_from_slice(&1.0f32.to_le_bytes()); // gas
        b
    }

    #[test]
    fn parses_core() {
        let t = parse_rtcarinfo(&rtcarinfo()).unwrap();
        assert_eq!(t.speed_kmh, 210);
        assert_eq!(t.rpm, 7300);
        assert_eq!(t.gear, b'3'); // raw 4 → gear 3
        assert_eq!(t.throttle, 100);
    }

    #[test]
    fn gear_reverse_neutral() {
        let mut b = rtcarinfo();
        b[76..80].copy_from_slice(&0i32.to_le_bytes());
        assert_eq!(parse_rtcarinfo(&b).unwrap().gear, b'R');
        b[76..80].copy_from_slice(&1i32.to_le_bytes());
        assert_eq!(parse_rtcarinfo(&b).unwrap().gear, b'N');
    }

    /// wheel_slip comes from slipRatio[4]@132 (max |ratio| ×100); accG@32/36
    /// feed the g channels; susp_impact has no source in RTCarInfo.
    #[test]
    fn slip_and_g_forces() {
        let mut b = rtcarinfo();
        b[32..36].copy_from_slice(&1.2f32.to_le_bytes()); // accG_horizontal
        b[36..40].copy_from_slice(&(-0.8f32).to_le_bytes()); // accG_frontal (braking)
        b[136..140].copy_from_slice(&(-0.35f32).to_le_bytes()); // slipRatio FR
        b[140..144].copy_from_slice(&0.15f32.to_le_bytes()); // slipRatio RL
        let t = parse_rtcarinfo(&b).unwrap();
        assert_eq!(t.g_lat_x100, 120);
        assert_eq!(t.g_long_x100, -80);
        assert_eq!(t.wheel_slip, 35); // max |slipRatio|
        assert_eq!(t.susp_impact, 0);
    }

    #[test]
    fn rejects_wrong_size() {
        assert!(parse_rtcarinfo(&[0u8; 100]).is_none());
        assert!(is_handshake_response(&[0u8; 408]));
    }
}
