//! Shared active-pedal FFB channel math — the wheel-slip and suspension-impact
//! scaling that every telemetry decoder feeds into `Telemetry::{wheel_slip,
//! susp_impact}`. Centralized here so the caps and clamps live in ONE place.
//!
//! The per-decoder copies had already drifted: three decoders (Codemasters,
//! GT7, EA WRC) reintroduced a standstill wheel-slip bug that the R3E decoder
//! was specifically written to avoid — dividing by a floored body speed while
//! subtracting that same floored value, so a parked car read 100% slip and
//! drove continuous max wheel-slip vibration. Routing them all through one
//! helper makes that class of divergence impossible.

/// Suspension-velocity full-scale for the 0..=1000 impact proxy — the hub
/// velocity (m/s) of a hard bottom-out that should read full-scale. Most
/// decoders use this; F1's telemetry documents a higher 5 m/s cap and passes
/// its own value instead.
pub const SUSP_V_CAP_HARD_BOTTOM_OUT: f32 = 2.0;

/// Body-relative wheel-slip proxy: `max_wheels |wheel − body| / max(|body|, 1)`,
/// ×100, clamped to 0..=10000.
///
/// Subtracts the RAW body speed (not a floored `max(body, 1)`) so a stationary
/// car — every wheel and the body at 0 — reads 0 slip. Flooring the body in
/// *both* the subtraction and the divisor (the earlier per-decoder form) made
/// `|0 − 1| / 1 = 1.0` → a false 100% slip at a standstill. Only the divisor is
/// floored, to keep the ratio finite at very low speed.
///
/// Both the body and each wheel speed are compared by MAGNITUDE (both abs'd
/// here, in one place) — so a sign convention on either (e.g. a wheel reported
/// negative in reverse, or low-speed jitter) can't manufacture false slip. All
/// callers therefore pass their raw extracted speeds; they must not pre-abs.
///
/// `wheel_speeds` are per-wheel linear speeds in the body's units (m/s);
/// callers extract them however the packet encodes them (angular×radius, a
/// contact-patch speed, a tyre speed, …).
pub fn body_relative_slip(body_speed: f32, wheel_speeds: [f32; 4]) -> i32 {
    let body = body_speed.abs();
    let denom = body.max(1.0);
    let max_slip = wheel_speeds
        .iter()
        .fold(0.0f32, |m, &w| m.max((w.abs() - body).abs() / denom));
    (max_slip * 100.0).round().clamp(0.0, 10_000.0) as i32
}

/// Slip-ratio wheel-slip proxy: `max_wheels |ratio|` ×100, clamped to
/// 0..=10000. For sources that already expose a per-wheel slip ratio (AC/ACC,
/// Forza, RaceRoom/rF2 shared memory, F1 MotionEx).
pub fn slip_from_ratios(ratios: [f32; 4]) -> i32 {
    let max_ratio = ratios.iter().fold(0.0f32, |m, &r| m.max(r.abs()));
    (max_ratio * 100.0).round().clamp(0.0, 10_000.0) as i32
}

/// Suspension-velocity impact proxy: peak `|hub velocity|` normalized to
/// 0..=1000 against `cap` (m/s that should read full-scale), clamped.
pub fn susp_impact_from_velocity(hub_velocities: [f32; 4], cap: f32) -> i32 {
    let max_v = hub_velocities.iter().fold(0.0f32, |m, &v| m.max(v.abs()));
    ((max_v / cap) * 1000.0).round().clamp(0.0, 1000.0) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slip_zero_at_standstill() {
        // The regression three decoders reintroduced: a parked car (body 0,
        // every wheel 0) must read 0% slip, not 100%.
        assert_eq!(body_relative_slip(0.0, [0.0; 4]), 0);
    }

    #[test]
    fn slip_low_speed_matched_wheels_no_false_positive() {
        // Creeping at 0.5 m/s with wheels matched → no slip (not 100%).
        assert_eq!(body_relative_slip(0.5, [0.5; 4]), 0);
    }

    #[test]
    fn slip_body_relative_typical() {
        // One wheel at 60 vs body 50 → |60−50|/50 = 0.2 → 20 (the R3E case).
        assert_eq!(body_relative_slip(50.0, [60.0, 50.0, 50.0, 50.0]), 20);
        // |15−30|/30 = 0.5 → 50 (the EA WRC case).
        assert_eq!(body_relative_slip(30.0, [15.0, 30.0, 30.0, 30.0]), 50);
    }

    #[test]
    fn slip_real_lockup_still_reads() {
        // Genuine wheel lockup while moving (wheels 0, body 40) is real slip.
        assert_eq!(body_relative_slip(40.0, [0.0; 4]), 100);
    }

    #[test]
    fn slip_wheel_sign_ignored() {
        // A wheel speed reported signed (reverse) is compared by magnitude, so
        // it can't fabricate slip vs a forward body speed. Without the internal
        // abs, |−30 − 30|/30 = 2.0 would read a false 200%.
        assert_eq!(body_relative_slip(30.0, [-30.0, 30.0, 30.0, 30.0]), 0);
        // And near-standstill jitter with a slightly-negative wheel isn't 100%.
        assert_eq!(body_relative_slip(0.5, [-0.5, 0.5, 0.5, 0.5]), 0);
    }

    #[test]
    fn slip_from_ratios_max_abs() {
        assert_eq!(slip_from_ratios([0.1, -0.35, 0.2, 0.05]), 35);
    }

    #[test]
    fn susp_impact_scales_and_clamps() {
        assert_eq!(susp_impact_from_velocity([1.0, 0.0, 0.0, 0.0], 2.0), 500);
        // 3 m/s of a 2 m/s cap → 1500, clamped to 1000.
        assert_eq!(susp_impact_from_velocity([3.0, 0.0, 0.0, 0.0], 2.0), 1000);
    }
}
