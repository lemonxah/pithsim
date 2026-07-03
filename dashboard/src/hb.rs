//! Handbrake integration: callback wiring + the background device loop that
//! owns the handbrake's HID handle (a separate USB device from the DDU, with
//! its own connection lifecycle). Ported from the standalone pith-hb
//! dashboard; state is mirrored into the `Hb` Slint global and the wizard's
//! sub-page routing lives in `HbPage` (independent of the app-level `Page`).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use pith_device::{device_present, Handbrake, Serial, PID_HANDBRAKE, PITH_VID};
use pith_hb_core::proto;

use crate::ctx::Ctx;
use crate::firmware::semver_cmp;
use crate::net::http::http_download_file;
use crate::paths::cache_dir;
use crate::ui_bridge::sstr;
use crate::{AppWindow, Hb, HbPage};

/// The handbrake's ROM download-mode USB identity (ESP32-S2 native bootloader,
/// enumerates as a CDC serial port — distinct from the app firmware's 0x8001).
const HB_BOOT_PID: &str = "0002";
const HB_BOOT_VID: &str = "303a";

const TELEM_TIMEOUT_MS: u64 = 100;
// ~8 misses * 100ms timeout = 800ms of telemetry silence before we bother
// checking the link is still alive — the firmware streams on every fresh HX711
// sample, which is virtually always, so a real gap this long means unplugged.
const MISS_LIMIT: u32 = 8;
const PRESENCE_SCAN_INTERVAL: Duration = Duration::from_millis(1000);
const AUTO_CAL_DURATION: Duration = Duration::from_secs(8);

/// A user-requested action for the handbrake device thread. Only the LATEST
/// matters: a fast deadzone-slider drag fires `SetDeadzone` many times a
/// second, and only the final position needs to reach the firmware — so this
/// is a single overwritable slot (a "latest-wins outbox"), not a queue.
pub enum HbOutbound {
    /// (Re)start the timed auto-calibration learn window.
    StartAutoCal,
    /// Abort a learn window in progress — must go through the device thread
    /// (not straight Slint navigation) so it actually stops the tracking;
    /// otherwise a finish landing after the user already left the screen
    /// would silently flip them back to the deadzone page.
    CancelAutoCal,
    SetDeadzone(u8, u8),
    SetInverted(bool),
    Save,
    Cancel,
    Reset,
    /// Stream this downloaded app image to the device over @OTA. Runs on the
    /// device thread because it needs exclusive use of the HID handle.
    OtaFile(std::path::PathBuf),
}

pub fn wire_hb_callbacks(ui: &AppWindow, ctx: &Arc<Ctx>) {
    let hb = ui.global::<Hb>();

    hb.on_auto_cal_start_requested({
        let c = ctx.clone();
        move || c.send_hb(HbOutbound::StartAutoCal)
    });

    hb.on_auto_cal_cancel_requested({
        let c = ctx.clone();
        move || c.send_hb(HbOutbound::CancelAutoCal)
    });

    hb.on_set_deadzone({
        let c = ctx.clone();
        move |lo, hi| {
            c.send_hb(HbOutbound::SetDeadzone(
                lo.clamp(0, 100) as u8,
                hi.clamp(0, 100) as u8,
            ))
        }
    });

    hb.on_set_inverted({
        let c = ctx.clone();
        move |inverted| c.send_hb(HbOutbound::SetInverted(inverted))
    });

    hb.on_save_requested({
        let c = ctx.clone();
        move || c.send_hb(HbOutbound::Save)
    });

    hb.on_cancel_requested({
        let c = ctx.clone();
        move || c.send_hb(HbOutbound::Cancel)
    });

    hb.on_reset_requested({
        let c = ctx.clone();
        move || c.send_hb(HbOutbound::Reset)
    });

    hb.on_flash_latest_requested({
        let c = ctx.clone();
        move || flash_hb_latest(&c)
    });

    hb.on_install_update_requested({
        let c = ctx.clone();
        move || install_hb_update(&c)
    });

    hb.set_device_found(device_present(PITH_VID, PID_HANDBRAKE));
}

