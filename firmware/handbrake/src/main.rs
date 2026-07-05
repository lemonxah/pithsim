// pith-hb — single-axis USB handbrake firmware. Builds for ESP32-S2 (Lolin
// S2 Mini, the shipped/default hardware) or ESP32-S3 (see justfile's
// build-s3/flash-s3/image-s3 — a different Xtensa target, selected via
// --target/MCU env overrides + the `esp32s3` Cargo feature); the Rust logic
// is identical on both, only the `board` label in @CAP differs (device.rs).
//
// HX711 load cell -> integer smoothing -> calibration (idle/max/deadzone,
// persisted in NVS) -> a single 16-bit HID axis (report id 1) — a plain USB
// joystick, no COM port. The calibration wizard's protocol (`@`-commands)
// and a continuous `$raw,pct` telemetry stream ride a second HID report
// (id 2, vendor channel) instead. Firmware updates arrive over that same
// channel (@OTA, dual app slots + rollback). No display/LEDs — see the DDU
// for that end of things.

mod autozero;
mod cal;
mod device;
mod hx711;
mod ota;
mod usb;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;

use autozero::AutoZero;
use hx711::{Hx711, Iir};

fn main() {
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    let serial = device::serial();
    log::info!("pith-hb boot — serial {serial}");

    usb::init(serial);

    // Confirm this image so the OTA rollback watchdog doesn't revert it on
    // the next reset (no-op when the running slot isn't pending-verify).
    ota::mark_valid();

    let peripherals = Peripherals::take().expect("peripherals already taken");

    // Take the default NVS partition ONCE (singleton) — shared by the
    // calibration store and the WiFi driver/credentials.
    let nvs_part = EspDefaultNvsPartition::take().ok();

    let mut cell = Hx711::new(peripherals.pins.gpio1, peripherals.pins.gpio2)
        .expect("HX711 GPIO init failed");
    // No smoothing by default (shift 0 = passthrough): at a HX711's typical
    // 10 SPS (no RATE pin broken out), even light filtering added a
    // noticeable ramp-up on a hard pull. The deadzone (tuned in the
    // calibration wizard) absorbs noise near idle/max instead — raise this
    // if mid-travel jitter bothers you, especially once/if you're at 80 SPS
    // (RATE pin tied high), where the settling cost per shift level is 8x
    // shorter and much less noticeable.
    let mut filter = Iir::new(0);
    // Continuous idle drift compensation — see autozero.rs. Keeps the axis
    // reading exactly 0% at rest over time (temperature, mechanical creep)
    // without needing the app or a manual recalibration.
    let mut auto_zero = AutoZero::new();

    let mut rt = usb::Runtime::new(nvs_part.clone());

    // WiFi transport (optional): connects with NVS-stored creds (provisioned
    // via @WIFI), streams the axis/state to the dashboard, and relays
    // @-commands. If the radio/NVS/event-loop can't init, we log and stay
    // USB-only. The game axis over USB HID is unaffected.
    let wifi_shared = Arc::new(pith_fw_wifi::WifiShared::new());
    match (EspSystemEventLoop::take(), nvs_part) {
        (Ok(sysloop), Some(nvs)) => {
            pith_fw_wifi::spawn(
                peripherals.modem,
                sysloop,
                nvs,
                wifi_shared.clone(),
                serial.to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
                pith_fw_wifi::WifiOpts {
                    kind: "handbrake",
                    stream_axis: true,
                    ota: Some(pith_fw_wifi::OtaHooks {
                        begin: ota::begin,
                        feed: ota::feed,
                        active: ota_active,
                    }),
                },
            );
        }
        _ => log::warn!("wifi: event loop / NVS unavailable — USB-only"),
    }

    let mut ticks: u32 = 0;
    loop {
        usb::poll_hid(&mut rt, &wifi_shared);

        // Dispatch any commands received over WiFi through the same handler.
        let wifi_cmds: Vec<String> = wifi_shared.rx.lock().unwrap().drain(..).collect();
        for cmd in wifi_cmds {
            let reply = usb::handle_command(&cmd, &mut rt, &wifi_shared);
            wifi_shared.tx.lock().unwrap().push(reply);
        }

        if ota::should_reboot() {
            // Give the queued OTADONE reply time to flush over HID first.
            std::thread::sleep(Duration::from_millis(300));
            log::info!("OTA complete — rebooting into the new image");
            unsafe { esp_idf_svc::sys::esp_restart() };
        }
        ota::check_timeout();

        if let Some(sample) = cell.try_read() {
            rt.raw = filter.push(sample);
            auto_zero.observe(rt.raw, &mut rt.pending);
            let axis = rt.output();
            usb::push_axis(axis);
            // Publish the axis for the WiFi thread to stream.
            wifi_shared.axis.store(axis, Ordering::Relaxed);
            if let Ok(mut sl) = wifi_shared.state_line.try_lock() {
                *sl = format!("raw={} axis={}", rt.raw, axis);
            }
            // Keep the channel clean (and the CPU on flash writes) mid-OTA.
            if !ota::ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
                usb::push_telem(&rt);
            }
        }

        std::thread::sleep(Duration::from_millis(2));
        ticks = ticks.wrapping_add(1);
        if ticks % 2500 == 0 {
            log::info!("alive — usb mounted={} raw={}", usb::mounted(), rt.raw);
        }
    }
}

/// `pith_fw_wifi::OtaHooks::active` adapter (fn pointer, so no closure).
fn ota_active() -> bool {
    ota::ACTIVE.load(Ordering::SeqCst)
}
