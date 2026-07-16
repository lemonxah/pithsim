// pith-pedals — ESP32-S3 active pedal firmware. Target hardware confirmed:
// gilphilbert's "PCBA V2" control board, v2.2 Rev B (ESP32-S3FH4R2, ADS1256
// loadcell ADC, a JSSmotor JSS57P1.5N closed-loop stepper over RS232) — see
// docs/pedals.md §0 and board.rs for the sourced pin map.
//
// Architecture: all control math (loadcell scaling, lever kinematics, Kalman
// filtering, the admittance model, effect oscillators, the Modbus register
// map) lives in the host-tested `pith-pedals-core` crate. This binary is the
// hardware glue: read the ADS1256, call `Controller::tick`, push the joystick
// axis (report 1), and — once homed AND armed — forward the target to the
// servo. Config/effects/state ride the report-2 JSON channel (`@CFG`/`@ACT`/
// `@GETCFG`/`?`/`@CAP`/`@ARM`/`@DISARM`), and OTA uses the same dual-slot
// `@OTA` mechanism as the DDU/handbrake.
//
// SAFETY: motor output is disarmed by default. The loadcell→joystick path
// runs immediately (it commands nothing), but the servo is only driven after
// the pedal is homed and the user sends `@ARM` post bench-validation
// (docs/pedals.md §0). The servo driver itself refuses motion until armed.

mod ads1256;
mod board;
mod device;
mod led;
mod ota;
mod runtime;
mod servo;
mod usb;
mod wifi;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;

