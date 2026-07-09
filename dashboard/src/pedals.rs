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
use pith_pedals_core::curve::ForceCurve;
use pith_pedals_core::effects::pct_byte;
use pith_pedals_core::protocol::{CustomVibration, PedalAction, PedalConfig, PedalId};

use crate::ctx::Ctx;
use crate::firmware::semver_cmp;
use crate::net::http::http_download_file;
use crate::paths::cache_dir;
use crate::state::State;
use crate::telemetry::{
    FIELD_ABS_ACTIVE, FIELD_G_LONG_X100, FIELD_MAX_RPM, FIELD_RPM, FIELD_SUSP_IMPACT,
    FIELD_TC_SLIP, FIELD_WHEEL_SLIP,
};
use crate::ui_bridge::sstr;
use crate::{AppWindow, CurvePt, PedalProfile, Pedals};

/// kg ↔ N: the config stores force in Newtons (×10); the UI works in kg like
/// the reference plugin.
const G: f32 = 9.81;
/// How many polyline samples the spline preview uses.
const SPLINE_SAMPLES: usize = 48;
/// The force-curve plot's fixed aspect ratio (width:height) — must match the
/// `viewbox-width` and `height: self.width / N` in `force_curve.slint`.
const PLOT_ASPECT: f32 = 2.5;

const PRESENCE_SCAN_INTERVAL: Duration = Duration::from_millis(1000);
const ACTION_INTERVAL: Duration = Duration::from_millis(20); // ~50 Hz, matches the reference's tick rate
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// A user-requested action for the pedal device thread (latest-wins, same
/// rationale as the handbrake's `HbOutbound`).
pub enum PedalsOutbound {
    PushConfig(PedalConfig),
    RefreshConfig,
    ProvisionWifi { ssid: String, password: String },
    /// Stream this downloaded app image to the connected pedal over @OTA. Runs
    /// on the device thread because it needs exclusive use of the HID handle.
    OtaFile(std::path::PathBuf),
}

// ---- Per-role config slots (multi-pedal) ----
// A rig has up to three pedals; the screen's Clutch/Brake/Throttle selector
// rebinds ONE shared editor to the selected role (the SimHub plugin's
// pattern). Each role keeps a full PedalConfig here so fields the editor
// doesn't surface (geometry, filters) survive role switches, and a config
// pulled from a device lands in the slot matching its `pedal_type`.

static PEDAL_SLOTS: std::sync::OnceLock<std::sync::Mutex<[PedalConfig; 3]>> =
    std::sync::OnceLock::new();

fn slots() -> &'static std::sync::Mutex<[PedalConfig; 3]> {
    PEDAL_SLOTS.get_or_init(|| {
        std::sync::Mutex::new([
            PedalConfig::defaults(PedalId::Clutch),
            PedalConfig::defaults(PedalId::Brake),
            PedalConfig::defaults(PedalId::Throttle),
        ])
    })
}

fn role_of(index: usize) -> PedalId {
    match index {
        0 => PedalId::Clutch,
        2 => PedalId::Throttle,
        _ => PedalId::Brake,
    }
}

fn index_of(id: PedalId) -> usize {
    match id {
        PedalId::Clutch => 0,
        PedalId::Brake => 1,
        PedalId::Throttle => 2,
    }
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

/// Rebuild the profile-manager list model (name + its game/car binding) from
/// the on-disk profiles + bindings, so the UI list always reflects the store.
fn push_profiles(ui: &AppWindow) {
    let profiles = load_profiles();
    let bindings = load_bindings();
    let rows: Vec<PedalProfile> = profiles
        .keys()
        .map(|name| PedalProfile {
            name: name.clone().into(),
            binding: bindings.get(name).cloned().unwrap_or_default().into(),
        })
        .collect();
    ui.global::<Pedals>()
        .set_profile_list(std::rc::Rc::new(slint::VecModel::from(rows)).into());
}

/// Refresh the pedal's "update available" state from the fetched pedals-v*
/// releases (newest tag carrying a pith-pedals-*.bin) vs the version reported
/// by the connected pedal's caps. Mirrors `hb::recompute_hb_update`.
pub fn recompute_pedals_update(ui: &AppWindow, s: &State) {
    let p = ui.global::<Pedals>();
    match s.pedals_releases.first().map(|r| r.tag.clone()) {
        Some(tag) => {
            p.set_fw_latest(sstr(&if tag.starts_with('v') {
                tag.clone()
            } else {
                format!("v{tag}")
            }));
            p.set_update_available(!s.pedals_fw.is_empty() && semver_cmp(&tag, &s.pedals_fw) > 0);
        }
        None => {
            p.set_fw_latest(sstr(""));
            p.set_update_available(false);
        }
    }
}

/// Newest pedals release's app image (`pith-pedals-<board>.bin`, streamed over
/// @OTA), skipping the `-full` merged image (that's the ROM-bootloader path).
fn latest_pedals_app(s: &State) -> Option<(String, String)> {
    let rel = s.pedals_releases.first()?;
    let url = rel
        .pedals_bin
        .iter()
        .find(|(b, _)| !b.ends_with("-full"))
        .map(|(_, u)| u.clone())?;
    Some((rel.tag.clone(), url))
}

/// In-place pedal update over USB HID: download the newest app image, then
/// hand it to the device thread to stream over @OTA (mirrors the handbrake).
fn install_pedals_update(ctx: &Arc<Ctx>) {
    let Some(ui) = ctx.ui.upgrade() else { return };
    let p = ui.global::<Pedals>();
    if p.get_flashing() {
        return;
    }
    if !p.get_connected() {
        p.set_flash_status(sstr("Pedal not connected over USB"));
        return;
    }
    let (tag, url) = {
        let s = ctx.lock();
        match latest_pedals_app(&s) {
            Some(x) => x,
            None => {
                p.set_flash_status(sstr("No published pedal firmware found"));
                return;
            }
        }
    };
    p.set_flashing(true);
    p.set_flash_progress(0.0);
    p.set_flash_status(sstr(&format!("Downloading {tag}…")));

    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let out = cache_dir().join(format!("pith-pedals-{tag}.bin"));
        let pc = ctx.clone();
        let ok = http_download_file(&url, &out, move |frac| {
            // download is the first quarter of the bar; the @OTA stream is the rest
            pc.ui_run(move |u| u.global::<Pedals>().set_flash_progress(frac as f32 * 0.25));
        })
        .await;
        if !ok || !out.exists() {
            ctx.ui_run(|u| {
                let p = u.global::<Pedals>();
                p.set_flashing(false);
                p.set_flash_status(sstr("Download failed"));
            });
            return;
        }
        ctx.ui_run(|u| u.global::<Pedals>().set_flash_status(sstr("Updating over USB…")));
        ctx.send_pedals(PedalsOutbound::OtaFile(out));
    });
}

