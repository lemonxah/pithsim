//! pith-recovery — the Pith DDU recovery app.
//!
//! Lives in the `factory` partition and boots FIRST on every reset. It brings up
//! the panels + touch, shows a countdown ("N seconds till boot", tap to enter
//! recovery), then **chain-loads the main firmware** (sets the boot partition to
//! the active OTA slot + resets into it). The main firmware sets the boot partition
//! back to `factory` early in its boot, so the next reset returns here — recovery
//! is always the front door, even if the main firmware is bricked.
//!
//! The display + touch bring-up mirrors the firmware's `display.rs` (same SPI bus,
//! shared DC, ST7796 panels, XPT2046 touch) — deliberately self-contained so the
//! recovery image shares no code that could carry the main firmware's bug.

mod fat12;

use std::cell::RefCell;
use std::rc::Rc;
use std::thread::sleep;
use std::time::Duration;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Pixel, RgbColor, Size};
use embedded_graphics::primitives::Rectangle;

use esp_idf_svc::hal::delay::Ets;
use esp_idf_svc::hal::gpio::{AnyIOPin, Output, PinDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::spi::{
    config::Config as SpiConfig, Dma, SpiDeviceDriver, SpiDriver, SpiDriverConfig,
};
use esp_idf_svc::hal::units::FromValueType;
use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs};
use esp_idf_svc::sys;

use mipidsi::interface::SpiInterface;
use mipidsi::models::ST7796;
use mipidsi::options::{ColorInversion, ColorOrder, Orientation, Rotation};
use mipidsi::Builder;

const W: i32 = 480;
const H: i32 = 320;
const NS: &str = "dash"; // shared NVS namespace with the main firmware

// Stock XIAO-S3 DDU wiring (mirrors firmware DevicePins::default).
const SCLK: i32 = 7;
const MOSI: i32 = 9;
const MISO: i32 = 8;
const DC: i32 = 2;
const DISP1_CS: i32 = 1;
const DISP2_CS: i32 = 3;
const TOUCH1_CS: i32 = 5;
const TOUCH2_CS: i32 = 6; // must be driven (deselected) or it corrupts the shared MISO
const BACKLIGHT: i32 = 4; // D3 / GPIO4 — panel backlight enable (active-high)
const LED_DIN: i32 = 43; // shift-light strip data (WS2812/SK6812)

// XPT2046 calibration (same as firmware display.rs; Y inverted).
const X_MIN: i32 = 300;
const X_MAX: i32 = 3900;
const Y_MIN: i32 = 3900;
const Y_MAX: i32 = 300;
const Z_PRESS: i32 = 250;

