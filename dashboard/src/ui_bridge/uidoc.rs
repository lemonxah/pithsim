//! Bridge from the authoring model (zones + ModSpecs) to a pith-ui `UiDoc`, plus
//! the live desktop preview. The preview renders through the EXACT same pith-ui
//! engine the device runs (same fonts, same dirty-rect draw), so the GUI mirror
//! is pixel-identical to the ST7796 panel — and the `UiDoc` we render here is the
//! same one pushed to the device via `@UI`.

use slint::ComponentHandle;

use crate::state::{ModSpec, State};
use crate::telemetry::field_id_from_str;
use crate::{AppWindow, RaceLayout};

use crate::state::ElemSpec;
use pith_core::format::{Fmt, Pal, RuleOp};
use pith_core::registry::telemetry_from_fields;
use pith_core::simhub::Telemetry;
use pith_ui::{Align, El, Kind, Node, Rect, Rule, Screen, Slot, UiDoc, VAlign};

// The race panels are 480×320 (320×480 physical, rotated 90°).
pub const SCREEN_W: u32 = 480;
pub const SCREEN_H: u32 = 320;

fn rules_of(m: &ModSpec) -> Vec<Rule> {
    m.rules
        .iter()
        .map(|r| Rule {
            op: RuleOp::from_str(&r.op),
            threshold: r.v,
            color: Pal::from_str(&r.color),
        })
        .collect()
}

fn align_of(a: i32) -> Align {
    match a {
        0 => Align::Left,
        2 => Align::Right,
        _ => Align::Center,
    }
}
fn valign_of(v: i32) -> VAlign {
    match v {
        0 => VAlign::Top,
        2 => VAlign::Bottom,
        _ => VAlign::Center,
    }
}

fn elem_rules(e: &ElemSpec) -> Vec<Rule> {
    e.rules
        .iter()
        .map(|r| Rule {
            op: RuleOp::from_str(&r.op),
            threshold: r.v,
            color: Pal::from_str(&r.color),
        })
        .collect()
}

/// One composed element -> a pith-ui leaf Kind. `rev` is the physically installed
/// rev-LED count (so an `rpmStrip` element matches the real strip); `map` is the
/// selected track outline baked into any `map` element.
fn elem_kind(e: &ElemSpec, rev: u8, map: &[u16]) -> Kind {
    let field = field_id_from_str(&e.field) as u8;
    let base = Pal::from_str(&e.base);
    let fmt = if e.fmt_type.is_empty() {
        None
    } else {
        Some(Fmt::from_str(&e.fmt_type))
    };
    let size = e.size.clamp(0, 255) as u8;
    let align = align_of(e.align);
    let valign = valign_of(e.valign);
    match e.kind.as_str() {
        "label" => Kind::Label {
            text: e.text.clone(),
            color: base,
            size,
            align,
            valign,
        },
        "value" => Kind::Value {
            field,
            fmt,
            scale: e.scale,
            unit: e.unit.clone(),
            base,
            rules: elem_rules(e),
            size,
            align,
            valign,
        },
        "bar" => Kind::Bar {
            field,
            label: e.text.clone(),
            scale: e.scale,
            base,
            rules: elem_rules(e),
        },
        "gear" => Kind::GearSpeed { speed: false },
        "gearSpeed" => Kind::GearSpeed { speed: true },
        "rpmStrip" => Kind::RpmStrip { count: rev },
        "tyreGrid" => Kind::TyreGrid,
        "tcDual" => Kind::TcDual,
        "sectors" => Kind::Sectors,
        "lapPair" => Kind::LapPair,
        "position" => Kind::Position {
            label: e.text.clone(),
        },
        "flag" => Kind::Flag {
            field,
            base,
            rules: elem_rules(e),
        },
        "map" => Kind::Map { pts: map.to_vec() },
        "button" => Kind::Button {
            label: if e.text.is_empty() {
                "BTN".into()
            } else {
                e.text.clone()
            },
            color: base,
            action: e.action.clone(),
            toggle: e.toggle,
            hid: e.hid.clamp(0, 32) as u8,
            field,
            rules: elem_rules(e),
            on_color: Pal::Green,
        },
        _ => Kind::Value {
            field,
            fmt,
            scale: e.scale,
            unit: e.unit.clone(),
            base,
            rules: elem_rules(e),
            size,
            align,
            valign,
        },
    }
}

