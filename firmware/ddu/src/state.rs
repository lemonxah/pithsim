//! Device state pushed/queried by the dashboard: the runtime pin map, the
//! config JSON blobs (race layout / buttons / profile / car data), rev-counter
//! brightness, the SimHub car model, and parse stats. Persisted to NVS namespace
//! "dash" as raw JSON/scalars (version-proof: the UI parses the JSON when it
//! renders, Phase 5). Guarded by a single Mutex since both the USB task and the
//! CDC poll loop dispatch commands.

use std::sync::atomic::AtomicBool;
use std::sync::{Mutex, OnceLock};

use esp_idf_svc::nvs::{EspCustomNvsPartition, EspDefaultNvsPartition, EspNvs, NvsCustom, NvsDefault};
use serde::{Deserialize, Serialize};

const NS: &str = "dash";

/// True while the display task has the device in sleep mode (screens dark).
/// The LED task reads this to blank the shift-light strip for the duration.
pub static SLEEPING: AtomicBool = AtomicBool::new(false);

/// Bump the persisted consecutive-boot-fail counter and return the new value. Call
/// once at boot, before the risky init. [`boot_mark_ok`] resets it after we've run
/// stably; a crash before that leaves it bumped, so the RECOVERY app (which boots
/// first and reads `bootfail` from the shared namespace) can show "previous boot
/// failed Nx" and offer the layout wipe. All safe-mode UX lives in pith-recovery —
/// the old in-app BIOS helpers were removed with it.
pub fn boot_attempt_begin() -> u8 {
    with(|s| {
        let Some(nvs) = s.nvs.as_ref() else { return 1 };
        let n = nvs
            .get_u8("bootfail")
            .ok()
            .flatten()
            .unwrap_or(0)
            .saturating_add(1);
        let _ = nvs.set_u8("bootfail", n);
        n
    })
}

/// Record which OTA slot (0/1) holds the active main firmware, so the recovery app
/// chain-loads it on the next boot. Written by the OTA completion path: instead of
/// pointing otadata at the new image ourselves, we hand the choice to recovery
/// (which owns boot-slot selection in the recovery-first model).
pub fn set_main_slot(idx: u8) {
    with(|s| {
        if let Some(nvs) = s.nvs.as_ref() {
            let _ = nvs.set_u8("mainslot", idx & 1);
        }
    });
}

/// Mark this boot as good (clears the fail counter). Skips the NVS write when it's
/// already zero so a healthy boot doesn't wear flash.
pub fn boot_mark_ok() {
    with(|s| {
        if let Some(nvs) = s.nvs.as_ref() {
            if nvs.get_u8("bootfail").ok().flatten().unwrap_or(0) != 0 {
                let _ = nvs.set_u8("bootfail", 0);
            }
        }
    })
}

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
    /// Panel backlight enable (active-high). Default D3 / GPIO4. `serde(default)` so
    /// configs saved before this field existed still deserialize (keep their pins).
    #[serde(default = "bl_default")]
    pub backlight: i32,
    pub led_din: i32,
    pub race_screen: i32,
    pub led_rev: i32,
    pub led_tc: i32,
    pub led_abs: i32,
    pub led_rgbw: i32,
}

fn bl_default() -> i32 {
    4
}

impl Default for DevicePins {
    fn default() -> Self {
        DevicePins {
            sclk: 7, mosi: 9, miso: 8, dc: 2,
            disp1_cs: 1, disp2_cs: 3,
            touch1_cs: 5, touch2_cs: 6,
            backlight: 4,
            led_din: 43,
            race_screen: 0,
            led_rev: 12, led_tc: 2, led_abs: 2, led_rgbw: 1,
        }
    }
}

/// Keys routed to the big `nvsblob` partition (384 KB in the flash tail): the
/// JSON blobs that together can overflow the 24 KB default `nvs`. Reads fall
/// back to the default partition so data saved before the partition existed
/// (or on devices not yet re-flashed with the new table) still loads.
const BLOB_KEYS: &[&str] = &["uijson", "edjson", "racejson", "carjson", "profjson", "buttonsjson"];

