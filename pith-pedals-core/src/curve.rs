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
        let n = self.count;
        let x = &self.travel_pct;
        let y = &self.force_pct;
        let q = travel_pct_q.clamp(x[0], x[n - 1]);

        let mut seg = 0usize;
        while seg < n - 2 {
            if q <= x[seg + 1] {
                break;
            }
            seg += 1;
        }

        let dx = x[seg + 1] - x[seg];
        let t = (q - x[seg]) / dx;
        (1.0 - t) * y[seg]
            + t * y[seg + 1]
            + t * (1.0 - t) * (self.a[seg] * (1.0 - t) + self.b[seg] * t)
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
}
