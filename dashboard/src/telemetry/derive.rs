//! Dashboard-side derived telemetry: core fields no game transmits directly but
//! that we can compute from the stream — **best lap** (min completed lap),
//! **fuel per lap / laps-left** (fuel burn across laps) and a **lap delta**
//! (current pace vs best, by track position). Each only fills a field the source
//! left empty, so a source that *does* provide it always wins.
//!
//! NOTE: we deliberately do NOT estimate current-lap time. A wall-clock estimate
//! has no idea when the game is paused/stopped or has no laps at all (Forza
//! free-roam), so it just counts up forever. Current lap is shown only when a
//! source provides a real game timer (rF2/LMU derive it from the session clock,
//! ACC reads it, etc.); otherwise it stays blank.

use pith_core::simhub::Telemetry;

/// Which computed fields a telemetry source actually supplies. Sticky per source
/// (set once a source sends a real value, so a momentary 0 — delta on-pace — doesn't
/// make us think the source dropped it). When a field is supplied by ANY live
/// source we must NOT compute our own, even if this frame's merged value is 0: our
/// tracker's state is stale and would emit garbage.
#[derive(Default, Clone, Copy)]
pub struct Provided {
    pub best_lap: bool,
    pub delta: bool,
    pub fuel_per_lap: bool,
}

impl Provided {
    /// Mark every field this frame carries a real (non-zero) value for. Sticky.
    pub fn observe(&mut self, t: &Telemetry) {
        self.best_lap |= t.best_lap_ms != 0;
        self.delta |= t.delta_ms != 0;
        self.fuel_per_lap |= t.fuel_per_lap_ml != 0;
    }
    pub fn merge(self, o: Provided) -> Provided {
        Provided {
            best_lap: self.best_lap || o.best_lap,
            delta: self.delta || o.delta,
            fuel_per_lap: self.fuel_per_lap || o.fuel_per_lap,
        }
    }
}

/// All derived-field trackers, run in order each frame.
#[derive(Default)]
pub struct Derived {
    best: BestLap,
    fuel: Fuel,
    ve: Ve,
    delta: Delta,
}

impl Derived {
    /// Fill computed fields. `provided` says which fields a live source already
    /// supplies — those are left untouched (compute only when NO source has them).
    pub fn update(&mut self, t: &mut Telemetry, provided: Provided) {
        if !provided.best_lap {
            self.best.update(t); // best needs last_lap; run before delta
        }
        self.fuel.update(t, provided.fuel_per_lap);
        self.ve.update(t);
        if !provided.delta {
            self.delta.update(t);
        }
        // Surface-average tyre temp = mean of the inner/mid/outer tread zones, so
        // EVERY source exposes the single HUD-style number (the shim already sends
        // it for rF2/LMU; this covers Forza/ACC/GT7/SimHub etc.). NA-aware so a
        // missing zone stays "--" rather than averaging in garbage.
        let na = pith_core::format::NA;
        // Only fill it when the source DIDN'T (cur == 0); the shm path already sends
        // an avg computed from raw Kelvin, which is more accurate than re-averaging
        // the rounded zones, so don't clobber it.
        let avg = |cur: i32, i: i32, m: i32, o: i32| {
            if cur != 0 {
                cur
            } else if i == na || m == na || o == na {
                na
            } else {
                (i + m + o) / 3
            }
        };
        t.tt_avg_fl = avg(t.tt_avg_fl, t.tt_fl_i, t.tt_fl_m, t.tt_fl_o);
        t.tt_avg_fr = avg(t.tt_avg_fr, t.tt_fr_i, t.tt_fr_m, t.tt_fr_o);
        t.tt_avg_rl = avg(t.tt_avg_rl, t.tt_rl_i, t.tt_rl_m, t.tt_rl_o);
        t.tt_avg_rr = avg(t.tt_avg_rr, t.tt_rr_i, t.tt_rr_m, t.tt_rr_o);
    }
}

/// Best lap = the fastest completed lap seen (when the source doesn't send one).
#[derive(Default)]
struct BestLap {
    best_ms: i32,
    last_seen_lap: i32,
}

