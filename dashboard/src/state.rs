use std::collections::BTreeMap;

use crate::telemetry::FIELD_COUNT;

#[derive(Clone, Copy, Default)]
pub struct LedDef {
    pub rgb: u32,
    pub threshold: i32,
}

#[derive(Clone, Default)]
pub struct ColorRule {
    pub op: String,
    pub v: i32,
    pub color: String,
}

#[derive(Clone)]
pub struct ModSpec {
    pub id: String,
    pub templ: String,
    pub kind: String,
    pub field: String,
    pub label: String,
    pub fmt_type: String,
    pub unit: String,
    pub base: String,    // for a button: OFF-state colour
    pub on_base: String, // for a toggle button: ON-state colour
    pub scale: i32,
    pub size_pct: i32,
    pub rules: Vec<ColorRule>,
    pub enabled: bool,
    // Freeform placement (device pixels) + which display (0/1) the node lives on.
    // Zone-authored modules leave these at 0; the freeform editor sets them.
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub display: u8,
    // Composable widget tree: when `els` is non-empty the node is a custom widget
    // (root Row/Col of these elements); empty -> the built-in for `kind`/`templ`.
    pub dir: i32, // root layout: 0 = column, 1 = row
    pub gap: i32,
    pub els: Vec<ElemSpec>,
    // Button nodes (kind == "button"): which HID joystick button it drives (1..=32,
    // 0 = none) and whether it latches (toggle) or is momentary (push). The HID
    // index is explicit so it never shifts when buttons are reordered/added.
    pub toggle: bool,
    pub hid: i32,
    // Tab page this node belongs to when its display is tabbed (0 otherwise).
    pub page: i32,
}

/// One element inside a composed widget (a leaf in the pith-ui element tree).
#[derive(Clone)]
pub struct ElemSpec {
    pub kind: String, // label/value/bar/gear/gearSpeed/rpmStrip/tyreGrid/tcDual/sectors/lapPair/position/flag/map/button
    pub flex: i32,    // share of the main axis (>= 1)
    pub field: String,
    pub text: String, // label / button / position caption text
    pub fmt_type: String,
    pub unit: String,
    pub scale: i32,
    pub base: String,
    pub size: i32,
    pub align: i32,     // 0 left, 1 center, 2 right
    pub valign: i32,    // 0 top, 1 center, 2 bottom
    pub action: String, // legacy button semantic name (kept for back-compat)
    pub rules: Vec<ColorRule>,
    // Button elements: HID joystick button (1..=32, 0 = none) + latch/momentary.
    pub toggle: bool,
    pub hid: i32,
}

impl Default for ElemSpec {
    fn default() -> Self {
        ElemSpec {
            kind: "label".into(),
            flex: 1,
            field: String::new(),
            text: String::new(),
            fmt_type: String::new(),
            unit: String::new(),
            scale: 0,
            base: "white".into(),
            size: 0,
            align: 1,
            valign: 1,
            action: String::new(),
            rules: Vec::new(),
            toggle: false,
            hid: 0,
        }
    }
}

impl Default for ModSpec {
    fn default() -> Self {
        ModSpec {
            id: String::new(),
            templ: String::new(),
            kind: String::new(),
            field: String::new(),
            label: String::new(),
            fmt_type: String::new(),
            unit: String::new(),
            base: "white".to_string(),
            on_base: "green".to_string(),
            scale: 0,
            size_pct: 0,
            rules: Vec::new(),
            enabled: true,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            display: 0,
            dir: 0,
            gap: 4,
            els: Vec::new(),
            toggle: false,
            hid: 0,
            page: 0,
        }
    }
}

#[derive(Clone, Default)]
pub struct Zone {
    pub key: String,
    pub title: String,
    pub modules: Vec<ModSpec>,
}

#[derive(Clone, Default)]
pub struct Preset {
    pub name: String,
    pub builtin: bool,
    pub zones: Vec<Zone>,
    pub nodes: Vec<ModSpec>, // freeform layout snapshot (per-display via ModSpec.display)
}

#[derive(Clone)]
pub struct BtnData {
    pub label: String,
    pub toggle: bool,
    pub on: bool,
    pub action: String,
    pub col: u32,
    pub sync: bool,
    pub field: String,
    pub avail: bool,
}

#[derive(Clone, Default)]
pub struct CarItem {
    pub sim: String,
    pub name: String,
    pub id: String,
    pub path: String,
    pub klass: String,
    pub redline: i32,
    pub led_n: i32,
    pub led_cols: Vec<u32>,
}

