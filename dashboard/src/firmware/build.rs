use slint::ComponentHandle;
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::ctx::Ctx;
use crate::firmware::FIRMWARE_GIT_URL;
use crate::paths::{default_firmware_path, file_size_str, repo_root};
use crate::ui_bridge::sstr;
use crate::Firmware;

fn parse_ninja(l: &str) -> Option<f32> {
    if l.starts_with('[') {
        let inner = &l[1..];
        if let Some(slash) = inner.find('/') {
            let a: i32 = inner[..slash].trim().parse().ok()?;
            let rest = &inner[slash + 1..];
            let end = rest
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(rest.len());
            let b: i32 = rest[..end].parse().ok()?;
            if b > 0 {
                return Some(a as f32 / b as f32);
            }
        }
    }
    None
}

fn parse_flash(l: &str) -> Option<f32> {
    if let (Some(pp), Some(pe)) = (l.find('('), l.find("%)")) {
        if pe > pp {
            let n = crate::util::atoi(&l[pp + 1..pe]);
            if (0..=100).contains(&n) {
                return Some(n as f32 / 100.0);
            }
        }
    }
    parse_ninja(l)
}

fn build_sh(root: &str, tail: &str) -> String {
    // The firmware is a Rust esp project in the monorepo's `firmware/` subdir.
    // Clone the monorepo if absent (NOT the retired pithddu-firmware repo),
    // fast-forward otherwise, then run `tail` in the firmware crate — its
    // rust-toolchain.toml selects the `esp` channel automatically.
    format!(
        "set -e; [ -d '{root}/.git' ] || git clone --depth 1 {FIRMWARE_GIT_URL} '{root}'; \
         git -C '{root}' pull --ff-only 2>/dev/null || true; \
         cd '{root}/firmware'; {tail}"
    )
}

async fn stream_cmd(ctx: &Arc<Ctx>, sh: &str, mut on_line: impl FnMut(&str)) -> (bool, bool) {
    let mut cmd = Command::new("bash");
    cmd.arg("-c").arg(format!("exec 2>&1\n{sh}"));
    cmd.stdout(Stdio::piped()).stdin(Stdio::null());
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(_) => return (false, false),
    };
    if let Some(id) = child.id() {
        ctx.build_pgid.store(id as i32, Ordering::SeqCst);
    }
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(l)) = lines.next_line().await {
        if ctx.build_cancel.load(Ordering::SeqCst) {
            break;
        }
        on_line(&l);
    }
    let status = child.wait().await.ok();
    ctx.build_pgid.store(-1, Ordering::SeqCst);
    let cancelled = ctx.build_cancel.load(Ordering::SeqCst);
    let ok = !cancelled && status.map(|s| s.success()).unwrap_or(false);
    (ok, cancelled)
}

pub fn start_firmware_build(ctx: &Arc<Ctx>) {
    let ui = match ctx.ui.upgrade() {
        Some(u) => u,
        None => return,
    };
    let fw = ui.global::<Firmware>();
    if fw.get_building() {
        return;
    }
    if !crate::firmware::can_build_firmware() {
        fw.set_build_status(sstr("ESP-IDF not found"));
        return;
    }
    let (board_id, board_name) = {
        let s = ctx.lock();
        let b = s.cur_board();
        (b.id.clone(), b.name.clone())
    };
    let root = repo_root().to_string_lossy().to_string();
    fw.set_building(true);
    fw.set_build_progress(0.0);
    fw.set_have_bin(false);
    fw.set_build_status(sstr(&format!("Building for {board_name}…")));
    fw.set_build_log(sstr("starting"));
    ctx.build_cancel.store(false, Ordering::SeqCst);

    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        // Cross-compile, then pack the app image the dashboard installs over OTA —
        // identical to the `firmware-v*` CI (`espflash save-image`).
        let tail = format!(
            "cargo build --release && \
             espflash save-image --chip esp32s3 \
               target/xtensa-esp32s3-espidf/release/pithddu pithddu-{board_id}.bin"
        );
        let sh = build_sh(&root, &tail);
        let (ok, cancelled) = {
            let cb_ctx = ctx.clone();
            stream_cmd(&ctx, &sh, move |l| {
                let prog = parse_flash(l);
                let tail: String = l.chars().take(80).collect();
                cb_ctx.ui_run(move |u| {
                    let fw = u.global::<Firmware>();
                    if let Some(p) = prog {
                        fw.set_build_progress(p);
                    }
                    if !tail.is_empty() {
                        fw.set_build_log(sstr(&tail));
                    }
                });
            })
            .await
        };
        let bin = if ok { default_firmware_path() } else { None };
        let size = bin
            .as_ref()
            .map(|b| file_size_str(std::path::Path::new(b)))
            .unwrap_or_else(|| "—".into());
        ctx.ui_run(move |u| {
            let fw = u.global::<Firmware>();
            fw.set_building(false);
            fw.set_have_bin(bin.is_some());
            if let Some(b) = &bin {
                fw.set_bin_path(sstr(b));
                fw.set_size(sstr(&size));
            }
            fw.set_build_progress(if ok { 1.0 } else { 0.0 });
            fw.set_build_status(sstr(&if cancelled {
                "Build cancelled".to_string()
            } else if ok {
                format!("Built firmware for {board_name} · {size}")
            } else {
                "Build failed — see terminal/console for details".to_string()
            }));
        });
    });
}

