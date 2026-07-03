//! High-level command API for the Pith Handbrake over the shared `Hid`
//! transport: one blocking call per `@`-command (pith-hb-core's calibration
//! protocol on HID report id 2). Telemetry (`$raw,pct`) streams continuously
//! and can arrive interleaved with a command's reply, so `read_reply` skips
//! it while waiting for `OK`/`ERR`. Report id 1 (the axis) is never touched:
//! that's what the OS/games read directly through the joystick API.

use std::time::Instant;

use pith_hb_core::proto::{self, Reply};

use crate::hid::Hid;
use crate::{PID_HANDBRAKE, PITH_VID};

const CMD_TIMEOUT_MS: u64 = 500;

#[derive(Default)]
pub struct Handbrake {
    hid: Hid,
}

/// The `?` status reply, parsed from its `raw=.. pct=.. idle=.. max=.. dzlo=..
/// dzhi=.. inv=.. calibrated=..` key=value body.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub raw: i32,
    pub pct_x10: u16,
    pub idle_raw: i32,
    pub max_raw: i32,
    pub deadzone_lo_pct: u8,
    pub deadzone_hi_pct: u8,
    pub inverted: bool,
    pub calibrated: bool,
}

impl Status {
    fn parse(data: &str) -> Option<Self> {
        let kv = proto::parse_kv(data);
        let get = |key: &str| kv.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        Some(Status {
            raw: get("raw")?.parse().ok()?,
            pct_x10: get("pct")?.parse().ok()?,
            idle_raw: get("idle")?.parse().ok()?,
            max_raw: get("max")?.parse().ok()?,
            deadzone_lo_pct: get("dzlo")?.parse().ok()?,
            deadzone_hi_pct: get("dzhi")?.parse().ok()?,
            inverted: get("inv")? == "1",
            calibrated: get("calibrated")? == "1",
        })
    }
}

impl Handbrake {
    /// Open the HID connection to the (single) handbrake by VID/PID — there's
    /// no ambiguity to resolve like a serial-port picker would need.
    pub fn connect(&mut self) -> bool {
        self.hid.open(PITH_VID, PID_HANDBRAKE)
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
                return None; // read_line timed out
            }
            if line.starts_with('$') {
                continue; // telemetry, not our reply — keep waiting
            }
            if let Some(r) = proto::parse_reply_line(&line) {
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
                proto::parse_kv(&data)
                    .into_iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
            ),
            Reply::Err(_) => None,
        }
    }

    /// Set pending idle to an explicit raw value (e.g. an auto-calibration's
    /// observed minimum) rather than "whatever the sensor reads right now".
    pub fn set_idle(&mut self, raw: i32) -> bool {
        matches!(self.command(&format!("@SETIDLE{raw}")), Some(Reply::Ok(_)))
    }

    /// `Err(code)` uses the fixed vocabulary in `pith_hb_core::proto::err`
    /// (e.g. `"span"` if idle/max ended up too close together).
    pub fn set_max(&mut self, raw: i32) -> Result<(), String> {
        match self.command(&format!("@SETMAX{raw}")) {
            Some(Reply::Ok(_)) => Ok(()),
            Some(Reply::Err(code)) => Err(code),
            None => Err("timeout".to_string()),
        }
    }

    pub fn set_deadzone(&mut self, lo: u8, hi: u8) -> bool {
        matches!(self.command(&format!("@DZ{lo},{hi}")), Some(Reply::Ok(_)))
    }

    pub fn set_inverted(&mut self, inverted: bool) -> bool {
        matches!(
            self.command(&format!("@INV{}", inverted as u8)),
            Some(Reply::Ok(_))
        )
    }

    pub fn save(&mut self) -> bool {
        matches!(self.command("@SAVE"), Some(Reply::Ok(_)))
    }

    pub fn cancel(&mut self) -> bool {
        matches!(self.command("@CANCEL"), Some(Reply::Ok(_)))
    }

    pub fn reset(&mut self) -> bool {
        matches!(self.command("@RESET"), Some(Reply::Ok(_)))
    }

    /// `?` status: the calibration currently in effect (the firmware's
    /// "pending" set — equal to the saved one except mid-wizard-edit) plus the
    /// latest raw/output sample. The dashboard calls this after every command
    /// that can change calibration so its UI reflects the firmware, not a
    /// client-side guess.
    pub fn status(&mut self) -> Option<Status> {
        match self.command("?")? {
            Reply::Ok(data) => Status::parse(&data),
            Reply::Err(_) => None,
        }
    }

    /// Poll the next telemetry sample (non-blocking beyond `timeout_ms`). Any
    /// stray `OK`/`ERR` line seen here (there shouldn't be one outside a
    /// `command()` call) is silently dropped — this path only reports telemetry.
    pub fn read_telem(&mut self, timeout_ms: u64) -> Option<proto::Telem> {
        let line = self.hid.read_line(timeout_ms);
        if line.is_empty() {
            return None;
        }
        proto::parse_telem_line(&line)
    }
}

impl Handbrake {
    /// Stream a new app image over the `@OTA` protocol (same dialect as the
    /// DDU): `@OTA<size>` -> OTAREADY, then raw bytes ACK'd per 2048 with "K",
    /// OTADONE + device reboot on success. The device stops its telemetry
    /// stream during the transfer; any stale line is skipped while waiting.
    pub fn ota_upload(
        &mut self,
        img: &[u8],
        mut on_progress: impl FnMut(i32),
    ) -> Result<(), String> {
        if !self.hid.write_str(&format!("@OTA{}\n", img.len())) {
            return Err("write failed".to_string());
        }
        // esp_ota_begin erases the target slot first — allow a generous window.
        self.ota_wait("OTAREADY", 12000)?;
        const ACK: usize = 2048;
        let mut sent = 0usize;
        while sent < img.len() {
            let end = std::cmp::min(sent + ACK, img.len());
            if !self.hid.write(&img[sent..end]) {
                return Err(format!("write error at {sent}"));
            }
            sent = end;
            if sent < img.len() {
                self.ota_wait("K", 6000)
                    .map_err(|e| format!("no ack at {sent}: {e}"))?;
            }
            on_progress((sent * 100 / img.len()) as i32);
        }
        // Device sends OTADONE then reboots — a missing reply (already
        // rebooting) is fine; only an explicit OTAERR is a failure.
        match self.ota_wait("OTADONE", 8000) {
            Ok(()) => Ok(()),
            Err(e) if e.contains("OTAERR") => Err(e),
            Err(_) => Ok(()), // timed out: device already rebooting
        }
    }

    /// Wait for an OTA reply containing `needle`, skipping unrelated telemetry
    /// or status lines that share the channel.
    fn ota_wait(&mut self, needle: &str, ms: u64) -> Result<(), String> {
        let t0 = Instant::now();
        loop {
            let elapsed = t0.elapsed().as_millis() as u64;
            if elapsed >= ms {
                return Err(format!("timed out waiting for {needle}"));
            }
            let line = self.hid.read_line((ms - elapsed).clamp(1, 2000));
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if t.contains("OTAERR") {
                return Err("device error: OTAERR".to_string());
            }
            if t == needle || (needle.len() > 2 && t.contains(needle)) {
                return Ok(());
            }
            // else: stale/interleaved line — skip and keep waiting.
        }
    }
}
