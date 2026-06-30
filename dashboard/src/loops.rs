use slint::ComponentHandle;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::ctx::Ctx;
use crate::games::detect_game;
use crate::net::cardata::{auto_apply_car_model, prefetch_game_data};
use crate::persist::{race_layout_from_json, save_race_layout};
use pith_sim::decoders::try_decode;
use crate::telemetry::frame_from_telem;
use crate::ui_bridge::cars::{push_car_results, push_classes, rebuild_filtered};
use crate::ui_bridge::firmware::{recompute_update_available, refresh_serial_ports};
use crate::ui_bridge::telemetry::{apply_caps, apply_status, apply_telemetry};
use crate::ui_bridge::{model, refresh_race, sstr};
use crate::{AppState, CarLib, Firmware, FwComponent, RaceLayout, Telemetry, TelemetryUdp};

pub fn try_connect(ctx: &Arc<Ctx>) -> bool {
    let mut d = ctx.dash();
    if d.hid.open(0x303A, 0x4002) {
        d.use_hid = true;
        if d.capabilities().contains("name") {
            return true;
        }
        d.hid.close();
    }
    d.use_hid = true;
    false
}

pub fn dash_close(ctx: &Arc<Ctx>) {
    let mut d = ctx.dash();
    d.hid.close();
    d.ser.close();
    d.use_hid = false;
}

/// Push one parsed `$`-frame to the device (~30 Hz) and the dashboard's own
/// overview preview (~12 Hz). Shared by every telemetry source (SimHub text
/// frames + decoded game packets + active connectors) so they all reach the
/// device + preview identically. `line` must start with `$`.
pub(crate) fn push_sim_frame(
    ctx: &Arc<Ctx>,
    line: &str,
    source: &str,
    last_push: &mut Instant,
    last_preview: &mut Instant,
) {
    if ctx.ota_active.load(Ordering::SeqCst) {
        return;
    }
    let Some(incoming) = pith_core::simhub::parse_line(line) else {
        return;
    };
    let now = Instant::now();
    // Merge every live source (augment, not replace), then augment with computed
    // fields (best/cur lap, fuel/lap, delta). Lap-to-lap tracking lives in State.
    let merged_frame = {
        let mut s = ctx.lock();
        s.last_sim_frame = Some(now);
        // Cache this source's latest frame (+ which computed fields it supplies,
        // sticky); expire sources silent for >2 s.
        s.src_frames
            .retain(|(lbl, _, at, _)| lbl == source || now.duration_since(*at) < Duration::from_secs(2));
        match s.src_frames.iter_mut().find(|(lbl, _, _, _)| lbl == source) {
            Some(e) => {
                e.1 = incoming;
                e.2 = now;
                e.3.observe(&incoming);
            }
            None => {
                let mut p = crate::telemetry::derive::Provided::default();
                p.observe(&incoming);
                s.src_frames.push((source.to_string(), incoming, now, p));
            }
        }
        // Rebuild from defaults and overlay each live source's NON-default fields.
        // Order by a STABLE priority (authoritative source applied last → wins), NOT
        // recency — otherwise two feeds taking turns being "newest" make the winner
        // of a shared field (e.g. gear) alternate every frame and flicker. A source
        // still never zeroes a field another provides (only non-default overlays).
        let mut order: Vec<usize> = (0..s.src_frames.len()).collect();
        order.sort_by_key(|&i| (source_priority(&s.src_frames[i].0), i));
        let mut merged = pith_core::simhub::Telemetry::default();
        let mut provided = crate::telemetry::derive::Provided::default();
        for &i in &order {
            let src = s.src_frames[i].1;
            provided = provided.merge(s.src_frames[i].3);
            if src.gear != 0 {
                merged.gear = src.gear;
            }
            for id in 1..pith_core::registry::FIELD_COUNT {
                let v = pith_core::registry::field_value(&src, id);
                if v != 0 {
                    pith_core::registry::set_field(&mut merged, id, v);
                }
            }
        }
        // Compute fields only when NO live source supplies them.
        s.derived.update(&mut merged, provided);
        frame_from_telem(&merged)
    };
    let line: &str = &merged_frame;
    // Push to the device (~30 Hz — smooth on the LCD, half the HID traffic of 60,
    // and no @T round-trip back from the device).
    if now.duration_since(*last_push) >= Duration::from_millis(33) {
        *last_push = now;
        let mut d = ctx.dash();
        if d.connected() {
            d.push_telemetry(line);
        }
    }
    // Feed the dashboard's OWN overview directly (~12 Hz) — much smoother than the
    // 6 Hz device_loop, capped so the rendered preview doesn't hog the UI thread.
    if now.duration_since(*last_preview) >= Duration::from_millis(80) {
        *last_preview = now;
        let frame = line[1..].to_string();
        let c2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = c2.lock();
            u.global::<Telemetry>().set_connected(true);
            apply_telemetry(&u, &mut s, &frame);
        });
    }
}

