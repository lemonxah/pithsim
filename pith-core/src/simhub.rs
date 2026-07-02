//! SimHub Custom Serial protocol parser — Rust port of `simhub_proto.c`.
//!
//! Wire format, one frame per line:
//! ```text
//!   $g;speed;rpm;maxRpm;shiftRpm;curLap;lastLap;bestLap;pbLap;estLap;delta;
//!     pos;field;lap;totalLaps;lapsLeft;water;oil;oilP;boost;tc;abs;bias;
//!     fuel;fuelCap;fuelPerLap;fuelLaps;
//!     ttFLi;ttFLm;ttFLo;ttFRi;ttFRm;ttFRo;ttRLi;ttRLm;ttRLo;ttRRi;ttRRm;ttRRo;
//!     tpFL;tpFR;tpRL;tpRR;twFL;twFR;twRL;twRR;btFL;btFR;btRL;btRR;
//!     thr;brk;clu;steer;tcAct;absAct;lights;wipers;pitLim;ign;posX;posZ;
//!     s1;s2;s3;bs1;bs2;bs3
//! ```
//! - leading `$` is a resync sentinel; bytes before it on a line are ignored
//! - the first 4 fields are REQUIRED; later fields are OPTIONAL and default to 0
//!   (tyre wear defaults to 100), so short frames still parse
//! - times are integer milliseconds; delta is SIGNED, in 0.1 ms units

/// Latest parsed telemetry. Field names match `field_registry.json` accessors so
/// the generated `field_value()` can map a field id straight to a struct field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Telemetry {
    pub gear: u8, // b'R', b'N', or b'1'..b'9'
    // Core
    pub speed_kmh: i32,
    pub rpm: i32,
    pub max_rpm: i32,
    pub shift_rpm: i32,
    // Timing (ms)
    pub cur_lap_ms: i32,
    pub last_lap_ms: i32,
    pub best_lap_ms: i32,
    pub pb_lap_ms: i32,
    pub est_lap_ms: i32,
    pub delta_ms: i32, // signed, 0.1 ms units (10000 = 1.0000 s; neg = faster)
    // Race
    pub position: i32,
    pub field_size: i32,
    pub laps_done: i32,
    pub total_laps: i32,
    pub laps_left: i32,
    // Engine / car
    pub water_c: i32,
    pub oil_c: i32,
    pub oil_press_x10: i32,
    pub boost_kpa: i32,
    pub tc: i32,
    pub abs: i32,
    pub brake_bias_x10: i32,
    // Fuel
    pub fuel_dl: i32,
    pub fuel_cap_dl: i32,
    pub fuel_per_lap_ml: i32,
    pub fuel_laps_x10: i32,
    // Tyres: 3-zone temps per corner (inner / middle / outer)
    pub tt_fl_i: i32,
    pub tt_fl_m: i32,
    pub tt_fl_o: i32,
    pub tt_fr_i: i32,
    pub tt_fr_m: i32,
    pub tt_fr_o: i32,
    pub tt_rl_i: i32,
    pub tt_rl_m: i32,
    pub tt_rl_o: i32,
    pub tt_rr_i: i32,
    pub tt_rr_m: i32,
    pub tt_rr_o: i32,
    // Tyre pressures (kPa) and wear (% remaining)
    pub tp_fl: i32,
    pub tp_fr: i32,
    pub tp_rl: i32,
    pub tp_rr: i32,
    pub tw_fl: i32,
    pub tw_fr: i32,
    pub tw_rl: i32,
    pub tw_rr: i32,
    // Brakes (C)
    pub bt_fl: i32,
    pub bt_fr: i32,
    pub bt_rl: i32,
    pub bt_rr: i32,
    // Inputs
    pub throttle: i32,
    pub brake: i32,
    pub clutch: i32,
    pub steer: i32,
    // Aids engagement (live) — drive the side LEDs
    pub tc_active: i32,
    pub abs_active: i32,
    // Car control on/off states — drive button-box toggle sync
    pub headlights: i32,
    pub wipers: i32,
    pub pit_limiter: i32,
    pub ignition: i32,
    // Current race flag as a code: 0 none, 1 green, 2 yellow, 3 blue, 4 white,
    // 5 checkered, 6 black/meatball. Drives the Flag widget.
    pub flag: i32,
    // Lap progress in 0..=1000 (= 0..100.0%). Places the dot on the track map.
    pub track_pct: i32,
    // World position for the self-learned track map
    pub pos_x: i32,
    pub pos_z: i32,
    // Sector times (ms): this lap, then personal-best sectors for coloring
    pub s1_ms: i32,
    pub s2_ms: i32,
    pub s3_ms: i32,
    pub bs1_ms: i32,
    pub bs2_ms: i32,
    pub bs3_ms: i32,
    // Hybrid / electric boost (LMU & other ERS cars). battery_pct in 0..=1000
    // (= 0..100.0%); ers_state 0 unavailable, 1 inactive, 2 propulsion, 3 regen.
    pub battery_pct: i32,
    pub ers_state: i32,
    // Tyre carcass/core temp (0.1°C) — the stable structural temp, distinct from
    // the surface tread inner/mid/outer gradient (tt_*_i/m/o).
    pub tt_carc_fl: i32,
    pub tt_carc_fr: i32,
    pub tt_carc_rl: i32,
    pub tt_carc_rr: i32,
    // Tyre surface average (0.1°C) — mean of the inner/mid/outer tread zones; the
    // single number that matches a sim's per-tyre HUD readout.
    pub tt_avg_fl: i32,
    pub tt_avg_fr: i32,
    pub tt_avg_rl: i32,
    pub tt_avg_rr: i32,
    // Tyre compound per corner: 0 = soft, 1 = medium, 2 = hard, 3 = wet (-1 = n/a).
    pub comp_fl: i32,
    pub comp_fr: i32,
    pub comp_rl: i32,
    pub comp_rr: i32,
    // Extra TC channels (LMU): slip + cut, alongside the main level (`tc`).
    pub tc_slip: i32,
    pub tc_cut: i32,
    // LMU Virtual Energy: stint energy budget remaining in 0..=1000 (= 0..100.0%);
    // `fuel_is_ve` = 1 when the car is energy-regulated (Hypercar/LMDh) so fuel is
    // shown as % not litres. `ve_per_lap` = VE used per lap (0.1%), derived.
    pub virtual_energy: i32,
    pub ve_per_lap: i32,
    pub fuel_is_ve: i32,
}

