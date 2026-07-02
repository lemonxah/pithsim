//! USB glue: drives the C shim (components/pith_hb_usb) for both the HID axis
//! report and the report-id-2 vendor command channel (the calibration
//! protocol + telemetry stream — no CDC/COM-port at all). The HID OUT
//! callback runs on the small TinyUSB task, so it only buffers raw bytes;
//! `poll_hid` drains and dispatches them from the main loop's big stack.

use std::ffi::{c_void, CString};
use std::sync::Mutex;

use esp_idf_svc::sys;
use pith_hb_core::proto::{self, HostCmd, ParseError};
use pith_hb_core::Calibration;

use crate::cal::CalStore;

const LINE_MAX: usize = 256; // calibration commands are short text lines
const TX_CAP: usize = 4096; // bound the reply/telemetry backlog if nobody's reading

// Raw HID-OUT bytes buffered by the USB callback for the main task to process.
static HID_RX: Mutex<Vec<u8>> = Mutex::new(Vec::new());

// HID reply/telemetry channel (report id 2), chunked into 61-byte payloads.
struct HidTx {
    buf: Vec<u8>,
    pos: usize,
}
static HID_TX: Mutex<HidTx> = Mutex::new(HidTx {
    buf: Vec::new(),
    pos: 0,
});

/// Shared runtime state: the calibration in effect ("pending" — what `@TARE`/
/// `@MAXC`/`@DZ`/`@INV` mutate live and what drives both the HID axis and the
/// `$raw,pct` stream), the last-saved calibration (what `@CANCEL` reverts to),
/// the NVS store, and the latest raw HX711 sample.
pub struct Runtime {
    pub pending: Calibration,
    pub saved: Calibration,
    pub store: CalStore,
    pub raw: i32,
    hid_line: Vec<u8>,
}

impl Runtime {
    pub fn new() -> Self {
        let store = crate::cal::init();
        let saved = store.load();
        Runtime {
            pending: saved,
            saved,
            store,
            raw: 0,
            hid_line: Vec::new(),
        }
    }

    /// The 0..=65535 axis value for the current raw sample under the pending
    /// calibration — this is what both the HID report and the telemetry line
    /// carry, so wizard previews always match what the firmware will output.
    pub fn output(&self) -> u16 {
        self.pending.apply(self.raw)
    }
}

pub fn init(serial: &str) {
    let c = CString::new(serial).unwrap_or_default();
    unsafe { sys::pith_hb_usb_init(c.as_ptr()) };
}

pub fn mounted() -> bool {
    unsafe { sys::pith_hb_usb_mounted() }
}

/// Push the current axis value as an HID report (report id 1), if the
/// endpoint can take one right now (skip otherwise — the next main-loop tick
/// will retry).
pub fn push_axis(value: u16) {
    if unsafe { sys::pith_hid_ready() } {
        unsafe {
            sys::pith_hid_send_axis(value);
        }
    }
}

/// Queue one `$raw,pct\n` telemetry line onto the report-id-2 channel.
pub fn push_telem(rt: &Runtime) {
    write_line(&proto::format_telem(rt.raw, rt.output()));
}

// ---- device -> Rust callback (called from the TinyUSB task) ----

/// HID OUT on report id 2: `buf` = `[N][payload…]` (length byte then N
/// bytes). Runs on the small TinyUSB task — do the minimum here; `poll_hid`
/// (big stack) does the actual line assembly + dispatch.
#[no_mangle]
pub extern "C" fn pith_on_hid_cmd(buf: *const u8, len: i32) {
    if buf.is_null() || len < 1 {
        return;
    }
    let data = unsafe { std::slice::from_raw_parts(buf, len as usize) };
    let n = (data[0] as usize).min(data.len() - 1);
    if n == 0 {
        return;
    }
    let payload = &data[1..1 + n];
    if let Ok(mut b) = HID_RX.lock() {
        b.extend_from_slice(payload);
    }
}

/// An HID IN report finished — pump the next queued chunk.
#[no_mangle]
pub extern "C" fn pith_on_hid_tx_complete() {
    pump_hid_tx();
}

// ---- line accumulation + dispatch ----

