use serde_json::{json, Map, Value};

use crate::catalog::{zone_index, PINDEFS, ZONE_KEYS, ZONE_TITLES};
use crate::paths::*;
use crate::state::{BtnData, ColorRule, ModSpec, Preset, State, Zone};

fn jstr(v: &Value, k: &str, d: &str) -> String {
    v.get(k).and_then(|x| x.as_str()).unwrap_or(d).to_string()
}
fn jint(v: &Value, k: &str, d: i64) -> i64 {
    v.get(k).and_then(|x| x.as_i64()).unwrap_or(d)
}
fn jbool(v: &Value, k: &str, d: bool) -> bool {
    v.get(k).and_then(|x| x.as_bool()).unwrap_or(d)
}
fn jf64(v: &Value, k: &str, d: f64) -> f64 {
    v.get(k).and_then(|x| x.as_f64()).unwrap_or(d)
}

pub fn build_shift_json(s: &State) -> String {
    let g = s.sel_gear as usize;
    let mut j = Map::new();
    j.insert("ledNumber".into(), json!(12));
    let hz = if s.blink_hz < 1.0 {
        1.0
    } else {
        s.blink_hz as f64
    };
    j.insert(
        "redlineBlinkInterval".into(),
        json!((1000.0 / (2.0 * hz)) as i32),
    );
    j.insert("firstLedPct".into(), json!(s.first_led_pct));
    j.insert("animation".into(), json!(s.animation));
    let mut colors: Vec<Value> = Vec::new();
    colors.push(json!(format!("#FF{:06X}", s.leds[g][11].rgb & 0xFFFFFF)));
    for i in 0..12 {
        colors.push(json!(format!("#FF{:06X}", s.leds[g][i].rgb & 0xFFFFFF)));
    }
    j.insert("ledColor".into(), Value::Array(colors));
    let mut per_gear = Map::new();
    for gear in 1..=6usize {
        let mut arr: Vec<Value> = vec![json!(s.redline_rpm)];
        for i in 0..12 {
            arr.push(json!(s.redline_rpm * s.leds[gear][i].threshold / 100));
        }
        per_gear.insert(gear.to_string(), Value::Array(arr));
    }
    j.insert("ledRpm".into(), json!([Value::Object(per_gear)]));
    Value::Object(j).to_string()
}

pub fn build_race_layout_json(s: &State) -> String {
    let mut mods: Vec<Value> = Vec::new();
    for z in &s.zones {
        let mut order = 0;
        let zi = zone_index(&z.key);
        for m in &z.modules {
            if !m.enabled {
                continue;
            }
            let mut jm = Map::new();
            jm.insert("k".into(), json!(m.kind));
            if !m.field.is_empty() {
                jm.insert("f".into(), json!(m.field));
            }
            if !m.label.is_empty() {
                jm.insert("l".into(), json!(m.label));
            }
            let mut fmt = Map::new();
            if !m.fmt_type.is_empty() {
                fmt.insert("t".into(), json!(m.fmt_type));
            }
            if !m.unit.is_empty() {
                fmt.insert("u".into(), json!(m.unit));
            }
            if m.scale > 0 {
                fmt.insert("sc".into(), json!(m.scale));
            }
            if !fmt.is_empty() {
                jm.insert("fmt".into(), Value::Object(fmt));
            }
            if m.base != "white" && !m.base.is_empty() {
                jm.insert("b".into(), json!(m.base));
            }
            if m.size_pct > 0 {
                jm.insert("sz".into(), json!(m.size_pct));
            }
            if !m.rules.is_empty() {
                let r: Vec<Value> = m
                    .rules
                    .iter()
                    .map(|rl| json!({"op": rl.op, "v": rl.v, "c": rl.color}))
                    .collect();
                jm.insert("r".into(), Value::Array(r));
            }
            jm.insert("z".into(), json!(zi));
            jm.insert("o".into(), json!(order));
            order += 1;
            mods.push(Value::Object(jm));
        }
    }
    json!({"v": 1, "mods": mods}).to_string()
}

