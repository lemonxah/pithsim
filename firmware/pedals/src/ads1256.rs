//! ADS1256 loadcell ADC driver — a faithful Rust port of the exact library
//! the reference firmware pins (`ChrGri/ADS1255-ADS1256`) plus the init
//! sequence `LoadCell.cpp` performs on it, against the pins in
//! [`crate::board`] (this board routes the cell to AIN0/AIN1 differential).
//! Scaling/bias math lives in `pith_pedals_core::loadcell` so it's
//! host-testable; this module only moves bytes.
//!
//! Timing notes carried over from that library:
//! - SPI mode 1, MSB first, clock = ADC crystal / 4 (7.68 MHz / 4 = 1.92 MHz).
//! - t6 (command → data) ≈ 6.5 µs, rounded up to 7 µs.
//! - CS is held low across a whole RDATA sequence, so CS is driven manually
//!   here rather than by the SPI peripheral.
//! - DRDY falling edge = sample ready; the reference blocks a dedicated task
//!   on an ISR-fed semaphore, mirrored here as a GPIO ISR feeding a FreeRTOS
//!   task notification.

use std::num::NonZeroU32;

use esp_idf_svc::hal::delay::Ets;
use esp_idf_svc::hal::gpio::{AnyIOPin, Input, InterruptType, Output, PinDriver};
use esp_idf_svc::hal::spi::{config as spi_config, SpiDeviceDriver, SpiDriver, SpiDriverConfig};
use esp_idf_svc::hal::task::notification::Notification;
use esp_idf_svc::sys::EspError;

use pith_pedals_core::loadcell;

// Command opcodes (ADS1256 datasheet, table 24).
const CMD_WAKEUP: u8 = 0x00;
const CMD_RDATA: u8 = 0x01;
const CMD_SDATAC: u8 = 0x0F;
const CMD_RREG: u8 = 0x10;
const CMD_WREG: u8 = 0x50;
const CMD_SELFCAL: u8 = 0xF0;
const CMD_SYNC: u8 = 0xFC;

// Register addresses.
const REG_MUX: u8 = 0x01;
const REG_ADCON: u8 = 0x02;
const REG_DRATE: u8 = 0x03;

pub struct Ads1256 {
    spi: SpiDeviceDriver<'static, SpiDriver<'static>>,
    cs: PinDriver<'static, AnyIOPin, Output>,
    drdy: PinDriver<'static, AnyIOPin, Input>,
    // Fed by the DRDY ISR; consumed only by the blocking `wait_and_read`
    // path (see its note) — the polling `try_read` path doesn't touch it.
    #[allow(dead_code)]
    notification: Notification,
}

impl Ads1256 {
    /// Builds the driver and runs the reference's init sequence: SDATAC,
    /// data rate 2000 SPS, PGA 64, no input buffer, SELFCAL, then MUX to
    /// differential AIN0/AIN1. Blocks (polling DRDY) through calibration,
    /// so call it from the control task, not an ISR.
    pub fn new(
        spi2: esp_idf_svc::hal::spi::SPI2,
        sck: AnyIOPin,
        mosi: AnyIOPin,
        miso: AnyIOPin,
        cs: AnyIOPin,
        drdy: AnyIOPin,
    ) -> Result<Self, EspError> {
        let baud_hz = (loadcell::ADC_CLOCK_MHZ * 1_000_000.0 / 4.0) as u32;
        let spi = SpiDeviceDriver::new_single(
            spi2,
            sck,
            mosi,
            Some(miso),
            Option::<AnyIOPin>::None, // CS is manual (held across sequences)
            &SpiDriverConfig::new(),
            &spi_config::Config::new()
                .baudrate(baud_hz.into())
                .data_mode(spi_config::MODE_1),
        )?;

        let mut cs = PinDriver::output(cs)?;
        cs.set_high()?;
        let mut drdy = PinDriver::input(drdy)?;
        drdy.set_interrupt_type(InterruptType::NegEdge)?;

        let notification = Notification::new();
        let notifier = notification.notifier();
        // Safety: the callback only touches the ISR-safe notifier.
        unsafe {
            drdy.subscribe(move || {
                notifier.notify_and_yield(NonZeroU32::new(1).unwrap());
            })?;
        }

        let mut adc = Ads1256 {
            spi,
            cs,
            drdy,
            notification,
        };
        adc.begin()?;
        adc.set_channel(loadcell::ADC_CHANNEL_P, loadcell::ADC_CHANNEL_N)?;
        Ok(adc)
    }