/// Handle one inbound SimHub text frame line (`$…`, `@CM…`, `@MAP…`). This is the
/// exact path the TCP listener used, kept intact so the (UDP-updated) SimHub
/// plugin still delivers the full field set, car model and track unchanged.
fn handle_text_frame(
    ctx: &Arc<Ctx>,
    line: &str,
    source: &str,
    last_push: &mut Instant,
    last_preview: &mut Instant,
) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if line.starts_with("@REL") {
        apply_relatives(ctx, line);
    } else if let Some(model) = line.strip_prefix("@CM") {
        apply_car_model(ctx, model.trim());
    } else if let Some(track) = line.strip_prefix("@MAP") {
        apply_track(ctx, track.trim());
    } else if line.starts_with('$') {
        push_sim_frame(ctx, line, source, last_push, last_preview);
    }
}

/// Store a multi-car relatives/standings list (for the preview) and forward the
/// raw `@REL` line to the device. The editor preview picks up `s.relatives` on its
/// next telemetry tick.
fn apply_relatives(ctx: &Arc<Ctx>, line: &str) {
    if let Some(rel) = pith_ui::Relatives::from_wire(line) {
        ctx.lock().relatives = rel;
    }
    if ctx.ota_active.load(Ordering::SeqCst) {
        return;
    }
    let mut d = ctx.dash();
    if d.connected() {
        d.push_relatives(line);
    }
}

/// Apply a car model from any source: show it as the detected car, match the
/// library, and push the per-car LED profile + shift scalars
/// (`auto_apply_car_model` dedups on the model string, so calling it per-frame is
/// cheap).
fn apply_car_model(ctx: &Arc<Ctx>, model: &str) {
    if model.is_empty() {
        return;
    }
    {
        let mut s = ctx.lock();
        s.detected_model = model.to_string();
        crate::net::cardata::auto_apply_car_model(ctx, &mut s, model);
    }
    let label = model.to_string();
    ctx.ui_run(move |u| {
        u.global::<CarLib>().set_detected_car(sstr(&label));
    });
}

/// Apply a detected track name from any source. Self-learned maps were removed —
/// the detected track now just selects the Map widget's outline. TODO (maps are on
/// the back burner): replace `trackmap::outline_for` with an SVG database keyed by
/// track name and push the chosen outline to the device on detection.
fn apply_track(ctx: &Arc<Ctx>, track: &str) {
    if track.is_empty() {
        return;
    }
    let mut s = ctx.lock();
    if s.map_track != track {
        s.map_track = track.to_string();
    }
}

/// Merge priority for a source label — higher wins when two live sources fill the
/// same field, so the winner is stable (no per-frame flicker). Shared memory is the
/// most complete/accurate, then the active game connectors, then SimHub, then the
/// passive UDP decoders. Ties fall back to first-seen order.
fn source_priority(label: &str) -> u8 {
    let l = label.to_ascii_lowercase();
    if l.contains("shim") || l.contains("shm") || matches!(label, "rF2/LMU" | "AC/ACC" | "AC EVO" | "RaceRoom") {
        4
    } else if matches!(label, "ACC" | "Assetto Corsa" | "Gran Turismo 7") {
        3
    } else if l.contains("simhub") {
        2
    } else {
        1
    }
}

/// Every source that has fed a frame in the last 2 s (sorted, deduped) — so the
/// page can show all live feeds at once instead of flipping between them when
/// several stream together (e.g. shim + SimHub).
fn live_sources(ctx: &Arc<Ctx>) -> Vec<String> {
    let now = Instant::now();
    let s = ctx.lock();
    let mut labels: Vec<String> = s
        .src_frames
        .iter()
        .filter(|(_, _, at, _)| now.duration_since(*at) < Duration::from_secs(2))
        .map(|(l, _, _, _)| l.clone())
        .collect();
    drop(s);
    labels.sort();
    labels.dedup();
    labels
}

/// Push the live-source list (a model the page renders one-per-line) + a joined
/// string (kept for the per-decoder "receiving" highlight).
fn push_source_label(ctx: &Arc<Ctx>) {
    let list = live_sources(ctx);
    let joined = list.join("  +  ");
    ctx.ui_run(move |u| {
        let g = u.global::<TelemetryUdp>();
        g.set_last_source(sstr(&joined));
        let items: Vec<slint::SharedString> = list.iter().map(|l| sstr(l)).collect();
        g.set_sources(model(items));
    });
}

/// Reflect the running UDP server's status into the Telemetry-UDP page.
fn push_udp_status(ctx: &Arc<Ctx>, bound: Option<u16>, packets: u64, pps: i32, _source: &str) {
    ctx.ui_run(move |u| {
        let g = u.global::<TelemetryUdp>();
        g.set_listening(bound.is_some());
        g.set_bound_port(bound.map(|p| p as i32).unwrap_or(0));
        g.set_packets(packets as i32);
        g.set_pps(pps);
    });
    push_source_label(ctx);
}

