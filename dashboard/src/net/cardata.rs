use slint::ComponentHandle;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde_json::Value;

use crate::ctx::Ctx;
use crate::net::http::{http_download_file, http_get};
use crate::paths::*;
use crate::state::{CarItem, State};
use crate::ui_bridge::cars::{push_car_results, push_classes, rebuild_filtered};
use crate::ui_bridge::shift::{push_led_model, push_shift_scalars};
use crate::ui_bridge::sstr;
use crate::util::{norm_name, trim};
use crate::CarLib;

const MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/Lovely-Sim-Racing/lovely-car-data/main/data/manifest.json";
const CAR_BASE_URL: &str =
    "https://raw.githubusercontent.com/Lovely-Sim-Racing/lovely-car-data/main/data/";
const COMMITS_URL: &str =
    "https://api.github.com/repos/Lovely-Sim-Racing/lovely-car-data/commits/main";
const TARBALL_URL: &str =
    "https://codeload.github.com/Lovely-Sim-Racing/lovely-car-data/tar.gz/refs/heads/main";

// ---- built-in car profiles (dashboard/cars/*.json, embedded at build) ----
//
// Profiles we ship with the tool for games the lovely-car-data library can't
// serve. Each is a normal library-schema JSON file in `dashboard/cars/` —
// add a file + one table entry to add a game. They are injected at the FRONT
// of `all_cars` (so local profiles win a matching tie against the downloaded
// library), reachable via the `builtin:` path scheme in [`car_body_by_path`],
// and need no network or packaging step (embedded via `include_str!`).
//
// F1: the UDP telemetry never sends a car/team name (`Decoded::car` is always
// `None`), so a name match can never fire — `decoder` lets the F1 decoder
// auto-select this profile directly. Every F1 car in a season runs the same
// spec hybrid V6 (the ledRpm thresholds are identical across every team entry
// in lovely-car-data, confirming the shared-engine assumption), so one
// profile is genuinely accurate for the whole grid. Forza and most other UDP
// decoders deliberately have NO builtin: they report the vehicle's live
// `max_rpm`, so the generic rev bar already scales per-vehicle.
struct BuiltinCar {
    /// Sim ids this profile appears under in the car library.
    sims: &'static [&'static str],
    /// Decoder `name()` that auto-selects this profile when its game sends no
    /// usable car identity ("" = never auto-selected, library entry only).
    decoder: &'static str,
    /// `builtin:` path suffix — the stable lookup key for `car_body_by_path`.
    key: &'static str,
    json: &'static str,
}

const BUILTIN_CARS: &[BuiltinCar] = &[BuiltinCar {
    sims: &["f12024", "f12025"],
    decoder: pith_sim::f1::NAME,
    key: "f1",
    json: include_str!("../../cars/f1.json"),
}];

/// The embedded JSON for a `builtin:<key>` path, if it is one.
fn builtin_body(path: &str) -> Option<&'static str> {
    let key = path.strip_prefix("builtin:")?;
    BUILTIN_CARS.iter().find(|b| b.key == key).map(|b| b.json)
}

/// The built-in profiles as library entries (one per sim id), used to seed
/// `all_cars` ahead of the downloaded manifest.
pub fn builtin_car_items() -> Vec<CarItem> {
    let mut out = Vec::new();
    for b in BUILTIN_CARS {
        let Ok(j) = serde_json::from_str::<Value>(b.json) else {
            continue; // malformed shipped file — also caught by the unit test
        };
        let cols = parse_car_led_colors(&j);
        for sim in b.sims {
            out.push(CarItem {
                sim: sim.to_string(),
                name: trim(j.get("carName").and_then(|x| x.as_str()).unwrap_or(b.key)),
                id: j
                    .get("carId")
                    .and_then(|x| x.as_str())
                    .unwrap_or(b.key)
                    .to_string(),
                path: format!("builtin:{}", b.key),
                klass: j
                    .get("carClass")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                redline: derive_redline(&j),
                led_n: cols.len() as i32,
                led_cols: cols.clone(),
            });
        }
    }
    out
}