pub fn cancel_firmware_build(ctx: &Arc<Ctx>) {
    ctx.build_cancel.store(true, Ordering::SeqCst);
    let pgid = ctx.build_pgid.load(Ordering::SeqCst);
    #[cfg(unix)]
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
    }
    let _ = pgid;
}

pub fn start_serial_flash(ctx: &Arc<Ctx>, port: String, full_image: bool) {
    let ui = match ctx.ui.upgrade() {
        Some(u) => u,
        None => return,
    };
    let fw = ui.global::<Firmware>();
    if fw.get_flashing_serial() || fw.get_building() {
        return;
    }
    if !crate::firmware::can_build_firmware() {
        fw.set_serial_status(sstr("ESP-IDF not found"));
        return;
    }
    if port.is_empty() {
        fw.set_serial_status(sstr("Select a serial port"));
        return;
    }
    let (_board_id, board_name) = {
        let s = ctx.lock();
        let b = s.cur_board();
        (b.id.clone(), b.name.clone())
    };
    let root = repo_root().to_string_lossy().to_string();
    fw.set_flashing_serial(true);
    fw.set_serial_progress(0.0);
    fw.set_serial_status(sstr(&format!(
        "{} → {port} …",
        if full_image {
            "Full flash"
        } else {
            "App flash"
        }
    )));
    fw.set_serial_log(sstr("starting"));
    ctx.build_cancel.store(false, Ordering::SeqCst);

    let ctx = ctx.clone();
    ctx.clone().spawn(async move {
        // espflash flashes the freshly-built ELF (bootloader + partitions + app).
        let tail_cmd = format!(
            "cargo build --release && \
             espflash flash --chip esp32s3 --port '{port}' --baud 460800 \
               target/xtensa-esp32s3-espidf/release/pithddu"
        );
        let sh = build_sh(&root, &tail_cmd);
        let (ok, cancelled) = {
            let cb_ctx = ctx.clone();
            stream_cmd(&ctx, &sh, move |l| {
                let prog = parse_flash(l);
                let tail: String = l.chars().take(80).collect();
                cb_ctx.ui_run(move |u| {
                    let fw = u.global::<Firmware>();
                    if let Some(p) = prog {
                        fw.set_serial_progress(p);
                    }
                    if !tail.is_empty() {
                        fw.set_serial_log(sstr(&tail));
                    }
                });
            })
            .await
        };
        let pfx = if full_image {
            "Flashed full image to "
        } else {
            "Flashed app to "
        };
        ctx.ui_run(move |u| {
            let fw = u.global::<Firmware>();
            fw.set_flashing_serial(false);
            fw.set_serial_progress(if ok { 1.0 } else { 0.0 });
            fw.set_serial_status(sstr(&if cancelled {
                "Flash cancelled".to_string()
            } else if ok {
                format!("{pfx}{board_name} on {port}")
            } else {
                "Flash failed — wrong port, or hold BOOT + tap RESET to enter download mode"
                    .to_string()
            }));
        });
        let ctx2 = ctx.clone();
        ctx.ui_run(move |u| {
            let s = ctx2.lock();
            crate::ui_bridge::firmware::refresh_firmware_local(&u, &s);
        });
    });
}