#[derive(Clone, Default)]
pub struct FwRelease {
    pub tag: String,
    /// DDU app images by board id (`pithddu-<board>.bin` assets).
    pub board_bin: BTreeMap<String, String>,
    /// Handbrake app images by board id (`pith-hb-<board>.bin` assets) — the
    /// same firmware-v* release carries every device's firmware.
    pub hb_bin: BTreeMap<String, String>,
}

#[derive(Clone)]
pub struct BoardPin {
    pub label: String,
    pub gpio: i32,
}

#[derive(Clone)]
pub struct BoardDef {
    pub name: String,
    pub id: String,
    pub target: String, // esp chip family (esp32s3 / esp32s2) — picks a compatible image
    pub pins: Vec<BoardPin>,
}

pub struct State {
    pub redline_rpm: i32,
    pub first_led_pct: i32,
    pub brightness: i32,
    pub animation: i32,
    pub rpm_source: i32,
    pub sel_gear: i32,
    pub blink_enabled: bool,
    pub shift_custom: bool,
    pub blink_hz: f32,
    pub car_name: String,
    pub car_game: String,
    pub car_id: String,
    pub leds: [[LedDef; 12]; 7],

    pub zones: Vec<Zone>,
    pub nodes: Vec<ModSpec>, // freeform race-screen layout (the pith-ui authoring model)
    pub edit_display: u8,    // which display the freeform editor is showing (0/1)
    pub tabs: [Vec<String>; 2], // per-display tab page names (empty = display not tabbed)
    pub edit_tab: i32,       // tab page currently shown in the editor
    pub map_track: String, // track whose bundled outline the Map widget shows (manual or auto-detected)
    pub last_sim_frame: Option<std::time::Instant>, // when the plugin last fed us (gates the @T round-trip)
    // Latest frame per telemetry source (label, parsed, when, which computed fields
    // it supplies). Multiple live sources are MERGED (augment, not replace) so e.g.
    // UDP + shim don't fight over ignition/pit-limiter, and we only compute a field
    // when NO source supplies it. Entries expire after ~2 s of silence.
    pub src_frames: Vec<(
        String,
        pith_core::simhub::Telemetry,
        std::time::Instant,
        crate::telemetry::derive::Provided,
    )>,
    pub custom_swatches: Vec<String>, // saved colour-picker swatches ("#rrggbb")
    pub sel_elem: i32, // selected element index within the selected widget (-1 = none)
    pub drag_origin: Option<(String, i32, i32, i32, i32)>, // (id,x,y,w,h) at gesture start
    pub presets: Vec<Preset>,
    pub active_preset: i32,
    pub race_dirty: bool,
    pub uid: i32,

    pub btn_pages: Vec<Vec<BtnData>>,

    pub all_cars: Vec<CarItem>,
    pub filtered: Vec<usize>,
    pub game: i32,
    pub klass: i32,
    pub sel_car: i32,
    pub query: String,
    pub class_list: Vec<String>,
    pub last_auto_model: String,
    pub detected_model: String,
    pub detected_game_idx: i32,
    pub sims: Vec<(String, String)>,

    pub telem: [i32; FIELD_COUNT],
    pub gear_ch: char,

    pub pin_gpio: Vec<i32>,
    pub race_screen: i32,
    pub led_rev: i32,
    pub led_tc: i32,
    pub led_abs: i32,
    pub led_rgbw: i32,
    pub disp_rot: i32, // 0..3 = 0/90/180/270°
    pub disp_flip_h: bool,
    pub disp_flip_v: bool,
    pub disp_bgr: bool, // panel colour order BGR (vs RGB)
    pub disp_inv: bool, // invert colours
    pub boards: Vec<BoardDef>,
    pub board: i32,

    pub device_fw: String,
    pub hb_fw: String, // handbrake firmware version (from its @CAP), for update checks
    pub serial_ports: Vec<crate::device::PortInfo>,
    pub releases: Vec<FwRelease>,    // DDU stream (firmware-v* tags)
    pub hb_releases: Vec<FwRelease>, // handbrake stream (handbrake-v* tags)

    pub device_log: Vec<String>, // firmware logs streamed over HID report id 3

    pub udp_port: u16, // UDP telemetry server port (SimHub plugin + direct game decoders)

    // Active connectors (we reach out to the game rather than just listening).
    pub acc_enabled: bool,
    pub acc_host: String,
    pub acc_port: u16,
    pub acc_password: String,
    pub ac_enabled: bool,
    pub ac_host: String,
    pub ac_port: u16,
    pub gt7_enabled: bool,
    pub gt7_host: String,  // PlayStation IP (GT7 streams from the console)
    pub shm_enabled: bool, // read AC/ACC shared memory from /dev/shm (needs a bridge)

    // Dashboard-side derived core fields (best/current lap, fuel/lap, delta) for
    // sources that don't transmit them.
    pub derived: crate::telemetry::derive::Derived,

