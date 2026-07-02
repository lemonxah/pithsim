use slint::ComponentHandle;
use slint::SharedString;

use super::{model, pal, sstr};
use crate::catalog::{mod_name, CATALOG};
use crate::state::{ModSpec, State};
use crate::telemetry::{
    field_def, field_id_from_str, fmtc_format, idx_of, op_from_str, rule_match, Fmt, FIELDS,
    FIELD_COUNT, FIELD_FIELD_SIZE, FIELD_POSITION, FMT_NAMES, KIND_OPTIONS, OP_NAMES,
    PALETTE_TOKENS,
};
use crate::{
    AppWindow, LayoutZone, ModuleDef, NodeBox, PresetCard, RaceLayout, ResolvedModule,
    ResolvedZone, RuleRow, ZoneModule,
};

pub fn mod_display(m: &ModSpec) -> String {
    if !m.label.is_empty() {
        m.label.clone()
    } else {
        mod_name(&m.templ).to_string()
    }
}

pub fn rule_summary(m: &ModSpec) -> String {
    if m.rules.is_empty() {
        return "no rules".to_string();
    }
    let mut s = String::new();
    for r in &m.rules {
        if !s.is_empty() {
            s.push_str("  ");
        }
        s.push_str(&format!("{}{}->{}", r.op, r.v, r.color));
    }
    s
}

pub fn zone_summary(zones: &[crate::state::Zone]) -> String {
    let mut n = 0;
    let mut s = String::new();
    for z in zones {
        for m in &z.modules {
            if m.enabled {
                if n < 3 {
                    if !s.is_empty() {
                        s.push_str(" · ");
                    }
                    s.push_str(&mod_display(m));
                }
                n += 1;
            }
        }
    }
    if n > 3 {
        s.push_str(&format!(" · +{}", n - 3));
    }
    if s.is_empty() {
        "empty".to_string()
    } else {
        s
    }
}

pub fn to_zone_module(m: &ModSpec) -> ZoneModule {
    let fid = field_id_from_str(&m.field);
    ZoneModule {
        id: sstr(&m.id),
        r#type: sstr(&m.templ),
        name: sstr(&mod_display(m)),
        enabled: m.enabled,
        kind: sstr(&m.kind),
        field: sstr(&m.field),
        label: sstr(&m.label),
        unit: sstr(&m.unit),
        fmt: sstr(&m.fmt_type),
        scale: m.scale,
        base: sstr(&m.base),
        rule_summary: sstr(&rule_summary(m)),
        size: m.size_pct,
        x: m.x,
        y: m.y,
        w: m.w,
        h: m.h,
        toggle: m.toggle,
        hid: m.hid,
        page: m.page,
        kind_idx: idx_of(&KIND_OPTIONS, &m.kind),
        field_idx: if fid > 0 { fid as i32 - 1 } else { -1 },
        fmt_idx: if m.fmt_type.is_empty() {
            -1
        } else {
            idx_of(&FMT_NAMES, &m.fmt_type)
        },
        base_idx: idx_of(&PALETTE_TOKENS, &m.base),
        on_base_idx: idx_of(&PALETTE_TOKENS, &m.on_base),
        base_color: pal_color(&m.base),
        on_base_color: pal_color(&m.on_base),
    }
}

/// Resolve a palette token ("green") or "#rrggbb" to a Slint swatch colour.
pub fn pal_color(token: &str) -> slint::Color {
    let (r, g, b) = pith_core::format::Pal::from_str(token).rgb888();
    slint::Color::from_rgb_u8(r, g, b)
}

pub fn push_zones(ui: &AppWindow, s: &State) {
    let zs: Vec<LayoutZone> = s
        .zones
        .iter()
        .map(|z| {
            let ms: Vec<ZoneModule> = z.modules.iter().map(to_zone_module).collect();
            LayoutZone {
                key: sstr(&z.key),
                title: sstr(&z.title),
                modules: model(ms),
            }
        })
        .collect();
    ui.global::<RaceLayout>().set_zones(model(zs));
}

