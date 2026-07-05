//! Loadcell scaling + bias/variance estimation, ported from the reference
//! project's `LoadCell.cpp` (ADS1256 branch, the one PCB V2.x compiles) and
//! the exact ADS1256 library it pins (`ChrGri/ADS1255-ADS1256`), so a raw
//! 24-bit ADC code turns into the same kilograms the reference computes.
//! The physical cell on this build is a DYLY-107 mini S-type, 50 kg — the
//! same class of cell the reference wires to this board; its rating reaches
//! this module via `PedalConfig::loadcell_rating_kg`.
//!
//! Hardware-independent on purpose: the firmware's SPI/DRDY driver feeds raw
//! codes in, the dashboard's calibration UI can reuse the same math, and all
//! of it is unit-testable on the host.

// The reference's compile-time cell model (Main.h): a 300 kg cell, 5 V
// excitation, 2 mV/V sensitivity. These only matter as the BASE of the
// runtime rescale below — `setLoadcellRating()` replaces the rating with the
// configured one, exactly like the reference does.
const BASE_RATING_KG: f32 = 300.0;
const EXCITATION_V: f32 = 5.0;
const SENSITIVITY_MV_PER_V: f32 = 2.0;

/// kg-per-volt for the compile-time base cell:
/// `LOADCELL_WEIGHT_RATING_KG / (LOADCELL_EXCITATION_V * (LOADCELL_SENSITIVITY_MV_V/1000))`.
const BASE_CONVERSION_FACTOR: f32 =
    BASE_RATING_KG / (EXCITATION_V * (SENSITIVITY_MV_PER_V / 1000.0));

/// ADS1256 analog config, verbatim from the reference (`LoadCell.cpp`):
/// 7.68 MHz crystal, 2.5 V reference, PGA gain 64 (register code 6),
/// 2000 SPS (`ADS1256_DRATE_2000SPS`), differential AIN0/AIN1, no input
/// buffer. SPI runs at crystal/4, mode 1.
pub const ADC_CLOCK_MHZ: f32 = 7.68;
pub const ADC_VREF_V: f32 = 2.5;
pub const ADC_GAIN_CODE: u8 = 6; // PGA = 1 << 6 = 64
pub const ADC_PGA: i32 = 1 << ADC_GAIN_CODE as i32;
pub const ADC_DRATE_2000SPS: u8 = 0xB0;
pub const ADC_CHANNEL_P: u8 = 0; // AIN0
pub const ADC_CHANNEL_N: u8 = 1; // AIN1

/// Samples captured at boot to estimate the zero offset + noise floor
/// (`s_numberOfSamplesForLoadcellOffsetEstimation_i32`).
pub const BIAS_ESTIMATION_SAMPLES: u32 = 1000;

/// Reference floors the variance estimate here (≈(8 g)², from an observed
/// ±50 g fluctuation ≙ 6σ) so the Kalman filter never divides by ~zero.
pub const VARIANCE_MIN_KG2: f32 = 7.0e-5;

/// Default variance before an estimate exists (`s_defaultVarianceEstimate_fl32`).
pub const VARIANCE_DEFAULT_KG2: f32 = 0.2 * 0.2;

/// Signed 24-bit ADC code → volts, exactly the pinned library's
/// `readCurrentChannel()`: `(code / 0x7FFFFF) * (2*VREF / PGA)`.
pub fn code_to_volts(code: i32) -> f32 {
    (code as f32 / 0x7F_FFFF as f32) * ((2.0 * ADC_VREF_V) / ADC_PGA as f32)
}

/// Runtime kg-per-volt for the configured cell rating, ported from
/// `setLoadcellRating()`: `2.0 * rating * (BASE_FACTOR / BASE_RATING)`.
/// (The 2.0 is the reference's own scaling, kept verbatim so a config that
/// felt right on the reference firmware feels identical here.) For the
/// DYLY-107's 50 kg rating this is 10 000 kg/V.
pub fn conversion_factor_kg_per_volt(rating_kg: u8) -> f32 {
    2.0 * rating_kg as f32 * (BASE_CONVERSION_FACTOR / BASE_RATING_KG)
}

/// Welford running mean/variance over the boot-time quiescent samples —
/// the reference's `estimateBiasAndVariance()`.
#[derive(Debug, Default)]
pub struct BiasEstimator {
    n: u32,
    mean: f32,
    m2: f32,
}

