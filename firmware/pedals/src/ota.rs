//! OTA-over-USB — the same `@OTA` dialect as the DDU, over the report-id-2
//! command channel: the dashboard sends `@OTA<size>` then streams the raw
//! app image into the inactive slot via the raw esp_ota_* handle API (the
//! safe wrapper's borrow lifetime fights the across-callbacks streaming
//! model). Flow:
//!   @OTA<size> -> OTAREADY ; stream bytes, ACK "K" per 2048 ; OTADONE + reboot.
//! On error: OTAERR. An abandoned transfer (no bytes for 4 s) is aborted.
//!
//! Unlike the DDU there is no factory/recovery app owning slot selection:
//! on success we point the bootloader at the new slot directly, and the
//! rollback watchdog (CONFIG_BOOTLOADER_APP_ROLLBACK_ENABLE) reverts to the
//! previous slot if the new image never marks itself valid.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use esp_idf_svc::sys;

use crate::usb::write_line;

const ACK_CHUNK: i32 = 2048; // ACK every this-many bytes (host flow control)
const TIMEOUT_US: i64 = 4_000_000;

/// Fast-path flag so the byte feed (and the main loop's telemetry push) can
/// skip work unless an OTA is in flight.
pub static ACTIVE: AtomicBool = AtomicBool::new(false);
static REBOOT: AtomicBool = AtomicBool::new(false);

struct Ota {
    handle: sys::esp_ota_handle_t,
    part: usize, // *const esp_partition_t kept as usize (Send-safe)
    remaining: i32,
    acc: i32,
    last_us: i64,
}

static OTA: Mutex<Option<Ota>> = Mutex::new(None);

fn now_us() -> i64 {
    unsafe { sys::esp_timer_get_time() }
}

/// Handle `@OTA<size>`: erase the inactive slot and start receiving.
pub fn begin(size: i32) {
    if size <= 0 {
        write_line("OTAERR\n");
        return;
    }
    unsafe {
        let part = sys::esp_ota_get_next_update_partition(core::ptr::null());
        if part.is_null() {
            log::error!("OTA: no next update partition (old single-factory table?)");
            write_line("OTAERR\n");
            return;
        }
        let label = core::ffi::CStr::from_ptr((*part).label.as_ptr())
            .to_str()
            .unwrap_or("?");
        log::info!("OTA: target '{label}' @0x{:x} size {size}", (*part).address);
        let mut handle: sys::esp_ota_handle_t = 0;
        let r = sys::esp_ota_begin(part, size as usize, &mut handle);
        if r != 0 {
            log::error!("OTA: esp_ota_begin failed: 0x{r:x}");
            write_line("OTAERR\n");
            return;
        }
        *OTA.lock().unwrap() = Some(Ota {
            handle,
            part: part as usize,
            remaining: size,
            acc: 0,
            last_us: now_us(),
        });
    }
    ACTIVE.store(true, Ordering::SeqCst);
    write_line("OTAREADY\n");
}

/// Feed raw image bytes. Returns true if an OTA is active and the bytes were
/// consumed (so the caller skips line accumulation); false otherwise.
pub fn feed(data: &[u8]) -> bool {
    enum Post {
        None,
        Ack,
        Done,
        Err,
    }
    // Compute the reply while holding the lock, send after release.
    let post = {
        let mut g = OTA.lock().unwrap();
        let ota = match g.as_mut() {
            Some(o) => o,
            None => return false,
        };
        ota.last_us = now_us();
        let n = (data.len() as i32).min(ota.remaining).max(0) as usize;
        let res =
            unsafe { sys::esp_ota_write(ota.handle, data.as_ptr() as *const core::ffi::c_void, n) };
        if res != 0 {
            log::error!("OTA: esp_ota_write failed: 0x{res:x}");
            unsafe { sys::esp_ota_end(ota.handle) };
            *g = None;
            ACTIVE.store(false, Ordering::SeqCst);
            Post::Err
        } else {
            ota.remaining -= n as i32;
            ota.acc += n as i32;
            if ota.remaining <= 0 {
                let part = ota.part as *const sys::esp_partition_t;
                let ok = unsafe {
                    sys::esp_ota_end(ota.handle) == 0 && sys::esp_ota_set_boot_partition(part) == 0
                };
                *g = None;
                ACTIVE.store(false, Ordering::SeqCst);
                if ok {
                    REBOOT.store(true, Ordering::SeqCst);
                    Post::Done
                } else {
                    Post::Err
                }
            } else if ota.acc >= ACK_CHUNK {
                ota.acc -= ACK_CHUNK;
                Post::Ack
            } else {
                Post::None
            }
        }
    };
    match post {
        Post::Ack => write_line("K\n"),
        Post::Done => write_line("OTADONE\n"),
        Post::Err => write_line("OTAERR\n"),
        Post::None => {}
    }
    true
}

/// Abort an abandoned transfer (PC crashed mid-flash) so the device leaves OTA
/// mode and a fresh `@OTA` retry parses cleanly. Call periodically.
pub fn check_timeout() {
    if !ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let mut g = OTA.lock().unwrap();
    if let Some(ota) = g.as_ref() {
        if now_us() - ota.last_us > TIMEOUT_US {
            unsafe { sys::esp_ota_end(ota.handle) };
            *g = None;
            ACTIVE.store(false, Ordering::SeqCst);
            log::warn!("OTA timed out — aborted abandoned transfer");
        }
    }
}

/// True once a completed OTA wants a reboot into the new image.
pub fn should_reboot() -> bool {
    REBOOT.load(Ordering::SeqCst)
}

/// We booted and ran successfully — confirm this image so the rollback
/// watchdog (if we just OTA'd into it) doesn't revert us on the next reset.
pub fn mark_valid() {
    unsafe { sys::esp_ota_mark_app_valid_cancel_rollback() };
}