/// UDP telemetry receiver. Binds `0.0.0.0:<port>` and accepts two kinds of
/// datagram on the same socket:
///   * SimHub plugin text frames (`$…`, `@CM…`, `@MAP…`) — handled verbatim, so
///     the full field set still flows;
///   * native game telemetry (binary), run through the decoder registry
///     (Forza Horizon 6 first) → a `$`-frame → the same device/preview path.
/// The bind port is live-reconfigurable from the Telemetry-UDP page.
pub fn udp_listener_loop(ctx: Arc<Ctx>) {
    use std::net::UdpSocket;

    let bind = |port: u16| -> Option<UdpSocket> {
        match UdpSocket::bind(("0.0.0.0", port)) {
            Ok(s) => {
                let _ = s.set_read_timeout(Some(Duration::from_millis(250)));
                Some(s)
            }
            Err(_) => None,
        }
    };

    let mut port = ctx.lock().udp_port;
    let mut socket = bind(port);
    if socket.is_none() {
        push_udp_status(&ctx, None, 0, 0, "");
    }

    let mut buf = [0u8; 2048];
    let mut last_push = Instant::now();
    let mut last_preview = Instant::now();
    let mut last_ident = Instant::now() - Duration::from_secs(1);
    let mut last_stat = Instant::now();
    let mut packets: u64 = 0;
    let mut packets_at_stat: u64 = 0;
    let mut source = String::new();
    // A text sender can identify itself with an `@SRC<label>` line (the pith-shim
    // does this); otherwise an un-tagged text feed is assumed to be the SimHub
    // plugin. Tracked with a timestamp so it decays if the tagged sender stops.
    let mut text_src: Option<(String, Instant)> = None;

    while ctx.running.load(Ordering::SeqCst) {
        // Live port change from the UI → rebind.
        let want = ctx.lock().udp_port;
        if want != port || socket.is_none() {
            port = want;
            socket = bind(port);
            push_udp_status(&ctx, socket.as_ref().map(|_| port), packets, 0, &source);
            if socket.is_none() {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        }
        let sock = socket.as_ref().unwrap();

        if let Ok((n, _addr)) = sock.recv_from(&mut buf) {
            let data = &buf[..n];
            // Dispatch by first non-space byte: '$'/'@' → SimHub text; else binary.
            let first = data.iter().find(|b| !b.is_ascii_whitespace()).copied();
            if matches!(first, Some(b'$') | Some(b'@')) {
                if let Ok(text) = std::str::from_utf8(data) {
                    let mut had_frame = false;
                    for line in text.lines() {
                        let l = line.trim();
                        if let Some(src) = l.strip_prefix("@SRC") {
                            // Sender self-identification (e.g. the pith-shim).
                            text_src = Some((src.trim().to_string(), Instant::now()));
                        } else {
                            if l.starts_with('$') {
                                had_frame = true;
                            }
                            // Label this frame's source (fresh @SRC tag, else the
                            // SimHub plugin) so multi-source merge can tell feeds apart.
                            let cur_src = match &text_src {
                                Some((s, t)) if t.elapsed() < Duration::from_secs(3) => s.as_str(),
                                _ => "SimHub plugin",
                            };
                            handle_text_frame(&ctx, line, cur_src, &mut last_push, &mut last_preview);
                        }
                    }
                    if had_frame {
                        packets += 1;
                        // Use the tagged source if it's fresh, else the SimHub plugin.
                        source = match &text_src {
                            Some((s, t)) if t.elapsed() < Duration::from_secs(3) => s.clone(),
                            _ => "SimHub plugin".to_string(),
                        };
                    }
                }
            } else if let Some((name, dec)) = try_decode(data) {
                let frame = frame_from_telem(&dec.telem);
                push_sim_frame(&ctx, &frame, name, &mut last_push, &mut last_preview);
                packets += 1;
                source = name.to_string();
                // Surface the source game + (numeric) car identity ~1 Hz.
                if last_ident.elapsed() >= Duration::from_secs(1) {
                    last_ident = Instant::now();
                    let car = dec.car.unwrap_or_default();
                    ctx.ui_run(move |u| {
                        let cl = u.global::<CarLib>();
                        cl.set_detected_game(sstr(name));
                        cl.set_detected_car(sstr(&car));
                    });
                }
            }
        }

        // Refresh the page's live status ~2 Hz.
        if last_stat.elapsed() >= Duration::from_millis(500) {
            let dt = last_stat.elapsed().as_secs_f64();
            let pps = ((packets - packets_at_stat) as f64 / dt).round() as i32;
            last_stat = Instant::now();
            packets_at_stat = packets;
            push_udp_status(&ctx, Some(port), packets, pps, &source);
        }
    }
}

pub fn device_loop(ctx: Arc<Ctx>) {
    let mut fw_tick: u32 = 0;
    while ctx.running.load(Ordering::SeqCst) {
        // Re-scan for a locally built firmware image every ~2s so a bin built from
        // the terminal (just image) lights up the "FLASH LOCAL BUILD" button live.
        if fw_tick % 12 == 0 {
            let c2 = ctx.clone();
            ctx.ui_run(move |u| {
                let s = c2.lock();
                crate::ui_bridge::firmware::refresh_firmware_local(&u, &s);
            });
        }
        fw_tick = fw_tick.wrapping_add(1);
        if !ctx.dash().connected() {
            if try_connect(&ctx) {
                // Freshly (re)connected device: its LED car profile is RAM-only and
                // lost on reboot. Forget the last auto-applied car so the plugin's
                // next @CM heartbeat re-pushes the live @C and the LEDs light again.
                ctx.lock().last_auto_model.clear();
                let caps = ctx.dash().capabilities();
                let st = ctx.dash().status();
                let c2 = ctx.clone();
                ctx.ui_run(move |u| {
                    let mut s = c2.lock();
                    apply_caps(&u, &mut s, &caps);
                    let app = u.global::<AppState>();
                    app.set_connected(true);
                    app.set_conn_detail(sstr("Connected · HID (SimHub-safe)"));
                    app.set_health_pct(82);
                    if !st.is_empty() {
                        apply_status(&u, &mut s, &c2, &st);
                    }
                });
                // On connect, pull the device's saved layout so the editor shows what's
                // actually on the device — unless there are unsaved local edits to keep.
                if !ctx.lock().race_dirty {
                    read_race_from_device(&ctx);
                }
            } else {
                std::thread::sleep(Duration::from_millis(1500));
                continue;
            }
        }
        if ctx.ota_active.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(120));
            continue;
        }
        let st = ctx.dash().status();
        if st.is_empty() {
            dash_close(&ctx);
            let c2 = ctx.clone();
            ctx.ui_run(move |u| {
                let mut s = c2.lock();
                let app = u.global::<AppState>();
                app.set_connected(false);
                app.set_conn_detail(sstr("Disconnected"));
                app.set_health_pct(0);
                u.global::<Telemetry>().set_connected(false);
                let fw = u.global::<Firmware>();
                fw.set_current(sstr("—"));
                fw.set_components(model(Vec::<FwComponent>::new()));
                s.device_fw.clear();
                recompute_update_available(&u, &s);
                refresh_serial_ports(&u, &mut s);
            });
        } else {
            // If the SimHub plugin is feeding us (recent frame), drive our OWN view
            // from that frame and SKIP the @T round-trip — the app already has the
            // data, so polling it back from the device is pure waste + contention.
            let feeding = ctx
                .lock()
                .last_sim_frame
                .map_or(false, |t| t.elapsed() < Duration::from_millis(1500));
            // When the plugin feeds us, the sim_listener already drives the overview
            // (~12 Hz) AND pushes to the device — so here we just skip the @T poll.
            // Only poll @T for telemetry when there's no plugin feeding the app.
            if !feeding {
                let tl = ctx.dash().telemetry();
                let c2 = ctx.clone();
                ctx.ui_run(move |u| {
                    let mut s = c2.lock();
                    apply_status(&u, &mut s, &c2, &st);
                    if !tl.is_empty() {
                        apply_telemetry(&u, &mut s, &tl);
                    }
                });
            }
        }
        // Stream firmware logs (HID report id 3) into the GUI's device-log view.
        let new_logs = ctx.dash().take_device_logs();
        if !new_logs.is_empty() {
            let c2 = ctx.clone();
            ctx.ui_run(move |u| {
                let mut s = c2.lock();
                s.device_log.extend(new_logs.iter().cloned());
                let len = s.device_log.len();
                if len > 2000 {
                    s.device_log.drain(..len - 2000);
                }
                crate::ui_bridge::push_device_log(&u, &s);
            });
        }
        std::thread::sleep(Duration::from_millis(160));
    }
}

