//! The CDC wire protocol shared by the firmware and the dashboard: plain
//! `\n`-terminated text lines, no JSON. Device -> host is either a `$raw,pct`
//! telemetry line (streamed continuously) or an `OK`/`ERR` command reply.
//! Host -> device is a `?` status request or an `@`-prefixed command.

/// A parsed host -> device command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCmd {
    /// `?` — one-line status.
    Status,
    /// `@CAP` — capability/handshake (board, firmware version, serial, proto version).
    Cap,
    /// `@SETIDLE<raw>` — set pending idle to an explicit raw value (the host
    /// decides what value that is — e.g. an auto-calibration's observed min).
    SetIdle(i32),
    /// `@SETMAX<raw>` — set pending max to an explicit raw value.
    SetMax(i32),
    /// `@DZ<lo>,<hi>` — set pending deadzone percentages (0-100 each).
    SetDeadzone { lo: u8, hi: u8 },
    /// `@INV<0|1>` — set pending inversion.
    SetInverted(bool),
    /// `@SAVE` — commit the pending calibration to NVS.
    Save,
    /// `@CANCEL` — discard the pending calibration, revert to last-saved.
    Cancel,
    /// `@RESET` — wipe the persisted calibration back to uncalibrated defaults.
    Reset,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    Unknown,
    BadArgs,
}

/// Parse one host -> device command line (already trimmed of CR/LF by the caller).
pub fn parse_host_line(line: &str) -> Result<HostCmd, ParseError> {
    let line = line.trim();
    if line == "?" {
        return Ok(HostCmd::Status);
    }
    let rest = line.strip_prefix('@').ok_or(ParseError::Unknown)?;
    match rest {
        "CAP" => return Ok(HostCmd::Cap),
        "SAVE" => return Ok(HostCmd::Save),
        "CANCEL" => return Ok(HostCmd::Cancel),
        "RESET" => return Ok(HostCmd::Reset),
        _ => {}
    }
    if let Some(arg) = rest.strip_prefix("SETIDLE") {
        let raw: i32 = arg.trim().parse().map_err(|_| ParseError::BadArgs)?;
        return Ok(HostCmd::SetIdle(raw));
    }
    if let Some(arg) = rest.strip_prefix("SETMAX") {
        let raw: i32 = arg.trim().parse().map_err(|_| ParseError::BadArgs)?;
        return Ok(HostCmd::SetMax(raw));
    }
    if let Some(args) = rest.strip_prefix("DZ") {
        let mut it = args.split(',');
        let lo: u8 = it
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or(ParseError::BadArgs)?;
        let hi: u8 = it
            .next()
            .and_then(|s| s.trim().parse().ok())
            .ok_or(ParseError::BadArgs)?;
        if it.next().is_some() || lo > 100 || hi > 100 {
            return Err(ParseError::BadArgs);
        }
        return Ok(HostCmd::SetDeadzone { lo, hi });
    }
    if let Some(arg) = rest.strip_prefix("INV") {
        let v: u8 = arg.trim().parse().map_err(|_| ParseError::BadArgs)?;
        if v > 1 {
            return Err(ParseError::BadArgs);
        }
        return Ok(HostCmd::SetInverted(v != 0));
    }
    Err(ParseError::Unknown)
}

// ---- device -> host ----

/// Fixed `ERR` code vocabulary, kept small and stable so the dashboard can
/// show a real message instead of a bare "ERR".
pub mod err {
    /// `@SETMAX` captured a span too small to be usable (see `Calibration::span_ok`).
    pub const SPAN: &str = "span";
    /// A numeric argument was outside its valid range.
    pub const RANGE: &str = "range";
    /// The command line didn't parse.
    pub const PARSE: &str = "parse";
    /// NVS read/write failed.
    pub const NVS: &str = "nvs";
}

/// Format a `$<raw>,<pct_x10>\n` telemetry line. `output` is the 0..=65535
/// value from `Calibration::apply`; `pct_x10` is that same value rescaled to
/// 0..=1000 (one implied decimal place of percent) for compact, human-legible
/// framing.
pub fn format_telem(raw: i32, output: u16) -> String {
    let pct_x10 = (output as u32 * 1000 / 65535) as u16;
    format!("${raw},{pct_x10}\n")
}

/// A parsed `$raw,pct_x10` telemetry line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Telem {
    pub raw: i32,
    pub pct_x10: u16,
}

pub fn parse_telem_line(line: &str) -> Option<Telem> {
    let rest = line.trim().strip_prefix('$')?;
    let mut it = rest.split(',');
    let raw: i32 = it.next()?.parse().ok()?;
    let pct_x10: u16 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some(Telem { raw, pct_x10 })
}

