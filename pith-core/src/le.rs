//! Little-endian byte readers shared by the UDP game decoders and the
//! shared-memory parsers. Callers length-check before reading.

#![allow(dead_code)] // a few readers are used only by some decoders/parsers

#[inline]
pub fn u8(b: &[u8], o: usize) -> u8 {
    b[o]
}
#[inline]
pub fn i8(b: &[u8], o: usize) -> i8 {
    b[o] as i8
}
#[inline]
pub fn u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
pub fn i16(b: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([b[o], b[o + 1]])
}
#[inline]
pub fn u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
pub fn i32(b: &[u8], o: usize) -> i32 {
    i32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
pub fn f32(b: &[u8], o: usize) -> f32 {
    f32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
#[inline]
pub fn f64(b: &[u8], o: usize) -> f64 {
    f64::from_le_bytes([
        b[o],
        b[o + 1],
        b[o + 2],
        b[o + 3],
        b[o + 4],
        b[o + 5],
        b[o + 6],
        b[o + 7],
    ])
}

/// Map a numeric gear (-1 = reverse, 0 = neutral, 1..n forward) to the `$`-frame
/// gear byte (`R`/`N`/`1`..`9`), clamping forward gears to a single digit.
pub fn gear_byte(g: i32) -> u8 {
    match g {
        x if x < 0 => b'R',
        0 => b'N',
        1..=9 => b'0' + g as u8,
        _ => b'9',
    }
}
