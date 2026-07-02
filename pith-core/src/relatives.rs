//! Multi-car "relatives / standings" data — the only telemetry that isn't a
//! single-car scalar, so it rides its own `@REL` wire line (not the `$`-frame).
//!
//! The **host** (dashboard / `pith-shim`) builds a [`Relatives`] from a sim's
//! all-cars buffer (rF2 scoring, ACC broadcasting, …), encodes it with
//! [`Relatives::to_wire`], and sends it to the device. The **device** decodes the
//! identical struct with [`Relatives::from_wire`] and a widget renders it in
//! either "standings" (by position, gap to leader) or "relative" (cars nearest on
//! track, signed gap to you) mode.

use crate::le;

/// Max cars carried on one `@REL` line (top group ∪ cars around the player).
pub const MAX_REL: usize = 12;
/// Short-name byte budget per car (ASCII, separators stripped).
pub const NAME_LEN: usize = 14;

pub const FLAG_PLAYER: u8 = 1 << 0;
pub const FLAG_IN_PITS: u8 = 1 << 1;

/// One car in the relatives/standings list.
#[derive(Clone, Copy)]
pub struct RelCar {
    pub place: u8,         // race position, 1-based
    pub gap_leader_ms: i32, // gap to the leader (standings view) — sim-accurate
    pub gap_rel_ms: i32,   // signed gap to the player on track (+ ahead, − behind)
    pub laps: i16,         // laps completed
    pub flags: u8,         // FLAG_PLAYER | FLAG_IN_PITS
    pub name: [u8; NAME_LEN], // ASCII, NUL-padded
}

impl Default for RelCar {
    fn default() -> Self {
        Self { place: 0, gap_leader_ms: 0, gap_rel_ms: 0, laps: 0, flags: 0, name: [0; NAME_LEN] }
    }
}

impl RelCar {
    pub fn is_player(&self) -> bool {
        self.flags & FLAG_PLAYER != 0
    }
    pub fn in_pits(&self) -> bool {
        self.flags & FLAG_IN_PITS != 0
    }
    /// Name as a `&str` (up to the first NUL).
    pub fn name_str(&self) -> &str {
        let end = self.name.iter().position(|&b| b == 0).unwrap_or(NAME_LEN);
        core::str::from_utf8(&self.name[..end]).unwrap_or("")
    }
    /// Set the display name (ASCII-sanitised, wire separators replaced, truncated).
    pub fn set_name(&mut self, s: &str) {
        self.name = [0; NAME_LEN];
        for (dst, b) in self.name.iter_mut().zip(s.bytes()) {
            // keep it ASCII-printable and free of the wire separators.
            *dst = if (0x20..0x7f).contains(&b) && b != b'|' && b != b',' { b } else { b'_' };
        }
    }
}

/// A bounded list of cars plus the player's index within it.
#[derive(Clone)]
pub struct Relatives {
    pub cars: [RelCar; MAX_REL],
    pub count: u8,
    pub player: u8, // index into `cars` of the player's row (or 0 if none)
}

impl Default for Relatives {
    fn default() -> Self {
        Self { cars: [RelCar::default(); MAX_REL], count: 0, player: 0 }
    }
}

impl Relatives {
    pub fn entries(&self) -> &[RelCar] {
        &self.cars[..self.count as usize]
    }

    /// Encode as a single `@REL` line:
    /// `@REL|<player>|place,gapLeader,gapRel,laps,flags,name|…`
    pub fn to_wire(&self) -> String {
        let mut s = String::with_capacity(16 + self.count as usize * 24);
        s.push_str("@REL|");
        s.push_str(&self.player.to_string());
        for c in self.entries() {
            s.push('|');
            s.push_str(&c.place.to_string());
            s.push(',');
            s.push_str(&c.gap_leader_ms.to_string());
            s.push(',');
            s.push_str(&c.gap_rel_ms.to_string());
            s.push(',');
            s.push_str(&c.laps.to_string());
            s.push(',');
            s.push_str(&c.flags.to_string());
            s.push(',');
            s.push_str(c.name_str());
        }
        s
    }

