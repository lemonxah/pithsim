//! Force-vs-travel curve: an 11-point natural cubic spline, ported from
//! `Common_Libs/CubicInterpolatorFloat` (the reference project's own curve
//! math, also used by its SimHub plugin's editor for consistent preview).
//!
//! The knot derivatives are solved once (curve edit time) via the Thomas
//! algorithm for a tridiagonal system — the classic "natural cubic spline"
//! formulation — then each evaluation is a single Hermite-form blend
//! between the two knots bracketing the query point. Faithful port of
//! `Cubic::fitMatrix` / `Cubic::interpolate` (ESP32/src/ForceCurve.cpp +
//! Common_Libs/CubicInterpolatorFloat/src/CubicInterpolatorFloat.cpp).

pub const MAX_POINTS: usize = 11;

/// A force-vs-travel curve: `travel_pct[i]` (0..100, strictly increasing) to
/// `force_pct[i]` (0..100, relative — scaled to `[min_force, max_force]` by
/// the caller). `count` of the 11 slots are in use (>= 2).
#[derive(Clone, Debug, PartialEq)]
pub struct ForceCurve {
    pub travel_pct: [f32; MAX_POINTS],
    pub force_pct: [f32; MAX_POINTS],
    pub count: usize,
    // Precomputed Hermite tangent coefficients, one pair per segment
    // (count - 1 segments), solved by `fit`.
    a: [f32; MAX_POINTS],
    b: [f32; MAX_POINTS],
}

impl ForceCurve {
    /// A straight-line default (0% travel -> 0% force, 100% -> 100%).
    pub fn linear_default() -> Self {
        let mut c = ForceCurve {
            travel_pct: [0.0; MAX_POINTS],
            force_pct: [0.0; MAX_POINTS],
            count: 2,
            a: [0.0; MAX_POINTS],
            b: [0.0; MAX_POINTS],
        };
        c.travel_pct[1] = 100.0;
        c.force_pct[1] = 100.0;
        c.fit();
        c
    }

    /// Build from explicit knots (`travel_pct` must be strictly increasing;
    /// `points.len()` must be 2..=11) and solve the spline coefficients.
    pub fn from_points(points: &[(f32, f32)]) -> Option<Self> {
        if points.len() < 2 || points.len() > MAX_POINTS {
            return None;
        }
        for w in points.windows(2) {
            if w[1].0 <= w[0].0 {
                return None; // travel must be strictly increasing
            }
        }
        let mut c = ForceCurve {
            travel_pct: [0.0; MAX_POINTS],
            force_pct: [0.0; MAX_POINTS],
            count: points.len(),
            a: [0.0; MAX_POINTS],
            b: [0.0; MAX_POINTS],
        };
        for (i, &(t, f)) in points.iter().enumerate() {
            c.travel_pct[i] = t;
            c.force_pct[i] = f;
        }
        c.fit();
        Some(c)
    }

