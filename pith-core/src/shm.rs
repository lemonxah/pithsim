//! Sim shared-memory struct parsers (byte-compatible with the games' Windows
//! shared memory). Shared by the dashboard's native `/dev/shm` reader and the
//! in-prefix shim/bridge tool, so the offsets live in exactly one place.
//!
//! Each parser takes a raw shared-memory snapshot and returns a [`Telemetry`].
//! Offsets verified against: AC/ACC `SPageFilePhysics`, RaceRoom `r3e_shared`
//! (`r3e.h`), rFactor 2 / LMU `rF2Telemetry`/`rF2Scoring` (`rF2State.h`,
//! `#pragma pack(4)`).

use crate::le;
use crate::simhub::Telemetry;

/// AC / ACC / AC EVO `SPageFilePhysics` (4-byte naturally-aligned fields). The
/// head (`gas@4 brake@8 fuel@12 gear@16 rpms@20 speedKmh@28`) and the fields up
/// to `abs@252` are common to original AC and ACC; the richer fields past 252
/// (clutch/brake temps/ignition/water) are ACC-only, so they're gated on the
/// full 800-byte ACC block to avoid mis-reading the shorter original-AC struct.
pub fn parse_ac_physics(b: &[u8]) -> Option<Telemetry> {
    if b.len() < 32 || le::i32(b, 0) == 0 {
        return None; // too short, or packetId 0 = no data yet
    }
    let mut t = Telemetry::idle();
    t.throttle = (le::f32(b, 4) * 100.0).round().clamp(0.0, 100.0) as i32;
    t.brake = (le::f32(b, 8) * 100.0).round().clamp(0.0, 100.0) as i32;
    t.fuel_dl = (le::f32(b, 12) * 10.0).round().max(0.0) as i32;
    // gear: 0 = reverse, 1 = neutral, 2 = 1st … → numeric = raw - 1.
    t.gear = le::gear_byte(le::i32(b, 16) - 1);
    t.rpm = le::i32(b, 20).max(0);
    t.steer = (le::f32(b, 24) * 100.0).round().clamp(-100.0, 100.0) as i32; // steerAngle -1..1
    t.speed_kmh = le::f32(b, 28).round().max(0.0) as i32;

    // ---- common region (≤252): valid for both original AC and ACC ----
    if b.len() >= 256 {
        // wheelsPressure[4] @88 (kPa), tyreCoreTemperature[4] @152 (°C); FL,FR,RL,RR.
        t.tp_fl = le::f32(b, 88).round() as i32;
        t.tp_fr = le::f32(b, 92).round() as i32;
        t.tp_rl = le::f32(b, 96).round() as i32;
        t.tp_rr = le::f32(b, 100).round() as i32;
        set_tyre(&mut t,
            le::f32(b, 152).round() as i32, le::f32(b, 156).round() as i32,
            le::f32(b, 160).round() as i32, le::f32(b, 164).round() as i32);
        t.pit_limiter = le::i32(b, 248); // pitLimiterOn
        t.tc_active = (le::f32(b, 204) > 0.0) as i32; // live TC intervention
        t.abs_active = (le::f32(b, 252) > 0.0) as i32; // live ABS intervention
    }
    // ---- ACC-only extended region (full 800-byte block) ----
    if b.len() >= 800 {
        t.clutch = (le::f32(b, 364) * 100.0).round().clamp(0.0, 100.0) as i32;
        // brakeTemp[4] @348 (°C), FL,FR,RL,RR.
        t.bt_fl = le::f32(b, 348).round() as i32;
        t.bt_fr = le::f32(b, 352).round() as i32;
        t.bt_rl = le::f32(b, 356).round() as i32;
        t.bt_rr = le::f32(b, 360).round() as i32;
        t.brake_bias_x10 = (le::f32(b, 564) * 1000.0).round() as i32; // 0..1 → x10%
        t.water_c = le::f32(b, 712).round() as i32;
        t.ignition = le::i32(b, 772); // ignitionOn
    }
    Some(t)
}

