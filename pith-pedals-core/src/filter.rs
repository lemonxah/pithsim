//! Loadcell denoising filters, ported from the reference project's
//! `SignalFilter_1st_order.cpp` (2-state constant-velocity Kalman filter)
//! and `SignalFilter_2nd_order.cpp` (3-state constant-acceleration KF), plus
//! the exponential filter inlined in its `Main.cpp`. Selected at runtime by
//! `PedalConfig::kf_force_model_order` exactly like the reference's
//! `kfModelOrder_u8`: 0 = const-velocity KF, 1 = const-accel KF,
//! 2 = exponential, anything else = raw passthrough.
//!
//! Unit quirks kept verbatim from the reference (they're load-bearing for
//! the tuning constants): measurements are converted kg→grams internally
//! (R is scaled by 1e6 to match), dt is in *milliseconds*, clamped to
//! [1, 5000] µs before conversion, and the returned velocity is in g/ms —
//! numerically identical to kg/s.

const KF_CV_ACCEL_NOISE: f32 = 1.0e2; // s_kfModelNoiseAcceleration_fl32
const KF_CA_JERK_NOISE: f32 = 1.0e-5; // s_kfModelNoiseForceJerk_fl32

fn noise_scaling(model_noise: u8) -> f32 {
    let s = model_noise as f32 / 255.0;
    if s < 0.001 {
        0.001
    } else {
        s
    }
}

fn clamp_dt_ms(elapsed_us: u32) -> f32 {
    elapsed_us.clamp(1, 5000) as f32 / 1000.0
}

/// 2-state [position; velocity] Kalman filter, constant-velocity model with
/// random-acceleration process noise, Joseph-form covariance update —
/// faithful port of `KalmanFilter1stOrder`.
#[derive(Debug, Clone)]
pub struct KalmanCv {
    x: [f32; 2],
    p: [[f32; 2]; 2],
    r: f32, // measurement noise, grams²
}

impl KalmanCv {
    /// `variance_kg2`: boot-time loadcell variance estimate (kg²), converted
    /// to grams² like the reference constructor.
    pub fn new(variance_kg2: f32) -> Self {
        KalmanCv {
            x: [0.0; 2],
            p: [[1000.0, 0.0], [0.0, 1000.0]],
            r: variance_kg2 * 1000.0 * 1000.0,
        }
    }

    /// One predict+update step. `measurement_kg` in kg, `elapsed_us` since
    /// the previous call. Returns the filtered value in kg.
    pub fn update(&mut self, measurement_kg: f32, elapsed_us: u32, model_noise: u8) -> f32 {
        let z = measurement_kg * 1000.0; // grams
        let dt = clamp_dt_ms(elapsed_us);
        let (dt2, dt3, dt4) = (dt * dt, dt * dt * dt, dt * dt * dt * dt);

        let accel_var = noise_scaling(model_noise) * KF_CV_ACCEL_NOISE;
        let q = [
            [accel_var * dt4 / 4.0, accel_var * dt3 / 2.0],
            [accel_var * dt3 / 2.0, accel_var * dt2],
        ];

        // Predict: x = F x, P = F P F' + Q with F = [1 dt; 0 1].
        let xp = [self.x[0] + dt * self.x[1], self.x[1]];
        let p = &self.p;
        let pp = [
            [
                p[0][0] + dt * (p[1][0] + p[0][1] + dt * p[1][1]) + q[0][0],
                p[0][1] + dt * p[1][1] + q[0][1],
            ],
            [p[1][0] + dt * p[1][1] + q[1][0], p[1][1] + q[1][1]],
        ];

        // Update with scalar measurement z = H x, H = [1 0].
        let residual = z - xp[0];
        let s = pp[0][0] + self.r;
        if s.abs() > 1e-6 {
            let k = [pp[0][0] / s, pp[1][0] / s];
            self.x = [xp[0] + k[0] * residual, xp[1] + k[1] * residual];
            self.p = joseph_update_2(&pp, &k, self.r);
        } else {
            self.p = [[0.0; 2]; 2];
            self.x = xp;
        }
        self.x[0] / 1000.0 // grams -> kg
    }

    /// Estimated rate of change (g/ms == kg/s), `changeVelocity()`.
    pub fn velocity(&self) -> f32 {
        self.x[1]
    }
}