/// Push the freeform node overlays for the display being edited, plus the
/// display selector state. The pith-ui image carries the pixels; these boxes are
/// just the interactive selection/drag handles.
pub fn push_nodes(ui: &AppWindow, s: &State) {
    let rl = ui.global::<RaceLayout>();
    let sel = rl.get_sel_id().to_string();
    let tabs = s
        .tabs
        .get(s.edit_display as usize)
        .cloned()
        .unwrap_or_default();
    let tabbed = !tabs.is_empty();
    let boxes: Vec<NodeBox> = s
        .nodes
        .iter()
        // on a tabbed display, only show the active page's nodes (matches the device)
        .filter(|m| m.display == s.edit_display && (!tabbed || m.page == s.edit_tab))
        .map(|m| NodeBox {
            id: sstr(&m.id),
            x: m.x,
            y: m.y,
            w: m.w,
            h: m.h,
            kind: sstr(&m.kind),
            label: sstr(&mod_display(m)),
            selected: m.id == sel,
        })
        .collect();
    rl.set_nodes(model(boxes));
    rl.set_edit_display(s.edit_display as i32);
    rl.set_display_count(2); // the DDU drives two ST7796 panels
    let names: Vec<SharedString> = tabs.iter().map(|t| sstr(t)).collect();
    rl.set_tab_names(model(names));
    rl.set_edit_tab(s.edit_tab);
}

pub fn push_presets(ui: &AppWindow, s: &State) {
    let ps: Vec<PresetCard> = s
        .presets
        .iter()
        .enumerate()
        .map(|(i, p)| PresetCard {
            name: sstr(&p.name),
            summary: sstr(&zone_summary(&p.zones)),
            active: i as i32 == s.active_preset,
            builtin: p.builtin,
        })
        .collect();
    ui.global::<RaceLayout>().set_presets(model(ps));
}

pub fn push_catalog(ui: &AppWindow, _s: &State) {
    let cat: Vec<ModuleDef> = CATALOG
        .iter()
        .map(|m| ModuleDef {
            r#type: sstr(m.0),
            name: sstr(m.1),
            desc: sstr(m.2),
        })
        .collect();
    ui.global::<RaceLayout>().set_catalog(model(cat));
}

fn spec_color(m: &ModSpec, iv: i32) -> slint::Color {
    let mut tok = if m.base.is_empty() { "white" } else { &m.base };
    for r in &m.rules {
        if rule_match(iv, op_from_str(&r.op), r.v) {
            tok = &r.color;
            break;
        }
    }
    pal(tok)
}

fn resolve_value(s: &State, m: &ModSpec) -> (String, i32) {
    let id = field_id_from_str(&m.field);
    let iv = if id > 0 && id < FIELD_COUNT {
        s.telem[id]
    } else {
        0
    };
    let def = field_def(id);
    let fmt = if !m.fmt_type.is_empty() {
        crate::telemetry::fmt_from_str(&m.fmt_type)
    } else {
        def.map(|d| d.fmt).unwrap_or(Fmt::Int)
    };
    let sc = if m.scale > 0 {
        m.scale
    } else {
        def.map(|d| d.scale).unwrap_or(1)
    };
    (fmtc_format(iv, fmt, sc, &m.unit), iv)
}

pub fn push_resolved(ui: &AppWindow, s: &State) {
    let rzs: Vec<ResolvedZone> = s
        .zones
        .iter()
        .map(|z| {
            let mut order = 0;
            let rms: Vec<ResolvedModule> = z
                .modules
                .iter()
                .map(|m| {
                    let mut rm = ResolvedModule {
                        zone: sstr(&z.key),
                        order,
                        kind: sstr(&m.kind),
                        label: sstr(&m.label),
                        value: sstr(""),
                        value_col: pal("white"),
                        pct: 0,
                        enabled: m.enabled,
                        size: m.size_pct,
                    };
                    order += 1;
                    if m.kind == "stat" || m.kind == "bar" {
                        let (val, iv) = resolve_value(s, m);
                        rm.value = sstr(&val);
                        rm.value_col = spec_color(m, iv);
                        if m.kind == "bar" {
                            let sc = if m.scale > 0 { m.scale } else { 1 };
                            let p = if sc > 1 { iv / sc } else { iv };
                            rm.pct = p.clamp(0, 100);
                        }
                    } else if m.kind == "position" {
                        let b =
                            format!("P{}/{}", s.telem[FIELD_POSITION], s.telem[FIELD_FIELD_SIZE]);
                        rm.value = sstr(&b);
                        rm.label = sstr(if m.label.is_empty() { "POS" } else { &m.label });
                        rm.value_col = spec_color(m, s.telem[FIELD_POSITION]);
                    }
                    rm
                })
                .collect();
            ResolvedZone {
                key: sstr(&z.key),
                modules: model(rms),
            }
        })
        .collect();
    ui.global::<RaceLayout>().set_render_zones(model(rzs));
}

