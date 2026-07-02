//! Screenshot mode: render each app page to a PNG (for the README / docs).
//!
//! `pith-dashboard --shots [dir]` boots the UI with the seeded demo data (no
//! device/UDP loops, no network), then walks the sidebar pages, snapshots each
//! via `Window::take_snapshot()` and writes `<dir>/<page>.png`. PNG is encoded
//! with `flate2` (already a dep) so there's no new crate.

use std::cell::Cell;
use std::io::Write;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use slint::ComponentHandle;

use crate::{AppState, AppWindow, Page};

/// The pages to capture, paired with the output file stem.
const PAGES: &[(Page, &str)] = &[
    (Page::Overview, "overview"),
    (Page::Race, "screens"),
    (Page::Shift, "shift-lights"),
    (Page::Cars, "car-library"),
    (Page::Udp, "telemetry-udp"),
    (Page::Firmware, "firmware"),
    (Page::Device, "device"),
    (Page::Handbrake, "handbrake"),
];

/// Run the dashboard in screenshot mode and exit. Returns the number written.
pub fn run(ui: &AppWindow, rt: &tokio::runtime::Runtime, dir: &str) {
    // Lightweight init: seed the demo + push UI models, but don't spawn the
    // device/UDP/game loops or hit the network (keeps it offline + side-effect
    // free, and the demo data makes every widget look populated).
    let _ctx = crate::app::init_screenshot(ui, rt);
    let app = ui.global::<AppState>();
    app.set_connected(true); // drop the "no device" gate so the body is visible
    app.set_conn_detail("Connected · demo".into());
    app.set_page(PAGES[0].0);
    ui.window().set_size(slint::PhysicalSize::new(1320, 860));

    let out = dir.to_string();
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("screenshots: cannot create {out}: {e}");
        return;
    }

    let idx = Rc::new(Cell::new(0usize));
    let warmed = Rc::new(Cell::new(false));
    let shooting = Rc::new(Cell::new(true)); // page[0] is set; first action is to shoot it
    let weak = ui.as_weak();
    let timer = slint::Timer::default();
    // Two-phase per page so a page change always gets a full interval to render
    // before its snapshot (a Slint property change → tree re-eval is lazy, so
    // set-then-immediately-snapshot captures the previous page). Phases alternate:
    //   shoot → save current page, switch to next, request redraw
    //   render → (just wait one tick for the new page to paint)
    timer.start(slint::TimerMode::Repeated, Duration::from_millis(400), {
        let idx = idx.clone();
        let warmed = warmed.clone();
        let shooting = shooting.clone();
        let out = out.clone();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if !warmed.get() {
                warmed.set(true);
                ui.window().request_redraw();
                return; // let page 0 paint before its first snapshot
            }
            if !shooting.get() {
                // "render" tick: the page set last tick is now painting.
                shooting.set(true);
                ui.window().request_redraw();
                return;
            }
            let i = idx.get();
            let (_, name) = PAGES[i];
            match ui.window().take_snapshot() {
                Ok(buf) => {
                    let path = format!("{out}/{name}.png");
                    if let Err(e) =
                        save_png(Path::new(&path), buf.width(), buf.height(), buf.as_bytes())
                    {
                        eprintln!("screenshots: write {path} failed: {e}");
                    } else {
                        println!(
                            "screenshots: wrote {path} ({}×{})",
                            buf.width(),
                            buf.height()
                        );
                    }
                }
                Err(e) => eprintln!("screenshots: snapshot {name} failed: {e}"),
            }
            let next = i + 1;
            if next < PAGES.len() {
                idx.set(next);
                shooting.set(false); // next tick is a render-wait for the new page
                ui.global::<AppState>().set_page(PAGES[next].0);
                ui.window().request_redraw();
            } else {
                let _ = slint::quit_event_loop();
            }
        }
    });

    let _ = ui.run();
    let _ = timer; // keep the timer alive across the event loop
}

/// Encode RGBA8 pixels as a PNG (8-bit, color type 6), zlib via flate2.
fn save_png(path: &Path, w: u32, h: u32, rgba: &[u8]) -> std::io::Result<()> {
    let mut png: Vec<u8> = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // bit depth, RGBA, deflate, no filter, no interlace
    chunk(&mut png, b"IHDR", &ihdr);

    // Filter byte 0 (None) prepended to each scanline, then zlib-compress.
    let stride = (w as usize) * 4;
    let mut raw = Vec::with_capacity((stride + 1) * h as usize);
    for y in 0..h as usize {
        raw.push(0);
        raw.extend_from_slice(&rgba[y * stride..y * stride + stride]);
    }
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    enc.write_all(&raw)?;
    let idat = enc.finish()?;
    chunk(&mut png, b"IDAT", &idat);
    chunk(&mut png, b"IEND", &[]);

    std::fs::write(path, &png)
}

/// Append one PNG chunk (length + type + data + CRC32).
fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = flate2::Crc::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.sum().to_be_bytes());
}
