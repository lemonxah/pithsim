//! USB transport layer: drives the C shim (components/pith_usb) and implements
//! the device→Rust callbacks. Handles the HID report-id-2 command channel framing
//! ([len][payload], chunked replies) and CDC telemetry draining, then routes
//! complete lines to the command dispatcher.
//!
//! Phase 2a: channel plumbing + a minimal dispatcher (enough for the dashboard to
//! connect) — the full `@`-command set, OTA, and NVS land in 2b.

use std::ffi::CString;
use std::sync::Mutex;

use esp_idf_svc::sys;
use pith_core::simhub::{self, Telemetry};

/// Which transport a command arrived on; replies route back the same way.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Transport {
    Cdc,
    Hid,
}

const LINE_MAX: usize = 8192; // fits a full layout/profile JSON push

struct LineBuf {
    buf: Vec<u8>,
}
impl LineBuf {
    const fn new() -> Self {
        LineBuf { buf: Vec::new() }
    }
}

// Per-transport line accumulators so CDC telemetry and HID commands don't mix.
static CDC_LINE: Mutex<LineBuf> = Mutex::new(LineBuf::new());
static HID_LINE: Mutex<LineBuf> = Mutex::new(LineBuf::new());

// HID reply channel (report id 2), chunked into 61-byte payloads.
struct HidTx {
    buf: Vec<u8>,
    pos: usize,
}
static HID_TX: Mutex<HidTx> = Mutex::new(HidTx {
    buf: Vec::new(),
    pos: 0,
});

/// Latest parsed telemetry (written here, read by the LED/UI tasks later).
pub static TELEM: Mutex<Telemetry> = Mutex::new(telem_zero());

// const-fn zero so TELEM can be a `static Mutex` initializer.
const fn telem_zero() -> Telemetry {
    // SAFETY-free: all fields are integers; build a zeroed Telemetry. We can't
    // call Default in const, so list via core::mem::zeroed is not const either —
    // instead rely on Telemetry being all-ints and use a const constructor.
    Telemetry {
        gear: b'N',
        speed_kmh: 0, rpm: 0, max_rpm: 0, shift_rpm: 0,
        cur_lap_ms: 0, last_lap_ms: 0, best_lap_ms: 0, pb_lap_ms: 0, est_lap_ms: 0, delta_ms: 0,
        position: 0, field_size: 0, laps_done: 0, total_laps: 0, laps_left: 0,
        water_c: 0, oil_c: 0, oil_press_x10: 0, boost_kpa: 0, tc: 0, abs: 0, brake_bias_x10: 0,
        fuel_dl: 0, fuel_cap_dl: 0, fuel_per_lap_ml: 0, fuel_laps_x10: 0,
        tt_fl_i: 0, tt_fl_m: 0, tt_fl_o: 0, tt_fr_i: 0, tt_fr_m: 0, tt_fr_o: 0,
        tt_rl_i: 0, tt_rl_m: 0, tt_rl_o: 0, tt_rr_i: 0, tt_rr_m: 0, tt_rr_o: 0,
        tp_fl: 0, tp_fr: 0, tp_rl: 0, tp_rr: 0, tw_fl: 0, tw_fr: 0, tw_rl: 0, tw_rr: 0,
        bt_fl: 0, bt_fr: 0, bt_rl: 0, bt_rr: 0,
        throttle: 0, brake: 0, clutch: 0, steer: 0, tc_active: 0, abs_active: 0,
        headlights: 0, wipers: 0, pit_limiter: 0, ignition: 0, pos_x: 0, pos_z: 0,
        s1_ms: 0, s2_ms: 0, s3_ms: 0, bs1_ms: 0, bs2_ms: 0, bs3_ms: 0,
    }
}

/// Bring up the USB composite device (PHY + TinyUSB + device task).
pub fn init(serial: &str) {
    let c = CString::new(serial).unwrap_or_default();
    unsafe { sys::pith_usb_init(c.as_ptr()) };
}

pub fn mounted() -> bool {
    unsafe { sys::pith_usb_mounted() }
}

/// Drain the CDC RX FIFO and feed the telemetry line accumulator. Call often.
pub fn poll_cdc() {
    let mut tmp = [0u8; 256];
    loop {
        let n = unsafe { sys::pith_cdc_read(tmp.as_mut_ptr(), tmp.len() as i32) };
        if n <= 0 {
            break;
        }
        feed(Transport::Cdc, &tmp[..n as usize]);
    }
}

