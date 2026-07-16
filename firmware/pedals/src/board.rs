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
//! both the ADC's `PIN_RST_U8` and `EMERGENCY_PIN_U8`. The v2.2b BOM has no
//! MCP4725, so GPIO4 is the brake FET's; GPIO6 stays shared (ADC RST +
//! EMRGNCY header).
//!
//! The COMPLETE board map — every module (hub, CH343 console, MAX3232,
//! optos, FETs), every connector, and the actual JSS drive's connector — is
//! `docs/pedals-board-map.md`, sourced from the v2.2b BOM + the reference
//! wiring docs. Highlights that matter here: the USB-C port hides an SL2.1S
//! hub (CH343P on UART0 = the console + hands-free flashing, plus the S3's
//! native USB = our TinyUSB HID), and the servo UART goes through a MAX3232
//! so the RS232 port is true ±RS232 levels by construction.

// Not consumed by any driver yet (Phase 1 has none) — kept here now that
// the hardware is confirmed, so Phase 2 starts from real numbers.
#![allow(dead_code)]

/// ADC SPI bus. This board's `PCB_VERSION == 9` block in the reference
/// firmware's `Main.h` never defines `USES_ADS1220`, which routes
/// `LoadCell.cpp` to its `#ifndef USES_ADS1220` branch — `<ADS1256.h>`
/// (7.68 MHz crystal, 2.5V vref) — NOT the ADS1220 used by the newer V6/V7
/// dev boards. The v2.2b BOM's actual chip is an **ADS1255** (U2): same SPI
/// command set and registers as the ADS1256, fewer mux inputs — irrelevant
/// here since the load cell is AIN0/AIN1 differential.
pub const ADC_DRDY: i32 = 15;
pub const ADC_RST: i32 = 6; // see module docs: shared with EMERGENCY_PIN
pub const ADC_SCK: i32 = 16;
pub const ADC_MISO: i32 = 18; // DOUT
pub const ADC_MOSI: i32 = 17; // DIN
pub const ADC_CS: i32 = 7;

/// Step/dir pins for a stepper actuator — NOT the chosen path (see
/// `ISV57_TX`/`ISV57_RX` below), kept here as-is from the reference
/// project's own pin table in case a future build wants pulse control.
pub const STEPPER_DIR: i32 = 37;
pub const STEPPER_STEP: i32 = 38;

/// Optional MCP4725 analog-output DAC (I2C) — see module docs re: GPIO4 reuse.
pub const MCP4725_SDA: i32 = 5;
pub const MCP4725_SCL: i32 = 4;

/// CFG1/CFG2 in the reference pin table (a pedal-type DIP on the v2.1
/// board). The v2.2b has NO DIP switch (confirmed on the physical board and
/// absent from its BOM) — pedal type is software-assigned, which is also
/// how the pith firmware works (identity via USB serial). Pins effectively
/// free on this revision.
pub const PEDAL_ASSIGN_CFG1: i32 = 1;
pub const PEDAL_ASSIGN_CFG2: i32 = 2;

pub const BUZZER: i32 = 21;

/// Actuator link (see docs/pedals.md §0): UART1 to the board's MAX3232
/// RS232 transceiver (CN3), wired the same way the reference project drives
/// its default iSV57T servo. This build's actuator is a JSSmotor/Dewo
/// JSS57P1.5N closed-loop stepper (confirmed on the unit: RS232 port +
/// PEND±/ALM± outputs), driven by Modbus RTU — see
/// `pith-pedals-core::servo_jss57p` for the register map/framing.
///
/// Levels are safe by construction through CN3 (the MAX3232 handles ±RS232;
/// the caution only applies if bypassing the board's port). Still to
/// discover on the bench: serial parameters — baud/parity/slave ID are
/// unpublished (candidates: 9600/19200/38400/57600/115200, 8N1, slave ID
/// 1-31 — success is register 0 reading back 57). Do not assume the
/// iSV57T's 38400 8N1.
pub const ISV57_TX: i32 = 10;
pub const ISV57_RX: i32 = 9;

/// Broken-out extensibility pins (README: "GPIO 33/34/35 are broken out");
/// 33 is the ESP-NOW pairing button in the reference firmware.
pub const PAIRING_BUTTON: i32 = 33;

/// PLANNED (not wired yet): the drive's ALM (alarm) / PEND (in-position)
/// outputs land on these broken-out pads. The drive HAS these outputs; it's
/// the PCBA that provides no terminal for them — the reference design just
/// leaves them unconnected (docs/pedals-board-map.md). These outputs are
/// typically open-collector pairs (ALM+/ALM−): wire the − side to GND and
/// the + side to the pad, input with internal pull-up, active-low. Confirm
/// the drive's output circuit before wiring — a 5V push-pull output would
/// need a divider, the S3 pins are NOT 5V-tolerant.
pub const DRIVE_ALM: i32 = 34;
pub const DRIVE_PEND: i32 = 35;

/// UART0 — the console. Routed to the onboard CH343P USB-UART bridge behind
/// the board's USB hub, so logs + hands-free flashing need no extra wiring
/// (the CH343 enumerates as its own CDC-ACM port alongside the S3's USB).
pub const UART0_TX: i32 = 43;
pub const UART0_RX: i32 = 44;

/// Onboard status LED: ONE WS2812 (NeoPixel) pixel, GRB order, 800 kHz —
/// the reference firmware drives it with `Adafruit_NeoPixel(1, LED_GPIO_U8,
/// NEO_GRB + NEO_KHZ800)` (`USING_LED` in `Main.cpp`), NOT a plain GPIO LED.
/// Driven here by `led.rs` via the espressif/led_strip RMT component.
pub const STATUS_LED: i32 = 12;
pub const BRAKE_RESISTOR: i32 = 4; // see module docs: shared with MCP4725_SCL
pub const SERVO_POWER_ENABLE: i32 = 3;
pub const EMERGENCY_STOP: i32 = 6; // see module docs: shared with ADC_RST
