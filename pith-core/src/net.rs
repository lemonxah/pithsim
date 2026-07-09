//! Pith WiFi transport wire protocol — shared by the firmware WiFi module
//! (`firmware/*/src/wifi.rs`) and the dashboard's UDP transport
//! (`dashboard/src/wifi.rs`). All packets are text, one per UDP datagram.
//!
//! Topology (see `docs/pedals.md` §4): each device joins the LAN as a WiFi
//! STA, broadcasts a discovery beacon, and once a dashboard subscribes,
//! streams its joystick axis + state to the dashboard and accepts the same
//! `@`-command protocol it speaks over USB. The dashboard feeds the axis into
//! a software virtual joystick (`dashboard/src/vjoy.rs`) so the game reads it
//! with no USB cable to the device. The game axis staying USB HID is still
//! supported — WiFi is an addition, not a replacement.

/// Devices broadcast beacons and send axis/state/reply packets to the
/// dashboard on this UDP port.
pub const DISCOVERY_PORT: u16 = 42424;
/// The dashboard sends commands (`@…`) and telemetry (`$…`) to a device on
/// this UDP port.
pub const DEVICE_PORT: u16 = 42425;

// ---- device -> dashboard packet prefixes ----

/// `PITH <type> <serial> <fw>` — periodic presence beacon (broadcast). `type`
/// is the device kind (`ddu`/`handbrake`/`pedals`).
pub const BEACON_PREFIX: &str = "PITH ";
/// `AX <serial> <value>` — the joystick axis, value 0..=65535 (same range as
/// the USB HID axis). Sent at the device's stream rate while subscribed.
pub const AXIS_PREFIX: &str = "AX ";
/// `BT <serial> <mask>` — the 32-button bitmask (the DDU's touch "button
/// box", same bits as its USB HID report). Sent when it changes, plus a
/// periodic refresh, while subscribed.
pub const BUTTONS_PREFIX: &str = "BT ";
/// `ST <serial> <text>` — device status line (the `?` reply body).
pub const STATE_PREFIX: &str = "ST ";
/// `RE <serial> <text>` — reply to a dashboard command.
pub const REPLY_PREFIX: &str = "RE ";

// ---- dashboard -> device ----

/// The dashboard announces itself so the device starts streaming to the
/// datagram's source address. Sent in reply to a beacon.
pub const SUBSCRIBE_CMD: &str = "@SUB";

/// Build a discovery beacon line (no trailing newline).
pub fn beacon(kind: &str, serial: &str, fw: &str) -> String {
    format!("{BEACON_PREFIX}{kind} {serial} {fw}")
}

/// Build an axis packet line.
pub fn axis_packet(serial: &str, value: u16) -> String {
    format!("{AXIS_PREFIX}{serial} {value}")
}

/// Build a buttons packet line.
pub fn buttons_packet(serial: &str, mask: u32) -> String {
    format!("{BUTTONS_PREFIX}{serial} {mask}")
}

/// Parsed device→dashboard packet.
#[derive(Debug, Clone, PartialEq)]
pub enum DevicePacket {
    Beacon {
        kind: String,
        serial: String,
        fw: String,
    },
    Axis {
        serial: String,
        value: u16,
    },
    Buttons {
        serial: String,
        mask: u32,
    },
    State {
        serial: String,
        text: String,
    },
    Reply {
        serial: String,
        text: String,
    },
}

/// Parse one device→dashboard datagram, if recognized.
pub fn parse_device_packet(line: &str) -> Option<DevicePacket> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix(BEACON_PREFIX) {
        let mut it = rest.splitn(3, ' ');
        let kind = it.next()?.to_string();
        let serial = it.next()?.to_string();
        let fw = it.next().unwrap_or("").to_string();
        return Some(DevicePacket::Beacon { kind, serial, fw });
    }
    if let Some(rest) = line.strip_prefix(AXIS_PREFIX) {
        let (serial, val) = rest.split_once(' ')?;
        return Some(DevicePacket::Axis {
            serial: serial.to_string(),
            value: val.trim().parse().ok()?,
        });
    }
    if let Some(rest) = line.strip_prefix(BUTTONS_PREFIX) {
        let (serial, mask) = rest.split_once(' ')?;
        return Some(DevicePacket::Buttons {
            serial: serial.to_string(),
            mask: mask.trim().parse().ok()?,
        });
    }
    if let Some(rest) = line.strip_prefix(STATE_PREFIX) {
        let (serial, text) = rest.split_once(' ')?;
        return Some(DevicePacket::State {
            serial: serial.to_string(),
            text: text.to_string(),
        });
    }
    if let Some(rest) = line.strip_prefix(REPLY_PREFIX) {
        let (serial, text) = rest.split_once(' ')?;
        return Some(DevicePacket::Reply {
            serial: serial.to_string(),
            text: text.to_string(),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beacon_round_trips() {
        let line = beacon("pedals", "PITHPEDAL-AABBCC", "0.1.0");
        assert_eq!(
            parse_device_packet(&line),
            Some(DevicePacket::Beacon {
                kind: "pedals".into(),
                serial: "PITHPEDAL-AABBCC".into(),
                fw: "0.1.0".into(),
            })
        );
    }

    #[test]
    fn axis_round_trips() {
        let line = axis_packet("PITHPEDAL-1", 32768);
        assert_eq!(
            parse_device_packet(&line),
            Some(DevicePacket::Axis {
                serial: "PITHPEDAL-1".into(),
                value: 32768,
            })
        );
    }

    #[test]
    fn buttons_round_trip() {
        let line = buttons_packet("PITH-DDU-1", 0x8000_0005);
        assert_eq!(
            parse_device_packet(&line),
            Some(DevicePacket::Buttons {
                serial: "PITH-DDU-1".into(),
                mask: 0x8000_0005,
            })
        );
    }

    #[test]
    fn state_and_reply_parse() {
        assert_eq!(
            parse_device_packet("ST PITHPEDAL-1 position=500 force=120"),
            Some(DevicePacket::State {
                serial: "PITHPEDAL-1".into(),
                text: "position=500 force=120".into(),
            })
        );
        assert_eq!(
            parse_device_packet("RE PITHPEDAL-1 OK"),
            Some(DevicePacket::Reply {
                serial: "PITHPEDAL-1".into(),
                text: "OK".into(),
            })
        );
    }

    #[test]
    fn unknown_lines_rejected() {
        assert_eq!(parse_device_packet("garbage"), None);
        assert_eq!(parse_device_packet("$1;2;3"), None);
    }
}
