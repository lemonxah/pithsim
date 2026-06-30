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

    #[test]
    fn rejects_wrong_size() {
        assert!(parse_rtcarinfo(&[0u8; 100]).is_none());
        assert!(is_handshake_response(&[0u8; 408]));
    }
}