/// Merge ACC `SPageFileGraphic` (`acpmf_graphics`) fields into a physics-derived
/// `Telemetry` — this is the ONLY source of wipers / lights / session flag.
pub fn apply_acc_graphics(t: &mut Telemetry, g: &[u8]) {
    if g.len() < 1308 {
        return; // need through wiperLV@1304
    }
    t.laps_done = le::i32(g, 132).max(0); // completedLaps
    t.position = le::i32(g, 136).max(0);
    // Lap times (ms) straight from the graphics page — game data, so the current
    // lap resets at the line (no wall-clock fallback). ACC parks "no time" at a
    // huge sentinel, so reject anything past 30 min.
    let lap = |o: usize| {
        let v = le::i32(g, o);
        if (0..30 * 60 * 1000).contains(&v) {
            v
        } else {
            0
        }
    };
    t.cur_lap_ms = lap(140); // iCurrentTime
    t.last_lap_ms = lap(144); // iLastTime
    t.best_lap_ms = lap(148); // iBestTime
    t.track_pct = (le::f32(g, 248) * 1000.0).clamp(0.0, 1000.0) as i32; // normalizedCarPos
    t.tc = le::i32(g, 1268); // TC level
    t.abs = le::i32(g, 1280); // ABS level
    t.headlights = (le::i32(g, 1296) > 0) as i32; // lightsStage (0 off)
    t.wipers = le::i32(g, 1304).max(0); // wiperLV
    t.flag = map_acc_flag(le::i32(g, 1224)); // AC_FLAG_TYPE → our flag code
}

/// AC_FLAG_TYPE → our flag code (0 none,1 green,2 yellow,3 blue,4 white,
/// 5 checkered,6 black).
fn map_acc_flag(f: i32) -> i32 {
    match f {
        1 => 3, // blue
        2 => 2, // yellow
        3 => 6, // black
        4 => 4, // white
        5 => 5, // checkered
        7 => 1, // green (ACC)
        _ => 0, // none / penalty / orange
    }
}

/// Car model + track from the AC / ACC `SPageFileStatic` page: `carModel`
/// (UTF-16 `wchar[33]`) @68, `track` @134. (AC EVO uses a different layout — do
/// not use this for `acevo_pmf_static`.)
pub fn ac_static_identity(s: &[u8]) -> (Option<String>, Option<String>) {
    if s.len() < 200 {
        return (None, None);
    }
    (non_empty(utf16_str(s, 68, 33)), non_empty(utf16_str(s, 134, 33)))
}

/// Merge rF2/LMU `$rFactor2SMMP_Extended$` aid levels into a telemetry snapshot —
/// rF2 keeps TC/ABS *levels* only here, not in Telemetry/Scoring. File layout:
/// 8-byte version block + `rF2Extended` (mPhysics @ +16), so `rF2PhysicsOptions`
/// `mTractionControl` @ file 24, `mAntiLockBrakes` @ 25. (Written at session start
/// and persisted, not per-frame.)
pub fn apply_rf2_extended(t: &mut Telemetry, ext: &[u8]) {
    if ext.len() < 26 {
        return;
    }
    t.tc = ext[24] as i32; // mTractionControl (0..3)
    t.abs = ext[25] as i32; // mAntiLockBrakes (0..2)
}

