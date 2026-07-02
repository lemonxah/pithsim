use crate::state::{BoardDef, BoardPin, BtnData, ColorRule, ElemSpec, ModSpec, Preset, State, Zone};

pub const CATALOG: &[(&str, &str, &str)] = &[
    ("gearSpeed", "Gear + Speed", "Big gear glyph and speed"),
    ("rpmLights", "RPM Lights", "12-LED shift strip"),
    ("lapDelta", "Lap Delta", "Delta to best/optimal"),
    ("position", "Position", "Race position / field"),
    ("fuel", "Fuel", "Fuel remaining"),
    ("fuelPerLap", "Fuel per Lap", "Avg consumption"),
    ("tyres", "Tyre Temps", "4-corner tyre grid"),
    ("allTyres", "All Tyres", "Fullscreen tyre panel: temps, pressure, brake, wear"),
    ("tyreWear", "Tyre Wear", "4-corner wear %"),
    ("lapTimes", "Lap Times", "Current + best"),
    ("lastLap", "Last Lap", "Last lap time"),
    ("sectors", "Sectors", "S1/S2/S3 splits"),
    ("brakeBias", "Brake Bias", "Front bias %"),
    ("water", "Water Temp", "Coolant temp"),
    ("oil", "Oil Temp", "Oil temp"),
    ("battery", "Battery (ERS)", "Hybrid state-of-charge %"),
    ("ers", "ERS State", "Boost: idle/deploy/regen"),
    ("virtualEnergy", "Virtual Energy", "LMU energy budget %"),
    ("vePerLap", "VE per Lap", "Energy used per lap %"),
    ("tcAbs", "TC / ABS", "Aid levels"),
    ("tcTriple", "TC (3)", "TC level / slip / cut"),
    ("mapPosition", "Track Map", "Position dot on track"),
    ("flag", "Flag", "Current flag colour"),
    ("clock", "Session Clock", "Time remaining"),
    ("speed", "Speed only", "Speed value"),
    ("gear", "Gear only", "Gear glyph"),
    ("rpmValue", "RPM Value", "Numeric rpm"),
    ("relatives", "Relatives", "Cars near you on track + gaps"),
    ("standings", "Standings", "Race order + gap to leader"),
    ("button", "Button", "HID button (push / toggle)"),
];

pub const ZONE_KEYS: [&str; 5] = ["topStrip", "leftRail", "center", "rightRail", "bottom"];
pub const ZONE_TITLES: [&str; 5] = ["TOP STRIP", "LEFT RAIL", "CENTER", "RIGHT RAIL", "BOTTOM"];
pub const DEG: &str = "\u{00B0}C"; // temperature unit shown on temp widgets

pub fn zone_index(k: &str) -> usize {
    ZONE_KEYS.iter().position(|&z| z == k).unwrap_or(0)
}

pub fn mod_name(ty: &str) -> &str {
    CATALOG
        .iter()
        .find(|m| m.0 == ty)
        .map(|m| m.1)
        .unwrap_or(ty)
}