/// Joseph-form covariance update for a 2-state filter with H=[1,0]:
/// P = (I-KH) P (I-KH)' + K R K', then symmetrized.
fn joseph_update_2(pp: &[[f32; 2]; 2], k: &[f32; 2], r: f32) -> [[f32; 2]; 2] {
    let ikh = [[1.0 - k[0], 0.0], [-k[1], 1.0]];
    let mut t = [[0.0f32; 2]; 2]; // (I-KH) * P
    for i in 0..2 {
        for j in 0..2 {
            t[i][j] = ikh[i][0] * pp[0][j] + ikh[i][1] * pp[1][j];
        }
    }
    let mut out = [[0.0f32; 2]; 2]; // t * (I-KH)' + K R K'
    for i in 0..2 {
        for j in 0..2 {
            out[i][j] = t[i][0] * ikh[j][0] + t[i][1] * ikh[j][1] + k[i] * r * k[j];
        }
    }
    let sym = (out[0][1] + out[1][0]) / 2.0;
    out[0][1] = sym;
    out[1][0] = sym;
    out
}

/// 3-state [position; velocity; acceleration] Kalman filter, constant-
/// acceleration model with random-jerk process noise — port of
/// `KalmanFilter2ndOrder` (same structure: H=[1,0,0], grams internally,
/// Joseph-form update; the matrix algebra here is the standard dense form
/// of the reference's hand-unrolled expressions).
#[derive(Debug, Clone)]
pub struct KalmanCa {
    x: [f32; 3],
    p: [[f32; 3]; 3],
    r: f32,
}

impl KalmanCa {
    pub fn new(variance_kg2: f32) -> Self {
        let mut p = [[0.0f32; 3]; 3];
        for (i, row) in p.iter_mut().enumerate() {
            row[i] = 1000.0;
        }
        KalmanCa {
            x: [0.0; 3],
            p,
            r: variance_kg2 * 1000.0 * 1000.0,
        }
    }

    pub fn update(&mut self, measurement_kg: f32, elapsed_us: u32, model_noise: u8) -> f32 {
        let z = measurement_kg * 1000.0;
        let dt = clamp_dt_ms(elapsed_us);
        let dt2 = dt * dt;
        let dt3 = dt2 * dt;
        let dt4 = dt2 * dt2;
        let dt5 = dt4 * dt;
        let dt6 = dt5 * dt;

        let f = [[1.0, dt, 0.5 * dt2], [0.0, 1.0, dt], [0.0, 0.0, 1.0]];
        let jerk_var = noise_scaling(model_noise) * KF_CA_JERK_NOISE;
        let q = [
            [
                jerk_var * dt6 / 36.0,
                jerk_var * dt5 / 12.0,
                jerk_var * dt4 / 6.0,
            ],
            [
                jerk_var * dt5 / 12.0,
                jerk_var * dt4 / 4.0,
                jerk_var * dt3 / 2.0,
            ],
            [jerk_var * dt4 / 6.0, jerk_var * dt3 / 2.0, jerk_var * dt2],
        ];

        // Predict.
        let xp = [
            self.x[0] + dt * self.x[1] + 0.5 * dt2 * self.x[2],
            self.x[1] + dt * self.x[2],
            self.x[2],
        ];
        let fp = mat_mul_3(&f, &self.p);
        let mut pp = mat_mul_3t(&fp, &f);
        for i in 0..3 {
            for j in 0..3 {
                pp[i][j] += q[i][j];
            }
        }

        // Update, H = [1 0 0].
        let residual = z - xp[0];
        let s = pp[0][0] + self.r;
        if s.abs() > 1e-6 {
            let k = [pp[0][0] / s, pp[1][0] / s, pp[2][0] / s];
            for i in 0..3 {
                self.x[i] = xp[i] + k[i] * residual;
            }
            // Joseph form: (I-KH) P (I-KH)' + K R K', with H = [1 0 0].
            let ikh = [[1.0 - k[0], 0.0, 0.0], [-k[1], 1.0, 0.0], [-k[2], 0.0, 1.0]];
            let t = mat_mul_3(&ikh, &pp);
            let mut out = mat_mul_3t(&t, &ikh);
            for i in 0..3 {
                for j in 0..3 {
                    out[i][j] += k[i] * self.r * k[j];
                }
            }
            // Symmetrize the upper triangle (Joseph form should already be
            // symmetric; this corrects float error).
            for (i, j) in [(0, 1), (0, 2), (1, 2)] {
                let sym = (out[i][j] + out[j][i]) / 2.0;
                out[i][j] = sym;
                out[j][i] = sym;
            }
            self.p = out;
        } else {
            self.p = [[0.0; 3]; 3];
            self.x = xp;
        }
        self.x[0] / 1000.0
    }

