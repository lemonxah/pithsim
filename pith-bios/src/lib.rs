#![no_std]
//! pith-bios — the Pith DDU's on-device BIOS / early-boot recovery UI.
//!
//! A tiny, dependency-light recovery console: an early **boot splash** (which
//! firmware image + flash slot is loading, with a tap-to-enter countdown) and a
//! touch-driven **recovery menu**. It only *draws* — via pith-ui's shared `text` /
//! `fill_round` primitives, so it matches the rest of the UI — and *maps taps to
//! actions*; the firmware owns the boot flow and every side-effect (touch reads,
//! NVS, reboot, OTA). Kept out of the big UI engine so the recovery path stays
//! minimal and robust.

extern crate alloc;

use embedded_graphics::{pixelcolor::Rgb565, prelude::*};
use pith_ui::{fill_round, pal, text, HorizontalAlignment, Pal, VerticalPosition};

/// The recovery UI is fixed to the 480×320 panel.
const W: i32 = 480;
const H: i32 = 320;

/// A recovery action chosen by a tap in the menu. The caller (recovery app or the
/// firmware's in-app BIOS) executes it in a context-appropriate way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Boot the main firmware now (recovery: chain-load the main slot; in-app:
    /// continue into the normal UI).
    Boot,
    /// Wipe the saved layout, then boot/reboot clean.
    ResetConfig,
    /// Expose the NVS contents as a USB drive (read-only) so the host can see the
    /// locally-stored config files. Recovery-app only.
    MountUsb,
    /// Restart into the ROM's USB download (bootloader/DFU) mode so the host can
    /// flash over USB without touching the physical BOOT+RESET buttons.
    Download,
    /// Reboot now (returns to recovery).
    Reboot,
}

// Menu button rects (x, y, w, h); each maps to an accent colour + action.
const BOOT: (i32, i32, i32, i32) = (60, 94, 360, 38);
const RESET: (i32, i32, i32, i32) = (60, 136, 360, 38);
const MOUNT: (i32, i32, i32, i32) = (60, 178, 360, 38);
const DOWNLOAD: (i32, i32, i32, i32) = (60, 220, 360, 38);
const REBOOT: (i32, i32, i32, i32) = (60, 262, 360, 38);

fn menu_def(i: usize) -> ((i32, i32, i32, i32), &'static str, Pal, Action) {
    match i {
        0 => (BOOT, "Boot firmware", Pal::Green, Action::Boot),
        1 => (RESET, "Reset config", Pal::Amber, Action::ResetConfig),
        2 => (MOUNT, "Mount as USB drive", Pal::Cyan, Action::MountUsb),
        3 => (DOWNLOAD, "USB flash mode (download)", Pal::Red, Action::Download),
        _ => (REBOOT, "Reboot", Pal::White, Action::Reboot),
    }
}

/// Number of menu buttons.
pub fn menu_button_count() -> usize {
    5
}
/// Screen rect (x, y, w, h) of menu button `i` — so the caller can blit just it.
pub fn menu_button_rect(i: usize) -> (i32, i32, i32, i32) {
    menu_def(i).0
}
/// The [`Action`] for menu button `i`.
pub fn menu_action(i: usize) -> Action {
    menu_def(i).3
}

fn inside(r: (i32, i32, i32, i32), tx: i32, ty: i32) -> bool {
    tx >= r.0 && tx < r.0 + r.2 && ty >= r.1 && ty < r.1 + r.3
}

/// The menu button index under (tx, ty), if any.
pub fn menu_button_at(tx: i32, ty: i32) -> Option<usize> {
    (0..menu_button_count()).find(|&i| inside(menu_def(i).0, tx, ty))
}

/// Map a tap to a recovery [`Action`] (None if it missed every button).
pub fn action_at(tx: i32, ty: i32) -> Option<Action> {
    menu_button_at(tx, ty).map(menu_action)
}