fn kind_of(m: &ModSpec, rev: u8, map: &[u16]) -> Kind {
    // A customised widget (non-empty `els`) -> a Row/Col of its elements. Otherwise
    // the shared builtin() defines the widget (decomposable ones come back as a
    // Widget(El) tree the editor can later customise).
    if !m.els.is_empty() {
        let children: Vec<Slot> = m
            .els
            .iter()
            .map(|e| Slot {
                flex: e.flex.clamp(1, 65535) as u16,
                el: El::Leaf(elem_kind(e, rev, map)),
            })
            .collect();
        let gap = m.gap.clamp(0, 255) as u8;
        let root = if m.dir == 1 {
            El::Row {
                gap,
                pad: 2,
                children,
            }
        } else {
            El::Col {
                gap,
                pad: 2,
                children,
            }
        };
        return Kind::Widget(alloc_box(root));
    }
    let field = field_id_from_str(&m.field) as u8;
    let base = Pal::from_str(&m.base);
    // Buttons carry toggle/hid that builtin() can't express, so build the Kind here.
    if m.kind == "button" {
        return Kind::Button {
            label: if m.label.is_empty() {
                "BTN".into()
            } else {
                m.label.clone()
            },
            color: base, // OFF-state colour
            action: String::new(),
            toggle: m.toggle,
            hid: m.hid.clamp(0, 32) as u8,
            field,
            rules: rules_of(m),
            on_color: Pal::from_str(if m.on_base.is_empty() {
                "green"
            } else {
                &m.on_base
            }),
        };
    }
    // RPM strip matches the physically installed rev-LED count.
    if m.kind == "rpmStrip" {
        return Kind::RpmStrip { count: rev };
    }
    // Map bakes in the selected track's outline (pushed with the layout).
    if m.kind == "map" {
        return Kind::Map { pts: map.to_vec() };
    }
    // Relatives/standings: `toggle` = view mode (off relative, on standings),
    // `hid` = visible row count (data arrives separately on the @REL line).
    if m.kind == "relatives" {
        return Kind::Relatives {
            mode: m.toggle as u8,
            rows: m
                .hid
                .clamp(0, pith_ui::Relatives::default().cars.len() as i32) as u8,
        };
    }
    let fmt = if m.fmt_type.is_empty() {
        None
    } else {
        Some(Fmt::from_str(&m.fmt_type))
    };
    let size = m.size_pct.clamp(0, 255) as u8;
    pith_ui::builtin(
        &m.kind,
        field,
        &m.label,
        fmt,
        m.scale,
        &m.unit,
        base,
        rules_of(m),
        size,
    )
}

fn alloc_box(el: El) -> Box<El> {
    Box::new(el)
}

/// Build a pith-ui `Screen` from the freeform nodes assigned to `display`.
pub fn build_screen(s: &State, display: u8) -> Screen {
    let rev = s.led_rev.clamp(0, 48) as u8;
    // Bundled outline for the (manual or auto-detected) track; empty = placeholder.
    let map = crate::trackmap::outline_for(&s.map_track);
    let nodes: Vec<Node> = s
        .nodes
        .iter()
        .filter(|m| m.display == display && m.enabled)
        .map(|m| Node {
            rect: Rect {
                x: m.x,
                y: m.y,
                w: m.w.max(0) as u32,
                h: m.h.max(0) as u32,
            },
            kind: kind_of(m, rev, &map),
            page: m.page.clamp(0, 255) as u8,
        })
        .collect();
    let tabs = s.tabs.get(display as usize).cloned().unwrap_or_default();
    Screen {
        display,
        w: SCREEN_W,
        h: SCREEN_H,
        bg: Pal::Bg,
        nodes,
        tabs,
    }
}

