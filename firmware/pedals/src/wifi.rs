//! The pedal's WiFi transport is the shared `pith-fw-wifi` module — this file
//! just re-exports it so `crate::wifi::…` keeps working. Device-specific
//! options (kind = "pedals", streams a joystick axis) are passed at spawn in
//! `main.rs`.

pub use pith_fw_wifi::{spawn, OtaHooks, WifiOpts, WifiShared};