/// Refresh the handbrake's "update available" state from the fetched releases
/// (the newest release that carries a pith-hb-*.bin asset) vs the firmware
/// version last reported by the connected handbrake's @CAP.
pub fn recompute_hb_update(ui: &AppWindow, s: &crate::state::State) {
    let hb = ui.global::<Hb>();
    let latest = s.hb_releases.first().map(|r| r.tag.clone());
    match latest {
        Some(tag) => {
            hb.set_fw_latest(sstr(&if tag.starts_with('v') {
                tag.clone()
            } else {
                format!("v{tag}")
            }));
            hb.set_update_available(!s.hb_fw.is_empty() && semver_cmp(&tag, &s.hb_fw) > 0);
        }
        None => {
            hb.set_fw_latest(sstr(""));
            hb.set_update_available(false);
        }
    }
}

/// Newest release carrying handbrake firmware, split by asset kind: the app
/// image (`pith-hb-<board>.bin`, streamed over @OTA) and the full merged
/// image (`pith-hb-<board>-full.bin`, written at 0x0 over the ROM bootloader
/// for recovery / one-time migration from the pre-OTA partition layout).
fn latest_hb_assets(s: &crate::state::State) -> Option<(String, Option<String>, Option<String>)> {
    let rel = s.hb_releases.first()?;
    let app = rel
        .hb_bin
        .iter()
        .find(|(b, _)| !b.ends_with("-full"))
        .map(|(_, u)| u.clone());
    let full = rel
        .hb_bin
        .iter()
        .find(|(b, _)| b.ends_with("-full"))
        .map(|(_, u)| u.clone());
    Some((rel.tag.clone(), app, full))
}

/// In-place update over USB HID: download the newest app image, then hand it
/// to the device thread to stream over @OTA. No bootloader dance — the device
/// flips slots and reboots itself; the connect loop picks it back up.
fn install_hb_update(ctx: &Arc<Ctx>) {
    let ui = match ctx.ui.upgrade() {
        Some(u) => u,
        None => return,
    };
    let hb = ui.global::<Hb>();
    if hb.get_flashing() {
        return;
    }
    if !hb.get_connected() {
        hb.set_flash_status(sstr("Handbrake not connected"));
        return;
    }
    let (tag, url) = {
        let s = ctx.lock();
        match latest_hb_assets(&s).and_then(|(tag, app, _)| app.map(|u| (tag, u))) {
            Some(x) => x,
            None => {
                hb.set_flash_status(sstr("No published handbrake firmware found"));
                return;
            }
        }
    };
    hb.set_flashing(true);
    hb.set_flash_progress(0.0);
    hb.set_flash_status(sstr(&format!("Downloading {tag}…")));

    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let out = cache_dir().join(format!("pith-hb-{tag}.bin"));
        let pc = ctx.clone();
        let ok = http_download_file(&url, &out, move |frac| {
            // download is the first quarter of the bar; the @OTA stream is the rest
            pc.ui_run(move |u| u.global::<Hb>().set_flash_progress(frac as f32 * 0.25));
        })
        .await;
        if !ok || !out.exists() {
            ctx.ui_run(|u| {
                let hb = u.global::<Hb>();
                hb.set_flashing(false);
                hb.set_flash_status(sstr("Download failed"));
            });
            return;
        }
        ctx.ui_run(|u| {
            u.global::<Hb>()
                .set_flash_status(sstr("Updating over USB…"));
        });
        ctx.send_hb(HbOutbound::OtaFile(out));
    });
}

/// The serial device path of a handbrake sitting in ROM download mode, if one
/// is plugged in right now.
fn hb_bootloader_port() -> Option<String> {
    Serial::list()
        .into_iter()
        .find(|p| p.vid == HB_BOOT_VID && p.pid == HB_BOOT_PID)
        .map(|p| p.device)
}