    /// Decode an `@REL` line produced by [`Relatives::to_wire`]. Malformed cars
    /// are skipped; returns `None` only if the prefix/header is missing.
    pub fn from_wire(line: &str) -> Option<Relatives> {
        let body = line.strip_prefix("@REL|")?;
        let mut parts = body.split('|');
        let player: u8 = parts.next()?.parse().ok()?;
        let mut out = Relatives { player, ..Default::default() };
        for rec in parts {
            if out.count as usize >= MAX_REL {
                break;
            }
            let mut f = rec.split(',');
            let mut car = RelCar::default();
            // place,gapLeader,gapRel,laps,flags then the (possibly empty) name.
            let (place, gl, gr, laps, flags) = (
                f.next().and_then(|v| v.parse().ok()),
                f.next().and_then(|v| v.parse().ok()),
                f.next().and_then(|v| v.parse().ok()),
                f.next().and_then(|v| v.parse().ok()),
                f.next().and_then(|v| v.parse().ok()),
            );
            match (place, gl, gr, laps, flags) {
                (Some(p), Some(a), Some(b), Some(l), Some(fl)) => {
                    car.place = p;
                    car.gap_leader_ms = a;
                    car.gap_rel_ms = b;
                    car.laps = l;
                    car.flags = fl;
                }
                _ => continue,
            }
            car.set_name(f.next().unwrap_or(""));
            out.cars[out.count as usize] = car;
            out.count += 1;
        }
        Some(out)
    }
}

// ── rF2 / LMU scoring → Relatives ────────────────────────────────────────────
// Offsets validated against rF2State.h (pack(4), 64-bit). File base 12:
// mNumVehicles@116, vehicles@560 stride 584, track length (scoringInfo.mLapDist)@100.
// Element-relative: mVehicleName@36, mLapDist@104, mTotalLaps(short)@100,
// mIsPlayer@196, mInPits@198, mPlace@199, mBestLapTime@144, mTimeBehindLeader@244.
const VEH_BASE: usize = 560;
const VEH_STRIDE: usize = 584;

struct RawCar {
    place: u8,
    laps: i16,
    lapdist: f64,
    best_s: f64,
    behind_leader_s: f64,
    in_pits: bool,
    is_player: bool,
    name: String,
}

