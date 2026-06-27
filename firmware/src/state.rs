//! Device state pushed/queried by the dashboard: the runtime pin map, the
//! config JSON blobs (race layout / buttons / profile / car data), rev-counter
//! brightness, the SimHub car model, and parse stats. Persisted to NVS namespace
//! "dash" as raw JSON/scalars (version-proof: the UI parses the JSON when it
//! renders, Phase 5). Guarded by a single Mutex since both the USB task and the
//! CDC poll loop dispatch commands.

use std::sync::{Mutex, OnceLock};

use esp_idf_svc::nvs::{EspDefaultNvsPartition, EspNvs, NvsDefault};
use serde::{Deserialize, Serialize};

const NS: &str = "dash";

/// Runtime GPIO pin map for displays, touch and the LED strip (mirrors the legacy
/// device_pins_t). Defaults = the Seeed XIAO S3 stock wiring.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct DevicePins {
    pub sclk: i32,
    pub mosi: i32,
    pub miso: i32,
    pub dc: i32,
    pub disp1_cs: i32,
    pub disp2_cs: i32,
    pub touch1_cs: i32,
    pub touch2_cs: i32,
    pub led_din: i32,
    pub race_screen: i32,
    pub led_rev: i32,
    pub led_tc: i32,
    pub led_abs: i32,
    pub led_rgbw: i32,
}

impl Default for DevicePins {
    fn default() -> Self {
        DevicePins {
            sclk: 7, mosi: 9, miso: 8, dc: 2,
            disp1_cs: 1, disp2_cs: 3,
            touch1_cs: 5, touch2_cs: 6,
            led_din: 43,
            race_screen: 0,
            led_rev: 12, led_tc: 2, led_abs: 2, led_rgbw: 1,
        }
    }
}

pub struct AppState {
    nvs: Option<EspNvs<NvsDefault>>,
    pub pins: DevicePins,
    pub race_json: String,    // last @RS push (echoed by @RG)
    pub ui_doc: Option<pith_ui::UiDoc>, // active pith-ui layout (@UI)
    pub ui_json: String,      // last @UI push (echoed by @UG)
    pub ui_ver: u32,          // bumped on change -> dirty-rect cache invalidation
    pub buttons_json: String, // last @BS push
    pub profile_json: String, // last @P push (dashboard rarely sends)
    pub car_json: String,     // last @C / @SL push
    pub button_pages: u8,
    pub brightness: u8,       // 0..100
    pub car_model: String,    // from @CM
    pub frames_ok: u32,
    pub frames_bad: u32,
    pub sim_on: bool,
    pub cfg_reboot: bool,     // pin map changed -> reboot to apply
}

static STATE: OnceLock<Mutex<AppState>> = OnceLock::new();

/// Initialize state: open NVS and restore any persisted config. Call once at boot
/// before USB starts.
pub fn init() {
    let nvs = EspDefaultNvsPartition::take()
        .ok()
        .and_then(|part| EspNvs::new(part, NS, true).ok());

    let mut s = AppState {
        nvs,
        pins: DevicePins::default(),
        race_json: String::new(),
        ui_doc: None,
        ui_json: String::new(),
        ui_ver: 0,
        buttons_json: String::new(),
        profile_json: String::new(),
        car_json: String::new(),
        button_pages: 0,
        brightness: 43, // ~DEFAULT_BRIGHT (110/255)
        car_model: String::new(),
        frames_ok: 0,
        frames_bad: 0,
        sim_on: false,
        cfg_reboot: false,
    };
    s.load();
    let _ = STATE.set(Mutex::new(s));
}

/// Run `f` with exclusive access to the state.
pub fn with<R>(f: impl FnOnce(&mut AppState) -> R) -> R {
    let m = STATE.get().expect("state::init not called");
    let mut g = m.lock().unwrap();
    f(&mut g)
}

impl AppState {
    fn get_str_owned(&self, key: &str) -> Option<String> {
        let nvs = self.nvs.as_ref()?;
        let mut buf = vec![0u8; 8192];
        match nvs.get_str(key, &mut buf) {
            Ok(Some(s)) => Some(s.to_owned()),
            _ => None,
        }
    }

    fn set_str(&mut self, key: &str, val: &str) {
        if let Some(nvs) = self.nvs.as_mut() {
            let _ = nvs.set_str(key, val);
        }
    }

    fn load(&mut self) {
        if let Some(j) = self.get_str_owned("pinsjson") {
            if let Ok(p) = serde_json::from_str::<DevicePins>(&j) {
                self.pins = p;
            }
        }
        self.race_json = self.get_str_owned("racejson").unwrap_or_default();
        self.ui_json = self.get_str_owned("uijson").unwrap_or_default();
        if !self.ui_json.is_empty() {
            self.ui_doc = serde_json::from_str(&self.ui_json).ok();
        }
        self.buttons_json = self.get_str_owned("btnsjson").unwrap_or_default();
        self.profile_json = self.get_str_owned("profjson").unwrap_or_default();
        self.car_json = self.get_str_owned("carjson").unwrap_or_default();
        if let Some(nvs) = self.nvs.as_ref() {
            if let Ok(Some(b)) = nvs.get_u8("bright") {
                self.brightness = b.min(100);
            }
            if let Ok(Some(n)) = nvs.get_u8("btnpages") {
                self.button_pages = n;
            }
        }
    }

    // ---- command appliers (return true on success) ----

    pub fn apply_race(&mut self, json: &str) -> bool {
        if serde_json::from_str::<serde_json::Value>(json).is_err() {
            return false;
        }
        self.race_json = json.to_owned();
        self.set_str("racejson", json);
        true
    }

