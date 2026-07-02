//! WS2812/SK6812 rev/TC/ABS LED strip. Drives the espressif/led_strip RMT device
//! and computes rev-bar colors with the shared pith_core::shift logic (so the
//! strip and the on-screen RPM strip match). Car shift-light data (@C/@SL) is
//! parsed here into pith_core::shift::CarData.

use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use esp_idf_svc::sys;
use pith_core::shift::{segment_rgb, CarData, RevCfg, CAR_GEARS, CAR_LED_MAX};

use crate::{state, usb};

// TC amber / ABS cyan (RGB888).
const C_TC: (u8, u8, u8) = (255, 80, 0);
const C_ABS: (u8, u8, u8) = (0, 150, 255);

/// Loaded car shift-light data (None = use the generic RevCfg bar).
static CAR: Mutex<Option<CarData>> = Mutex::new(None);

/// Snapshot the loaded car profile (default when none) so the on-screen RPM strip
/// renders identically to the hardware LED strip (same `segment_rgb` inputs).
pub fn current_car() -> CarData {
    CAR.lock().unwrap().clone().unwrap_or_default()
}

fn now_ms() -> i64 {
    unsafe { sys::esp_timer_get_time() / 1000 }
}

/// Parse + apply a lovely-car-data JSON (@C / @SL). Returns false if invalid.
/// An empty object (`{}`) — or empty payload — CLEARS the loaded car so the
/// strip falls back to the generic rev bar driven by the telemetry's
/// shift/max RPM (the dashboard sends this when the running game has no
/// matching car profile; without it a stale car from a previous game keeps
/// thresholds the new game's cars may never reach, i.e. dead shift lights).
pub fn apply_car_json(json: &str) -> bool {
    let t = json.trim();
    if t.is_empty() || t == "{}" {
        *CAR.lock().unwrap() = None;
        return true;
    }
    match parse_car(json) {
        Some(c) => {
            *CAR.lock().unwrap() = Some(c);
            true
        }
        None => false,
    }
}

// "#AARRGGBB" / "#RRGGBB" -> 0xRRGGBB (alpha dropped).
fn parse_hex(s: &str) -> u32 {
    let s = s.strip_prefix('#').unwrap_or(s);
    u32::from_str_radix(s, 16).unwrap_or(0) & 0x00FF_FFFF
}

fn parse_car(json: &str) -> Option<CarData> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut c = CarData::default();

    if let Some(nm) = v.get("carName").and_then(|x| x.as_str()) {
        c.name = nm.chars().take(23).collect();
    }
    let raw_n = v.get("ledNumber").and_then(|x| x.as_i64()).unwrap_or(0).max(0) as usize;
    let n = raw_n.min(CAR_LED_MAX);
    c.led_count = n;
    // More LEDs than the strip: drop the LOWER (early) LEDs, keep the upper shift
    // zone near redline. `skip` shifts our read window up.
    let skip = raw_n.saturating_sub(CAR_LED_MAX);

    // redlineBlinkInterval from the car data solely decides the redline flash:
    //   0  -> no flash (LEDs hold solid at redline)
    //   >0 -> strobe at that ms interval (clamped to >=20 ms so it stays readable)
    // An ABSENT field defaults to 100 ms (flash) for back-compat with older data.
    let bi = v.get("redlineBlinkInterval").and_then(|x| x.as_i64()).unwrap_or(100);
    c.blink_ms = if bi <= 0 { 0 } else { bi.max(20) as u16 };

    // ledColor[0] is the redline color; [1..] per-LED. Keep upper n -> skip+i+1.
    if let Some(lc) = v.get("ledColor").and_then(|x| x.as_array()) {
        for i in 0..n {
            if let Some(col) = lc.get(skip + i + 1).and_then(|x| x.as_str()) {
                c.led_color[i] = parse_hex(col);
            }
        }
    }

    // ledRpm[0] maps each gear -> [redline, led1rpm, ..., ledNrpm].
    let gears = v.get("ledRpm").and_then(|x| x.as_array()).and_then(|a| a.first());
    if let Some(gears) = gears {
        const KEYS: [&str; CAR_GEARS] =
            ["R", "N", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
        for (gi, key) in KEYS.iter().enumerate() {
            let arr = match gears.get(*key).and_then(|x| x.as_array()) {
                Some(a) => a,
                None => continue,
            };
            if let Some(rl) = arr.first().and_then(|x| x.as_i64()) {
                c.redline[gi] = rl as u16;
            }
            for i in 0..n {
                if let Some(th) = arr.get(skip + i + 1).and_then(|x| x.as_i64()) {
                    c.thresh[gi][i] = th as u16;
                }
            }
        }
    }

    if n == 0 {
        return None;
    }
    c.valid = true;
    Some(c)
}

/// The RMT strip + its rev/TC/ABS layout. Lives entirely in the LED task (the
/// raw handle is not Send), so no synchronization needed on it.
struct Strip {
    handle: sys::led_strip_handle_t,
    rgbw: bool,
    rev_count: i32,
    tc_first: i32,
    tc_count: i32,
    abs_first: i32,
    abs_count: i32,
    led_count: i32,
}