/// Build a [`Relatives`] from an rF2/LMU `$rFactor2SMMP_Scoring$` buffer.
pub fn parse_rf2_relatives(scoring: &[u8]) -> Option<Relatives> {
    if scoring.len() < 120 {
        return None;
    }
    let n = (le::i32(scoring, 116).max(0) as usize).min(128);
    let track_len = le::f64(scoring, 100);
    if n == 0 || !(track_len.is_finite() && track_len > 1.0) {
        return None;
    }

    let mut cars: Vec<RawCar> = Vec::with_capacity(n);
    for i in 0..n {
        let b = VEH_BASE + i * VEH_STRIDE;
        if scoring.len() < b + VEH_STRIDE {
            break;
        }
        // mDriverName[32]@4 is the actual driver; mVehicleName[64]@36 is the car/
        // livery ("#21:WEC …" in LMU). Prefer the driver, fall back to the vehicle.
        let read = |off: usize, max: usize| {
            let end = (b + off..b + off + max).take_while(|&o| scoring[o] != 0).count();
            core::str::from_utf8(&scoring[b + off..b + off + end]).unwrap_or("").trim().to_string()
        };
        let driver = read(4, 32);
        let name = if driver.is_empty() { read(36, 64) } else { driver };
        cars.push(RawCar {
            place: scoring[b + 199],
            laps: le::i16(scoring, b + 100),
            lapdist: le::f64(scoring, b + 104),
            best_s: le::f64(scoring, b + 144),
            behind_leader_s: le::f64(scoring, b + 244),
            in_pits: scoring[b + 198] != 0,
            is_player: scoring[b + 196] != 0,
            name,
        });
    }
    let player_i = cars.iter().position(|c| c.is_player)?;
    let player_dist = cars[player_i].lapdist;

    // Pace for the track→time conversion of the relative gap: player's best lap,
    // else the field's fastest, else a nominal 55 m/s.
    let best_s = {
        let pb = cars[player_i].best_s;
        let any = cars.iter().map(|c| c.best_s).filter(|&s| s > 0.0).fold(f64::INFINITY, f64::min);
        if pb > 0.0 { pb } else if any.is_finite() { any } else { 0.0 }
    };
    let pace_mps = if best_s > 0.0 { track_len / best_s } else { 55.0 };

    let mut built: Vec<RelCar> = Vec::with_capacity(cars.len());
    for (i, c) in cars.iter().enumerate() {
        // Signed track gap to player, wrapped to the nearest direction.
        let mut d = c.lapdist - player_dist;
        if d > track_len / 2.0 {
            d -= track_len;
        } else if d < -track_len / 2.0 {
            d += track_len;
        }
        let gap_rel_ms = if i == player_i { 0 } else { (d / pace_mps * 1000.0).round() as i32 };
        let mut flags = 0u8;
        if c.is_player {
            flags |= FLAG_PLAYER;
        }
        if c.in_pits {
            flags |= FLAG_IN_PITS;
        }
        let mut rc = RelCar {
            place: c.place,
            gap_leader_ms: (c.behind_leader_s.max(0.0) * 1000.0).round() as i32,
            gap_rel_ms,
            laps: c.laps.max(0),
            flags,
            name: [0; NAME_LEN],
        };
        rc.set_name(short_name(&c.name));
        built.push(rc);
    }

    Some(select(built, player_i))
}

/// Build a bounded [`Relatives`] from a host-assembled car list (any sim).
/// Applies the same leading-group ∪ nearest-to-player windowing every source
/// uses, so ACC broadcasting, rF2 scoring, etc. produce identical wire lines.
pub fn from_cars(built: Vec<RelCar>, player_i: usize) -> Relatives {
    select(built, player_i)
}

/// Trim a full vehicle name to a compact label (last word, or the head).
pub fn short_name(name: &str) -> &str {
    let t = name.trim();
    if t.len() <= NAME_LEN {
        t
    } else {
        // prefer a trailing surname-ish token if the tail fits
        t.rsplit([' ', '_']).next().filter(|w| w.len() <= NAME_LEN).unwrap_or(&t[..NAME_LEN])
    }
}