/// Download the newest pith-hb app image and flash it to the handbrake's
/// factory app partition over the ROM bootloader (espflash write-bin). The
/// device must already be in download mode — the connect sub-screen only
/// offers the button while the bootloader port is present.
fn flash_hb_latest(ctx: &Arc<Ctx>) {
    let ui = match ctx.ui.upgrade() {
        Some(u) => u,
        None => return,
    };
    let hb = ui.global::<Hb>();
    if hb.get_flashing() {
        return;
    }
    // Prefer the full merged image at 0x0 (bootloader + partition table +
    // app) — it recovers any device and performs the one-time migration to
    // the OTA partition layout. Releases predating it only ship the app
    // image, which matches the OLD single-factory table at 0x10000.
    let (tag, url, offset) = {
        let s = ctx.lock();
        match latest_hb_assets(&s).and_then(|(tag, app, full)| match (full, app) {
            (Some(u), _) => Some((tag, u, "0x0")),
            (None, Some(u)) => Some((tag, u, "0x10000")),
            (None, None) => None,
        }) {
            Some(x) => x,
            None => {
                hb.set_flash_status(sstr("No published handbrake firmware found"));
                return;
            }
        }
    };
    let port = match hb_bootloader_port() {
        Some(p) => p,
        None => {
            hb.set_flash_status(sstr("Bootloader not detected — hold BOOT, tap RESET first"));
            return;
        }
    };
    hb.set_flashing(true);
    hb.set_flash_progress(0.0);
    hb.set_flash_status(sstr(&format!("Downloading {tag}…")));

    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let out = cache_dir().join(format!("pith-hb-{tag}-{offset}.bin"));
        let pc = ctx.clone();
        let ok = http_download_file(&url, &out, move |frac| {
            // download is the first half of the progress bar
            pc.ui_run(move |u| u.global::<Hb>().set_flash_progress(frac as f32 * 0.5));
        })
        .await;
        if !ok || !out.exists() {
            ctx.ui_run(|u| {
                let hb = u.global::<Hb>();
                hb.set_flashing(false);
                hb.set_flash_status(sstr("Download failed"));
            });
            return;
        }
        ctx.ui_run(|u| {
            let hb = u.global::<Hb>();
            hb.set_flash_progress(0.5);
            hb.set_flash_status(sstr("Flashing over the ROM bootloader…"));
        });
        let output = tokio::process::Command::new("espflash")
            .args(["write-bin", offset, &out.to_string_lossy(), "--port", &port])
            .stdin(std::process::Stdio::null())
            .output()
            .await;
        // Surface the real failure, not a guess: espflash missing vs its own
        // last error line (port busy, wrong mode, etc.).
        let (ok, detail) = match &output {
            Ok(o) if o.status.success() => (true, String::new()),
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stderr);
                let last = text
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("espflash failed")
                    .trim()
                    .to_string();
                (false, last)
            }
            Err(_) => (
                false,
                "espflash not found — install it (cargo install espflash)".to_string(),
            ),
        };
        ctx.ui_run(move |u| {
            let hb = u.global::<Hb>();
            hb.set_flashing(false);
            hb.set_flash_progress(if ok { 1.0 } else { 0.0 });
            hb.set_flash_status(sstr(&if ok {
                "Flashed — tap RESET to boot the new firmware".to_string()
            } else {
                format!("Flash failed: {detail}")
            }));
        });
    });
}

/// Tracks an in-progress auto-calibration learn window: the observed raw
/// min/max seen so far, and the very first sample (the handbrake is assumed
/// to be at rest when the window starts) used to tell idle from max — both
/// wiring directions produce the same {min, max} pair, so we need a way to
/// know which end is "released".
struct AutoCal {
    active: bool,
    start: Instant,
    first: Option<i32>,
    lo: i32,
    hi: i32,
}

