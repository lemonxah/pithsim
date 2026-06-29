//! Shared logic for the in-prefix shm tools. Windows-only (reads Win32 shared
//! memory); the parsers + `$`-frame serializer come from `pith-core`.

#[cfg(windows)]
pub mod win;

/// Candidate Win32 mapping names per game (tried in order). The `/dev/shm`
/// filename the bridge writes is the last path segment (after `Local\`/`$`).
pub const AC_PHYSICS: &[&str] = &["Local\\acpmf_physics", "acpmf_physics"];
pub const AC_GRAPHICS: &[&str] = &["Local\\acpmf_graphics", "acpmf_graphics"];
pub const AC_STATIC: &[&str] = &["Local\\acpmf_static", "acpmf_static"];
pub const ACEVO_PHYSICS: &[&str] = &["Local\\acevo_pmf_physics", "acevo_pmf_physics"];
pub const ACEVO_GRAPHICS: &[&str] = &["Local\\acevo_pmf_graphics", "acevo_pmf_graphics"];
pub const ACEVO_STATIC: &[&str] = &["Local\\acevo_pmf_static", "acevo_pmf_static"];
pub const R3E: &[&str] = &["$R3E", "Local\\$R3E"];
pub const RF2_TELEMETRY: &[&str] = &["$rFactor2SMMP_Telemetry$", "Local\\$rFactor2SMMP_Telemetry$"];
pub const RF2_SCORING: &[&str] = &["$rFactor2SMMP_Scoring$", "Local\\$rFactor2SMMP_Scoring$"];
pub const RF2_EXTENDED: &[&str] = &["$rFactor2SMMP_Extended$", "Local\\$rFactor2SMMP_Extended$"];
/// LMU's own native shared memory (1.3+): live TC/ABS levels, the game's delta,
/// virtual energy, battery — none of which TheIronWolf's rF2 plugin exposes live.
pub const LMU_DATA: &[&str] = &["LMU_Data", "Local\\LMU_Data", "Global\\LMU_Data"];

/// What the shim sends each tick: the `$`-frame plus optional identity and an
/// optional `@REL` relatives/standings line.
pub struct ShimRead {
    pub frame: String,
    pub label: &'static str,
    pub car: Option<String>,
    pub track: Option<String>,
    pub relatives: Option<String>,
}

/// Read whichever sim is currently exposing shared memory and return a ready-to-
/// send `$`-frame + identity. `None` if nothing is running.
#[cfg(windows)]
pub fn read_frame() -> Option<ShimRead> {
    use pith_core::shm;
    // rF2 / LMU needs both buffers (to match the player car + read names).
    if let (Some(t), Some(s)) = (win::read_any(RF2_TELEMETRY), win::read_any(RF2_SCORING)) {
        if let Some(mut tel) = shm::parse_rf2(&t, &s) {
            if let Some(ext) = win::read_any(RF2_EXTENDED) {
                shm::apply_rf2_extended(&mut tel, &ext); // static TC/ABS assist setting
            }
            // LMU 1.3+ native map: overlays LIVE TC/ABS levels + the game's own delta.
            if let Some(lmu) = win::read_any(LMU_DATA) {
                shm::apply_lmu_native(&mut tel, &lmu);
            }
            let (car, track) = shm::rf2_identity(&t, &s);
            let relatives = pith_core::relatives::parse_rf2_relatives(&s).map(|r| r.to_wire());
            return Some(ShimRead { frame: tel.to_frame(), label: "rF2/LMU", car, track, relatives });
        }
    }
    // AC / ACC / AC EVO: physics + graphics (+ static for identity).
    for (phys, graph, stat, label) in [
        (ACEVO_PHYSICS, ACEVO_GRAPHICS, ACEVO_STATIC, "AC EVO"),
        (AC_PHYSICS, AC_GRAPHICS, AC_STATIC, "AC/ACC"),
    ] {
        if let Some(pb) = win::read_any(phys) {
            if let Some(mut tel) = shm::parse_ac_physics(&pb) {
                if let Some(gb) = win::read_any(graph) {
                    shm::apply_acc_graphics(&mut tel, &gb);
                }
                // Static-page identity is AC/ACC only (EVO's layout differs).
                let (car, track) = if label == "AC/ACC" {
                    win::read_any(stat)
                        .map(|sb| shm::ac_static_identity(&sb))
                        .unwrap_or((None, None))
                } else {
                    (None, None)
                };
                return Some(ShimRead { frame: tel.to_frame(), label, car, track, relatives: None });
            }
        }
    }
    if let Some(b) = win::read_any(R3E) {
        if let Some(tel) = shm::parse_r3e(&b) {
            return Some(ShimRead { frame: tel.to_frame(), label: "RaceRoom", car: None, track: None, relatives: None });
        }
    }
    None
}

/// Mappings the bridge copies to `/dev/shm` (Win32 name candidates → dest path
/// under Wine's `Z:\dev\shm`, which is the host's `/dev/shm`).
#[cfg(windows)]
pub const COPY_BLOCKS: &[(&[&str], &str)] = &[
    (AC_PHYSICS, "Z:\\dev\\shm\\acpmf_physics"),
    (AC_GRAPHICS, "Z:\\dev\\shm\\acpmf_graphics"),
    (AC_STATIC, "Z:\\dev\\shm\\acpmf_static"),
    (ACEVO_PHYSICS, "Z:\\dev\\shm\\acevo_pmf_physics"),
    (ACEVO_GRAPHICS, "Z:\\dev\\shm\\acevo_pmf_graphics"),
    (ACEVO_STATIC, "Z:\\dev\\shm\\acevo_pmf_static"),
    (R3E, "Z:\\dev\\shm\\$R3E"),
    (RF2_TELEMETRY, "Z:\\dev\\shm\\$rFactor2SMMP_Telemetry$"),
    (RF2_SCORING, "Z:\\dev\\shm\\$rFactor2SMMP_Scoring$"),
    (RF2_EXTENDED, "Z:\\dev\\shm\\$rFactor2SMMP_Extended$"),
];