/// Auto-apply the built-in profile tied to `decoder_name`, if any. Used when a
/// decoder's game sent no usable car identity (or no library match was found).
/// Shares `last_auto_model` with [`auto_apply_car_model`] as the single "what's
/// on the device" dedup key, so device-reconnect / game-switch invalidation
/// (which clear that key) automatically re-push this profile too.
pub fn apply_builtin_for_decoder(ctx: &Arc<Ctx>, s: &mut State, decoder_name: &str) -> bool {
    let Some(b) = BUILTIN_CARS
        .iter()
        .find(|b| !b.decoder.is_empty() && b.decoder == decoder_name)
    else {
        return false;
    };
    // Find its library entry (seeded by builtin_car_items); fall back to a
    // synthesized item so this still works before any manifest has loaded.
    let path = format!("builtin:{}", b.key);
    let car = s
        .all_cars
        .iter()
        .find(|c| c.path == path)
        .cloned()
        .or_else(|| builtin_car_items().into_iter().find(|c| c.path == path));
    let Some(car) = car else {
        return false;
    };
    if s.last_auto_model == car.id {
        return true; // already applied (key cleared on reconnect/game switch)
    }
    s.last_auto_model = car.id.clone();
    spawn_apply_profile(ctx, car, true);
    true
}

/// No car profile could be matched for this game — clear the device's loaded
/// car (`@C{}`) so its LED strip falls back to the GENERIC rev bar, which is
/// driven by the telemetry's live shift/max RPM (every decoder sets those).
/// Without this, a stale profile from a previous game — persisted on the
/// device across reboots — keeps per-gear thresholds the new game's cars may
/// never reach, so the shift lights just stay dark. Dedups on
/// `last_auto_model` like every other apply path (a real match later
/// overwrites the sentinel and wins).
pub fn apply_generic_profile(ctx: &Arc<Ctx>, s: &mut State, decoder_name: &str) {
    let sentinel = format!("#generic:{decoder_name}");
    if s.last_auto_model == sentinel {
        return;
    }
    s.last_auto_model = sentinel;
    // Preview parity: reset the editor's LED model to the generic ramp so the
    // on-screen strip mirrors what the device will now show.
    crate::catalog::seed_shift(s);
    s.car_name.clear();
    s.car_id.clear();
    let label = format!("Generic shift lights ({decoder_name}, max-RPM)");
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let cleared = ctx.dash().connected() && ctx.dash().push_car("{}");
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let s = ctx2.lock();
            push_shift_scalars(&u, &s);
            push_led_model(&u, &s);
            u.global::<CarLib>().set_detected_car(sstr(""));
            u.global::<CarLib>().set_status(sstr(&if cleared {
                label.clone()
            } else {
                format!("{label} — device offline")
            }));
        });
    });
}

/// Shared tail of the auto-apply paths: fetch the profile body (instant for
/// `builtin:` paths), push it to the device, and load it into the GUI state.
/// Runs on a spawned task so the device round-trip (up to ~2 s on a wedged
/// HID link) never happens on the caller's thread — callers typically hold
/// the State mutex, and blocking there freezes the UI + every telemetry loop.
fn spawn_apply_profile(ctx: &Arc<Ctx>, car: CarItem, builtin: bool) {
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let body = car_body_by_path(&car.path).await;
        let mut redline = car.redline;
        let minified = match serde_json::from_str::<Value>(&body) {
            Ok(j) => {
                if redline == 0 {
                    redline = derive_redline(&j);
                }
                serde_json::to_string(&j).unwrap_or_default()
            }
            Err(_) => {
                let label = car.name.clone();
                ctx.ui_run(move |u| {
                    u.global::<CarLib>()
                        .set_status(sstr(&format!("{label}: profile parse error")));
                });
                return;
            }
        };
        let ok = !minified.is_empty() && ctx.dash().connected() && ctx.dash().push_car(&minified);
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = ctx2.lock();
            if redline > 0 {
                s.redline_rpm = redline;
                s.shift_custom = false;
            }
            s.car_name = car.name.clone();
            s.car_game = car.sim.clone();
            s.car_id = car.id.clone();
            if let Ok(j) = serde_json::from_str::<Value>(&body) {
                load_car_into_leds(&mut s, &j);
            }
            if let Some(ai) = s
                .all_cars
                .iter()
                .position(|c| c.path == car.path && c.sim == car.sim)
            {
                if let Some(fi) = s.filtered.iter().position(|&x| x == ai) {
                    u.global::<CarLib>().set_sel(fi as i32);
                }
            }
            push_shift_scalars(&u, &s);
            push_led_model(&u, &s);
            push_car_results(&u, &s);
            crate::persist::save_active_car(&s);
            if builtin {
                u.global::<CarLib>().set_detected_car(sstr(&car.name));
            }
            u.global::<CarLib>().set_status(sstr(&if ok {
                format!("Auto: {}", car.name)
            } else {
                format!("Matched {} (offline)", car.name)
            }));
        });
    });
}

