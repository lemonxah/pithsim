//! Shared device transport for all Pith sim-racing gear: the HID
//! command/log channel, the serial fallback, the DDU `Dash` abstraction
//! (including the `@OTA` firmware-upload protocol), the `Handbrake`
//! calibration API, and the `Pedals` config/action/state API. Used by the
//! dashboard GUI and the `pith-flash` CLI so every host tool speaks to the
//! devices through exactly one code path.

pub mod dash;
pub mod handbrake;
pub mod hid;
pub mod pedals;
pub mod serial;

pub use dash::Dash;
pub use handbrake::Handbrake;
pub use hid::device_present;
pub use pedals::Pedals;
pub use serial::{PortInfo, Serial};

/// All Pith devices enumerate under the Espressif VID with a Pith-allocated
/// PID per device type. MUST match the `idVendor`/`idProduct` in each
/// firmware's USB descriptor (firmware/<device>/components/*/…usb.c).
pub const PITH_VID: u16 = 0x303A;
/// Pith DDU (firmware/ddu, XIAO ESP32-S3).
pub const PID_DDU: u16 = 0x4002;
/// Pith Handbrake (firmware/handbrake, Lolin S2 Mini + HX711).
pub const PID_HANDBRAKE: u16 = 0x8001;
/// Pith active pedal (firmware/pedals; see docs/pedals.md). One board per
/// pedal (clutch/brake/throttle), same PID — they're told apart by serial.
pub const PID_PEDALS: u16 = 0x8002;
// Future gear: allocate the next 0x80xx PID here and list it in DEVICE_PIDS
// so enumeration + udev docs stay in one place.

/// Legacy alias for the DDU's PID (predates the multi-device monorepo).
pub const PITH_PID: u16 = PID_DDU;

/// Every known Pith device type, for "what's plugged in" enumeration.
pub const DEVICE_PIDS: &[(u16, &str)] = &[
    (PID_DDU, "DDU"),
    (PID_HANDBRAKE, "Handbrake"),
    (PID_PEDALS, "Pedals"),
];