/// Mirror the running game's sim-id + car model into the pedals globals so the
/// profile manager can offer one-click "bind to what's running now" chips.
fn push_detected(ctx: &Arc<Ctx>) {
    let game = detected_game_id(ctx);
    let car = ctx.lock().detected_model.clone();
    ctx.ui_run(move |u| {
        let p = u.global::<Pedals>();
        p.set_detected_game(sstr(&game));
        p.set_detected_car(sstr(&car));
    });
}

// ---- Per-game/per-car auto profile switching (SimHub's
// ApplyProfileAutoForGame/ApplyProfileAutoForCar). Each profile can carry a
// binding string of comma-separated keys; a key matches the running game's
// sim-id or a substring of the current car model. Car matches win over game
// matches (more specific), exactly like the reference. ----

fn load_bindings() -> std::collections::BTreeMap<String, String> {
    let body =
        std::fs::read_to_string(crate::paths::pedals_profile_bindings_path()).unwrap_or_default();
    if body.is_empty() {
        return Default::default();
    }
    serde_json::from_str(&body).unwrap_or_default()
}

fn save_bindings(b: &std::collections::BTreeMap<String, String>) -> bool {
    match serde_json::to_string_pretty(b) {
        Ok(json) => std::fs::write(crate::paths::pedals_profile_bindings_path(), json).is_ok(),
        Err(_) => false,
    }
}

/// A binding string's individual keys (comma-separated, trimmed, non-empty).
fn split_keys(binding: &str) -> Vec<String> {
    binding
        .split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect()
}

/// Mirror profile `name`'s binding into the binding editor: the active-profile
/// highlight, the raw preview string, the removable key chips, and the game
/// chip rows (every known sim, flagged when linked). Empty `name` clears it.
fn push_binding_ui(ctx: &Arc<Ctx>, u: &AppWindow, name: &str) {
    let pg = u.global::<Pedals>();
    pg.set_active_profile(sstr(name));
    let keys = if name.is_empty() {
        Vec::new()
    } else {
        split_keys(&load_bindings().get(name).cloned().unwrap_or_default())
    };
    pg.set_profile_bind(sstr(&keys.join(", ")));
    let chips: Vec<slint::SharedString> = keys.iter().map(|k| k.as_str().into()).collect();
    pg.set_bind_keys(std::rc::Rc::new(slint::VecModel::from(chips)).into());

    let sims = ctx.lock().sims.clone();
    let opts: Vec<crate::GameOpt> = sims
        .into_iter()
        .map(|(gname, id)| crate::GameOpt {
            on: keys.iter().any(|k| k.eq_ignore_ascii_case(&id)),
            name: gname.into(),
            id: id.into(),
        })
        .collect();
    // Two chip rows so ten sims never overflow the card's width.
    let half = opts.len().div_ceil(2).max(1);
    let rows: Vec<slint::ModelRc<crate::GameOpt>> = opts
        .chunks(half)
        .map(|c| slint::ModelRc::from(std::rc::Rc::new(slint::VecModel::from(c.to_vec()))))
        .collect();
    pg.set_bind_games(std::rc::Rc::new(slint::VecModel::from(rows)).into());
}

/// Apply `f` to the active profile's binding keys, persist, and refresh both
/// the binding editor and the profile list's row previews.
fn mutate_binding(ctx: &Arc<Ctx>, u: &AppWindow, f: impl FnOnce(&mut Vec<String>)) {
    let name = u.global::<Pedals>().get_active_profile().to_string();
    if name.is_empty() {
        return;
    }
    let mut bindings = load_bindings();
    let mut keys = split_keys(bindings.get(&name).map(String::as_str).unwrap_or(""));
    f(&mut keys);
    if keys.is_empty() {
        bindings.remove(&name);
    } else {
        bindings.insert(name.clone(), keys.join(", "));
    }
    save_bindings(&bindings);
    push_profiles(u);
    push_binding_ui(ctx, u, &name);
}

/// The profile whose binding best matches the current car/game, if any. Car
/// matches take priority over game matches.
fn resolve_auto_profile(
    car: &str,
    game: &str,
    bindings: &std::collections::BTreeMap<String, String>,
) -> Option<String> {
    let car_l = car.trim().to_lowercase();
    let game_l = game.trim().to_lowercase();
    let mut game_match: Option<String> = None;
    let mut car_match: Option<String> = None;
    for (name, bind) in bindings {
        for key in bind
            .split(',')
            .map(|k| k.trim().to_lowercase())
            .filter(|k| !k.is_empty())
        {
            if !car_l.is_empty() && (car_l.contains(&key) || key.contains(&car_l)) {
                car_match.get_or_insert_with(|| name.clone());
            } else if !game_l.is_empty() && key == game_l {
                game_match.get_or_insert_with(|| name.clone());
            }
        }
    }
    car_match.or(game_match)
}