fn json_int(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))
}

pub fn derive_redline(j: &Value) -> i32 {
    let mut rl = 0;
    if let Some(arr) = j.get("ledRpm").and_then(|x| x.as_array()) {
        if let Some(obj) = arr.first().and_then(|x| x.as_object()) {
            for (_gear, a) in obj {
                if let Some(items) = a.as_array() {
                    for v in items {
                        if let Some(n) = json_int(v) {
                            if n as i32 > rl {
                                rl = n as i32;
                            }
                        }
                    }
                }
            }
        }
    }
    rl
}

pub fn parse_car_led_colors(j: &Value) -> Vec<u32> {
    let arr = match j.get("ledColor").and_then(|x| x.as_array()) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let raw = j.get("ledNumber").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
    let n = if raw > 12 { 12 } else { raw };
    let skip = if raw > 12 { raw - 12 } else { 0 };
    let mut out = Vec::new();
    for i in 0..n {
        let idx = (skip + i + 1) as usize;
        let mut s = arr
            .get(idx)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if s.starts_with('#') {
            s = s[1..].to_string();
        }
        out.push(crate::util::hex_prefix(&s) & 0xFFFFFF);
    }
    out
}

pub fn load_car_into_leds(s: &mut State, j: &Value) {
    let cols = parse_car_led_colors(j);
    let redline = derive_redline(j);
    if cols.is_empty() || redline <= 0 {
        return;
    }
    let raw = j.get("ledNumber").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
    let skip = if raw > 12 { raw - 12 } else { 0 };
    let gears = j
        .get("ledRpm")
        .and_then(|x| x.as_array())
        .and_then(|a| a.first())
        .filter(|o| o.is_object());
    for gear in 1..=6usize {
        let key = gear.to_string();
        let arr = gears.and_then(|g| g.get(&key)).and_then(|a| a.as_array());
        for i in 0..12 {
            let col = if i < cols.len() { cols[i] } else { 0 };
            let mut thr = 0;
            if let Some(a) = arr {
                let idx = (skip as usize) + i + 1;
                if let Some(v) = a.get(idx).and_then(json_int) {
                    thr = (v as i32) * 100 / redline;
                }
            }
            s.leds[gear][i].rgb = col;
            s.leds[gear][i].threshold = thr;
        }
    }
}

fn car_dedup_sig(sim: &str, it: &CarItem) -> String {
    let body = read_file(&data_root().join(&it.path));
    if !body.is_empty() {
        let mut b = body;
        if let Some(p) = b.find("\"carId\"") {
            let e = b[p..].find('\n').map(|o| p + o).unwrap_or(b.len());
            b.replace_range(p..e, "");
        }
        format!("{sim}\u{1f}{b}")
    } else {
        format!("{sim}\u{1f}#{}", it.id)
    }
}

pub fn parse_manifest(s: &mut State, body: &str) {
    s.all_cars.clear();
    // Shipped built-ins come first — present even with no manifest (offline,
    // first run), and ahead of the downloaded library so a matching tie in
    // auto_apply_car_model resolves to our own profile.
    s.all_cars.extend(builtin_car_items());
    let j: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return,
    };
    let cars = match j.get("cars").and_then(|c| c.as_object()) {
        Some(c) => c,
        None => return,
    };
    let mut seen = std::collections::HashSet::new();
    for (sim, arr) in cars {
        if let Some(list) = arr.as_array() {
            for c in list {
                let it = CarItem {
                    sim: sim.clone(),
                    name: trim(c.get("carName").and_then(|x| x.as_str()).unwrap_or("")),
                    id: c
                        .get("carId")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    path: c
                        .get("path")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .to_string(),
                    ..Default::default()
                };
                if it.name.is_empty() {
                    continue;
                }
                if !seen.insert(car_dedup_sig(sim, &it)) {
                    continue;
                }
                s.all_cars.push(it);
            }
        }
    }
}