    // Latest multi-car relatives/standings (from a sim's @REL line) — forwarded to
    // the device and mirrored in the preview.
    pub relatives: pith_ui::Relatives,
}

impl Default for State {
    fn default() -> Self {
        State {
            redline_rpm: 8200,
            first_led_pct: 62,
            brightness: 80,
            animation: 0,
            rpm_source: 0,
            sel_gear: 1,
            blink_enabled: true,
            shift_custom: false,
            blink_hz: 5.0,
            car_name: "—".to_string(),
            car_game: "iRacing".to_string(),
            car_id: String::new(),
            leds: [[LedDef::default(); 12]; 7],
            zones: Vec::new(),
            nodes: Vec::new(),
            edit_display: 0,
            tabs: [Vec::new(), Vec::new()],
            edit_tab: 0,
            map_track: "(none)".to_string(),
            last_sim_frame: None,
            src_frames: Vec::new(),
            custom_swatches: Vec::new(),
            sel_elem: -1,
            drag_origin: None,
            presets: Vec::new(),
            active_preset: 0,
            race_dirty: false,
            uid: 0,
            btn_pages: Vec::new(),
            all_cars: Vec::new(),
            filtered: Vec::new(),
            game: 0,
            klass: 0,
            sel_car: -1,
            query: String::new(),
            class_list: vec!["All classes".to_string()],
            last_auto_model: String::new(),
            detected_model: String::new(),
            detected_game_idx: -1,
            sims: vec![
                ("iRacing", "iracing"),
                ("ACC", "assettocorsacompetizione"),
                ("Le Mans Ultimate", "lmu"),
                ("Automobilista 2", "automobilista2"),
                ("Assetto Corsa", "assettocorsa"),
                ("AC EVO", "assettocorsaevo"),
                ("F1 24", "f12024"),
                ("F1 25", "f12025"),
                ("RaceRoom", "rrre"),
                ("Project Motor Racing", "projectmotorracing"),
            ]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
            telem: [0; FIELD_COUNT],
            gear_ch: 'N',
            pin_gpio: vec![7, 9, 8, 2, 1, 3, 5, 6, 43],
            race_screen: 0,
            disp_rot: 3,
            disp_flip_h: true,
            disp_flip_v: false,
            disp_bgr: true,
            disp_inv: false,
            led_rev: 12,
            led_tc: 2,
            led_abs: 2,
            led_rgbw: 1,
            boards: Vec::new(),
            board: 0,
            device_fw: String::new(),
            hb_fw: String::new(),
            serial_ports: Vec::new(),
            releases: Vec::new(),
            hb_releases: Vec::new(),
            device_log: Vec::new(),
            udp_port: 28909,
            acc_enabled: false,
            acc_host: "127.0.0.1".to_string(),
            acc_port: 9000,
            acc_password: "asd".to_string(),
            ac_enabled: false,
            ac_host: "127.0.0.1".to_string(),
            ac_port: 9996,
            gt7_enabled: false,
            gt7_host: String::new(),
            shm_enabled: true,
            derived: Default::default(),
            relatives: Default::default(),
        }
    }
}

impl State {
    pub fn sim_of(&self, game_idx: i32) -> String {
        self.sims
            .get(game_idx as usize)
            .map(|s| s.1.clone())
            .unwrap_or_default()
    }
    pub fn cur_board(&self) -> &BoardDef {
        let b = if self.board >= 0 && (self.board as usize) < self.boards.len() {
            self.board as usize
        } else {
            0
        };
        &self.boards[b]
    }
    pub fn board_pins(&self) -> &[BoardPin] {
        &self.cur_board().pins
    }
    pub fn board_idx_of_gpio(&self, gpio: i32) -> i32 {
        self.board_pins()
            .iter()
            .position(|p| p.gpio == gpio)
            .map(|i| i as i32)
            .unwrap_or(-1)
    }
    pub fn cur_board_id(&self) -> String {
        if self.boards.is_empty() {
            String::new()
        } else {
            self.cur_board().id.clone()
        }
    }

    /// Does `rel` ship an image the current board can run? True on an exact
    /// board-id match, or any image built for the same chip family (one binary
    /// per chip; board pins are configured at runtime via @PINS).
    pub fn release_has_image(&self, rel: &FwRelease) -> bool {
        let board = self.cur_board_id();
        if rel.board_bin.contains_key(&board) {
            return true;
        }
        let chip = self.cur_board().target.as_str();
        rel.board_bin.keys().any(|bid| {
            self.boards
                .iter()
                .find(|b| &b.id == bid)
                .map(|b| b.target.as_str())
                == Some(chip)
        })
    }
}