pub struct AppState {
    nvs: Option<EspNvs<NvsDefault>>,
    /// Big-blob partition (`nvsblob`) — `None` on devices flashed with the old
    /// partition table; everything then behaves exactly as before.
    nvs_blob: Option<EspNvs<NvsCustom>>,
    pub pins: DevicePins,
    pub race_json: String,    // last @RS push (echoed by @RG)
    pub ui_doc: Option<pith_ui::UiDoc>, // active pith-ui layout (@UI)
    pub ui_json: String,      // last @UI push (echoed by @UG)
    pub ui_ver: u32,          // bumped on change -> dirty-rect cache invalidation
    pub pending_ui: Option<String>, // raw @UI JSON awaiting parse on the display task
                                    // (parsing on the 4 KB USB task overflows its stack)
    pub editor_json: String,  // opaque editor layout blob (@EL), echoed by @EG so the
                              // GUI can read its OWN full freeform layout back losslessly
    pub buttons_json: String, // last @BS push
    pub profile_json: String, // last @P push (dashboard rarely sends)
    pub car_json: String,     // last @C / @SL push
    pub button_pages: u8,
    pub brightness: u8,       // 0..100
    /// Auto-sleep timeout in seconds (0 = never): no telemetry and no touch for
    /// this long turns the screens + LEDs off until woken. Set on the config screen.
    pub sleep_timeout_s: u16,
    pub car_model: String,    // from @CM
    pub relatives: pith_ui::Relatives, // from @REL (multi-car standings/relatives)
    pub frames_ok: u32,
    pub frames_bad: u32,
    pub sim_on: bool,
    pub cfg_reboot: bool,     // pin map changed -> reboot to apply
    // Display orientation (runtime-configurable so a differently-mounted panel
    // doesn't need a recompile). Applied at init + live on @DO.
    pub disp_rot: u8,         // 0..3 = 0/90/180/270 degrees
    pub disp_flip_h: bool,    // mirror horizontally
    pub disp_flip_v: bool,    // mirror vertically
    pub disp_bgr: bool,       // panel colour order is BGR (vs RGB)
    pub disp_inv: bool,       // invert colours (negative)
    pub disp_ver: u32,        // bumped on change -> display task re-applies (orientation)
}

static STATE: OnceLock<Mutex<AppState>> = OnceLock::new();

