//! OTA-over-USB: the dashboard sends `@OTA<size>` then streams the raw .bin to
//! the inactive app slot; we write it via the raw esp_ota_* handle API (the safe
//! wrapper's borrow lifetime fights the across-callbacks streaming model), then
//! reboot into it (rollback reverts a bad image). Flow:
//!   @OTA<size> -> OTAREADY ; stream bytes, ACK "K" per 2048 ; OTADONE + reboot.
//! On error: OTAERR. An abandoned transfer (no bytes for 4 s) is aborted.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use esp_idf_svc::sys;

use crate::usb::{reply, Transport};

const ACK_CHUNK: i32 = 2048; // ACK every this-many bytes (host flow control)
const TIMEOUT_US: i64 = 4_000_000;

/// Fast-path flag so the byte feed can skip locking unless an OTA is in flight.
pub static ACTIVE: AtomicBool = AtomicBool::new(false);
static REBOOT: AtomicBool = AtomicBool::new(false);

struct Ota {
    handle: sys::esp_ota_handle_t,
    part: usize, // *const esp_partition_t kept as usize (Send-safe)
    remaining: i32,
    total: i32,
    acc: i32,
    itf: Transport,
    last_us: i64,
}

/// Coarse OTA progress 0..100 for the on-screen "updating" bar (0 if inactive).
pub fn progress_pct() -> i32 {
    let g = OTA.lock().unwrap();
    match g.as_ref() {
        Some(o) if o.total > 0 => ((o.total - o.remaining) * 100 / o.total).clamp(0, 100),
        _ => 0,
    }
}

static OTA: Mutex<Option<Ota>> = Mutex::new(None);

fn now_us() -> i64 {
    unsafe { sys::esp_timer_get_time() }
}

/// Handle `@OTA<size>`: erase the next slot and start receiving. Replies on `itf`.
pub fn begin(itf: Transport, size: i32) {
    if size <= 0 {
        reply(itf, "OTAERR\n");
        return;
    }
    unsafe {
        let part = sys::esp_ota_get_next_update_partition(core::ptr::null());
        if part.is_null() {
            log::error!("OTA: no next update partition");
            reply(itf, "OTAERR\n");
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
            reply(itf, "OTAERR\n");
            return;
        }
        *OTA.lock().unwrap() = Some(Ota {
            handle,
            part: part as usize,
            remaining: size,
            total: size,
            acc: 0,
            itf,
            last_us: now_us(),
        });
    }
    ACTIVE.store(true, Ordering::SeqCst);
    reply(itf, "OTAREADY\n");
}

/// Feed raw image bytes. Returns true if this itf owns the active OTA and the
/// bytes were consumed (so the caller skips line accumulation); false otherwise.
pub fn feed(itf: Transport, data: &[u8]) -> bool {
    // Compute the reply (if any) while holding the lock, then send after release.
    enum Post {
        None,
        Ack,
        Done,
        Err,
    }
    let post = {
        let mut g = OTA.lock().unwrap();
        let ota = match g.as_mut() {
            Some(o) if o.itf == itf => o,
            _ => return false,
        };
        ota.last_us = now_us();
        let n = (data.len() as i32).min(ota.remaining).max(0) as usize;
        let res = unsafe {
            sys::esp_ota_write(ota.handle, data.as_ptr() as *const core::ffi::c_void, n)
        };
        if res != 0 {
            log::error!("OTA: esp_ota_write failed at {} bytes: 0x{res:x}", ota.total - ota.remaining);
            unsafe { sys::esp_ota_end(ota.handle) };
            *g = None;
            ACTIVE.store(false, Ordering::SeqCst);
            Post::Err
        } else {
            ota.remaining -= n as i32;
            ota.acc += n as i32;
            if ota.remaining <= 0 {
                let part = ota.part as *const sys::esp_partition_t;
                let ok = unsafe { sys::esp_ota_end(ota.handle) == 0 };
                // Recovery (factory) owns boot-slot selection in the recovery-first
                // model. So instead of pointing otadata at the new image ourselves,
                // record which OTA slot we just wrote in NVS; on reboot the device
                // lands in recovery, which chain-loads this slot.
                let slot = unsafe {
                    ((*part).subtype as i32
                        - sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_OTA_0 as i32)
                        .clamp(0, 1) as u8
                };
                *g = None;
                ACTIVE.store(false, Ordering::SeqCst);
                if ok {
                    crate::state::set_main_slot(slot);
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
        Post::Ack => reply(itf, "K\n"),
        Post::Done => reply(itf, "OTADONE\n"),
        Post::Err => reply(itf, "OTAERR\n"),
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

/// We booted and ran successfully — confirm this image so the rollback timer
/// (if we just OTA'd into it) doesn't revert us on the next reset.
pub fn mark_valid() {
    unsafe { sys::esp_ota_mark_app_valid_cancel_rollback() };
}

/// Point the boot partition back at the `factory` (recovery) app so the NEXT reset
/// returns to recovery instead of re-running this main image. Called early in the
/// main firmware's boot — recovery is always the front door (it chain-loads us).
/// A crash before this still lands in recovery anyway (factory is the bootloader's
/// fallback when the selected slot is bad).
pub fn return_to_recovery_on_next_boot() {
    unsafe {
        let fac = sys::esp_partition_find_first(
            sys::esp_partition_type_t_ESP_PARTITION_TYPE_APP,
            sys::esp_partition_subtype_t_ESP_PARTITION_SUBTYPE_APP_FACTORY,
            core::ptr::null(),
        );
        if !fac.is_null() {
            if sys::esp_ota_set_boot_partition(fac) != 0 {
                log::warn!("could not point boot back to recovery (factory)");
            }
        } else {
            log::warn!("factory (recovery) partition not found");
        }
    }
}
