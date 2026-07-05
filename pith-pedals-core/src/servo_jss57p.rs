//! JSS57P-series (P1.5N / P2N / P3N) Modbus RTU register map and guarded
//! write helpers — see `docs/pedals.md` §0. This drives the actual actuator
//! on the pedal build (a JSS57P3N), over the drive's 5-pin RS232 tuning
//! port (+5V, TXD, GND, RXD, NC). This is **not** the same product as the
//! `JSS57-R` (an RS485-bus servo with a different register map, whose
//! manual `modbus.rs` cites only for its generic CRC/framing test vectors —
//! that manual was fetched first by mistake, from an AliExpress listing for
//! the wrong sibling product, and does not apply to the register addresses
//! below).
//!
//! Register numbers are exactly the manufacturer's parameter numbers
//! (decimal, used directly as Modbus holding-register addresses), per:
//! <https://cdn.shopify.com/s/files/1/0014/4313/5560/files/Nema23_integrated_digital_hybrid_servo.pdf>
//! and a protocol brief the user compiled from it.
//!
//! STATUS — do not wire to real hardware yet (see `docs/pedals.md` §0):
//! - Physical layer (TTL vs true ±RS232 voltage on TXD/RXD) is UNVERIFIED —
//!   scope it before connecting to a 3.3V MCU UART.
//! - Serial parameters (baud, parity, slave ID) are UNPUBLISHED — the
//!   manual defers to the vendor's "Protuner" tool. Must be discovered on
//!   the bench (identity probe: read register 0, expect 57) before any of
//!   this talks to a real drive.
//! - FC 0x10 (write-multiple) support is unconfirmed for this drive; if
//!   unsupported, write 32-bit register pairs as two sequential FC 0x06
//!   writes instead.

#![allow(dead_code)]

use crate::modbus;

pub const REG_DRIVE_MODEL: u16 = 0; // RO — identity/link check, expect 57
pub const REG_LOOP_MODE: u16 = 1; // 0=open, 1=closed
pub const REG_MOTOR_TYPE: u16 = 2; // DO NOT WRITE
pub const REG_CURRENT_LOOP_KP: u16 = 3; // RO
pub const REG_CURRENT_LOOP_KI: u16 = 4; // RO
pub const REG_POSITION_LOOP_KP: u16 = 5;
pub const REG_SPEED_LOOP_KP: u16 = 6;
pub const REG_SPEED_LOOP_KI: u16 = 7;
pub const REG_MICROSTEPS_PER_REV: u16 = 8;
pub const REG_ENCODER_RESOLUTION: u16 = 9;
pub const REG_TRACKING_ERROR_ALARM: u16 = 10;
pub const REG_OPEN_LOOP_HOLD_CURRENT: u16 = 11; // x100mA
pub const REG_CLOSED_LOOP_PEAK_CURRENT: u16 = 12; // x100mA
pub const REG_PULSE_FILTER_TIME: u16 = 13; // x50us — the brief's designated safe write-test target
pub const REG_ENABLE_POLARITY: u16 = 14;
pub const REG_ALARM_OUTPUT_POLARITY: u16 = 15;
pub const REG_PULSE_INPUT_MODE: u16 = 16; // 0=PUL/DIR, 1=CW/CCW
pub const REG_PULSE_ACTIVE_EDGE: u16 = 17;
pub const REG_PEND_FUNCTION: u16 = 18; // 0=in-position, 1=brake
pub const REG_PEND_POLARITY: u16 = 19;
pub const REG_ACCEL_LO: u16 = 20;
pub const REG_ACCEL_HI: u16 = 21;
pub const REG_DECEL_LO: u16 = 22;
pub const REG_DECEL_HI: u16 = 23;
pub const REG_MAX_SPEED_LO: u16 = 24;
pub const REG_MAX_SPEED_HI: u16 = 25;
pub const REG_TARGET_PULSES_LO: u16 = 26;
pub const REG_TARGET_PULSES_HI: u16 = 27;
pub const REG_MOTION_COMMAND: u16 = 28;
pub const REG_POSITION_MODE: u16 = 29; // 0=incremental, 1=absolute
pub const REG_ABS_POSITION_LO: u16 = 30; // RO
pub const REG_ABS_POSITION_HI: u16 = 31; // RO
pub const REG_MOTION_STATE: u16 = 32; // RO — 1=complete, 0=moving
pub const REG_SAVE_TO_EEPROM: u16 = 33; // write 1 — only after params verified
pub const REG_FACTORY_RESET: u16 = 34; // NEVER WRITE