pub fn game_loop(ctx: Arc<Ctx>) {
    let sims = ctx.lock().sims.clone();
    let mut last = -2;
    while ctx.running.load(Ordering::SeqCst) {
        let gi = detect_game(&sims);
        if gi != last {
            last = gi;
            let c2 = ctx.clone();
            ctx.ui_run(move |u| {
                let detected_model;
                let do_prefetch;
                {
                    let mut s = c2.lock();
                    // When SimHub is actively feeding us it's authoritative for the
                    // game + car — don't let a flaky process scan (especially under
                    // Wine) wipe the detected game/car SimHub provided.
                    let feeding = s
                        .last_sim_frame
                        .map_or(false, |t| t.elapsed() < std::time::Duration::from_millis(3000));
                    if gi < 0 && feeding {
                        return;
                    }
                    s.detected_game_idx = gi;
                    let cl = u.global::<CarLib>();
                    let dg = if gi >= 0 {
                        s.sims[gi as usize].0.clone()
                    } else {
                        String::new()
                    };
                    cl.set_detected_game(sstr(&dg));
                    if gi < 0 {
                        cl.set_detected_car(sstr(""));
                        s.detected_model.clear();
                        s.last_auto_model.clear();
                        do_prefetch = false;
                        detected_model = String::new();
                    } else if gi != s.game {
                        s.game = gi;
                        s.klass = 0;
                        s.sel_car = -1;
                        cl.set_game(gi);
                        cl.set_klass(0);
                        cl.set_sel(-1);
                        push_classes(&u, &mut s);
                        rebuild_filtered(&mut s);
                        push_car_results(&u, &s);
                        s.last_auto_model.clear();
                        detected_model = s.detected_model.clone();
                        do_prefetch = true;
                    } else {
                        do_prefetch = false;
                        detected_model = String::new();
                    }
                }
                if do_prefetch {
                    prefetch_game_data(&c2);
                    if !detected_model.is_empty() {
                        let mut s = c2.lock();
                        auto_apply_car_model(&c2, &mut s, &detected_model);
                    }
                }
            });
        }
        std::thread::sleep(Duration::from_secs(3));
    }
}

