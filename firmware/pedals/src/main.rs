// pith-pedals — ESP32-S3 active pedal firmware. Target hardware confirmed:
// gilphilbert's "PCBA V2" control board, v2.2 Rev B (ESP32-S3FH4R2, ADS1256
// loadcell ADC, iSV57T servo over RS232) — see docs/pedals.md §0 and
// board.rs for the sourced pin map.
//
// Phase 1 (this crate, see docs/pedals.md): USB HID joystick axis (report 1)
// + a JSON config/action/state command channel (report 2, `@CFG`/`@GETCFG`/
// `@ACT`/`?`/`@CAP`) speaking pith-pedals-core's protocol, plus the same
// dual-slot `@OTA` update mechanism as the DDU/handbrake. No loadcell or
// servo driver yet — `runtime::Runtime::sample` is an explicit placeholder,
// and `board.rs`'s pin constants are not wired to anything. Phase 2
// (bench-validated, not here) ports the reference project's admittance
// force controller and the ADS1256/iSV57T drivers.

mod board;
mod device;
mod ota;
mod runtime;
mod usb;

use std::time::Duration;

use esp_idf_svc::log::EspLogger;

fn main() {
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    let serial = device::serial();
    log::info!("pith-pedals boot — serial {serial}");

    usb::init(serial);

    // Confirm this image so the OTA rollback watchdog doesn't revert it on
    // the next reset (no-op when the running slot isn't pending-verify).
    ota::mark_valid();

    let mut rt = runtime::Runtime::new();

    let mut ticks: u32 = 0;
    loop {
        usb::poll_hid(&mut rt);

        if ota::should_reboot() {
            // Give the queued OTADONE reply time to flush over HID first.
            std::thread::sleep(Duration::from_millis(300));
            log::info!("OTA complete — rebooting into the new image");
            unsafe { esp_idf_svc::sys::esp_restart() };
        }
        ota::check_timeout();

        if !ota::ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
            rt.sample();
            usb::push_axis(rt.output());
        }

        std::thread::sleep(Duration::from_millis(2));
        ticks = ticks.wrapping_add(1);
        if ticks % 2500 == 0 {
            log::info!("alive — usb mounted={}", usb::mounted());
        }
    }
}