/// Overlay **LMU's NATIVE shared memory** (`LMU_Data`) onto an rF2-plugin-derived
/// snapshot. LMU 1.3+ writes its own map (separate from TheIronWolf's
/// `$rFactor2SMMP_*$`) carrying the data the rF2 plugin lacks LIVE: in-car TC/ABS
/// **levels** + activation, the game's own lap delta, battery SoC, wiper state. The
/// rF2 plugin's `mPhysics` only has the static assist *setting* — this is what the
/// in-game HUD / SimHub actually read. Base telemetry (gear/rpm/fuel/temps) is left
/// to the rF2 plugin. Layout per S397's `SharedMemoryInterface.hpp` (mirrored by
/// TinyPedal/pyLMUSharedMemory): telemetry@128464, playerVehicleIdx@128465,
/// telemInfo[]@128468 stride 1888; per-entry offsets below. `#pragma pack(4)`.
pub fn apply_lmu_native(t: &mut Telemetry, b: &[u8]) {
    const PLAYER_IDX: usize = 128465;
    const ENTRIES: usize = 128468;
    const STRIDE: usize = 1888;
    if b.len() <= PLAYER_IDX || b[128464] == 0 {
        return; // too short / no active vehicles
    }
    let idx = b[PLAYER_IDX] as usize;
    if idx >= 104 {
        return;
    }
    let base = ENTRIES + idx * STRIDE;
    if b.len() < base + STRIDE {
        return;
    }
    // Live in-car aid LEVELS + activation flags (the values on the wheel HUD).
    t.abs_active = (b[base + 746] != 0) as i32; // mABSActive
    t.tc_active = (b[base + 747] != 0) as i32; // mTCActive
    t.wipers = b[base + 749] as i32; // mWiperState (0 off,1 auto,2 slow,3 fast)
    t.tc = b[base + 750] as i32; // mTC (current level)
    t.abs = b[base + 756] as i32; // mABS (current level)
    // The game's own lap delta (mDeltaBest, double seconds, neg = ahead) → 0.1 ms.
    let dbest = le::f64(b, base + 696);
    if dbest.is_finite() && dbest.abs() < 600.0 {
        t.delta_ms = (dbest * 10000.0).round() as i32;
    }
    // Battery state of charge (mBatteryChargeFraction, double 0..1).
    let soc = le::f64(b, base + 704);
    if soc.is_finite() && (0.0..=1.0).contains(&soc) {
        t.battery_pct = (soc * 1000.0).round() as i32;
    }
}

/// Car model + track from the rF2 / LMU scoring buffer: `mTrackName` (ASCII) at
/// file offset 16; the player's `mVehicleName` at `560 + i*584 + 36` (player
/// element found via `mIsPlayer@196`). Plain NUL-terminated `char`.
pub fn rf2_identity(_telem: &[u8], scoring: &[u8]) -> (Option<String>, Option<String>) {
    // mTrackName is the first field of rF2ScoringInfo, which starts at file
    // offset 12 (8-byte version block + 4-byte mBytesUpdatedHint, no pad).
    let track = if scoring.len() >= 12 + 64 {
        ascii_str(scoring, 12, 64)
    } else {
        String::new()
    };
    let mut car = String::new();
    if scoring.len() >= 120 {
        let n = (le::i32(scoring, 116).max(0) as usize).min(128);
        for i in 0..n {
            let base = 560 + i * 584;
            if scoring.len() < base + 584 {
                break;
            }
            if scoring[base + 196] != 0 {
                car = ascii_str(scoring, base + 36, 64);
                break;
            }
        }
    }
    (non_empty(car), non_empty(track))
}

