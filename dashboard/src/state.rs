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
    pub base: String,
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
            scale: 0,
            size_pct: 0,
            rules: Vec::new(),
            enabled: true,
            x: 0,
            y: 0,
            w: 0,
            h: 0,
            display: 0,
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

#[derive(Clone)]
pub struct SimRow {
    pub id: String,
    pub label: String,
    pub expr: String,
    pub enabled: bool,
    pub builtin: bool,
}

#[derive(Clone, Default)]
pub struct FwRelease {
    pub tag: String,
    pub board_bin: BTreeMap<String, String>,
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
    #[allow(dead_code)] // board metadata; the Rust firmware target is fixed (esp32s3)
    pub target: String,
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
    pub boards: Vec<BoardDef>,
    pub board: i32,

    pub device_fw: String,
    pub serial_ports: Vec<crate::device::PortInfo>,
    pub releases: Vec<FwRelease>,

    pub sim: Vec<SimRow>,
    pub sim_uid: i32,
    pub sim_query: String,
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
            led_rev: 12,
            led_tc: 2,
            led_abs: 2,
            led_rgbw: 1,
            boards: Vec::new(),
            board: 0,
            device_fw: String::new(),
            serial_ports: Vec::new(),
            releases: Vec::new(),
            sim: Vec::new(),
            sim_uid: 0,
            sim_query: String::new(),
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
}