pub fn default_spec(ty: &str) -> ModSpec {
    let mut m = ModSpec {
        templ: ty.to_string(),
        enabled: true,
        ..Default::default()
    };
    let stat = |m: &mut ModSpec, f: &str, l: &str, u: &str, b: &str| {
        m.kind = "stat".into();
        m.field = f.into();
        m.label = l.into();
        m.unit = u.into();
        m.base = b.into();
    };
    match ty {
        "gearSpeed" => m.kind = "gearSpeed".into(),
        "gear" => m.kind = "gear".into(),
        "rpmLights" => m.kind = "rpmStrip".into(),
        "rpmValue" => stat(&mut m, "rpm", "RPM", "", "white"),
        "speed" => stat(&mut m, "speed_kmh", "KM/H", "", "white"),
        "lapDelta" => {
            stat(&mut m, "delta_ms", "DELTA", "", "amber");
            m.rules = vec![
                ColorRule {
                    op: "<".into(),
                    v: 0,
                    color: "green".into(),
                },
                ColorRule {
                    op: ">".into(),
                    v: 0,
                    color: "red".into(),
                },
            ];
        }
        "position" => {
            m.kind = "position".into();
            m.field = "position".into();
            m.label = "POS".into();
        }
        "fuel" => stat(&mut m, "fuel_dl", "FUEL", "L", "white"),
        "fuelPerLap" => stat(&mut m, "fuel_per_lap_ml", "FUEL/LAP", "L", "white"),
        "tyres" => {
            m.kind = "tyreGrid".into();
            m.field = "tt_avg_fl".into();
            m.unit = DEG.into();
        }
        "allTyres" => {
            m.kind = "tyrePanel".into();
            m.unit = DEG.into();
        }
        "tyreWear" => {
            m.kind = "tyreGrid".into();
            m.field = "tw_fl".into();
            m.unit = "%".into();
        }
        "lapTimes" => m.kind = "lapPair".into(),
        "lastLap" => stat(&mut m, "last_lap_ms", "LAST", "", "white"),
        "sectors" => m.kind = "sectors".into(),
        "brakeBias" => stat(&mut m, "brake_bias_x10", "BIAS", "%", "white"),
        "water" => {
            stat(&mut m, "water_c", "H2O", DEG, "white");
            m.rules = vec![ColorRule {
                op: ">".into(),
                v: 105,
                color: "red".into(),
            }];
        }
        "oil" => stat(&mut m, "oil_c", "OIL", DEG, "white"),
        "battery" => {
            stat(&mut m, "battery_pct", "BATT", "%", "green");
            m.rules = vec![
                ColorRule { op: "<".into(), v: 150, color: "red".into() },
                ColorRule { op: "<".into(), v: 350, color: "amber".into() },
            ];
        }
        "ers" => stat(&mut m, "ers_state", "ERS", "", "cyan"),
        "virtualEnergy" => {
            stat(&mut m, "virtual_energy", "ENERGY", "%", "green");
            m.rules = vec![
                ColorRule { op: "<".into(), v: 50, color: "red".into() },
                ColorRule { op: "<".into(), v: 150, color: "amber".into() },
            ];
        }
        "vePerLap" => stat(&mut m, "ve_per_lap", "VE/LAP", "%", "white"),
        "tcAbs" => m.kind = "tcDual".into(),
        "tcTriple" => m.kind = "tcTriple".into(),
        "mapPosition" => m.kind = "map".into(),
        "flag" => {
            m.kind = "flag".into();
            m.field = "flag".into();
            m.label = "FLAG".into();
            m.base = "dim".into(); // shown when no flag is out
            m.rules = vec![
                ColorRule { op: "==".into(), v: 1, color: "green".into() },
                ColorRule { op: "==".into(), v: 2, color: "amber".into() },
                ColorRule { op: "==".into(), v: 3, color: "blue".into() },
                ColorRule { op: "==".into(), v: 4, color: "white".into() },
                ColorRule { op: "==".into(), v: 5, color: "white".into() },
                ColorRule { op: "==".into(), v: 6, color: "red".into() },
            ];
        }
        // Relatives/standings: reuse `toggle` for the view mode (off = relative,
        // on = standings) and `hid` for the visible row count.
        "relatives" => {
            m.kind = "relatives".into();
            m.toggle = false;
            m.hid = 6;
        }
        "standings" => {
            m.kind = "relatives".into();
            m.toggle = true;
            m.hid = 6;
        }
        "button" => {
            m.kind = "button".into();
            m.label = "BTN".into();
            m.base = "dim".into();
            m.hid = 1; // node_add reassigns to the next free HID button on drop
        }
        _ => stat(&mut m, "speed_kmh", mod_name(ty), "", "white"),
    }
    m
}

