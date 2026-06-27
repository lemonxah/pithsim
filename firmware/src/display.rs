//! Dual ST7796 SPI displays + XPT2046 touch on the shared SPI2 bus, and the
//! render/interaction loop. One panel shows the @RS race screen (or the config
//! screen), the other the @BS button box. Drawing is direct to the mipidsi
//! displays (double-buffering is a later refinement). Touch drives HID buttons,
//! page changes, brightness, sim toggle and reboot.
//!
//! Hardware constraints mirrored from the legacy firmware: a single shared DC
//! pin across both panels (wrapped in Rc<RefCell> since the task is single-
//! threaded), and pushing display 2 before display 1 on the shared bus.

use std::cell::RefCell;
use std::rc::Rc;
use std::thread;
use std::time::Duration;

use esp_idf_svc::hal::delay::Ets;
use esp_idf_svc::hal::gpio::{AnyIOPin, Output, PinDriver};
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::hal::spi::{
    config::Config as SpiConfig, Dma, SpiDeviceDriver, SpiDriver, SpiDriverConfig,
};
use esp_idf_svc::hal::units::FromValueType;
use mipidsi::interface::SpiInterface;
use mipidsi::models::ST7796;
use mipidsi::options::{ColorInversion, Orientation, Rotation};
use mipidsi::Builder;

use embedded_graphics::pixelcolor::Rgb565;
use embedded_graphics::prelude::{DrawTarget, OriginDimensions, Pixel, RgbColor, Size};
use embedded_graphics::primitives::Rectangle;

use crate::{hid, ota, state, ui, usb};

/// An in-RAM RGB565 framebuffer (lives in PSRAM). The whole UI is drawn here —
/// fonts and fills hit RAM instead of issuing a windowed SPI write per glyph
/// pixel — then the full buffer is streamed to the panel in one DMA blit. This
/// is what makes redraws fast (direct-to-SPI font drawing was ~1 s/frame).
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

/// Stream a whole framebuffer to a panel in one windowed write (DMA). A macro
/// (not a generic fn) sidesteps naming the display's concrete reset-pin type.
macro_rules! blit {
    ($disp:expr, $fb:expr) => {{
        let _ = $disp.set_pixels(
            0,
            0,
            (ui::W - 1) as u16,
            (ui::H - 1) as u16,
            $fb.data.iter().copied(),
        );
    }};
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

// XPT2046 calibration (from the legacy lgfx setup): Y is inverted.
const X_MIN: i32 = 300;
const X_MAX: i32 = 3900;
const Y_MIN: i32 = 3900;
const Y_MAX: i32 = 300;
const Z_THRESH: u16 = 400;

/// Shared DC pin (one GPIO drives both panels). The display task is single-
/// threaded, so Rc<RefCell> is sufficient.
#[derive(Clone)]
struct SharedDc(Rc<RefCell<PinDriver<'static, AnyIOPin, Output>>>);
impl embedded_hal::digital::ErrorType for SharedDc {
    type Error = core::convert::Infallible; // DC toggles can't meaningfully fail
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
    (((buf[1] as u16) << 8) | buf[2] as u16) >> 3
}

/// Read the touch panel; returns screen coords if pressed.
fn read_touch<S: embedded_hal::spi::SpiDevice>(dev: &mut S) -> Option<(i32, i32)> {
    let z1 = xpt_read(dev, 0xB0);
    if z1 < Z_THRESH {
        return None;
    }
    let rx = xpt_read(dev, 0xD0) as i32; // X
    let ry = xpt_read(dev, 0x90) as i32; // Y
    let sx = ((rx - X_MIN) * ui::W / (X_MAX - X_MIN)).clamp(0, ui::W - 1);
    let sy = ((ry - Y_MIN) * ui::H / (Y_MAX - Y_MIN)).clamp(0, ui::H - 1);
    Some((sx, sy))
}

#[derive(PartialEq, Clone, Copy)]
enum RaceMode {
    Race,
    Config,
}

fn now_ms() -> i64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() / 1000 }
}