    /// Solve the tridiagonal system for each knot's derivative estimate
    /// (Thomas algorithm), then derive the two Hermite coefficients per
    /// segment. Verbatim port of `Cubic::fitMatrix`.
    fn fit(&mut self) {
        let n = self.count;
        let x = &self.travel_pct;
        let y = &self.force_pct;

        let mut mat_a = [0.0f32; MAX_POINTS];
        let mut mat_b = [0.0f32; MAX_POINTS];
        let mut mat_c = [0.0f32; MAX_POINTS];
        let mut r = [0.0f32; MAX_POINTS];

        let dx1 = x[1] - x[0];
        mat_c[0] = 1.0 / dx1;
        mat_b[0] = 2.0 * mat_c[0];
        r[0] = 3.0 * (y[1] - y[0]) / (dx1 * dx1);

        for i in 1..n - 1 {
            let dx1 = x[i] - x[i - 1];
            let dx2 = x[i + 1] - x[i];
            mat_a[i] = 1.0 / dx1;
            mat_c[i] = 1.0 / dx2;
            mat_b[i] = 2.0 * (mat_a[i] + mat_c[i]);
            let dy1 = y[i] - y[i - 1];
            let dy2 = y[i + 1] - y[i];
            r[i] = 3.0 * (dy1 / (dx1 * dx1) + dy2 / (dx2 * dx2));
        }

        let dx1 = x[n - 1] - x[n - 2];
        let dy1 = y[n - 1] - y[n - 2];
        mat_a[n - 1] = 1.0 / dx1;
        mat_b[n - 1] = 2.0 * mat_a[n - 1];
        r[n - 1] = 3.0 * (dy1 / (dx1 * dx1));

        // Thomas algorithm (tridiagonal solve) for the knot derivatives `k`.
        let mut c_prime = [0.0f32; MAX_POINTS];
        let mut d_prime = [0.0f32; MAX_POINTS];
        let mut k = [0.0f32; MAX_POINTS];

        c_prime[0] = mat_c[0] / mat_b[0];
        for i in 1..n {
            c_prime[i] = mat_c[i] / (mat_b[i] - c_prime[i - 1] * mat_a[i]);
        }
        d_prime[0] = r[0] / mat_b[0];
        for i in 1..n {
            d_prime[i] =
                (r[i] - d_prime[i - 1] * mat_a[i]) / (mat_b[i] - c_prime[i - 1] * mat_a[i]);
        }
        k[n - 1] = d_prime[n - 1];
        for i in (0..n - 1).rev() {
            k[i] = d_prime[i] - c_prime[i] * k[i + 1];
        }

        for i in 1..n {
            let dx1 = x[i] - x[i - 1];
            let dy1 = y[i] - y[i - 1];
            self.a[i - 1] = k[i - 1] * dx1 - dy1;
            self.b[i - 1] = -k[i] * dx1 + dy1;
        }
    }

    /// Evaluate the curve at `travel_pct_q` (0..100, clamped) -> force_pct
    /// (0..100 relative; caller scales to the configured force range).
    /// Verbatim port of `Cubic::interpolate`'s single-point case.
    pub fn eval(&self, travel_pct_q: f32) -> f32 {
        let (seg, t, _) = self.segment_at(travel_pct_q);
        let y = &self.force_pct;
        (1.0 - t) * y[seg]
            + t * y[seg + 1]
            + t * (1.0 - t) * (self.a[seg] * (1.0 - t) + self.b[seg] * t)
    }

    /// First derivative dforce_pct/dtravel_pct at `travel_pct_q` — the port
    /// of `EvalForceGradientCubicSpline` (normalized form): product rule on
    /// the Hermite blend, then chain rule through `t = (q - x0)/dx`.
    pub fn eval_gradient(&self, travel_pct_q: f32) -> f32 {
        let (seg, t, dx) = self.segment_at(travel_pct_q);
        let y = &self.force_pct;
        let (a, b) = (self.a[seg], self.b[seg]);
        let dy = y[seg + 1] - y[seg];
        dy / dx + (1.0 - 2.0 * t) * (a * (1.0 - t) + b * t) / dx + t * (1.0 - t) * (b - a) / dx
    }

    fn segment_at(&self, travel_pct_q: f32) -> (usize, f32, f32) {
        let n = self.count;
        let x = &self.travel_pct;
        let q = travel_pct_q.clamp(x[0], x[n - 1]);
        let mut seg = 0usize;
        while seg < n - 2 {
            if q <= x[seg + 1] {
                break;
            }
            seg += 1;
        }
        let dx = x[seg + 1] - x[seg];
        (seg, (q - x[seg]) / dx, dx)
    }
}

/// A [`ForceCurve`] scaled to an absolute force range — the analogue of the
/// reference's `EvalForceCubicSpline` (which returns
/// `forceMin + eval%/100 * (forceMax - forceMin)`, both in kg, with
/// forceMin = preload and forceMax = max force from the config).
#[derive(Clone, Debug)]
pub struct ScaledCurve {
    pub curve: ForceCurve,
    pub min_kg: f32,
    pub max_kg: f32,
}

