use super::hid::Hid;
use super::serial::Serial;

#[derive(Default)]
pub struct Dash {
    pub ser: Serial,
    pub hid: Hid,
    pub use_hid: bool,
    pub log: String,
}

impl Dash {
    fn logln(&mut self, s: &str) {
        self.log.push_str(s);
        self.log.push('\n');
    }

    pub fn connected(&self) -> bool {
        if self.use_hid {
            self.hid.is_open()
        } else {
            self.ser.is_open()
        }
    }

    fn tx_str(&mut self, s: &str) {
        if self.use_hid {
            self.hid.write_str(s);
        } else {
            self.ser.write_str(s);
        }
    }
    fn tx_raw(&mut self, d: &[u8]) -> bool {
        if self.use_hid {
            self.hid.write(d)
        } else {
            self.ser.write(d)
        }
    }
    fn rx_line(&mut self, ms: u64) -> String {
        if self.use_hid {
            self.hid.read_line(ms)
        } else {
            self.ser.read_line(ms)
        }
    }
    fn drain_t(&mut self) {
        if self.use_hid {
            self.hid.drain();
        } else {
            self.ser.drain();
        }
    }

    pub fn command(&mut self, line: &str) -> (bool, String) {
        self.drain_t();
        self.tx_str(&format!("{line}\n"));
        let r = self.rx_line(2000);
        let ok = r.contains("OK") || r.contains("READY");
        (ok, r)
    }

    pub fn status(&mut self) -> String {
        self.drain_t();
        self.tx_str("?\n");
        self.rx_line(2000)
    }
    pub fn telemetry(&mut self) -> String {
        self.drain_t();
        self.tx_str("@T\n");
        self.rx_line(2000)
    }
    pub fn capabilities(&mut self) -> String {
        self.drain_t();
        self.tx_str("@CAP\n");
        self.rx_line(2000)
    }

    #[allow(dead_code)]
    pub fn push_profile(&mut self, json: &str) -> bool {
        let (ok, r) = self.command(&format!("@P{json}"));
        self.logln(&format!("profile: {r}"));
        ok
    }
    pub fn push_car(&mut self, json: &str) -> bool {
        let (ok, r) = self.command(&format!("@C{json}"));
        self.logln(&format!("car: {r}"));
        ok
    }
    pub fn push_race(&mut self, json: &str) -> bool {
        let (ok, r) = self.command(&format!("@RS{json}"));
        self.logln(&format!("race: {r}"));
        ok
    }
    /// Push a pith-ui UiDoc (JSON) for the device to render with dirty-rect.
    pub fn push_ui(&mut self, json: &str) -> bool {
        let (ok, r) = self.command(&format!("@UI{json}"));
        self.logln(&format!("ui: {r}"));
        ok
    }
    pub fn push_buttons(&mut self, json: &str) -> bool {
        let (ok, r) = self.command(&format!("@BS{json}"));
        self.logln(&format!("buttons: {r}"));
        ok
    }
    pub fn push_shift(&mut self, json: &str) -> bool {
        let (ok, r) = self.command(&format!("@SL{json}"));
        self.logln(&format!("shift: {r}"));
        ok
    }
    pub fn set_brightness(&mut self, pct: i32) {
        self.command(&format!("@B{pct}"));
    }
    /// Push the display orientation (applied live on the device, no reboot).
    pub fn push_disp(&mut self, rot: i32, flip_h: bool, flip_v: bool) -> bool {
        let (ok, r) = self.command(&format!(
            "@DO{{\"rot\":{rot},\"fh\":{flip_h},\"fv\":{flip_v}}}"
        ));
        self.logln(&format!("disp: {r}"));
        ok
    }

    pub fn ota_upload(&mut self, img: &[u8], mut on_progress: impl FnMut(i32)) -> bool {
        self.drain_t();
        self.tx_str(&format!("@OTA{}\n", img.len()));
        if !self.rx_line(3000).contains("OTAREADY") {
            self.logln("OTA: no OTAREADY (port busy?)");
            return false;
        }
        const ACK: usize = 2048;
        let mut sent = 0usize;
        while sent < img.len() {
            let end = std::cmp::min(sent + ACK, img.len());
            if !self.tx_raw(&img[sent..end]) {
                self.logln("OTA: write error");
                return false;
            }
            sent = end;
            if sent < img.len() {
                let k = self.rx_line(6000);
                if !k.contains('K') || k.contains("ERR") {
                    self.logln(&format!("OTA: failed at {sent}"));
                    return false;
                }
            }
            on_progress((sent * 100 / img.len()) as i32);
        }
        let done = self.rx_line(8000);
        self.logln(&format!(
            "OTA: {}",
            if done.is_empty() {
                "no reply (device rebooting)"
            } else {
                &done
            }
        ));
        if done.contains("OTAERR") {
            return false;
        }
        true
    }
}
