//! Handbrake integration: callback wiring + the background device loop that
//! owns the handbrake's HID handle (a separate USB device from the DDU, with
//! its own connection lifecycle). Ported from the standalone pith-hb
//! dashboard; state is mirrored into the `Hb` Slint global and the wizard's
//! sub-page routing lives in `HbPage` (independent of the app-level `Page`).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use slint::ComponentHandle;

use pith_hb_core::proto;
use pith_device::{device_present, Handbrake, PID_HANDBRAKE, PITH_VID};

use crate::ctx::Ctx;
use crate::ui_bridge::sstr;
use crate::{AppWindow, Hb, HbPage};

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

    hb.set_device_found(device_present(PITH_VID, PID_HANDBRAKE));
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
                ctx.ui_run(move |u| {
                    u.global::<Hb>().set_device_found(found);
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
    ctx.ui_run(move |u| {
        let hb = u.global::<Hb>();
        hb.set_board(sstr(&board));
        hb.set_fw_version(sstr(&fw));
        hb.set_serial(sstr(&serial));
        hb.set_connected(true);
        hb.set_conn_detail(sstr(""));
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
    }
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
