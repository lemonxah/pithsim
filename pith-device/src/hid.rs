use hidapi::{HidApi, HidDevice};
use std::time::{Duration, Instant};

/// True if a device matching `vid`/`pid` is currently plugged in — a cheap
/// presence check that doesn't open (and so doesn't lock out) the device.
pub fn device_present(vid: u16, pid: u16) -> bool {
    let Ok(api) = HidApi::new() else {
        return false;
    };
    let found = api
        .device_list()
        .any(|d| d.vendor_id() == vid && d.product_id() == pid);
    found
}

#[derive(Default)]
pub struct Hid {
    api: Option<HidApi>,
    dev: Option<HidDevice>,
    rx: Vec<u8>,         // report id 2 — command-reply bytes
    log_acc: Vec<u8>,    // report id 3 — partial device-log line
    logs: Vec<String>,   // complete device-log lines awaiting the UI
}

impl Hid {
    pub fn is_open(&self) -> bool {
        self.dev.is_some()
    }

    pub fn open(&mut self, vid: u16, pid: u16) -> bool {
        self.close();
        if self.api.is_none() {
            self.api = HidApi::new().ok();
        }
        let api = match self.api.as_ref() {
            Some(a) => a,
            None => return false,
        };
        match api.open(vid, pid) {
            Ok(d) => {
                let _ = d.set_blocking_mode(false);
                self.dev = Some(d);
                self.rx.clear();
                true
            }
            Err(_) => false,
        }
    }

    pub fn close(&mut self) {
        self.dev = None;
    }

    /// Route one received HID report by report id: id 2 → command-reply bytes,
    /// id 3 → device-log lines. `r` is the byte count returned by read_timeout.
    fn route(&mut self, buf: &[u8], r: usize) {
        if r < 2 {
            return;
        }
        let id = buf[0];
        let mut n = buf[1] as usize;
        if n > r - 2 {
            n = r - 2;
        }
        if n == 0 {
            return;
        }
        let payload = &buf[2..2 + n];
        match id {
            0x02 => self.rx.extend_from_slice(payload),
            0x03 => {
                self.log_acc.extend_from_slice(payload);
                while let Some(nl) = self.log_acc.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = self.log_acc.drain(..=nl).collect();
                    let mut l = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
                    while l.ends_with('\r') {
                        l.pop();
                    }
                    if !l.is_empty() {
                        self.logs.push(l);
                    }
                }
                // Bound the backlog if the UI hasn't drained for a while.
                if self.logs.len() > 2000 {
                    let drop = self.logs.len() - 2000;
                    self.logs.drain(..drop);
                }
            }
            _ => {}
        }
    }

    /// Hand off and clear the device-log lines captured so far.
    pub fn take_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }

    pub fn write(&mut self, data: &[u8]) -> bool {
        let dev = match self.dev.as_ref() {
            Some(d) => d,
            None => return false,
        };
        let mut off = 0;
        loop {
            let n = std::cmp::min(61, data.len() - off);
            let mut rep = [0u8; 64];
            rep[0] = 0x02;
            rep[1] = n as u8;
            if n > 0 {
                rep[2..2 + n].copy_from_slice(&data[off..off + n]);
            }
            if dev.write(&rep).is_err() {
                return false;
            }
            off += n;
            if off >= data.len() {
                break;
            }
        }
        true
    }

    pub fn write_str(&mut self, s: &str) -> bool {
        self.write(s.as_bytes())
    }

    pub fn drain(&mut self) {
        // Read everything pending, but route it: device-log reports (id 3) are
        // kept (captured into `logs`), stale command-reply bytes are discarded.
        loop {
            let mut buf = [0u8; 64];
            let r = match self.dev.as_ref() {
                Some(d) => d.read_timeout(&mut buf, 0).unwrap_or(0),
                None => break,
            };
            if r == 0 {
                break;
            }
            self.route(&buf, r);
        }
        self.rx.clear();
    }

    pub fn read_line(&mut self, ms: u64) -> String {
        if self.dev.is_none() {
            return String::new();
        }
        let deadline = Instant::now() + Duration::from_millis(ms);
        loop {
            if let Some(nl) = self.rx.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.rx.drain(..=nl).collect();
                let mut l = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
                while l.ends_with('\r') {
                    l.pop();
                }
                return l;
            }
            let mut buf = [0u8; 64];
            // Re-borrow per read so we can route (which needs &mut self) afterwards.
            let r = match self.dev.as_ref() {
                Some(d) => d.read_timeout(&mut buf, 40).unwrap_or(0),
                None => return String::new(),
            };
            if r > 0 {
                self.route(&buf, r);
            }
            if Instant::now() >= deadline && !self.rx.contains(&b'\n') {
                return String::new();
            }
        }
    }
}
