//! Direct game-telemetry decoder framework.
//!
//! A decoder turns one raw UDP datagram from a game's *native* telemetry output
//! into our common [`Telemetry`]. Decoders are content/length addressed:
//! [`GameDecoder::decode`] returns `None` when the datagram isn't its format, so
//! the listener can try each registered decoder in turn until one claims it.
//!
//! This is the binary path. The SimHub plugin's text `$`/`@` frames are *not*
//! decoders — they're the canonical wire format and are handled directly by the
//! listener. Adding a new game means writing one `GameDecoder` and listing it in
//! [`REGISTRY`]; nothing else changes.

use pith_core::simhub::Telemetry;

/// Result of decoding one datagram.
pub struct Decoded {
    /// The telemetry snapshot to forward to the device + preview.
    pub telem: Telemetry,
    /// Best-effort car identity, when the format exposes one (e.g. Forza's
    /// numeric ordinal). `None` when unknown. Surfaced on the Telemetry-UDP page.
    pub car: Option<String>,
}

/// A decoder for one game's native UDP telemetry format.
pub trait GameDecoder: Sync {
    /// Human-facing source label shown on the Telemetry-UDP page.
    fn name(&self) -> &'static str;
    /// Try to decode one datagram. `None` = "not my packet" (wrong length/magic),
    /// so the listener should try the next decoder.
    fn decode(&self, buf: &[u8]) -> Option<Decoded>;
}

/// Every direct (passive, listen-only) decoder the listener tries, in order.
/// Add new fire-and-forget UDP games here. Order matters only for formats that
/// could share a datagram length — each decoder also content-gates, so the first
/// that positively claims a packet wins.
pub const REGISTRY: &[&dyn GameDecoder] = &[
    &super::forza::ForzaDecoder,
    &super::f1::F1Decoder,
    &super::pcars::PCarsDecoder,
    &super::codemasters::CodemastersDecoder,
    &super::outgauge::OutGaugeDecoder,
];

/// Run a datagram through every registered decoder; return the first match with
/// its source label.
pub fn try_decode(buf: &[u8]) -> Option<(&'static str, Decoded)> {
    for d in REGISTRY {
        if let Some(dec) = d.decode(buf) {
            return Some((d.name(), dec));
        }
    }
    None
}

/// Labels of every supported direct game (for the UI's "supported games" list).
pub fn supported_games() -> Vec<&'static str> {
    REGISTRY.iter().map(|d| d.name()).collect()
}