// Race panel geometry: zone rects matching the firmware ZONES table. Used to give
// zone-authored modules concrete freeform rects when seeding the freeform editor.
const ZONES_GEO: [(&str, i32, i32, i32, i32, bool); 5] = [
    ("topStrip", 0, 2, 480, 42, true),
    ("leftRail", 4, 50, 128, 220, false),
    ("center", 136, 50, 208, 220, false),
    ("rightRail", 348, 50, 128, 220, false),
    ("bottom", 0, 276, 480, 42, true),
];

/// Lay a zone-based layout out into freeform nodes (on display 0), matching the
/// firmware's per-zone auto-layout — so existing presets open as draggable boxes.
pub fn zones_to_nodes(zones: &[Zone]) -> Vec<ModSpec> {
    let mut out: Vec<ModSpec> = Vec::new();
    for &(key, zx, zy, zw, zh, horiz) in ZONES_GEO.iter() {
        let zone = match zones.iter().find(|z| z.key == key) {
            Some(z) => z,
            None => continue,
        };
        let mods: Vec<&ModSpec> = zone.modules.iter().filter(|m| m.enabled).collect();
        let n = mods.len() as i32;
        if n == 0 {
            continue;
        }
        for (i, m) in mods.iter().enumerate() {
            let i = i as i32;
            let (x, y, w, h) = if horiz {
                (zx + zw * i / n, zy, zw / n, zh)
            } else {
                (zx, zy + zh * i / n, zw, zh / n)
            };
            let mut node = (*m).clone();
            node.x = x;
            node.y = y;
            node.w = w;
            node.h = h;
            node.display = 0;
            out.push(node);
        }
    }
    out
}

fn make_preset(uid: &mut i32, name: &str, builtin: bool, spec: &[(&str, &[&str])]) -> Preset {
    let mut p = Preset {
        name: name.into(),
        builtin,
        zones: Vec::new(),
        nodes: Vec::new(),
    };
    for z in 0..5 {
        let mut zn = Zone {
            key: ZONE_KEYS[z].into(),
            title: ZONE_TITLES[z].into(),
            modules: Vec::new(),
        };
        for (key, types) in spec {
            if zn.key == *key {
                for t in *types {
                    let mut ms = default_spec(t);
                    ms.id = format!("{t}-{uid}");
                    *uid += 1;
                    zn.modules.push(ms);
                }
            }
        }
        p.zones.push(zn);
    }
    p.nodes = zones_to_nodes(&p.zones);
    p
}

pub fn seed_presets(s: &mut State) {
    // A single, general-purpose starting layout. Users build their own from here
    // and save them as presets; we no longer ship a pile of canned variants.
    s.presets.clear();
    s.presets.push(make_preset(
        &mut s.uid,
        "Default",
        true,
        &[
            ("topStrip", &["rpmLights"]),
            ("leftRail", &["lapDelta", "position"]),
            ("center", &["gearSpeed"]),
            ("rightRail", &["fuel", "tyres"]),
            ("bottom", &["lapTimes"]),
        ],
    ));
    s.zones = s.presets[0].zones.clone();
    s.nodes = s.presets[0].nodes.clone();
    s.active_preset = 0;
}

pub fn seed_shift(s: &mut State) {
    const RAMP: [u32; 12] = [
        0x00E676, 0x00E676, 0x00E676, 0x00E676, 0xFFB300, 0xFFB300, 0xFFB300, 0xFFB300, 0xFF3B30,
        0xFF3B30, 0xFF3B30, 0xFF3B30,
    ];
    const THR: [i32; 12] = [62, 66, 70, 74, 78, 82, 85, 88, 91, 94, 97, 99];
    for gear in 1..=6 {
        for i in 0..12 {
            s.leds[gear][i].rgb = RAMP[i];
            s.leds[gear][i].threshold = THR[i];
        }
    }
}

pub const BUTTON_FIELDS: [&str; 7] = [
    "",
    "headlights",
    "wipers",
    "pit_limiter",
    "ignition",
    "tc_active",
    "abs_active",
];