/// Decode a NUL-terminated UTF-16LE string of up to `max_chars` from offset `o`.
fn utf16_str(b: &[u8], o: usize, max_chars: usize) -> String {
    let units: Vec<u16> = (0..max_chars)
        .map(|i| o + i * 2)
        .take_while(|&p| p + 1 < b.len())
        .map(|p| u16::from_le_bytes([b[p], b[p + 1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units).trim().to_string()
}

/// Decode a NUL-terminated ASCII/UTF-8 string of up to `max` bytes from offset `o`.
fn ascii_str(b: &[u8], o: usize, max: usize) -> String {
    let end = (o + max).min(b.len());
    let slice = &b[o..end];
    let n = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
    String::from_utf8_lossy(&slice[..n]).trim().to_string()
}

fn non_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn set_tyre(t: &mut Telemetry, fl: i32, fr: i32, rl: i32, rr: i32) {
    let (fl, fr, rl, rr) = (fl * 10, fr * 10, rl * 10, rr * 10); // °C → 0.1°C
    t.tt_fl_i = fl; t.tt_fl_m = fl; t.tt_fl_o = fl;
    t.tt_fr_i = fr; t.tt_fr_m = fr; t.tt_fr_o = fr;
    t.tt_rl_i = rl; t.tt_rl_m = rl; t.tt_rl_o = rl;
    t.tt_rr_i = rr; t.tt_rr_m = rr; t.tt_rr_o = rr;
}

/// RaceRoom `r3e_shared` (fully packed; absolute offsets; single local car).
pub fn parse_r3e(b: &[u8]) -> Option<Telemetry> {
    if b.len() < 1520 {
        return None;
    }
    let rps = le::f32(b, 1396); // engine_rps, rad/s
    if !rps.is_finite() || rps < 0.0 {
        return None;
    }
    let rpm = |r: f32| (r * 9.549297).round().max(0.0) as i32; // rad/s → rpm
    let pct = |o: usize| {
        let v = le::f32(b, o);
        if v < 0.0 {
            0
        } else {
            (v * 100.0).round() as i32
        } // -1 = n/a
    };
    let lap_ms = |o: usize| {
        let s = le::f32(b, o);
        if s < 0.0 {
            0
        } else {
            (s * 1000.0).round() as i32
        }
    };
    let mut t = Telemetry::idle();
    t.speed_kmh = (le::f32(b, 1392) * 3.6).round().max(0.0) as i32;
    t.rpm = rpm(rps);
    t.max_rpm = rpm(le::f32(b, 1400));
    t.shift_rpm = rpm(le::f32(b, 1404)); // upshift_rps
    let raw_gear = le::i32(b, 1408); // -2 n/a, -1 R, 0 N, 1+
    t.gear = le::gear_byte(if raw_gear < -1 { 0 } else { raw_gear });
    t.fuel_dl = (le::f32(b, 1456) * 10.0).round().max(0.0) as i32;
    t.fuel_cap_dl = (le::f32(b, 1460) * 10.0).round().max(0.0) as i32;
    t.throttle = pct(1500);
    t.brake = pct(1508);
    t.clutch = pct(1516);
    t.position = le::i32(b, 988).max(0);
    t.laps_done = le::i32(b, 1028).max(0);
    t.cur_lap_ms = lap_ms(1100);
    t.best_lap_ms = lap_ms(1068);
    t.last_lap_ms = lap_ms(1084);
    // Extra car state (later in the struct).
    if b.len() >= 1624 {
        t.water_c = le::f32(b, 1480).round() as i32; // engine_temp
        t.oil_c = le::f32(b, 1484).round() as i32; // engine_oil_temp
        t.oil_press_x10 = (le::f32(b, 1492) * 10.0).round() as i32; // oil pressure
        t.pit_limiter = (le::i32(b, 1572) == 1) as i32;
        t.headlights = (le::i32(b, 1620) > 0) as i32;
        // aid_settings (abs@1536, tc@1540): -1 N/A, 0 off, 1 on, 5 = active now.
        let abs_aid = le::i32(b, 1536);
        let tc_aid = le::i32(b, 1540);
        t.abs = if abs_aid == 5 { 1 } else { abs_aid.max(0) };
        t.tc = if tc_aid == 5 { 1 } else { tc_aid.max(0) };
        t.abs_active = (abs_aid == 5) as i32;
        t.tc_active = (tc_aid == 5) as i32;
    }
    Some(t)
}

/// rF2 / LMU telemetry. Matches the player car by `mID` across the telemetry and
/// scoring buffers (the arrays are not index-aligned). `#pragma pack(4)`.
pub fn parse_rf2(telem: &[u8], scoring: &[u8]) -> Option<Telemetry> {
    const TELEM_BASE: usize = 16;
    const TELEM_STRIDE: usize = 1888;
    if telem.len() < TELEM_BASE + TELEM_STRIDE {
        return None;
    }
    let tn = (le::i32(telem, 12).max(0) as usize).min(128);

    // Find the player in scoring (mNumVehicles@116, vehicles@560 stride 584,
    // mID@0, mIsPlayer@196): capture both the mID (to match telemetry) and the
    // scoring element base (for lap times / position / sectors / flag below).
    let (player_id, sbase) = (|| {
        if scoring.len() < 122 {
            return (None, None);
        }
        let n = (le::i32(scoring, 116).max(0) as usize).min(128);
        for i in 0..n {
            let base = 560 + i * 584;
            if scoring.len() < base + 584 {
                break;
            }
            if scoring[base + 196] != 0 {
                return (Some(le::i32(scoring, base)), Some(base));
            }
        }
        (None, None)
    })();

    // Find the matching telemetry element; fall back to index 0.
    let mut idx = 0usize;
    if let Some(pid) = player_id {
        for j in 0..tn {
            let base = TELEM_BASE + j * TELEM_STRIDE;
            if telem.len() < base + TELEM_STRIDE {
                break;
            }
            if le::i32(telem, base) == pid {
                idx = j;
                break;
            }
        }
    }
    let base = TELEM_BASE + idx * TELEM_STRIDE;
    if telem.len() < base + TELEM_STRIDE {
        return None;
    }

    let mut t = Telemetry::idle();
    t.gear = le::gear_byte(le::i32(telem, base + 352)); // -1=R, 0=N, 1+
    t.rpm = le::f64(telem, base + 356).round().max(0.0) as i32;
    t.max_rpm = le::f64(telem, base + 532).round().max(0.0) as i32;
    t.shift_rpm = t.max_rpm;
    let (vx, vy, vz) = (
        le::f64(telem, base + 184),
        le::f64(telem, base + 192),
        le::f64(telem, base + 200),
    );
    t.speed_kmh = ((vx * vx + vy * vy + vz * vz).sqrt() * 3.6).round().max(0.0) as i32;
    t.fuel_dl = (le::f64(telem, base + 524) * 10.0).round().max(0.0) as i32;
    t.throttle = (le::f64(telem, base + 388) * 100.0).round() as i32;
    t.brake = (le::f64(telem, base + 396) * 100.0).round() as i32;
    // mUnfilteredSteering @404 (-1..1).
    t.steer = (le::f64(telem, base + 404) * 100.0).round().clamp(-100.0, 100.0) as i32;
    t.clutch = (le::f64(telem, base + 412) * 100.0).round() as i32;
    t.water_c = le::f64(telem, base + 364).round() as i32;
    t.oil_c = le::f64(telem, base + 372).round() as i32;
    t.laps_done = le::i32(telem, base + 20);
    // rF2 has no current-lap-time field ("instantly becomes last"); derive it from
    // mElapsedTime@12 − mLapStartET@24 (both doubles, seconds).
    let cur = le::f64(telem, base + 12) - le::f64(telem, base + 24);
    t.cur_lap_ms = (cur * 1000.0).round().max(0.0) as i32;
    t.fuel_cap_dl = (le::f64(telem, base + 608) * 10.0).round().max(0.0) as i32;
    // Status bytes: mHeadlights@543, mSpeedLimiter@604, mIgnitionStarter@619.
    t.headlights = (telem[base + 543] != 0) as i32;
    t.pit_limiter = (telem[base + 604] != 0) as i32;
    t.ignition = (telem[base + 619] != 0) as i32;
    // Hybrid / electric boost (LMU hypercars & LMDh): battery state-of-charge
    // (mBatteryChargeFraction@696, 0..1 → 0..100.0%) and the boost-motor state
    // (mElectricBoostMotorState@736: 0 unavailable, 1 inactive, 2 propulsion,
    // 3 regeneration).
    let batt = le::f64(telem, base + 696);
    if batt.is_finite() {
        t.battery_pct = (batt * 1000.0).round().clamp(0.0, 1000.0) as i32;
    }
    t.ers_state = telem[base + 736] as i32;
    // Per-wheel (FL,FR,RL,RR @ base+848 stride 260). Tyre temps in 0.1°C:
    // **carcass** (mTireCarcassTemperature@204, Kelvin) is the stable core temp the
    // in-game HUD shows → the headline `_m`; the surface tread L/R (mTemperature
    // [0]@128 / [2]@144) give the inner/outer gradient. Brake temp@+24 (°C),
    // pressure@+120 (kPa), wear (mWear@152, 0..1 fraction remaining) → %.
    let k2dc = |k: f64| ((k - 273.15) * 10.0).round() as i32; // Kelvin → 0.1°C
    for (i, (ti, tm, to, bt, tp, tw)) in [
        (&mut t.tt_fl_i, &mut t.tt_fl_m, &mut t.tt_fl_o, &mut t.bt_fl, &mut t.tp_fl, &mut t.tw_fl),
        (&mut t.tt_fr_i, &mut t.tt_fr_m, &mut t.tt_fr_o, &mut t.bt_fr, &mut t.tp_fr, &mut t.tw_fr),
        (&mut t.tt_rl_i, &mut t.tt_rl_m, &mut t.tt_rl_o, &mut t.bt_rl, &mut t.tp_rl, &mut t.tw_rl),
        (&mut t.tt_rr_i, &mut t.tt_rr_m, &mut t.tt_rr_o, &mut t.bt_rr, &mut t.tp_rr, &mut t.tw_rr),
    ]
    .into_iter()
    .enumerate()
    {
        let w = base + 848 + i * 260;
        *ti = k2dc(le::f64(telem, w + 128)); // surface tread left
        *tm = k2dc(le::f64(telem, w + 204)); // carcass core (headline)
        *to = k2dc(le::f64(telem, w + 144)); // surface tread right
        *bt = le::f64(telem, w + 24).round() as i32;
        *tp = le::f64(telem, w + 120).round() as i32;
        *tw = (le::f64(telem, w + 152) * 100.0).round().clamp(0.0, 100.0) as i32;
    }

    // ---- Scoring-buffer fields for the player car (not in telemetry): position,
    // lap/sector times, track %, session flag, field size. Times are doubles in
    // seconds (negative = none); header base is 12, vehicle base 560 stride 584.
    if let Some(sb) = sbase {
        if scoring.len() >= sb + 584 {
            let ms = |o: usize| {
                let s = le::f64(scoring, sb + o);
                if s >= 0.0 { (s * 1000.0).round() as i32 } else { 0 }
            };
            t.position = scoring[sb + 199] as i32; // mPlace (1-based)
            t.best_lap_ms = ms(144);
            t.last_lap_ms = ms(168);
            let s1 = le::f64(scoring, sb + 176); // mCurSector1
            let s2c = le::f64(scoring, sb + 184); // mCurSector2 (cumulative)
            if s1 >= 0.0 {
                t.s1_ms = (s1 * 1000.0).round() as i32;
            }
            if s1 >= 0.0 && s2c >= s1 {
                t.s2_ms = ((s2c - s1) * 1000.0).round() as i32;
            }
            // track %: vehicle mLapDist@104 / track length (scoringInfo.mLapDist@100).
            // The SCORING buffer updates slower than the high-rate TELEMETRY buffer
            // the current-lap time comes from, so the raw position lags the clock and
            // the lap delta (time-at-position vs a reference) comes out noisy/wrong.
            // Extrapolate the position forward by speed × (telemetry clock − scoring
            // clock) so position and time refer to the SAME instant — the way a
            // consistent delta needs them. (mCurrentET@80 and mElapsedTime@base+12 are
            // both session ET in seconds.)
            let lapdist = le::f64(scoring, sb + 104);
            let tracklen = le::f64(scoring, 100);
            if tracklen > 1.0 && lapdist >= 0.0 {
                let scoring_et = le::f64(scoring, 80); // scoringInfo.mCurrentET
                let telem_et = le::f64(telem, base + 12); // mElapsedTime (fresh)
                let lag = (telem_et - scoring_et).clamp(0.0, 1.0); // buffer desync, s
                let pos = lapdist + (t.speed_kmh as f64 / 3.6) * lag;
                t.track_pct = ((pos / tracklen) * 1000.0).clamp(0.0, 1000.0) as i32;
            }
            // flag: session phase (file 120) + yellow (121) + per-car mFlag (sb+504).
            let phase = scoring[120];
            let yellow = scoring[121] as i8;
            let carflag = scoring[sb + 504];
            t.flag = if phase == 8 {
                5 // checkered (session over)
            } else if phase == 6 || yellow > 0 {
                2 // yellow / full-course yellow
            } else if carflag == 6 {
                3 // blue
            } else if phase == 5 {
                1 // green
            } else {
                0
            };
            t.field_size = le::i32(scoring, 116); // mNumVehicles
            t.total_laps = le::i32(scoring, 96).max(0); // mMaxLaps
        }
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ac_physics_head() {
        let mut b = vec![0u8; 64];
        b[0..4].copy_from_slice(&123i32.to_le_bytes());
        b[4..8].copy_from_slice(&1.0f32.to_le_bytes());
        b[12..16].copy_from_slice(&42.5f32.to_le_bytes());
        b[16..20].copy_from_slice(&4i32.to_le_bytes());
        b[20..24].copy_from_slice(&7600i32.to_le_bytes());
        b[28..32].copy_from_slice(&188.0f32.to_le_bytes());
        let t = parse_ac_physics(&b).unwrap();
        assert_eq!(t.rpm, 7600);
        assert_eq!(t.gear, b'3');
        assert_eq!(t.speed_kmh, 188);
        assert_eq!(t.throttle, 100);
        assert_eq!(t.fuel_dl, 425);
    }

    #[test]
    fn parses_r3e() {
        let mut b = vec![0u8; 1700]; // ≥1624 so the extended block (TC/ABS) is read
        b[1392..1396].copy_from_slice(&50.0f32.to_le_bytes());
        b[1396..1400].copy_from_slice(&785.40f32.to_le_bytes());
        b[1408..1412].copy_from_slice(&3i32.to_le_bytes());
        b[1500..1504].copy_from_slice(&1.0f32.to_le_bytes());
        b[1536..1540].copy_from_slice(&1i32.to_le_bytes()); // abs aid = on
        b[1540..1544].copy_from_slice(&5i32.to_le_bytes()); // tc aid = active
        let t = parse_r3e(&b).unwrap();
        assert_eq!(t.speed_kmh, 180);
        assert_eq!(t.rpm, 7500);
        assert_eq!(t.gear, b'3');
        assert_eq!(t.throttle, 100);
        assert_eq!(t.abs, 1);
        assert_eq!(t.tc, 1); // 5 (active) normalises to on
        assert_eq!(t.tc_active, 1);
        assert_eq!(t.abs_active, 0);
    }

    #[test]
    fn rf2_extended_tc_abs() {
        let mut ext = vec![0u8; 64];
        ext[24] = 3; // mTractionControl
        ext[25] = 2; // mAntiLockBrakes
        let mut t = Telemetry::idle();
        apply_rf2_extended(&mut t, &ext);
        assert_eq!(t.tc, 3);
        assert_eq!(t.abs, 2);
    }

    #[test]
    fn ac_static_car_and_track() {
        let mut s = vec![0u8; 420];
        // carModel "ferrari_488_gt3" @68 (UTF-16LE)
        for (i, u) in "ferrari_488_gt3".encode_utf16().enumerate() {
            s[68 + i * 2..70 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        for (i, u) in "spa".encode_utf16().enumerate() {
            s[134 + i * 2..136 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        let (car, track) = ac_static_identity(&s);
        assert_eq!(car.as_deref(), Some("ferrari_488_gt3"));
        assert_eq!(track.as_deref(), Some("spa"));
    }

    #[test]
    fn rf2_car_and_track() {
        let mut s = vec![0u8; 560 + 584];
        // mTrackName @ file 12 (header base 12)
        s[12..17].copy_from_slice(b"Sebr\0");
        s[116..120].copy_from_slice(&1i32.to_le_bytes()); // mNumVehicles
        s[560 + 196] = 1; // mIsPlayer
        s[560 + 36..560 + 36 + 9].copy_from_slice(b"BMW M4\0\0\0"); // mVehicleName@36
        let (car, track) = rf2_identity(&[], &s);
        assert_eq!(car.as_deref(), Some("BMW M4"));
        assert_eq!(track.as_deref(), Some("Sebr"));
    }

    #[test]
    fn parses_rf2_with_player_match() {
        let mut s = vec![0u8; 560 + 584];
        s[116..120].copy_from_slice(&1i32.to_le_bytes());
        s[560..564].copy_from_slice(&42i32.to_le_bytes());
        s[560 + 196] = 1;
        let mut t = vec![0u8; 16 + 2 * 1888];
        t[12..16].copy_from_slice(&2i32.to_le_bytes());
        t[16..20].copy_from_slice(&7i32.to_le_bytes());
        let p = 16 + 1888;
        t[p..p + 4].copy_from_slice(&42i32.to_le_bytes());
        t[p + 352..p + 356].copy_from_slice(&3i32.to_le_bytes());
        t[p + 356..p + 364].copy_from_slice(&7200.0f64.to_le_bytes());
        t[p + 532..p + 540].copy_from_slice(&8000.0f64.to_le_bytes());
        t[p + 184..p + 192].copy_from_slice(&30.0f64.to_le_bytes());
        let out = parse_rf2(&t, &s).unwrap();
        assert_eq!(out.gear, b'3');
        assert_eq!(out.rpm, 7200);
        assert_eq!(out.max_rpm, 8000);
        assert_eq!(out.speed_kmh, 108);
    }
}
