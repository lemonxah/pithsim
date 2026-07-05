//! Pedal linkage kinematics, ported from the reference project's
//! `PedalGeometry.h` (forward kinematics + loadcell→pedal-face force
//! conversion) and the analytical inverse kinematics from
//! `StepperMovementStrategy.h` §15.
//!
//! Geometry model (reference's a/b/c/d linkage, law-of-cosines):
//! - A: lower pedal pivot (origin), B: sled/rear pivot, C: upper pedal pivot
//! - `a`: loadcell rod (C–B), `b`: lower pedal plate (A–C),
//!   `c`: sled line A–B (horizontal component grows with sled travel),
//!   `d`: upper pedal plate (C to foot plate)
//!
//! The reference uses `FastTrig` approximations (`iacos`, `isin`, `atan2Fast`)
//! for speed on a 4 kHz loop; this port uses libm's exact `acos`/`atan2`/
//! `sin`/`cos` — the S3 has a hardware FPU and our loop runs at 2 kHz, so
//! exactness is affordable and strictly more accurate.

/// Static linkage dimensions (mm), from `PedalConfig`'s `length_*` fields.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Linkage {
    pub a_mm: f32,
    pub b_mm: f32,
    pub d_mm: f32,
    pub c_horizontal_mm: f32,
    pub c_vertical_mm: f32,
}

impl Linkage {
    /// Pedal incline angle (degrees, relative to horizontal) for a sled
    /// position in mm — `pedalInclineAngleDeg`.
    pub fn incline_angle_deg(&self, sled_mm: f32) -> f32 {
        let c_hor = self.c_horizontal_mm + sled_mm;
        let c = (self.c_vertical_mm * self.c_vertical_mm + c_hor * c_hor).sqrt();

        let nom = self.b_mm * self.b_mm + c * c - self.a_mm * self.a_mm;
        let den = 2.0 * self.b_mm * c;
        let mut alpha_rad = 0.0f32;
        if den.abs() > 0.01 {
            alpha_rad = (nom / den).clamp(-1.0, 1.0).acos();
        }
        if c_hor.abs() > 0.01 {
            alpha_rad += self.c_vertical_mm.atan2(c_hor);
        }
        alpha_rad.to_degrees()
    }

    /// Loadcell reading → force at the pedal face — `convertToPedalForce`:
    /// `F_pedal = F_loadcell * b/(b+d) * sqrt(1 - cos²γ)` with γ from the
    /// law of cosines on the a/b/c triangle at this sled position.
    pub fn pedal_force(&self, loadcell_force: f32, sled_mm: f32) -> f32 {
        let c_hor = self.c_horizontal_mm + sled_mm;
        let c = (self.c_vertical_mm * self.c_vertical_mm + c_hor * c_hor).sqrt();

        let b_plus_d = (self.b_mm + self.d_mm).abs();
        let nom = self.a_mm * self.a_mm + self.b_mm * self.b_mm - c * c;
        let den = 2.0 * self.a_mm * self.b_mm;
        let mut cos_sq = 0.0f32;
        if den.abs() > 0.01 {
            let arg = nom / den;
            cos_sq = arg * arg;
        }
        let one_minus = 1.0 - cos_sq;
        if b_plus_d > 0.0 && one_minus > 0.0 {
            loadcell_force * self.b_mm / b_plus_d * one_minus.sqrt()
        } else {
            loadcell_force
        }
    }

