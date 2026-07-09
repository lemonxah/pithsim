use slint::ComponentHandle;
use std::sync::Arc;

use serde_json::Value;

use crate::ctx::Ctx;
use crate::firmware::ota::start_ota_from_bin_path;
use crate::firmware::{semver_cmp, FIRMWARE_RELEASES_URL};
use crate::net::http::{http_download_file, http_get};
use crate::paths::cache_dir;
use crate::state::FwRelease;
use crate::ui_bridge::firmware::{recompute_update_available, update_release_board_match};
use crate::ui_bridge::{model, sstr};
use crate::{Firmware, FwState};

/// Choose the release image for the current board: prefer an exact board-id
/// match, else any image built for the same chip family. The firmware is one
/// binary per chip (board pins are set at runtime via @PINS), so the esp32s3
/// image works on every esp32s3 board. Returns (download_url, exact_match).
fn pick_bin(s: &crate::state::State, rel: &FwRelease) -> Option<(String, bool)> {
    let board = s.cur_board_id();
    if let Some(u) = rel.board_bin.get(&board) {
        return Some((u.clone(), true));
    }
    let chip = s.cur_board().target.clone();
    rel.board_bin.iter().find_map(|(bid, u)| {
        let bin_chip = s
            .boards
            .iter()
            .find(|b| &b.id == bid)
            .map(|b| b.target.as_str());
        (bin_chip == Some(chip.as_str())).then(|| (u.clone(), false))
    })
}

pub fn fetch_firmware_releases(ctx: &Arc<Ctx>) {
    ctx.ui_run(|u| {
        let fw = u.global::<Firmware>();
        fw.set_fetching_releases(true);
        fw.set_releases_status(sstr("Checking GitHub for releases…"));
    });
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let (body, code) = http_get(FIRMWARE_RELEASES_URL).await;
        // Every device's firmware versions independently on its own release
        // stream, told apart by tag prefix: ddu-vX.Y.Z is the DDU,
        // handbrake-vX.Y.Z the handbrake. The prefix is stripped so the
        // version reads + compares like the device's `X.Y.Z` from @CAP.
        let mut rels: Vec<FwRelease> = Vec::new();
        let mut hb_rels: Vec<FwRelease> = Vec::new();
        let mut pedals_rels: Vec<FwRelease> = Vec::new();
        if let Ok(j) = serde_json::from_str::<Value>(&body) {
            if let Some(arr) = j.as_array() {
                for r in arr {
                    if r.get("draft").and_then(|x| x.as_bool()).unwrap_or(false) {
                        continue;
                    }
                    let raw_tag = r.get("tag_name").and_then(|x| x.as_str()).unwrap_or("");
                    // Each device firmwares on its own release stream, told apart
                    // by tag prefix; the asset filename prefix identifies the bin.
                    let (stream, asset_prefix, tag) =
                        if let Some(t) = raw_tag.strip_prefix("handbrake-") {
                            ("hb", "pith-hb-", t)
                        } else if let Some(t) = raw_tag.strip_prefix("pedals-") {
                            ("pedals", "pith-pedals-", t)
                        } else if let Some(t) = raw_tag.strip_prefix("ddu-") {
                            ("ddu", "pithddu-", t)
                        } else {
                            continue; // dashboard-v* etc.
                        };
                    let mut fr = FwRelease {
                        tag: tag.to_string(),
                        board_bin: Default::default(),
                        hb_bin: Default::default(),
                        pedals_bin: Default::default(),
                    };
                    if let Some(assets) = r.get("assets").and_then(|a| a.as_array()) {
                        for a in assets {
                            let name = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
                            if !name.ends_with(".bin") {
                                continue;
                            }
                            let url = a
                                .get("browser_download_url")
                                .and_then(|x| x.as_str())
                                .unwrap_or("")
                                .to_string();
                            let Some(board) = name
                                .strip_prefix(asset_prefix)
                                .and_then(|b| b.strip_suffix(".bin"))
                                .filter(|b| !b.is_empty())
                            else {
                                continue;
                            };
                            match stream {
                                "hb" => fr.hb_bin.insert(board.to_string(), url),
                                "pedals" => fr.pedals_bin.insert(board.to_string(), url),
                                _ => fr.board_bin.insert(board.to_string(), url),
                            };
                        }
                    }
                    if fr.tag.is_empty() {
                        continue;
                    }
                    match stream {
                        "hb" if !fr.hb_bin.is_empty() => hb_rels.push(fr),
                        "pedals" if !fr.pedals_bin.is_empty() => pedals_rels.push(fr),
                        "ddu" if !fr.board_bin.is_empty() => rels.push(fr),
                        _ => {}
                    }
                }
            }
        }
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let mut s = ctx2.lock();
            s.releases = rels;
            s.releases
                .sort_by(|a, b| semver_cmp(&b.tag, &a.tag).cmp(&0));
            s.hb_releases = hb_rels;
            s.hb_releases
                .sort_by(|a, b| semver_cmp(&b.tag, &a.tag).cmp(&0));
            s.pedals_releases = pedals_rels;
            s.pedals_releases
                .sort_by(|a, b| semver_cmp(&b.tag, &a.tag).cmp(&0));
            let fw = u.global::<Firmware>();
            let labels: Vec<slint::SharedString> = s
                .releases
                .iter()
                .map(|r| sstr(&format!("{}  ·  {} boards", r.tag, r.board_bin.len())))
                .collect();
            fw.set_releases(model(labels));
            if fw.get_sel_release() >= s.releases.len() as i32 {
                fw.set_sel_release(0);
            }
            fw.set_fetching_releases(false);
            fw.set_releases_status(sstr(&if s.releases.is_empty() {
                if code == 0 {
                    "Offline — couldn't reach GitHub".to_string()
                } else {
                    "No published releases found".to_string()
                }
            } else {
                format!(
                    "{} release(s) · latest {}",
                    s.releases.len(),
                    s.releases[0].tag
                )
            }));
            update_release_board_match(&u, &s);
            recompute_update_available(&u, &s);
            crate::hb::recompute_hb_update(&u, &s);
            crate::pedals::recompute_pedals_update(&u, &s);
        });
    });
}