/// Initialize state: open NVS and restore any persisted config. Call once at boot
/// before USB starts.
pub fn init() {
    let nvs = EspDefaultNvsPartition::take()
        .ok()
        .and_then(|part| EspNvs::new(part, NS, true).ok());
    let nvs_blob = EspCustomNvsPartition::take("nvsblob")
        .ok()
        .and_then(|part| EspNvs::new(part, NS, true).ok());
    if nvs_blob.is_none() {
        log::info!("nvsblob partition absent (old table) — large blobs use default nvs");
    }

    let mut s = AppState {
        nvs,
        nvs_blob,
        pins: DevicePins::default(),
        race_json: String::new(),
        ui_doc: None,
        ui_json: String::new(),
        ui_ver: 0,
        pending_ui: None,
        editor_json: String::new(),
        buttons_json: String::new(),
        profile_json: String::new(),
        car_json: String::new(),
        button_pages: 0,
        brightness: 43, // ~DEFAULT_BRIGHT (110/255)
        sleep_timeout_s: 30,
        car_model: String::new(),
        relatives: pith_ui::Relatives::default(),
        frames_ok: 0,
        frames_bad: 0,
        sim_on: false,
        cfg_reboot: false,
        // Default matches the XIAO DDU reference panels (270° + horizontal flip,
        // BGR colour order).
        disp_rot: 3,
        disp_flip_h: true,
        disp_flip_v: false,
        disp_bgr: true,
        disp_inv: false,
        disp_ver: 0,
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
        // Stored as a blob (not str): NVS strings are capped at ~4000 bytes, which a
        // real freeform layout / editor blob blows past. Blobs span pages.
        let mut buf = vec![0u8; 32768];
        // Blob-routed keys prefer the big partition; fall back to the default
        // one so pre-partition data (or an old-table device) still loads.
        if BLOB_KEYS.contains(&key) {
            if let Some(blob) = self.nvs_blob.as_ref() {
                if let Ok(Some(data)) = blob.get_raw(key, &mut buf) {
                    return String::from_utf8(data.to_vec()).ok();
                }
            }
        }
        let nvs = self.nvs.as_ref()?;
        match nvs.get_raw(key, &mut buf) {
            Ok(Some(data)) => String::from_utf8(data.to_vec()).ok(),
            _ => None,
        }
    }

    fn set_str(&mut self, key: &str, val: &str) {
        // Blob, not str (see get_str_owned). Log failures — a full NVS partition
        // would otherwise silently drop the layout and boot to factory defaults.
        if BLOB_KEYS.contains(&key) {
            if let Some(blob) = self.nvs_blob.as_mut() {
                match blob.set_raw(key, val.as_bytes()) {
                    Ok(_) => {
                        // Migrated: retire the copy in the small default nvs so
                        // it stops eating the 24 KB settings partition.
                        if let Some(nvs) = self.nvs.as_mut() {
                            let _ = nvs.remove(key);
                        }
                    }
                    Err(e) => log::warn!("nvsblob set '{key}' ({} bytes) FAILED: {e}", val.len()),
                }
                return;
            }
        }
        if let Some(nvs) = self.nvs.as_mut() {
            if let Err(e) = nvs.set_raw(key, val.as_bytes()) {
                log::warn!("nvs set '{key}' ({} bytes) FAILED: {e}", val.len());
            }
        }
    }

    fn load(&mut self) {
        if let Some(j) = self.get_str_owned("pinsjson") {
            if let Ok(p) = serde_json::from_str::<DevicePins>(&j) {
                self.pins = p;
            }
        }
        self.race_json = self.get_str_owned("racejson").unwrap_or_default();
        self.editor_json = self.get_str_owned("edjson").unwrap_or_default();
        self.ui_json = self.get_str_owned("uijson").unwrap_or_default();
        if !self.ui_json.is_empty() {
            // Defer the typed parse to the display task (bigger stack) — same reason
            // as @UI: parsing here on the main task can overflow its stack.
            self.pending_ui = Some(self.ui_json.clone());
        }
        self.buttons_json = self.get_str_owned("btnsjson").unwrap_or_default();
        self.profile_json = self.get_str_owned("profjson").unwrap_or_default();
        self.car_json = self.get_str_owned("carjson").unwrap_or_default();
        if let Some(nvs) = self.nvs.as_ref() {
            if let Ok(Some(b)) = nvs.get_u8("bright") {
                self.brightness = b.clamp(5, 100); // floor at 5% (see set_brightness)
            }
            if let Ok(Some(n)) = nvs.get_u8("btnpages") {
                self.button_pages = n;
            }
            if let Ok(Some(t)) = nvs.get_u16("sleepto") {
                self.sleep_timeout_s = t;
            }
            if let Ok(Some(r)) = nvs.get_u8("disprot") {
                self.disp_rot = r & 3;
            }
            if let Ok(Some(f)) = nvs.get_u8("dispflip") {
                self.disp_flip_h = f & 1 != 0;
                self.disp_flip_v = f & 2 != 0;
            }
            // Separate key so the colour defaults (BGR) survive a device that only
            // ever persisted the orientation bits.
            if let Ok(Some(c)) = nvs.get_u8("dispcol") {
                self.disp_bgr = c & 1 != 0;
                self.disp_inv = c & 2 != 0;
            }
        }
    }

    /// Apply a display config change. Rotation + flips re-apply live (disp_ver);
    /// colour order / inversion can only be set at panel init, so a change there
    /// flags a reboot. Persists everything.
    pub fn apply_disp(&mut self, rot: u8, flip_h: bool, flip_v: bool, bgr: bool, inv: bool) {
        let colour_changed = bgr != self.disp_bgr || inv != self.disp_inv;
        self.disp_rot = rot & 3;
        self.disp_flip_h = flip_h;
        self.disp_flip_v = flip_v;
        self.disp_bgr = bgr;
        self.disp_inv = inv;
        self.disp_ver = self.disp_ver.wrapping_add(1);
        if let Some(nvs) = self.nvs.as_mut() {
            let _ = nvs.set_u8("disprot", self.disp_rot);
            let _ = nvs.set_u8("dispflip", (flip_h as u8) | ((flip_v as u8) << 1));
            let _ = nvs.set_u8("dispcol", (bgr as u8) | ((inv as u8) << 1));
        }
        if colour_changed {
            self.cfg_reboot = true;
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
    /// Accept a pushed UiDoc (@UI). The JSON is only stored + persisted here — the
    /// actual typed parse is deferred to the display task via [`apply_pending_ui`],
    /// because deserializing on the 4 KB USB task overflows its stack and reboots.
    pub fn queue_ui(&mut self, json: &str) -> bool {
        self.ui_json = json.to_owned();
        self.pending_ui = Some(json.to_owned());
        self.set_str("uijson", json);
        true
    }

    /// Store the GUI's opaque editor-layout blob (@EL). The device never parses it;
    /// it just persists + echoes it (@EG) so the editor round-trips losslessly.
    pub fn apply_editor(&mut self, json: &str) -> bool {
        self.editor_json = json.to_owned();
        self.set_str("edjson", json);
        true
    }

    /// Parse a pending UiDoc (called from the display task's larger stack). Returns
    /// true if a doc was applied (so the caller can invalidate its render caches).
    pub fn apply_pending_ui(&mut self) -> bool {
        let Some(json) = self.pending_ui.take() else {
            return false;
        };
        match serde_json::from_str::<pith_ui::UiDoc>(&json) {
            Ok(doc) => {
                let screens = doc.screens.len();
                let nodes: usize = doc.screens.iter().map(|s| s.nodes.len()).sum();
                self.ui_doc = Some(doc);
                self.ui_ver = self.ui_ver.wrapping_add(1);
                log::info!("apply_ui ok: {} bytes, {} screens, {} nodes (ver {})", json.len(), screens, nodes, self.ui_ver);
                true
            }
            Err(e) => {
                log::warn!("apply_ui PARSE FAIL: {} bytes: {e}", json.len());
                false
            }
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
        if self.car_json == json {
            return true; // unchanged — skip the NVS write (the dashboard
                         // re-pushes @C on every reconnect; no flash wear)
        }
        self.car_json = json.to_owned();
        self.set_str("carjson", json);
        true
    }

    pub fn set_brightness(&mut self, pct: i32) {
        // Floor at 5%: at 0 the rev LEDs scale to fully black (c*0/100) and look
        // "broken" even though everything works — never let it bottom out there.
        let b = pct.clamp(5, 100) as u8;
        // Live brightness updates (GUI slider, on-screen slider) can arrive rapidly;
        // only touch NVS when the value actually changes so dragging doesn't wear flash.
        if b == self.brightness {
            return;
        }
        self.brightness = b;
        if let Some(nvs) = self.nvs.as_ref() {
            let _ = nvs.set_u8("bright", b);
        }
    }

    pub fn set_sleep_timeout(&mut self, secs: u16) {
        if secs == self.sleep_timeout_s {
            return;
        }
        self.sleep_timeout_s = secs;
        if let Some(nvs) = self.nvs.as_ref() {
            let _ = nvs.set_u16("sleepto", secs);
        }
    }

    pub fn set_car_model(&mut self, model: &str) {
        self.car_model = model.to_owned();
    }

    pub fn set_relatives(&mut self, line: &str) {
        if let Some(r) = pith_ui::Relatives::from_wire(line) {
            self.relatives = r;
        }
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
        p.backlight = pin(p.backlight, "backlight");
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
            "{{\"name\":\"Pith DDU\",\"fw\":\"{fw}\",\"board\":\"{board}\",\"serial\":\"{serial}\",\"buttonPages\":{bp},\
\"screens\":[{{\"role\":\"main\",\"w\":480,\"h\":320,\"touch\":true}},\
{{\"role\":\"side\",\"w\":480,\"h\":320,\"touch\":true}}],\
\"leds\":{{\"rev\":{lr},\"tc\":{lt},\"abs\":{la},\"separate\":true}},\
\"pins\":{{\"sclk\":{sclk},\"mosi\":{mosi},\"miso\":{miso},\"dc\":{dc},\
\"disp1_cs\":{d1},\"disp2_cs\":{d2},\"touch1_cs\":{t1},\"touch2_cs\":{t2},\"backlight\":{bl},\"led_din\":{din},\
\"race_screen\":{rs},\"led_rev\":{lr},\"led_tc\":{lt},\"led_abs\":{la},\"led_rgbw\":{lw}}},\
\"disp\":{{\"rot\":{rot},\"fh\":{fh},\"fv\":{fv},\"bgr\":{bgr},\"inv\":{inv}}}}}\n",
            fw = env!("CARGO_PKG_VERSION"),
            bp = self.button_pages,
            lr = p.led_rev, lt = p.led_tc, la = p.led_abs, lw = p.led_rgbw,
            sclk = p.sclk, mosi = p.mosi, miso = p.miso, dc = p.dc,
            d1 = p.disp1_cs, d2 = p.disp2_cs, t1 = p.touch1_cs, t2 = p.touch2_cs,
            bl = p.backlight, din = p.led_din, rs = p.race_screen,
            rot = self.disp_rot, fh = self.disp_flip_h, fv = self.disp_flip_v,
            bgr = self.disp_bgr, inv = self.disp_inv,
        )
    }
}