fn main() {
    esp_idf_svc::sys::link_patches();
    EspLogger::initialize_default();

    // Status LED first — white as "main() reached", before anything that can
    // crash. With the console on UART0 this is the only zero-tooling way to
    // see whether the app is even booting (see led.rs for the color map).
    let mut led = led::init();

    let serial = device::serial();
    log::info!("pith-pedals boot — serial {serial}");

    usb::init(serial);

    // Confirm this image so the OTA rollback watchdog doesn't revert it.
    ota::mark_valid();

    let peripherals = Peripherals::take().expect("peripherals already taken");
    let pins = peripherals.pins;

    // --- WiFi transport (optional, disarmed of any motor role). Runs on its
    // own thread: connects with NVS-stored creds (provisioned via @WIFI),
    // streams the axis/state to the dashboard, and relays @-commands. If the
    // radio/NVS/event-loop can't init we just log and stay USB-only. ---
    let wifi_shared = Arc::new(wifi::WifiShared::new());
    match (EspSystemEventLoop::take(), EspDefaultNvsPartition::take()) {
        (Ok(sysloop), Ok(nvs)) => {
            wifi::spawn(
                peripherals.modem,
                sysloop,
                nvs,
                wifi_shared.clone(),
                serial.to_string(),
                env!("CARGO_PKG_VERSION").to_string(),
                wifi::WifiOpts {
                    kind: "pedals",
                    stream_axis: true,
                    stream_buttons: false,
                    ota: Some(wifi::OtaHooks {
                        begin: ota::begin,
                        feed: ota::feed,
                        active: ota_active,
                    }),
                },
            );
        }
        _ => log::warn!("wifi: event loop / NVS unavailable — USB-only"),
    }

    // --- Loadcell ADC (ADS1256). Reading it commands nothing, so this is
    // always safe to bring up. On failure we log and run without a loadcell
    // (axis reads 0%) rather than bricking the USB/OTA path. ---
    let mut adc = match ads1256::Ads1256::new(
        peripherals.spi2,
        pins.gpio16.into(), // SCK  (board::ADC_SCK)
        pins.gpio17.into(), // MOSI (board::ADC_MOSI / DIN)
        pins.gpio18.into(), // MISO (board::ADC_MISO / DOUT)
        pins.gpio7.into(),  // CS   (board::ADC_CS)
        pins.gpio15.into(), // DRDY (board::ADC_DRDY)
    ) {
        Ok(a) => {
            log::info!("ADS1256 loadcell ready");
            Some(a)
        }
        Err(e) => {
            log::warn!("ADS1256 init failed ({e:?}) — running without loadcell");
            None
        }
    };

    // --- Servo UART (JSS57P1.5N, RS232 Modbus). Opened DISARMED. Baud/slave
    // are unpublished and must be discovered on the bench (docs/pedals.md
    // §0); 9600/1 is the brief's first probe candidate. The driver refuses
    // motion until armed, so opening the port here is harmless. ---
    let mut servo = match servo::Servo::new(
        peripherals.uart1,
        pins.gpio10.into(), // TX (board::ISV57_TX)
        pins.gpio9.into(),  // RX (board::ISV57_RX)
        9600,
        1,
    ) {
        Ok(s) => {
            log::info!("servo UART open (disarmed; run bench discovery before @ARM)");
            Some(s)
        }
        Err(e) => {
            log::warn!("servo UART init failed ({e:?})");
            None
        }
    };

    let mut rt = runtime::Runtime::new();

    // Open-loop physical position estimate (steps from soft-min). Without live
    // encoder feedback wired yet, this follows the last commanded target; the
    // admittance model's soft-leash tolerates the small divergence. Real
    // closed-loop feedback (reading the servo's position registers) is a
    // Phase-2 bench task.
    let mut phys_steps: f32 = 0.0;
    let mut last_code: i32 = 0;
    let mut last_us: i64 = now_us();
    let mut ticks: u32 = 0;

    loop {
        usb::poll_hid(&mut rt, &wifi_shared);

        // Dispatch any commands that arrived over WiFi through the same
        // handler USB uses, queueing replies back to the WiFi thread.
        let wifi_cmds: Vec<String> = wifi_shared.rx.lock().unwrap().drain(..).collect();
        for cmd in wifi_cmds {
            let reply = usb::handle_command(&cmd, &mut rt, &wifi_shared);
            wifi_shared.tx.lock().unwrap().push(reply);
        }

        if ota::should_reboot() {
            std::thread::sleep(Duration::from_millis(300));
            log::info!("OTA complete — rebooting into the new image");
            unsafe { esp_idf_svc::sys::esp_restart() };
        }
        ota::check_timeout();

        if !ota::ACTIVE.load(std::sync::atomic::Ordering::SeqCst) {
            // Fresh loadcell sample if one is ready, else reuse the last.
            if let Some(adc) = adc.as_mut() {
                if let Ok(Some(code)) = adc.try_read() {
                    last_code = code;
                }
            }

            let now = now_us();
            let dt_us = (now - last_us).clamp(1, 5000) as u32;
            last_us = now;
            let now_ms = now / 1000;

            let out = rt.tick(last_code, phys_steps, 0, now_ms, dt_us);
            usb::push_axis(out.joystick);

            // Publish axis + status for the WiFi thread to stream.
            wifi_shared.axis.store(out.joystick, Ordering::Relaxed);
            if let Ok(mut sl) = wifi_shared.state_line.try_lock() {
                *sl = format!(
                    "position={} force={} joy={} armed={}",
                    rt.state.position_pct_x10, rt.state.force_n_x10, out.joystick, rt.armed as u8
                );
            }

            // Only drive the motor once homed AND armed (and the servo is
            // present + armed at the driver level). Otherwise the pedal is a
            // pure loadcell→joystick device.
            if rt.armed && rt.homed {
                if let Some(servo) = servo.as_mut() {
                    if servo.is_armed() {
                        let _ = servo.send_target(out.target_steps as i32);
                    }
                }
                // Open-loop position follow.
                phys_steps = (out.target_steps - rt.soft_min() as f32).max(0.0);
            }
        }

        if let Some(led) = led.as_mut() {
            led.tick(
                now_us() / 1000,
                ota::ACTIVE.load(std::sync::atomic::Ordering::SeqCst),
                rt.armed,
                usb::mounted(),
            );
        }

        std::thread::sleep(Duration::from_millis(1));
        ticks = ticks.wrapping_add(1);
        if ticks % 5000 == 0 {
            log::info!(
                "alive — usb={} armed={} homed={}",
                usb::mounted(),
                rt.armed,
                rt.homed
            );
        }
    }
}

/// Microseconds since boot (esp_timer).
fn now_us() -> i64 {
    unsafe { esp_idf_svc::sys::esp_timer_get_time() }
}

/// `pith_fw_wifi::OtaHooks::active` adapter (fn pointer, so no closure).
fn ota_active() -> bool {
    ota::ACTIVE.load(Ordering::SeqCst)
}