/// Draw one menu button. When `pressed`, it fills with its accent colour (dark
/// label) so you can see which button your finger is on; otherwise a panel fill
/// with the accent as the label colour. Blit [`menu_button_rect`] after.
pub fn draw_menu_button<D: DrawTarget<Color = Rgb565>>(d: &mut D, i: usize, pressed: bool) {
    let (r, label, accent, _) = menu_def(i);
    let (bg, fg) = if pressed {
        (pal(accent), Rgb565::BLACK)
    } else {
        (pal(Pal::Panel), pal(accent))
    };
    fill_round(d, r.0, r.1, r.2, r.3, 8, bg);
    text(d, label, r.0 + r.2 / 2, r.1 + r.3 / 2, 16, fg, HorizontalAlignment::Center, VerticalPosition::Center);
}

/// Countdown line region on the splash — so the caller blits only this each second
/// instead of the whole screen (no flicker).
pub const SPLASH_CD_RECT: (i32, i32, i32, i32) = (20, 190, 440, 72);

/// Splash chrome: everything EXCEPT the changing countdown line. Draw once.
pub fn render_splash_chrome<D: DrawTarget<Color = Rgb565>>(d: &mut D, version: &str, slot: &str, prev_fails: u8) {
    let _ = d.clear(pal(Pal::Bg));
    text(d, "PITH DDU", W / 2, 84, 40, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
    let v = alloc::format!("recovery  -  v{version}  ({slot})");
    text(d, &v, W / 2, 128, 14, pal(Pal::Cyan), HorizontalAlignment::Center, VerticalPosition::Center);
    if prev_fails > 0 {
        let m = alloc::format!("previous boot failed {prev_fails}x");
        text(d, &m, W / 2, 160, 12, pal(Pal::Amber), HorizontalAlignment::Center, VerticalPosition::Center);
    }
    text(d, "tap screen for recovery", W / 2, 250, 12, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
}

/// Just the countdown number line: erase + redraw within [`SPLASH_CD_RECT`].
pub fn render_splash_countdown<D: DrawTarget<Color = Rgb565>>(d: &mut D, secs_left: i32) {
    let r = SPLASH_CD_RECT;
    fill_round(d, r.0, r.1, r.2, r.3, 0, pal(Pal::Bg)); // erase (radius 0 = plain rect)
    let unit = if secs_left == 1 { "second" } else { "seconds" };
    let c = alloc::format!("{secs_left} {unit} till boot");
    text(d, &c, W / 2, 214, 24, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
}

/// Combined splash (full redraw) — convenience for the firmware's in-app BIOS.
pub fn render_splash<D: DrawTarget<Color = Rgb565>>(d: &mut D, version: &str, slot: &str, secs_left: i32, prev_fails: u8) {
    render_splash_chrome(d, version, slot, prev_fails);
    render_splash_countdown(d, secs_left);
}

/// The recovery menu (full draw). Touch-driven — dispatch taps via [`action_at`];
/// for press feedback redraw a single button with [`draw_menu_button`].
pub fn render_menu<D: DrawTarget<Color = Rgb565>>(d: &mut D, version: &str, slot: &str, prev_fails: u8) {
    let _ = d.clear(pal(Pal::Bg));
    text(d, "RECOVERY", W / 2, 28, 18, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
    let v = alloc::format!("v{version}   ({slot})");
    text(d, &v, W / 2, 54, 12, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
    if prev_fails > 0 {
        let m = alloc::format!("boot failed {prev_fails}x  -  pick a recovery option");
        text(d, &m, W / 2, 82, 12, pal(Pal::Amber), HorizontalAlignment::Center, VerticalPosition::Center);
    } else {
        text(d, "setup / recovery", W / 2, 82, 12, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
    }
    for i in 0..menu_button_count() {
        draw_menu_button(d, i, false);
    }
    text(d, "USB flash mode = flash from the PC, no BOOT/RESET buttons", W / 2, 310, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
}

/// A simple centered two-line status / placeholder screen.
pub fn render_message<D: DrawTarget<Color = Rgb565>>(d: &mut D, title: &str, line: &str) {
    let _ = d.clear(pal(Pal::Bg));
    text(d, title, W / 2, H / 2 - 16, 24, pal(Pal::White), HorizontalAlignment::Center, VerticalPosition::Center);
    text(d, line, W / 2, H / 2 + 20, 14, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
    text(d, "tap to go back", W / 2, H - 24, 11, pal(Pal::Dim), HorizontalAlignment::Center, VerticalPosition::Center);
}