#[allow(clippy::too_many_arguments)]
fn b(l: &str, t: bool, on: bool, a: &str, c: u32, sy: bool, f: &str, av: bool) -> BtnData {
    BtnData {
        label: l.into(),
        toggle: t,
        on,
        action: a.into(),
        col: c,
        sync: sy,
        field: f.into(),
        avail: av,
    }
}

pub fn seed_buttons(s: &mut State) {
    s.btn_pages.clear();
    s.btn_pages.push(vec![
        b(
            "Pit Limiter",
            true,
            false,
            "PitLimiter",
            0x00E5A0,
            true,
            "pit_limiter",
            true,
        ),
        b(
            "Headlights",
            true,
            true,
            "Headlights",
            0xFFB300,
            true,
            "headlights",
            true,
        ),
        b(
            "Wipers", true, false, "Wipers", 0x2E9DFF, true, "wipers", true,
        ),
        b("Radio", false, false, "Radio", 0x00E5A0, false, "", false),
        b("Marker", false, false, "Marker", 0x00E5A0, false, "", false),
        b(
            "Reset Lap",
            false,
            false,
            "ResetLap",
            0x00E5A0,
            false,
            "",
            false,
        ),
    ]);
    s.btn_pages.push(vec![
        b("TC+", false, false, "TCPlus", 0x00E5A0, false, "", false),
        b("TC-", false, false, "TCMinus", 0x00E5A0, false, "", false),
        b(
            "ABS",
            true,
            false,
            "ABS",
            0x2E9DFF,
            true,
            "abs_active",
            true,
        ),
        b("BB+", false, false, "BBPlus", 0x00E5A0, false, "", false),
        b("BB-", false, false, "BBMinus", 0x00E5A0, false, "", false),
        b("MAP+", false, false, "MapPlus", 0x00E5A0, false, "", false),
    ]);
    s.btn_pages.push(vec![
        b(
            "Ignition", true, false, "Ignition", 0xFFB300, false, "", false,
        ),
        b(
            "Starter", false, false, "Starter", 0x00E5A0, false, "", false,
        ),
        b(
            "Pit Speed",
            false,
            false,
            "PitSpeed",
            0x00E5A0,
            false,
            "",
            false,
        ),
        b(
            "DRS",
            true,
            false,
            "DRS",
            0x00E5A0,
            true,
            "DRSAvailable",
            false,
        ),
        b("Push2Pass", false, false, "P2P", 0x00E5A0, false, "", false),
        b(
            "Neutral", false, false, "Neutral", 0x00E5A0, false, "", false,
        ),
    ]);
}

pub const PINDEFS: &[(&str, &str)] = &[
    ("sclk", "SPI SCLK"),
    ("mosi", "SPI MOSI"),
    ("miso", "SPI MISO"),
    ("dc", "Shared DC"),
    ("disp1_cs", "Display 1 CS"),
    ("disp2_cs", "Display 2 CS"),
    ("touch1_cs", "Touch 1 CS"),
    ("touch2_cs", "Touch 2 CS"),
    ("led_din", "LED data"),
];
pub const PIN_N: usize = 9;

/// Element types that can go inside a widget (id, display name).
pub const ELEM_KINDS: &[(&str, &str)] = &[
    ("label", "Label (text)"),
    ("value", "Value"),
    ("bar", "Bar"),
    ("gear", "Gear"),
    ("gearSpeed", "Gear + Speed"),
    ("rpmStrip", "RPM Strip"),
    ("tyreGrid", "Tyre Grid"),
    ("tyrePanel", "All Tyres"),
    ("tcDual", "TC / ABS"),
    ("tcTriple", "TC (3)"),
    ("sectors", "Sectors"),
    ("lapPair", "Lap Times"),
    ("position", "Position"),
    ("flag", "Flag"),
    ("map", "Map"),
    ("button", "Button"),
];

pub fn elem_kind_name(id: &str) -> &str {
    ELEM_KINDS.iter().find(|e| e.0 == id).map(|e| e.1).unwrap_or(id)
}

