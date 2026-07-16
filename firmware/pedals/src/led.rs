//! Onboard status LED — ONE WS2812 (GRB, 800 kHz) pixel on GPIO12
//! (`board::STATUS_LED`; `LED_GPIO_U8` in the reference firmware, which
//! drives it with Adafruit_NeoPixel). Driven via the espressif/led_strip RMT
//! component, the same setup as the DDU's rev strip.
//!
//! The LED is set to WHITE as the very first thing `main()` does, so it
//! doubles as a boot diagnostic that needs no UART adapter: dark = the app
//! never ran (bootloader/flash problem); white then dark = crash during
//! early init; a color with the once-a-second heartbeat blink = main loop
//! alive. Colors after boot:
//!   yellow = running, USB not mounted (no host / cable / enumeration issue)
//!   green  = running, USB mounted
//!   purple = OTA in progress
//!   red    = ARMED (motor output live — deliberately alarming)
//!
//! Every path here is best-effort: an LED failure logs and degrades to None,
//! it must never take the boot down with it.

use esp_idf_svc::sys;

pub const WHITE: (u8, u8, u8) = (40, 40, 40);
pub const YELLOW: (u8, u8, u8) = (50, 40, 0);
pub const GREEN: (u8, u8, u8) = (0, 50, 0);
pub const PURPLE: (u8, u8, u8) = (40, 0, 40);
pub const RED: (u8, u8, u8) = (60, 0, 0);
pub const OFF: (u8, u8, u8) = (0, 0, 0);

pub struct Led {
    handle: sys::led_strip_handle_t,
    cur: (u8, u8, u8),
}

/// Bring up the pixel and show `WHITE` ("main() reached"). None on failure.
pub fn init() -> Option<Led> {
    let mut cfg: sys::led_strip_config_t = unsafe { core::mem::zeroed() };
    cfg.strip_gpio_num = crate::board::STATUS_LED;
    cfg.max_leds = 1;
    cfg.led_pixel_format = sys::led_pixel_format_t_LED_PIXEL_FORMAT_GRB;
    cfg.led_model = sys::led_model_t_LED_MODEL_WS2812;

    let mut rmt: sys::led_strip_rmt_config_t = unsafe { core::mem::zeroed() };
    rmt.clk_src = sys::soc_periph_rmt_clk_src_t_RMT_CLK_SRC_DEFAULT;
    rmt.resolution_hz = 10_000_000;
    rmt.mem_block_symbols = 64;

    let mut handle: sys::led_strip_handle_t = core::ptr::null_mut();
    let err = unsafe { sys::led_strip_new_rmt_device(&cfg, &rmt, &mut handle) };
    if err != 0 || handle.is_null() {
        log::warn!("status LED init failed ({err}) — running without it");
        return None;
    }
    let mut led = Led { handle, cur: OFF };
    led.show(WHITE);
    Some(led)
}

impl Led {
    /// Set the pixel color; no-op (and no RMT traffic) when unchanged.
    pub fn show(&mut self, rgb: (u8, u8, u8)) {
        if rgb == self.cur {
            return;
        }
        self.cur = rgb;
        unsafe {
            sys::led_strip_set_pixel(self.handle, 0, rgb.0 as u32, rgb.1 as u32, rgb.2 as u32);
            sys::led_strip_refresh(self.handle);
        }
    }

    /// Main-loop status tick: pick the color for the current state and blink
    /// it off for 100 ms once a second — a visible heartbeat, so "alive" and
    /// "frozen showing a color" can't be confused.
    pub fn tick(&mut self, now_ms: i64, ota: bool, armed: bool, mounted: bool) {
        let base = if ota {
            PURPLE
        } else if armed {
            RED
        } else if mounted {
            GREEN
        } else {
            YELLOW
        };
        let on = (now_ms / 100) % 10 != 9;
        self.show(if on { base } else { OFF });
    }
}