impl Telemetry {
    /// Idle default with gear 'N' (matches the C `{ .gear = 'N' }`).
    pub fn idle() -> Self {
        Telemetry {
            gear: b'N',
            ..Default::default()
        }
    }

    /// Serialize into one canonical `$`-frame line (no trailing newline) — the
    /// exact inverse of [`parse_line`], in the positional order the firmware
    /// parses. Lets a decoded game/shared-memory snapshot ride the identical
    /// path to the device as a native SimHub text frame.
    pub fn to_frame(&self) -> String {
        use core::fmt::Write;
        let mut s = String::with_capacity(320);
        s.push('$');
        s.push(self.gear as char);
        macro_rules! a {
            ($v:expr) => {{
                let _ = write!(s, ";{}", $v);
            }};
        }
        a!(self.speed_kmh);
        a!(self.rpm);
        a!(self.max_rpm);
        a!(self.shift_rpm);
        a!(self.cur_lap_ms);
        a!(self.last_lap_ms);
        a!(self.best_lap_ms);
        a!(self.pb_lap_ms);
        a!(self.est_lap_ms);
        a!(self.delta_ms);
        a!(self.position);
        a!(self.field_size);
        a!(self.laps_done);
        a!(self.total_laps);
        a!(self.laps_left);
        a!(self.water_c);
        a!(self.oil_c);
        a!(self.oil_press_x10);
        a!(self.boost_kpa);
        a!(self.tc);
        a!(self.abs);
        a!(self.brake_bias_x10);
        a!(self.fuel_dl);
        a!(self.fuel_cap_dl);
        a!(self.fuel_per_lap_ml);
        a!(self.fuel_laps_x10);
        a!(self.tt_fl_i);
        a!(self.tt_fl_m);
        a!(self.tt_fl_o);
        a!(self.tt_fr_i);
        a!(self.tt_fr_m);
        a!(self.tt_fr_o);
        a!(self.tt_rl_i);
        a!(self.tt_rl_m);
        a!(self.tt_rl_o);
        a!(self.tt_rr_i);
        a!(self.tt_rr_m);
        a!(self.tt_rr_o);
        a!(self.tp_fl);
        a!(self.tp_fr);
        a!(self.tp_rl);
        a!(self.tp_rr);
        a!(self.tw_fl);
        a!(self.tw_fr);
        a!(self.tw_rl);
        a!(self.tw_rr);
        a!(self.bt_fl);
        a!(self.bt_fr);
        a!(self.bt_rl);
        a!(self.bt_rr);
        a!(self.throttle);
        a!(self.brake);
        a!(self.clutch);
        a!(self.steer);
        a!(self.tc_active);
        a!(self.abs_active);
        a!(self.headlights);
        a!(self.wipers);
        a!(self.pit_limiter);
        a!(self.ignition);
        a!(self.flag);
        a!(self.track_pct);
        a!(self.pos_x);
        a!(self.pos_z);
        a!(self.s1_ms);
        a!(self.s2_ms);
        a!(self.s3_ms);
        a!(self.bs1_ms);
        a!(self.bs2_ms);
        a!(self.bs3_ms);
        a!(self.battery_pct);
        a!(self.ers_state);
        // Tyre surface average (×4) then carcass core (×4). MUST stay in this order,
        // matching field_registry.json + parse_line (positional frame contract).
        a!(self.tt_avg_fl);
        a!(self.tt_avg_fr);
        a!(self.tt_avg_rl);
        a!(self.tt_avg_rr);
        a!(self.tt_carc_fl);
        a!(self.tt_carc_fr);
        a!(self.tt_carc_rl);
        a!(self.tt_carc_rr);
        // Tyre compound per corner (0 soft,1 med,2 hard,3 wet,-1 n/a).
        a!(self.comp_fl);
        a!(self.comp_fr);
        a!(self.comp_rl);
        a!(self.comp_rr);
        a!(self.tc_slip);
        a!(self.tc_cut);
        a!(self.virtual_energy);
        a!(self.ve_per_lap);
        a!(self.fuel_is_ve);
        s
    }
}