/// Seed a widget's editable element tree from its built-in. `stat` decomposes into
/// a label + a value (so the label/value arrangement can be changed); every other
/// kind becomes a single element the user can build around.
pub fn default_els(m: &ModSpec) -> Vec<ElemSpec> {
    match m.kind.as_str() {
        "stat" => vec![
            ElemSpec {
                kind: "label".into(),
                text: m.label.clone(),
                base: "dim".into(),
                size: 11,
                flex: 1,
                ..Default::default()
            },
            ElemSpec {
                kind: "value".into(),
                field: m.field.clone(),
                fmt_type: m.fmt_type.clone(),
                unit: m.unit.clone(),
                scale: m.scale,
                base: m.base.clone(),
                size: m.size_pct,
                flex: 2,
                rules: m.rules.clone(),
                ..Default::default()
            },
        ],
        _ => vec![ElemSpec {
            kind: m.kind.clone(),
            field: m.field.clone(),
            text: m.label.clone(),
            fmt_type: m.fmt_type.clone(),
            unit: m.unit.clone(),
            scale: m.scale,
            base: m.base.clone(),
            size: m.size_pct,
            flex: 1,
            rules: m.rules.clone(),
            ..Default::default()
        }],
    }
}

pub fn seed_boards(s: &mut State) {
    let xiao = BoardDef {
        name: "Seeed XIAO ESP32-S3".into(),
        id: "xiao_s3".into(),
        target: "esp32s3".into(),
        pins: [
            ("D0", 1),
            ("D1", 2),
            ("D2", 3),
            ("D3", 4),
            ("D4", 5),
            ("D5", 6),
            ("D6", 43),
            ("D7", 44),
            ("D8", 7),
            ("D9", 8),
            ("D10", 9),
        ]
        .iter()
        .map(|(l, g)| BoardPin {
            label: (*l).into(),
            gpio: *g,
        })
        .collect(),
    };
    const GP_S3: &[i32] = &[
        1, 2, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 21, 38, 39, 40, 41, 42, 43, 44,
        45, 47, 48,
    ];
    const GP_S2: &[i32] = &[
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 33, 34, 35, 36, 37, 38, 39,
        40, 41, 42, 43, 44, 45,
    ];
    let gpio_board = |nm: &str, id: &str, target: &str, gp: &[i32]| BoardDef {
        name: nm.into(),
        id: id.into(),
        target: target.into(),
        pins: gp
            .iter()
            .map(|g| BoardPin {
                label: format!("GPIO{g}"),
                gpio: *g,
            })
            .collect(),
    };
    s.boards = vec![
        xiao,
        gpio_board("ESP32-S3-DevKitC-1", "devkitc_s3", "esp32s3", GP_S3),
        gpio_board("Waveshare ESP32-S3-Zero", "zero_s3", "esp32s3", GP_S3),
        gpio_board("Generic ESP32-S3", "generic_s3", "esp32s3", GP_S3),
        gpio_board("ESP32-S2 DevKit (1 screen)", "devkit_s2", "esp32s2", GP_S2),
        gpio_board("Lolin S2 Mini (1 screen)", "s2_mini", "esp32s2", GP_S2),
    ];
}

pub const GAME_PROCS: &[(&str, &str)] = &[
    ("iracingsim", "iracing"),
    ("ac2-win64-shipping", "assettocorsacompetizione"),
    ("acc.exe", "assettocorsacompetizione"),
    ("le mans ultimate", "lmu"),
    ("ams2avx", "automobilista2"),
    ("ams2", "automobilista2"),
    ("assettocorsaevo", "assettocorsaevo"),
    ("acevo", "assettocorsaevo"),
    ("acs.exe", "assettocorsa"),
    ("f1_24", "f12024"),
    ("f1_25", "f12025"),
    ("rrre", "rrre"),
    ("projectmotorracing", "projectmotorracing"),
];