    pub fn velocity(&self) -> f32 {
        self.x[1]
    }
}

fn mat_mul_3(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            for (l, brow) in b.iter().enumerate() {
                out[i][j] += a[i][l] * brow[j];
            }
        }
    }
    out
}

/// a * b' (multiply by transpose of b).
fn mat_mul_3t(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut out = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for (j, brow) in b.iter().enumerate() {
            for l in 0..3 {
                out[i][j] += a[i][l] * brow[l];
            }
        }
    }
    out
}

/// The complete force filter, dispatching on `kf_force_model_order` exactly
/// like the reference's `switch (kfModelOrder_u8)` in `Main.cpp`.
#[derive(Debug, Clone)]
pub struct ForceFilter {
    cv: KalmanCv,
    ca: KalmanCa,
    exp_state: f32,
    last_value: f32,
}

impl ForceFilter {
    pub fn new(variance_kg2: f32) -> Self {
        ForceFilter {
            cv: KalmanCv::new(variance_kg2),
            ca: KalmanCa::new(variance_kg2),
            exp_state: 0.0,
            last_value: 0.0,
        }
    }

    /// Returns (filtered kg, velocity kg/s).
    pub fn update(
        &mut self,
        raw_kg: f32,
        elapsed_us: u32,
        model_order: u8,
        model_noise: u8,
    ) -> (f32, f32) {
        let dt_s = (elapsed_us.max(1)) as f32 * 1e-6;
        match model_order {
            0 => {
                let v = self.cv.update(raw_kg, elapsed_us, model_noise);
                (v, self.cv.velocity())
            }
            1 => {
                let v = self.ca.update(raw_kg, elapsed_us, model_noise);
                (v, self.ca.velocity())
            }
            2 => {
                // alpha = 1 - noise/5000 (Main.cpp) — higher noise = faster.
                let alpha = 1.0 - model_noise as f32 / 5000.0;
                self.exp_state = self.exp_state * alpha + raw_kg * (1.0 - alpha);
                let vel = (self.exp_state - self.last_value) / dt_s;
                self.last_value = self.exp_state;
                (self.exp_state, vel)
            }
            _ => {
                let vel = (raw_kg - self.last_value) / dt_s;
                self.last_value = raw_kg;
                (raw_kg, vel)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kalman_cv_converges_to_constant() {
        let mut kf = KalmanCv::new(0.0004); // 20g sigma
        let mut out = 0.0;
        for _ in 0..2000 {
            out = kf.update(5.0, 500, 128);
        }
        assert!((out - 5.0).abs() < 0.05, "converged to {out}");
        assert!(kf.velocity().abs() < 0.1);
    }

    #[test]
    fn kalman_cv_tracks_ramp_velocity() {
        let mut kf = KalmanCv::new(0.0004);
        // 10 kg/s ramp sampled at 2 kHz.
        let mut t = 0.0f32;
        for _ in 0..4000 {
            t += 0.0005;
            kf.update(10.0 * t, 500, 200);
        }
        // velocity state is g/ms == kg/s.
        assert!(
            (kf.velocity() - 10.0).abs() < 1.0,
            "velocity {}",
            kf.velocity()
        );
    }

    #[test]
    fn kalman_ca_converges_to_constant() {
        let mut kf = KalmanCa::new(0.0004);
        let mut out = 0.0;
        for _ in 0..4000 {
            out = kf.update(3.0, 500, 128);
        }
        assert!((out - 3.0).abs() < 0.1, "converged to {out}");
    }

    #[test]
    fn exponential_filter_smooths() {
        let mut f = ForceFilter::new(0.0004);
        let mut out = 0.0;
        for _ in 0..5000 {
            (out, _) = f.update(7.0, 500, 2, 128);
        }
        assert!((out - 7.0).abs() < 0.01);
        // One outlier barely moves it.
        let (after, _) = f.update(100.0, 500, 2, 128);
        assert!(after < 10.0, "outlier leaked: {after}");
    }

    #[test]
    fn raw_mode_passes_through() {
        let mut f = ForceFilter::new(0.0004);
        let (v, _) = f.update(4.2, 500, 9, 128);
        assert_eq!(v, 4.2);
    }
}