pub fn race_layout_from_json(s: &mut State, j: &Value) {
    let arr = match j.get("mods").and_then(|m| m.as_array()) {
        Some(a) => a,
        None => return,
    };
    let mut zones: Vec<Zone> = (0..5)
        .map(|z| Zone {
            key: ZONE_KEYS[z].into(),
            title: ZONE_TITLES[z].into(),
            modules: Vec::new(),
        })
        .collect();
    for jm in arr {
        let mut m = ModSpec {
            kind: jstr(jm, "k", "stat"),
            field: jstr(jm, "f", ""),
            label: jstr(jm, "l", ""),
            ..Default::default()
        };
        if let Some(fmt) = jm.get("fmt").filter(|f| f.is_object()) {
            m.fmt_type = jstr(fmt, "t", "");
            m.unit = jstr(fmt, "u", "");
            m.scale = jint(fmt, "sc", 0) as i32;
        }
        m.base = jstr(jm, "b", "white");
        m.size_pct = jint(jm, "sz", 0) as i32;
        if let Some(rules) = jm.get("r").and_then(|r| r.as_array()) {
            for r in rules {
                m.rules.push(ColorRule {
                    op: jstr(r, "op", ">"),
                    v: jint(r, "v", 0) as i32,
                    color: jstr(r, "c", "red"),
                });
            }
        }
        m.enabled = true;
        m.id = format!("m{}", s.uid);
        s.uid += 1;
        let mut z = jint(jm, "z", 0) as i32;
        if !(0..=4).contains(&z) {
            z = 0;
        }
        zones[z as usize].modules.push(m);
    }
    s.zones = zones;
    // @RG only carries the legacy zone layout (NOT the freeform nodes, which live
    // in the @UI UiDoc). Only seed the freeform editor from zones when it's empty —
    // otherwise reading/syncing would clobber the user's freeform edits with the
    // zone-derived default. (Full freeform round-trip is done via the editor echo.)
    if s.nodes.is_empty() {
        s.nodes = crate::catalog::zones_to_nodes(&s.zones);
    }
}

fn rules_to_json(rules: &[ColorRule]) -> Vec<Value> {
    rules
        .iter()
        .map(|rl| json!({"op": rl.op, "v": rl.v, "c": rl.color}))
        .collect()
}
fn rules_from_json(j: &Value) -> Vec<ColorRule> {
    j.get("rules")
        .and_then(|r| r.as_array())
        .map(|rs| {
            rs.iter()
                .map(|r| ColorRule {
                    op: jstr(r, "op", ">"),
                    v: jint(r, "v", 0) as i32,
                    color: jstr(r, "c", "red"),
                })
                .collect()
        })
        .unwrap_or_default()
}
fn elem_to_json(e: &crate::state::ElemSpec) -> Value {
    json!({
        "kind": e.kind, "flex": e.flex, "field": e.field, "text": e.text,
        "fmt": e.fmt_type, "unit": e.unit, "scale": e.scale, "base": e.base,
        "size": e.size, "align": e.align, "valign": e.valign, "action": e.action,
        "toggle": e.toggle, "hid": e.hid,
        "rules": rules_to_json(&e.rules),
    })
}
fn elem_from_json(j: &Value) -> crate::state::ElemSpec {
    crate::state::ElemSpec {
        kind: jstr(j, "kind", "label"),
        flex: jint(j, "flex", 1) as i32,
        field: jstr(j, "field", ""),
        text: jstr(j, "text", ""),
        fmt_type: jstr(j, "fmt", ""),
        unit: jstr(j, "unit", ""),
        scale: jint(j, "scale", 0) as i32,
        base: jstr(j, "base", "white"),
        size: jint(j, "size", 0) as i32,
        align: jint(j, "align", 1) as i32,
        valign: jint(j, "valign", 1) as i32,
        action: jstr(j, "action", ""),
        rules: rules_from_json(j),
        toggle: jbool(j, "toggle", false),
        hid: jint(j, "hid", 0) as i32,
    }
}