// ---- device -> Rust callbacks (called from the TinyUSB task) ----

/// HID OUT on report id 2: `buf` = `[N][payload…]` (length byte then N bytes).
#[no_mangle]
pub extern "C" fn pith_on_hid_cmd(buf: *const u8, len: i32) {
    if buf.is_null() || len < 1 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(buf, len as usize) };
    let n = (data[0] as usize).min(data.len() - 1);
    if n > 0 {
        feed(Transport::Hid, &data[1..1 + n]);
    }
}

/// An HID IN report finished — pump the next queued reply chunk.
#[no_mangle]
pub extern "C" fn pith_on_hid_tx_complete() {
    pump_hid_tx();
}

// ---- line accumulation + dispatch ----

fn feed(t: Transport, bytes: &[u8]) {
    // During an OTA the owning transport's bytes are raw image data, not lines.
    if crate::ota::ACTIVE.load(std::sync::atomic::Ordering::Relaxed)
        && crate::ota::feed(t, bytes)
    {
        return;
    }
    let lock = match t {
        Transport::Cdc => &CDC_LINE,
        Transport::Hid => &HID_LINE,
    };
    // Collect complete lines under the lock, dispatch after releasing it.
    let mut lines: Vec<String> = Vec::new();
    {
        let mut lb = lock.lock().unwrap();
        for &c in bytes {
            if c == b'\n' || c == b'\r' {
                if !lb.buf.is_empty() {
                    lines.push(String::from_utf8_lossy(&lb.buf).into_owned());
                    lb.buf.clear();
                }
                continue;
            }
            // '$' starts a fresh telemetry frame: flush any partial line first.
            if c == b'$' && !lb.buf.is_empty() {
                lines.push(String::from_utf8_lossy(&lb.buf).into_owned());
                lb.buf.clear();
            }
            if lb.buf.len() < LINE_MAX - 1 {
                lb.buf.push(c);
            } else {
                lb.buf.clear();
            }
        }
    }
    for line in lines {
        dispatch(t, &line);
    }
}

/// Full command dispatcher (everything except OTA, which is handled as raw bytes
/// in `feed`, Phase 2b-2). Command-prefix order mirrors the legacy firmware so
/// shared prefixes (@PINS/@P, @BS/@B, @CAP/@CM/@C, @RG/@RS, @SL/@S) resolve right.
fn dispatch(t: Transport, line: &str) {
    if line.is_empty() {
        return;
    }
    if line == "?" {
        reply(t, &status_line());
        return;
    }
    if let Some(rest) = line.strip_prefix("@PINS") {
        let ok = crate::state::with(|s| s.apply_pins(rest));
        reply(t, if ok { "OK\n" } else { "ERR\n" });
        return;
    }
    if let Some(rest) = line.strip_prefix("@P") {
        let ok = crate::state::with(|s| s.apply_profile(rest));
        reply(t, if ok { "OK\n" } else { "ERR\n" });
        return;
    }
    if let Some(rest) = line.strip_prefix("@BS") {
        let ok = crate::state::with(|s| s.apply_buttons(rest));
        reply(t, if ok { "OK\n" } else { "ERR\n" });
        return;
    }
    if let Some(rest) = line.strip_prefix("@B") {
        crate::state::with(|s| s.set_brightness(rest.trim().parse().unwrap_or(0)));
        reply(t, "OK\n");
        return;
    }
    if line.starts_with("@CAP") {
        let cap = crate::state::with(|s| s.cap_json(crate::device::serial()));
        reply(t, &cap);
        return;
    }
    if line == "@T" {
        reply(t, &telem_reply());
        return;
    }
    if line.starts_with("@RG") {
        // Must contain OK/READY AND the {json} body (the app checks both).
        let j = crate::state::with(|s| s.race_json.clone());
        let body = if j.is_empty() { "{}" } else { &j };
        reply(t, &format!("OK {body}\n"));
        return;
    }
    if let Some(rest) = line.strip_prefix("@RS") {
        let ok = crate::state::with(|s| s.apply_race(rest));
        reply(t, if ok { "OK\n" } else { "ERR\n" });
        return;
    }
    if line.starts_with("@UG") {
        // Read back the active pith-ui layout (JSON), like @RG.
        let j = crate::state::with(|s| s.ui_json.clone());
        let body = if j.is_empty() { "{}" } else { &j };
        reply(t, &format!("OK {body}\n"));
        return;
    }
    if let Some(rest) = line.strip_prefix("@UI") {
        let ok = crate::state::with(|s| s.apply_ui(rest));
        reply(t, if ok { "OK\n" } else { "ERR\n" });
        return;
    }
    if let Some(rest) = line.strip_prefix("@SL") {
        let ok = crate::led::apply_car_json(rest);
        if ok {
            crate::state::with(|s| s.apply_car(rest));
        }
        reply(t, if ok { "OK\n" } else { "ERR\n" });
        return;
    }
    if let Some(rest) = line.strip_prefix("@CM") {
        crate::state::with(|s| s.set_car_model(rest));
        reply(t, "OK\n");
        return;
    }
    if let Some(rest) = line.strip_prefix("@C") {
        let ok = crate::led::apply_car_json(rest);
        if ok {
            crate::state::with(|s| s.apply_car(rest));
        }
        reply(t, if ok { "OK\n" } else { "ERR\n" });
        return;
    }
    if let Some(rest) = line.strip_prefix("@OTA") {
        crate::ota::begin(t, rest.trim().parse().unwrap_or(0));
        return;
    }
    if line.starts_with("@S") {
        let (ok, bad) = crate::state::with(|s| {
            let r = (s.frames_ok, s.frames_bad);
            s.frames_ok = 0;
            s.frames_bad = 0;
            r
        });
        reply(t, &format!("ok={ok} bad={bad}\n"));
        return;
    }
    if line.starts_with('@') {
        reply(t, "OK\n"); // unknown @-command: ack
        return;
    }
    // Otherwise: a SimHub '$' telemetry frame. Sim mode overrides real telemetry.
    if let Some(tel) = simhub::parse_line(line) {
        if !crate::state::with(|s| s.sim_on) {
            *TELEM.lock().unwrap() = tel;
        }
        crate::state::with(|s| s.frames_ok += 1);
    } else {
        crate::state::with(|s| s.frames_bad += 1);
    }
}

