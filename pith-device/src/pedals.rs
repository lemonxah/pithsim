//! High-level command API for the Pith active pedal over the shared `Hid`
//! transport: JSON config/action/state on HID report id 2
//! (`pith_pedals_core::protocol`), matching `firmware/pedals/src/usb.rs`'s
//! `@CFG{json}` / `@GETCFG` / `@ACT{json}` / `?` / `@CAP` dispatch. Unlike
//! the DDU/handbrake's plain key=value replies, config/state round-trip as
//! JSON bodies inline in the `OK` reply (`OK{...}`).

use std::time::Instant;

use pith_pedals_core::protocol::{PedalAction, PedalConfig};

use crate::hid::Hid;
use crate::{PID_PEDALS, PITH_VID};

const CMD_TIMEOUT_MS: u64 = 500;

#[derive(Default)]
pub struct Pedals {
    hid: Hid,
}

/// Live device state, parsed from the `?` reply's `position=.. force=..
/// joy=.. err=.. servo=..` key=value body (mirrors the handbrake's `Status`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PedalStatus {
    pub position_pct_x10: u16,
    pub force_n_x10: u16,
    pub joystick_output: u16,
    pub error_code: u8,
    pub servo_on: bool,
}

impl PedalStatus {
    fn parse(body: &str) -> Option<Self> {
        let kv: Vec<(&str, &str)> = body
            .split_whitespace()
            .filter_map(|tok| tok.split_once('='))
            .collect();
        let get = |key: &str| kv.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        Some(PedalStatus {
            position_pct_x10: get("position")?.parse().ok()?,
            force_n_x10: get("force")?.parse().ok()?,
            joystick_output: get("joy")?.parse().ok()?,
            error_code: get("err")?.parse().ok()?,
            servo_on: get("servo")? == "1",
        })
    }
}

enum Reply {
    Ok(String),
    Err(String),
}

fn parse_reply_line(line: &str) -> Option<Reply> {
    if let Some(rest) = line.strip_prefix("OK") {
        Some(Reply::Ok(rest.to_string()))
    } else {
        line.strip_prefix("ERR")
            .map(|rest| Reply::Err(rest.trim().to_string()))
    }
}

impl Pedals {
    /// Open the HID connection to a pedal by VID/PID. With multiple pedals
    /// on the same PID, callers needing a specific one should enumerate by
    /// serial via `hidapi` directly — this mirrors `Handbrake::connect`'s
    /// "just the one" simplicity for the common single-pedal-per-run case.
    pub fn connect(&mut self) -> bool {
        self.hid.open(PITH_VID, PID_PEDALS)
    }

    pub fn close(&mut self) {
        self.hid.close()
    }

    pub fn is_open(&self) -> bool {
        self.hid.is_open()
    }

    fn read_reply(&mut self, timeout_ms: u64) -> Option<Reply> {
        let t0 = Instant::now();
        loop {
            let elapsed = t0.elapsed().as_millis() as u64;
            if elapsed >= timeout_ms {
                return None;
            }
            let line = self.hid.read_line(timeout_ms - elapsed);
            if line.is_empty() {
                return None;
            }
            if let Some(r) = parse_reply_line(&line) {
                return Some(r);
            }
        }
    }

    fn command(&mut self, cmd: &str) -> Option<Reply> {
        if !self.hid.write_str(&format!("{cmd}\n")) {
            return None;
        }
        self.read_reply(CMD_TIMEOUT_MS)
    }

    /// `@CAP` handshake: board/firmware/serial/protocol-version key=value pairs.
    pub fn capabilities(&mut self) -> Option<Vec<(String, String)>> {
        match self.command("@CAP")? {
            Reply::Ok(data) => Some(
                data.split_whitespace()
                    .filter_map(|tok| tok.split_once('='))
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            Reply::Err(_) => None,
        }
    }

    /// `@GETCFG`: read the device's current configuration back.
    pub fn get_config(&mut self) -> Option<PedalConfig> {
        match self.command("@GETCFG")? {
            Reply::Ok(json) => serde_json::from_str(&json).ok(),
            Reply::Err(_) => None,
        }
    }

    /// `@CFG{json}`: push a new configuration (curve, effect gains, geometry, …).
    pub fn set_config(&mut self, cfg: &PedalConfig) -> Result<(), String> {
        let json = serde_json::to_string(cfg).map_err(|e| e.to_string())?;
        match self.command(&format!("@CFG{json}")) {
            Some(Reply::Ok(_)) => Ok(()),
            Some(Reply::Err(code)) => Err(code),
            None => Err("timeout".to_string()),
        }
    }

    /// `@ACT{json}`: push the live effect action (called every tick by the
    /// dashboard's effects engine — this is what replaces the SimHub
    /// plugin's per-frame `payloadPedalAction` writes).
    pub fn send_action(&mut self, action: &PedalAction) -> Result<(), String> {
        let json = serde_json::to_string(action).map_err(|e| e.to_string())?;
        match self.command(&format!("@ACT{json}")) {
            Some(Reply::Ok(_)) => Ok(()),
            Some(Reply::Err(code)) => Err(code),
            None => Err("timeout".to_string()),
        }
    }

    /// `?` status: live position/force/joystick-output/error/servo state.
    pub fn status(&mut self) -> Option<PedalStatus> {
        match self.command("?")? {
            Reply::Ok(data) => PedalStatus::parse(&data),
            Reply::Err(_) => None,
        }
    }
}