    /// `ADS1256::begin(drate, gain, false)` + the surrounding LoadCell.cpp
    /// calls, in the same order.
    fn begin(&mut self) -> Result<(), EspError> {
        self.command_after_drdy(CMD_SDATAC)?;
        self.write_register(REG_DRATE, loadcell::ADC_DRATE_2000SPS)?;
        let adcon = self.read_register(REG_ADCON)?;
        self.write_register(REG_ADCON, (adcon & !0x07) | loadcell::ADC_GAIN_CODE)?;
        // buffenable=false in the reference — STATUS register untouched.
        self.command_after_drdy(CMD_SELFCAL)?;
        self.wait_drdy_poll();
        Ok(())
    }

    /// `setChannel(P, N)`: MUX write + SYNC + WAKEUP restarts conversion on
    /// the new differential pair.
    fn set_channel(&mut self, p: u8, n: u8) -> Result<(), EspError> {
        self.write_register(REG_MUX, (p << 4) | (n & 0x0F))?;
        self.command_after_drdy(CMD_SYNC)?;
        self.command_after_drdy(CMD_WAKEUP)?;
        Ok(())
    }

    fn write_register(&mut self, reg: u8, value: u8) -> Result<(), EspError> {
        self.cs.set_low()?;
        self.spi.write(&[CMD_WREG | reg, 0, value])?;
        Ets::delay_us(1);
        self.cs.set_high()?;
        Ok(())
    }

    fn read_register(&mut self, reg: u8) -> Result<u8, EspError> {
        self.cs.set_low()?;
        self.spi.write(&[CMD_RREG | reg, 0])?;
        Ets::delay_us(7); // t6
        let mut out = [0u8];
        self.spi.transfer(&mut out, &[0u8])?;
        Ets::delay_us(1); // t11
        self.cs.set_high()?;
        Ok(out[0])
    }

    /// The library's `sendCommand`: waits for DRDY low (polling — only used
    /// during init/channel setup), then clocks out one opcode.
    fn command_after_drdy(&mut self, cmd: u8) -> Result<(), EspError> {
        self.cs.set_low()?;
        self.wait_drdy_poll();
        self.spi.write(&[cmd])?;
        Ets::delay_us(1);
        self.cs.set_high()?;
        Ok(())
    }

    fn wait_drdy_poll(&self) {
        while self.drdy.is_high() {
            // 2000 SPS → ≤500 µs; busy-wait like the reference's waitDRDY().
        }
    }

    /// Blocks on the next DRDY falling edge (ISR → task notification) and
    /// reads the signed 24-bit conversion. Mirrors the reference's
    /// semaphore-blocked `readLoadcellWeightInKg` inner loop, including the
    /// "discard if DRDY isn't actually low" guard. Not used by the current
    /// single-threaded loop (which polls via `try_read`) — kept for a future
    /// dedicated-task design that matches the reference's `loadcellReadingTask`.
    #[allow(dead_code)]
    pub fn wait_and_read(&mut self) -> Result<Option<i32>, EspError> {
        self.drdy.enable_interrupt()?;
        self.notification.wait(esp_idf_svc::hal::delay::BLOCK);
        if self.drdy.is_high() {
            return Ok(None); // spurious/late — skip this one
        }
        self.read_data().map(Some)
    }

    /// Non-blocking read: if DRDY is low a fresh sample is ready, read it;
    /// otherwise return `None` immediately. Lets the single-threaded main
    /// loop poll the ADC between USB service without blocking (the 2 kSPS
    /// DRDY cadence means a sample is ready almost every 500 µs anyway).
    pub fn try_read(&mut self) -> Result<Option<i32>, EspError> {
        if self.drdy.is_low() {
            self.read_data().map(Some)
        } else {
            Ok(None)
        }
    }

    /// RDATA: 24-bit two's-complement conversion result.
    fn read_data(&mut self) -> Result<i32, EspError> {
        self.cs.set_low()?;
        self.spi.write(&[CMD_RDATA])?;
        Ets::delay_us(7); // t6
        let mut bytes = [0u8; 3];
        self.spi.transfer(&mut bytes, &[0u8; 3])?;
        self.cs.set_high()?;
        let raw = ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32;
        // Sign-extend 24 -> 32 bits.
        let signed = if raw & 0x0080_0000 != 0 {
            (raw | 0xFF00_0000) as i32
        } else {
            raw as i32
        };
        Ok(signed)
    }
}
