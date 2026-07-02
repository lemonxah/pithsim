slint::include_modules!();

mod app;
mod callbacks;
mod catalog;
mod clipboard;
mod ctx;
// Device transport (HID + serial + OTA) moved to the shared `pith-device` crate
// so the `pith-flash` CLI reuses the exact same code path. Alias it as `device`
// so the existing `crate::device::…` references keep resolving unchanged.
use pith_device as device;
mod firmware;
mod games;
mod hb;
mod loops;
mod net;
mod paths;
mod persist;
mod screenshot;
mod state;
mod telemetry;
mod trackmap;
mod tray;
mod ui_bridge;
mod util;

use slint::ComponentHandle;
use std::sync::atomic::Ordering;
use std::time::Duration;

fn main() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("build tokio runtime");

    let _ = slint::set_xdg_app_id("pith-dashboard");

    let ui = AppWindow::new().expect("create window");

    // Screenshot mode: `pith-dashboard --shots [dir]` renders each page to a PNG
    // (for the README/docs) and exits.
    let args: Vec<String> = std::env::args().collect();
    if let Some(p) = args.iter().position(|a| a == "--shots") {
        let dir = args
            .get(p + 1)
            .map(String::as_str)
            .unwrap_or("docs/screenshots");
        screenshot::run(&ui, &rt, dir);
        return;
    }

    let ctx = app::init(&ui, &rt);

    let tray_ok = rt.block_on(tray::start(ui.as_weak()));
    ctx.tray_active.store(tray_ok, Ordering::SeqCst);
    if tray_ok {
        let w = ui.as_weak();
        ui.window().on_close_requested(move || {
            tray::hide_window(&w);
            slint::CloseRequestResponse::HideWindow
        });
    }

    let fw_timer = slint::Timer::default();
    {
        let c = ctx.clone();
        fw_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(30 * 60),
            move || {
                net::releases::fetch_firmware_releases(&c);
            },
        );
    }

    ui.run().expect("run event loop");

    ctx.running.store(false, Ordering::SeqCst);
    let _ = &fw_timer;
}