fn mod_to_json(m: &ModSpec) -> Value {
    let els: Vec<Value> = m.els.iter().map(elem_to_json).collect();
    json!({
        "id": m.id, "templ": m.templ, "kind": m.kind, "field": m.field,
        "label": m.label, "fmt": m.fmt_type, "unit": m.unit, "scale": m.scale,
        "base": m.base, "on_base": m.on_base, "sz": m.size_pct, "enabled": m.enabled, "rules": rules_to_json(&m.rules),
        "x": m.x, "y": m.y, "w": m.w, "h": m.h, "disp": m.display,
        "dir": m.dir, "gap": m.gap, "toggle": m.toggle, "hid": m.hid, "page": m.page, "els": els,
    })
}
fn mod_from_json(j: &Value) -> ModSpec {
    let m = ModSpec {
        id: jstr(j, "id", ""),
        templ: jstr(j, "templ", ""),
        kind: jstr(j, "kind", "stat"),
        field: jstr(j, "field", ""),
        label: jstr(j, "label", ""),
        fmt_type: jstr(j, "fmt", ""),
        unit: jstr(j, "unit", ""),
        scale: jint(j, "scale", 0) as i32,
        base: jstr(j, "base", "white"),
        on_base: jstr(j, "on_base", "green"),
        size_pct: jint(j, "sz", 0) as i32,
        enabled: jbool(j, "enabled", true),
        rules: rules_from_json(j),
        x: jint(j, "x", 0) as i32,
        y: jint(j, "y", 0) as i32,
        w: jint(j, "w", 0) as i32,
        h: jint(j, "h", 0) as i32,
        display: jint(j, "disp", 0) as u8,
        dir: jint(j, "dir", 0) as i32,
        gap: jint(j, "gap", 4) as i32,
        toggle: jbool(j, "toggle", false),
        hid: jint(j, "hid", 0) as i32,
        page: jint(j, "page", 0) as i32,
        els: j
            .get("els")
            .and_then(|e| e.as_array())
            .map(|a| a.iter().map(elem_from_json).collect())
            .unwrap_or_default(),
    };
    m
}

fn zones_to_json(zones: &[Zone]) -> Value {
    let zs: Vec<Value> = zones
        .iter()
        .map(|z| {
            let ms: Vec<Value> = z.modules.iter().map(mod_to_json).collect();
            json!({"key": z.key, "title": z.title, "modules": ms})
        })
        .collect();
    Value::Array(zs)
}

pub fn save_presets(s: &State) {
    let arr: Vec<Value> = s
        .presets
        .iter()
        .map(|p| json!({"name": p.name, "builtin": p.builtin, "zones": zones_to_json(&p.zones)}))
        .collect();
    let _ = std::fs::write(
        presets_path(),
        serde_json::to_string_pretty(&Value::Array(arr)).unwrap_or_default(),
    );
}

pub fn load_presets(s: &State) -> Option<(Vec<Preset>, i32)> {
    let body = read_file(&presets_path());
    if body.is_empty() {
        return None;
    }
    let j: Value = serde_json::from_str(&body).ok()?;
    let arr = j.as_array()?;
    if arr.is_empty() {
        return None;
    }
    let mut presets = Vec::new();
    for jp in arr {
        let mut p = Preset {
            name: jstr(jp, "name", "?"),
            builtin: jbool(jp, "builtin", false),
            zones: Vec::new(),
            nodes: Vec::new(),
        };
        if let Some(zones) = jp.get("zones").and_then(|z| z.as_array()) {
            for jz in zones {
                let mut z = Zone {
                    key: jstr(jz, "key", ""),
                    title: jstr(jz, "title", ""),
                    modules: Vec::new(),
                };
                if let Some(mods) = jz.get("modules").and_then(|m| m.as_array()) {
                    for jm in mods {
                        z.modules.push(mod_from_json(jm));
                    }
                }
                p.zones.push(z);
            }
        }
        // Prefer a stored freeform snapshot; otherwise derive from the zone layout.
        p.nodes = match jp.get("nodes").and_then(|n| n.as_array()) {
            Some(nodes) => nodes.iter().map(mod_from_json).collect(),
            None => crate::catalog::zones_to_nodes(&p.zones),
        };
        presets.push(p);
    }
    if presets.is_empty() {
        return None;
    }
    let _ = s;
    Some((presets, 0))
}

pub fn save_race_layout(s: &State) {
    let _ = std::fs::write(race_layout_path(), build_editor_layout_pretty(s));
}

/// The full editor layout as one JSON object (freeform nodes + zones + tabs).
fn editor_layout_value(s: &State) -> Value {
    let nodes: Vec<Value> = s.nodes.iter().map(mod_to_json).collect();
    json!({
        "active": s.active_preset,
        "zones": zones_to_json(&s.zones),
        "nodes": nodes,
        "tabs": [s.tabs[0].clone(), s.tabs[1].clone()],
        "map_track": s.map_track,
    })
}
fn build_editor_layout_pretty(s: &State) -> String {
    serde_json::to_string_pretty(&editor_layout_value(s)).unwrap_or_default()
}
/// Compact editor layout blob — pushed to the device via @EL so the GUI can read
/// its OWN full freeform layout back losslessly (the device just stores/echoes it).
pub fn build_editor_layout_json(s: &State) -> String {
    serde_json::to_string(&editor_layout_value(s)).unwrap_or_default()
}

