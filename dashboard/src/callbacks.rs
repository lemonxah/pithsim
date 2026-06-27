use slint::ComponentHandle;
use std::sync::Arc;

use slint::{Color, SharedString};

use crate::catalog::{default_spec, PINDEFS, PIN_N};
use crate::clipboard::copy_to_clipboard;
use crate::ctx::Ctx;
use crate::persist::*;
use crate::state::{BtnData, ColorRule, ModSpec, Preset, State};
use crate::ui_bridge::buttons::push_buttons_model;
use crate::ui_bridge::cars::{push_car_results, push_classes, rebuild_filtered};
use crate::ui_bridge::device::push_pins;
use crate::ui_bridge::firmware::{refresh_serial_ports, update_release_board_match};
use crate::ui_bridge::race::{push_edit_module, push_presets};
use crate::ui_bridge::shift::{pull_shift_scalars, push_led_model};
use crate::ui_bridge::simhub::{push_sim_fields, regen_simhub};
use crate::ui_bridge::{refresh_race, sstr, to_u32};
use crate::util::atoi;
use crate::{
    AppState, AppWindow, Buttons, CarLib, DeviceCfg, Firmware, RaceLayout, ShiftCfg, SimHub,
};

fn mark_dirty(u: &AppWindow, s: &State) {
    u.global::<RaceLayout>().set_dirty(true);
    save_race_layout(s);
}

fn with_selected<F: FnOnce(&mut ModSpec)>(u: &AppWindow, s: &mut State, f: F) -> bool {
    // The freeform editor selects nodes by id across all displays.
    let id = u.global::<RaceLayout>().get_sel_id().to_string();
    if let Some(m) = s.nodes.iter_mut().find(|m| m.id == id) {
        f(m);
        true
    } else {
        false
    }
}

const CANVAS_W: i32 = 480;
const CANVAS_H: i32 = 320;
const SNAP_PX: i32 = 6;