    /// Analytical inverse kinematics — target pedal angle (deg) → sled
    /// position (mm), from `StepperMovementStrategy.h` §15: place C on the
    /// pedal-arm circle at the target angle, then solve the loadcell-rod
    /// circle for the sled's horizontal coordinate (positive root; the sled
    /// is horizontally beyond the pedal arm). Returns `None` when the
    /// target is kinematically impossible (rod can't reach) — caller keeps
    /// its previous position, exactly like the reference's fallback.
    pub fn sled_mm_for_angle(&self, angle_deg: f32) -> Option<f32> {
        let angle_rad = angle_deg.to_radians();
        let cx = self.b_mm * angle_rad.cos();
        let cy = self.b_mm * angle_rad.sin();
        let dy = self.c_vertical_mm - cy;
        let dx_sq = self.a_mm * self.a_mm - dy * dy;
        if dx_sq > 0.0 {
            let c_hor = cx + dx_sq.sqrt();
            Some(c_hor - self.c_horizontal_mm)
        } else {
            None
        }
    }
}

/// Steps → sled mm: `steps * (1/steps_per_rev) * pitch` (`sledPositionInMM`).
pub fn sled_mm_from_steps(steps_from_min: f32, steps_per_rev: u32, pitch_mm_per_rev: f32) -> f32 {
    if steps_per_rev == 0 {
        return 0.0;
    }
    steps_from_min * (1.0 / steps_per_rev as f32) * pitch_mm_per_rev
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference-default geometry (DiyActivePedal_types.cpp defaults):
    /// a=205, b=220, d=60, cH=215, cV=60.
    fn default_linkage() -> Linkage {
        Linkage {
            a_mm: 205.0,
            b_mm: 220.0,
            d_mm: 60.0,
            c_horizontal_mm: 215.0,
            c_vertical_mm: 60.0,
        }
    }

    #[test]
    fn angle_is_monotonic_in_sled_travel() {
        // The incline angle must change monotonically with sled position for
        // the inverse kinematics to be single-valued — direction (increasing
        // vs decreasing) depends on the linkage and is absorbed by the motor-
        // direction config flag. For the reference default geometry it
        // decreases as the sled extends.
        let l = default_linkage();
        let a0 = l.incline_angle_deg(0.0);
        let a50 = l.incline_angle_deg(50.0);
        let a100 = l.incline_angle_deg(100.0);
        assert!(
            a0 > a50 && a50 > a100,
            "expected monotonic incline: {a0} {a50} {a100}"
        );
        // Sanity: physical pedal angles live in a plausible range.
        assert!(a100 > 10.0 && a0 < 120.0);
    }

    #[test]
    fn inverse_kinematics_round_trips_forward() {
        let l = default_linkage();
        for sled in [0.0f32, 20.0, 40.0, 60.0, 80.0, 100.0] {
            let angle = l.incline_angle_deg(sled);
            let back = l.sled_mm_for_angle(angle).expect("angle must be reachable");
            assert!(
                (back - sled).abs() < 0.05,
                "IK round trip at {sled}mm gave {back}mm"
            );
        }
    }

    #[test]
    fn pedal_force_applies_lever_ratio() {
        let l = default_linkage();
        let f = l.pedal_force(10.0, 50.0);
        // b/(b+d) = 220/280 ≈ 0.786 is the upper bound (times sin γ ≤ 1).
        assert!(f > 0.0 && f < 10.0 * (220.0 / 280.0) + 0.01, "force {f}");
    }

    #[test]
    fn degenerate_geometry_passes_force_through() {
        let l = Linkage {
            a_mm: 0.0,
            b_mm: 0.0,
            d_mm: 0.0,
            c_horizontal_mm: 0.0,
            c_vertical_mm: 0.0,
        };
        // All-zero geometry (unconfigured) must not blow up or zero the force.
        assert_eq!(l.pedal_force(5.0, 0.0), 5.0);
        assert_eq!(l.incline_angle_deg(0.0), 0.0);
    }

    #[test]
    fn steps_to_mm_uses_pitch() {
        // 3750 steps/rev (reference Pr0.08), 5mm pitch: one rev = 5mm.
        assert!((sled_mm_from_steps(3750.0, 3750, 5.0) - 5.0).abs() < 1e-4);
        assert_eq!(sled_mm_from_steps(100.0, 0, 5.0), 0.0);
    }
}