impl AutoCal {
    fn idle() -> Self {
        AutoCal {
            active: false,
            start: Instant::now(),
            first: None,
            lo: i32::MAX,
            hi: i32::MIN,
        }
    }

    fn restart(&mut self) {
        self.active = true;
        self.start = Instant::now();
        self.first = None;
        self.lo = i32::MAX;
        self.hi = i32::MIN;
    }
}

/// Owns the `Handbrake` (the only thread that touches its HID handle):
/// connects automatically as soon as a device is detected, drains the command
/// outbox, runs the auto-calibration learn window, and mirrors
/// telemetry/status to the UI.
pub fn hb_device_loop(ctx: Arc<Ctx>) {
    let mut dev = Handbrake::default();
    let mut last_scan = Instant::now() - PRESENCE_SCAN_INTERVAL;
    let mut miss = 0u32;
    let mut auto_cal = AutoCal::idle();

    while ctx.running.load(Ordering::SeqCst) {
        if !dev.is_open() {
            if last_scan.elapsed() >= PRESENCE_SCAN_INTERVAL {
                last_scan = Instant::now();
                let found = device_present(PITH_VID, PID_HANDBRAKE);
                // A handbrake in ROM download mode enumerates as a serial port
                // instead of the HID device — surface it so the connect screen
                // can offer the firmware-flash card.
                let boot = hb_bootloader_port().is_some();
                ctx.ui_run(move |u| {
                    let hb = u.global::<Hb>();
                    hb.set_device_found(found);
                    hb.set_bootloader_present(boot);
                });
                if found {
                    try_connect(&ctx, &mut dev);
                }
            }
            take_outbox(&ctx, Duration::from_millis(300)); // nothing to act on yet; just paces the scan
            continue;
        }

        if let Some(cmd) = take_outbox(&ctx, Duration::from_millis(0)) {
            handle_command(&ctx, &mut dev, &mut auto_cal, cmd);
            miss = 0;
            continue;
        }

        match dev.read_telem(TELEM_TIMEOUT_MS) {
            Some(t) => {
                miss = 0;
                on_telem(&ctx, &mut dev, &mut auto_cal, t);
            }
            None => {
                miss += 1;
                if miss >= MISS_LIMIT {
                    miss = 0;
                    if dev.status().is_none() {
                        dev.close();
                        auto_cal.active = false;
                        ctx.ui_run(|u| {
                            let hb = u.global::<Hb>();
                            hb.set_connected(false);
                            hb.set_conn_detail(sstr("Disconnected"));
                            hb.set_page(HbPage::Connect);
                        });
                    }
                }
            }
        }
    }
}

/// Pop the pending outbound command, waiting up to `timeout` for one to
/// arrive (0 = don't wait). See `HbOutbound` for why this is a single
/// overwritable slot rather than a queue.
fn take_outbox(ctx: &Ctx, timeout: Duration) -> Option<HbOutbound> {
    let (m, cv) = &*ctx.hb_out;
    let mut g = m.lock().unwrap();
    if g.is_some() {
        return g.take();
    }
    if timeout.is_zero() {
        return None;
    }
    let (mut g2, _timed_out) = cv.wait_timeout(g, timeout).unwrap();
    g2.take()
}

fn try_connect(ctx: &Arc<Ctx>, dev: &mut Handbrake) {
    if dev.connect() {
        if let Some(caps) = dev.capabilities() {
            apply_caps(ctx, &caps);
            if let Some(st) = dev.status() {
                let next = if st.calibrated {
                    HbPage::Monitor
                } else {
                    HbPage::AutoCalibrate
                };
                push_status(ctx, st);
                set_page(ctx, next);
            }
            return;
        }
    }
    dev.close();
    ctx.ui_run(|u| {
        u.global::<Hb>()
            .set_conn_detail(sstr("Connect failed — will retry"));
    });
}

