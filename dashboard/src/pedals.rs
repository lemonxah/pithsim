//! Active pedal integration: the device thread that owns the pedal's HID
//! handle, and the effects engine that replaces the SimHub plugin's
//! `DIYFFBPedal.cs` `DataUpdate` — reading the live telemetry merge (the
//! same one the DDU/race-screen use, from `pith-sim`'s UDP/shm decoders,
//! not SimHub) and streaming a `PedalAction` every tick.
//!
//! One pedal for now (see docs/pedals.md — this proves the pipeline end to
//! end; a full 3-pedal rig is the natural next step once validated on real
//! hardware).

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use slint::ComponentHandle;

use pith_device::{device_present, Pedals as PedalsDev, PID_PEDALS, PITH_VID};
use pith_pedals_core::effects::pct_byte;
use pith_pedals_core::protocol::{PedalAction, PedalConfig, PedalId};

use crate::ctx::Ctx;
use crate::telemetry::{FIELD_ABS_ACTIVE, FIELD_MAX_RPM, FIELD_RPM, FIELD_TC_SLIP};
use crate::ui_bridge::sstr;
use crate::{AppWindow, Pedals};

const PRESENCE_SCAN_INTERVAL: Duration = Duration::from_millis(1000);
const ACTION_INTERVAL: Duration = Duration::from_millis(20); // ~50 Hz, matches the reference's tick rate
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// A user-requested action for the pedal device thread (latest-wins, same
/// rationale as the handbrake's `HbOutbound`).
pub enum PedalsOutbound {
    PushConfig(PedalConfig),
    RefreshConfig,
}

// ---- Named profiles (the dashboard's answer to the SimHub plugin's
// per-game/per-car profile system) — a flat name -> PedalConfig store on
// disk. Save/load happen host-side (UI state <-> file); loading a profile
// also queues a PushConfig so it takes effect on the device immediately. ----

fn load_profiles() -> std::collections::BTreeMap<String, PedalConfig> {
    let body = std::fs::read_to_string(crate::paths::pedals_profiles_path()).unwrap_or_default();
    if body.is_empty() {
        return Default::default();
    }
    serde_json::from_str(&body).unwrap_or_default()
}

fn save_profiles(profiles: &std::collections::BTreeMap<String, PedalConfig>) -> bool {
    match serde_json::to_string_pretty(profiles) {
        Ok(json) => std::fs::write(crate::paths::pedals_profiles_path(), json).is_ok(),
        Err(_) => false,
    }
}

fn push_profiles_model(ui: &AppWindow, names: Vec<String>) {
    let model: Vec<slint::SharedString> = names.into_iter().map(|n| n.into()).collect();
    ui.global::<Pedals>()
        .set_profiles(std::rc::Rc::new(slint::VecModel::from(model)).into());
}

fn config_from_ui(pg: &Pedals) -> PedalConfig {
    PedalConfig {
        abs_frequency_hz: pg.get_abs_frequency_hz().clamp(0, 255) as u8,
        abs_amplitude_kg20: pg.get_abs_amplitude().clamp(0, 255) as u8,
        rpm_amplitude_kg: pg.get_rpm_amplitude_kg().clamp(0, 255) as u8,
        g_multiplier: pg.get_g_multiplier().clamp(0, 255) as u8,
        wheel_slip_amplitude: pg.get_wheel_slip_amplitude().clamp(0, 255) as u8,
        road_impact_multiplier: pg.get_road_impact_multiplier().clamp(0, 255) as u8,
        virtual_mass_pct: pg.get_virtual_mass_pct().clamp(0, 255) as u8,
        virtual_damping_pct: pg.get_virtual_damping_pct().clamp(0, 255) as u8,
        ..PedalConfig::defaults(PedalId::Brake)
    }
}

