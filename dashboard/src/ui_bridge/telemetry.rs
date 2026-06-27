use slint::ComponentHandle;
use std::sync::Arc;

use super::{model, sstr};
use crate::ctx::Ctx;
use crate::state::State;
use crate::telemetry::*;
use crate::{
    AppState, AppWindow, CarLib, DeviceCaps, Firmware, FwComponent, RaceLayout, ScreenSpec,
    Telemetry,
};

const TIMES: &str = "\u{00D7}";

fn c_atoi(s: &str) -> i32 {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut i = 0;
    let mut sign: i64 = 1;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        if b[i] == b'-' {
            sign = -1;
        }
        i += 1;
    }
    let mut n: i64 = 0;
    while i < b.len() && b[i].is_ascii_digit() {
        n = n * 10 + (b[i] - b'0') as i64;
        if n > i32::MAX as i64 {
            n = i32::MAX as i64;
        }
        i += 1;
    }
    (sign * n) as i32
}

fn c_atof(s: &str) -> f32 {
    let s = s.trim_start();
    let b = s.as_bytes();
    let mut end = 0;
    let mut seen_dot = false;
    let mut seen_e = false;
    while end < b.len() {
        let c = b[end];
        if c.is_ascii_digit() {
            end += 1;
        } else if (c == b'-' || c == b'+') && (end == 0 || b[end - 1] == b'e' || b[end - 1] == b'E')
        {
            end += 1;
        } else if c == b'.' && !seen_dot && !seen_e {
            seen_dot = true;
            end += 1;
        } else if (c == b'e' || c == b'E') && !seen_e && end > 0 {
            seen_e = true;
            end += 1;
        } else {
            break;
        }
    }
    s[..end].parse::<f32>().unwrap_or(0.0)
}

fn find_int(s: &str, key: &str, def: i32) -> i32 {
    match s.find(key) {
        Some(p) => c_atoi(&s[p + key.len()..]),
        None => def,
    }
}

pub fn apply_status(ui: &AppWindow, s: &mut State, ctx: &Arc<Ctx>, line: &str) {
    let t = ui.global::<Telemetry>();
    let gear = match line.find("g=") {
        Some(p) => line[p + 2..].chars().next().unwrap_or('\0'),
        None => 'N',
    };
    let speed = find_int(line, "s=", 0);
    let rpm = find_int(line, "r=", 0);
    let mut maxrpm = s.redline_rpm;
    if let Some(rp) = line.find("r=") {
        if let Some(sl) = line[rp..].find('/') {
            maxrpm = c_atoi(&line[rp + sl + 1..]);
        }
    }
    let delta = find_int(line, "delta=", 0);
    let mut fuel = 0.0f32;
    if let Some(fp) = line.find("fuel=") {
        fuel = c_atof(&line[fp + 5..]);
    }
    if let Some(cp) = line.find("car=") {
        let mut model = line[cp + 4..].to_string();
        while model.ends_with('\n') || model.ends_with('\r') || model.ends_with(' ') {
            model.pop();
        }
        if s.detected_game_idx < 0 {
            model.clear();
        }
        ui.global::<CarLib>().set_detected_car(sstr(&model));
        s.detected_model = model.clone();
        if !model.is_empty() {
            crate::net::cardata::auto_apply_car_model(ctx, s, &model);
        }
    }
    t.set_gear(sstr(&if gear == '\0' {
        String::new()
    } else {
        gear.to_string()
    }));
    t.set_speed(speed);
    t.set_rpm(rpm);
    let eff_max = if maxrpm > 1000 { maxrpm } else { s.redline_rpm };
    t.set_redline(eff_max);
    t.set_delta(delta as f32 / 10000.0);
    t.set_fuel(fuel);
    t.set_redline_active(rpm >= eff_max * 99 / 100);
    t.set_connected(true);
    t.set_game(sstr("device"));
    t.set_hz(6.0);

    s.gear_ch = gear;
    s.telem[FIELD_SPEED_KMH] = speed;
    s.telem[FIELD_RPM] = rpm;
    s.telem[FIELD_DELTA_MS] = delta;
    s.telem[FIELD_FUEL_DL] = (fuel * 10.0 + 0.5) as i32;
    super::race::push_resolved(ui, s);
    super::uidoc::push_preview(ui, s);
}