/// Snap a dragged node's rect to alignment guides: the screen centre/edges and
/// every other node's left/centre/right + top/centre/bottom. Returns the snapped
/// (x, y) plus the active guide lines (device x / y; -1 = none) for drawing.
fn snap_pos(
    nodes: &[ModSpec],
    drag_id: &str,
    display: u8,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> (i32, i32, i32, i32) {
    let mut vc = vec![0, CANVAS_W / 2, CANVAS_W];
    let mut hc = vec![0, CANVAS_H / 2, CANVAS_H];
    for n in nodes {
        if n.id == drag_id || n.display != display {
            continue;
        }
        vc.extend_from_slice(&[n.x, n.x + n.w / 2, n.x + n.w]);
        hc.extend_from_slice(&[n.y, n.y + n.h / 2, n.y + n.h]);
    }
    // best snap offset over the three edges (start/centre/end) against candidates
    let best = |edges: [i32; 3], cands: &[i32]| -> (i32, i32) {
        let (mut bd, mut off, mut guide) = (SNAP_PX + 1, 0, -1);
        for e in edges {
            for &c in cands {
                let d = (c - e).abs();
                if d < bd {
                    bd = d;
                    off = c - e;
                    guide = c;
                }
            }
        }
        if bd <= SNAP_PX {
            (off, guide)
        } else {
            (0, -1)
        }
    };
    let (ox, gv) = best([x, x + w / 2, x + w], &vc);
    let (oy, gh) = best([y, y + h / 2, y + h], &hc);
    (x + ox, y + oy, gv, gh)
}

/// Align the selected node's centre to the nearest other node on the same display:
/// `horizontal` aligns the Y centres (a row), otherwise the X centres (a column).
fn align_node(c: &Arc<Ctx>, horizontal: bool) {
    let u = match c.ui.upgrade() {
        Some(u) => u,
        None => return,
    };
    let mut st = c.lock();
    let id = u.global::<RaceLayout>().get_sel_id().to_string();
    let disp = st.edit_display;
    let sel = st.nodes.iter().find(|m| m.id == id).map(|m| {
        if horizontal {
            (m.y + m.h / 2, m.h)
        } else {
            (m.x + m.w / 2, m.w)
        }
    });
    let (sc, sz) = match sel {
        Some(v) => v,
        None => return,
    };
    let target = st
        .nodes
        .iter()
        .filter(|m| m.id != id && m.display == disp)
        .map(|m| if horizontal { m.y + m.h / 2 } else { m.x + m.w / 2 })
        .min_by_key(|cc| (cc - sc).abs());
    if let Some(tc) = target {
        if let Some(m) = st.nodes.iter_mut().find(|m| m.id == id) {
            if horizontal {
                m.y = (tc - sz / 2).clamp(0, CANVAS_H - m.h);
            } else {
                m.x = (tc - sz / 2).clamp(0, CANVAS_W - m.w);
            }
        }
        mark_dirty(&u, &st);
        refresh_race(&u, &st);
    }
}

pub fn wire_callbacks(ui: &AppWindow, ctx: &Arc<Ctx>) {
    {
        let app = ui.global::<AppState>();
        app.on_connect(|| {});
        let c = ctx.clone();
        app.on_disconnect(move || crate::loops::dash_close(&c));
        let c = ctx.clone();
        app.on_sync_device(move || crate::loops::sync_from_device(&c));
        app.on_minimize(|| {});
        let c = ctx.clone();
        app.on_close(move || {
            if c.tray_active.load(std::sync::atomic::Ordering::SeqCst) {
                crate::tray::hide_window(&c.ui);
            } else {
                let _ = slint::quit_event_loop();
            }
        });
    }

    {
        let s = ui.global::<ShiftCfg>();
        let c = ctx.clone();
        s.on_select_gear(move |gear| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.sel_gear = gear;
                push_led_model(&u, &st);
                save_shift_cfg(&st);
            }
        });
        let c = ctx.clone();
        s.on_set_led_threshold(move |idx, pct| {
            if !(0..12).contains(&idx) {
                return;
            }
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                let g = st.sel_gear as usize;
                st.leds[g][idx as usize].threshold = pct;
                st.shift_custom = true;
                push_led_model(&u, &st);
                save_active_car(&st);
            }
        });
        let c = ctx.clone();
        s.on_set_led_color(move |idx, color: Color| {
            if !(0..12).contains(&idx) {
                return;
            }
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                let g = st.sel_gear as usize;
                st.leds[g][idx as usize].rgb = to_u32(color);
                st.shift_custom = true;
                push_led_model(&u, &st);
                save_active_car(&st);
            }
        });
        let c = ctx.clone();
        s.on_save(move || {
            let u = match c.ui.upgrade() {
                Some(u) => u,
                None => return,
            };
            {
                let mut st = c.lock();
                pull_shift_scalars(&u, &mut st);
                st.shift_custom = true;
                save_shift_cfg(&st);
            }
            if c.dash().connected() {
                let (json, bright) = {
                    let st = c.lock();
                    (build_shift_json(&st), st.brightness)
                };
                c.dash().push_shift(&json);
                c.dash().set_brightness(bright);
            }
        });
        let c = ctx.clone();
        s.on_persist(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                pull_shift_scalars(&u, &mut st);
                save_shift_cfg(&st);
            }
        });
    }

    {
        let rl = ui.global::<RaceLayout>();
        let c = ctx.clone();
        rl.on_select_preset(move |i| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if i < 0 || i as usize >= st.presets.len() {
                    return;
                }
                st.zones = st.presets[i as usize].zones.clone();
                st.nodes = st.presets[i as usize].nodes.clone();
                st.active_preset = i;
                st.race_dirty = false;
                let rl = u.global::<RaceLayout>();
                rl.set_dirty(false);
                rl.set_sel_id(sstr(""));
                refresh_race(&u, &st);
                save_race_layout(&st);
            }
        });
        let c = ctx.clone();
        rl.on_new_preset(move |name: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                let p = Preset {
                    name: name.to_string(),
                    builtin: false,
                    zones: st.zones.clone(),
                    nodes: st.nodes.clone(),
                };
                st.presets.push(p);
                st.active_preset = st.presets.len() as i32 - 1;
                st.race_dirty = false;
                u.global::<RaceLayout>().set_dirty(false);
                push_presets(&u, &st);
                save_presets(&st);
            }
        });
        let c = ctx.clone();
        rl.on_update_preset(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                let a = st.active_preset;
                if a < 0 || a as usize >= st.presets.len() {
                    return;
                }
                st.presets[a as usize].zones = st.zones.clone();
                st.presets[a as usize].nodes = st.nodes.clone();
                st.race_dirty = false;
                let rl = u.global::<RaceLayout>();
                rl.set_dirty(false);
                rl.set_save_status(sstr("Saved to preset"));
                push_presets(&u, &st);
                save_presets(&st);
            }
        });
        let c = ctx.clone();
        rl.on_delete_preset(move |i| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if i < 0 || i as usize >= st.presets.len() || st.presets[i as usize].builtin {
                    return;
                }
                st.presets.remove(i as usize);
                if st.active_preset >= st.presets.len() as i32 {
                    st.active_preset = 0;
                }
                push_presets(&u, &st);
                save_presets(&st);
            }
        });
        let c = ctx.clone();
        rl.on_toggle_module(move |zk: SharedString, id: SharedString, on| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if let Some(z) = st.zones.iter_mut().find(|z| z.key == zk.as_str()) {
                    for m in z.modules.iter_mut() {
                        if m.id == id.as_str() {
                            m.enabled = on;
                        }
                    }
                }
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_add_module(move |zk: SharedString, ty: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if let Some(zi) = st.zones.iter().position(|z| z.key == zk.as_str()) {
                    let mut m = default_spec(ty.as_str());
                    let id = format!("{}-{}", ty.as_str(), st.uid);
                    st.uid += 1;
                    m.id = id.clone();
                    st.zones[zi].modules.push(m);
                    let rl = u.global::<RaceLayout>();
                    rl.set_sel_zone(zk.clone());
                    rl.set_sel_id(sstr(&id));
                }
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_remove_module(move |zk: SharedString, id: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if let Some(z) = st.zones.iter_mut().find(|z| z.key == zk.as_str()) {
                    z.modules.retain(|m| m.id != id.as_str());
                }
                if u.global::<RaceLayout>().get_sel_id().as_str() == id.as_str() {
                    u.global::<RaceLayout>().set_sel_id(sstr(""));
                }
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_move_module(move |zk: SharedString, from, to| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if let Some(z) = st.zones.iter_mut().find(|z| z.key == zk.as_str()) {
                    let n = z.modules.len() as i32;
                    if from >= 0 && from < n && to >= 0 && to < n && from != to {
                        let m = z.modules.remove(from as usize);
                        z.modules.insert(to as usize, m);
                        mark_dirty(&u, &st);
                        refresh_race(&u, &st);
                    }
                }
            }
        });
        let c = ctx.clone();
        rl.on_select_module(move |zk: SharedString, id: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let rl = u.global::<RaceLayout>();
                rl.set_sel_zone(zk);
                rl.set_sel_id(id);
                let st = c.lock();
                push_edit_module(&u, &st);
            }
        });
        macro_rules! mod_setter {
            ($method:ident, $field:ident) => {{
                let c = ctx.clone();
                rl.$method(move |v: SharedString| {
                    if let Some(u) = c.ui.upgrade() {
                        let mut st = c.lock();
                        with_selected(&u, &mut st, |m| m.$field = v.to_string());
                        mark_dirty(&u, &st);
                        refresh_race(&u, &st);
                    }
                });
            }};
        }
        mod_setter!(on_set_mod_kind, kind);
        mod_setter!(on_set_mod_field, field);
        mod_setter!(on_set_mod_label, label);
        mod_setter!(on_set_mod_unit, unit);
        mod_setter!(on_set_mod_fmt, fmt_type);
        mod_setter!(on_set_mod_base, base);
        let c = ctx.clone();
        rl.on_set_mod_size(move |v| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                with_selected(&u, &mut st, |m| m.size_pct = v.clamp(0, 100));
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_add_rule(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                with_selected(&u, &mut st, |m| {
                    m.rules.push(ColorRule {
                        op: ">".into(),
                        v: 0,
                        color: "red".into(),
                    })
                });
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_set_rule(
            move |idx, op: SharedString, v: SharedString, color: SharedString| {
                if let Some(u) = c.ui.upgrade() {
                    let mut st = c.lock();
                    with_selected(&u, &mut st, |m| {
                        if idx >= 0 && (idx as usize) < m.rules.len() {
                            m.rules[idx as usize] = ColorRule {
                                op: op.to_string(),
                                v: atoi(v.as_str()),
                                color: color.to_string(),
                            };
                        }
                    });
                    mark_dirty(&u, &st);
                    refresh_race(&u, &st);
                }
            },
        );
        let c = ctx.clone();
        rl.on_remove_rule(move |idx| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                with_selected(&u, &mut st, |m| {
                    if idx >= 0 && (idx as usize) < m.rules.len() {
                        m.rules.remove(idx as usize);
                    }
                });
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_save(move || {
            let u = match c.ui.upgrade() {
                Some(u) => u,
                None => return,
            };
            let rl = u.global::<RaceLayout>();
            let msg = if !c.dash().connected() {
                "Not connected".to_string()
            } else {
                let (race_json, ui_json) = {
                    let st = c.lock();
                    (
                        build_race_layout_json(&st),
                        crate::ui_bridge::uidoc::build_uidoc_json(&st),
                    )
                };
                // Push the legacy @RS layout (fallback) AND the pith-ui UiDoc (@UI),
                // which the firmware renders with dirty-rect when present.
                let ok_race = c.dash().push_race(&race_json);
                let ok_ui = c.dash().push_ui(&ui_json);
                if ok_race || ok_ui {
                    let mut st = c.lock();
                    st.race_dirty = false;
                    rl.set_dirty(false);
                    "Saved to device".to_string()
                } else {
                    "Device rejected — update firmware".to_string()
                }
            };
            rl.set_save_status(sstr(&msg));
        });
        let c = ctx.clone();
        rl.on_read_device(move || crate::loops::read_race_from_device(&c));

        // ---- freeform editor: select / add / remove / drag / resize / display ----
        let c = ctx.clone();
        rl.on_node_select(move |id: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                u.global::<RaceLayout>().set_sel_id(id);
                let st = c.lock();
                push_edit_module(&u, &st);
                crate::ui_bridge::race::push_nodes(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_node_add(move |ty: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                let mut m = default_spec(ty.as_str());
                let id = format!("{}-{}", ty.as_str(), st.uid);
                st.uid += 1;
                m.id = id.clone();
                m.display = st.edit_display;
                // Drop a default 140×70 box near the middle of the panel.
                m.x = 170;
                m.y = 125;
                m.w = 140;
                m.h = 70;
                st.nodes.push(m);
                u.global::<RaceLayout>().set_sel_id(sstr(&id));
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_node_remove(move |id: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.nodes.retain(|m| m.id != id.as_str());
                if u.global::<RaceLayout>().get_sel_id().as_str() == id.as_str() {
                    u.global::<RaceLayout>().set_sel_id(sstr(""));
                }
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_node_drag_start(move |id: SharedString| {
            let mut st = c.lock();
            if let Some(m) = st.nodes.iter().find(|m| m.id == id.as_str()) {
                st.drag_origin = Some((m.id.clone(), m.x, m.y, m.w, m.h));
            }
        });
        // During a drag we update state + re-render the preview image live, but do
        // NOT rebuild the node-overlay model (that would recreate the elements and
        // kill the in-progress gesture). The overlay box follows via a local offset.
        let c = ctx.clone();
        rl.on_node_move(move |dx, dy| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if let Some((id, ox, oy, _, _)) = st.drag_origin.clone() {
                    if let Some(m) = st.nodes.iter_mut().find(|m| m.id == id) {
                        m.x = (ox + dx).clamp(0, 480 - m.w.max(1));
                        m.y = (oy + dy).clamp(0, 320 - m.h.max(1));
                    }
                    crate::ui_bridge::uidoc::push_preview(&u, &st);
                }
            }
        });
        let c = ctx.clone();
        rl.on_node_resize(move |dw, dh| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if let Some((id, _, _, ow, oh)) = st.drag_origin.clone() {
                    if let Some(m) = st.nodes.iter_mut().find(|m| m.id == id) {
                        m.w = (ow + dw).clamp(20, 480 - m.x);
                        m.h = (oh + dh).clamp(16, 320 - m.y);
                    }
                    crate::ui_bridge::uidoc::push_preview(&u, &st);
                }
            }
        });
        let c = ctx.clone();
        rl.on_node_drag_end(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.drag_origin = None;
                let rl = u.global::<RaceLayout>();
                rl.set_snap_vx(-1);
                rl.set_snap_hy(-1);
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        // Snap-to-guides while dragging: snap the raw position to the screen + other
        // nodes, publish the active guide lines, and return the snapped delta so the
        // overlay box follows the snap (no model rebuild mid-gesture).
        let c = ctx.clone();
        rl.on_node_snap(move |raw_dx, raw_dy| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if let Some((id, ox, oy, ow, oh)) = st.drag_origin.clone() {
                    let nx = (ox + raw_dx).clamp(0, CANVAS_W - ow.max(1));
                    let ny = (oy + raw_dy).clamp(0, CANVAS_H - oh.max(1));
                    let disp = st.edit_display;
                    let (sx, sy, gv, gh) = snap_pos(&st.nodes, &id, disp, nx, ny, ow, oh);
                    if let Some(m) = st.nodes.iter_mut().find(|m| m.id == id) {
                        m.x = sx;
                        m.y = sy;
                    }
                    let rl = u.global::<RaceLayout>();
                    rl.set_snap_vx(gv);
                    rl.set_snap_hy(gh);
                    crate::ui_bridge::uidoc::push_preview(&u, &st);
                    return crate::Pt { x: sx - ox, y: sy - oy };
                }
            }
            crate::Pt { x: raw_dx, y: raw_dy }
        });
        // --- alignment buttons (act on the selected node) ---
        let c = ctx.clone();
        rl.on_node_center(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                with_selected(&u, &mut st, |m| {
                    m.x = (CANVAS_W - m.w) / 2;
                    m.y = (CANVAS_H - m.h) / 2;
                });
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_node_center_h(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                with_selected(&u, &mut st, |m| m.x = (CANVAS_W - m.w) / 2);
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_node_center_v(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                with_selected(&u, &mut st, |m| m.y = (CANVAS_H - m.h) / 2);
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_node_align_h(move || align_node(&c, true));
        let c = ctx.clone();
        rl.on_node_align_v(move || align_node(&c, false));
        let c = ctx.clone();
        rl.on_set_node_geo(move |x, y, w, h| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                with_selected(&u, &mut st, |m| {
                    m.w = w.clamp(20, 480);
                    m.h = h.clamp(16, 320);
                    m.x = x.clamp(0, 480 - m.w);
                    m.y = y.clamp(0, 320 - m.h);
                });
                mark_dirty(&u, &st);
                refresh_race(&u, &st);
            }
        });
        let c = ctx.clone();
        rl.on_switch_display(move |d| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.edit_display = d.clamp(0, 1) as u8;
                u.global::<RaceLayout>().set_sel_id(sstr(""));
                refresh_race(&u, &st);
            }
        });
    }

    {
        let cl = ui.global::<CarLib>();
        let c = ctx.clone();
        cl.on_select_game(move |i| {
            let u = match c.ui.upgrade() {
                Some(u) => u,
                None => return,
            };
            {
                let mut st = c.lock();
                if i < 0 || i as usize >= st.sims.len() {
                    return;
                }
                st.game = i;
                st.klass = 0;
                st.sel_car = -1;
                let cl = u.global::<CarLib>();
                cl.set_game(i);
                cl.set_klass(0);
                cl.set_sel(-1);
                push_classes(&u, &mut st);
                rebuild_filtered(&mut st);
                push_car_results(&u, &st);
            }
            crate::net::cardata::prefetch_game_data(&c);
        });
        let c = ctx.clone();
        cl.on_set_query(move |q: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.query = q.to_string();
                rebuild_filtered(&mut st);
                push_car_results(&u, &st);
            }
        });
        let c = ctx.clone();
        cl.on_select_class(move |i| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.klass = i;
                u.global::<CarLib>().set_klass(i);
                rebuild_filtered(&mut st);
                push_car_results(&u, &st);
            }
        });
        let c = ctx.clone();
        cl.on_select_car(move |i| crate::net::cardata::select_car(&c, i));
        let c = ctx.clone();
        cl.on_set_active(move |i| crate::net::cardata::set_active_car(&c, i));
        let c = ctx.clone();
        cl.on_apply_redline(move |i| crate::net::cardata::set_active_car(&c, i));
        let c = ctx.clone();
        cl.on_refresh(move || crate::net::cardata::refresh_database(&c));
    }

    {
        let bt = ui.global::<Buttons>();
        bt.on_select_tile(|_, _| {});
        macro_rules! btn_setter {
            ($method:ident, $val:ty, |$b:ident, $v:ident| $body:block) => {{
                let c = ctx.clone();
                bt.$method(move |$v: $val| {
                    if let Some(u) = c.ui.upgrade() {
                        let mut st = c.lock();
                        if let Some((p, sel)) = cur_btn(&u, &st) {
                            let $b = &mut st.btn_pages[p][sel];
                            $body
                        }
                        push_buttons_model(&u, &st);
                        save_buttons(&st);
                    }
                });
            }};
        }
        btn_setter!(on_set_label, SharedString, |b, l| {
            b.label = l.to_string();
        });
        btn_setter!(on_set_kind, bool, |b, tog| {
            b.toggle = tog;
        });
        btn_setter!(on_set_action, SharedString, |b, a| {
            b.action = a.to_string();
        });
        btn_setter!(on_set_sync, bool, |b, v| {
            b.sync = v;
        });
        btn_setter!(on_set_field, SharedString, |b, f| {
            b.field = f.to_string();
            b.avail = !b.field.is_empty();
        });
        btn_setter!(on_set_color, Color, |b, col| {
            b.col = to_u32(col);
        });
        let c = ctx.clone();
        bt.on_select_page(move |i| {
            if let Some(u) = c.ui.upgrade() {
                let st = c.lock();
                if i < 0 || i as usize >= st.btn_pages.len() {
                    return;
                }
                let b = u.global::<Buttons>();
                b.set_page(i);
                b.set_sel(0);
                push_buttons_model(&u, &st);
            }
        });
        let c = ctx.clone();
        bt.on_add_page(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if st.btn_pages.len() >= 5 {
                    return;
                }
                let page: Vec<BtnData> = (0..6)
                    .map(|_| BtnData {
                        label: "Button".into(),
                        toggle: false,
                        on: false,
                        action: "HID".into(),
                        col: 0x00E5A0,
                        sync: false,
                        field: String::new(),
                        avail: false,
                    })
                    .collect();
                st.btn_pages.push(page);
                let b = u.global::<Buttons>();
                b.set_page(st.btn_pages.len() as i32 - 1);
                b.set_sel(0);
                push_buttons_model(&u, &st);
                save_buttons(&st);
            }
        });
        let c = ctx.clone();
        bt.on_delete_page(move |i| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if st.btn_pages.len() <= 1 || i < 0 || i as usize >= st.btn_pages.len() {
                    return;
                }
                st.btn_pages.remove(i as usize);
                let b = u.global::<Buttons>();
                if b.get_page() >= st.btn_pages.len() as i32 {
                    b.set_page(st.btn_pages.len() as i32 - 1);
                }
                b.set_sel(0);
                push_buttons_model(&u, &st);
                save_buttons(&st);
            }
        });
        let c = ctx.clone();
        bt.on_save(move || {
            let u = match c.ui.upgrade() {
                Some(u) => u,
                None => return,
            };
            let b = u.global::<Buttons>();
            if !c.dash().connected() {
                b.set_save_status(sstr("Not connected"));
                return;
            }
            let json = build_buttons_json(&c.lock());
            b.set_save_status(sstr(if c.dash().push_buttons(&json) {
                "Saved to device"
            } else {
                "Device rejected — update firmware"
            }));
        });
    }

    {
        let fw = ui.global::<Firmware>();
        let c = ctx.clone();
        fw.on_install(move || crate::firmware::ota::start_ota_from_bin_path(&c));
        let c = ctx.clone();
        fw.on_retry(move || crate::firmware::ota::start_ota_from_bin_path(&c));
        let c = ctx.clone();
        fw.on_flash_built(move || crate::firmware::ota::start_ota_from_bin_path(&c));
        let c = ctx.clone();
        fw.on_build(move || crate::firmware::build::start_firmware_build(&c));
        let c = ctx.clone();
        fw.on_cancel_build(move || crate::firmware::build::cancel_firmware_build(&c));
        let c = ctx.clone();
        fw.on_refresh_releases(move || crate::net::releases::fetch_firmware_releases(&c));
        let c = ctx.clone();
        fw.on_select_release(move |i| {
            if let Some(u) = c.ui.upgrade() {
                u.global::<Firmware>().set_sel_release(i);
                let st = c.lock();
                update_release_board_match(&u, &st);
            }
        });
        let c = ctx.clone();
        fw.on_flash_release(move || crate::net::releases::flash_selected_release(&c));
        let c = ctx.clone();
        fw.on_refresh_ports(move || {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                refresh_serial_ports(&u, &mut st);
            }
        });
        let c = ctx.clone();
        fw.on_serial_flash(move || {
            let u = match c.ui.upgrade() {
                Some(u) => u,
                None => return,
            };
            let f = u.global::<Firmware>();
            let i = f.get_serial_port();
            let full = f.get_full_image();
            let port = {
                let st = c.lock();
                if i >= 0 && (i as usize) < st.serial_ports.len() {
                    st.serial_ports[i as usize].device.clone()
                } else {
                    String::new()
                }
            };
            crate::firmware::build::start_serial_flash(&c, port, full);
        });
    }

    {
        let dc = ui.global::<DeviceCfg>();
        let c = ctx.clone();
        dc.on_set_pin(move |key: SharedString, opt_idx| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                let gpio = {
                    let p = st.board_pins();
                    if opt_idx < 0 || opt_idx as usize >= p.len() {
                        return;
                    }
                    p[opt_idx as usize].gpio
                };
                for i in 0..PIN_N {
                    if PINDEFS[i].0 == key.as_str() {
                        st.pin_gpio[i] = gpio;
                    }
                }
                push_pins(&u, &st);
            }
        });
        let c = ctx.clone();
        dc.on_set_board(move |b| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                if b < 0 || b as usize >= st.boards.len() {
                    return;
                }
                st.board = b;
                save_board(&st);
                push_pins(&u, &st);
                update_release_board_match(&u, &st);
            }
        });
        let c = ctx.clone();
        dc.on_set_race_screen(move |v| {
            c.lock().race_screen = if v == 1 { 1 } else { 0 };
        });
        let c = ctx.clone();
        dc.on_set_led_rev(move |v| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.led_rev = v.clamp(0, 64);
                push_pins(&u, &st);
            }
        });
        let c = ctx.clone();
        dc.on_set_led_tc(move |v| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.led_tc = v.clamp(0, 16);
                push_pins(&u, &st);
            }
        });
        let c = ctx.clone();
        dc.on_set_led_abs(move |v| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.led_abs = v.clamp(0, 16);
                push_pins(&u, &st);
            }
        });
        let c = ctx.clone();
        dc.on_set_led_rgbw(move |v| {
            c.lock().led_rgbw = if v { 1 } else { 0 };
        });
        let c = ctx.clone();
        dc.on_set_disp(move |rot, fh, fv, bgr, inv| {
            if let Some(u) = c.ui.upgrade() {
                let r = rot.clamp(0, 3);
                {
                    let mut st = c.lock();
                    st.disp_rot = r;
                    st.disp_flip_h = fh;
                    st.disp_flip_v = fv;
                    st.disp_bgr = bgr;
                    st.disp_inv = inv;
                }
                // Reflect immediately, then push to the device (orientation live;
                // colour-order/invert reboots the device to re-init the panel).
                let dc = u.global::<DeviceCfg>();
                dc.set_disp_rot(r);
                dc.set_disp_flip_h(fh);
                dc.set_disp_flip_v(fv);
                dc.set_disp_bgr(bgr);
                dc.set_disp_inv(inv);
                if c.dash().connected() {
                    c.dash().push_disp(r, fh, fv, bgr, inv);
                }
            }
        });
        let c = ctx.clone();
        dc.on_save_pins(move || {
            let u = match c.ui.upgrade() {
                Some(u) => u,
                None => return,
            };
            let dc = u.global::<DeviceCfg>();
            if !c.dash().connected() {
                dc.set_pins_status(sstr("Not connected"));
                return;
            }
            let json = build_pins_json(&c.lock());
            let (ok, _) = c.dash().command(&format!("@PINS{json}"));
            dc.set_pins_status(sstr(if ok {
                "Saved — device rebooting to apply"
            } else {
                "Device rejected — update firmware"
            }));
        });
    }

    {
        let sh = ui.global::<SimHub>();
        let c = ctx.clone();
        sh.on_set_query(move |q: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.sim_query = q.to_string();
                push_sim_fields(&u, &st);
            }
        });
        let c = ctx.clone();
        sh.on_toggle_field(move |id: SharedString, on| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                for r in st.sim.iter_mut() {
                    if r.id == id.as_str() {
                        r.enabled = on;
                    }
                }
                regen_simhub(&u, &st);
            }
        });
        let c = ctx.clone();
        sh.on_add_field(move |label: SharedString, expr: SharedString| {
            if label.is_empty() || expr.is_empty() {
                return;
            }
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                let id = format!("custom{}", st.sim_uid);
                st.sim_uid += 1;
                st.sim.push(crate::state::SimRow {
                    id,
                    label: label.to_string(),
                    expr: expr.to_string(),
                    enabled: true,
                    builtin: false,
                });
                push_sim_fields(&u, &st);
                regen_simhub(&u, &st);
            }
        });
        let c = ctx.clone();
        sh.on_remove_field(move |id: SharedString| {
            if let Some(u) = c.ui.upgrade() {
                let mut st = c.lock();
                st.sim.retain(|r| r.builtin || r.id != id.as_str());
                push_sim_fields(&u, &st);
                regen_simhub(&u, &st);
            }
        });
        let c = ctx.clone();
        sh.on_regenerate(move || {
            if let Some(u) = c.ui.upgrade() {
                let st = c.lock();
                regen_simhub(&u, &st);
            }
        });
        let c = ctx.clone();
        sh.on_copy_generated(move || {
            if let Some(u) = c.ui.upgrade() {
                let sh = u.global::<SimHub>();
                let ok = copy_to_clipboard(sh.get_generated().as_str());
                sh.set_copy_status(sstr(if ok {
                    "✓ Copied to clipboard"
                } else {
                    "Copy failed (no clipboard tool)"
                }));
            }
        });
        let c = ctx.clone();
        sh.on_copy_car_message(move || {
            if let Some(u) = c.ui.upgrade() {
                let sh = u.global::<SimHub>();
                let ok = copy_to_clipboard(sh.get_car_message().as_str());
                sh.set_copy_status(sstr(if ok {
                    "✓ Copied car message"
                } else {
                    "Copy failed (no clipboard tool)"
                }));
            }
        });
    }
    let _ = ctx;
}

fn cur_btn(u: &AppWindow, s: &State) -> Option<(usize, usize)> {
    let b = u.global::<Buttons>();
    let p = b.get_page();
    let sel = b.get_sel();
    if p < 0
        || p as usize >= s.btn_pages.len()
        || sel < 0
        || sel as usize >= s.btn_pages[p as usize].len()
    {
        return None;
    }
    Some((p as usize, sel as usize))
}