pub fn wire_pedals_callbacks(ui: &AppWindow, ctx: &Arc<Ctx>) {
    let p = ui.global::<Pedals>();

    p.on_push_config_requested({
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move || {
            let Some(u) = ui_weak.upgrade() else { return };
            // A first working slice of PedalConfig from the UI's sliders —
            // everything else keeps its current default until the curve
            // editor + remaining fields land.
            let cfg = config_from_ui(&u.global::<Pedals>());
            c.send_pedals(PedalsOutbound::PushConfig(cfg));
        }
    });

    p.on_refresh_config_requested({
        let c = ctx.clone();
        move || c.send_pedals(PedalsOutbound::RefreshConfig)
    });

    p.on_save_profile_requested({
        let ui_weak = ui.as_weak();
        move |name| {
            let Some(u) = ui_weak.upgrade() else { return };
            let name = name.trim().to_string();
            if name.is_empty() {
                return;
            }
            let cfg = config_from_ui(&u.global::<Pedals>());
            let mut profiles = load_profiles();
            profiles.insert(name.clone(), cfg);
            let ok = save_profiles(&profiles);
            let names: Vec<String> = profiles.keys().cloned().collect();
            push_profiles_model(&u, names);
            let msg = if ok {
                format!("Saved profile \"{name}\"")
            } else {
                "Failed to save profile".to_string()
            };
            u.global::<Pedals>().set_config_status(sstr(&msg));
        }
    });

    p.on_load_profile_requested({
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move |name| {
            let Some(u) = ui_weak.upgrade() else { return };
            let profiles = load_profiles();
            let Some(cfg) = profiles.get(name.as_str()) else {
                return;
            };
            apply_config_to_ui(&c, cfg);
            u.global::<Pedals>()
                .set_config_status(sstr(&format!("Loaded profile \"{name}\" — push to apply")));
        }
    });

    p.on_delete_profile_requested({
        let ui_weak = ui.as_weak();
        move |name| {
            let Some(u) = ui_weak.upgrade() else { return };
            let mut profiles = load_profiles();
            profiles.remove(name.as_str());
            save_profiles(&profiles);
            let names: Vec<String> = profiles.keys().cloned().collect();
            push_profiles_model(&u, names);
        }
    });

    let names: Vec<String> = load_profiles().keys().cloned().collect();
    push_profiles_model(ui, names);
    p.set_device_found(device_present(PITH_VID, PID_PEDALS));
}

fn apply_config_to_ui(ctx: &Arc<Ctx>, cfg: &PedalConfig) {
    let cfg = cfg.clone();
    ctx.ui_run(move |u| {
        let p = u.global::<Pedals>();
        p.set_abs_frequency_hz(cfg.abs_frequency_hz as i32);
        p.set_abs_amplitude(cfg.abs_amplitude_kg20 as i32);
        p.set_rpm_amplitude_kg(cfg.rpm_amplitude_kg as i32);
        p.set_g_multiplier(cfg.g_multiplier as i32);
        p.set_wheel_slip_amplitude(cfg.wheel_slip_amplitude as i32);
        p.set_road_impact_multiplier(cfg.road_impact_multiplier as i32);
        p.set_virtual_mass_pct(cfg.virtual_mass_pct as i32);
        p.set_virtual_damping_pct(cfg.virtual_damping_pct as i32);
    });
}

/// Build the live `PedalAction` from the current telemetry merge — this is
/// the direct equivalent of the SimHub plugin's per-frame
/// `DIYFFBPedal.DataUpdate`. ABS forwards the CURRENT level every tick (not
/// an edge pulse): the reference project's own plugin re-evaluates
/// `sendAbsSignal_local_b` fresh each frame and the firmware calls its
/// oscillator's `trigger()` on every truthy receipt, so holding the
/// boolean high is what keeps the effect alive — no host-side edge
/// detection needed.
///
/// G-force, wheel-slip-ratio, and road/suspension-impact telemetry aren't
/// decoded yet (tracked separately — extending the field registry with
/// verified per-game offsets, not guessed ones); those fields stay at 0
/// until that lands. `tc_slip` (already decoded on rF2/LMU) is used as a
/// partial stand-in for wheel-slip magnitude since it's the closest signal
/// currently available, not a perfect match for wheel-spin ratio.
fn build_action(telem: &[i32]) -> PedalAction {
    let get = |idx: usize| telem.get(idx).copied().unwrap_or(0);
    let rpm = get(FIELD_RPM).max(0) as f32;
    let max_rpm = get(FIELD_MAX_RPM).max(0) as f32;
    PedalAction {
        trigger_abs: get(FIELD_ABS_ACTIVE) != 0,
        rpm_pct: pct_byte(rpm, max_rpm),
        g_value: 0, // pending: verified per-game accG offsets
        wheel_slip: pct_byte(get(FIELD_TC_SLIP).max(0) as f32, 100.0),
        impact_value: 0, // pending: verified per-game suspension/impact offsets
        trigger_cv: [false; 4],
    }
}