pub fn sync_from_device(ctx: &Arc<Ctx>) {
    let connected = ctx.dash().connected();
    if !connected {
        ctx.ui_run(|u| {
            u.global::<AppState>()
                .set_sync_status(sstr("Not connected"))
        });
        return;
    }
    ctx.ui_run(|u| {
        u.global::<AppState>()
            .set_sync_status(sstr("Syncing from device…"))
    });
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let caps = ctx.dash().capabilities();
        let (ok, reply) = ctx.dash().command("@RG");
        let c2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = c2.lock();
            if !caps.is_empty() {
                apply_caps(&u, &mut s, &caps);
            }
            let mut got_layout = false;
            if ok {
                if let Some(b) = reply.find('{') {
                    if let Ok(j) = serde_json::from_str::<serde_json::Value>(&reply[b..]) {
                        if j.get("mods")
                            .map(|m| !m.as_array().map(|a| a.is_empty()).unwrap_or(true))
                            .unwrap_or(false)
                        {
                            race_layout_from_json(&mut s, &j);
                            s.race_dirty = false;
                            u.global::<RaceLayout>().set_dirty(false);
                            refresh_race(&u, &s);
                            save_race_layout(&s);
                            got_layout = true;
                        }
                    }
                }
            }
            u.global::<AppState>().set_sync_status(sstr(if got_layout {
                "Synced from device"
            } else {
                "Synced — device has no saved layout"
            }));
        });
    });
}

pub fn read_race_from_device(ctx: &Arc<Ctx>) {
    let connected = ctx.dash().connected();
    if !connected {
        ctx.ui_run(|u| {
            u.global::<RaceLayout>()
                .set_save_status(sstr("Not connected"))
        });
        return;
    }
    ctx.ui_run(|u| {
        u.global::<RaceLayout>()
            .set_save_status(sstr("Reading from device…"))
    });
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        // Prefer the full editor blob (@EG) — a lossless round-trip of the freeform
        // layout. Fall back to the legacy zone layout (@RG) on older firmware.
        let ed = ctx.dash().read_editor();
        let rg = ctx.dash().command("@RG").1;
        let c2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = c2.lock();
            let rl = u.global::<RaceLayout>();
            if let Some(b) = ed.find('{') {
                if let Ok(j) = serde_json::from_str::<serde_json::Value>(&ed[b..]) {
                    if crate::persist::apply_editor_layout_json(&mut s, &j) {
                        s.race_dirty = false;
                        rl.set_dirty(false);
                        refresh_race(&u, &s);
                        save_race_layout(&s);
                        rl.set_save_status(sstr("Loaded from device"));
                        return;
                    }
                }
            }
            if let Some(b) = rg.find('{') {
                if let Ok(j) = serde_json::from_str::<serde_json::Value>(&rg[b..]) {
                    if j.get("mods")
                        .map(|m| !m.as_array().map(|a| a.is_empty()).unwrap_or(true))
                        .unwrap_or(false)
                    {
                        race_layout_from_json(&mut s, &j);
                        s.race_dirty = false;
                        rl.set_dirty(false);
                        refresh_race(&u, &s);
                        save_race_layout(&s);
                        rl.set_save_status(sstr("Loaded from device (legacy)"));
                        return;
                    }
                }
            }
            rl.set_save_status(sstr("Device has no saved layout"));
        });
    });
}

// ============================ active connectors ============================
// These reach out to a game (rather than passively listening on the shared UDP
// port) and feed the same `$`-frame path as everything else.

use pith_sim::acc;

/// Set the Telemetry-UDP page's "source" label (throttled by the caller).
fn set_udp_source(ctx: &Arc<Ctx>, _src: &str) {
    // Show all live sources together rather than just this loop's label.
    push_source_label(ctx);
}