/// Pick the cars to send: the leading group ∪ the cars nearest the player on
/// track, capped at `MAX_REL`, returned in race-position order. The device then
/// shows a subset per its view mode.
fn select(mut built: Vec<RelCar>, player_i: usize) -> Relatives {
    let mut out = Relatives::default();
    if built.len() <= MAX_REL {
        built.sort_by_key(|c| c.place);
        for (i, c) in built.iter().enumerate() {
            out.cars[i] = *c;
            if c.is_player() {
                out.player = i as u8;
            }
        }
        out.count = built.len() as u8;
        return out;
    }

    const TOP: usize = 5;
    let mut keep: Vec<usize> = Vec::with_capacity(MAX_REL);
    // top by position
    let mut by_place: Vec<usize> = (0..built.len()).collect();
    by_place.sort_by_key(|&i| built[i].place);
    for &i in by_place.iter().take(TOP) {
        keep.push(i);
    }
    // nearest to player on track (player included via gap 0)
    let mut by_near: Vec<usize> = (0..built.len()).collect();
    by_near.sort_by_key(|&i| built[i].gap_rel_ms.unsigned_abs());
    for &i in &by_near {
        if keep.len() >= MAX_REL {
            break;
        }
        if !keep.contains(&i) {
            keep.push(i);
        }
    }
    let _ = player_i;
    keep.sort_by_key(|&i| built[i].place);
    for (slot, &i) in keep.iter().enumerate() {
        out.cars[slot] = built[i];
        if built[i].is_player() {
            out.player = slot as u8;
        }
    }
    out.count = keep.len() as u8;
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_round_trip() {
        let mut r = Relatives::default();
        r.player = 1;
        r.count = 2;
        r.cars[0] = RelCar { place: 1, gap_leader_ms: 0, gap_rel_ms: -2300, laps: 5, flags: 0, name: [0; NAME_LEN] };
        r.cars[0].set_name("VERSTAPPEN");
        r.cars[1] = RelCar { place: 2, gap_leader_ms: 1450, gap_rel_ms: 0, laps: 5, flags: FLAG_PLAYER, name: [0; NAME_LEN] };
        r.cars[1].set_name("HAMILTON");
        let wire = r.to_wire();
        let back = Relatives::from_wire(&wire).unwrap();
        assert_eq!(back.count, 2);
        assert_eq!(back.player, 1);
        assert_eq!(back.cars[0].place, 1);
        assert_eq!(back.cars[0].gap_rel_ms, -2300);
        assert_eq!(back.cars[0].name_str(), "VERSTAPPEN");
        assert_eq!(back.cars[1].gap_leader_ms, 1450);
        assert!(back.cars[1].is_player());
        assert_eq!(back.cars[1].name_str(), "HAMILTON");
    }

    #[test]
    fn from_wire_skips_garbage() {
        // missing prefix → None
        assert!(Relatives::from_wire("REL|0|1,0,0,0,0,X").is_none());
        // one good car, one malformed (skipped)
        let r = Relatives::from_wire("@REL|0|1,0,0,3,1,ME|bogus|2,500,500,3,0,YOU").unwrap();
        assert_eq!(r.count, 2);
        assert_eq!(r.cars[0].name_str(), "ME");
        assert_eq!(r.cars[1].name_str(), "YOU");
    }

    #[test]
    fn rf2_relatives_from_scoring() {
        // Synthetic scoring buffer: 3 cars, player = car index 1.
        let mut s = vec![0u8; VEH_BASE + 3 * VEH_STRIDE];
        s[116..120].copy_from_slice(&3i32.to_le_bytes()); // mNumVehicles
        s[100..108].copy_from_slice(&3000.0f64.to_le_bytes()); // track length 3 km
        let mut set = |i: usize, place: u8, dist: f64, behind: f64, player: bool, name: &str, best: f64| {
            let b = VEH_BASE + i * VEH_STRIDE;
            let nb = name.as_bytes();
            s[b + 36..b + 36 + nb.len()].copy_from_slice(nb);
            s[b + 100..b + 102].copy_from_slice(&3i16.to_le_bytes()); // laps
            s[b + 104..b + 112].copy_from_slice(&dist.to_le_bytes());
            s[b + 144..b + 152].copy_from_slice(&best.to_le_bytes());
            s[b + 196] = player as u8;
            s[b + 199] = place;
            s[b + 244..b + 252].copy_from_slice(&behind.to_le_bytes());
        };
        // leader at 1500 m, player 100 m behind on track, third car 100 m ahead.
        set(0, 1, 1500.0, 0.0, false, "LEADER", 60.0); // pace = 3000/60 = 50 m/s
        set(1, 2, 1400.0, 1.5, true, "PLAYER", 60.0);
        set(2, 3, 1500.0, 3.0, false, "THIRD", 60.0);
        let r = parse_rf2_relatives(&s).unwrap();
        assert_eq!(r.count, 3);
        // player row found, gap_rel 0, flagged
        let p = &r.cars[r.player as usize];
        assert!(p.is_player());
        assert_eq!(p.gap_rel_ms, 0);
        assert_eq!(p.gap_leader_ms, 1500); // 1.5 s behind leader
        // leader is 100 m ahead of player @ 50 m/s → +2000 ms
        let lead = r.cars.iter().find(|c| c.place == 1).unwrap();
        assert_eq!(lead.gap_rel_ms, 2000);
        assert_eq!(lead.name_str(), "LEADER");
    }
}