/// Byte cursor over the frame.
struct Cur<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Cur<'a> {
    fn peek(&self) -> u8 {
        if self.i < self.b.len() {
            self.b[self.i]
        } else {
            0
        }
    }

    /// Parse a non-negative integer at the cursor, advancing past the digits.
    /// Returns None if no digit is present (mirrors C `parse_uint` returning -1).
    fn parse_uint(&mut self) -> Option<i32> {
        if !self.peek().is_ascii_digit() {
            return None;
        }
        let mut v: i32 = 0;
        while self.peek().is_ascii_digit() {
            v = v * 10 + (self.peek() - b'0') as i32;
            self.i += 1;
        }
        Some(v)
    }

    /// Optional signed int: if a (possibly signed) number is present, store it in
    /// `out` and advance; if the field is empty, leave `out` and the cursor
    /// untouched (do NOT consume a lone sign). Port of C `parse_int_opt`.
    fn parse_int_opt(&mut self, out: &mut i32) {
        let mut j = self.i;
        let mut sign = 1;
        if j < self.b.len() && (self.b[j] == b'-' || self.b[j] == b'+') {
            if self.b[j] == b'-' {
                sign = -1;
            }
            j += 1;
        }
        if j >= self.b.len() || !self.b[j].is_ascii_digit() {
            return; // empty field -> keep default, do not consume the sign
        }
        let mut v: i32 = 0;
        while j < self.b.len() && self.b[j].is_ascii_digit() {
            v = v * 10 + (self.b[j] - b'0') as i32;
            j += 1;
        }
        *out = sign * v;
        self.i = j;
    }

    /// Consume a single ';' separator. Returns false if not at one.
    fn expect_sep(&mut self) -> bool {
        if self.peek() != b';' {
            return false;
        }
        self.i += 1;
        true
    }

    /// If at ';', consume it and parse an optional signed int into `field`.
    /// Returns false once there are no more separators. Port of C `opt_field`.
    fn opt_field(&mut self, field: &mut i32) -> bool {
        if self.peek() != b';' {
            return false;
        }
        self.i += 1;
        self.parse_int_opt(field);
        true
    }
}