/// Build the full UiDoc: always a display-0 screen, plus display 1 when it has
/// nodes (so the device renders both panels via pith-ui).
pub fn build_uidoc(s: &State) -> UiDoc {
    let mut screens = vec![build_screen(s, 0)];
    if s.nodes.iter().any(|m| m.display == 1) {
        screens.push(build_screen(s, 1));
    }
    UiDoc {
        version: 1,
        screens,
    }
}

/// Serialize the UiDoc to JSON for the `@UI` wire command (text-safe, matches the
/// firmware's `serde_json::from_str::<UiDoc>` decode).
pub fn build_uidoc_json(s: &State) -> String {
    serde_json::to_string(&build_uidoc(s)).unwrap_or_else(|_| "{}".to_string())
}

/// Rehydrate a pith-ui Telemetry from the dashboard's flat field array + gear.
fn current_telemetry(s: &State) -> Telemetry {
    let mut t = telemetry_from_fields(&s.telem);
    t.gear = s.gear_ch as u8;
    t
}

/// Render `screen` against live telemetry into a slint image, using the device's
/// own pith-ui renderer + fonts (pixel-identical mirror).
fn render_image(
    screen: &Screen,
    active_tab: u8,
    t: &Telemetry,
    rel: &pith_ui::Relatives,
) -> slint::Image {
    let mut fb = pith_ui::Framebuffer::new(screen.w, screen.h);
    if screen.tabs.is_empty() {
        pith_ui::render_screen(screen, t, 0, 0, &pith_ui::CarData::default(), rel, &mut fb);
    } else {
        pith_ui::render_tabbed(
            screen,
            active_tab,
            t,
            0,
            0,
            &pith_ui::CarData::default(),
            rel,
            &mut fb,
        );
    }
    let mut buf = slint::SharedPixelBuffer::<slint::Rgba8Pixel>::new(screen.w, screen.h);
    buf.make_mut_bytes().copy_from_slice(&fb.to_rgba8());
    slint::Image::from_rgba8(buf)
}

/// Render the display currently being edited with live telemetry, and push the
/// resulting image into the RaceLayout preview. Called on every layout edit and
/// telemetry tick so the mirror stays live + exact.
pub fn push_preview(ui: &AppWindow, s: &State) {
    let screen = build_screen(s, s.edit_display);
    let t = current_telemetry(s);
    let img = render_image(&screen, s.edit_tab.clamp(0, 255) as u8, &t, &s.relatives);
    ui.global::<RaceLayout>().set_preview_image(img);
}

#[cfg(test)]
mod tests {
    use crate::state::{ModSpec, State};

    // The firmware decodes @UI with serde_json::from_str::<UiDoc>; prove the
    // dashboard's serde_json::to_string output round-trips to the same type.
    #[test]
    fn uidoc_json_roundtrips_for_firmware() {
        let mut s = State::default();
        s.nodes = vec![
            ModSpec {
                id: "a".into(),
                kind: "gearSpeed".into(),
                x: 170,
                y: 120,
                w: 140,
                h: 80,
                display: 0,
                ..Default::default()
            },
            ModSpec {
                id: "b".into(),
                kind: "stat".into(),
                field: "fuel_dl".into(),
                label: "FUEL".into(),
                x: 10,
                y: 10,
                w: 100,
                h: 50,
                display: 1,
                ..Default::default()
            },
        ];
        let json = super::build_uidoc_json(&s);
        let doc: pith_ui::UiDoc = serde_json::from_str(&json).expect("firmware-side decode");
        assert_eq!(doc.screens.len(), 2); // display 0 + display 1
        assert_eq!(doc.screens[0].display, 0);
        assert_eq!(doc.screens[1].display, 1);
        assert_eq!(doc.screens[0].nodes.len(), 1);
    }
}