pub fn flash_selected_release(ctx: &Arc<Ctx>) {
    let ui = match ctx.ui.upgrade() {
        Some(u) => u,
        None => return,
    };
    let fw = ui.global::<Firmware>();
    let i = fw.get_sel_release();
    let connected = ctx.dash().connected();
    let (tag, url, board, valid, has_board, exact, chip) = {
        let s = ctx.lock();
        let valid = i >= 0 && (i as usize) < s.releases.len();
        if !valid {
            (
                String::new(),
                String::new(),
                String::new(),
                false,
                false,
                true,
                String::new(),
            )
        } else {
            let board = s.cur_board_id();
            let chip = s.cur_board().target.clone();
            match pick_bin(&s, &s.releases[i as usize]) {
                Some((url, exact)) => (
                    s.releases[i as usize].tag.clone(),
                    url,
                    board,
                    true,
                    true,
                    exact,
                    chip,
                ),
                None => (
                    s.releases[i as usize].tag.clone(),
                    String::new(),
                    board,
                    true,
                    false,
                    true,
                    chip,
                ),
            }
        }
    };
    if !valid {
        fw.set_releases_status(sstr("Pick a version first"));
        return;
    }
    if !connected {
        fw.set_releases_status(sstr("Connect the device to flash a release (OTA)"));
        return;
    }
    if !has_board {
        // No image for this chip family at all (e.g. the esp32s2 boards, which the
        // Rust firmware doesn't target yet).
        fw.set_releases_status(sstr(&format!(
            "{tag} has no {chip} build yet — build from source below"
        )));
        return;
    }
    if !exact {
        // A different board of the same chip; the image is universal (pins are set
        // at runtime via @PINS), so flashing it is safe.
        fw.set_status_line(sstr(&format!(
            "No {board}-specific image; installing the compatible {chip} build."
        )));
    }
    fw.set_releases_status(sstr(&format!("Downloading {tag} for {board}…")));
    fw.set_status_line(sstr(&format!("Downloading {tag} ({board}) from GitHub…")));
    fw.set_state(FwState::Downloading);
    fw.set_progress(0.0);

    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let out = cache_dir().join(format!("pithddu-{board}-{tag}.bin"));
        let pc = ctx.clone();
        let ok = http_download_file(&url, &out, move |frac| {
            pc.ui_run(move |u| u.global::<Firmware>().set_progress(frac as f32));
        })
        .await;
        let exists = out.exists();
        let out_str = out.to_string_lossy().to_string();
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let fw = u.global::<Firmware>();
            if !ok || !exists {
                fw.set_state(FwState::Failure);
                fw.set_releases_status(sstr("Download failed"));
                return;
            }
            fw.set_bin_path(sstr(&out_str));
            fw.set_releases_status(sstr(&format!("Flashing {tag} over USB…")));
            start_ota_from_bin_path(&ctx2);
        });
    });
}