fn apply_caps(ctx: &Arc<Ctx>, caps: &[(String, String)]) {
    let get = |k: &str| {
        caps.iter()
            .find(|(kk, _)| kk == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let board = get("board");
    let fw = get("fw");
    let serial = get("serial");
    ctx.lock().hb_fw = fw.clone();
    let c2 = ctx.clone();
    ctx.ui_run(move |u| {
        let hb = u.global::<Hb>();
        hb.set_board(sstr(&board));
        hb.set_fw_version(sstr(&fw));
        hb.set_serial(sstr(&serial));
        hb.set_connected(true);
        hb.set_conn_detail(sstr(""));
        hb.set_bootloader_present(false); // app firmware is running, not the ROM
        recompute_hb_update(&u, &c2.lock());
    });
}

fn push_status(ctx: &Arc<Ctx>, st: pith_device::handbrake::Status) {
    ctx.ui_run(move |u| {
        let hb = u.global::<Hb>();
        hb.set_raw(st.raw);
        hb.set_pct_x10(st.pct_x10 as i32);
        hb.set_idle_raw(st.idle_raw);
        hb.set_max_raw(st.max_raw);
        hb.set_deadzone_lo(st.deadzone_lo_pct as i32);
        hb.set_deadzone_hi(st.deadzone_hi_pct as i32);
        hb.set_inverted(st.inverted);
        hb.set_calibrated(st.calibrated);
    });
}

fn set_page(ctx: &Arc<Ctx>, page: HbPage) {
    ctx.ui_run(move |u| {
        u.global::<Hb>().set_page(page);
    });
}

fn set_error(ctx: &Arc<Ctx>, msg: &str) {
    let msg = msg.to_string();
    ctx.ui_run(move |u| {
        u.global::<Hb>().set_error(sstr(&msg));
    });
}

fn clear_error(ctx: &Arc<Ctx>) {
    ctx.ui_run(|u| {
        u.global::<Hb>().set_error(sstr(""));
    });
}

fn handle_command(ctx: &Arc<Ctx>, dev: &mut Handbrake, auto_cal: &mut AutoCal, cmd: HbOutbound) {
    clear_error(ctx);
    match cmd {
        HbOutbound::StartAutoCal => {
            auto_cal.restart();
            ctx.ui_run(|u| {
                let hb = u.global::<Hb>();
                hb.set_learn_active(true);
                hb.set_learn_min(0);
                hb.set_learn_max(0);
                hb.set_learn_remaining_s((AUTO_CAL_DURATION.as_secs()) as i32);
                hb.set_learn_progress(0.0);
            });
        }
        HbOutbound::CancelAutoCal => {
            auto_cal.active = false;
            ctx.ui_run(|u| {
                u.global::<Hb>().set_learn_active(false);
            });
            set_page(ctx, HbPage::Monitor);
        }
        HbOutbound::SetDeadzone(lo, hi) => {
            dev.set_deadzone(lo, hi);
            if let Some(st) = dev.status() {
                push_status(ctx, st);
            }
        }
        HbOutbound::SetInverted(inverted) => {
            dev.set_inverted(inverted);
            if let Some(st) = dev.status() {
                push_status(ctx, st);
            }
        }
        HbOutbound::Save => {
            if dev.save() {
                if let Some(st) = dev.status() {
                    push_status(ctx, st);
                }
                set_page(ctx, HbPage::Done);
            } else {
                set_error(ctx, "save failed");
            }
        }
        HbOutbound::Cancel => {
            dev.cancel();
            if let Some(st) = dev.status() {
                push_status(ctx, st);
            }
            set_page(ctx, HbPage::Monitor);
        }
        HbOutbound::Reset => {
            dev.reset();
            if let Some(st) = dev.status() {
                push_status(ctx, st);
            }
            set_page(ctx, HbPage::AutoCalibrate);
        }
        HbOutbound::OtaFile(path) => {
            let img = match std::fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    finish_flash(ctx, Err(format!("read image: {e}")));
                    return;
                }
            };
            let pc = ctx.clone();
            let r = dev.ota_upload(&img, move |pct| {
                // the @OTA stream is the remaining 3/4 of the progress bar
                let frac = 0.25 + (pct as f32 / 100.0) * 0.75;
                pc.ui_run(move |u| u.global::<Hb>().set_flash_progress(frac));
            });
            if r.is_ok() {
                // The device flips slots and reboots: drop our side so the
                // presence scan reconnects to the new firmware cleanly.
                dev.close();
                auto_cal.active = false;
                ctx.ui_run(|u| {
                    let hb = u.global::<Hb>();
                    hb.set_connected(false);
                    hb.set_conn_detail(sstr("Rebooting into the new firmware…"));
                    hb.set_page(HbPage::Connect);
                });
            }
            finish_flash(ctx, r);
        }
    }
}

