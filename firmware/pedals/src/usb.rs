//! USB glue: drives the C shim (components/pith_pedals_usb) for both the HID
//! axis report and the report-id-2 vendor command channel. Transport-level
//! logic (chunked TX FIFO, RX line reassembly, `@OTA` byte-mode takeover) is
//! the same proven design as the handbrake's `usb.rs`; the command set here
//! is pith-pedals-core's JSON config/action/state protocol instead of the
//! handbrake's plain key=value calibration commands.

use std::ffi::{c_void, CString};
use std::sync::Mutex;

use esp_idf_svc::sys;
use pith_pedals_core::protocol::{PedalAction, PedalConfig};

use crate::runtime::Runtime;

const LINE_MAX: usize = 4096; // PedalConfig JSON is a few hundred bytes; leave headroom
const TX_CAP: usize = 8192;

// Raw HID-OUT bytes buffered by the USB callback for the main task to process.
static HID_RX: Mutex<Vec<u8>> = Mutex::new(Vec::new());

// HID reply channel (report id 2), chunked into 61-byte payloads.
struct HidTx {
    buf: Vec<u8>,
    pos: usize,
}
static HID_TX: Mutex<HidTx> = Mutex::new(HidTx {
    buf: Vec::new(),
    pos: 0,
});

pub fn init(serial: &str) {
    let c = CString::new(serial).unwrap_or_default();
    unsafe { sys::pith_pedals_usb_init(c.as_ptr()) };
}

pub fn mounted() -> bool {
    unsafe { sys::pith_pedals_usb_mounted() }
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

static HID_LINE: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Drain HID-OUT bytes buffered by the callback and process them on the main
/// task (big stack). Call every main-loop iteration.
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
    // Mid-OTA the channel carries the raw image, not text — hand every byte
    // to the OTA writer and skip line accumulation.
    if crate::ota::feed(bytes) {
        return;
    }
    let mut lines: Vec<String> = Vec::new();
    {
        let mut line = HID_LINE.lock().unwrap();
        for &c in bytes {
            if c == b'\n' || c == b'\r' {
                if !line.is_empty() {
                    lines.push(String::from_utf8_lossy(&line).into_owned());
                    line.clear();
                }
                continue;
            }
            if line.len() < LINE_MAX - 1 {
                line.push(c);
            } else {
                line.clear(); // runaway line — drop it rather than grow forever
            }
        }
    }
    for line in lines {
        // @OTA replies out-of-band and flips the channel into raw-byte mode,
        // so it never goes through the normal command -> single-reply path.
        if let Some(rest) = line.strip_prefix("@OTA") {
            crate::ota::begin(rest.trim().parse().unwrap_or(0));
            continue;
        }
        let reply = dispatch(&line, rt);
        write_line(&reply);
    }
}

fn dispatch(line: &str, rt: &mut Runtime) -> String {
    let line = line.trim();
    if line == "?" {
        return status_line(rt);
    }
    if line == "@CAP" {
        return format!(
            "OK board=pcba_v2.2b fw={} serial={} proto=1\n",
            env!("CARGO_PKG_VERSION"),
            crate::device::serial()
        );
    }
    if line == "@GETCFG" {
        return match serde_json::to_string(&rt.config) {
            Ok(json) => format!("OK{json}\n"),
            Err(_) => "ERR encode\n".to_string(),
        };
    }
    if let Some(json) = line.strip_prefix("@CFG") {
        return match serde_json::from_str::<PedalConfig>(json) {
            Ok(cfg) => {
                rt.config = cfg;
                "OK\n".to_string()
            }
            Err(_) => "ERR parse\n".to_string(),
        };
    }
    if let Some(json) = line.strip_prefix("@ACT") {
        return match serde_json::from_str::<PedalAction>(json) {
            Ok(action) => {
                rt.action = action;
                "OK\n".to_string()
            }
            Err(_) => "ERR parse\n".to_string(),
        };
    }
    "ERR unknown\n".to_string()
}

fn status_line(rt: &Runtime) -> String {
    format!(
        "OK position={} force={} joy={} err={} servo={}\n",
        rt.state.position_pct_x10,
        rt.state.force_n_x10,
        rt.output(),
        rt.state.error_code,
        rt.state.servo_on as u8,
    )
}

// ---- report-id-2 TX (chunked FIFO byte stream) ----

pub fn write_line(s: &str) {
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
