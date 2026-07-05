use slint::ComponentHandle;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicUsize};
use std::sync::{Arc, Mutex};

use crate::catalog::{seed_boards, seed_buttons, seed_presets, seed_shift};
use crate::ctx::Ctx;
use crate::device::Dash;
use crate::firmware::APP_FW_VERSION;
use crate::net::cardata::{
    load_manifest_from_cache_or_net, prefetch_game_data, refresh_database, sync_database_if_stale,
};
use crate::net::releases::fetch_firmware_releases;
use crate::persist::*;
use crate::state::State;
use crate::telemetry::*;
use crate::ui_bridge::buttons::push_buttons_model;
use crate::ui_bridge::cars::{push_car_results, push_classes, push_games, rebuild_filtered};
use crate::ui_bridge::device::push_pins;
use crate::ui_bridge::firmware::{refresh_firmware_local, refresh_serial_ports};
use crate::ui_bridge::race::{push_catalog, push_editor_options};
use crate::ui_bridge::shift::{push_led_model, push_shift_scalars};
use crate::ui_bridge::{col, model, sstr};
use crate::{AppWindow, Firmware, FwComponent, Telemetry, TyreCell};

fn seed_demo(u: &AppWindow, s: &mut State) {
    let fw = u.global::<Firmware>();
    fw.set_update_available(false);
    fw.set_current(sstr("—"));
    fw.set_latest(sstr(&format!("v{APP_FW_VERSION}")));
    fw.set_components(model(Vec::<FwComponent>::new()));
    fw.set_notes(model(vec![
        sstr("Data-driven race screen (@RS layout language)"),
        sstr("3 button pages (@BS)"),
        sstr("Dual CDC: command + data ports"),
        sstr("@CAP reports main + side screens"),
    ]));
    refresh_firmware_local(u, s);

    let t = u.global::<Telemetry>();
    t.set_tyres(model(vec![
        TyreCell {
            temp: 86,
            col: col(0x00E676),
        },
        TyreCell {
            temp: 88,
            col: col(0x00E676),
        },
        TyreCell {
            temp: 84,
            col: col(0x2E9DFF),
        },
        TyreCell {
            temp: 85,
            col: col(0x00E676),
        },
    ]));
    t.set_position(4);
    t.set_field_size(20);
    t.set_lap_num(12);
    t.set_cur_lap(sstr("1:24.318"));
    t.set_best_lap(sstr("1:22.900"));
    t.set_last_lap(sstr("1:23.450"));
    t.set_speed_unit(sstr("KM/H"));
    t.set_water_c(92);
    t.set_oil_c(104);
    t.set_brake_bias(56.5);
    t.set_tc(4);
    t.set_abs(2);

    s.gear_ch = '4';
    s.telem[FIELD_SPEED_KMH] = 212;
    s.telem[FIELD_RPM] = 8100;
    s.telem[FIELD_MAX_RPM] = 8800;
    s.telem[FIELD_SHIFT_RPM] = 8100;
    s.telem[FIELD_CUR_LAP_MS] = 84318;
    s.telem[FIELD_LAST_LAP_MS] = 83450;
    s.telem[FIELD_BEST_LAP_MS] = 82900;
    s.telem[FIELD_PB_LAP_MS] = 82500;
    s.telem[FIELD_EST_LAP_MS] = 83100;
    s.telem[FIELD_DELTA_MS] = -3120;
    s.telem[FIELD_POSITION] = 4;
    s.telem[FIELD_FIELD_SIZE] = 20;
    s.telem[FIELD_LAPS_DONE] = 12;
    s.telem[FIELD_TOTAL_LAPS] = 30;
    s.telem[FIELD_LAPS_LEFT] = 18;
    s.telem[FIELD_WATER_C] = 92;
    s.telem[FIELD_OIL_C] = 104;
    s.telem[FIELD_OIL_PRESS_X10] = 42;
    s.telem[FIELD_BOOST_KPA] = 120;
    s.telem[FIELD_TC] = 4;
    s.telem[FIELD_ABS] = 2;
    s.telem[FIELD_BRAKE_BIAS_X10] = 565;
    s.telem[FIELD_FUEL_DL] = 486;
    s.telem[FIELD_FUEL_CAP_DL] = 750;
    s.telem[FIELD_FUEL_PER_LAP_ML] = 2400;
    s.telem[FIELD_FUEL_LAPS_X10] = 130;
    s.telem[FIELD_TT_FL_M] = 88;
    s.telem[FIELD_TT_FR_M] = 90;
    s.telem[FIELD_TT_RL_M] = 86;
    s.telem[FIELD_TT_RR_M] = 87;
    s.telem[FIELD_TP_FL] = 165;
    s.telem[FIELD_TP_FR] = 166;
    s.telem[FIELD_TP_RL] = 160;
    s.telem[FIELD_TP_RR] = 161;
    s.telem[FIELD_TW_FL] = 92;
    s.telem[FIELD_TW_FR] = 90;
    s.telem[FIELD_TW_RL] = 91;
    s.telem[FIELD_TW_RR] = 89;
    s.telem[FIELD_BT_FL] = 350;
    s.telem[FIELD_BT_FR] = 360;
    s.telem[FIELD_BT_RL] = 310;
    s.telem[FIELD_BT_RR] = 320;
    s.telem[FIELD_THROTTLE] = 100;
    s.telem[FIELD_BRAKE] = 0;
    s.telem[FIELD_CLUTCH] = 0;
    s.telem[FIELD_STEER] = 0;
    s.telem[FIELD_S1_MS] = 28100;
    s.telem[FIELD_S2_MS] = 30900;
    s.telem[FIELD_S3_MS] = 25600;
}