/// A parsed `OK`/`ERR` command reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reply {
    Ok(String),
    Err(String),
}

pub fn parse_reply_line(line: &str) -> Option<Reply> {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("OK") {
        Some(Reply::Ok(rest.trim().to_string()))
    } else {
        line.strip_prefix("ERR")
            .map(|rest| Reply::Err(rest.trim().to_string()))
    }
}

/// Parse the flat `key=value key2=value2 ...` body of an `@CAP` reply.
pub fn parse_kv(s: &str) -> Vec<(&str, &str)> {
    s.split_whitespace()
        .filter_map(|tok| tok.split_once('='))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status() {
        assert_eq!(parse_host_line("?"), Ok(HostCmd::Status));
    }

    #[test]
    fn parses_zero_arg_commands() {
        assert_eq!(parse_host_line("@CAP"), Ok(HostCmd::Cap));
        assert_eq!(parse_host_line("@SAVE"), Ok(HostCmd::Save));
        assert_eq!(parse_host_line("@CANCEL"), Ok(HostCmd::Cancel));
        assert_eq!(parse_host_line("@RESET"), Ok(HostCmd::Reset));
    }

    #[test]
    fn parses_set_idle_and_max() {
        assert_eq!(parse_host_line("@SETIDLE1234"), Ok(HostCmd::SetIdle(1234)));
        assert_eq!(parse_host_line("@SETIDLE-500"), Ok(HostCmd::SetIdle(-500)));
        assert_eq!(parse_host_line("@SETMAX98765"), Ok(HostCmd::SetMax(98765)));
        assert_eq!(parse_host_line("@SETIDLEfoo"), Err(ParseError::BadArgs));
    }

    #[test]
    fn parses_deadzone() {
        assert_eq!(
            parse_host_line("@DZ5,12"),
            Ok(HostCmd::SetDeadzone { lo: 5, hi: 12 })
        );
    }

    #[test]
    fn rejects_out_of_range_deadzone() {
        assert_eq!(parse_host_line("@DZ101,0"), Err(ParseError::BadArgs));
        assert_eq!(parse_host_line("@DZ5"), Err(ParseError::BadArgs));
        assert_eq!(parse_host_line("@DZfoo,0"), Err(ParseError::BadArgs));
    }

    #[test]
    fn parses_inverted() {
        assert_eq!(parse_host_line("@INV1"), Ok(HostCmd::SetInverted(true)));
        assert_eq!(parse_host_line("@INV0"), Ok(HostCmd::SetInverted(false)));
        assert_eq!(parse_host_line("@INV2"), Err(ParseError::BadArgs));
    }

    #[test]
    fn rejects_unknown() {
        assert_eq!(parse_host_line("@NOPE"), Err(ParseError::Unknown));
        assert_eq!(parse_host_line("nope"), Err(ParseError::Unknown));
        assert_eq!(parse_host_line(""), Err(ParseError::Unknown));
    }

    #[test]
    fn telem_line_roundtrips() {
        let line = format_telem(-12345, 65535);
        assert_eq!(line, "$-12345,1000\n");
        assert_eq!(
            parse_telem_line(&line),
            Some(Telem {
                raw: -12345,
                pct_x10: 1000
            })
        );
    }

    #[test]
    fn telem_line_rejects_garbage() {
        assert_eq!(parse_telem_line("OK\n"), None);
        assert_eq!(parse_telem_line("$1,2,3\n"), None);
        assert_eq!(parse_telem_line("$notanumber,2\n"), None);
    }

    #[test]
    fn reply_line_parses_ok_and_err() {
        assert_eq!(
            parse_reply_line("OK board=lolin_s2_mini\n"),
            Some(Reply::Ok("board=lolin_s2_mini".to_string()))
        );
        assert_eq!(parse_reply_line("OK\n"), Some(Reply::Ok(String::new())));
        assert_eq!(
            parse_reply_line("ERR span\n"),
            Some(Reply::Err("span".to_string()))
        );
        assert_eq!(parse_reply_line("$1,2\n"), None);
    }

    #[test]
    fn kv_parses_cap_reply() {
        let kv = parse_kv("board=lolin_s2_mini fw=0.1.0 serial=PITHHB-ABC proto=1");
        assert_eq!(
            kv,
            vec![
                ("board", "lolin_s2_mini"),
                ("fw", "0.1.0"),
                ("serial", "PITHHB-ABC"),
                ("proto", "1"),
            ]
        );
    }
}