/// The sim-id of the currently process-detected game (from `game_loop`), or "".
/// Lets a connector auto-activate only while its game is actually running.
fn detected_sim(ctx: &Arc<Ctx>) -> String {
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

/// Build a `$`-frame from one ACC car update. ACC carries no RPM/pedals/fuel, so
/// those stay at idle — the dash, timing and track map work; shift lights don't.
fn acc_frame(u: &acc::CarUpdate) -> String {
    use pith_core::simhub::Telemetry;
    let mut t = Telemetry::idle();
    t.gear = crate::telemetry::le::gear_byte(u.gear);
    t.speed_kmh = u.kmh;
    t.position = u.position;
    t.laps_done = u.laps;
    t.delta_ms = u.delta_ms * 10; // ms → 0.1 ms units
    t.cur_lap_ms = u.cur_ms;
    t.last_lap_ms = u.last_ms;
    t.best_lap_ms = u.best_ms;
    t.track_pct = (u.spline * 1000.0).clamp(0.0, 1000.0) as i32;
    t.pos_x = u.world_x.round() as i32;
    t.pos_z = u.world_y.round() as i32;
    t.s1_ms = u.sectors[0];
    t.s2_ms = u.sectors[1];
    t.s3_ms = u.sectors[2];
    frame_from_telem(&t)
}

fn set_acc_status(ctx: &Arc<Ctx>, status: &'static str) {
    ctx.ui_run(move |u| u.global::<TelemetryUdp>().set_acc_status(sstr(status)));
}

/// ACC "Broadcasting" client. When enabled on the Telemetry-UDP page, registers
/// with the game (default 127.0.0.1:9000), streams the focused car's update, and
/// feeds it to the device. Re-registers on silence; tears down on config change.
pub fn acc_connector_loop(ctx: Arc<Ctx>) {
    use std::net::UdpSocket;
    let mut last_push = Instant::now();
    let mut last_preview = Instant::now();

    'outer: loop {
        if !ctx.running.load(Ordering::SeqCst) {
            return;
        }
        let (enabled, host, port, password) = {
            let s = ctx.lock();
            (s.acc_enabled, s.acc_host.clone(), s.acc_port, s.acc_password.clone())
        };
        // Only reach out while ACC is actually running (auto-detected) — no point
        // spamming REGISTER at a dead port otherwise.
        let running = detected_sim(&ctx) == "assettocorsacompetizione";
        if !enabled || !running {
            set_acc_status(&ctx, if !enabled { "Off" } else { "Waiting for ACC" });
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        let sock = match UdpSocket::bind(("0.0.0.0", 0)) {
            Ok(s) => s,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
        let reg = acc::encode_register("Pith DDU", &password, 250, "");
        let _ = sock.send_to(&reg, (host.as_str(), port));
        set_acc_status(&ctx, "Connecting…");

        let mut conn_id: Option<i32> = None;
        let mut focused: i32 = -1;
        let mut last_register = Instant::now();
        let mut last_rx = Instant::now();
        let mut last_src = Instant::now() - Duration::from_secs(2);
        let mut buf = [0u8; 2048];

        loop {
            if !ctx.running.load(Ordering::SeqCst) {
                return;
            }
            // Config changed / disabled / game closed → unregister and restart from
            // the top (which shows "Off" or "Waiting for ACC" rather than a stale
            // "Connected").
            {
                let s = ctx.lock();
                let cfg_changed = !s.acc_enabled
                    || s.acc_host != host
                    || s.acc_port != port
                    || s.acc_password != password;
                drop(s);
                if cfg_changed || detected_sim(&ctx) != "assettocorsacompetizione" {
                    if let Some(id) = conn_id {
                        let _ = sock.send_to(&acc::encode_request(acc::UNREGISTER, id), (host.as_str(), port));
                    }
                    continue 'outer;
                }
            }
            // Broadcasting stream went silent → stop showing a stale "Connected".
            if conn_id.is_some() && last_rx.elapsed() > Duration::from_secs(3) {
                set_acc_status(&ctx, "Reconnecting…");
            }
            if let Ok((n, _)) = sock.recv_from(&mut buf) {
                last_rx = Instant::now();
                if let Some(msg) = acc::parse(&buf[..n]) {
                    match msg {
                        acc::AccMsg::Registered { connection_id } => {
                            conn_id = Some(connection_id);
                            let _ = sock.send_to(
                                &acc::encode_request(acc::REQUEST_ENTRY_LIST, connection_id),
                                (host.as_str(), port),
                            );
                            let _ = sock.send_to(
                                &acc::encode_request(acc::REQUEST_TRACK_DATA, connection_id),
                                (host.as_str(), port),
                            );
                            set_acc_status(&ctx, "Connected");
                        }
                        acc::AccMsg::RegisterFailed => set_acc_status(&ctx, "Rejected — check password"),
                        acc::AccMsg::Realtime { focused_car_index } => focused = focused_car_index,
                        acc::AccMsg::Car(u) => {
                            if focused < 0 || u.car_index == focused {
                                let frame = acc_frame(&u);
                                push_sim_frame(&ctx, &frame, "ACC", &mut last_push, &mut last_preview);
                                if last_src.elapsed() >= Duration::from_secs(1) {
                                    last_src = Instant::now();
                                    set_udp_source(&ctx, "ACC");
                                }
                            }
                        }
                        acc::AccMsg::Other => {}
                    }
                }
            }
            // No traffic for a while (or never registered) → (re)register.
            if (conn_id.is_none() || last_rx.elapsed() > Duration::from_secs(3))
                && last_register.elapsed() > Duration::from_secs(2)
            {
                last_register = Instant::now();
                conn_id = None;
                let _ = sock.send_to(&reg, (host.as_str(), port));
            }
        }
    }
}

fn set_ac_status(ctx: &Arc<Ctx>, status: &'static str) {
    ctx.ui_run(move |u| u.global::<TelemetryUdp>().set_ac_status(sstr(status)));
}

/// Assetto Corsa (original) handshake client. Auto-connects when AC is detected:
/// handshakes on :9996, subscribes, then streams `RTCarInfo` → device. Has RPM,
/// so shift-lights work (off the device's configured redline).
pub fn ac_connector_loop(ctx: Arc<Ctx>) {
    use pith_sim::ac;
    use std::net::UdpSocket;
    let mut last_push = Instant::now();
    let mut last_preview = Instant::now();

    'outer: loop {
        if !ctx.running.load(Ordering::SeqCst) {
            return;
        }
        let (enabled, host, port) = {
            let s = ctx.lock();
            (s.ac_enabled, s.ac_host.clone(), s.ac_port)
        };
        let running = detected_sim(&ctx) == "assettocorsa";
        if !enabled || !running {
            set_ac_status(&ctx, if !enabled { "Off" } else { "Waiting for AC" });
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        let sock = match UdpSocket::bind(("0.0.0.0", 0)) {
            Ok(s) => s,
            Err(_) => {
                std::thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = sock.send_to(&ac::encode_op(ac::OP_HANDSHAKE), (host.as_str(), port));
        set_ac_status(&ctx, "Connecting…");

        let mut subscribed = false;
        let mut last_handshake = Instant::now();
        let mut last_rx = Instant::now();
        let mut last_src = Instant::now() - Duration::from_secs(2);
        let mut buf = [0u8; 1024];

        loop {
            if !ctx.running.load(Ordering::SeqCst) {
                return;
            }
            {
                let s = ctx.lock();
                let cfg_changed = !s.ac_enabled || s.ac_host != host || s.ac_port != port;
                drop(s);
                if cfg_changed || detected_sim(&ctx) != "assettocorsa" {
                    let _ = sock.send_to(&ac::encode_op(ac::OP_DISMISS), (host.as_str(), port));
                    continue 'outer;
                }
            }
            if subscribed && last_rx.elapsed() > Duration::from_secs(3) {
                set_ac_status(&ctx, "Reconnecting…");
            }
            if let Ok((n, _)) = sock.recv_from(&mut buf) {
                last_rx = Instant::now();
                let data = &buf[..n];
                if ac::is_handshake_response(data) {
                    let _ = sock.send_to(&ac::encode_op(ac::OP_SUBSCRIBE_UPDATE), (host.as_str(), port));
                    subscribed = true;
                    set_ac_status(&ctx, "Connected");
                    // The handshake reply carries the car + track names.
                    if let Some((car, track)) = ac::parse_handshake(data) {
                        apply_car_model(&ctx, car.trim());
                        apply_track(&ctx, track.trim());
                    }
                } else if let Some(t) = ac::parse_rtcarinfo(data) {
                    let frame = frame_from_telem(&t);
                    push_sim_frame(&ctx, &frame, "Assetto Corsa", &mut last_push, &mut last_preview);
                    if last_src.elapsed() >= Duration::from_secs(1) {
                        last_src = Instant::now();
                        set_udp_source(&ctx, "Assetto Corsa");
                    }
                }
            }
            // Re-handshake if we never subscribed or the stream went quiet.
            if (!subscribed || last_rx.elapsed() > Duration::from_secs(3))
                && last_handshake.elapsed() > Duration::from_secs(2)
            {
                last_handshake = Instant::now();
                subscribed = false;
                let _ = sock.send_to(&ac::encode_op(ac::OP_HANDSHAKE), (host.as_str(), port));
            }
        }
    }
}

fn set_gt7_status(ctx: &Arc<Ctx>, status: &'static str) {
    ctx.ui_run(move |u| u.global::<TelemetryUdp>().set_gt7_status(sstr(status)));
}

/// Gran Turismo 7 client. Manual (the console pushes to us): sends the heartbeat
/// to the PlayStation IP on :33739, receives Salsa20-encrypted packets on :33740,
/// decrypts + parses → device. Has RPM + rev-limiter, so shift-lights work.
pub fn gt7_connector_loop(ctx: Arc<Ctx>) {
    use pith_sim::gt7;
    use std::net::UdpSocket;
    let mut last_push = Instant::now();
    let mut last_preview = Instant::now();

    'outer: loop {
        if !ctx.running.load(Ordering::SeqCst) {
            return;
        }
        let (enabled, host) = {
            let s = ctx.lock();
            (s.gt7_enabled, s.gt7_host.clone())
        };
        if !enabled || host.trim().is_empty() {
            set_gt7_status(&ctx, if !enabled { "Off" } else { "Set console IP" });
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        let sock = match UdpSocket::bind(("0.0.0.0", gt7::RECV_PORT)) {
            Ok(s) => s,
            Err(_) => {
                set_gt7_status(&ctx, "Port 33740 busy");
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        let _ = sock.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = sock.send_to(gt7::HEARTBEAT, (host.as_str(), gt7::SEND_PORT));
        set_gt7_status(&ctx, "Connecting…");

        let mut pkts: u32 = 0;
        let mut last_src = Instant::now() - Duration::from_secs(2);
        let mut last_rx = Instant::now();
        let mut buf = [0u8; 2048];

        loop {
            if !ctx.running.load(Ordering::SeqCst) {
                return;
            }
            {
                let s = ctx.lock();
                if !s.gt7_enabled || s.gt7_host != host {
                    continue 'outer;
                }
            }
            if let Ok((n, _)) = sock.recv_from(&mut buf) {
                last_rx = Instant::now();
                if let Some(pt) = gt7::decrypt(&buf[..n]) {
                    if let Some(t) = gt7::parse(&pt) {
                        let frame = frame_from_telem(&t);
                        push_sim_frame(&ctx, &frame, "Gran Turismo 7", &mut last_push, &mut last_preview);
                        set_gt7_status(&ctx, "Connected");
                        if last_src.elapsed() >= Duration::from_secs(1) {
                            last_src = Instant::now();
                            set_udp_source(&ctx, "Gran Turismo 7");
                        }
                    }
                }
                pkts += 1;
                // GT7 stops after ~100 packets unless re-fed; resend the heartbeat.
                if pkts % 100 == 0 {
                    let _ = sock.send_to(gt7::HEARTBEAT, (host.as_str(), gt7::SEND_PORT));
                }
            } else if last_rx.elapsed() > Duration::from_secs(2) {
                // No data: keep prodding the console.
                let _ = sock.send_to(gt7::HEARTBEAT, (host.as_str(), gt7::SEND_PORT));
                set_gt7_status(&ctx, "Waiting for GT7…");
            }
        }
    }
}

fn set_shm_status(ctx: &Arc<Ctx>, status: &'static str) {
    ctx.ui_run(move |u| u.global::<TelemetryUdp>().set_shm_status(sstr(status)));
}

/// Native shared-memory reader (Linux). Scans `/dev/shm` for a known sim block
/// (exposed there by an in-prefix bridge like simshmbridge), reads + parses it
/// → device. This is how ACC gets RPM/shift-lights natively, no SimHub/plugin.
#[cfg(target_os = "linux")]
pub fn shm_reader_loop(ctx: Arc<Ctx>) {
    use pith_sim::shm_read as shm;
    let mut last_push = Instant::now();
    let mut last_preview = Instant::now();
    let mut last_src = Instant::now() - Duration::from_secs(2);
    let mut missing_since = Instant::now();

    while ctx.running.load(Ordering::SeqCst) {
        if !ctx.lock().shm_enabled {
            set_shm_status(&ctx, "Off");
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        match shm::read_once() {
            Some(r) => {
                let frame = frame_from_telem(&r.telem);
                push_sim_frame(&ctx, &frame, r.label, &mut last_push, &mut last_preview);
                if last_src.elapsed() >= Duration::from_secs(1) {
                    last_src = Instant::now();
                    set_shm_status(&ctx, "Reading");
                    set_udp_source(&ctx, r.label);
                    // Car/track drive the library LED match + self-learned map.
                    if let Some(car) = r.car.as_deref() {
                        apply_car_model(&ctx, car);
                    }
                    if let Some(track) = r.track.as_deref() {
                        apply_track(&ctx, track);
                    }
                }
                missing_since = Instant::now();
                std::thread::sleep(Duration::from_millis(20)); // ~50 Hz
            }
            None => {
                if missing_since.elapsed() > Duration::from_secs(1) {
                    set_shm_status(&ctx, "No /dev/shm block (run a bridge)");
                }
                std::thread::sleep(Duration::from_millis(400));
            }
        }
    }
}

/// No shared-memory reader off Linux (Windows/macOS use the native SDKs).
#[cfg(not(target_os = "linux"))]
pub fn shm_reader_loop(_ctx: Arc<Ctx>) {}