/// Load a full editor layout (from @EG / a saved blob) back into State. Returns
/// false if there's no usable `nodes` array (so the caller can keep the current one).
pub fn apply_editor_layout_json(s: &mut State, j: &Value) -> bool {
    let nodes = match j.get("nodes").and_then(|n| n.as_array()) {
        Some(n) if !n.is_empty() => n,
        _ => return false,
    };
    s.nodes = nodes.iter().map(mod_from_json).collect();
    // fresh unique ids so they can't collide with the running uid counter
    for m in &mut s.nodes {
        m.id = format!("m{}", s.uid);
        s.uid += 1;
    }
    if let Some(zones) = j.get("zones").and_then(|z| z.as_array()) {
        let mut zs: Vec<Zone> = Vec::new();
        for jz in zones {
            let mut z = Zone {
                key: jstr(jz, "key", ""),
                title: jstr(jz, "title", ""),
                modules: Vec::new(),
            };
            if let Some(mods) = jz.get("modules").and_then(|m| m.as_array()) {
                for jm in mods {
                    z.modules.push(mod_from_json(jm));
                }
            }
            zs.push(z);
        }
        if !zs.is_empty() {
            s.zones = zs;
        }
    }
    if let Some(tabs) = j.get("tabs").and_then(|t| t.as_array()) {
        for (i, tv) in tabs.iter().take(2).enumerate() {
            s.tabs[i] = tv
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(|x| x.to_string()))
                        .collect()
                })
                .unwrap_or_default();
        }
    }
    s.map_track = jstr(j, "map_track", "(none)");
    true
}

/// Per-display tab names from a stored race layout (empty when not tabbed).
pub fn load_race_tabs() -> [Vec<String>; 2] {
    let body = read_file(&race_layout_path());
    let mut out = [Vec::new(), Vec::new()];
    if let Ok(j) = serde_json::from_str::<Value>(&body) {
        if let Some(arr) = j.get("tabs").and_then(|t| t.as_array()) {
            for (i, disp) in arr.iter().take(2).enumerate() {
                if let Some(names) = disp.as_array() {
                    out[i] = names
                        .iter()
                        .filter_map(|n| n.as_str().map(|s| s.to_string()))
                        .collect();
                }
            }
        }
    }
    out
}

/// Selected track-map name from a stored race layout (default "(none)").
pub fn load_map_track() -> String {
    let body = read_file(&race_layout_path());
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|j| {
            j.get("map_track")
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "(none)".to_string())
}

pub fn load_race_layout() -> Option<(Vec<Zone>, Vec<ModSpec>, i32)> {
    let body = read_file(&race_layout_path());
    if body.is_empty() {
        return None;
    }
    let j: Value = serde_json::from_str(&body).ok()?;
    let zarr = j.get("zones")?.as_array()?;
    if zarr.is_empty() {
        return None;
    }
    let mut zones = Vec::new();
    for jz in zarr {
        let mut z = Zone {
            key: jstr(jz, "key", ""),
            title: jstr(jz, "title", ""),
            modules: Vec::new(),
        };
        if let Some(mods) = jz.get("modules").and_then(|m| m.as_array()) {
            for jm in mods {
                z.modules.push(mod_from_json(jm));
            }
        }
        zones.push(z);
    }
    if zones.is_empty() {
        return None;
    }
    // Freeform nodes: stored snapshot if present, else derived from the zones.
    let nodes = match j.get("nodes").and_then(|n| n.as_array()) {
        Some(arr) if !arr.is_empty() => arr.iter().map(mod_from_json).collect(),
        _ => crate::catalog::zones_to_nodes(&zones),
    };
    Some((zones, nodes, jint(&j, "active", 0) as i32))
}

pub fn build_buttons_json(s: &State) -> String {
    let pages: Vec<Value> = s
        .btn_pages
        .iter()
        .map(|page| {
            let btns: Vec<Value> = page
                .iter()
                .map(|b| {
                    json!({
                        "label": b.label,
                        "kind": if b.toggle { "toggle" } else { "push" },
                        "action": b.action,
                        "color": format!("{:06X}", b.col & 0xFFFFFF),
                        "sync": b.sync,
                        "field": b.field,
                    })
                })
                .collect();
            Value::Array(btns)
        })
        .collect();
    json!({"pages": pages}).to_string()
}