impl BestLap {
    fn update(&mut self, t: &mut Telemetry) {
        if t.laps_done < self.last_seen_lap {
            self.best_ms = 0; // new session → forget
        }
        self.last_seen_lap = t.laps_done;
        for v in [t.best_lap_ms, t.last_lap_ms] {
            if v > 0 && (self.best_ms == 0 || v < self.best_ms) {
                self.best_ms = v;
            }
        }
        if t.best_lap_ms == 0 && self.best_ms > 0 {
            t.best_lap_ms = self.best_ms;
        }
    }
}

/// Fuel burned per completed lap → `fuel_per_lap_ml` + `fuel_laps_x10`.
#[derive(Default)]
struct Fuel {
    started: bool,
    last_lap: i32,
    lap_start_fuel_dl: i32,
    per_lap_ml: i32,
}

impl Fuel {
    fn update(&mut self, t: &mut Telemetry, provided: bool) {
        if t.fuel_per_lap_ml > 0 {
            self.per_lap_ml = t.fuel_per_lap_ml; // source provides it — adopt
        } else if provided {
            // A source supplies fuel/lap but it's momentarily 0 — restore the last
            // value rather than recomputing from burn.
            if self.per_lap_ml > 0 {
                t.fuel_per_lap_ml = self.per_lap_ml;
            }
        } else {
            if !self.started || t.laps_done < self.last_lap {
                self.started = true;
                self.last_lap = t.laps_done;
                self.lap_start_fuel_dl = t.fuel_dl;
            } else if t.laps_done > self.last_lap {
                let used_ml = (self.lap_start_fuel_dl - t.fuel_dl) * 100; // 1 dl = 100 ml
                self.last_lap = t.laps_done;
                self.lap_start_fuel_dl = t.fuel_dl;
                if used_ml > 50 && used_ml < 30_000 {
                    self.per_lap_ml = if self.per_lap_ml == 0 {
                        used_ml
                    } else {
                        (self.per_lap_ml * 3 + used_ml) / 4 // EMA
                    };
                }
            }
            if self.per_lap_ml > 0 {
                t.fuel_per_lap_ml = self.per_lap_ml;
            }
        }
        // laps-left only when the source didn't send it.
        if t.fuel_laps_x10 == 0 && self.per_lap_ml > 0 && t.fuel_dl > 0 {
            t.fuel_laps_x10 = t.fuel_dl * 1000 / self.per_lap_ml;
        }
    }
}

/// Virtual-energy burned per completed lap (LMU energy-regulated cars) →
/// `ve_per_lap`, in 0.1% units. Mirrors [`Fuel`] but on `virtual_energy`.
#[derive(Default)]
struct Ve {
    started: bool,
    last_lap: i32,
    lap_start_ve: i32,
    per_lap: i32,
}

impl Ve {
    fn update(&mut self, t: &mut Telemetry) {
        if t.fuel_is_ve == 0 || t.virtual_energy <= 0 {
            return;
        }
        if !self.started || t.laps_done < self.last_lap {
            self.started = true;
            self.last_lap = t.laps_done;
            self.lap_start_ve = t.virtual_energy;
        } else if t.laps_done > self.last_lap {
            let used = self.lap_start_ve - t.virtual_energy; // 0.1% units
            self.last_lap = t.laps_done;
            self.lap_start_ve = t.virtual_energy;
            if used > 1 && used < 1000 {
                self.per_lap = if self.per_lap == 0 {
                    used
                } else {
                    (self.per_lap * 3 + used) / 4
                };
            }
        }
        if self.per_lap > 0 {
            t.ve_per_lap = self.per_lap;
        }
    }
}

const NB: usize = 1000; // track-position buckets — 0.1% each, matching track_pct

/// Predictive lap delta — the same algorithm a sim HUD uses: store the reference
/// lap as a table of elapsed-time-at-distance, then each frame take the current
/// distance and show `current_elapsed − reference_at(distance)`. We can't read
/// LMU's HUD delta or its reference lap (neither is in shared memory), so we
/// rebuild it from the best lap we observe, at 0.1% resolution with gap-filling so
/// it stays smooth. 0.1 ms units; negative = ahead of the reference.
struct Delta {
    started: bool,
    have_ref: bool,
    last_lap: i32,
    best_lap_ms: i32,
    last_b: i32,          // last track bucket filled this lap (−1 = none yet)
    ref_t: [i32; NB + 1], // reference lap: elapsed ms at each bucket (−1 = unset)
    cur_t: [i32; NB + 1], // current lap, filled as we go
}