pub const IDENTITY_DRIVE_MODEL: u16 = 57;

/// Factory microsteps/revolution (register 8) for a JSS57P — the pulses-per-rev
/// the controller's step→mm conversions assume until the drive's reg 8 is read
/// back on the bench. The previous actuator (iSV57T) used 3750; this drive's
/// default is 400, so anything still hardcoding 3750 mis-scales travel ~9×.
pub const DEFAULT_MICROSTEPS_PER_REV: u16 = 400;

pub const MOTION_CMD_IDLE: u16 = 0;
pub const MOTION_CMD_POSITION_MOVE: u16 = 1;
pub const MOTION_CMD_VELOCITY_RUN: u16 = 2;
pub const MOTION_CMD_DECEL_STOP: u16 = 3;
pub const MOTION_CMD_HARD_STOP: u16 = 4;

/// Factory defaults, indexed by register address 0..=34, for the bring-up
/// "bulk-read and compare against defaults" verification step. `None` for
/// read-only/stateful/no-default entries. 32-bit pairs (accel/decel/max
/// speed/target pulses) are split low-register-first per the manual's own
/// worked example (register 26 reads 3200, register 27 reads 0 at reset).
pub const DEFAULTS: [Option<u16>; 35] = [
    Some(57),   // 0 drive model
    Some(1),    // 1 loop mode
    Some(0),    // 2 motor type
    None,       // 3 current loop Kp (RO)
    None,       // 4 current loop Ki (RO)
    Some(300),  // 5 position loop Kp
    Some(400),  // 6 speed loop Kp
    Some(80),   // 7 speed loop Ki
    Some(DEFAULT_MICROSTEPS_PER_REV), // 8 microsteps/rev
    Some(4000), // 9 encoder resolution
    Some(1000), // 10 tracking error alarm
    Some(30),   // 11 open-loop hold current
    Some(60),   // 12 closed-loop peak current
    Some(60),   // 13 pulse filter time
    Some(1),    // 14 enable polarity
    Some(0),    // 15 alarm output polarity
    Some(0),    // 16 pulse input mode
    Some(0),    // 17 pulse active edge
    Some(0),    // 18 PEND function
    Some(0),    // 19 PEND polarity
    Some(6400), // 20 accel lo
    Some(0),    // 21 accel hi
    Some(6400), // 22 decel lo
    Some(0),    // 23 decel hi
    Some(1600), // 24 max speed lo
    Some(0),    // 25 max speed hi
    Some(3200), // 26 target pulses lo
    Some(0),    // 27 target pulses hi
    Some(0),    // 28 motion command
    Some(0),    // 29 position mode
    None,       // 30 abs position lo (RO, stateful)
    None,       // 31 abs position hi (RO, stateful)
    Some(1),    // 32 motion state (RO)
    None,       // 33 save to EEPROM (write-only)
    None,       // 34 factory reset (never write)
];

/// Bench discovery candidates — baud/parity/slave ID are unpublished, so
/// bring-up has to scan these rather than assume a value. Do NOT assume the
/// iSV57T's/JSS57-R's 38400 8N1 default here; this is a different product.
pub const DISCOVERY_BAUD_CANDIDATES: [u32; 5] = [9600, 19200, 38400, 57600, 115200];
pub const DISCOVERY_SLAVE_ID_MIN: u8 = 1;
pub const DISCOVERY_SLAVE_ID_MAX: u8 = 31;

#[derive(Debug, PartialEq, Eq)]
pub enum GuardError {
    /// Register the manual says never to write (motor type, factory reset).
    Forbidden(u16),
    /// Register is read-only per the manual.
    ReadOnly(u16),
}

fn guard_write(addr: u16) -> Result<(), GuardError> {
    match addr {
        REG_MOTOR_TYPE | REG_FACTORY_RESET => Err(GuardError::Forbidden(addr)),
        REG_DRIVE_MODEL | REG_CURRENT_LOOP_KP | REG_CURRENT_LOOP_KI | REG_ABS_POSITION_LO
        | REG_ABS_POSITION_HI | REG_MOTION_STATE => Err(GuardError::ReadOnly(addr)),
        _ => Ok(()),
    }
}

