//! pith-flash — OTA-flash a firmware `.bin` to the Pith DDU over its USB link.
//!
//! Uses the exact same path as the dashboard's "FLASH LOCAL BUILD" button: the
//! shared `pith-device` crate's `Dash::ota_upload`, which streams the image over
//! the device's `@OTA` command channel (HID by default, or a serial port). No
//! boot button / download mode — the running firmware reflashes itself.

use std::io::Write;
use std::process::ExitCode;
use std::time::Instant;

use pith_device::{Dash, Serial, PITH_PID, PITH_VID};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let mut bin_path: Option<String> = None;
    let mut port: Option<String> = None;
    let mut list = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-p" | "--port" => {
                i += 1;
                match args.get(i) {
                    Some(p) => port = Some(p.clone()),
                    None => {
                        eprintln!("error: --port needs a value");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "-l" | "--list" => list = true,
            "-h" | "--help" => {
                print_usage();
                return ExitCode::SUCCESS;
            }
            s if !s.starts_with('-') && bin_path.is_none() => bin_path = Some(s.to_string()),
            other => {
                eprintln!("error: unexpected argument '{other}'\n");
                print_usage();
                return ExitCode::FAILURE;
            }
        }
        i += 1;
    }

    if list {
        let ports = Serial::list();
        if ports.is_empty() {
            println!("No serial ports found.");
        }
        for p in ports {
            let tag = if p.is_dash {
                " [Pith DDU]"
            } else if p.is_esp {
                " [ESP]"
            } else {
                ""
            };
            println!("{}  {} {}{}", p.device, p.manufacturer, p.product, tag);
        }
        return ExitCode::SUCCESS;
    }

    let bin_path = match bin_path {
        Some(p) => p,
        None => {
            eprintln!("error: no firmware .bin given\n");
            print_usage();
            return ExitCode::FAILURE;
        }
    };

    let img = match std::fs::read(&bin_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: cannot read '{bin_path}': {e}");
            return ExitCode::FAILURE;
        }
    };
    if img.is_empty() {
        eprintln!("error: '{bin_path}' is empty");
        return ExitCode::FAILURE;
    }
    println!("Image:  {bin_path}  ({} bytes)", img.len());

    // ---- open the device (HID by default, or a named serial port) ----
    let mut dash = Dash::default();
    if let Some(ref p) = port {
        if !dash.ser.open(p) {
            eprintln!("error: cannot open serial port '{p}'");
            return ExitCode::FAILURE;
        }
        dash.use_hid = false;
        println!("Device: serial {p}");
    } else {
        if !dash.hid.open(PITH_VID, PITH_PID) {
            eprintln!(
                "error: no Pith DDU found on USB HID ({:04x}:{:04x}).\n\
                 Is the dashboard GUI holding the device? Close it, or use --port <serial>.",
                PITH_VID, PITH_PID
            );
            return ExitCode::FAILURE;
        }
        dash.use_hid = true;
        println!("Device: USB HID {:04x}:{:04x}", PITH_VID, PITH_PID);
    }

    // Confirm it actually answers as a Pith device before sending an image.
    let caps = dash.capabilities();
    if !caps.contains("name") {
        eprintln!("warning: device did not answer @CAP — flashing anyway.");
    }

    // ---- stream the image (same @OTA protocol as the GUI) ----
    let start = Instant::now();
    let mut last = -1i32;
    let ok = dash.ota_upload(&img, |pct| {
        if pct != last {
            last = pct;
            print!("\rFlashing… {pct:3}%");
            let _ = std::io::stdout().flush();
        }
    });
    println!();

    if ok {
        println!(
            "✓ Flashed in {:.1}s — the device is rebooting into the new image.",
            start.elapsed().as_secs_f32()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "✗ Flash failed. Full device log:\n----\n{}\n----",
            dash.log.trim_end()
        );
        ExitCode::FAILURE
    }
}

fn print_usage() {
    eprintln!("pith-flash — OTA-flash a firmware .bin to the Pith DDU over USB (no boot button)\n");
    eprintln!("USAGE:");
    eprintln!("    pith-flash <firmware.bin> [--port <serial>]\n");
    eprintln!("OPTIONS:");
    eprintln!(
        "    -p, --port <dev>   Flash over a serial port instead of USB HID (e.g. /dev/ttyACM0)"
    );
    eprintln!("    -l, --list         List serial ports and exit");
    eprintln!("    -h, --help         Show this help\n");
    eprintln!("The device must already be running OTA-capable firmware (it reflashes itself).");
}