impl Default for Delta {
    fn default() -> Self {
        Self {
            started: false,
            have_ref: false,
            last_lap: 0,
            best_lap_ms: 0,
            last_b: -1,
            ref_t: [-1; NB + 1],
            cur_t: [-1; NB + 1],
        }
    }
}

impl Delta {
    fn new_lap(&mut self) {
        self.cur_t = [-1; NB + 1];
        self.last_b = -1;
    }

    fn update(&mut self, t: &mut Telemetry) {
        if t.delta_ms != 0 {
            return; // source already provides a delta — don't override it
        }
        let cur_ms = t.cur_lap_ms;
        let b = t.track_pct.clamp(0, 1000); // 0..=1000 == bucket index

        if !self.started || t.laps_done < self.last_lap {
            self.started = true;
            self.last_lap = t.laps_done;
            self.new_lap();
        } else if t.laps_done > self.last_lap {
            // Adopt the just-completed lap as the reference if it's a new best.
            if t.last_lap_ms > 0 && (!self.have_ref || t.last_lap_ms < self.best_lap_ms) {
                self.ref_t = self.cur_t;
                self.best_lap_ms = t.last_lap_ms;
                self.have_ref = true;
            }
            self.last_lap = t.laps_done;
            self.new_lap();
        }
        if cur_ms <= 0 {
            return;
        }

        // Record the current lap densely: fill any buckets skipped since the last
        // frame by linear interpolation, so the reference table has no holes.
        let bi = b as usize;
        if self.last_b < 0 || b <= self.last_b {
            self.cur_t[bi] = cur_ms;
        } else {
            let prev = self.cur_t[self.last_b as usize].max(0);
            let span = b - self.last_b;
            for k in 1..=span {
                self.cur_t[(self.last_b + k) as usize] = prev + (cur_ms - prev) * k / span;
            }
        }
        if b > self.last_b {
            self.last_b = b;
        }

        // delta = our elapsed time − the reference lap's elapsed time at this exact
        // track position.
        if self.have_ref {
            let r = self.ref_t[bi];
            if r >= 0 {
                t.delta_ms = (cur_ms - r) * 10; // ms → 0.1 ms units
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_lap_from_completed() {
        let mut d = Derived::default();
        // Each frame is a fresh parse_line where the source leaves best_lap_ms = 0.
        let frame = |d: &mut Derived, lap, last| {
            let mut t = Telemetry::idle();
            t.laps_done = lap;
            t.last_lap_ms = last;
            d.update(&mut t, Provided::default());
            t.best_lap_ms
        };
        assert_eq!(frame(&mut d, 2, 84_000), 84_000);
        assert_eq!(frame(&mut d, 3, 82_500), 82_500); // faster
        assert_eq!(frame(&mut d, 4, 83_000), 82_500); // slower → best stays
    }

    #[test]
    fn delta_vs_reference_lap() {
        let mut d = Delta::default();
        let feed = |d: &mut Delta, lap, tp, cur, last| {
            let mut t = Telemetry::idle();
            t.laps_done = lap;
            t.track_pct = tp;
            t.cur_lap_ms = cur;
            t.last_lap_ms = last;
            d.update(&mut t);
            t.delta_ms
        };
        // Lap 1 reference: 80 ms per 0.1% of track → an 80 s lap.
        for tp in [250, 500, 750, 1000] {
            feed(&mut d, 1, tp, tp * 80, 0);
        }
        // Cross the line: adopt lap 1 (80 s) as the reference.
        feed(&mut d, 2, 0, 0, 80_000);
        // Lap 2 at 50%: 200 ms slower than the reference → +2000 (0.1 ms units).
        assert_eq!(feed(&mut d, 2, 500, 500 * 80 + 200, 80_000), 2000);
        // Lap 2 at 75%: 150 ms faster → −1500.
        assert_eq!(feed(&mut d, 2, 750, 750 * 80 - 150, 80_000), -1500);
    }

    #[test]
    fn fuel_per_lap_from_burn() {
        let mut d = Derived::default();
        let mut t = Telemetry::idle();
        t.laps_done = 1;
        t.fuel_dl = 500;
        d.update(&mut t, Provided::default());
        t.laps_done = 2;
        t.fuel_dl = 476; // burned 2.4 L
        d.update(&mut t, Provided::default());
        assert_eq!(t.fuel_per_lap_ml, 2400);
        assert_eq!(t.fuel_laps_x10, 476 * 1000 / 2400);
    }
}