impl ScaledCurve {
    /// Absolute spring force (kg) at normalized position `pos_01` (0..1).
    pub fn force_kg(&self, pos_01: f32) -> f32 {
        let range = self.max_kg - self.min_kg;
        if range > 0.0 {
            self.min_kg + self.curve.eval(pos_01.clamp(0.0, 1.0) * 100.0) / 100.0 * range
        } else {
            self.min_kg
        }
    }

    /// Local stiffness as kg per unit of normalized position (kg / pos_01).
    /// Divide by travel-steps for the reference's kg/step; multiply by
    /// `9.81 / total_travel_m` for N/m.
    pub fn gradient_kg_per_unit(&self, pos_01: f32) -> f32 {
        let range = self.max_kg - self.min_kg;
        // d(force_kg)/d(pos_01) = d(force_pct)/d(travel_pct) * range/100 * 100
        self.curve.eval_gradient(pos_01.clamp(0.0, 1.0) * 100.0) * range
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_default_passes_through_endpoints() {
        let c = ForceCurve::linear_default();
        assert_eq!(c.eval(0.0), 0.0);
        assert_eq!(c.eval(100.0), 100.0);
        // A 2-point "curve" degenerates to a straight line.
        assert!((c.eval(50.0) - 50.0).abs() < 0.01);
    }

    #[test]
    fn passes_through_all_knots() {
        let pts = [(0.0, 0.0), (20.0, 40.0), (60.0, 55.0), (100.0, 100.0)];
        let c = ForceCurve::from_points(&pts).unwrap();
        for &(t, f) in &pts {
            assert!(
                (c.eval(t) - f).abs() < 0.01,
                "knot ({t},{f}) not reproduced: got {}",
                c.eval(t)
            );
        }
    }

    #[test]
    fn clamps_outside_range() {
        let c = ForceCurve::linear_default();
        assert_eq!(c.eval(-10.0), c.eval(0.0));
        assert_eq!(c.eval(150.0), c.eval(100.0));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(ForceCurve::from_points(&[(0.0, 0.0)]).is_none()); // too few
        assert!(ForceCurve::from_points(&[(0.0, 0.0), (0.0, 50.0)]).is_none()); // non-increasing
        let too_many: Vec<(f32, f32)> = (0..12).map(|i| (i as f32 * 10.0, 0.0)).collect();
        assert!(ForceCurve::from_points(&too_many).is_none());
    }

    #[test]
    fn gradient_of_linear_curve_is_slope() {
        let c = ForceCurve::linear_default();
        for q in [0.0, 25.0, 50.0, 99.0] {
            assert!(
                (c.eval_gradient(q) - 1.0).abs() < 0.01,
                "gradient at {q} = {}",
                c.eval_gradient(q)
            );
        }
    }

    #[test]
    fn gradient_matches_finite_difference() {
        let pts = [(0.0, 0.0), (20.0, 40.0), (60.0, 55.0), (100.0, 100.0)];
        let c = ForceCurve::from_points(&pts).unwrap();
        for q in [5.0f32, 30.0, 50.0, 80.0, 95.0] {
            let h = 0.01;
            let fd = (c.eval(q + h) - c.eval(q - h)) / (2.0 * h);
            let an = c.eval_gradient(q);
            assert!(
                (fd - an).abs() < 0.05,
                "at {q}: analytic {an} vs finite-diff {fd}"
            );
        }
    }

    #[test]
    fn scaled_curve_maps_preload_to_max() {
        let sc = ScaledCurve {
            curve: ForceCurve::linear_default(),
            min_kg: 2.0,
            max_kg: 60.0,
        };
        assert!((sc.force_kg(0.0) - 2.0).abs() < 1e-3);
        assert!((sc.force_kg(1.0) - 60.0).abs() < 1e-3);
        assert!((sc.force_kg(0.5) - 31.0).abs() < 0.1);
        // Linear: gradient = full range per unit pos.
        assert!((sc.gradient_kg_per_unit(0.5) - 58.0).abs() < 0.5);
    }
}