impl BiasEstimator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, weight_kg: f32) {
        self.n += 1;
        let delta = weight_kg - self.mean;
        self.mean += delta / self.n as f32;
        self.m2 += delta * (weight_kg - self.mean);
    }

    pub fn count(&self) -> u32 {
        self.n
    }

    pub fn done(&self) -> bool {
        self.n >= BIAS_ESTIMATION_SAMPLES
    }

    /// (zero offset, variance, standard deviation), variance floored like
    /// the reference so downstream filters stay sane.
    pub fn estimate(&self) -> (f32, f32, f32) {
        let var = if self.n >= 2 {
            (self.m2 / (self.n as f32 - 1.0)).max(VARIANCE_MIN_KG2)
        } else {
            VARIANCE_DEFAULT_KG2
        };
        (self.mean, var, var.sqrt())
    }
}

/// Complete code→kg scaling for one calibrated cell. `weight_kg` mirrors
/// `readLoadcellWeightInKg()`: `volts * factor - (zero + 3σ)` — the 3σ
/// deadband keeps a quiescent pedal reading ≤ 0 kg instead of dithering
/// around it.
#[derive(Debug, Clone, Copy)]
pub struct LoadcellScale {
    factor_kg_per_volt: f32,
    zero_kg: f32,
    sigma_kg: f32,
    invert: bool,
}

impl LoadcellScale {
    pub fn new(rating_kg: u8, invert: bool) -> Self {
        LoadcellScale {
            factor_kg_per_volt: conversion_factor_kg_per_volt(rating_kg),
            zero_kg: 0.0,
            sigma_kg: 0.0,
            invert,
        }
    }

    /// Install the boot-time bias estimate (in the same *scaled* kg space
    /// this struct outputs, like the reference which estimates over
    /// already-converted readings).
    pub fn set_bias(&mut self, zero_kg: f32, sigma_kg: f32) {
        self.zero_kg = zero_kg;
        self.sigma_kg = sigma_kg;
    }

    /// Raw (uncorrected) kg for a code — what the bias estimator consumes.
    pub fn raw_kg(&self, code: i32) -> f32 {
        let v = code_to_volts(if self.invert { -code } else { code });
        v * self.factor_kg_per_volt
    }

    /// Bias-corrected weight, the value the control loop uses.
    pub fn weight_kg(&self, code: i32) -> f32 {
        self.raw_kg(code) - (self.zero_kg + 3.0 * self.sigma_kg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_conversion_factor_matches_reference_constants() {
        // 300 / (5 * 0.002) = 30_000 kg/V (f32 accumulation, so ~±0.01).
        assert!((BASE_CONVERSION_FACTOR - 30_000.0).abs() < 1.0);
    }

    #[test]
    fn dyly107_rating_gives_reference_scaling() {
        // setLoadcellRating(50): 2 * 50 * (30000/300) = 10_000 kg/V.
        assert!((conversion_factor_kg_per_volt(50) - 10_000.0).abs() < 1.0);
    }

    #[test]
    fn full_scale_code_is_full_scale_voltage() {
        // +FS code = 2*Vref/PGA = 5/64 V.
        let v = code_to_volts(0x7F_FFFF);
        assert!((v - 5.0 / 64.0).abs() < 1e-6);
        assert!((code_to_volts(-0x7F_FFFF) + 5.0 / 64.0).abs() < 1e-6);
    }

    #[test]
    fn welford_matches_known_mean_and_variance() {
        let mut est = BiasEstimator::new();
        for w in [1.0f32, 2.0, 3.0, 4.0, 5.0] {
            est.add(w);
        }
        let (mean, var, sigma) = est.estimate();
        assert!((mean - 3.0).abs() < 1e-6);
        assert!((var - 2.5).abs() < 1e-5); // sample variance of 1..5
        assert!((sigma - 2.5f32.sqrt()).abs() < 1e-5);
    }

    #[test]
    fn variance_is_floored() {
        let mut est = BiasEstimator::new();
        for _ in 0..100 {
            est.add(1.234); // zero variance stream
        }
        let (_, var, _) = est.estimate();
        assert!((var - VARIANCE_MIN_KG2).abs() < 1e-9);
    }

    #[test]
    fn weight_applies_zero_and_three_sigma() {
        let mut scale = LoadcellScale::new(50, false);
        scale.set_bias(0.5, 0.01);
        let raw = scale.raw_kg(1000);
        let w = scale.weight_kg(1000);
        assert!((raw - w - (0.5 + 0.03)).abs() < 1e-6);
    }

    #[test]
    fn invert_flips_sign() {
        let scale = LoadcellScale::new(50, true);
        let plain = LoadcellScale::new(50, false);
        assert!((scale.raw_kg(1000) + plain.raw_kg(1000)).abs() < 1e-6);
    }
}