pub fn apply_telemetry(ui: &AppWindow, s: &mut State, line: &str) {
    if line.is_empty() {
        return;
    }
    for (idx, tok) in line.split(';').enumerate() {
        if idx == 0 {
            if let Some(c) = tok.chars().next() {
                s.gear_ch = c;
            }
        } else if idx < FIELD_COUNT {
            s.telem[idx] = c_atoi(tok);
        }
    }
    let t = ui.global::<Telemetry>();
    t.set_gear(sstr(&s.gear_ch.to_string()));
    t.set_speed(s.telem[FIELD_SPEED_KMH]);
    t.set_rpm(s.telem[FIELD_RPM]);
    let maxr = if s.telem[FIELD_MAX_RPM] > 1000 {
        s.telem[FIELD_MAX_RPM]
    } else {
        s.redline_rpm
    };
    t.set_redline(maxr);
    t.set_redline_active(s.telem[FIELD_RPM] >= maxr * 99 / 100);
    t.set_delta(s.telem[FIELD_DELTA_MS] as f32 / 10000.0);
    t.set_fuel(s.telem[FIELD_FUEL_DL] as f32 / 10.0);
    super::race::push_resolved(ui, s);
    super::uidoc::push_preview(ui, s);
}

pub fn apply_caps(ui: &AppWindow, s: &mut State, line: &str) {
    let j: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return,
    };
    let caps = ui.global::<DeviceCaps>();
    let mut screens: Vec<ScreenSpec> = Vec::new();
    let (mut main_res, mut side_res) = ("—".to_string(), "—".to_string());
    let (mut main_w, mut main_h, mut side_w, mut side_h) = (480, 320, 480, 320);
    if let Some(arr) = j.get("screens").and_then(|v| v.as_array()) {
        for (idx, sc) in arr.iter().enumerate() {
            let w = sc.get("w").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
            let h = sc.get("h").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
            let touch = sc.get("touch").and_then(|x| x.as_bool()).unwrap_or(false);
            let role = sc
                .get("role")
                .and_then(|x| x.as_str())
                .unwrap_or(if idx == 0 { "main" } else { "side" })
                .to_string();
            let res = format!("{w}{TIMES}{h}");
            if role == "main" {
                main_res = res.clone();
                main_w = w;
                main_h = h;
            } else if role == "side" {
                side_res = res.clone();
                side_w = w;
                side_h = h;
            }
            screens.push(ScreenSpec {
                role: sstr(&role),
                w,
                h,
                touch,
            });
        }
    }
    caps.set_main_w(main_w);
    caps.set_main_h(main_h);
    caps.set_side_w(side_w);
    caps.set_side_h(side_h);
    let (mut rev, mut tc, mut ab, mut sep, mut bp) = (0, 0, 0, true, 3);
    if let Some(l) = j.get("leds") {
        rev = l.get("rev").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
        tc = l.get("tc").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
        ab = l.get("abs").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
        sep = l.get("separate").and_then(|x| x.as_bool()).unwrap_or(true);
    }
    if let Some(b) = j.get("buttonPages").and_then(|x| x.as_i64()) {
        bp = b as i32;
    }
    caps.set_screens(model(screens));
    caps.set_main_res(sstr(&main_res));
    caps.set_side_res(sstr(&side_res));
    caps.set_rev_leds(rev);
    caps.set_tc_leds(tc);
    caps.set_abs_leds(ab);
    caps.set_separate_strip(sep);
    caps.set_button_pages(bp);
    caps.set_summary(sstr(&format!("Main {main_res} · Side {side_res}")));
    caps.set_known(true);

    if let Some(pj) = j.get("pins") {
        for i in 0..crate::catalog::PIN_N {
            s.pin_gpio[i] = pj
                .get(crate::catalog::PINDEFS[i].0)
                .and_then(|x| x.as_i64())
                .map(|x| x as i32)
                .unwrap_or(s.pin_gpio[i]);
        }
        s.race_screen = pj
            .get("race_screen")
            .and_then(|x| x.as_i64())
            .map(|x| x as i32)
            .unwrap_or(s.race_screen);
        s.led_rev = pj
            .get("led_rev")
            .and_then(|x| x.as_i64())
            .map(|x| x as i32)
            .unwrap_or(s.led_rev);
        s.led_tc = pj
            .get("led_tc")
            .and_then(|x| x.as_i64())
            .map(|x| x as i32)
            .unwrap_or(s.led_tc);
        s.led_abs = pj
            .get("led_abs")
            .and_then(|x| x.as_i64())
            .map(|x| x as i32)
            .unwrap_or(s.led_abs);
        s.led_rgbw = pj
            .get("led_rgbw")
            .and_then(|x| x.as_i64())
            .map(|x| x as i32)
            .unwrap_or(s.led_rgbw);
        super::device::push_pins(ui, s);
    }

    if let Some(d) = j.get("disp") {
        s.disp_rot = d
            .get("rot")
            .and_then(|x| x.as_i64())
            .map(|x| x as i32)
            .unwrap_or(s.disp_rot)
            .clamp(0, 3);
        s.disp_flip_h = d.get("fh").and_then(|x| x.as_bool()).unwrap_or(s.disp_flip_h);
        s.disp_flip_v = d.get("fv").and_then(|x| x.as_bool()).unwrap_or(s.disp_flip_v);
        let dc = ui.global::<crate::DeviceCfg>();
        dc.set_disp_rot(s.disp_rot);
        dc.set_disp_flip_h(s.disp_flip_h);
        dc.set_disp_flip_v(s.disp_flip_v);
    }

    let fwv = j
        .get("fw")
        .and_then(|x| x.as_str())
        .unwrap_or("?")
        .to_string();
    ui.global::<RaceLayout>()
        .set_main_res(sstr(if main_res == "—" {
            "480×320"
        } else {
            &main_res
        }));
    let app = ui.global::<AppState>();
    app.set_device_name(sstr(
        j.get("name").and_then(|x| x.as_str()).unwrap_or("Pith DDU"),
    ));
    app.set_fw_version(sstr(&format!("v{fwv}")));
    app.set_serial(sstr(j.get("serial").and_then(|x| x.as_str()).unwrap_or("")));

    let dev_board = j
        .get("board")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if !dev_board.is_empty() && dev_board != "unknown" {
        if let Some(bi) = s.boards.iter().position(|b| b.id == dev_board) {
            if s.board != bi as i32 {
                s.board = bi as i32;
                crate::persist::save_board(s);
                super::device::push_pins(ui, s);
            }
        }
    }

    let fw = ui.global::<Firmware>();
    fw.set_current(sstr(&format!("v{fwv}")));
    s.device_fw = if fwv == "?" {
        String::new()
    } else {
        fwv.clone()
    };
    super::firmware::recompute_update_available(ui, s);

    let mut comps = vec![
        FwComponent {
            name: sstr("Firmware"),
            version: sstr(&format!("v{fwv}")),
            updating: false,
            target: sstr(""),
        },
        FwComponent {
            name: sstr("Main display"),
            version: sstr(&main_res),
            updating: false,
            target: sstr(""),
        },
    ];
    if side_res != "—" {
        comps.push(FwComponent {
            name: sstr("Side display"),
            version: sstr(&side_res),
            updating: false,
            target: sstr(""),
        });
    }
    comps.push(FwComponent {
        name: sstr("Shift LEDs"),
        version: sstr(&format!("{rev} + {tc} + {ab}")),
        updating: false,
        target: sstr(""),
    });
    comps.push(FwComponent {
        name: sstr("Button pages"),
        version: sstr(&bp.to_string()),
        updating: false,
        target: sstr(""),
    });
    fw.set_components(model(comps));
}