pub fn save_buttons(s: &State) {
    let _ = std::fs::write(buttons_path(), build_buttons_json(s));
}

pub fn load_buttons() -> Option<Vec<Vec<BtnData>>> {
    let body = read_file(&buttons_path());
    if body.is_empty() {
        return None;
    }
    let j: Value = serde_json::from_str(&body).ok()?;
    let parr = j.get("pages")?.as_array()?;
    if parr.is_empty() {
        return None;
    }
    let mut pages = Vec::new();
    for pg in parr {
        let mut page = Vec::new();
        if let Some(btns) = pg.as_array() {
            for b in btns {
                let field = jstr(b, "field", "");
                let avail = !field.is_empty();
                let col = crate::util::hex_prefix(&jstr(b, "color", "00E5A0")) & 0xFFFFFF;
                page.push(BtnData {
                    label: jstr(b, "label", ""),
                    toggle: jstr(b, "kind", "push") == "toggle",
                    on: false,
                    action: jstr(b, "action", ""),
                    col,
                    sync: jbool(b, "sync", false),
                    field,
                    avail,
                });
            }
        }
        if !page.is_empty() {
            pages.push(page);
        }
    }
    if pages.is_empty() {
        None
    } else {
        Some(pages)
    }
}

pub fn build_pins_json(s: &State) -> String {
    let mut j = Map::new();
    for (pd, gpio) in PINDEFS.iter().zip(s.pin_gpio.iter()) {
        j.insert(pd.0.into(), json!(gpio));
    }
    j.insert("race_screen".into(), json!(s.race_screen));
    j.insert("led_rev".into(), json!(s.led_rev));
    j.insert("led_tc".into(), json!(s.led_tc));
    j.insert("led_abs".into(), json!(s.led_abs));
    j.insert("led_rgbw".into(), json!(s.led_rgbw));
    Value::Object(j).to_string()
}

pub fn save_board(s: &State) {
    let _ = std::fs::write(board_path(), s.board.to_string());
}
pub fn load_board(s: &mut State) {
    if let Ok(txt) = std::fs::read_to_string(board_path()) {
        if let Ok(b) = txt.trim().parse::<i32>() {
            if b >= 0 && (b as usize) < s.boards.len() {
                s.board = b;
            }
        }
    }
}

pub fn save_active_car(s: &State) {
    let mut gj: Vec<Value> = Vec::new();
    for gear in 0..7 {
        let row: Vec<Value> = (0..12)
            .map(|i| json!({"rgb": s.leds[gear][i].rgb, "thr": s.leds[gear][i].threshold}))
            .collect();
        gj.push(Value::Array(row));
    }
    let j = json!({
        "name": s.car_name, "game": s.car_game, "id": s.car_id,
        "redline": s.redline_rpm, "leds": gj,
    });
    let _ = std::fs::write(active_car_path(), j.to_string());
}

pub fn load_active_car(s: &mut State) -> bool {
    let body = read_file(&active_car_path());
    if body.is_empty() {
        return false;
    }
    let j: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return false,
    };
    s.car_name = jstr(&j, "name", &s.car_name);
    s.car_game = jstr(&j, "game", &s.car_game);
    s.car_id = jstr(&j, "id", &s.car_id);
    let rl = jint(&j, "redline", 0) as i32;
    if rl > 0 {
        s.redline_rpm = rl;
    }
    if let Some(leds) = j.get("leds").and_then(|l| l.as_array()) {
        for (gear, row) in leds.iter().enumerate() {
            if gear >= 7 {
                break;
            }
            if let Some(cells) = row.as_array() {
                for (i, c) in cells.iter().enumerate() {
                    if i >= 12 {
                        break;
                    }
                    s.leds[gear][i].rgb = c.get("rgb").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                    s.leds[gear][i].threshold = jint(c, "thr", 0) as i32;
                }
            }
        }
    }
    true
}

pub fn save_udp_cfg(s: &State) {
    let j = json!({
        "port": s.udp_port,
        "accEnabled": s.acc_enabled,
        "accHost": s.acc_host,
        "accPort": s.acc_port,
        "accPassword": s.acc_password,
        "acEnabled": s.ac_enabled,
        "acHost": s.ac_host,
        "acPort": s.ac_port,
        "gt7Enabled": s.gt7_enabled,
        "gt7Host": s.gt7_host,
        "shmEnabled": s.shm_enabled,
        "wifiSsid": s.wifi_ssid,
        "wifiPass": s.wifi_pass,
        "wifiDduEnabled": s.wifi_ddu_enabled,
        "wifiDduInput": s.wifi_ddu_input,
        "wifiHbInput": s.wifi_hb_input,
        "wifiPedalsInput": s.wifi_pedals_input,
        "pedalsAutoSwitch": s.pedals_auto_switch,
    });
    let _ = std::fs::write(udp_cfg_path(), j.to_string());
}

