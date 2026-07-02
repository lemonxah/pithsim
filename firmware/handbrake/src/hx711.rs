//! Bit-banged HX711 driver (no dedicated peripheral — this is a GPIO protocol)
//! plus a small integer smoothing filter. The ESP32-S2 has no hardware FPU, so
//! everything here is fixed-point/integer math.

use esp_idf_svc::hal::delay::Ets;
use esp_idf_svc::hal::gpio::{Gpio1, Gpio2, Input, Output, PinDriver};
use esp_idf_svc::hal::interrupt::IsrCriticalSection;
use esp_idf_svc::hal::peripheral::Peripheral;
use esp_idf_svc::sys::EspError;

/// Disables interrupts on this (single) core for the duration of the 24-bit
/// shift-in — a FreeRTOS tick landing mid-pulse would otherwise stretch a
/// clock edge out of the HX711's timing spec and corrupt a bit.
static CS: IsrCriticalSection = IsrCriticalSection::new();

pub struct Hx711<'d> {
    dout: PinDriver<'d, Gpio1, Input>,
    sck: PinDriver<'d, Gpio2, Output>,
}

impl<'d> Hx711<'d> {
    pub fn new(
        dout_pin: impl Peripheral<P = Gpio1> + 'd,
        sck_pin: impl Peripheral<P = Gpio2> + 'd,
    ) -> Result<Self, EspError> {
        let mut sck = PinDriver::output(sck_pin)?;
        sck.set_low()?;
        let dout = PinDriver::input(dout_pin)?;
        Ok(Hx711 { dout, sck })
    }

    /// True once a fresh conversion is ready to read (HX711 pulls DOUT low).
    pub fn is_ready(&self) -> bool {
        self.dout.is_low()
    }

    /// Read one 24-bit conversion (sign-extended to `i32`) if ready, else
    /// `None` — poll this from the main loop rather than blocking on it.
    /// Channel A / gain 128 (25 SCK pulses total), the HX711's default mode.
    pub fn try_read(&mut self) -> Option<i32> {
        if !self.is_ready() {
            return None;
        }
        let mut value: u32 = 0;
        {
            let _guard = CS.enter();
            for _ in 0..24 {
                let _ = self.sck.set_high();
                Ets::delay_us(1);
                value <<= 1;
                if self.dout.is_high() {
                    value |= 1;
                }
                let _ = self.sck.set_low();
                Ets::delay_us(1);
            }
            // 25th pulse: selects channel A / gain 128 for the NEXT conversion.
            let _ = self.sck.set_high();
            Ets::delay_us(1);
            let _ = self.sck.set_low();
            Ets::delay_us(1);
        }
        if value & 0x0080_0000 != 0 {
            value |= 0xFF00_0000; // sign-extend the 24-bit two's-complement value
        }
        Some(value as i32)
    }
}

/// A single-pole integer IIR filter (`y += (x - y) >> shift`) — cheap noise
/// smoothing for the raw ADC stream without any floating point. `shift: 0` is
/// a no-op passthrough (every sample counts as a full update immediately) —
/// any `shift > 0` trades response latency for smoothness, which matters a
/// lot at a HX711's typical 10 SPS (each shift level costs a real ~100ms of
/// settling time) and much less at 80 SPS (RATE pin tied high).
pub struct Iir {
    y: i32,
    shift: u32,
    primed: bool,
}

impl Iir {
    pub fn new(shift: u32) -> Self {
        Iir {
            y: 0,
            shift,
            primed: false,
        }
    }

    /// Feed one raw sample, return the filtered value. The very first sample
    /// seeds the filter directly so it doesn't ramp up slowly from zero.
    pub fn push(&mut self, x: i32) -> i32 {
        if !self.primed {
            self.y = x;
            self.primed = true;
        } else {
            self.y += (x - self.y) >> self.shift;
        }
        self.y
    }
}