/// Guarded wrapper over `modbus::encode_write_single` — refuses to encode a
/// frame for a register the manual says never to write, or that's
/// documented read-only. Callers still need to gate `REG_SAVE_TO_EEPROM`
/// themselves (only write it once bring-up parameters are verified).
pub fn encode_write_register(slave: u8, addr: u16, value: u16) -> Result<Vec<u8>, GuardError> {
    guard_write(addr)?;
    Ok(modbus::encode_write_single(slave, addr, value))
}

pub fn encode_read_register(slave: u8, addr: u16) -> Vec<u8> {
    modbus::encode_read_holding(slave, addr, 1)
}

/// Reads every register 0..=34 in one transaction — the bring-up "bulk-read
/// and compare against defaults" step.
pub fn encode_bulk_read(slave: u8) -> Vec<u8> {
    modbus::encode_read_holding(slave, 0, DEFAULTS.len() as u16)
}

/// FC 0x03 read of register 0 — the identity/link/baud/slave-ID probe.
pub fn encode_identity_probe(slave: u8) -> Vec<u8> {
    modbus::encode_read_holding(slave, REG_DRIVE_MODEL, 1)
}

pub fn is_identity_response(frame: &[u8]) -> bool {
    matches!(
        modbus::decode_read_holding_response(frame).as_deref(),
        Ok([v]) if *v == IDENTITY_DRIVE_MODEL
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_blocks_forbidden_and_readonly_registers() {
        assert!(matches!(
            encode_write_register(1, REG_MOTOR_TYPE, 0),
            Err(GuardError::Forbidden(REG_MOTOR_TYPE))
        ));
        assert!(matches!(
            encode_write_register(1, REG_FACTORY_RESET, 1),
            Err(GuardError::Forbidden(REG_FACTORY_RESET))
        ));
        assert!(matches!(
            encode_write_register(1, REG_DRIVE_MODEL, 57),
            Err(GuardError::ReadOnly(REG_DRIVE_MODEL))
        ));
        assert!(matches!(
            encode_write_register(1, REG_ABS_POSITION_LO, 0),
            Err(GuardError::ReadOnly(REG_ABS_POSITION_LO))
        ));
        assert!(matches!(
            encode_write_register(1, REG_MOTION_STATE, 0),
            Err(GuardError::ReadOnly(REG_MOTION_STATE))
        ));
    }

    #[test]
    fn guard_allows_the_designated_write_test_register() {
        assert!(encode_write_register(1, REG_PULSE_FILTER_TIME, 60).is_ok());
    }

    #[test]
    fn guard_allows_motion_command_and_target_registers() {
        assert!(encode_write_register(1, REG_MOTION_COMMAND, MOTION_CMD_POSITION_MOVE).is_ok());
        assert!(encode_write_register(1, REG_TARGET_PULSES_LO, 400).is_ok());
    }

    #[test]
    fn identity_probe_reads_register_zero() {
        assert_eq!(
            encode_identity_probe(1),
            modbus::encode_read_holding(1, 0, 1)
        );
    }

    #[test]
    fn bulk_read_covers_all_35_registers() {
        assert_eq!(encode_bulk_read(1), modbus::encode_read_holding(1, 0, 35));
    }

    #[test]
    fn identity_response_recognizes_drive_model_57() {
        // 57 decimal == 0x0039.
        let mut resp = vec![0x01, 0x03, 0x02, 0x00, 0x39];
        let crc = modbus::crc16(&resp);
        resp.push((crc & 0xFF) as u8);
        resp.push((crc >> 8) as u8);
        assert!(is_identity_response(&resp));
    }

    #[test]
    fn identity_response_rejects_wrong_model() {
        let mut resp = vec![0x01, 0x03, 0x02, 0x00, 0x01];
        let crc = modbus::crc16(&resp);
        resp.push((crc & 0xFF) as u8);
        resp.push((crc >> 8) as u8);
        assert!(!is_identity_response(&resp));
    }

    #[test]
    fn defaults_table_has_one_entry_per_register() {
        assert_eq!(DEFAULTS.len(), (REG_FACTORY_RESET + 1) as usize);
    }
}