pub fn init(ui: &AppWindow, rt: &tokio::runtime::Runtime) -> Arc<Ctx> {
    init_impl(ui, rt, true)
}

/// Screenshot/headless setup: seed the demo + populate every UI model, but don't
/// spawn the device/UDP/game loops or hit the network (offline, side-effect free).
pub fn init_screenshot(ui: &AppWindow, rt: &tokio::runtime::Runtime) -> Arc<Ctx> {
    init_impl(ui, rt, false)
}

fn init_impl(ui: &AppWindow, rt: &tokio::runtime::Runtime, live: bool) -> Arc<Ctx> {
    let mut s = State::default();
    seed_shift(&mut s);
    match load_presets(&s) {
        Some((presets, _)) => {
            s.presets = presets;
            s.zones = s.presets[0].zones.clone();
            s.nodes = s.presets[0].nodes.clone();
            s.active_preset = 0;
            s.uid = 1_000_000;
        }
        None => seed_presets(&mut s),
    }
    seed_buttons(&mut s);
    if let Some(pages) = load_buttons() {
        s.btn_pages = pages;
    }
    if let Some((zones, nodes, active)) = load_race_layout() {
        s.zones = zones;
        s.nodes = nodes;
        if active >= 0 && (active as usize) < s.presets.len() {
            s.active_preset = active;
        }
        s.tabs = load_race_tabs();
        s.map_track = load_map_track();
    }
    load_active_car(&mut s);
    load_shift_cfg(&mut s);
    load_udp_cfg(&mut s);
    seed_boards(&mut s);
    load_board(&mut s);
    rt.block_on(load_manifest_from_cache_or_net(&mut s));

    let ctx = Arc::new(Ctx {
        ui: ui.as_weak(),
        state: Arc::new(Mutex::new(s)),
        dash: Arc::new(Mutex::new(Dash::default())),
        rt: rt.handle().clone(),
        running: Arc::new(AtomicBool::new(true)),
        ota_active: Arc::new(AtomicBool::new(false)),
        sim_active: Arc::new(AtomicBool::new(false)),
        busy: Arc::new(AtomicBool::new(false)),
        car_gen: Arc::new(AtomicUsize::new(0)),
        build_cancel: Arc::new(AtomicBool::new(false)),
        build_pgid: Arc::new(AtomicI32::new(-1)),
        tray_active: Arc::new(AtomicBool::new(false)),
        dev_out: Arc::new((
            Mutex::new(crate::ctx::DevOutbox::default()),
            std::sync::Condvar::new(),
        )),
        hb_out: Arc::new((Mutex::new(None), std::sync::Condvar::new())),
        pedals_out: Arc::new(Mutex::new(None)),
        wifi_out: Arc::new(Mutex::new(Vec::new())),
    });

    {
        let mut st = ctx.lock();
        seed_demo(ui, &mut st);
        push_shift_scalars(ui, &st);
        push_led_model(ui, &st);
        push_catalog(ui, &st);
        // Populate every race-editor model (zones, nodes, presets, resolved,
        // edit module, elems, preview) up front — same as any later refresh.
        // Without push_nodes here the interactive boxes stay empty until the
        // first preset switch, so EDIT MODE looks dead until you reselect.
        crate::ui_bridge::refresh_race(ui, &st);
        push_editor_options(ui, &st);
        push_buttons_model(ui, &st);
        push_pins(ui, &st);
        refresh_firmware_local(ui, &st);
        refresh_serial_ports(ui, &mut st);
        push_games(ui, &st);
        push_classes(ui, &mut st);
        rebuild_filtered(&mut st);
        push_car_results(ui, &st);
        crate::ui_bridge::udp::push_udp_cfg(ui, &st);
    }

    crate::callbacks::wire_callbacks(ui, &ctx);
    crate::hb::wire_hb_callbacks(ui, &ctx);
    crate::pedals::wire_pedals_callbacks(ui, &ctx);

    // Screenshot/headless mode stops here: no network, no background loops.
    if !live {
        return ctx;
    }

    fetch_firmware_releases(&ctx);
    prefetch_game_data(&ctx);
    sync_database_if_stale(&ctx);

    let empty = ctx.lock().all_cars.is_empty();
    if empty {
        refresh_database(&ctx);
    }

    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::device_loop(c)
    });
    // The handbrake is its own USB device with its own connection lifecycle —
    // a dedicated thread owns its HID handle (never shared with the DDU's).
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::hb::hb_device_loop(c)
    });
    // The active pedal is likewise its own USB device with its own connection
    // lifecycle and its own effects-engine tick loop.
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::pedals::pedals_device_loop(c)
    });
    // Dedicated writer for the fire-and-forget device streams (telemetry +
    // relatives) — a wedged HID link stalls only this thread, not ingestion.
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::device_writer_loop(c)
    });
    // WiFi/UDP transport: discovers wireless Pith devices, routes their axis
    // into a virtual joystick (only when WiFi input mode is on), and forwards
    // telemetry to a wireless DDU.
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::wifi::wifi_loop(c)
    });
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::game_loop(c)
    });
    // UDP telemetry receiver: SimHub-plugin text frames + native game decoders
    // (Forza Horizon 6 first) on the configured port → device over HID.
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::udp_listener_loop(c)
    });
    // Active connectors: ACC + Assetto Corsa auto-connect when detected running;
    // GT7 streams from the configured PlayStation IP.
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::acc_connector_loop(c)
    });
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::ac_connector_loop(c)
    });
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::gt7_connector_loop(c)
    });
    // Native shared-memory reader (Linux /dev/shm via a bridge) — gives AC/ACC
    // RPM/shift-lights with no SimHub/plugin.
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::shm_reader_loop(c)
    });
    // GUI "Simulate" test feed (idles until the button is toggled on).
    rt.spawn_blocking({
        let c = ctx.clone();
        move || crate::loops::sim_loop(c)
    });

    ctx
}