pub async fn car_body_by_path(path: &str) -> String {
    // Shipped built-in profile — embedded in the binary, no disk or network.
    if let Some(body) = builtin_body(path) {
        return body.to_string();
    }
    let body = read_file(&data_root().join(path));
    if !body.is_empty() {
        return body;
    }
    let safe = path.replace('/', "_");
    let cf = cache_dir().join(format!("{safe}.car"));
    if cf.exists() {
        return read_file(&cf);
    }
    let (body, _code) = http_get(&format!("{CAR_BASE_URL}{path}")).await;
    if !body.is_empty() {
        let _ = std::fs::write(&cf, &body);
    }
    body
}

fn cached_commit() -> String {
    read_file(&commit_path())
}
fn store_commit(sha: &str) {
    let _ = std::fs::write(commit_path(), sha);
}
async fn latest_commit() -> String {
    let (body, _) = http_get(COMMITS_URL).await;
    if body.is_empty() {
        return String::new();
    }
    serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|j| j.get("sha").and_then(|x| x.as_str()).map(|s| s.to_string()))
        .unwrap_or_default()
}

fn extract_tar_gz(archive: &std::path::Path, dest: &std::path::Path) -> bool {
    let _ = std::fs::create_dir_all(dest);
    let f = match std::fs::File::open(archive) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let gz = flate2::read::GzDecoder::new(f);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(dest).is_ok()
}

pub async fn load_manifest_from_cache_or_net(s: &mut State) {
    let mut body = read_file(&data_root().join("manifest.json"));
    if body.is_empty() {
        let (b, _) = http_get(MANIFEST_URL).await;
        body = b;
        if !body.is_empty() {
            let _ = std::fs::write(manifest_cache_path(), &body);
        }
    }
    if body.is_empty() {
        body = read_file(&manifest_cache_path());
    }
    parse_manifest(s, &body);
}

async fn download_database(ctx: &Arc<Ctx>) -> bool {
    ctx.ui_run(|u| {
        let cl = u.global::<CarLib>();
        cl.set_downloading(true);
        cl.set_download_progress(0.0);
        cl.set_status(sstr("Downloading car database…"));
    });
    let archive = cache_dir().join("lovely-car-data.tar.gz");
    let pc = ctx.clone();
    let mut ok = http_download_file(TARBALL_URL, &archive, move |frac| {
        pc.ui_run(move |u| {
            u.global::<CarLib>()
                .set_download_progress((frac * 0.9) as f32)
        });
    })
    .await;
    if ok {
        let _ = std::fs::remove_dir_all(db_dir());
        ok = extract_tar_gz(&archive, &db_dir());
        let _ = std::fs::remove_file(&archive);
    }
    ctx.ui_run(move |u| {
        let cl = u.global::<CarLib>();
        cl.set_download_progress(1.0);
        cl.set_downloading(false);
        cl.set_status(sstr(if ok {
            "Database ready"
        } else {
            "Download failed (offline?)"
        }));
    });
    ok && data_root().join("manifest.json").exists()
}

pub fn refresh_database(ctx: &Arc<Ctx>) {
    if ctx.busy.swap(true, Ordering::SeqCst) {
        return;
    }
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let ok = download_database(&ctx).await;
        let sha = latest_commit().await;
        if ok && !sha.is_empty() {
            store_commit(&sha);
        }
        let body = if ok {
            read_file(&data_root().join("manifest.json"))
        } else {
            String::new()
        };
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = ctx2.lock();
            if !body.is_empty() {
                parse_manifest(&mut s, &body);
                push_classes(&u, &mut s);
                rebuild_filtered(&mut s);
            }
            push_car_results(&u, &s);
            drop(s);
            if !body.is_empty() {
                prefetch_game_data(&ctx2);
            }
        });
        ctx.busy.store(false, Ordering::SeqCst);
    });
}

pub fn sync_database_if_stale(ctx: &Arc<Ctx>) {
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let have_local = data_root().join("manifest.json").exists();
        let latest = latest_commit().await;
        if have_local && (latest.is_empty() || latest == cached_commit()) {
            return;
        }
        if !download_database(&ctx).await {
            return;
        }
        if !latest.is_empty() {
            store_commit(&latest);
        }
        let body = read_file(&data_root().join("manifest.json"));
        if body.is_empty() {
            return;
        }
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = ctx2.lock();
            parse_manifest(&mut s, &body);
            push_classes(&u, &mut s);
            rebuild_filtered(&mut s);
            push_car_results(&u, &s);
            u.global::<CarLib>().set_status(sstr("database updated"));
            drop(s);
            prefetch_game_data(&ctx2);
        });
    });
}

