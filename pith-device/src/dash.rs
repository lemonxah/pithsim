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

    /// Device-side log lines streamed over HID report id 3 (empty on serial).
    pub fn take_device_logs(&mut self) -> Vec<String> {
        if self.use_hid {
            self.hid.take_logs()
        } else {
            Vec::new()
        }
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
    /// Fire-and-forget: push one SimHub `$`-frame to the device. The device's
    /// dispatch parses any non-`@` line into TELEM, so this drives the screen the
    /// same way the old Custom Serial feed did — but over the HID channel the
    /// dashboard already owns. No reply is sent, so we don't drain/read (keeps the
    /// ~60 Hz stream cheap and off the command-reply path).
    pub fn push_telemetry(&mut self, frame: &str) {
        if frame.is_empty() {
            return;
        }
        self.tx_str(&format!("{frame}\n"));
    }
    /// Push a multi-car relatives/standings line (`@REL…`). Fire-and-forget like
    /// [`Self::push_telemetry`] — the device updates its relatives state, no reply.
    pub fn push_relatives(&mut self, line: &str) {
        if line.is_empty() {
            return;
        }
        self.tx_str(&format!("{line}\n"));
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
    /// Store the GUI's full editor-layout blob on the device (@EL) for lossless
    /// round-trip — the device echoes it back via @EG, it doesn't render it.
    pub fn push_editor(&mut self, json: &str) -> bool {
        let (ok, r) = self.command(&format!("@EL{json}"));
        self.logln(&format!("editor: {r}"));
        ok
    }
    /// Read the GUI's editor-layout blob back from the device (@EG). Returns the
    /// raw reply (caller extracts the JSON body).
    pub fn read_editor(&mut self) -> String {
        self.command("@EG").1
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
    /// Push the display config. Orientation applies live; a colour-order/invert
    /// change reboots the device (those are set at panel init).
    pub fn push_disp(
        &mut self,
        rot: i32,
        flip_h: bool,
        flip_v: bool,
        bgr: bool,
        inv: bool,
    ) -> bool {
        let (ok, r) = self.command(&format!(
            "@DO{{\"rot\":{rot},\"fh\":{flip_h},\"fv\":{flip_v},\"bgr\":{bgr},\"inv\":{inv}}}"
        ));
        self.logln(&format!("disp: {r}"));
        ok
    }

    pub fn ota_upload(&mut self, img: &[u8], mut on_progress: impl FnMut(i32)) -> bool {
        self.drain_t();
        self.tx_str(&format!("@OTA{}\n", img.len()));
        // Wait for OTAREADY, skipping any stale/interleaved lines (a late `?`-status
        // or telemetry reply on the shared channel can land here and false-fail the
        // handshake). esp_ota_begin erases the slot first, so allow a generous window.
        if !self.ota_wait("OTAREADY", 12000) {
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
            if sent < img.len() && !self.ota_wait("K", 6000) {
                self.logln(&format!("OTA: no ack at {sent}"));
                return false;
            }
            on_progress((sent * 100 / img.len()) as i32);
        }
        // Device sends OTADONE then reboots — a stale line or a missing reply
        // (already rebooting) is fine; only an explicit OTAERR is a failure.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(8000);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                self.logln("OTA: done (device rebooting)");
                return true;
            }
            let line = self.rx_line((remaining.as_millis() as u64).clamp(1, 2000));
            let t = line.trim();
            if t.contains("OTADONE") {
                self.logln("OTA: OTADONE");
                return true;
            }
            if t.contains("OTAERR") {
                self.logln(&format!("OTA: device error: {t}"));
                return false;
            }
        }
    }

    /// Wait for an OTA reply containing `needle`, skipping unrelated status /
    /// telemetry / log lines that share the channel (e.g. a second HID client's
    /// `?` reply). Returns false on an explicit OTAERR or on timeout.
    fn ota_wait(&mut self, needle: &str, ms: u64) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(ms);
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                self.logln(&format!("OTA: timed out waiting for {needle}"));
                return false;
            }
            let line = self.rx_line((remaining.as_millis() as u64).clamp(1, 2000));
            let t = line.trim();
            if t.is_empty() {
                continue;
            }
            if t.contains("OTAERR") {
                self.logln(&format!("OTA: device error: {t}"));
                return false;
            }
            // The reply tokens (K / OTAREADY / OTADONE) arrive on their own line.
            if t == needle || (needle.len() > 2 && t.contains(needle)) {
                return true;
            }
            // else: stale/interleaved line — skip and keep waiting.
        }
    }
}