// ---- framebuffer (PSRAM RGB565), trimmed from firmware display.rs ----
struct FrameBuf {
    data: Vec<Rgb565>,
    w: i32,
    h: i32,
}
impl FrameBuf {
    fn new(w: i32, h: i32) -> Self {
        Self { data: vec![Rgb565::BLACK; (w * h) as usize], w, h }
    }
    #[inline]
    fn put(&mut self, x: i32, y: i32, c: Rgb565) {
        if x >= 0 && y >= 0 && x < self.w && y < self.h {
            self.data[(y * self.w + x) as usize] = c;
        }
    }
}
impl OriginDimensions for FrameBuf {
    fn size(&self) -> Size {
        Size::new(self.w as u32, self.h as u32)
    }
}
impl DrawTarget for FrameBuf {
    type Color = Rgb565;
    type Error = core::convert::Infallible;
    fn draw_iter<I>(&mut self, pixels: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Pixel<Rgb565>>,
    {
        for Pixel(p, c) in pixels {
            self.put(p.x, p.y, c);
        }
        Ok(())
    }
    fn fill_contiguous<I>(&mut self, area: &Rectangle, colors: I) -> Result<(), Self::Error>
    where
        I: IntoIterator<Item = Rgb565>,
    {
        let mut it = colors.into_iter();
        for y in area.top_left.y..area.top_left.y + area.size.height as i32 {
            for x in area.top_left.x..area.top_left.x + area.size.width as i32 {
                match it.next() {
                    Some(c) => self.put(x, y, c),
                    None => return Ok(()),
                }
            }
        }
        Ok(())
    }
    fn fill_solid(&mut self, area: &Rectangle, color: Rgb565) -> Result<(), Self::Error> {
        let x0 = area.top_left.x.max(0);
        let y0 = area.top_left.y.max(0);
        let x1 = (area.top_left.x + area.size.width as i32).min(self.w);
        let y1 = (area.top_left.y + area.size.height as i32).min(self.h);
        for y in y0..y1 {
            let row = (y * self.w) as usize;
            for x in x0..x1 {
                self.data[row + x as usize] = color;
            }
        }
        Ok(())
    }
    fn clear(&mut self, color: Rgb565) -> Result<(), Self::Error> {
        self.data.iter_mut().for_each(|p| *p = color);
        Ok(())
    }
}

macro_rules! blit {
    ($disp:expr, $fb:expr) => {{
        let _ = $disp.set_pixels(0, 0, (W - 1) as u16, (H - 1) as u16, $fb.data.iter().copied());
    }};
}

/// Blit only the rectangle (x0,y0)-(x1,y1) inclusive — dirty redraw, so the
/// countdown / a pressed button pushes a few KB instead of the whole frame.
macro_rules! blit_rect {
    ($disp:expr, $fb:expr, $x0:expr, $y0:expr, $x1:expr, $y1:expr) => {{
        let (x0, y0, x1, y1) = ($x0.max(0), $y0.max(0), $x1.min(W - 1), $y1.min(H - 1));
        if x1 >= x0 && y1 >= y0 {
            let w = $fb.w;
            let mut buf: Vec<Rgb565> = Vec::with_capacity(((x1 - x0 + 1) * (y1 - y0 + 1)) as usize);
            for yy in y0..=y1 {
                let base = (yy * w) as usize;
                for xx in x0..=x1 {
                    buf.push($fb.data[base + xx as usize]);
                }
            }
            let _ = $disp.set_pixels(x0 as u16, y0 as u16, x1 as u16, y1 as u16, buf.into_iter());
        }
    }};
}

/// Blank the shift-light strip (WS2812/SK6812). It powers on with a stale/garbage
/// frame on reboot, so recovery clears it immediately. Inits an RMT strip, writes
/// all-zero, then frees the channel (the LEDs latch off). A generous max_leds with
/// the wider SK6812 format guarantees every LED on either strip type gets zeros.
fn leds_off(gpio: i32) {
    unsafe {
        let mut cfg: sys::led_strip_config_t = core::mem::zeroed();
        cfg.strip_gpio_num = gpio;
        cfg.max_leds = 64;
        cfg.led_pixel_format = sys::led_pixel_format_t_LED_PIXEL_FORMAT_GRBW;
        cfg.led_model = sys::led_model_t_LED_MODEL_SK6812;
        let mut rmt: sys::led_strip_rmt_config_t = core::mem::zeroed();
        rmt.clk_src = sys::soc_periph_rmt_clk_src_t_RMT_CLK_SRC_DEFAULT;
        rmt.resolution_hz = 10_000_000;
        rmt.mem_block_symbols = 64;
        let mut h: sys::led_strip_handle_t = core::ptr::null_mut();
        if sys::led_strip_new_rmt_device(&cfg, &rmt, &mut h) == 0 && !h.is_null() {
            sys::led_strip_clear(h);
            sys::led_strip_refresh(h);
            sys::led_strip_del(h);
        }
    }
}

#[derive(Clone)]
struct SharedDc(Rc<RefCell<PinDriver<'static, AnyIOPin, Output>>>);
impl embedded_hal::digital::ErrorType for SharedDc {
    type Error = core::convert::Infallible;
}
impl embedded_hal::digital::OutputPin for SharedDc {
    fn set_low(&mut self) -> Result<(), core::convert::Infallible> {
        let _ = self.0.borrow_mut().set_low();
        Ok(())
    }
    fn set_high(&mut self) -> Result<(), core::convert::Infallible> {
        let _ = self.0.borrow_mut().set_high();
        Ok(())
    }
}

fn xpt_read<S: embedded_hal::spi::SpiDevice>(dev: &mut S, cmd: u8) -> u16 {
    let mut buf = [cmd, 0, 0];
    let _ = dev.transfer_in_place(&mut buf);
    ((((buf[1] as u16) << 8) | buf[2] as u16) >> 3) & 0x0FFF
}

/// Read the touch panel; returns screen coords if pressed (270°+flip mapping).
fn read_touch<S: embedded_hal::spi::SpiDevice>(dev: &mut S) -> Option<(i32, i32)> {
    let z1 = xpt_read(dev, 0xB0) as i32;
    let z2 = xpt_read(dev, 0xC0) as i32;
    let rx = xpt_read(dev, 0xD0) as i32;
    let ry = xpt_read(dev, 0x90) as i32;
    let z = z1 + 4095 - z2;
    // Diagnostic: log raw touch values (throttled) so we can see whether a press
    // reads sane mid-range values or garbage. Remove once touch is confirmed.
    {
        use std::sync::atomic::{AtomicU32, Ordering};
        static TICK: AtomicU32 = AtomicU32::new(0);
        let n = TICK.fetch_add(1, Ordering::Relaxed);
        if z >= Z_PRESS || n % 64 == 0 {
            log::info!("touch raw: z={z} z1={z1} z2={z2} rx={rx} ry={ry} (thr={Z_PRESS})");
        }
    }
    if z < Z_PRESS {
        return None;
    }
    // Reject floating / no-response reads: an unselected or disconnected XPT2046
    // MISO reads as the rail (0x000 or 0xFFF), which sails past Z_PRESS and looks
    // like a permanent press (the bug that traps recovery in its menu). A real
    // touch lands mid-range, well inside the calibration window.
    if !(50..=4045).contains(&rx) || !(50..=4045).contains(&ry) {
        return None;
    }
    let nx = ((rx - X_MIN) * 1000 / (X_MAX - X_MIN)).clamp(0, 1000);
    let ny = ((ry - Y_MIN) * 1000 / (Y_MAX - Y_MIN)).clamp(0, 1000);
    let sx = (ny * (W - 1) / 1000).clamp(0, W - 1);
    let sy = ((1000 - nx) * (H - 1) / 1000).clamp(0, H - 1);
    Some((sx, sy))
}

fn now_ms() -> i64 {
    unsafe { sys::esp_timer_get_time() / 1000 }
}

/// Slot label of the OTA partition we'll chain-load (for the splash).
fn slot_label(main_slot: u8) -> String {
    format!("ota_{main_slot}")
}

/// Expose the saved NVS config as a read-only USB drive: gather every known
/// blob key from BOTH homes (the big `nvsblob` partition first — where the main
/// firmware migrates the layout blobs — then the default `nvs`), build a FAT12
/// RAM disk, and start TinyUSB MSC. The disk buffer is leaked: MSC serves it
/// until the user reboots out of drive mode.
fn mount_nvs_as_usb(nvs: Option<&EspNvs<esp_idf_svc::nvs::NvsDefault>>) -> bool {
    // 8.3-clean names (.jsn, not .json — FAT12 extensions are 3 chars).
    const KEYS: &[(&str, &str)] = &[
        ("uijson", "ui.jsn"),
        ("edjson", "editor.jsn"),
        ("racejson", "race.jsn"),
        ("carjson", "car.jsn"),
        ("profjson", "profile.jsn"),
        ("buttonsjson", "buttons.jsn"),
        ("pinsjson", "pins.jsn"),
    ];
    let blob_nvs = esp_idf_svc::nvs::EspCustomNvsPartition::take("nvsblob")
        .ok()
        .and_then(|p| EspNvs::new(p, NS, false).ok());

    let mut buf = vec![0u8; 32768];
    let mut read_key = |key: &str| -> Option<Vec<u8>> {
        if let Some(b) = blob_nvs.as_ref() {
            if let Ok(Some(data)) = b.get_raw(key, &mut buf) {
                if !data.is_empty() {
                    return Some(data.to_vec());
                }
            }
        }
        if let Ok(Some(data)) = nvs?.get_raw(key, &mut buf) {
            if !data.is_empty() {
                return Some(data.to_vec());
            }
        }
        None
    };

    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for (key, fname) in KEYS {
        if let Some(data) = read_key(key) {
            files.push((fname.to_string(), data));
        }
    }
    let readme = format!(
        "Pith DDU saved configuration (read-only)\n\
         recovery v{}\n\n\
         Files are the device's NVS blobs as pushed by the dashboard:\n\
         ui/editor/race = screen layouts, car = shift-light profile,\n\
         pins = GPIO map. {} file(s) present.\n\
         Tap the device screen to exit and reboot.\n",
        env!("CARGO_PKG_VERSION"),
        files.len(),
    );
    files.push(("readme.txt".to_string(), readme.into_bytes()));

    let entries: Vec<fat12::FileEntry> = files
        .iter()
        .map(|(n, d)| fat12::FileEntry { name: n, data: d })
        .collect();
    let disk: &'static [u8] = Box::leak(fat12::build(&entries).into_boxed_slice());
    unsafe { sys::pith_msc_start(disk.as_ptr(), disk.len() as u32) }
}

/// Wipe the saved layout blobs so the main firmware boots clean. Clears BOTH
/// homes: the default `nvs` (legacy location) and the big `nvsblob` partition
/// the main firmware migrates the layout blobs to (absent on old tables —
/// best-effort).
fn reset_layout(nvs: &mut EspNvs<esp_idf_svc::nvs::NvsDefault>) {
    for k in ["uijson", "racejson", "edjson"] {
        let _ = nvs.set_raw(k, b"");
    }
    if let Ok(part) = esp_idf_svc::nvs::EspCustomNvsPartition::take("nvsblob") {
        if let Ok(mut blob) = EspNvs::new(part, NS, true) {
            for k in ["uijson", "racejson", "edjson"] {
                let _ = blob.set_raw(k, b"");
            }
        }
    }
}

/// Set the boot partition to the active main OTA slot and reset into it. The main
/// firmware points the boot partition back at `factory` early, so the next reset
/// returns to recovery.
fn chain_load_main(main_slot: u8) -> ! {
    unsafe {
        let subtype = if main_slot == 1 {
            sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_1
        } else {
            sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_0
        };
        let part = sys::esp_partition_find_first(
            sys::esp_partition_type_t_ESP_PARTITION_TYPE_APP,
            subtype,
            core::ptr::null(),
        );
        if part.is_null() {
            log::error!("main slot ota_{main_slot} not found — staying in recovery");
            // Fall through to a reset so we try again rather than hang.
        } else if sys::esp_ota_set_boot_partition(part) != 0 {
            log::error!("esp_ota_set_boot_partition(ota_{main_slot}) failed");
        }
        sleep(Duration::from_millis(80));
        sys::esp_restart();
    }
}

fn main() {
    sys::link_patches();
    esp_idf_svc::log::EspLogger::initialize_default();
    log::info!("pith-recovery v{} starting", env!("CARGO_PKG_VERSION"));

    // NVS (shared "dash" namespace): which OTA slot holds the main firmware + the
    // boot-fail counter the main firmware maintains (shown on the splash).
    let mut nvs = EspDefaultNvsPartition::take()
        .ok()
        .and_then(|p| EspNvs::new(p, NS, true).ok());
    let main_slot = nvs
        .as_ref()
        .and_then(|n| n.get_u8("mainslot").ok().flatten())
        .unwrap_or(0)
        .min(1);
    let prev_fails = nvs
        .as_ref()
        .and_then(|n| n.get_u8("bootfail").ok().flatten())
        .unwrap_or(0)
        .saturating_sub(1);

    // --- display + touch bring-up (shared SPI2 bus) ---
    let peripherals = Peripherals::take().expect("peripherals");

    // FIRST: blank the shift-light strip — it powers on bright with a stale frame
    // when the device reboots into recovery. Clear it before anything else.
    leds_off(LED_DIN);

    // Backlight enable (active-high, D3/GPIO4): drive it high right away so the
    // panels are lit + stable (a floating enable pin makes the screen flicker).
    // Bound for the whole run so the pin stays driven.
    let _backlight = PinDriver::output(unsafe { AnyIOPin::new(BACKLIGHT) }).map(|mut bl| {
        let _ = bl.set_high();
        bl
    });
    let driver = SpiDriver::new(
        peripherals.spi2,
        unsafe { AnyIOPin::new(SCLK) },
        unsafe { AnyIOPin::new(MOSI) },
        Some(unsafe { AnyIOPin::new(MISO) }),
        &SpiDriverConfig::new().dma(Dma::Auto(8192)),
    )
    .expect("spi bus");

    let lcd_cfg = SpiConfig::new().baudrate(60.MHz().into());
    let touch_cfg = SpiConfig::new().baudrate(2.MHz().into());
    let dc = SharedDc(Rc::new(RefCell::new(
        PinDriver::output(unsafe { AnyIOPin::new(DC) }).expect("dc"),
    )));
    let dev1 = SpiDeviceDriver::new(&driver, Some(unsafe { AnyIOPin::new(DISP1_CS) }), &lcd_cfg).expect("dev1");
    let dev2 = SpiDeviceDriver::new(&driver, Some(unsafe { AnyIOPin::new(DISP2_CS) }), &lcd_cfg).expect("dev2");
    let mut t1 = SpiDeviceDriver::new(&driver, Some(unsafe { AnyIOPin::new(TOUCH1_CS) }), &touch_cfg).expect("t1");
    // The second touch controller shares the SPI bus. Even though recovery only
    // reads t1, t2's CS must be a driven SPI-CS (deasserted high) — otherwise it
    // floats, the unselected XPT2046 randomly drives MISO, and t1 reads garbage
    // (the reason recovery touch didn't respond). Created + held, never read.
    let mut t2 = SpiDeviceDriver::new(&driver, Some(unsafe { AnyIOPin::new(TOUCH2_CS) }), &touch_cfg).expect("t2");

    let buf1: &'static mut [u8] = vec![0u8; 16384].leak();
    let buf2: &'static mut [u8] = vec![0u8; 16384].leak();
    let mut delay = Ets;
    // Default DDU orientation: 270° + horizontal flip, BGR colour order.
    let orient = Orientation::new().rotate(Rotation::Deg270).flip_horizontal();
    let mut disp1 = Builder::new(ST7796, SpiInterface::new(dev1, dc.clone(), buf1))
        .display_size(320, 480)
        .orientation(orient)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Normal)
        .init(&mut delay)
        .expect("disp1");
    let mut disp2 = Builder::new(ST7796, SpiInterface::new(dev2, dc.clone(), buf2))
        .display_size(320, 480)
        .orientation(orient)
        .color_order(ColorOrder::Bgr)
        .invert_colors(ColorInversion::Normal)
        .init(&mut delay)
        .expect("disp2");

    let mut fb = FrameBuf::new(W, H);
    let version = env!("CARGO_PKG_VERSION");
    let slot = slot_label(main_slot);

    // --- countdown (tap to enter recovery) ---
    // Dirty redraw: paint the static chrome once, then re-blit ONLY the countdown
    // line each second (no full-screen flicker). Touch on either panel enters.
    pith_bios::render_splash_chrome(&mut fb, version, &slot, prev_fails);
    blit!(disp1, fb);
    blit!(disp2, fb);
    let start = now_ms();
    const WINDOW_MS: i64 = 3000;
    let mut last_secs = -1;
    let mut entered = false;
    loop {
        let elapsed = now_ms() - start;
        if elapsed >= WINDOW_MS {
            break;
        }
        let secs = (((WINDOW_MS - elapsed) + 999) / 1000).max(1) as i32;
        if secs != last_secs {
            last_secs = secs;
            pith_bios::render_splash_countdown(&mut fb, secs);
            let (cx, cy, cw, ch) = pith_bios::SPLASH_CD_RECT;
            blit_rect!(disp1, fb, cx, cy, cx + cw - 1, cy + ch - 1);
            blit_rect!(disp2, fb, cx, cy, cx + cw - 1, cy + ch - 1);
        }
        if read_touch(&mut t1).is_some() || read_touch(&mut t2).is_some() {
            entered = true;
            break;
        }
        sleep(Duration::from_millis(30));
    }

    if !entered {
        chain_load_main(main_slot);
    }

    // --- recovery menu (touch-driven; all four buttons) ---
    // Wait for finger-up so the entering tap doesn't fall onto a button.
    while read_touch(&mut t1).is_some() || read_touch(&mut t2).is_some() {
        sleep(Duration::from_millis(15));
    }
    pith_bios::render_menu(&mut fb, version, &slot, prev_fails);
    blit!(disp1, fb);
    blit!(disp2, fb);

    // Press feedback + action. Finger DOWN moves the highlight to the button under
    // it; finger UP fires that button's action. The action must be read from
    // `pressed` BEFORE it's cleared — the previous version cleared it in the
    // highlight step (over==None on release), so releases never fired = dead buttons.
    let mut pressed: Option<usize> = None;
    loop {
        match read_touch(&mut t1).or_else(|| read_touch(&mut t2)) {
            Some((tx, ty)) => {
                // finger down: highlight the button under it (dirty-blit the change)
                let over = pith_bios::menu_button_at(tx, ty);
                if over != pressed {
                    for b in [pressed, over].into_iter().flatten() {
                        pith_bios::draw_menu_button(&mut fb, b, Some(b) == over);
                        let (bx, by, bw, bh) = pith_bios::menu_button_rect(b);
                        blit_rect!(disp1, fb, bx, by, bx + bw - 1, by + bh - 1);
                        blit_rect!(disp2, fb, bx, by, bx + bw - 1, by + bh - 1);
                    }
                    pressed = over;
                }
            }
            None => {
                // finger up: run the action for the button that was held (if any)
                if let Some(b) = pressed.take() {
                    pith_bios::draw_menu_button(&mut fb, b, false);
                    let (bx, by, bw, bh) = pith_bios::menu_button_rect(b);
                    blit_rect!(disp1, fb, bx, by, bx + bw - 1, by + bh - 1);
                    blit_rect!(disp2, fb, bx, by, bx + bw - 1, by + bh - 1);
                    match pith_bios::menu_action(b) {
                        pith_bios::Action::Boot => chain_load_main(main_slot),
                        pith_bios::Action::ResetConfig => {
                            if let Some(n) = nvs.as_mut() {
                                reset_layout(n);
                            }
                            chain_load_main(main_slot);
                        }
                        pith_bios::Action::MountUsb => {
                            // Read-only USB drive of the saved config: build a
                            // FAT12 RAM disk from the NVS blobs and serve it via
                            // TinyUSB MSC (pith_msc). One-way trip: USB stays up
                            // until the user taps to reboot.
                            let ok = mount_nvs_as_usb(nvs.as_ref());
                            pith_bios::render_message(
                                &mut fb,
                                "USB DRIVE",
                                if ok {
                                    "connected read-only - tap to reboot"
                                } else {
                                    "USB bring-up failed - tap to reboot"
                                },
                            );
                            blit!(disp1, fb);
                            blit!(disp2, fb);
                            sleep(Duration::from_millis(400));
                            while read_touch(&mut t1).is_none() && read_touch(&mut t2).is_none() {
                                sleep(Duration::from_millis(30));
                            }
                            unsafe {
                                sleep(Duration::from_millis(80));
                                sys::esp_restart();
                            }
                        }
                        pith_bios::Action::Download => {
                            // Software route into the ROM's USB download mode —
                            // same state as holding BOOT while tapping RESET, no
                            // buttons needed: set RTC_CNTL_FORCE_DOWNLOAD_BOOT
                            // (RTC_CNTL_OPTION1_REG = 0x6000812C, bit 0 — from
                            // esp-idf soc/esp32s3/rtc_cntl_reg.h) and restart.
                            // The chip re-enumerates as Espressif's ROM loader;
                            // flash from the PC (pith-flash/espflash), which
                            // resets it back into normal boot when done. A power
                            // cycle also exits (the bit doesn't survive one).
                            pith_bios::render_message(
                                &mut fb,
                                "USB FLASH MODE",
                                "entering download mode - flash from the PC",
                            );
                            blit!(disp1, fb);
                            blit!(disp2, fb);
                            log::warn!("entering ROM USB download mode (software DFU)");
                            sleep(Duration::from_millis(300));
                            unsafe {
                                const RTC_CNTL_OPTION1_REG: *mut u32 = 0x6000_812C as *mut u32;
                                const RTC_CNTL_FORCE_DOWNLOAD_BOOT: u32 = 1 << 0;
                                core::ptr::write_volatile(RTC_CNTL_OPTION1_REG, RTC_CNTL_FORCE_DOWNLOAD_BOOT);
                                sys::esp_restart();
                            }
                        }
                        pith_bios::Action::Reboot => unsafe {
                            sleep(Duration::from_millis(80));
                            sys::esp_restart();
                        },
                    }
                }
            }
        }
        sleep(Duration::from_millis(20));
    }
}