/// Drain HID-OUT bytes buffered by the callback and process them on the main
/// task (big stack). Call every main-loop iteration alongside the HX711 poll.
pub fn poll_hid(rt: &mut Runtime) {
    let bytes = {
        let mut b = HID_RX.lock().unwrap();
        if b.is_empty() {
            return;
        }
        std::mem::take(&mut *b)
    };
    feed(&bytes, rt);
}

fn feed(bytes: &[u8], rt: &mut Runtime) {
    let mut lines: Vec<String> = Vec::new();
    for &c in bytes {
        if c == b'\n' || c == b'\r' {
            if !rt.hid_line.is_empty() {
                lines.push(String::from_utf8_lossy(&rt.hid_line).into_owned());
                rt.hid_line.clear();
            }
            continue;
        }
        if rt.hid_line.len() < LINE_MAX - 1 {
            rt.hid_line.push(c);
        } else {
            rt.hid_line.clear(); // runaway line — drop it rather than grow forever
        }
    }
    for line in lines {
        let reply = dispatch(&line, rt);
        write_line(&reply);
    }
}

fn dispatch(line: &str, rt: &mut Runtime) -> String {
    match proto::parse_host_line(line) {
        Ok(HostCmd::Status) => status_line(rt),
        Ok(HostCmd::Cap) => format!(
            "OK board=lolin_s2_mini fw={} serial={} proto=1\n",
            env!("CARGO_PKG_VERSION"),
            crate::device::serial()
        ),
        Ok(HostCmd::SetIdle(raw)) => {
            rt.pending.idle_raw = raw;
            "OK\n".to_string()
        }
        Ok(HostCmd::SetMax(raw)) => {
            if Calibration::span_ok(rt.pending.idle_raw, raw) {
                rt.pending.max_raw = raw;
                rt.pending.calibrated = true;
                "OK\n".to_string()
            } else {
                format!("ERR {}\n", proto::err::SPAN)
            }
        }
        Ok(HostCmd::SetDeadzone { lo, hi }) => {
            rt.pending.deadzone_lo_pct = lo;
            rt.pending.deadzone_hi_pct = hi;
            "OK\n".to_string()
        }
        Ok(HostCmd::SetInverted(inverted)) => {
            rt.pending.inverted = inverted;
            "OK\n".to_string()
        }
        Ok(HostCmd::Save) => {
            if rt.store.save(&rt.pending) {
                rt.saved = rt.pending;
                "OK\n".to_string()
            } else {
                format!("ERR {}\n", proto::err::NVS)
            }
        }
        Ok(HostCmd::Cancel) => {
            rt.pending = rt.saved;
            "OK\n".to_string()
        }
        Ok(HostCmd::Reset) => {
            rt.saved = Calibration::default();
            rt.pending = rt.saved;
            if rt.store.reset() {
                "OK\n".to_string()
            } else {
                format!("ERR {}\n", proto::err::NVS)
            }
        }
        Err(ParseError::Unknown) => format!("ERR {}\n", proto::err::PARSE),
        Err(ParseError::BadArgs) => format!("ERR {}\n", proto::err::RANGE),
    }
}

fn status_line(rt: &Runtime) -> String {
    format!(
        "OK raw={} pct={} idle={} max={} dzlo={} dzhi={} inv={} calibrated={}\n",
        rt.raw,
        rt.output(),
        rt.pending.idle_raw,
        rt.pending.max_raw,
        rt.pending.deadzone_lo_pct,
        rt.pending.deadzone_hi_pct,
        rt.pending.inverted as u8,
        rt.pending.calibrated as u8,
    )
}

// ---- report-id-2 TX (replies + telemetry share one FIFO byte stream) ----

fn write_line(s: &str) {
    {
        let mut tx = HID_TX.lock().unwrap();
        // Compact already-sent bytes so `buf` doesn't creep upward over time.
        if tx.pos > 0 {
            let consumed = tx.pos;
            tx.buf.drain(..consumed);
            tx.pos = 0;
        }
        tx.buf.extend_from_slice(s.as_bytes());
        // Bounded: if nothing's draining (no app connected), drop the oldest
        // overflow instead of growing forever.
        if tx.buf.len() > TX_CAP {
            let overflow = tx.buf.len() - TX_CAP;
            tx.buf.drain(..overflow);
        }
    }
    pump_hid_tx();
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
    if unsafe { sys::pith_hid_send(2, rep.as_ptr() as *const c_void, (n + 1) as i32) } {
        tx.pos += n;
    }
}
