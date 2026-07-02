//! Shift-light computation — the color of each rev-bar LED for the current
//! telemetry. Pure (time passed in as `now_ms`) so it's host-testable and shared
//! by BOTH the physical LED strip and the on-screen RPM strip, guaranteeing they
//! match. Port of `led_rev_segment_rgb` + the car-data / rev-config logic.

use crate::simhub::Telemetry;

pub const CAR_LED_MAX: usize = 12; // physical rev LEDs
pub const CAR_GEARS: usize = 11; // R, N, 1..9
/// Shift-light flash half-period (pub so render caches can key repaints on the
/// exact same phase the strip strobes with).
pub const FLASH_MS: i64 = 100;

/// Generic progressive rev-bar config (used when no car is loaded). Colors RGB888.
#[derive(Clone, Copy)]
pub struct RevCfg {
    pub start_pct: u8, // bar starts lighting here (% of shift RPM)
    pub flash_pct: u8, // whole bar strobes at/above here
    pub n_green: u8,
    pub n_red: u8,
    pub n_blue: u8,
    pub col_green: u32,
    pub col_red: u32,
    pub col_blue: u32,
    pub col_flash: u32,
}

impl Default for RevCfg {
    fn default() -> Self {
        // REV_CFG_DEFAULT: fills from 50% of shift RPM, strobes blue at the top.
        RevCfg {
            start_pct: 50,
            flash_pct: 98,
            n_green: 6,
            n_red: 3,
            n_blue: 3,
            col_green: 0x00FF00,
            col_red: 0xFF0000,
            col_blue: 0x0050FF,
            col_flash: 0x0050FF,
        }
    }
}

/// Per-car shift-light data (lovely-car-data). Invalid until a car is loaded.
#[derive(Clone)]
pub struct CarData {
    pub valid: bool,
    pub led_count: usize,                       // ledNumber (clamped to CAR_LED_MAX)
    pub blink_ms: u16,                          // redlineBlinkInterval
    pub led_color: [u32; CAR_LED_MAX],          // per-LED RGB888
    pub redline: [u16; CAR_GEARS],              // per-gear shift/redline rpm
    pub thresh: [[u16; CAR_LED_MAX]; CAR_GEARS], // per-gear per-LED on-threshold
    pub name: String,
}

impl Default for CarData {
    fn default() -> Self {
        CarData {
            valid: false,
            led_count: 0,
            blink_ms: 100,
            led_color: [0; CAR_LED_MAX],
            redline: [0; CAR_GEARS],
            thresh: [[0; CAR_LED_MAX]; CAR_GEARS],
            name: String::new(),
        }
    }
}

/// Gear char ('R','N','1'..'9') -> gear index 0..10 (default neutral).
pub fn gear_index(g: u8) -> usize {
    match g {
        b'R' => 0,
        b'N' => 1,
        b'1'..=b'9' => 2 + (g - b'1') as usize,
        _ => 1,
    }
}

/// Effective shift RPM: the reported value, else ~93% of max.
pub fn shift_rpm_of(t: &Telemetry) -> i32 {
    if t.shift_rpm > 4200 {
        t.shift_rpm
    } else if t.max_rpm > 4200 {
        t.max_rpm * 93 / 100
    } else {
        0
    }
}

