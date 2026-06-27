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

pub fn fetch_firmware_releases(ctx: &Arc<Ctx>) {
    ctx.ui_run(|u| {
        let fw = u.global::<Firmware>();
        fw.set_fetching_releases(true);
        fw.set_releases_status(sstr("Checking GitHub for releases…"));
    });
    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        let (body, code) = http_get(FIRMWARE_RELEASES_URL).await;
        let mut rels: Vec<FwRelease> = Vec::new();
        if let Ok(j) = serde_json::from_str::<Value>(&body) {
            if let Some(arr) = j.as_array() {
                for r in arr {
                    if r.get("draft").and_then(|x| x.as_bool()).unwrap_or(false) {
                        continue;
                    }
                    // Monorepo tags are `firmware-vX.Y.Z`; strip the stream prefix so
                    // the version reads + compares like the device's `X.Y.Z`.
                    let raw_tag = r.get("tag_name").and_then(|x| x.as_str()).unwrap_or("");
                    let mut fr = FwRelease {
                        tag: raw_tag
                            .strip_prefix("firmware-")
                            .unwrap_or(raw_tag)
                            .to_string(),
                        board_bin: Default::default(),
                    };
                    if let Some(assets) = r.get("assets").and_then(|a| a.as_array()) {
                        for a in assets {
                            let name = a.get("name").and_then(|x| x.as_str()).unwrap_or("");
                            if name.starts_with("pithddu-")
                                && name.len() > 12
                                && name.ends_with(".bin")
                            {
                                let board = name[8..name.len() - 4].to_string();
                                let url = a
                                    .get("browser_download_url")
                                    .and_then(|x| x.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                fr.board_bin.insert(board, url);
                            }
                        }
                    }
                    if !fr.tag.is_empty() && !fr.board_bin.is_empty() {
                        rels.push(fr);
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
    let (tag, url, board, valid, has_board) = {
        let s = ctx.lock();
        let valid = i >= 0 && (i as usize) < s.releases.len();
        if !valid {
            (String::new(), String::new(), String::new(), false, false)
        } else {
            let board = s.cur_board_id();
            let url = s.releases[i as usize].board_bin.get(&board).cloned();
            (
                s.releases[i as usize].tag.clone(),
                url.clone().unwrap_or_default(),
                board,
                true,
                url.is_some(),
            )
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
        fw.set_releases_status(sstr(&format!(
            "{tag} has no build for {board} — build it yourself below"
        )));
        return;
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