/// Parse a single frame line into a `Telemetry`. Returns `None` on a malformed
/// frame (first 4 fields invalid). Missing trailing fields keep their defaults.
/// Exact behavioral port of `simhub_parse_line`.
pub fn parse_line(line: &str) -> Option<Telemetry> {
    let b = line.as_bytes();

    // Resync: skip to the sentinel.
    let mut i = 0;
    while i < b.len() && b[i] != b'$' {
        i += 1;
    }
    if i >= b.len() || b[i] != b'$' {
        return None;
    }
    i += 1; // consume '$'
    let mut c = Cur { b, i };

    // Default tyre wear to "full" so an absent field doesn't read as worn out.
    let mut t = Telemetry {
        tw_fl: 100,
        tw_fr: 100,
        tw_rl: 100,
        tw_rr: 100,
        ..Telemetry::default()
    };

    // ---- Required core fields ----
    // Gear may arrive as a letter (N/R/1..9) or numeric (0 = neutral, -1 =
    // reverse, 1..9 = gears). Accept both.
    let g = c.peek();
    if g == b'-' {
        t.gear = b'R';
        c.i += 1;
        while c.peek().is_ascii_digit() {
            c.i += 1;
        }
    } else if g == b'0' {
        t.gear = b'N';
        c.i += 1;
    } else if g == b'R' || g == b'N' || (b'1'..=b'9').contains(&g) {
        t.gear = g;
        c.i += 1;
    } else {
        return None;
    }

    if !c.expect_sep() {
        return None;
    }
    match c.parse_uint() {
        Some(v) => t.speed_kmh = v,
        None => return None,
    }
    if !c.expect_sep() {
        return None;
    }
    match c.parse_uint() {
        Some(v) => t.rpm = v,
        None => return None,
    }
    if !c.expect_sep() {
        return None;
    }
    match c.parse_uint() {
        Some(v) => t.max_rpm = v,
        None => return None,
    }

    // ---- Optional extended fields, in fixed order ----
    // Stops at the first absent separator; remaining fields keep defaults.
    'fields: {
        if !c.opt_field(&mut t.shift_rpm) { break 'fields; }
        if !c.opt_field(&mut t.cur_lap_ms) { break 'fields; }
        if !c.opt_field(&mut t.last_lap_ms) { break 'fields; }
        if !c.opt_field(&mut t.best_lap_ms) { break 'fields; }
        if !c.opt_field(&mut t.pb_lap_ms) { break 'fields; }
        if !c.opt_field(&mut t.est_lap_ms) { break 'fields; }
        if !c.opt_field(&mut t.delta_ms) { break 'fields; }
        if !c.opt_field(&mut t.position) { break 'fields; }
        if !c.opt_field(&mut t.field_size) { break 'fields; }
        if !c.opt_field(&mut t.laps_done) { break 'fields; }
        if !c.opt_field(&mut t.total_laps) { break 'fields; }
        if !c.opt_field(&mut t.laps_left) { break 'fields; }
        if !c.opt_field(&mut t.water_c) { break 'fields; }
        if !c.opt_field(&mut t.oil_c) { break 'fields; }
        if !c.opt_field(&mut t.oil_press_x10) { break 'fields; }
        if !c.opt_field(&mut t.boost_kpa) { break 'fields; }
        if !c.opt_field(&mut t.tc) { break 'fields; }
        if !c.opt_field(&mut t.abs) { break 'fields; }
        if !c.opt_field(&mut t.brake_bias_x10) { break 'fields; }
        if !c.opt_field(&mut t.fuel_dl) { break 'fields; }
        if !c.opt_field(&mut t.fuel_cap_dl) { break 'fields; }
        if !c.opt_field(&mut t.fuel_per_lap_ml) { break 'fields; }
        if !c.opt_field(&mut t.fuel_laps_x10) { break 'fields; }
        if !c.opt_field(&mut t.tt_fl_i) { break 'fields; }
        if !c.opt_field(&mut t.tt_fl_m) { break 'fields; }
        if !c.opt_field(&mut t.tt_fl_o) { break 'fields; }
        if !c.opt_field(&mut t.tt_fr_i) { break 'fields; }
        if !c.opt_field(&mut t.tt_fr_m) { break 'fields; }
        if !c.opt_field(&mut t.tt_fr_o) { break 'fields; }
        if !c.opt_field(&mut t.tt_rl_i) { break 'fields; }
        if !c.opt_field(&mut t.tt_rl_m) { break 'fields; }
        if !c.opt_field(&mut t.tt_rl_o) { break 'fields; }
        if !c.opt_field(&mut t.tt_rr_i) { break 'fields; }
        if !c.opt_field(&mut t.tt_rr_m) { break 'fields; }
        if !c.opt_field(&mut t.tt_rr_o) { break 'fields; }
        if !c.opt_field(&mut t.tp_fl) { break 'fields; }
        if !c.opt_field(&mut t.tp_fr) { break 'fields; }
        if !c.opt_field(&mut t.tp_rl) { break 'fields; }
        if !c.opt_field(&mut t.tp_rr) { break 'fields; }
        if !c.opt_field(&mut t.tw_fl) { break 'fields; }
        if !c.opt_field(&mut t.tw_fr) { break 'fields; }
        if !c.opt_field(&mut t.tw_rl) { break 'fields; }
        if !c.opt_field(&mut t.tw_rr) { break 'fields; }
        if !c.opt_field(&mut t.bt_fl) { break 'fields; }
        if !c.opt_field(&mut t.bt_fr) { break 'fields; }
        if !c.opt_field(&mut t.bt_rl) { break 'fields; }
        if !c.opt_field(&mut t.bt_rr) { break 'fields; }
        if !c.opt_field(&mut t.throttle) { break 'fields; }
        if !c.opt_field(&mut t.brake) { break 'fields; }
        if !c.opt_field(&mut t.clutch) { break 'fields; }
        if !c.opt_field(&mut t.steer) { break 'fields; }
        if !c.opt_field(&mut t.tc_active) { break 'fields; }
        if !c.opt_field(&mut t.abs_active) { break 'fields; }
        if !c.opt_field(&mut t.headlights) { break 'fields; }
        if !c.opt_field(&mut t.wipers) { break 'fields; }
        if !c.opt_field(&mut t.pit_limiter) { break 'fields; }
        if !c.opt_field(&mut t.ignition) { break 'fields; }
        if !c.opt_field(&mut t.flag) { break 'fields; }
        if !c.opt_field(&mut t.track_pct) { break 'fields; }
        if !c.opt_field(&mut t.pos_x) { break 'fields; }
        if !c.opt_field(&mut t.pos_z) { break 'fields; }
        if !c.opt_field(&mut t.s1_ms) { break 'fields; }
        if !c.opt_field(&mut t.s2_ms) { break 'fields; }
        if !c.opt_field(&mut t.s3_ms) { break 'fields; }
        if !c.opt_field(&mut t.bs1_ms) { break 'fields; }
        if !c.opt_field(&mut t.bs2_ms) { break 'fields; }
        if !c.opt_field(&mut t.bs3_ms) { break 'fields; }
        if !c.opt_field(&mut t.battery_pct) { break 'fields; }
        if !c.opt_field(&mut t.ers_state) { break 'fields; }
        // Tyre surface average (×4) then carcass core (×4) — same order as to_frame.
        if !c.opt_field(&mut t.tt_avg_fl) { break 'fields; }
        if !c.opt_field(&mut t.tt_avg_fr) { break 'fields; }
        if !c.opt_field(&mut t.tt_avg_rl) { break 'fields; }
        if !c.opt_field(&mut t.tt_avg_rr) { break 'fields; }
        if !c.opt_field(&mut t.tt_carc_fl) { break 'fields; }
        if !c.opt_field(&mut t.tt_carc_fr) { break 'fields; }
        if !c.opt_field(&mut t.tt_carc_rl) { break 'fields; }
        if !c.opt_field(&mut t.tt_carc_rr) { break 'fields; }
        if !c.opt_field(&mut t.comp_fl) { break 'fields; }
        if !c.opt_field(&mut t.comp_fr) { break 'fields; }
        if !c.opt_field(&mut t.comp_rl) { break 'fields; }
        if !c.opt_field(&mut t.comp_rr) { break 'fields; }
        if !c.opt_field(&mut t.tc_slip) { break 'fields; }
        if !c.opt_field(&mut t.tc_cut) { break 'fields; }
        if !c.opt_field(&mut t.virtual_energy) { break 'fields; }
        if !c.opt_field(&mut t.ve_per_lap) { break 'fields; }
        if !c.opt_field(&mut t.fuel_is_ve) { break 'fields; }
    }

    // Trailing characters must only be separators / terminators / whitespace.
    while matches!(c.peek(), b';' | b'\r' | b'\n' | b' ' | b'\t') {
        c.i += 1;
    }
    if c.i < b.len() {
        return None;
    }

    Some(t)
}