pub fn prefetch_game_data(ctx: &Arc<Ctx>) {
    let gen_id = ctx.car_gen.fetch_add(1, Ordering::SeqCst) + 1;
    let (work, _sim): (Vec<(usize, String)>, String) = {
        let s = ctx.lock();
        let sim = s.sim_of(s.game);
        let work = s
            .all_cars
            .iter()
            .enumerate()
            .filter(|(_, c)| c.sim == sim && c.redline == 0 && c.klass.is_empty())
            .map(|(i, c)| (i, c.path.clone()))
            .collect();
        (work, sim)
    };
    if work.is_empty() {
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = ctx2.lock();
            push_classes(&u, &mut s);
        });
        return;
    }
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let mut done = 0;
        for (idx, path) in work {
            if gen_id != ctx.car_gen.load(Ordering::SeqCst) {
                return;
            }
            let body = car_body_by_path(&path).await;
            let (mut rl, mut led_n, mut klass, mut cols) = (0, 0, String::new(), Vec::new());
            if let Ok(j) = serde_json::from_str::<Value>(&body) {
                klass = j
                    .get("carClass")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string();
                led_n = j.get("ledNumber").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
                rl = derive_redline(&j);
                cols = parse_car_led_colors(&j);
            }
            if gen_id != ctx.car_gen.load(Ordering::SeqCst) {
                return;
            }
            {
                let mut s = ctx.lock();
                if idx < s.all_cars.len() {
                    s.all_cars[idx].redline = rl;
                    s.all_cars[idx].led_n = led_n;
                    s.all_cars[idx].klass = klass;
                    s.all_cars[idx].led_cols = cols;
                }
            }
            done += 1;
            if done % 8 == 0 {
                let ctx2 = ctx.clone();
                ctx.ui_run(move |u| {
                    if gen_id == ctx2.car_gen.load(Ordering::SeqCst) {
                        let mut s = ctx2.lock();
                        push_classes(&u, &mut s);
                        push_car_results(&u, &s);
                    }
                });
            }
        }
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            if gen_id == ctx2.car_gen.load(Ordering::SeqCst) {
                let mut s = ctx2.lock();
                push_classes(&u, &mut s);
                rebuild_filtered(&mut s);
                push_car_results(&u, &s);
            }
        });
    });
}

pub fn select_car(ctx: &Arc<Ctx>, filtered_idx: i32) {
    let idx = {
        let s = ctx.lock();
        if filtered_idx < 0 || filtered_idx as usize >= s.filtered.len() {
            return;
        }
        s.filtered[filtered_idx as usize]
    };
    ctx.ui_run(move |u| u.global::<CarLib>().set_sel(filtered_idx));
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let path = {
            let s = ctx.lock();
            s.all_cars[idx].path.clone()
        };
        let body = car_body_by_path(&path).await;
        let (mut redline, mut led_n, mut klass, mut cols) = (0, 0, String::new(), Vec::new());
        if let Ok(j) = serde_json::from_str::<Value>(&body) {
            klass = j
                .get("carClass")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
            led_n = j.get("ledNumber").and_then(|x| x.as_i64()).unwrap_or(0) as i32;
            redline = derive_redline(&j);
            cols = parse_car_led_colors(&j);
        }
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = ctx2.lock();
            if idx < s.all_cars.len() {
                s.all_cars[idx].redline = redline;
                s.all_cars[idx].led_n = led_n;
                s.all_cars[idx].klass = klass;
                s.all_cars[idx].led_cols = cols;
            }
            push_car_results(&u, &s);
            u.global::<CarLib>().set_sel(filtered_idx);
        });
    });
}