pub fn push_edit_module(ui: &AppWindow, s: &State) {
    let rl = ui.global::<RaceLayout>();
    let id = rl.get_sel_id().to_string();
    let m = match s.nodes.iter().find(|m| m.id == id) {
        Some(m) => m,
        None => {
            rl.set_sel_id(sstr(""));
            return;
        }
    };
    rl.set_edit(to_zone_module(m));
    let rr: Vec<RuleRow> = m
        .rules
        .iter()
        .map(|r| RuleRow {
            op: sstr(&r.op),
            v: r.v,
            color: sstr(&r.color),
            op_idx: idx_of(&OP_NAMES, &r.op),
            color_idx: idx_of(&PALETTE_TOKENS, &r.color),
        })
        .collect();
    rl.set_edit_rules(model(rr));
}

/// Push the selected widget's editable element tree (element list + layout state)
/// to the editor. Empty when no widget is selected / it isn't customised yet.
pub fn push_elems(ui: &AppWindow, s: &State) {
    use crate::catalog::{elem_kind_name, ELEM_KINDS};
    use crate::telemetry::PALETTE_TOKENS;
    use crate::ElemRow;

    let rl = ui.global::<RaceLayout>();
    let id = rl.get_sel_id().to_string();
    let node = s.nodes.iter().find(|m| m.id == id);
    let custom = node.map(|m| !m.els.is_empty()).unwrap_or(false);
    rl.set_widget_custom(custom);
    if let Some(m) = node {
        rl.set_widget_dir(m.dir);
        rl.set_widget_gap(m.gap);
    }
    let sel = s.sel_elem;
    let rows: Vec<ElemRow> = node
        .map(|m| {
            m.els
                .iter()
                .enumerate()
                .map(|(i, e)| {
                    let summary = if !e.text.is_empty() {
                        e.text.clone()
                    } else if !e.field.is_empty() {
                        e.field.clone()
                    } else {
                        String::new()
                    };
                    ElemRow {
                        idx: i as i32,
                        kind: sstr(elem_kind_name(&e.kind)),
                        summary: sstr(&summary),
                        selected: i as i32 == sel,
                        flex: e.flex,
                        field: sstr(&e.field),
                        text: sstr(&e.text),
                        base: sstr(&e.base),
                        size: e.size,
                        align: e.align,
                        valign: e.valign,
                        action: sstr(&e.action),
                        toggle: e.toggle,
                        hid: e.hid,
                        kind_idx: ELEM_KINDS
                            .iter()
                            .position(|k| k.0 == e.kind)
                            .map(|i| i as i32)
                            .unwrap_or(0),
                        field_idx: {
                            let fid = field_id_from_str(&e.field);
                            if fid > 0 {
                                fid as i32 - 1
                            } else {
                                -1
                            }
                        },
                        base_idx: idx_of(&PALETTE_TOKENS, &e.base),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    rl.set_elems(model(rows));
    rl.set_sel_elem(sel);
    let kinds: Vec<SharedString> = ELEM_KINDS.iter().map(|k| sstr(k.1)).collect();
    rl.set_elem_kinds(model(kinds));
}

pub fn push_editor_options(ui: &AppWindow, s: &State) {
    let rl = ui.global::<RaceLayout>();
    let fields: Vec<SharedString> = FIELDS.iter().map(|f| sstr(f.name)).collect();
    let kinds: Vec<SharedString> = KIND_OPTIONS.iter().map(|k| sstr(k)).collect();
    let palette: Vec<SharedString> = PALETTE_TOKENS.iter().map(|p| sstr(p)).collect();
    let fmts: Vec<SharedString> = FMT_NAMES.iter().map(|f| sstr(f)).collect();
    let palette_colors: Vec<slint::Color> = PALETTE_TOKENS.iter().map(|p| pal_color(p)).collect();
    rl.set_field_options(model(fields));
    rl.set_kind_options(model(kinds));
    rl.set_palette_options(model(palette));
    rl.set_palette_colors(model(palette_colors));
    rl.set_fmt_options(model(fmts));
    push_theme_swatches(ui, s);
    let tracks: Vec<SharedString> = crate::trackmap::TRACK_NAMES
        .iter()
        .map(|t| sstr(t))
        .collect();
    rl.set_map_tracks(model(tracks));
    rl.set_map_track_idx(crate::trackmap::track_index(&s.map_track));
}

/// Publish the saved custom swatches (hex strings + their resolved colours).
pub fn push_theme_swatches(ui: &AppWindow, s: &State) {
    let rl = ui.global::<RaceLayout>();
    let hexes: Vec<SharedString> = s.custom_swatches.iter().map(|h| sstr(h)).collect();
    let cols: Vec<slint::Color> = s.custom_swatches.iter().map(|h| pal_color(h)).collect();
    rl.set_theme_swatch_hexes(model(hexes));
    rl.set_theme_swatches(model(cols));
}
