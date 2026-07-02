//! Native Linux shared-memory telemetry reader.
//!
//! Sims expose telemetry via Windows named shared memory. Under Proton/Wine those
//! mappings are anonymous (pagefile-backed) and NOT visible at a stable
//! `/dev/shm` path — so this reader only works when an in-prefix **bridge**
//! (`simshmbridge` / our `pith-shm-bridge`) re-backs the mapping into a real
//! `/dev/shm/<name>` file. When that file exists we read + parse it directly,
//! giving full physics (incl. RPM → shift-lights) with no SimHub and no plugin.
//! See `SHARED_MEMORY.md`.
//!
//! The struct parsers live in `pith_sim::shm` (shared with the in-prefix tools);
//! this module only does the Linux `/dev/shm` discovery + reading.

use pith_core::simhub::Telemetry;

/// One parsed shared-memory snapshot: telemetry plus optional identity (car model
/// → library/LED match, track → self-learned map).
pub struct ShmRead {
    pub telem: Telemetry,
    pub label: &'static str,
    pub car: Option<String>,
    pub track: Option<String>,
    /// Optional diagnostic line(s) for the GUI device log (rF2/LMU temp+flag probe).
    pub debug: Option<String>,
}

/// Scan `/dev/shm` once and return the first sim block we can parse. rF2/LMU and
/// AC/ACC read multiple pages (telemetry+scoring, physics+graphics) to fill the
/// full field set + identity.
pub fn read_once() -> Option<ShmRead> {
    let entries: Vec<(String, std::path::PathBuf)> = match std::fs::read_dir("/dev/shm") {
        Ok(rd) => rd
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
            .collect(),
        Err(_) => return None,
    };
    let find = |needle: &str| {
        entries
            .iter()
            .find(|(n, _)| n.contains(needle))
            .map(|(_, p)| p)
    };
    let read = |needle: &str| find(needle).and_then(|p| std::fs::read(p).ok());

    // rF2 / LMU: telemetry + scoring (scoring also carries car/track names).
    if let (Some(tb), Some(sb)) = (read("rFactor2SMMP_Telemetry"), read("rFactor2SMMP_Scoring")) {
        if let Some(mut t) = crate::shm::parse_rf2(&tb, &sb) {
            // Extended buffer (if the bridge mirrors it) carries the static TC/ABS.
            if let Some(eb) = read("rFactor2SMMP_Extended") {
                crate::shm::apply_rf2_extended(&mut t, &eb);
            }
            // LMU native map (if the bridge mirrors it) → LIVE TC/ABS + game delta.
            let lb = read("LMU_Data");
            if let Some(lb) = &lb {
                crate::shm::apply_lmu_native(&mut t, lb);
            }
            let (car, track) = crate::shm::rf2_identity(&tb, &sb);
            let debug = Some(crate::shm::rf2_lmu_debug(&tb, &sb, lb.as_deref()));
            return Some(ShmRead {
                telem: t,
                label: "rF2 / LMU (shm)",
                car,
                track,
                debug,
            });
        }
    }
    // AC / ACC / AC EVO: physics + graphics (+ static for identity).
    for (phys, graph, stat, label) in [
        (
            "acevo_pmf_physics",
            "acevo_pmf_graphics",
            "acevo_pmf_static",
            "AC EVO (shm)",
        ),
        (
            "acpmf_physics",
            "acpmf_graphics",
            "acpmf_static",
            "AC/ACC (shm)",
        ),
    ] {
        let evo = label == "AC EVO (shm)";
        if let Some(pb) = read(phys) {
            if let Some(mut t) = crate::shm::parse_ac_physics(&pb) {
                let gb = read(graph);
                // The ACC graphics offsets are ACC-only — EVO's
                // SPageFileGraphicEvo is a different struct (narrow chars,
                // driver/car strings @3020+), so applying them there read junk.
                if !evo {
                    if let Some(gb) = &gb {
                        crate::shm::apply_acc_graphics(&mut t, gb);
                    }
                }
                let (car, track) = if evo {
                    crate::shm::acevo_identity(
                        &read(stat).unwrap_or_default(),
                        &gb.unwrap_or_default(),
                    )
                } else {
                    read(stat)
                        .map(|sb| crate::shm::ac_static_identity(&sb))
                        .unwrap_or((None, None))
                };
                return Some(ShmRead {
                    telem: t,
                    label,
                    car,
                    track,
                    debug: None,
                });
            }
        }
    }
    // RaceRoom (single buffer; identity is numeric only).
    if let Some(b) = read("R3E") {
        if let Some(t) = crate::shm::parse_r3e(&b) {
            return Some(ShmRead {
                telem: t,
                label: "RaceRoom (shm)",
                car: None,
                track: None,
                debug: None,
            });
        }
    }
    None
}