pub fn load_udp_cfg(s: &mut State) {
    let body = read_file(&udp_cfg_path());
    if body.is_empty() {
        return;
    }
    let j: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return,
    };
    let port = jint(&j, "port", s.udp_port as i64);
    if (1..=65535).contains(&port) {
        s.udp_port = port as u16;
    }
    s.acc_enabled = jbool(&j, "accEnabled", s.acc_enabled);
    s.acc_host = jstr(&j, "accHost", &s.acc_host);
    s.acc_port = clamp_port(jint(&j, "accPort", s.acc_port as i64), s.acc_port);
    s.acc_password = jstr(&j, "accPassword", &s.acc_password);
    s.ac_enabled = jbool(&j, "acEnabled", s.ac_enabled);
    s.ac_host = jstr(&j, "acHost", &s.ac_host);
    s.ac_port = clamp_port(jint(&j, "acPort", s.ac_port as i64), s.ac_port);
    s.gt7_enabled = jbool(&j, "gt7Enabled", s.gt7_enabled);
    s.gt7_host = jstr(&j, "gt7Host", &s.gt7_host);
    s.shm_enabled = jbool(&j, "shmEnabled", s.shm_enabled);
    s.wifi_ssid = jstr(&j, "wifiSsid", &s.wifi_ssid);
    s.wifi_pass = jstr(&j, "wifiPass", &s.wifi_pass);
    s.wifi_ddu_enabled = jbool(&j, "wifiDduEnabled", s.wifi_ddu_enabled);
    s.wifi_ddu_input = jbool(&j, "wifiDduInput", s.wifi_ddu_input);
    // Legacy single toggle (pre-Wireless-screen) seeds both input flags;
    // the per-hardware keys override it when present.
    let legacy_input = jbool(&j, "wifiInputEnabled", false);
    s.wifi_hb_input = jbool(&j, "wifiHbInput", legacy_input);
    s.wifi_pedals_input = jbool(&j, "wifiPedalsInput", legacy_input);
    s.pedals_auto_switch = jbool(&j, "pedalsAutoSwitch", s.pedals_auto_switch);
}

fn clamp_port(v: i64, default: u16) -> u16 {
    if (1..=65535).contains(&v) {
        v as u16
    } else {
        default
    }
}

pub fn save_shift_cfg(s: &State) {
    let j = json!({
        "firstLedPct": s.first_led_pct,
        "blinkEnabled": s.blink_enabled,
        "blinkHz": s.blink_hz,
        "animation": s.animation,
        "brightness": s.brightness,
        "rpmSource": s.rpm_source,
        "selGear": s.sel_gear,
        "shiftCustom": s.shift_custom,
        "redlineRpm": s.redline_rpm,
    });
    let _ = std::fs::write(shift_cfg_path(), j.to_string());
}

pub fn load_shift_cfg(s: &mut State) {
    let body = read_file(&shift_cfg_path());
    if body.is_empty() {
        return;
    }
    let j: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(_) => return,
    };
    s.first_led_pct = jint(&j, "firstLedPct", s.first_led_pct as i64) as i32;
    s.blink_enabled = jbool(&j, "blinkEnabled", s.blink_enabled);
    s.blink_hz = jf64(&j, "blinkHz", s.blink_hz as f64) as f32;
    s.animation = jint(&j, "animation", s.animation as i64) as i32;
    s.brightness = jint(&j, "brightness", s.brightness as i64) as i32;
    s.rpm_source = jint(&j, "rpmSource", s.rpm_source as i64) as i32;
    s.sel_gear = jint(&j, "selGear", s.sel_gear as i64) as i32;
    s.shift_custom = jbool(&j, "shiftCustom", s.shift_custom);
    if s.sel_gear < 0 || s.sel_gear > 6 {
        s.sel_gear = 1;
    }
    if s.rpm_source == 1 {
        let rl = jint(&j, "redlineRpm", 0) as i32;
        if rl > 0 {
            s.redline_rpm = rl;
        }
    }
}