/// Owns the `Pedals` HID handle: connects automatically, drains the command
/// outbox, and streams the effects-engine's `PedalAction` at ~50 Hz while
/// connected.
pub fn pedals_device_loop(ctx: Arc<Ctx>) {
    let mut dev = PedalsDev::default();
    let mut last_scan = std::time::Instant::now() - PRESENCE_SCAN_INTERVAL;
    let mut last_action = std::time::Instant::now() - ACTION_INTERVAL;
    let mut last_status = std::time::Instant::now() - STATUS_POLL_INTERVAL;

    while ctx.running.load(Ordering::SeqCst) {
        if !dev.is_open() {
            if last_scan.elapsed() >= PRESENCE_SCAN_INTERVAL {
                last_scan = std::time::Instant::now();
                let found = device_present(PITH_VID, PID_PEDALS);
                ctx.ui_run(move |u| {
                    u.global::<Pedals>().set_device_found(found);
                });
                if found {
                    try_connect(&ctx, &mut dev);
                }
            }
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        if let Some(cmd) = take_outbox(&ctx) {
            match cmd {
                PedalsOutbound::PushConfig(cfg) => {
                    let ok = dev.set_config(&cfg);
                    let msg = match ok {
                        Ok(()) => "Pushed to device".to_string(),
                        Err(e) => format!("Push failed: {e}"),
                    };
                    ctx.ui_run(move |u| u.global::<Pedals>().set_config_status(sstr(&msg)));
                }
                PedalsOutbound::RefreshConfig => {
                    if let Some(cfg) = dev.get_config() {
                        apply_config_to_ui(&ctx, &cfg);
                        ctx.ui_run(|u| {
                            u.global::<Pedals>()
                                .set_config_status(sstr("Refreshed from device"));
                        });
                    }
                }
            }
        }

        let now = std::time::Instant::now();
        if now.duration_since(last_action) >= ACTION_INTERVAL {
            last_action = now;
            let telem = { ctx.lock().telem };
            let action = build_action(&telem);
            if dev.send_action(&action).is_err() {
                dev.close();
                ctx.ui_run(|u| {
                    let p = u.global::<Pedals>();
                    p.set_connected(false);
                    p.set_conn_detail(sstr("Disconnected"));
                });
                continue;
            }
        }

        if now.duration_since(last_status) >= STATUS_POLL_INTERVAL {
            last_status = now;
            if let Some(st) = dev.status() {
                ctx.ui_run(move |u| {
                    let p = u.global::<Pedals>();
                    p.set_position_pct_x10(st.position_pct_x10 as i32);
                    p.set_force_n_x10(st.force_n_x10 as i32);
                });
            }
        }

        std::thread::sleep(Duration::from_millis(5));
    }
}

fn take_outbox(ctx: &Ctx) -> Option<PedalsOutbound> {
    let mut g = ctx.pedals_out.lock().unwrap();
    g.take()
}

fn try_connect(ctx: &Arc<Ctx>, dev: &mut PedalsDev) {
    if dev.connect() {
        if let Some(caps) = dev.capabilities() {
            let get = |k: &str| {
                caps.iter()
                    .find(|(kk, _)| kk == k)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default()
            };
            let board = get("board");
            let fw = get("fw");
            let serial = get("serial");
            ctx.ui_run(move |u| {
                let p = u.global::<Pedals>();
                p.set_board(sstr(&board));
                p.set_fw_version(sstr(&fw));
                p.set_serial(sstr(&serial));
                p.set_connected(true);
                p.set_conn_detail(sstr(""));
            });
            if let Some(cfg) = dev.get_config() {
                apply_config_to_ui(ctx, &cfg);
            }
            return;
        }
    }
    dev.close();
    ctx.ui_run(|u| {
        u.global::<Pedals>()
            .set_conn_detail(sstr("Connect failed — will retry"));
    });
}