/// Build a mipidsi orientation from the runtime config (rotation 0..3 = 0/90/
/// 180/270°, plus optional mirroring), so a differently-mounted panel is a
/// config change rather than a recompile.
fn make_orientation(rot: u8, flip_h: bool, flip_v: bool) -> Orientation {
    let mut o = Orientation::new().rotate(match rot & 3 {
        0 => Rotation::Deg0,
        1 => Rotation::Deg90,
        2 => Rotation::Deg180,
        _ => Rotation::Deg270,
    });
    if flip_h {
        o = o.flip_horizontal();
    }
    if flip_v {
        o = o.flip_vertical();
    }
    o
}

fn display_task() {
    let peripherals = match Peripherals::take() {
        Ok(p) => p,
        Err(e) => {
            log::error!("peripherals take failed: {e:?}");
            return;
        }
    };
    let pins = state::with(|s| s.pins);

    let driver = match SpiDriver::new(
        peripherals.spi2,
        unsafe { AnyIOPin::new(pins.sclk) },
        unsafe { AnyIOPin::new(pins.mosi) },
        Some(unsafe { AnyIOPin::new(pins.miso) }),
        // DMA so big display flushes don't crawl on the CPU.
        &SpiDriverConfig::new().dma(Dma::Auto(8192)),
    ) {
        Ok(d) => d,
        Err(e) => {
            log::error!("spi bus: {e:?}");
            return;
        }
    };

    let lcd_cfg = SpiConfig::new().baudrate(40.MHz().into());
    let touch_cfg = SpiConfig::new().baudrate(2.MHz().into());

    let dc = SharedDc(Rc::new(RefCell::new(
        PinDriver::output(unsafe { AnyIOPin::new(pins.dc) }).expect("dc"),
    )));

    let dev1 = SpiDeviceDriver::new(&driver, Some(unsafe { AnyIOPin::new(pins.disp1_cs) }), &lcd_cfg).expect("dev1");
    let dev2 = SpiDeviceDriver::new(&driver, Some(unsafe { AnyIOPin::new(pins.disp2_cs) }), &lcd_cfg).expect("dev2");
    let mut t1 = SpiDeviceDriver::new(&driver, Some(unsafe { AnyIOPin::new(pins.touch1_cs) }), &touch_cfg).expect("t1");
    let mut t2 = SpiDeviceDriver::new(&driver, Some(unsafe { AnyIOPin::new(pins.touch2_cs) }), &touch_cfg).expect("t2");

    // Large per-display SPI scratch so mipidsi streams big chunks: 512 bytes was
    // only 256 px/flush, so a full redraw took ~600 transactions and you could
    // watch it paint. Heap-allocated (the task stack is just 12 KB); DMA above
    // makes each flush fast.
    let buf1: &'static mut [u8] = vec![0u8; 16384].leak();
    let buf2: &'static mut [u8] = vec![0u8; 16384].leak();
    let mut delay = Ets;
    let (drot, dfh, dfv) = state::with(|s| (s.disp_rot, s.disp_flip_h, s.disp_flip_v));
    let orient = make_orientation(drot, dfh, dfv);
    let mut disp1 = Builder::new(ST7796, SpiInterface::new(dev1, dc.clone(), buf1))
        .display_size(320, 480)
        .orientation(orient)
        .invert_colors(ColorInversion::Normal)
        .init(&mut delay)
        .expect("disp1");
    let mut disp2 = Builder::new(ST7796, SpiInterface::new(dev2, dc.clone(), buf2))
        .display_size(320, 480)
        .orientation(orient)
        .invert_colors(ColorInversion::Normal)
        .init(&mut delay)
        .expect("disp2");

    // race_screen pin = which physical panel index (0/1) shows the race screen.
    let race_is_1 = pins.race_screen == 0;

    let mut mode = RaceMode::Race;
    let mut last_touch_ms = now_ms();
    let mut page: usize = 0;
    let mut toggle_on = [false; 32];
    let mut prev_btn_down = false;
    let mut prev_d1_down = false;

    // pith-ui dirty-rect state (one cache per physical panel). The active layout is
    // cloned locally and only refreshed when the dashboard pushes a new UiDoc
    // (tracked via ui_ver), so the hot loop never re-parses or re-clones it.
    let mut race_cache = pith_ui::RenderCache::new();
    let mut side_cache = pith_ui::RenderCache::new();
    let mut local_doc: Option<pith_ui::UiDoc> = state::with(|s| s.ui_doc.clone());
    let mut last_ui_ver = state::with(|s| s.ui_ver);
    let mut last_mode = mode;
    let mut last_disp_ver = state::with(|s| s.disp_ver);

    // Two PSRAM framebuffers, one per physical panel. The UI draws into RAM
    // (fonts + fills never touch SPI) and each panel is flushed in a single DMA
    // blit. fb1 -> disp1, fb2 -> disp2.
    let mut fb1 = FrameBuf::new(ui::W, ui::H);
    let mut fb2 = FrameBuf::new(ui::W, ui::H);

    loop {
        let t = *usb::TELEM.lock().unwrap();
        let now = now_ms();

        if ota::ACTIVE.load(std::sync::atomic::Ordering::Relaxed) {
            let pct = ota_pct();
            ui::render_ota(&mut fb1, pct);
            blit!(disp1, fb1);
            blit!(disp2, fb1);
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        // Re-apply display orientation live when the dashboard changes it (@DO).
        let cur_disp_ver = state::with(|s| s.disp_ver);
        if cur_disp_ver != last_disp_ver {
            last_disp_ver = cur_disp_ver;
            let (r, fh, fv) = state::with(|s| (s.disp_rot, s.disp_flip_h, s.disp_flip_v));
            let o = make_orientation(r, fh, fv);
            let _ = disp1.set_orientation(o);
            let _ = disp2.set_orientation(o);
        }

        // Refresh the cached pith-ui layout only when the dashboard pushes a new one,
        // and force a full repaint of both panels on the next frame.
        let cur_ver = state::with(|s| s.ui_ver);
        if cur_ver != last_ui_ver {
            local_doc = state::with(|s| s.ui_doc.clone());
            last_ui_ver = cur_ver;
            race_cache.invalidate();
            side_cache.invalidate();
        }

        // Parse the pushed layouts each frame (cheap; could cache on change).
        let race_json = state::with(|s| s.race_json.clone());
        let btn_json = state::with(|s| s.buttons_json.clone());
        let buttons = ui::parse_buttons(&btn_json).unwrap_or_default();

        // --- touch: race panel (config nav / slider / sim / reboot) ---
        let race_touch = if race_is_1 { read_touch(&mut t1) } else { read_touch(&mut t2) };
        if let Some((tx, ty)) = race_touch {
            last_touch_ms = now;
            if !prev_d1_down {
                prev_d1_down = true;
                handle_race_touch(&mut mode, tx, ty);
            }
        } else {
            prev_d1_down = false;
        }
        if mode == RaceMode::Config && now - last_touch_ms > 8000 {
            mode = RaceMode::Race; // auto-return
        }
        // A mode switch changes the whole race panel -> force a full repaint.
        if mode != last_mode {
            race_cache.invalidate();
            last_mode = mode;
        }

        // --- touch: button panel ---
        let btn_touch = if race_is_1 { read_touch(&mut t2) } else { read_touch(&mut t1) };
        handle_button_touch(&buttons, &mut page, &mut toggle_on, &mut prev_btn_down, btn_touch);

        // A screen from the active UiDoc is selected by display index (0 = race
        // panel, 1 = side panel). Absent -> fall back to the legacy renderers.
        // Each panel is drawn into its framebuffer, then blitted in one DMA write.
        // --- render: side/button panel first (shared-bus ordering) ---
        {
            let fb = if race_is_1 { &mut fb2 } else { &mut fb1 };
            let side_scr = local_doc
                .as_ref()
                .and_then(|d| d.screens.iter().find(|s| s.display == 1));
            if let Some(scr) = side_scr {
                pith_ui::render_screen_diff(scr, &t, now, &mut side_cache, fb);
            } else {
                ui::render_buttons(fb, &buttons, page, &t, &toggle_on);
            }
            if race_is_1 {
                blit!(disp2, fb2);
            } else {
                blit!(disp1, fb1);
            }
        }
        // --- render: race panel ---
        {
            let fb = if race_is_1 { &mut fb1 } else { &mut fb2 };
            match mode {
                RaceMode::Config => {
                    let (b, sim) = state::with(|s| (s.brightness, s.sim_on));
                    ui::render_config(fb, b, sim);
                }
                RaceMode::Race => {
                    let race_scr = local_doc
                        .as_ref()
                        .and_then(|d| d.screens.iter().find(|s| s.display == 0));
                    if let Some(scr) = race_scr {
                        pith_ui::render_screen_diff(scr, &t, now, &mut race_cache, fb);
                    } else {
                        let layout = ui::parse_race(&race_json).unwrap_or_default();
                        ui::render_race(fb, &layout, &t, now);
                    }
                }
            }
            if race_is_1 {
                blit!(disp1, fb1);
            } else {
                blit!(disp2, fb2);
            }
        }

        thread::sleep(Duration::from_millis(33));
    }
}

fn handle_race_touch(mode: &mut RaceMode, tx: i32, ty: i32) {
    match mode {
        RaceMode::Race => {
            // Top-left hotspot opens config.
            if ui::hit((0, 0, 80, 60), tx, ty) {
                *mode = RaceMode::Config;
            }
        }
        RaceMode::Config => {
            if ui::hit(ui::BACK_BTN, tx, ty) {
                *mode = RaceMode::Race;
            } else if ui::hit(ui::SLD, tx, ty) {
                let pct = ((tx - ui::SLD.0) * 100 / ui::SLD.2).clamp(0, 100);
                state::with(|s| s.set_brightness(pct));
            } else if ui::hit(ui::SIM_BTN, tx, ty) {
                state::with(|s| s.sim_on = !s.sim_on);
            } else if ui::hit(ui::RBT_BTN, tx, ty) {
                thread::sleep(Duration::from_millis(150));
                unsafe { esp_idf_svc::sys::esp_restart() };
            }
        }
    }
}

fn handle_button_touch(
    buttons: &ui::Buttons,
    page: &mut usize,
    toggle_on: &mut [bool; 32],
    prev_down: &mut bool,
    touch: Option<(i32, i32)>,
) {
    match touch {
        Some((tx, ty)) => {
            if *prev_down {
                return; // edge-triggered
            }
            *prev_down = true;
            // tab bar?
            if ty < ui::TABH {
                let np = buttons.pages.len().max(1) as i32;
                let tw = ui::W / np;
                let p = (tx / tw).clamp(0, np - 1) as usize;
                *page = p;
                return;
            }
            if let Some(pg) = buttons.pages.get(*page) {
                for b in pg {
                    let r = ui::button_rect(b.hid % 8);
                    if ui::hit(r, tx, ty) {
                        if b.toggle {
                            let on = !toggle_on[b.hid.min(31)];
                            toggle_on[b.hid.min(31)] = on;
                            hid::set(b.hid, on);
                        } else {
                            hid::pulse(b.hid);
                        }
                    }
                }
            }
        }
        None => {
            *prev_down = false;
        }
    }
}

fn ota_pct() -> i32 {
    // ota module tracks progress internally; expose a coarse value.
    ota::progress_pct()
}

/// Spawn the display + touch + UI task.
pub fn spawn() {
    thread::Builder::new()
        .stack_size(12288)
        .name("display".into())
        .spawn(display_task)
        .expect("spawn display task");
}