impl Strip {
    fn set_px(&self, i: i32, rgb: (u8, u8, u8), bright: u8) {
        // bright is 0..100; matches the legacy c * (pct*255/100) / 255 == c*pct/100.
        let scale = |c: u8| c as u32 * bright as u32 / 100;
        let (r, g, b) = (scale(rgb.0), scale(rgb.1), scale(rgb.2));
        unsafe {
            if self.rgbw {
                sys::led_strip_set_pixel_rgbw(self.handle, i as u32, r, g, b, 0);
            } else {
                sys::led_strip_set_pixel(self.handle, i as u32, r, g, b);
            }
        }
    }

    fn clear(&self) {
        unsafe { sys::led_strip_clear(self.handle) };
    }
    fn refresh(&self) {
        unsafe { sys::led_strip_refresh(self.handle) };
    }
}

fn init_strip() -> Option<Strip> {
    let p = state::with(|s| s.pins);
    let rev = if p.led_rev > 0 { p.led_rev } else { 12 };
    let tc = p.led_tc.max(0);
    let abs = p.led_abs.max(0);
    let led_count = (rev + tc + abs).max(1);
    let rgbw = p.led_rgbw != 0;

    let mut cfg: sys::led_strip_config_t = unsafe { core::mem::zeroed() };
    cfg.strip_gpio_num = p.led_din;
    cfg.max_leds = led_count as u32;
    cfg.led_pixel_format = if rgbw {
        sys::led_pixel_format_t_LED_PIXEL_FORMAT_GRBW
    } else {
        sys::led_pixel_format_t_LED_PIXEL_FORMAT_GRB
    };
    cfg.led_model = if rgbw {
        sys::led_model_t_LED_MODEL_SK6812
    } else {
        sys::led_model_t_LED_MODEL_WS2812
    };

    let mut rmt: sys::led_strip_rmt_config_t = unsafe { core::mem::zeroed() };
    rmt.clk_src = sys::soc_periph_rmt_clk_src_t_RMT_CLK_SRC_DEFAULT;
    rmt.resolution_hz = 10_000_000;
    rmt.mem_block_symbols = 64;

    let mut handle: sys::led_strip_handle_t = core::ptr::null_mut();
    let err = unsafe { sys::led_strip_new_rmt_device(&cfg, &rmt, &mut handle) };
    if err != 0 || handle.is_null() {
        log::error!("led_strip init failed: {err}");
        return None;
    }
    let strip = Strip {
        handle,
        rgbw,
        rev_count: rev,
        tc_first: rev,
        tc_count: tc,
        abs_first: rev + tc,
        abs_count: abs,
        led_count,
    };
    strip.clear();
    strip.refresh();
    log::info!("rev strip on gpio{}, {} leds, rgbw={}", p.led_din, led_count, rgbw);
    Some(strip)
}

fn selftest(strip: &Strip) {
    let solid = [(255u8, 0, 0), (0, 255, 0), (0, 0, 255)];
    for c in solid {
        for i in 0..strip.led_count {
            strip.set_px(i, c, 100);
        }
        strip.refresh();
        thread::sleep(Duration::from_millis(220));
    }
    for i in 0..strip.led_count {
        strip.clear();
        strip.set_px(i, (255, 255, 255), 100);
        strip.refresh();
        thread::sleep(Duration::from_millis(45));
    }
    strip.clear();
    strip.refresh();
}

fn hex2rgb(c: u32) -> (u8, u8, u8) {
    ((c >> 16) as u8, (c >> 8) as u8, c as u8)
}

fn update(strip: &Strip) {
    let t = *usb::TELEM.lock().unwrap();
    let bright = state::with(|s| s.brightness);
    let cfg = RevCfg::default();
    let car_guard = CAR.lock().unwrap();
    let car_default;
    let car = match car_guard.as_ref() {
        Some(c) => c,
        None => {
            car_default = CarData::default();
            &car_default
        }
    };
    let ms = now_ms();
    for i in 0..strip.rev_count {
        let rgb = hex2rgb(segment_rgb(&t, i, strip.rev_count, &cfg, car, ms));
        strip.set_px(i, rgb, bright);
    }
    let off = (0u8, 0u8, 0u8);
    for i in 0..strip.tc_count {
        strip.set_px(strip.tc_first + i, if t.tc_active > 0 { C_TC } else { off }, bright);
    }
    for i in 0..strip.abs_count {
        strip.set_px(strip.abs_first + i, if t.abs_active > 0 { C_ABS } else { off }, bright);
    }
    strip.refresh();
}

/// Spawn the LED task: init the strip, restore any persisted car, run the
/// boot self-test, then drive the strip from telemetry at ~30 Hz.
pub fn spawn() {
    thread::Builder::new()
        .stack_size(4096)
        .name("led".into())
        .spawn(|| {
            let strip = match init_strip() {
                Some(s) => s,
                None => return,
            };
            // Re-apply the saved car profile so the strip uses it right after boot
            // (the dashboard also re-pushes the live @C on reconnect as a backstop).
            let cj = state::with(|s| s.car_json.clone());
            if !cj.is_empty() {
                apply_car_json(&cj);
            }
            selftest(&strip);
            loop {
                if state::SLEEPING.load(std::sync::atomic::Ordering::Relaxed) {
                    strip.clear();
                    strip.refresh();
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
                update(&strip);
                thread::sleep(Duration::from_millis(33));
            }
        })
        .expect("spawn led task");
}
