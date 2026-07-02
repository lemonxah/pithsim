//! Sim shared-memory struct parsers (byte-compatible with the games' Windows
//! shared memory). Shared by the dashboard's native `/dev/shm` reader and the
//! in-prefix shim/bridge tool, so the offsets live in exactly one place.
//!
//! Each parser takes a raw shared-memory snapshot and returns a [`Telemetry`].
//! Offsets verified against: AC/ACC `SPageFilePhysics`, RaceRoom `r3e_shared`
//! (`r3e.h`), rFactor 2 / LMU `rF2Telemetry`/`rF2Scoring` (`rF2State.h`,
//! `#pragma pack(4)`).

use pith_core::le;
use pith_core::simhub::Telemetry;

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
        // wheelsPressure[4] @88 is in PSI (per the official shared-memory doc;
        // ~27 in game) — convert to kPa so `tp_*` keeps one unit across sims
        // (the rF2 path stores 0.1 kPa). tyreCoreTemperature[4] @152 (°C).
        // Order FL,FR,RL,RR.
        const PSI_KPA: f32 = 6.894_76;
        t.tp_fl = (le::f32(b, 88) * PSI_KPA * 10.0).round() as i32;
        t.tp_fr = (le::f32(b, 92) * PSI_KPA * 10.0).round() as i32;
        t.tp_rl = (le::f32(b, 96) * PSI_KPA * 10.0).round() as i32;
        t.tp_rr = (le::f32(b, 100) * PSI_KPA * 10.0).round() as i32;
        set_tyre(
            &mut t,
            le::f32(b, 152).round() as i32,
            le::f32(b, 156).round() as i32,
            le::f32(b, 160).round() as i32,
            le::f32(b, 164).round() as i32,
        );
        t.pit_limiter = le::i32(b, 248); // pitLimiterOn
        t.tc_active = (le::f32(b, 204) > 0.0) as i32; // live TC intervention
        t.abs_active = (le::f32(b, 252) > 0.0) as i32; // live ABS intervention
    }
    // ---- ACC-only extended region (full 800-byte block) ----
    if b.len() >= 800 {
        t.clutch = (le::f32(b, 364) * 100.0).round().clamp(0.0, 100.0) as i32;
        // brakeTemp[4] @348 (°C→0.1°C), FL,FR,RL,RR.
        t.bt_fl = (le::f32(b, 348) * 10.0).round() as i32;
        t.bt_fr = (le::f32(b, 352) * 10.0).round() as i32;
        t.bt_rl = (le::f32(b, 356) * 10.0).round() as i32;
        t.bt_rr = (le::f32(b, 360) * 10.0).round() as i32;
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
/// not use this for `acevo_pmf_static`; see [`acevo_identity`].)
pub fn ac_static_identity(s: &[u8]) -> (Option<String>, Option<String>) {
    if s.len() < 200 {
        return (None, None);
    }
    (
        non_empty(utf16_str(s, 68, 33)),
        non_empty(utf16_str(s, 134, 33)),
    )
}

/// Car model + track from AC EVO's pages. Unlike AC/ACC, EVO strings are
/// narrow 8-bit chars (pack(4)) and the CAR lives on the GRAPHICS page:
/// `SPageFileGraphicEvo.car_model` `char[33]` @3086 (after driver name @3020 /
/// surname @3053); `SPageFileStaticEvo.track` `char[33]` @136 (+ config @169).
/// Layout cross-verified between dSyncro/acevo-shared-memory and CrewChiefV4's
/// `ACEData.cs`. EVO is early-access — Kunos has changed structs between
/// builds — so both reads are length-gated and empty strings return `None`.
pub fn acevo_identity(static_b: &[u8], graphics_b: &[u8]) -> (Option<String>, Option<String>) {
    let car = if graphics_b.len() >= 3086 + 33 {
        non_empty(ascii_str(graphics_b, 3086, 33))
    } else {
        None
    };
    let track = if static_b.len() >= 136 + 33 {
        non_empty(ascii_str(static_b, 136, 33))
    } else {
        None
    };
    (car, track)
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
    t.tc_slip = b[base + 752] as i32; // mTCSlip
    t.tc_cut = b[base + 754] as i32; // mTCCut
    t.abs = b[base + 756] as i32; // mABS (current level)
                                  // Virtual Energy (mVirtualEnergy@776, f32 0..1) — present on energy-regulated
                                  // cars (Hypercar/LMDh). When valid, mark fuel as VE so it's shown as a %.
    let ve = le::f32(b, base + 776);
    if ve.is_finite() && (0.0..=1.0).contains(&ve) && ve > 0.0 {
        t.virtual_energy = (ve as f64 * 1000.0).round() as i32;
        t.fuel_is_ve = 1;
    }
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
    // Tyre temps: LMU's in-game HUD shows the INNER-LAYER temperature (steady,
    // ~bulk rubber), not the raw contact-surface tread that parse_rf2 mapped —
    // surface swings ±25 °C between a straight and a braking zone, which is why
    // the dash disagreed so violently with the (stable) HUD number. The native
    // entry mirrors the rF2 wheel layout, and unlike the compat plugin buffer
    // (whose carcass reads ~ambient in LMU) it's written by the game itself.
    // Every value is gated on a plausible Kelvin range so a layout surprise
    // degrades to the old behaviour instead of garbage.
    if let Some(v) = crate::rf2::VehicleTelem::at(b, base) {
        let ok = |k: f64| k.is_finite() && (200.0..500.0).contains(&k);
        let dc = |k: f64| ((k - 273.15) * 10.0).round() as i32; // K → 0.1 °C
        let set =
            |i: usize, zi: &mut i32, zm: &mut i32, zo: &mut i32, avg: &mut i32, carc: &mut i32| {
                let w = v.wheel(i);
                let (ki, km, ko) = (w.inner_temp_k(0), w.inner_temp_k(1), w.inner_temp_k(2));
                if ok(ki) && ok(km) && ok(ko) {
                    *zi = dc(ki);
                    *zm = dc(km);
                    *zo = dc(ko);
                    *avg = dc((ki + km + ko) / 3.0);
                }
                let kc = w.carcass_temp_k();
                if ok(kc) {
                    *carc = dc(kc);
                }
            };
        set(
            0,
            &mut t.tt_fl_i,
            &mut t.tt_fl_m,
            &mut t.tt_fl_o,
            &mut t.tt_avg_fl,
            &mut t.tt_carc_fl,
        );
        set(
            1,
            &mut t.tt_fr_i,
            &mut t.tt_fr_m,
            &mut t.tt_fr_o,
            &mut t.tt_avg_fr,
            &mut t.tt_carc_fr,
        );
        set(
            2,
            &mut t.tt_rl_i,
            &mut t.tt_rl_m,
            &mut t.tt_rl_o,
            &mut t.tt_avg_rl,
            &mut t.tt_carc_rl,
        );
        set(
            3,
            &mut t.tt_rr_i,
            &mut t.tt_rr_m,
            &mut t.tt_rr_o,
            &mut t.tt_avg_rr,
            &mut t.tt_carc_rr,
        );
    }
}

/// LOGGING ONLY (no logic change). Dumps the raw rF2/LMU fields behind the two
/// LMU reports — carcass temp reading ~ambient, and missing race flags — so we
/// can see in-game whether LMU actually populates them and where the real values
/// live. Player lookup mirrors `parse_rf2` (kept in sync; this never mutates).
pub fn rf2_lmu_debug(telem: &[u8], scoring: &[u8], lmu: Option<&[u8]>) -> String {
    const TELEM_BASE: usize = 16;
    const TELEM_STRIDE: usize = 1888;
    // Re-find the player exactly as parse_rf2 does (scoring element + telem idx).
    let (player_id, sbase) = {
        let (mut pid, mut sb) = (None, None);
        if scoring.len() >= 122 {
            let n = (le::i32(scoring, 116).max(0) as usize).min(128);
            for i in 0..n {
                let b = 560 + i * 584;
                if scoring.len() < b + 584 {
                    break;
                }
                if scoring[b + 196] != 0 {
                    pid = Some(le::i32(scoring, b));
                    sb = Some(b);
                    break;
                }
            }
        }
        (pid, sb)
    };
    let tn = (le::i32(telem, 12).max(0) as usize).min(128);
    let mut idx = 0usize;
    if let Some(pid) = player_id {
        for j in 0..tn {
            let b = TELEM_BASE + j * TELEM_STRIDE;
            if telem.len() < b + TELEM_STRIDE {
                break;
            }
            if le::i32(telem, b) == pid {
                idx = j;
                break;
            }
        }
    }
    let base = TELEM_BASE + idx * TELEM_STRIDE;
    let mut out = String::new();
    // Flags: mGamePhase@120, mYellowFlagState@121, mSectorFlag[3]@122, per-car mFlag@sb+504.
    if scoring.len() >= 125 {
        let phase = scoring[120];
        let yellow = scoring[121] as i8;
        let sect = [scoring[122] as i8, scoring[123] as i8, scoring[124] as i8];
        let carflag = sbase.map(|b| scoring[b + 504] as i8).unwrap_or(-1);
        out +=
            &format!("[shm] flag: phase={phase} yellow={yellow} sect={sect:?} carFlag={carflag}\n");
    }
    // FL tyre temps (raw Kelvin → °C): surface tread L/C/R + carcass core.
    if telem.len() >= base + 848 + 260 {
        let w = base + 848;
        let k = |o: usize| le::f64(telem, w + o);
        out += &format!(
            "[shm] FL °C: surf[{:.1}/{:.1}/{:.1}] inner[{:.1}/{:.1}/{:.1}] carcass={:.1}\n",
            k(128) - 273.15,
            k(136) - 273.15,
            k(144) - 273.15, // surface tread L/C/R
            k(212) - 273.15,
            k(220) - 273.15,
            k(228) - 273.15, // inner layer L/C/R
            k(204) - 273.15, // carcass core
        );
    }
    // Compound names (verify @620/@638 offsets vs the in-game compound).
    if telem.len() >= base + 656 {
        out += &format!(
            "[shm] compound: front={:?}({}) rear={:?}({})\n",
            ascii_str(telem, base + 620, 18),
            compound_code(&ascii_str(telem, base + 620, 18)),
            ascii_str(telem, base + 638, 18),
            compound_code(&ascii_str(telem, base + 638, 18)),
        );
    }
    // LMU native map: confirm it's mapped + hex-dump the native scoring block
    // (~offset 1632) where flag/FCY state should live for the LMU-only override.
    match lmu {
        Some(b) if b.len() > 128468 => {
            out += &format!(
                "[shm] LMU_Data len={} active={} pIdx={}",
                b.len(),
                b[128464],
                b[128465]
            );
            // Track name is at 1632; by rF2ScoringInfo layout mGamePhase ≈ +108 → 1740.
            // Dump that flag region (phase/yellow/sector) so a yellow shows a change.
            if b.len() >= 1740 + 12 {
                out += " lmuFlag@1740:";
                for o in 0..12 {
                    out += &format!(" {:02x}", b[1740 + o]);
                }
            }
            out += "\n";
            let pidx = b[128465] as usize;
            let lbase = 128468 + pidx * 1888;
            // Hypothesis: LMU's native entry uses the rF2 field layout but with LMU's
            // own (HUD-matching) values. Read it via the SAME struct + offsets and
            // print rpm/water/oil/FL-temps. If rpm matches the engine and the temps
            // match the HUD, we just read everything from the native map for LMU.
            if let Some(lv) = crate::rf2::VehicleTelem::at(b, lbase) {
                let w0 = lv.wheel(0);
                out += &format!(
                    "[shm] LMU-native@rF2off: rpm={:.0} water={:.1} oil={:.1} | FLsurfC[{:.1}/{:.1}/{:.1}] FLinnerC[{:.1}/{:.1}/{:.1}] carcC={:.1}\n",
                    lv.rpm(), lv.water_temp(), lv.oil_temp(),
                    w0.surface_temp_k(0) - 273.15, w0.surface_temp_k(1) - 273.15, w0.surface_temp_k(2) - 273.15,
                    w0.inner_temp_k(0) - 273.15, w0.inner_temp_k(1) - 273.15, w0.inner_temp_k(2) - 273.15,
                    w0.carcass_temp_k() - 273.15,
                );
            }
            // Pending features: the 3 TC fields (mTC@750/mTCMax@751/mTCSlip@752/
            // mTCCut@754, u8) for the TC-triple widget, and mVirtualEnergy@776 (f32)
            // for LMU fuel-as-%. Compare these to the in-game TC settings + the VE %.
            if b.len() >= lbase + 780 {
                out +=
                    &format!(
                    "[shm] LMU tc[lvl={} max={} slip={} cut={}] VE@776={:.4} battSoC@704={:.3}\n",
                    b[lbase + 750], b[lbase + 751], b[lbase + 752], b[lbase + 754],
                    le::f32(b, lbase + 776), le::f64(b, lbase + 704),
                );
            }
            // Fallback hunt: if the layout differs, scan the native entry for f32/f64
            // values near the game's tyre temp (~85-95°C / ~358-368 K) to locate it.
            if pidx < 104 && b.len() >= lbase + 1888 {
                out += "[shm] LMU-native ~game-temp scan:";
                let mut o = 0;
                while o + 8 <= 1888 {
                    let f32v = le::f32(b, lbase + o);
                    let f64v = le::f64(b, lbase + o);
                    let near = |v: f64| (85.0..95.0).contains(&v) || (358.0..368.0).contains(&v);
                    if near(f32v as f64) {
                        out += &format!(" f32@{o}={f32v:.1}");
                    }
                    if near(f64v) {
                        out += &format!(" f64@{o}={f64v:.1}");
                    }
                    o += 4;
                }
                out += "\n";
            }
        }
        Some(_) => out += "[shm] LMU_Data present but short\n",
        None => out += "[shm] LMU_Data: not mapped\n",
    }
    out
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
/// Map an rF2/LMU tyre compound NAME ("Soft"/"Medium"/"Hard"/"Wet", or sim-
/// specific) to our code: 0 soft, 1 medium, 2 hard, 3 wet/inter, -1 unknown.
fn compound_code(name: &str) -> i32 {
    let n = name.to_ascii_lowercase();
    if n.is_empty() {
        -1
    } else if n.contains("wet") || n.contains("rain") || n.contains("inter") {
        3
    } else if n.contains("hard") {
        2
    } else if n.contains("medium") || n.contains("(m)") {
        1
    } else if n.contains("soft") {
        0
    } else {
        -1
    }
}

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
    t.tt_fl_i = fl;
    t.tt_fl_m = fl;
    t.tt_fl_o = fl;
    t.tt_fr_i = fr;
    t.tt_fr_m = fr;
    t.tt_fr_o = fr;
    t.tt_rl_i = rl;
    t.tt_rl_m = rl;
    t.tt_rl_o = rl;
    t.tt_rr_i = rr;
    t.tt_rr_m = rr;
    t.tt_rr_o = rr;
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

    // Typed view over the player's vehicle telemetry — all offsets live in `rf2`.
    let v = crate::rf2::VehicleTelem::at(telem, base)?;
    let mut t = Telemetry::idle();
    t.gear = le::gear_byte(v.gear()); // -1=R, 0=N, 1+
    t.rpm = v.rpm().round().max(0.0) as i32;
    t.max_rpm = v.max_rpm().round().max(0.0) as i32;
    t.shift_rpm = t.max_rpm;
    t.speed_kmh = (v.speed_ms() * 3.6).round().max(0.0) as i32;
    t.fuel_dl = (v.fuel() * 10.0).round().max(0.0) as i32;
    t.throttle = (v.throttle() * 100.0).round() as i32;
    t.brake = (v.brake() * 100.0).round() as i32;
    t.steer = (v.steering() * 100.0).round().clamp(-100.0, 100.0) as i32;
    t.clutch = (v.clutch() * 100.0).round() as i32;
    t.water_c = v.water_temp().round() as i32;
    t.oil_c = v.oil_temp().round() as i32;
    t.laps_done = v.lap_number();
    // rF2 has no current-lap-time field ("instantly becomes last"); derive it from
    // mElapsedTime − mLapStartET (seconds).
    let cur = v.elapsed_time() - v.lap_start_et();
    t.cur_lap_ms = (cur * 1000.0).round().max(0.0) as i32;
    t.fuel_cap_dl = (v.fuel_capacity() * 10.0).round().max(0.0) as i32;
    t.headlights = v.headlights() as i32;
    t.pit_limiter = v.speed_limiter() as i32;
    t.ignition = v.ignition() as i32;
    // Hybrid / electric boost (LMU hypercars & LMDh): battery SoC (0..1 → 0..100.0%)
    // and boost-motor state (0 unavailable, 1 inactive, 2 propulsion, 3 regen).
    let batt = v.battery_charge_fraction();
    if batt.is_finite() {
        t.battery_pct = (batt * 1000.0).round().clamp(0.0, 1000.0) as i32;
    }
    t.ers_state = v.electric_boost_motor_state();
    // Per-wheel tyre data. Stock rF2's tyre HUD tracks the SURFACE tread temp
    // (`mTemperature`, left/center/right), so that's what goes in here — but
    // LMU's HUD shows the much steadier INNER-LAYER temp, so when LMU's native
    // map is present `apply_lmu_native` OVERRIDES these with inner-layer values
    // (surface swings ±25 °C between straight and braking zone; the user-visible
    // symptom of showing surface on LMU was "55–105 °C while the HUD says 79").
    // `_i/_m/_o` = surface inner/middle/outer; tt_carc keeps the carcass core.
    // Kelvin → 0.1°C with a 200 K floor: garbage wheels become NA → "--", not -273°C.
    const NA: i32 = pith_core::format::NA;
    let k2dc = |k: f64| {
        if k.is_finite() && k > 200.0 {
            ((k - 273.15) * 10.0).round() as i32
        } else {
            NA
        }
    };
    #[allow(clippy::type_complexity)]
    let wheels: [(
        &mut i32,
        &mut i32,
        &mut i32,
        &mut i32,
        &mut i32,
        &mut i32,
        &mut i32,
        &mut i32,
    ); 4] = [
        (
            &mut t.tt_fl_i,
            &mut t.tt_fl_m,
            &mut t.tt_fl_o,
            &mut t.tt_avg_fl,
            &mut t.tt_carc_fl,
            &mut t.bt_fl,
            &mut t.tp_fl,
            &mut t.tw_fl,
        ),
        (
            &mut t.tt_fr_i,
            &mut t.tt_fr_m,
            &mut t.tt_fr_o,
            &mut t.tt_avg_fr,
            &mut t.tt_carc_fr,
            &mut t.bt_fr,
            &mut t.tp_fr,
            &mut t.tw_fr,
        ),
        (
            &mut t.tt_rl_i,
            &mut t.tt_rl_m,
            &mut t.tt_rl_o,
            &mut t.tt_avg_rl,
            &mut t.tt_carc_rl,
            &mut t.bt_rl,
            &mut t.tp_rl,
            &mut t.tw_rl,
        ),
        (
            &mut t.tt_rr_i,
            &mut t.tt_rr_m,
            &mut t.tt_rr_o,
            &mut t.tt_avg_rr,
            &mut t.tt_carc_rr,
            &mut t.bt_rr,
            &mut t.tp_rr,
            &mut t.tw_rr,
        ),
    ];
    for (i, (ti, tm, to, avg, carc, bt, tp, tw)) in wheels.into_iter().enumerate() {
        let wh = v.wheel(i);
        let (ki, km, ko) = (
            wh.surface_temp_k(0),
            wh.surface_temp_k(1),
            wh.surface_temp_k(2),
        );
        let zi = k2dc(ki); // surface tread, left
        let zm = k2dc(km); // surface tread, center
        let zo = k2dc(ko); // surface tread, right
        *ti = zi;
        *tm = zm;
        *to = zo;
        *carc = k2dc(wh.carcass_temp_k());
        // Average — computed from the RAW Kelvin (mean then one conversion), so it
        // isn't degraded by averaging the already-rounded 0.1°C zones. This is the
        // game's per-tyre number; matches the in-game HUD.
        *avg = if zi == NA || zm == NA || zo == NA {
            NA
        } else {
            k2dc((ki + km + ko) / 3.0)
        };
        *bt = k2dc(wh.brake_temp_k()); // Kelvin → 0.1°C
        *tp = (wh.pressure_kpa() * 10.0).round() as i32; // kPa → 0.1 kPa
        *tw = (wh.wear_fraction() * 100.0).round().clamp(0.0, 100.0) as i32;
    }

    // Tyre compound is per-AXLE (front → FL/FR, rear → RL/RR).
    let front = compound_code(v.front_compound_name());
    let rear = compound_code(v.rear_compound_name());
    t.comp_fl = front;
    t.comp_fr = front;
    t.comp_rl = rear;
    t.comp_rr = rear;

    // ---- Scoring-buffer fields for the player car (not in telemetry): position,
    // lap/sector times, track %, session flag, field size. Times are doubles in
    // seconds (negative = none); header base is 12, vehicle base 560 stride 584.
    if let Some(sb) = sbase {
        if scoring.len() >= sb + 584 {
            let ms = |o: usize| {
                let s = le::f64(scoring, sb + o);
                if s >= 0.0 {
                    (s * 1000.0).round() as i32
                } else {
                    0
                }
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
            // flag: session phase (120) + full-course yellow (121) + per-sector flag
            // (mSectorFlag[3]@122) + per-car mFlag (sb+504). LMU signals LOCAL yellows
            // ONLY via mSectorFlag — value 1 = yellow, 11 = green/clear (observed) —
            // leaving phase=5/yellow=0, so we must check the sectors too.
            let phase = scoring[120];
            let yellow = scoring[121] as i8;
            let sector_yellow = scoring.len() > 124 && (122..=124).any(|o| scoring[o] == 1);
            let carflag = scoring[sb + 504];
            t.flag = if phase == 8 {
                5 // checkered (session over)
            } else if phase == 6 || yellow > 0 || sector_yellow {
                2 // yellow — full-course (phase/yellow) or local (sector flag)
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

    #[test]
    fn acevo_car_and_track() {
        // Car on the GRAPHICS page (char[33] @3086), track on STATIC (@136).
        let mut g = vec![0u8; 4900];
        g[3086..3086 + 12].copy_from_slice(b"ks_porsche_g\x00"[..12].try_into().unwrap());
        let mut s = vec![0u8; 208];
        s[136..136 + 7].copy_from_slice(b"laguna\x00");
        let (car, track) = acevo_identity(&s, &g);
        assert_eq!(car.as_deref(), Some("ks_porsche_g"));
        assert_eq!(track.as_deref(), Some("laguna"));
        // Short/absent pages → None, never a panic.
        assert_eq!(acevo_identity(&[], &[]), (None, None));
    }

    /// LMU HUD parity: the native map's inner-layer temps must OVERRIDE the
    /// surface temps parse_rf2 put in (surface swings wildly under braking;
    /// LMU's HUD shows the steady inner layer). Garbage/zero wheels must NOT
    /// override (plausibility gate).
    #[test]
    fn lmu_native_inner_layer_overrides_tyre_temps() {
        let mut t = Telemetry::idle();
        // parse_rf2 left violent surface temps in (e.g. braking spike, 105 °C).
        t.tt_fl_i = 1050;
        t.tt_fl_m = 1050;
        t.tt_fl_o = 1050;
        t.tt_avg_fl = 1050;

        // Minimal LMU_Data: header flag + player idx 0 + one 1888-byte entry.
        let base = 128468;
        let mut b = vec![0u8; base + 1888];
        b[128464] = 1; // activeVehicles
        b[128465] = 0; // playerVehicleIdx
                       // FL wheel = entry + WHEELS(848) + 0*260; inner layer zones @+212 (Kelvin).
        let w = base + 848;
        let k: f64 = 79.0 + 273.15; // the steady HUD-style 79 °C
        for z in 0..3 {
            b[w + 212 + z * 8..w + 212 + (z + 1) * 8].copy_from_slice(&k.to_le_bytes());
        }
        b[w + 204..w + 212].copy_from_slice(&(90.0f64 + 273.15).to_le_bytes()); // carcass

        apply_lmu_native(&mut t, &b);
        assert_eq!(t.tt_fl_i, 790);
        assert_eq!(t.tt_fl_m, 790);
        assert_eq!(t.tt_fl_o, 790);
        assert_eq!(t.tt_avg_fl, 790);
        assert_eq!(t.tt_carc_fl, 900);
        // FR wheel was all zeros in the buffer (0 K = implausible) → untouched.
        assert_eq!(t.tt_fr_i, 0);
        assert_eq!(t.tt_avg_fr, 0);
    }
}