/// Color (0xRRGGBB, 0 = off) of rev-bar LED `i` of `count`, for `t`. Honors a
/// loaded car (per-gear/per-LED thresholds + colors) or the generic `cfg` bar,
/// with the shift-point flash strobed using `now_ms`.
pub fn segment_rgb(
    t: &Telemetry,
    i: i32,
    count: i32,
    cfg: &RevCfg,
    car: &CarData,
    now_ms: i64,
) -> u32 {
    if count <= 0 {
        return 0;
    }

    // Car-data path: per-gear, per-LED RPM thresholds + per-LED colors.
    if car.valid {
        let gi = gear_index(t.gear);
        let rl = car.redline[gi] as i32;
        let over = rl > 0 && t.rpm >= rl;
        // The car data solely controls the redline flash: `blink_ms == 0` means the
        // car does NOT flash — hold the LEDs SOLID at/over redline; a non-zero value
        // strobes at that interval. Colours are always the car's own `led_color`.
        let flash_on = if car.blink_ms == 0 {
            true
        } else {
            (now_ms / car.blink_ms as i64) & 1 != 0
        };
        let off = ((count - car.led_count as i32) / 2).max(0); // center fewer LEDs
        let ci = i - off;
        if ci >= 0 && (ci as usize) < car.led_count {
            let ci = ci as usize;
            if over {
                return if flash_on { car.led_color[ci] } else { 0 };
            }
            if car.thresh[gi][ci] > 0 && t.rpm >= car.thresh[gi][ci] as i32 {
                return car.led_color[ci];
            }
        }
        return 0;
    }

    // Shift-config bar: fills from start_pct of the shift RPM, strobes at flash_pct.
    let shift = shift_rpm_of(t);
    let startrpm = shift * cfg.start_pct as i32 / 100;
    let span = shift - startrpm;
    let mut lit = 0;
    if span > 0 {
        lit = ((t.rpm - startrpm) * count / span).clamp(0, count);
    }
    let pct = if shift > 0 { t.rpm * 100 / shift } else { 0 };
    if pct >= cfg.flash_pct as i32 {
        return if (now_ms / FLASH_MS) & 1 != 0 {
            cfg.col_flash
        } else {
            0
        };
    }
    if i >= lit {
        return 0;
    }
    let ng = cfg.n_green as i32;
    let nr = cfg.n_red as i32;
    if i < ng {
        cfg.col_green
    } else if i < ng + nr {
        cfg.col_red
    } else {
        cfg.col_blue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simhub::Telemetry;

    fn tel(gear: u8, rpm: i32, max: i32, shift: i32) -> Telemetry {
        let mut t = Telemetry::idle();
        t.gear = gear;
        t.rpm = rpm;
        t.max_rpm = max;
        t.shift_rpm = shift;
        t
    }

    #[test]
    fn rev_bar_fills_and_flashes() {
        let cfg = RevCfg::default();
        let car = CarData::default();
        let count = 12;
        // Below start (50% of 8000=4000): all off.
        let t = tel(b'3', 3000, 8800, 8000);
        for i in 0..count {
            assert_eq!(segment_rgb(&t, i, count, &cfg, &car, 0), 0);
        }
        // Near shift but below flash_pct (98% of 8000 = 7840): some lit, no strobe.
        let t = tel(b'3', 7000, 8800, 8000);
        assert_ne!(segment_rgb(&t, 0, count, &cfg, &car, 0), 0); // first (green) lit
        // At/above flash_pct: strobes — off at now_ms=0, on at now_ms=FLASH_MS
        // (matches the C `((now/FLASH_MS)&1) ? col : 0`).
        let t = tel(b'3', 8000, 8800, 8000);
        assert_eq!(segment_rgb(&t, 0, count, &cfg, &car, 0), 0);
        assert_eq!(segment_rgb(&t, 0, count, &cfg, &car, FLASH_MS), cfg.col_flash);
    }

    #[test]
    fn rev_bar_zone_colors() {
        let cfg = RevCfg::default(); // 6 green, 3 red, 3 blue
        let car = CarData::default();
        let t = tel(b'4', 7800, 8800, 8000); // high enough to light most, below flash
        // Force full fill by being just under flash threshold.
        assert_eq!(segment_rgb(&t, 0, 12, &cfg, &car, 0), cfg.col_green);
        assert_eq!(segment_rgb(&t, 6, 12, &cfg, &car, 0), cfg.col_red);
        assert_eq!(segment_rgb(&t, 9, 12, &cfg, &car, 0), cfg.col_blue);
    }

    #[test]
    fn car_data_thresholds() {
        let cfg = RevCfg::default();
        let mut car = CarData::default();
        car.valid = true;
        car.led_count = 12;
        car.redline[gear_index(b'2')] = 9000;
        for ci in 0..12 {
            car.thresh[gear_index(b'2')][ci] = 6000 + ci as u16 * 200;
            car.led_color[ci] = 0x112233;
        }
        let t = tel(b'2', 6500, 9500, 9000);
        // LED 0 (thr 6000) lit, LED 11 (thr 8200) not.
        assert_eq!(segment_rgb(&t, 0, 12, &cfg, &car, 0), 0x112233);
        assert_eq!(segment_rgb(&t, 11, 12, &cfg, &car, 0), 0);
        // Over redline -> flash: off at now_ms=0, on at now_ms=blink_ms.
        let t = tel(b'2', 9200, 9500, 9000);
        assert_eq!(segment_rgb(&t, 0, 12, &cfg, &car, 0), 0);
        assert_eq!(segment_rgb(&t, 0, 12, &cfg, &car, car.blink_ms as i64), 0x112233);
    }
}
