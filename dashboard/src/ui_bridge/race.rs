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
    AppWindow, LayoutZone, ModuleDef, PresetCard, RaceLayout, ResolvedModule, ResolvedZone,
    RuleRow, ZoneModule,
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
        kind_idx: idx_of(&KIND_OPTIONS, &m.kind),
        field_idx: if fid > 0 { fid as i32 - 1 } else { -1 },
        fmt_idx: if m.fmt_type.is_empty() {
            -1
        } else {
            idx_of(&FMT_NAMES, &m.fmt_type)
        },
        base_idx: idx_of(&PALETTE_TOKENS, &m.base),
    }
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

pub fn selected_module_idx(ui: &AppWindow, s: &State) -> Option<(usize, usize)> {
    let rl = ui.global::<RaceLayout>();
    let zk = rl.get_sel_zone().to_string();
    let id = rl.get_sel_id().to_string();
    let zi = s.zones.iter().position(|z| z.key == zk)?;
    let mi = s.zones[zi].modules.iter().position(|m| m.id == id)?;
    Some((zi, mi))
}

pub fn push_edit_module(ui: &AppWindow, s: &State) {
    let rl = ui.global::<RaceLayout>();
    let m = match selected_module_idx(ui, s) {
        Some((zi, mi)) => &s.zones[zi].modules[mi],
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

pub fn push_editor_options(ui: &AppWindow, _s: &State) {
    let rl = ui.global::<RaceLayout>();
    let fields: Vec<SharedString> = FIELDS.iter().map(|f| sstr(f.name)).collect();
    let kinds: Vec<SharedString> = KIND_OPTIONS.iter().map(|k| sstr(k)).collect();
    let palette: Vec<SharedString> = PALETTE_TOKENS.iter().map(|p| sstr(p)).collect();
    let fmts: Vec<SharedString> = FMT_NAMES.iter().map(|f| sstr(f)).collect();
    rl.set_field_options(model(fields));
    rl.set_kind_options(model(kinds));
    rl.set_palette_options(model(palette));
    rl.set_fmt_options(model(fmts));
}