/// The running game's sim-id (from process/decoder detection), or "".
fn detected_game_id(ctx: &Ctx) -> String {
    let s = ctx.lock();
    if s.detected_game_idx >= 0 {
        s.sims
            .get(s.detected_game_idx as usize)
            .map(|g| g.1.clone())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

fn config_from_ui(pg: &Pedals) -> PedalConfig {
    // Read the curve control points (normalized 0..1) back into the config's
    // ×10-percent arrays.
    let points = curve_points_from_ui(pg);
    let curve_travel_pct_x10: Vec<u16> = points
        .iter()
        .map(|(x, _)| (x * 1000.0).round().clamp(0.0, 1000.0) as u16)
        .collect();
    let curve_force_pct_x10: Vec<u16> = points
        .iter()
        .map(|(_, y)| (y * 1000.0).round().clamp(0.0, 1000.0) as u16)
        .collect();

    // Base = the selected role's stored slot, so fields the editor doesn't
    // surface (geometry, filter tuning, …) carry through unchanged, and the
    // pushed config always carries the selected pedal_type.
    let sel = (pg.get_selected_pedal() as usize).min(2);
    let base = slots().lock().unwrap()[sel].clone();

    let u8f = |v: i32| v.clamp(0, 255) as u8;
    let cfg = PedalConfig {
        pedal_start_pct: pg.get_pedal_start_pct().clamp(0, 100) as u8,
        pedal_end_pct: pg.get_pedal_end_pct().clamp(0, 100) as u8,
        max_force_n_x10: (pg.get_max_force_kg() as f32 * G * 10.0)
            .round()
            .clamp(0.0, 65535.0) as u16,
        preload_force_n_x10: (pg.get_preload_kg() as f32 * G * 10.0)
            .round()
            .clamp(0.0, 65535.0) as u16,
        curve_travel_pct_x10,
        curve_force_pct_x10,
        // ---- effects ----
        abs_frequency_hz: u8f(pg.get_abs_frequency_hz()),
        abs_amplitude_kg20: u8f(pg.get_abs_amplitude()),
        abs_sawtooth: pg.get_abs_waveform() == 1,
        abs_affects_travel: pg.get_abs_apply_by() == 1,
        simulate_abs: pg.get_simulate_abs(),
        simulate_abs_value: u8f(pg.get_simulate_abs_value()),
        rpm_amplitude_kg: u8f(pg.get_rpm_amplitude_kg()),
        rpm_min_freq_hz: u8f(pg.get_rpm_min_freq_hz()),
        rpm_max_freq_hz: u8f(pg.get_rpm_max_freq_hz()),
        bite_point_enabled: pg.get_bite_point_enabled(),
        bite_point_trigger_pct: pg.get_bite_point_trigger_pct().clamp(0, 100) as u8,
        bite_point_amplitude: u8f(pg.get_bite_point_amplitude()),
        bite_point_freq_hz: u8f(pg.get_bite_point_freq_hz()),
        g_multiplier: u8f(pg.get_g_multiplier()),
        g_window: u8f(pg.get_g_window()),
        wheel_slip_amplitude: u8f(pg.get_wheel_slip_amplitude()),
        wheel_slip_freq_hz: u8f(pg.get_wheel_slip_freq_hz()),
        road_impact_multiplier: u8f(pg.get_road_impact_multiplier()),
        road_impact_window: u8f(pg.get_road_impact_window()),
        custom_vibration: [
            CustomVibration { amplitude: u8f(pg.get_custom_amp_1()), frequency_hz: u8f(pg.get_custom_freq_1()) },
            CustomVibration { amplitude: u8f(pg.get_custom_amp_2()), frequency_hz: u8f(pg.get_custom_freq_2()) },
            CustomVibration { amplitude: u8f(pg.get_custom_amp_3()), frequency_hz: u8f(pg.get_custom_freq_3()) },
            CustomVibration { amplitude: u8f(pg.get_custom_amp_4()), frequency_hz: u8f(pg.get_custom_freq_4()) },
        ],
        // ---- dynamics ----
        virtual_mass_pct: u8f(pg.get_virtual_mass_pct()),
        virtual_damping_pct: u8f(pg.get_virtual_damping_pct()),
        coulomb_friction_0p1n: u8f(pg.get_coulomb_friction_0p1n()),
        damping_progression: u8f(pg.get_damping_progression_pct()),
        endstop_travel_range_mm: u8f(pg.get_endstop_travel_mm()),
        endstop_stiffness_kg_per_mm: u8f(pg.get_endstop_stiffness_kg_mm()),
        // ---- geometry ----
        length_a_mm: pg.get_length_a_mm().clamp(0, 1000) as i16,
        length_b_mm: pg.get_length_b_mm().clamp(0, 1000) as i16,
        length_d_mm: pg.get_length_d_mm().clamp(0, 1000) as i16,
        length_c_horizontal_mm: pg.get_length_c_horizontal_mm().clamp(0, 1000) as i16,
        length_c_vertical_mm: pg.get_length_c_vertical_mm().clamp(0, 1000) as i16,
        length_travel_mm: pg.get_length_travel_mm().clamp(0, 1000) as i16,
        // ---- axis output ----
        max_game_output_pct: pg.get_max_game_output_pct().clamp(0, 100) as u8,
        travel_as_joystick_output: pg.get_axis_source() == 1,
        // ---- calibration / servo / general ----
        loadcell_rating_kg: pg.get_loadcell_rating_kg().clamp(1, 255) as u8,
        invert_loadcell: pg.get_invert_loadcell(),
        invert_motor_direction: pg.get_invert_motor_direction(),
        spindle_pitch_mm_per_rev: pg.get_spindle_pitch_mm().clamp(1, 255) as u8,
        servo_idle_timeout_s: u8f(pg.get_servo_idle_timeout_s()),
        step_loss_recovery: pg.get_step_loss_recovery(),
        crash_detection: pg.get_crash_detection(),
        endstop_detection_threshold: u8f(pg.get_endstop_detection_threshold()),
        min_force_for_effects_n: u8f(pg.get_min_force_for_effects_n()),
        debug_flags: u8f(pg.get_debug_flags()),
        // ---- filter ----
        kf_force_model_order: pg.get_kf_force_model_order().clamp(0, 3) as u8,
        kf_force_model_noise: u8f(pg.get_kf_force_model_noise()),
        kf_joystick_enabled: pg.get_kf_joystick_enabled(),
        kf_joystick_model_noise: u8f(pg.get_kf_joystick_model_noise()),
        pedal_type: role_of(sel),
        ..base
    };
    // Keep the slot current so switching roles and back doesn't lose edits.
    slots().lock().unwrap()[sel] = cfg.clone();
    cfg
}

/// Read the curve control-point model back as normalized (x, y) pairs.
fn curve_points_from_ui(pg: &Pedals) -> Vec<(f32, f32)> {
    use slint::Model;
    pg.get_curve_points().iter().map(|p| (p.x, p.y)).collect()
}

/// Build the cubic-spline preview as an SVG path string in the plot's 2.5×1
/// coordinate space (x scaled by `PLOT_ASPECT`, y flipped so force=1 is at
/// the top). Uses the same `pith_pedals_core::curve` math the firmware runs,
/// so the preview is exact.
fn spline_commands(points: &[(f32, f32)]) -> String {
    let pts100: Vec<(f32, f32)> = points.iter().map(|(x, y)| (x * 100.0, y * 100.0)).collect();
    let curve = ForceCurve::from_points(&pts100).unwrap_or_else(ForceCurve::linear_default);
    let mut s = String::with_capacity(SPLINE_SAMPLES * 20);
    for i in 0..=SPLINE_SAMPLES {
        let tx = i as f32 / SPLINE_SAMPLES as f32;
        let fy = (curve.eval(tx * 100.0) / 100.0).clamp(0.0, 1.0);
        let cmd = if i == 0 { 'M' } else { 'L' };
        s.push_str(&format!("{cmd} {:.4} {:.4} ", tx * PLOT_ASPECT, 1.0 - fy));
    }
    s
}

/// Push a set of control points to the UI model and recompute the spline.
fn set_curve_ui(pg: &Pedals, points: &[(f32, f32)]) {
    let model: Vec<CurvePt> = points.iter().map(|&(x, y)| CurvePt { x, y }).collect();
    pg.set_curve_points(std::rc::Rc::new(slint::VecModel::from(model)).into());
    pg.set_spline_commands(sstr(&spline_commands(points)));
}

/// Index of the control point nearest normalized (x, y), if any.
fn nearest_index(points: &[(f32, f32)], x: f32, y: f32) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            let da = (a.0 - x).powi(2) + (a.1 - y).powi(2);
            let db = (b.0 - x).powi(2) + (b.1 - y).powi(2);
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
}

/// One of the reference's four curve presets, as normalized control points.
fn preset_points(name: &str) -> Vec<(f32, f32)> {
    match name {
        // 6-point presets matching the reference's Linear/S/Exp/Log shapes.
        "s-curve" => vec![
            (0.0, 0.0),
            (0.2, 0.07),
            (0.4, 0.28),
            (0.6, 0.70),
            (0.8, 0.93),
            (1.0, 1.0),
        ],
        "exponent" => vec![
            (0.0, 0.0),
            (0.25, 0.06),
            (0.5, 0.20),
            (0.7, 0.42),
            (0.85, 0.66),
            (1.0, 1.0),
        ],
        "logarithm" => vec![
            (0.0, 0.0),
            (0.15, 0.34),
            (0.3, 0.58),
            (0.5, 0.80),
            (0.75, 0.94),
            (1.0, 1.0),
        ],
        // "linear" and anything else
        _ => vec![(0.0, 0.0), (1.0, 1.0)],
    }
}

pub fn wire_pedals_callbacks(ui: &AppWindow, ctx: &Arc<Ctx>) {
    let p = ui.global::<Pedals>();

    p.on_push_config_requested({
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move || {
            let Some(u) = ui_weak.upgrade() else { return };
            let cfg = config_from_ui(&u.global::<Pedals>());
            c.send_pedals(PedalsOutbound::PushConfig(cfg));
        }
    });

    p.on_refresh_config_requested({
        let c = ctx.clone();
        move || c.send_pedals(PedalsOutbound::RefreshConfig)
    });

    // ---- force-curve editor ----

    // Drag: move control point `idx` to (x, y), keeping travel strictly
    // increasing (endpoints pinned to x=0 / x=1, interior points clamped
    // between their neighbours so the spline solver stays valid).
    p.on_curve_point_moved({
        let ui_weak = ui.as_weak();
        move |idx, x, y| {
            let Some(u) = ui_weak.upgrade() else { return };
            let pg = u.global::<Pedals>();
            let mut pts = curve_points_from_ui(&pg);
            let i = idx as usize;
            if i >= pts.len() {
                return;
            }
            let last = pts.len() - 1;
            let y = y.clamp(0.0, 1.0);
            let nx = if i == 0 {
                0.0
            } else if i == last {
                1.0
            } else {
                let lo = pts[i - 1].0 + 0.01;
                let hi = pts[i + 1].0 - 0.01;
                x.clamp(lo, hi)
            };
            pts[i] = (nx, y);
            set_curve_ui(&pg, &pts);
        }
    });

    // Right-click: remove the point under the cursor if close, else add one.
    p.on_curve_right_click({
        let ui_weak = ui.as_weak();
        move |x, y| {
            let Some(u) = ui_weak.upgrade() else { return };
            let pg = u.global::<Pedals>();
            let mut pts = curve_points_from_ui(&pg);
            let near = nearest_index(&pts, x, y);
            let dist = near
                .map(|n| ((pts[n].0 - x).powi(2) + (pts[n].1 - y).powi(2)).sqrt())
                .unwrap_or(1.0);
            if let Some(n) = near {
                // Remove if close AND interior AND we'd keep >= 2 points.
                if dist < 0.04 && n != 0 && n != pts.len() - 1 && pts.len() > 2 {
                    pts.remove(n);
                    set_curve_ui(&pg, &pts);
                    return;
                }
            }
            // Otherwise add a new interior point at x (max 11, the spline cap).
            if pts.len() < 11 && x > 0.0 && x < 1.0 {
                let insert_at = pts.iter().position(|p| p.0 > x).unwrap_or(pts.len());
                pts.insert(insert_at, (x, y.clamp(0.0, 1.0)));
                set_curve_ui(&pg, &pts);
            }
        }
    });

    // Nearest control-point index to (x, y) — used by the editor to pick
    // which point a drag grabs.
    p.on_curve_nearest({
        let ui_weak = ui.as_weak();
        move |x, y| {
            let Some(u) = ui_weak.upgrade() else {
                return -1;
            };
            let pts = curve_points_from_ui(&u.global::<Pedals>());
            nearest_index(&pts, x, y).map(|n| n as i32).unwrap_or(-1)
        }
    });

    p.on_curve_preset({
        let ui_weak = ui.as_weak();
        move |name| {
            let Some(u) = ui_weak.upgrade() else { return };
            set_curve_ui(&u.global::<Pedals>(), &preset_points(name.as_str()));
        }
    });

    // Max-force/preload/travel sliders don't change the curve *shape* but the
    // spline preview's y-scale is normalized, so no recompute is needed — the
    // values are read straight from the globals at push time.
    p.on_curve_range_changed(|| {});

    p.on_save_profile_requested({
        let c = ctx.clone();
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
            push_profiles(&u);
            // Select the just-saved profile so its game/car linking is ready.
            push_binding_ui(&c, &u, &name);
            let msg = if ok {
                format!("Saved profile \"{name}\" — link it to a game / car below")
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
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move |name| {
            let Some(u) = ui_weak.upgrade() else { return };
            let mut profiles = load_profiles();
            profiles.remove(name.as_str());
            save_profiles(&profiles);
            // Drop any binding for the deleted profile too.
            let mut bindings = load_bindings();
            if bindings.remove(name.as_str()).is_some() {
                save_bindings(&bindings);
            }
            push_profiles(&u);
            // Deleting the selected profile closes its binding editor.
            if u.global::<Pedals>().get_active_profile() == name {
                push_binding_ui(&c, &u, "");
            }
        }
    });

    // ---- auto profile switching ----

    p.on_set_auto_switch({
        let c = ctx.clone();
        move |on| {
            let mut s = c.lock();
            s.pedals_auto_switch = on;
            // Clear the dedup key so enabling it applies the matching profile
            // immediately (not only on the next game/car change).
            s.pedals_last_auto.clear();
            crate::persist::save_udp_cfg(&s);
        }
    });

    // ---- game/car linking (binding editor) ----

    p.on_select_profile({
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move |name| {
            let Some(u) = ui_weak.upgrade() else { return };
            push_binding_ui(&c, &u, name.trim());
        }
    });

    p.on_bind_toggle_game({
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move |id| {
            let Some(u) = ui_weak.upgrade() else { return };
            mutate_binding(&c, &u, |keys| {
                match keys.iter().position(|k| k.eq_ignore_ascii_case(&id)) {
                    Some(i) => {
                        keys.remove(i);
                    }
                    None => keys.push(id.to_string()),
                }
            });
        }
    });

    p.on_bind_add_car({
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move |car| {
            let Some(u) = ui_weak.upgrade() else { return };
            let car = car.trim().to_string();
            if car.is_empty() {
                return;
            }
            mutate_binding(&c, &u, |keys| {
                if !keys.iter().any(|k| k.eq_ignore_ascii_case(&car)) {
                    keys.push(car);
                }
            });
        }
    });

    p.on_bind_remove_key({
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move |key| {
            let Some(u) = ui_weak.upgrade() else { return };
            mutate_binding(&c, &u, |keys| keys.retain(|k| k != key.as_str()));
        }
    });

    // ---- multi-pedal selector ----
    // Save the editor into the outgoing role's slot, then load the incoming
    // role's slot — one shared editor, three configs (the SimHub pattern).
    p.on_select_pedal({
        let c = ctx.clone();
        let ui_weak = ui.as_weak();
        move |new_index| {
            let Some(u) = ui_weak.upgrade() else { return };
            let pg = u.global::<Pedals>();
            let new_index = (new_index as usize).min(2);
            let old_index = (pg.get_selected_pedal() as usize).min(2);
            if new_index == old_index {
                return;
            }
            // config_from_ui stores the editor into the old slot itself.
            let _ = config_from_ui(&pg);
            pg.set_selected_pedal(new_index as i32);
            let cfg = slots().lock().unwrap()[new_index].clone();
            apply_config_to_ui(&c, &cfg);
        }
    });

    // ---- firmware update ----
    p.on_install_update_requested({
        let c = ctx.clone();
        move || install_pedals_update(&c)
    });

    // (WiFi provisioning + input mode moved to the Wireless screen —
    // see crate::wireless.)

    // Reflect the persisted auto-switch flag into the UI.
    p.set_auto_switch(ctx.lock().pedals_auto_switch);

    push_profiles(ui);
    // Pre-select the first saved profile so the game/car linking editor is
    // visible without an extra click.
    if let Some(first) = load_profiles().keys().next().cloned() {
        push_binding_ui(ctx, ui, &first);
    }
    p.set_device_found(device_present(PITH_VID, PID_PEDALS));

    // Seed the curve editor from the default config so the plot has a shape
    // before any device/profile loads.
    let defaults = PedalConfig::defaults(PedalId::Brake);
    let n = defaults
        .curve_travel_pct_x10
        .len()
        .min(defaults.curve_force_pct_x10.len());
    let seed: Vec<(f32, f32)> = (0..n)
        .map(|i| {
            (
                defaults.curve_travel_pct_x10[i] as f32 / 1000.0,
                defaults.curve_force_pct_x10[i] as f32 / 1000.0,
            )
        })
        .collect();
    let seed = if seed.len() >= 2 {
        seed
    } else {
        vec![(0.0, 0.0), (1.0, 1.0)]
    };
    set_curve_ui(&p, &seed);
}

/// Adopt a config that came FROM a device (connect/refresh/auto-switch):
/// store it in its role's slot, switch the editor to that role, mark the
/// role live, and load it into the UI.
fn adopt_device_config(ctx: &Arc<Ctx>, cfg: &PedalConfig) {
    let idx = index_of(cfg.pedal_type);
    slots().lock().unwrap()[idx] = cfg.clone();
    ctx.ui_run(move |u| {
        let p = u.global::<Pedals>();
        p.set_selected_pedal(idx as i32);
        let mut on = vec![false; 3];
        on[idx] = true;
        p.set_pedal_connected(std::rc::Rc::new(slint::VecModel::from(on)).into());
    });
    apply_config_to_ui(ctx, cfg);
}

/// Clear the per-role "device live" markers (single-USB-pedal for now; a
/// wireless pedal's role isn't known from its beacon yet).
fn clear_pedal_connected(ctx: &Arc<Ctx>) {
    ctx.ui_run(|u| {
        let model: Vec<bool> = vec![false; 3];
        u.global::<Pedals>()
            .set_pedal_connected(std::rc::Rc::new(slint::VecModel::from(model)).into());
    });
}

fn apply_config_to_ui(ctx: &Arc<Ctx>, cfg: &PedalConfig) {
    let cfg = cfg.clone();
    ctx.ui_run(move |u| {
        let p = u.global::<Pedals>();
        p.set_pedal_start_pct(cfg.pedal_start_pct as i32);
        p.set_pedal_end_pct(cfg.pedal_end_pct as i32);
        p.set_max_force_kg((cfg.max_force_n_x10 as f32 / 10.0 / G).round() as i32);
        p.set_preload_kg((cfg.preload_force_n_x10 as f32 / 10.0 / G).round() as i32);
        p.set_abs_frequency_hz(cfg.abs_frequency_hz as i32);
        p.set_abs_amplitude(cfg.abs_amplitude_kg20 as i32);
        p.set_rpm_amplitude_kg(cfg.rpm_amplitude_kg as i32);
        p.set_g_multiplier(cfg.g_multiplier as i32);
        p.set_wheel_slip_amplitude(cfg.wheel_slip_amplitude as i32);
        p.set_road_impact_multiplier(cfg.road_impact_multiplier as i32);
        p.set_virtual_mass_pct(cfg.virtual_mass_pct as i32);
        p.set_virtual_damping_pct(cfg.virtual_damping_pct as i32);
        // ---- effects (full parameter set) ----
        p.set_abs_waveform(cfg.abs_sawtooth as i32);
        p.set_abs_apply_by(cfg.abs_affects_travel as i32);
        p.set_simulate_abs(cfg.simulate_abs);
        p.set_simulate_abs_value(cfg.simulate_abs_value as i32);
        p.set_rpm_min_freq_hz(cfg.rpm_min_freq_hz as i32);
        p.set_rpm_max_freq_hz(cfg.rpm_max_freq_hz as i32);
        p.set_bite_point_enabled(cfg.bite_point_enabled);
        p.set_bite_point_trigger_pct(cfg.bite_point_trigger_pct as i32);
        p.set_bite_point_amplitude(cfg.bite_point_amplitude as i32);
        p.set_bite_point_freq_hz(cfg.bite_point_freq_hz as i32);
        p.set_g_window(cfg.g_window as i32);
        p.set_wheel_slip_freq_hz(cfg.wheel_slip_freq_hz as i32);
        p.set_road_impact_window(cfg.road_impact_window as i32);
        p.set_custom_amp_1(cfg.custom_vibration[0].amplitude as i32);
        p.set_custom_freq_1(cfg.custom_vibration[0].frequency_hz as i32);
        p.set_custom_amp_2(cfg.custom_vibration[1].amplitude as i32);
        p.set_custom_freq_2(cfg.custom_vibration[1].frequency_hz as i32);
        p.set_custom_amp_3(cfg.custom_vibration[2].amplitude as i32);
        p.set_custom_freq_3(cfg.custom_vibration[2].frequency_hz as i32);
        p.set_custom_amp_4(cfg.custom_vibration[3].amplitude as i32);
        p.set_custom_freq_4(cfg.custom_vibration[3].frequency_hz as i32);
        // ---- dynamics ----
        p.set_coulomb_friction_0p1n(cfg.coulomb_friction_0p1n as i32);
        p.set_damping_progression_pct(cfg.damping_progression as i32);
        p.set_endstop_travel_mm(cfg.endstop_travel_range_mm as i32);
        p.set_endstop_stiffness_kg_mm(cfg.endstop_stiffness_kg_per_mm as i32);
        // ---- geometry ----
        p.set_length_a_mm(cfg.length_a_mm as i32);
        p.set_length_b_mm(cfg.length_b_mm as i32);
        p.set_length_d_mm(cfg.length_d_mm as i32);
        p.set_length_c_horizontal_mm(cfg.length_c_horizontal_mm as i32);
        p.set_length_c_vertical_mm(cfg.length_c_vertical_mm as i32);
        p.set_length_travel_mm(cfg.length_travel_mm as i32);
        // ---- axis output ----
        p.set_max_game_output_pct(cfg.max_game_output_pct as i32);
        p.set_axis_source(cfg.travel_as_joystick_output as i32);
        // ---- calibration / servo / general ----
        p.set_loadcell_rating_kg(cfg.loadcell_rating_kg as i32);
        p.set_invert_loadcell(cfg.invert_loadcell);
        p.set_invert_motor_direction(cfg.invert_motor_direction);
        p.set_spindle_pitch_mm(cfg.spindle_pitch_mm_per_rev as i32);
        p.set_servo_idle_timeout_s(cfg.servo_idle_timeout_s as i32);
        p.set_step_loss_recovery(cfg.step_loss_recovery);
        p.set_crash_detection(cfg.crash_detection);
        p.set_endstop_detection_threshold(cfg.endstop_detection_threshold as i32);
        p.set_min_force_for_effects_n(cfg.min_force_for_effects_n as i32);
        p.set_debug_flags(cfg.debug_flags as i32);
        // ---- filter ----
        p.set_kf_force_model_order(cfg.kf_force_model_order as i32);
        p.set_kf_force_model_noise(cfg.kf_force_model_noise as i32);
        p.set_kf_joystick_enabled(cfg.kf_joystick_enabled);
        p.set_kf_joystick_model_noise(cfg.kf_joystick_model_noise as i32);

        // Curve: zip the config's ×10-percent arrays back to normalized 0..1.
        let n = cfg
            .curve_travel_pct_x10
            .len()
            .min(cfg.curve_force_pct_x10.len());
        let points: Vec<(f32, f32)> = (0..n)
            .map(|i| {
                (
                    cfg.curve_travel_pct_x10[i] as f32 / 1000.0,
                    cfg.curve_force_pct_x10[i] as f32 / 1000.0,
                )
            })
            .collect();
        let points = if points.len() >= 2 {
            points
        } else {
            vec![(0.0, 0.0), (1.0, 1.0)]
        };
        set_curve_ui(&p, &points);
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
/// G-force (`FIELD_G_LONG_X100`), per-wheel slip (`FIELD_WHEEL_SLIP`) and
/// road/suspension impact (`FIELD_SUSP_IMPACT`) are decoded per-game where
/// the source carries them (see `pith-sim`); `tc_slip` remains a fallback
/// slip proxy for sources that only expose it (rF2/LMU).
fn build_action(telem: &[i32]) -> PedalAction {
    let get = |idx: usize| telem.get(idx).copied().unwrap_or(0);
    let rpm = get(FIELD_RPM).max(0) as f32;
    let max_rpm = get(FIELD_MAX_RPM).max(0) as f32;

    // Longitudinal G (×100, signed: +accel/−brake) → the 128-centered
    // g_value byte the firmware's GForceEffect expects. Full ±2 G maps to
    // the full byte range; absent data (0) stays at 128 = "no G". The floor is
    // 1, not 0: the firmware reads g_value 0 (→ g−128 == −128.0) as its "no G
    // data" sentinel, so braking past ~2 G must saturate at 1, not underflow to
    // 0 and drop the firming to zero exactly when it should be strongest.
    let g_long_x100 = get(FIELD_G_LONG_X100);
    let g_value = (128 + (g_long_x100 * 127 / 200)).clamp(1, 255) as u8;

    // Wheel slip: prefer the new per-wheel max-slip field; fall back to the
    // TC-slip proxy (LMU-only) when the richer field is absent.
    let slip_raw = get(FIELD_WHEEL_SLIP).max(0);
    let slip = if slip_raw > 0 {
        slip_raw
    } else {
        get(FIELD_TC_SLIP).max(0)
    };

    PedalAction {
        trigger_abs: get(FIELD_ABS_ACTIVE) != 0,
        track_condition: 0, // no telemetry source for surface wetness yet
        rpm_pct: pct_byte(rpm, max_rpm),
        g_value,
        wheel_slip: pct_byte(slip as f32, 100.0),
        // Suspension/road impact (0..1000 normalized) → 0..255 magnitude.
        impact_value: pct_byte(get(FIELD_SUSP_IMPACT).max(0) as f32, 1000.0),
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
                push_detected(&ctx);
                if found {
                    try_connect(&ctx, &mut dev);
                }
            }
            // No USB pedal — if one is on the network, route the effects
            // stream + config pushes over WiFi instead (the firmware accepts
            // the identical protocol on UDP; see pith-fw-wifi).
            if let Some(serial) = wireless_pedal_serial(&ctx) {
                let now = std::time::Instant::now();
                if now.duration_since(last_action) >= ACTION_INTERVAL {
                    last_action = now;
                    let telem = { ctx.lock().telem };
                    let action = build_action(&telem);
                    if let Ok(json) = serde_json::to_string(&action) {
                        ctx.send_wifi(crate::wifi::WifiOut::Line {
                            serial: serial.clone(),
                            line: format!("@ACT{json}"),
                        });
                    }
                }
                if let Some(cmd) = take_outbox(&ctx) {
                    handle_outbox_wireless(&ctx, &serial, cmd);
                }
            }
            std::thread::sleep(Duration::from_millis(20));
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
                PedalsOutbound::ProvisionWifi { ssid, password } => {
                    let msg = match dev.provision_wifi(&ssid, &password) {
                        Ok(()) => format!("WiFi credentials sent for \"{ssid}\""),
                        Err(e) => format!("WiFi provisioning failed: {e}"),
                    };
                    ctx.ui_run(move |u| u.global::<crate::Wireless>().set_pedal_status(sstr(&msg)));
                }
                PedalsOutbound::RefreshConfig => {
                    if let Some(cfg) = dev.get_config() {
                        adopt_device_config(&ctx, &cfg);
                        ctx.ui_run(|u| {
                            u.global::<Pedals>()
                                .set_config_status(sstr("Refreshed from device"));
                        });
                    }
                }
                PedalsOutbound::OtaFile(path) => {
                    flash_pedal_ota(&ctx, &mut dev, &path);
                    continue; // link was torn down for the reboot; rescan next loop
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
                clear_pedal_connected(&ctx);
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
            maybe_auto_switch(&ctx, &mut dev);
            push_detected(&ctx);
        }

        std::thread::sleep(Duration::from_millis(5));
    }
}

/// If auto-switch is enabled and the running game/car changed since the last
/// applied profile, resolve the bound profile and push it to the device
/// (the reference's `ApplyProfileAutoForGame`/`ApplyProfileAutoForCar`).
/// Unlike a manual profile load (which only fills the UI), auto-switch
/// applies to the device — that's the whole point of it.
fn maybe_auto_switch(ctx: &Arc<Ctx>, dev: &mut PedalsDev) {
    let (enabled, car, last_auto) = {
        let s = ctx.lock();
        (
            s.pedals_auto_switch,
            s.detected_model.clone(),
            s.pedals_last_auto.clone(),
        )
    };
    if !enabled {
        return;
    }
    let game = detected_game_id(ctx);
    let key = format!("{game}|{car}");
    if key == last_auto {
        return; // already handled this game/car
    }
    ctx.lock().pedals_last_auto = key;

    let Some(name) = resolve_auto_profile(&car, &game, &load_bindings()) else {
        return;
    };
    let Some(cfg) = load_profiles().get(&name).cloned() else {
        return;
    };
    let msg = match dev.set_config(&cfg) {
        Ok(()) => format!("Auto-switched to \"{name}\""),
        Err(e) => format!("Auto-switch push failed: {e}"),
    };
    apply_config_to_ui(ctx, &cfg);
    ctx.ui_run(move |u| u.global::<Pedals>().set_config_status(sstr(&msg)));
}

fn take_outbox(ctx: &Ctx) -> Option<PedalsOutbound> {
    let mut g = ctx.pedals_out.lock().unwrap();
    g.take()
}

/// Serial of a wireless pedal on the network (from the WiFi discovery table),
/// if any. Used when no USB pedal is connected.
fn wireless_pedal_serial(ctx: &Ctx) -> Option<String> {
    ctx.lock()
        .wifi_devices
        .iter()
        .find(|(kind, ..)| kind == "pedals")
        .map(|(_, serial, ..)| serial.clone())
}

/// Route a user command to a wireless-only pedal over UDP. Replies come back
/// as `RE` packets and land in the config-status line (see crate::wifi).
fn handle_outbox_wireless(ctx: &Arc<Ctx>, serial: &str, cmd: PedalsOutbound) {
    match cmd {
        PedalsOutbound::PushConfig(cfg) => {
            if let Ok(json) = serde_json::to_string(&cfg) {
                ctx.send_wifi(crate::wifi::WifiOut::Line {
                    serial: serial.to_string(),
                    line: format!("@CFG{json}"),
                });
                ctx.ui_run(|u| {
                    u.global::<Pedals>()
                        .set_config_status(sstr("Pushed over WiFi…"));
                });
            }
        }
        PedalsOutbound::ProvisionWifi { ssid, password } => {
            // Already wireless — re-provision over the air.
            ctx.send_wifi(crate::wifi::WifiOut::Line {
                serial: serial.to_string(),
                line: format!("@WIFI {ssid} {password}"),
            });
        }
        PedalsOutbound::RefreshConfig => {
            // @GETCFG over the WiFi request/reply bridge — same config JSON
            // the USB path pulls, adopted into the matching role slot.
            let reply = crate::wifi::request(ctx, serial, "@GETCFG", Duration::from_millis(800));
            let cfg = reply.as_deref().and_then(|text| {
                let body = text.strip_prefix("OK").map(str::trim_start)?;
                serde_json::from_str::<PedalConfig>(body).ok()
            });
            match cfg {
                Some(cfg) => {
                    adopt_device_config(ctx, &cfg);
                    ctx.ui_run(|u| {
                        u.global::<Pedals>()
                            .set_config_status(sstr("Refreshed from device (WiFi)"));
                    });
                }
                None => ctx.ui_run(|u| {
                    u.global::<Pedals>()
                        .set_config_status(sstr("Refresh over WiFi failed — device didn't reply"));
                }),
            }
        }
        PedalsOutbound::OtaFile(path) => {
            // Firmware update over the air: same @OTAWIFI TCP pull the
            // handbrake/DDU use.
            match std::fs::read(&path) {
                Ok(image) => {
                    ctx.send_wifi(crate::wifi::WifiOut::Ota {
                        serial: serial.to_string(),
                        image,
                    });
                    ctx.ui_run(|u| {
                        let p = u.global::<Pedals>();
                        p.set_flashing(false);
                        p.set_flash_status(sstr("Update sent over WiFi — pedal will flash + reboot"));
                    });
                }
                Err(e) => {
                    let msg = format!("Update failed: read image: {e}");
                    ctx.ui_run(move |u| {
                        let p = u.global::<Pedals>();
                        p.set_flashing(false);
                        p.set_flash_status(sstr(&msg));
                    });
                }
            }
        }
    }
}

/// Read the downloaded image and stream it to the pedal over @OTA, driving the
/// flash-progress bar. On success the device flips slots + reboots, so we drop
/// our handle and let the presence scan reconnect to the new firmware.
fn flash_pedal_ota(ctx: &Arc<Ctx>, dev: &mut PedalsDev, path: &std::path::Path) {
    let img = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            let msg = format!("Update failed: read image: {e}");
            ctx.ui_run(move |u| {
                let p = u.global::<Pedals>();
                p.set_flashing(false);
                p.set_flash_status(sstr(&msg));
            });
            return;
        }
    };
    let pc = ctx.clone();
    // download was the first quarter of the bar; the @OTA stream is the rest.
    let r = dev.ota_upload(&img, move |pct| {
        let frac = 0.25 + (pct as f32 / 100.0) * 0.75;
        pc.ui_run(move |u| u.global::<Pedals>().set_flash_progress(frac));
    });
    dev.close();
    clear_pedal_connected(ctx);
    let ok = r.is_ok();
    let msg = match r {
        Ok(()) => "Update installed — pedal is rebooting".to_string(),
        Err(e) => format!("Update failed: {e}"),
    };
    ctx.ui_run(move |u| {
        let p = u.global::<Pedals>();
        p.set_flashing(false);
        p.set_flash_progress(if ok { 1.0 } else { 0.0 });
        p.set_flash_status(sstr(&msg));
        p.set_connected(false);
        p.set_conn_detail(sstr("Rebooting into the new firmware…"));
    });
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
            // Remember the running version for update checks against pedals-v*.
            ctx.lock().pedals_fw = fw.clone();
            let c2 = ctx.clone();
            ctx.ui_run(move |u| {
                let p = u.global::<Pedals>();
                p.set_board(sstr(&board));
                p.set_fw_version(sstr(&fw));
                p.set_serial(sstr(&serial));
                p.set_connected(true);
                p.set_conn_detail(sstr(""));
                recompute_pedals_update(&u, &c2.lock());
            });
            if let Some(cfg) = dev.get_config() {
                // Adopting switches the editor to this pedal's role and marks
                // it live in the selector.
                adopt_device_config(ctx, &cfg);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn action_for_g(g_long_x100: i32) -> PedalAction {
        let mut telem = vec![0i32; 128]; // > FIELD_COUNT; build_action reads by index
        telem[FIELD_G_LONG_X100] = g_long_x100;
        build_action(&telem)
    }

    #[test]
    fn g_value_never_hits_the_firmware_no_data_sentinel() {
        // Braking past ~2 G must NOT underflow g_value to 0: the firmware reads
        // g_value 0 (→ g−128 == −128.0) as its "no G data" sentinel and zeroes
        // the force exactly when firming should be strongest. It saturates at 1.
        for g in [-202, -203, -400, -1000] {
            assert!(
                action_for_g(g).g_value >= 1,
                "g_long_x100={g} produced the sentinel g_value 0"
            );
        }
        // Absent / genuinely-zero G stays centered at 128 (not the sentinel).
        assert_eq!(action_for_g(0).g_value, 128);
        // Full +2 G still maps to the top of the byte range.
        assert_eq!(action_for_g(200).g_value, 255);
    }
}
