//! Pin map for the confirmed target hardware: gilphilbert's "PCBA V2" control
//! board, v2.2 Rev B (`gilphilbert/DIY-Sim-Racing-FFB-Pedal-PCBs`,
//! `v2/v2.2/RevB`) — an integrated control+power board built around an
//! **ESP32-S3FH4R2** (confirmed from that board's BOM). In the reference
//! firmware this is `PCB_VERSION == 9` in `ESP32/include/Main.h` (the
//! `ControlBoard_PCBA_V2X` PlatformIO env), reproduced here verbatim from
//! that `#if` block — not yet wired to any driver (Phase 1, see
//! `docs/pedals.md`); this module exists so the real pin assignments are on
//! record before Phase 2 needs them.
//!
//! Two pin numbers are reused for two different purposes in the reference
//! source itself (not a transcription error on this end — verified by
//! reading `Main.h` directly): GPIO4 is both `MCP_SCL_U8` (I2C for an
//! optional MCP4725 analog-output DAC) and `BRAKE_RESISTOR_PIN_U8`; GPIO6 is
//! both the ADC's `PIN_RST_U8` and `EMERGENCY_PIN_U8`. Confirm against the
//! actual schematic before wiring anything to these — the MCP4725/emergency-
//! button features may simply be unpopulated on this SKU, freeing the pin
//! for its other use, but that's an inference, not something read from a
//! schematic.

// Not consumed by any driver yet (Phase 1 has none) — kept here now that
// the hardware is confirmed, so Phase 2 starts from real numbers.
#![allow(dead_code)]

/// ADC SPI bus. This board's `PCB_VERSION == 9` block in the reference
/// firmware's `Main.h` never defines `USES_ADS1220`, which routes
/// `LoadCell.cpp` to its `#ifndef USES_ADS1220` branch — `<ADS1256.h>`
/// (7.68 MHz crystal, 2.5V vref) — so this board uses an **ADS1256**, not
/// the ADS1220 used by the newer V6/V7 dev boards (PCB_VERSION 13/14,
/// which DO define `USES_ADS1220`).
pub const ADC_DRDY: i32 = 15;
pub const ADC_RST: i32 = 6; // see module docs: shared with EMERGENCY_PIN
pub const ADC_SCK: i32 = 16;
pub const ADC_MISO: i32 = 18; // DOUT
pub const ADC_MOSI: i32 = 17; // DIN
pub const ADC_CS: i32 = 7;

/// Stepper step/dir — present in the reference firmware's pin table for
/// this board, but the v2.2 board is normally paired with the iSV57T servo
/// (see the RS232 pins below); confirm which actuator path is populated.
pub const STEPPER_DIR: i32 = 37;
pub const STEPPER_STEP: i32 = 38;

/// Optional MCP4725 analog-output DAC (I2C) — see module docs re: GPIO4 reuse.
pub const MCP4725_SDA: i32 = 5;
pub const MCP4725_SCL: i32 = 4;

/// SW1 pedal-type DIP switch (throttle/brake/clutch assignment).
pub const PEDAL_ASSIGN_CFG1: i32 = 1;
pub const PEDAL_ASSIGN_CFG2: i32 = 2;

pub const BUZZER: i32 = 21;

/// UART to the iSV57T servo's RS232 interface chip.
pub const ISV57_TX: i32 = 10;
pub const ISV57_RX: i32 = 9;

/// Broken-out extensibility pin (README: "GPIO 33/34/35 are broken out");
/// used as the ESP-NOW pairing button in the reference firmware.
pub const PAIRING_BUTTON: i32 = 33;

pub const STATUS_LED: i32 = 12;
pub const BRAKE_RESISTOR: i32 = 4; // see module docs: shared with MCP4725_SCL
pub const SERVO_POWER_ENABLE: i32 = 3;
pub const EMERGENCY_STOP: i32 = 6; // see module docs: shared with ADC_RST
