//! Bench discovery probe for the JSS57P1.5N drive's RS232/Modbus port — answers
//! "does this drive speak Modbus at all, and at what baud/slave-ID?" without
//! touching anything: it only ever sends FC 0x03 reads.
//!
//! Wiring: 3.3V/5V TTL USB-UART adapter → drive TX/RX/GND (cross TX↔RX).
//! Scope the drive's TX pin first if you suspect true ±RS232 levels.
//!
//! Usage: cargo run -p pith-device --example jss_probe -- /dev/ttyUSB0
//!
//! Sweeps every candidate baud (9600..115200, 8N1) × slave ID (1..=31) with
//! the register-0 identity probe (expects 57). On a hit, bulk-reads registers
//! 0..=34 and prints them against the manual's defaults.

use std::time::Duration;

use pith_pedals_core::modbus;
use pith_pedals_core::servo_jss57p::{
    encode_bulk_read, encode_identity_probe, is_identity_response, DEFAULTS,
    DISCOVERY_BAUD_CANDIDATES, DISCOVERY_SLAVE_ID_MAX, DISCOVERY_SLAVE_ID_MIN,
};

const REG_NAMES: [&str; 35] = [
    "drive model (RO)",
    "loop mode (0=open 1=closed)",
    "motor type (DO NOT WRITE)",
    "current loop Kp (RO)",
    "current loop Ki (RO)",
    "position loop Kp",
    "speed loop Kp",
    "speed loop Ki",
    "microsteps/rev (default dial)",
    "encoder resolution",
    "tracking error alarm threshold",
    "open-loop hold current x100mA",
    "closed-loop peak current x100mA",
    "pulse filter time x50us",
    "enable polarity",
    "alarm output polarity",
    "pulse input mode (0=PUL/DIR)",
    "pulse active edge",
    "PEND function (0=in-position)",
    "PEND polarity",
    "accel lo",
    "accel hi",
    "decel lo",
    "decel hi",
    "max speed lo",
    "max speed hi",
    "target pulses lo",
    "target pulses hi",
    "motion command",
    "position mode (0=incr 1=abs)",
    "abs position lo (RO)",
    "abs position hi (RO)",
    "motion state (RO)",
    "save to EEPROM",
    "factory reset (NEVER WRITE)",
];

fn main() {
    let port_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/ttyUSB0".into());
    println!("probing {port_path} — identity read (reg 0, expect 57), 8N1");

    let mut hits = Vec::new();
    for &baud in &DISCOVERY_BAUD_CANDIDATES {
        let mut port = match serialport::new(&port_path, baud)
            .data_bits(serialport::DataBits::Eight)
            .parity(serialport::Parity::None)
            .stop_bits(serialport::StopBits::One)
            .timeout(Duration::from_millis(120))
            .open()
        {
            Ok(p) => p,
            Err(e) => {
                eprintln!("cannot open {port_path} at {baud}: {e}");
                return;
            }
        };
        print!("{baud:>6} baud: ");
        let mut hit_this_baud = None;
        for slave in DISCOVERY_SLAVE_ID_MIN..=DISCOVERY_SLAVE_ID_MAX {
            if let Some(frame) = transact(&mut *port, &encode_identity_probe(slave), 7) {
                if is_identity_response(&frame) {
                    hit_this_baud = Some(slave);
                    break;
                }
                // Answered but not 57 — still a live device, worth reporting.
                println!();
                println!("  slave {slave} answered but not identity 57: {frame:02x?}");
                print!("        ");
            }
        }
        match hit_this_baud {
            Some(slave) => {
                println!("HIT — slave ID {slave} answered identity 57");
                hits.push((baud, slave));
            }
            None => println!("silent"),
        }
    }

    let Some(&(baud, slave)) = hits.first() else {
        println!();
        println!("no response on any baud/slave-ID: this drive's firmware does not");
        println!("expose Modbus on these pins (or check wiring: TX↔RX crossed, common GND).");
        return;
    };

    println!();
    println!("bulk-reading registers 0..=34 at {baud} baud, slave {slave}:");
    let mut port = serialport::new(&port_path, baud)
        .timeout(Duration::from_millis(300))
        .open()
        .expect("reopen for bulk read");
    let Some(frame) = transact(&mut *port, &encode_bulk_read(slave), 5 + 2 * DEFAULTS.len()) else {
        println!("bulk read (FC 0x03 x35) got no reply — drive may cap read count; fall back to single reads.");
        return;
    };
    match modbus::decode_read_holding_response(&frame) {
        Ok(regs) => {
            for (i, v) in regs.iter().enumerate() {
                let name = REG_NAMES.get(i).copied().unwrap_or("?");
                let default = DEFAULTS.get(i).copied().flatten();
                let marker = match default {
                    Some(d) if d != *v => format!(" (manual default {d})"),
                    _ => String::new(),
                };
                println!("  reg {i:>2} = {v:>6}  {name}{marker}");
            }
        }
        Err(e) => println!("bulk read reply failed to decode: {e:?} — raw: {frame:02x?}"),
    }
}

/// One request/response exchange: drain stale input, write the frame, then
/// read until `expect` bytes arrive or the port times out. Returns whatever
/// arrived (None if nothing), leaving frame validation to the caller.
fn transact(port: &mut dyn serialport::SerialPort, req: &[u8], expect: usize) -> Option<Vec<u8>> {
    let _ = port.clear(serialport::ClearBuffer::Input);
    port.write_all(req).ok()?;
    let _ = port.flush();
    let mut buf = Vec::with_capacity(expect);
    let mut chunk = [0u8; 64];
    loop {
        match port.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() >= expect {
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::TimedOut => break,
            Err(_) => break,
        }
    }
    // Modbus RTU inter-frame silence before the next attempt.
    std::thread::sleep(Duration::from_millis(5));
    (!buf.is_empty()).then_some(buf)
}
