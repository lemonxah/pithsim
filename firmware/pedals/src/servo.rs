//! JSS57P3N servo transport — the on-device half of the Modbus RTU driver.
//! All framing/register knowledge lives in the host-tested
//! `pith_pedals_core::{modbus, servo_jss57p}`; this module only moves those
//! frames over the ESP32 UART and applies timeouts/retries.
//!
//! **SAFETY — this stays DISARMED until the user explicitly arms it.** The
//! JSS57P3N's serial parameters (baud/parity/slave ID) are unpublished and
//! its RS232 port's electrical levels are unconfirmed against a 3.3V UART
//! (see `docs/pedals.md` §0 and `board.rs`). `Servo::new` opens the UART and
//! can run the identity probe, but `send_target` / `enable` refuse to
//! transmit motion commands unless `armed` is set — which only happens after
//! the bench discovery/verification sequence succeeds and the user sends
//! `@ARM`. Nothing here commands the motor on its own.

// This is a complete driver: the identity-probe / register-read / arm /
// disarm / hard-stop methods are the on-device bench-discovery + e-stop API
// (driven by the host workflow in docs/pedals.md §0), not all wired into the
// steady-state control loop yet — so some are dead until that workflow lands.
#![allow(dead_code)]

use esp_idf_svc::hal::delay::TickType;
use esp_idf_svc::hal::gpio::AnyIOPin;
use esp_idf_svc::hal::uart::{config, UartDriver};
use esp_idf_svc::hal::units::Hertz;
use esp_idf_svc::sys::EspError;

use pith_pedals_core::modbus;
use pith_pedals_core::servo_jss57p as reg;

/// Per-transaction read timeout — the brief recommends ≥100 ms during
/// discovery (the drive can be slow at low baud).
const TXN_TIMEOUT_MS: u64 = 120;
const MAX_REPLY_LEN: usize = 64;

pub struct Servo {
    uart: UartDriver<'static>,
    slave: u8,
    /// Motion commands are refused until this is set (post-bench-validation).
    armed: bool,
    /// Whether the identity probe has confirmed a JSS57P responding.
    identified: bool,
}

impl Servo {
    /// Opens the UART at `baud` (8N1) on the actuator pins. Does NOT enable
    /// or move the motor. Call [`Servo::probe_identity`] next to confirm the
    /// link before arming.
    pub fn new(
        uart: impl esp_idf_svc::hal::peripheral::Peripheral<P = impl esp_idf_svc::hal::uart::Uart>
            + 'static,
        tx: AnyIOPin,
        rx: AnyIOPin,
        baud: u32,
        slave: u8,
    ) -> Result<Self, EspError> {
        let cfg = config::Config::new().baudrate(Hertz(baud));
        let uart = UartDriver::new(
            uart,
            tx,
            rx,
            Option::<AnyIOPin>::None, // no CTS
            Option::<AnyIOPin>::None, // no RTS (RS232, no DE line)
            &cfg,
        )?;
        Ok(Servo {
            uart,
            slave,
            armed: false,
            identified: false,
        })
    }

    pub fn is_armed(&self) -> bool {
        self.armed
    }

    pub fn is_identified(&self) -> bool {
        self.identified
    }

    /// Arms motion output — ONLY call after the bench discovery/verification
    /// sequence has confirmed serial params and safe motion (see module
    /// docs). Refuses to arm if the identity probe hasn't passed.
    pub fn arm(&mut self) -> bool {
        if self.identified {
            self.armed = true;
        }
        self.armed
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// One request→reply Modbus transaction. Flushes RX, writes the frame,
    /// reads back up to `MAX_REPLY_LEN` bytes until the timeout.
    fn transact(&mut self, frame: &[u8]) -> Result<Vec<u8>, EspError> {
        // Drain any stale bytes.
        let mut scratch = [0u8; MAX_REPLY_LEN];
        while self
            .uart
            .read(&mut scratch, TickType::new_millis(0).ticks())?
            > 0
        {}

        self.uart.write(frame)?;
        self.uart
            .wait_tx_done(TickType::new_millis(TXN_TIMEOUT_MS).ticks())?;

        let mut out = Vec::new();
        let deadline_ticks = TickType::new_millis(TXN_TIMEOUT_MS).ticks();
        loop {
            let n = self.uart.read(&mut scratch, deadline_ticks)?;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&scratch[..n]);
            if out.len() >= MAX_REPLY_LEN {
                break;
            }
        }
        Ok(out)
    }