pub fn set_active_car(ctx: &Arc<Ctx>, filtered_idx: i32) {
    let car = {
        let s = ctx.lock();
        if filtered_idx < 0 || filtered_idx as usize >= s.filtered.len() {
            return;
        }
        s.all_cars[s.filtered[filtered_idx as usize]].clone()
    };
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let body = car_body_by_path(&car.path).await;
        let mut minified = String::new();
        let mut redline = car.redline;
        let mut status = String::new();
        match serde_json::from_str::<Value>(&body) {
            Ok(j) => {
                minified = serde_json::to_string(&j).unwrap_or_default();
                if redline == 0 {
                    redline = derive_redline(&j);
                }
            }
            Err(_) => status = "car parse error".to_string(),
        }
        let connected = ctx.dash().connected();
        if !minified.is_empty() && connected {
            let ok = ctx.dash().push_car(&minified);
            status = if ok {
                format!("Sent {}", car.name)
            } else {
                "device rejected".to_string()
            };
        } else if !connected {
            status = "sent (offline cache) · connect to push".to_string();
        }
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = ctx2.lock();
            s.car_name = car.name.clone();
            s.car_game = car.sim.clone();
            s.car_id = car.id.clone();
            if redline > 0 {
                s.redline_rpm = redline;
                s.shift_custom = false;
            }
            if let Ok(j) = serde_json::from_str::<Value>(&body) {
                load_car_into_leds(&mut s, &j);
            }
            push_shift_scalars(&u, &s);
            push_led_model(&u, &s);
            push_car_results(&u, &s);
            crate::persist::save_active_car(&s);
            u.global::<CarLib>().set_status(sstr(&status));
        });
    });
}

/// Match `model` against the car library (built-ins seeded first, then the
/// downloaded manifest) and push the LED profile if found. Returns `false`
/// when nothing matched — callers can fall back to a decoder-keyed built-in
/// ([`apply_builtin_for_decoder`]) instead of leaving the generic bar.
///
/// `last_auto_model` is recorded ONLY on a successful match: a failed lookup
/// must stay retryable, because the library often isn't loaded yet on the
/// first tick (manifest fetch is async) — caching the failure would block the
/// profile for the whole session.
pub fn auto_apply_car_model(ctx: &Arc<Ctx>, s: &mut State, model: &str) -> bool {
    if model.is_empty() {
        return false;
    }
    if model == s.last_auto_model {
        return true; // already applied this model — not a failure, just a dedup
    }
    let sim = s.sim_of(s.game);
    // Match on the bare model — drop entry/livery suffixes like "#23:WEC" (rF2/LMU).
    let nm = norm_name(crate::util::clean_car_name(model));
    if nm.is_empty() {
        return false;
    }
    let mut best: i32 = -1;
    let mut best_score: usize = 0;
    for (i, c) in s.all_cars.iter().enumerate() {
        if c.sim != sim {
            continue;
        }
        let mut score = 0usize;
        for cand in [norm_name(&c.name), norm_name(&c.id)] {
            if cand.is_empty() {
                continue;
            }
            if cand == nm {
                score = score.max(1000);
            } else if cand.contains(&nm) {
                score = score.max(nm.len());
            } else if nm.contains(&cand) {
                score = score.max(cand.len());
            }
        }
        if score > best_score {
            best_score = score;
            best = i as i32;
        }
    }
    let model_owned = model.to_string();
    if best < 0 {
        ctx.ui_run(move |u| {
            u.global::<CarLib>()
                .set_status(sstr(&format!("No library match for '{model_owned}'")));
        });
        return false;
    }
    s.last_auto_model = model.to_string();
    let car = s.all_cars[best as usize].clone();
    spawn_apply_profile(ctx, car, false);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_profiles_are_valid_and_sane() {
        for b in BUILTIN_CARS {
            let j: Value = serde_json::from_str(b.json)
                .unwrap_or_else(|e| panic!("cars/{}.json must be valid JSON: {e}", b.key));
            let redline = derive_redline(&j);
            assert!(
                redline > 4000,
                "cars/{}.json: implausible redline {redline}",
                b.key
            );
            let cols = parse_car_led_colors(&j);
            assert_eq!(
                cols.len(),
                12,
                "cars/{}.json: colors clamp to the strip",
                b.key
            );
            assert!(
                cols.iter().any(|&c| c != 0),
                "cars/{}.json: all-blank colors",
                b.key
            );
            assert!(!b.sims.is_empty(), "cars/{}.json: no sims listed", b.key);
        }
    }

    #[test]
    fn builtin_items_seed_every_sim() {
        let items = builtin_car_items();
        // f1.json is listed under both F1 sims, keyed by the decoder's real name.
        assert!(items
            .iter()
            .any(|c| c.sim == "f12024" && c.path == "builtin:f1"));
        assert!(items
            .iter()
            .any(|c| c.sim == "f12025" && c.path == "builtin:f1"));
        assert!(BUILTIN_CARS.iter().any(|b| b.decoder == pith_sim::f1::NAME));
        assert_eq!(builtin_body("builtin:f1"), Some(BUILTIN_CARS[0].json));
        assert_eq!(builtin_body("f12025/220.json"), None);
    }
}
