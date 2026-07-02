// pithddu — ESP32-S3 SimHub dashboard firmware (Rust rewrite).
//
// Phase 2: USB composite device (CDC + HID) via the C shim over raw TinyUSB, the
// full `@`-command protocol (config pushes + NVS persistence + capability
// handshake), and OTA-over-USB. SimHub telemetry arrives on CDC; the dashboard's
// commands on the HID report-id-2 channel. LEDs/display land in later phases.

mod device;
mod display;
mod hid;
mod led;
mod ota;
mod sim;
mod state;
mod ui;
mod usb;

use std::thread::sleep;
use std::time::Duration;

fn main() {
    esp_idf_svc::sys::link_patches();
    // Route all `log::*` records to the UART console AND the GUI over HID report
    // id 3 (this board has no usable USB serial console once TinyUSB owns the port).
    usb::init_logger();

    let serial = device::serial();
    // Log WHY we booted (panic? watchdog? brownout? plain restart?) — streamed to
    // the GUI over the HID log channel, so an unexplained reboot in the field
    // (which looks like a spontaneous wake-from-sleep) is diagnosable after the fact.
    let rr = unsafe { esp_idf_svc::sys::esp_reset_reason() };
    let rr_name = match rr {
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_POWERON => "power-on",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_EXT => "external pin",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_SW => "software restart",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_PANIC => "PANIC",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_INT_WDT => "INTERRUPT WATCHDOG",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_TASK_WDT => "TASK WATCHDOG",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_WDT => "other watchdog",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_DEEPSLEEP => "deep-sleep wake",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_BROWNOUT => "BROWNOUT",
        esp_idf_svc::sys::esp_reset_reason_t_ESP_RST_USB => "usb",
        _ => "other",
    };
    log::info!("pithddu boot — serial {serial} (reset: {rr_name})");

    // Restore PC-pushed config (pins, layout, buttons, car, brightness) from NVS.
    // The LED task re-applies the saved car shift-light profile itself (see
    // led::spawn); the dashboard also re-pushes the live @C on reconnect.
    state::init();

    // Recovery (factory) chain-loaded us; point the boot partition back at it so the
    // NEXT reset returns to recovery — it's always the front door. Done early so even
    // an early crash reboots into recovery rather than re-running this image.
    ota::return_to_recovery_on_next_boot();

    // Bump the persisted consecutive-boot-fail counter (cleared once we've run
    // stably, see the main loop) so the recovery app can show "previous boot failed
    // Nx" on its splash. The in-app BIOS is gone — recovery (factory) boots first
    // and is the only on-device timer/menu now.
    state::boot_attempt_begin();

    // Bring up the composite USB device (PHY + TinyUSB + device task).
    usb::init(serial);

    // Rev/TC/ABS LED strip (own task: self-test + telemetry-driven shift lights).
    led::spawn();
    // HID gamepad service + bench-test sim generator.
    hid::spawn();
    sim::spawn();
    // Displays + touch + UI (own task).
    display::spawn();

    // We booted and ran successfully — confirm this image so a just-OTA'd update
    // isn't rolled back on the next reset.
    sleep(Duration::from_millis(1000));
    ota::mark_valid();
    log::info!("USB up; image marked valid");

    let mut ticks: u32 = 0;
    let mut boot_confirmed = false;
    loop {
        usb::poll_cdc(); // drain SimHub telemetry on CDC
        usb::poll_hid(); // drain HID-OUT bytes here (big stack) — the callback only buffers
        usb::pump_log_tx(); // flush queued logs to the GUI during quiet periods
        ota::check_timeout();

        // Ran stably past the risky boot window (~5 s) -> declare this boot good,
        // which clears the fail counter the recovery app displays.
        if !boot_confirmed && ticks > 1000 {
            boot_confirmed = true;
            state::boot_mark_ok();
        }

        if ota::should_reboot() {
            log::warn!("OTA complete — rebooting into the new image");
            sleep(Duration::from_millis(200));
            unsafe { esp_idf_svc::sys::esp_restart() };
        }
        if state::with(|s| s.cfg_reboot) {
            log::warn!("pin layout changed — rebooting to apply");
            sleep(Duration::from_millis(300)); // let the OK reply flush first
            unsafe { esp_idf_svc::sys::esp_restart() };
        }

        sleep(Duration::from_millis(5));
        ticks = ticks.wrapping_add(1);
        if ticks % 2000 == 0 {
            log::info!("alive — usb mounted={}", usb::mounted());
        }
    }
}