    /// Identity probe: read register 0, expect the drive-model value 57.
    /// Sets `identified` on success. Safe to call at any baud/slave while
    /// scanning — it never commands motion.
    pub fn probe_identity(&mut self) -> bool {
        let frame = reg::encode_identity_probe(self.slave);
        match self.transact(&frame) {
            Ok(reply) => {
                self.identified = reg::is_identity_response(&reply);
                self.identified
            }
            Err(_) => false,
        }
    }

    /// Reads a single holding register (no arming needed — read-only).
    pub fn read_register(&mut self, addr: u16) -> Option<u16> {
        let frame = reg::encode_read_register(self.slave, addr);
        let reply = self.transact(&frame).ok()?;
        modbus::decode_read_holding_response(&reply)
            .ok()
            .and_then(|v| v.first().copied())
    }

    /// Writes a single holding register, subject to the register guards in
    /// `servo_jss57p` (never touches forbidden/read-only registers). Requires
    /// arming for anything that could move the motor; a bare register write
    /// is allowed unarmed only for the safe bring-up write-test target
    /// (filter time) so discovery can verify writes stick.
    pub fn write_register(&mut self, addr: u16, value: u16) -> Result<(), ServoError> {
        if !self.armed && addr != reg::REG_PULSE_FILTER_TIME {
            return Err(ServoError::NotArmed);
        }
        let frame =
            reg::encode_write_register(self.slave, addr, value).map_err(ServoError::Guard)?;
        let reply = self.transact(&frame).map_err(ServoError::Esp)?;
        modbus::decode_write_single_ack(&reply, addr, value).map_err(ServoError::Modbus)
    }

    /// Commands an absolute target position (in servo pulses) via the
    /// position-move motion command. Requires arming. Splits the 32-bit
    /// target across the two target-pulse registers, then triggers the move.
    pub fn send_target(&mut self, target_pulses: i32) -> Result<(), ServoError> {
        if !self.armed {
            return Err(ServoError::NotArmed);
        }
        let lo = (target_pulses & 0xFFFF) as u16;
        let hi = ((target_pulses >> 16) & 0xFFFF) as u16;
        self.write_register(reg::REG_TARGET_PULSES_LO, lo)?;
        self.write_register(reg::REG_TARGET_PULSES_HI, hi)?;
        self.write_register(reg::REG_MOTION_COMMAND, reg::MOTION_CMD_POSITION_MOVE)
    }

    /// Emergency hard stop — the one motion write allowed to bypass the arm
    /// gate, so an e-stop path always works even before/without arming.
    pub fn hard_stop(&mut self) -> Result<(), ServoError> {
        let frame = reg::encode_write_register(
            self.slave,
            reg::REG_MOTION_COMMAND,
            reg::MOTION_CMD_HARD_STOP,
        )
        .map_err(ServoError::Guard)?;
        let reply = self.transact(&frame).map_err(ServoError::Esp)?;
        modbus::decode_write_single_ack(&reply, reg::REG_MOTION_COMMAND, reg::MOTION_CMD_HARD_STOP)
            .map_err(ServoError::Modbus)
    }
}

#[derive(Debug)]
pub enum ServoError {
    NotArmed,
    Guard(reg::GuardError),
    Modbus(modbus::ModbusError),
    Esp(EspError),
}