    /// Apply a pith-ui UiDoc (pushed as JSON via @UI). Parses + stores + persists,
    /// and bumps ui_ver so the display task invalidates its dirty-rect cache.
    pub fn apply_ui(&mut self, json: &str) -> bool {
        match serde_json::from_str::<pith_ui::UiDoc>(json) {
            Ok(doc) => {
                self.ui_doc = Some(doc);
                self.ui_json = json.to_owned();
                self.ui_ver = self.ui_ver.wrapping_add(1);
                self.set_str("uijson", json);
                true
            }
            Err(_) => false,
        }
    }

    pub fn apply_buttons(&mut self, json: &str) -> bool {
        let v: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return false,
        };
        // buttonPages = length of the "pages" array (for the @CAP handshake).
        self.button_pages = v
            .get("pages")
            .and_then(|p| p.as_array())
            .map(|a| a.len() as u8)
            .unwrap_or(0);
        self.buttons_json = json.to_owned();
        self.set_str("btnsjson", json);
        if let Some(nvs) = self.nvs.as_ref() {
            let _ = nvs.set_u8("btnpages", self.button_pages);
        }
        true
    }

    pub fn apply_profile(&mut self, json: &str) -> bool {
        if serde_json::from_str::<serde_json::Value>(json).is_err() {
            return false;
        }
        self.profile_json = json.to_owned();
        self.set_str("profjson", json);
        true
    }

    pub fn apply_car(&mut self, json: &str) -> bool {
        if serde_json::from_str::<serde_json::Value>(json).is_err() {
            return false;
        }
        self.car_json = json.to_owned();
        self.set_str("carjson", json);
        true
    }

    pub fn set_brightness(&mut self, pct: i32) {
        let b = pct.clamp(0, 100) as u8;
        self.brightness = b;
        if let Some(nvs) = self.nvs.as_ref() {
            let _ = nvs.set_u8("bright", b);
        }
    }

    pub fn set_car_model(&mut self, model: &str) {
        self.car_model = model.to_owned();
    }

    /// Apply a partial @PINS push (only present keys override current), persist,
    /// and flag a reboot. Returns true on valid JSON.
    pub fn apply_pins(&mut self, json: &str) -> bool {
        let v: serde_json::Value = match serde_json::from_str(json) {
            Ok(v) => v,
            Err(_) => return false,
        };
        let mut p = self.pins;
        let pin = |cur: i32, key: &str| -> i32 {
            match v.get(key).and_then(|x| x.as_i64()) {
                Some(n) if (0..=48).contains(&n) => n as i32,
                _ => cur,
            }
        };
        p.sclk = pin(p.sclk, "sclk");
        p.mosi = pin(p.mosi, "mosi");
        p.miso = pin(p.miso, "miso");
        p.dc = pin(p.dc, "dc");
        p.disp1_cs = pin(p.disp1_cs, "disp1_cs");
        p.disp2_cs = pin(p.disp2_cs, "disp2_cs");
        p.touch1_cs = pin(p.touch1_cs, "touch1_cs");
        p.touch2_cs = pin(p.touch2_cs, "touch2_cs");
        p.led_din = pin(p.led_din, "led_din");
        if let Some(n) = v.get("race_screen").and_then(|x| x.as_i64()) {
            p.race_screen = if n == 1 { 1 } else { 0 };
        }
        if let Some(n) = v.get("led_rev").and_then(|x| x.as_i64()) {
            if (0..=64).contains(&n) { p.led_rev = n as i32; }
        }
        if let Some(n) = v.get("led_tc").and_then(|x| x.as_i64()) {
            if (0..=16).contains(&n) { p.led_tc = n as i32; }
        }
        if let Some(n) = v.get("led_abs").and_then(|x| x.as_i64()) {
            if (0..=16).contains(&n) { p.led_abs = n as i32; }
        }
        if let Some(n) = v.get("led_rgbw").and_then(|x| x.as_i64()) {
            p.led_rgbw = if n != 0 { 1 } else { 0 };
        }
        self.pins = p;
        if let Ok(j) = serde_json::to_string(&p) {
            self.set_str("pinsjson", &j);
        }
        self.cfg_reboot = true;
        true
    }

    /// The @CAP capability handshake JSON (one line, must contain "name").
    pub fn cap_json(&self, serial: &str) -> String {
        let board = option_env!("PITHDDU_BOARD").unwrap_or("xiao_s3");
        let p = &self.pins;
        format!(
            "{{\"name\":\"Pith DDU\",\"fw\":\"0.9.5\",\"board\":\"{board}\",\"serial\":\"{serial}\",\"buttonPages\":{bp},\
\"screens\":[{{\"role\":\"main\",\"w\":480,\"h\":320,\"touch\":true}},\
{{\"role\":\"side\",\"w\":480,\"h\":320,\"touch\":true}}],\
\"leds\":{{\"rev\":{lr},\"tc\":{lt},\"abs\":{la},\"separate\":true}},\
\"pins\":{{\"sclk\":{sclk},\"mosi\":{mosi},\"miso\":{miso},\"dc\":{dc},\
\"disp1_cs\":{d1},\"disp2_cs\":{d2},\"touch1_cs\":{t1},\"touch2_cs\":{t2},\"led_din\":{din},\
\"race_screen\":{rs},\"led_rev\":{lr},\"led_tc\":{lt},\"led_abs\":{la},\"led_rgbw\":{lw}}}}}\n",
            bp = self.button_pages,
            lr = p.led_rev, lt = p.led_tc, la = p.led_abs, lw = p.led_rgbw,
            sclk = p.sclk, mosi = p.mosi, miso = p.miso, dc = p.dc,
            d1 = p.disp1_cs, d2 = p.disp2_cs, t1 = p.touch1_cs, t2 = p.touch2_cs,
            din = p.led_din, rs = p.race_screen,
        )
    }
}