/// Common tail of an @OTA attempt: clear the flashing state + surface result.
fn finish_flash(ctx: &Arc<Ctx>, r: Result<(), String>) {
    let msg = match &r {
        Ok(()) => "Update installed — device is rebooting".to_string(),
        Err(e) => format!("Update failed: {e}"),
    };
    let ok = r.is_ok();
    ctx.ui_run(move |u| {
        let hb = u.global::<Hb>();
        hb.set_flashing(false);
        hb.set_flash_progress(if ok { 1.0 } else { 0.0 });
        hb.set_flash_status(sstr(&msg));
    });
}

fn on_telem(ctx: &Arc<Ctx>, dev: &mut Handbrake, auto_cal: &mut AutoCal, t: proto::Telem) {
    let raw = t.raw;
    let pct = t.pct_x10 as i32;

    if !auto_cal.active {
        ctx.ui_run(move |u| {
            let hb = u.global::<Hb>();
            hb.set_raw(raw);
            hb.set_pct_x10(pct);
        });
        return;
    }

    if auto_cal.first.is_none() {
        auto_cal.first = Some(raw);
    }
    auto_cal.lo = auto_cal.lo.min(raw);
    auto_cal.hi = auto_cal.hi.max(raw);

    let elapsed = auto_cal.start.elapsed();
    if elapsed < AUTO_CAL_DURATION {
        let remaining_s = (AUTO_CAL_DURATION - elapsed).as_secs_f32().ceil() as i32;
        let progress = elapsed.as_secs_f32() / AUTO_CAL_DURATION.as_secs_f32();
        let (lo, hi) = (auto_cal.lo, auto_cal.hi);
        ctx.ui_run(move |u| {
            let hb = u.global::<Hb>();
            hb.set_raw(raw);
            hb.set_pct_x10(pct);
            hb.set_learn_min(lo);
            hb.set_learn_max(hi);
            hb.set_learn_remaining_s(remaining_s);
            hb.set_learn_progress(progress);
        });
        return;
    }

    // Window's up: whichever extreme is closer to the first sample (assumed
    // at-rest) is idle; the other is max. Works regardless of wiring polarity.
    let first = auto_cal.first.unwrap_or(raw);
    let (lo, hi) = (auto_cal.lo, auto_cal.hi);
    let (idle, max_raw) = if (first - lo).abs() <= (first - hi).abs() {
        (lo, hi)
    } else {
        (hi, lo)
    };

    let result = if dev.set_idle(idle) {
        dev.set_max(max_raw)
    } else {
        Err("comm".to_string())
    };

    match result {
        Ok(()) => {
            auto_cal.active = false;
            ctx.ui_run(|u| {
                u.global::<Hb>().set_learn_active(false);
            });
            if let Some(st) = dev.status() {
                push_status(ctx, st);
            }
            set_page(ctx, HbPage::Deadzone);
            clear_error(ctx);
        }
        Err(code) => {
            // Not enough range yet — keep trying automatically, no button needed.
            auto_cal.restart();
            set_error(ctx, &code);
        }
    }
}