/// `?` status reply, in the key=value shape the dashboard parses (g/s/r/fuel/
/// delta/bright/car). `car=` is last so the parser can read it to end-of-line.
fn status_line() -> String {
    let tel = *TELEM.lock().unwrap();
    let (bright, car) = crate::state::with(|s| (s.brightness, s.car_model.clone()));
    let heap = unsafe { sys::esp_get_free_heap_size() };
    format!(
        "esp-simhub | g={} s={} r={}/{} fuel={}.{} delta={} bright={} heap={} car={}\n",
        tel.gear as char,
        tel.speed_kmh,
        tel.rpm,
        tel.max_rpm,
        tel.fuel_dl / 10,
        (tel.fuel_dl % 10).abs(),
        tel.delta_ms,
        bright,
        heap,
        car,
    )
}

/// `@T` reply: gear char then every registry field value in id order.
fn telem_reply() -> String {
    use pith_core::registry::{field_value, FIELD_COUNT};
    let tel = *TELEM.lock().unwrap();
    let mut s = String::with_capacity(256);
    s.push(tel.gear as char);
    for id in 1..FIELD_COUNT {
        s.push(';');
        s.push_str(&field_value(&tel, id).to_string());
    }
    s.push('\n');
    s
}

// ---- replies ----

pub(crate) fn reply(t: Transport, s: &str) {
    match t {
        Transport::Cdc => {
            unsafe {
                sys::pith_cdc_write(s.as_ptr(), s.len() as i32);
                sys::pith_cdc_flush();
            }
        }
        Transport::Hid => {
            {
                let mut tx = HID_TX.lock().unwrap();
                if tx.pos >= tx.buf.len() {
                    tx.buf.clear();
                    tx.pos = 0;
                }
                tx.buf.extend_from_slice(s.as_bytes());
            }
            pump_hid_tx();
        }
    }
}

fn pump_hid_tx() {
    let mut tx = HID_TX.lock().unwrap();
    if tx.pos >= tx.buf.len() {
        tx.buf.clear();
        tx.pos = 0;
        return;
    }
    if !unsafe { sys::pith_hid_ready() } {
        return;
    }
    let remaining = tx.buf.len() - tx.pos;
    let n = remaining.min(61);
    let mut rep = [0u8; 62];
    rep[0] = n as u8;
    rep[1..1 + n].copy_from_slice(&tx.buf[tx.pos..tx.pos + n]);
    if unsafe { sys::pith_hid_send(2, rep.as_ptr() as *const core::ffi::c_void, (n + 1) as i32) } {
        tx.pos += n;
    }
}
