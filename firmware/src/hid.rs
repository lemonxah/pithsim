//! USB HID gamepad — a 32-button "button box" the sim sees as a controller.
//! Touch widgets hold/release via set(); a task pushes a joystick report (id 1)
//! via the pith_usb shim whenever the button mask changes. Port of hid_gamepad.c
//! (the old tap-pulse auto-release went away with true press-and-hold buttons).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use esp_idf_svc::sys;

const N: usize = 32;

// Whether the host has received our baseline (all-buttons-released) report since
// the last (re)connect. Without it the joystick never reports at boot — leaving
// some hosts/games (Forza via DirectInput) reading a phantom stuck button.
static BASELINE_SENT: AtomicBool = AtomicBool::new(false);

struct Gamepad {
    mask: u32, // current button bitmask
    sent: u32, // last mask reported to the host
}

static PAD: Mutex<Gamepad> = Mutex::new(Gamepad { mask: 0, sent: 0 });

/// Hold/release `btn` explicitly (toggles / press-and-hold widgets).
pub fn set(btn: usize, pressed: bool) {
    if btn >= N {
        return;
    }
    let mut p = PAD.lock().unwrap();
    if pressed {
        p.mask |= 1 << btn;
    } else {
        p.mask &= !(1 << btn);
    }
}

/// Push a report when the mask changed (or to establish the post-connect baseline).
fn service() {
    // Not enumerated yet: arm the baseline so we re-send once after (re)connect.
    if !unsafe { sys::pith_hid_ready() } {
        BASELINE_SENT.store(false, Ordering::Relaxed);
        return;
    }
    let need_baseline = !BASELINE_SENT.load(Ordering::Relaxed);
    let mask = {
        let p = PAD.lock().unwrap();
        // Send when the mask changed, OR once after connect to publish the
        // all-released baseline (so the host never reads a phantom button).
        if p.mask == p.sent && !need_baseline {
            return;
        }
        p.mask
    };
    let bytes = mask.to_le_bytes();
    if unsafe {
        sys::pith_hid_send(1, bytes.as_ptr() as *const core::ffi::c_void, bytes.len() as i32)
    } {
        PAD.lock().unwrap().sent = mask;
        BASELINE_SENT.store(true, Ordering::Relaxed);
    }
}

/// Spawn the HID service task (~100 Hz).
pub fn spawn() {
    thread::Builder::new()
        .stack_size(2048)
        .name("hid".into())
        .spawn(|| loop {
            service();
            thread::sleep(Duration::from_millis(10));
        })
        .expect("spawn hid task");
}
