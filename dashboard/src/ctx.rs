use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

use crate::device::Dash;
use crate::state::State;
use crate::AppWindow;

/// Latest-wins outbox for the fire-and-forget device streams (telemetry
/// `$`-frames + `@REL` relatives). Producers (the UDP listener, connectors,
/// shm reader) just replace the slot and move on; ONE writer thread
/// ([`crate::loops::device_writer_loop`]) does the blocking HID pushes. A
/// wedged HID link then stalls only the writer — telemetry ingestion, the
/// merge, and the GUI preview keep running — and stale frames are simply
/// overwritten rather than queueing up.
#[derive(Default)]
pub struct DevOutbox {
    pub telem: Option<String>,
    pub rel: Option<String>,
}

pub struct Ctx {
    pub ui: slint::Weak<AppWindow>,
    pub state: Arc<Mutex<State>>,
    pub dash: Arc<Mutex<Dash>>,
    pub rt: tokio::runtime::Handle,
    pub running: Arc<AtomicBool>,
    pub ota_active: Arc<AtomicBool>,
    /// GUI "Simulate" toggle — when set, sim_loop streams a full animated test
    /// telemetry feed (every field) + cycles car shift-light profiles to the device.
    pub sim_active: Arc<AtomicBool>,
    pub busy: Arc<AtomicBool>,
    pub car_gen: Arc<std::sync::atomic::AtomicUsize>,
    pub build_cancel: Arc<AtomicBool>,
    pub build_pgid: Arc<std::sync::atomic::AtomicI32>,
    pub tray_active: Arc<AtomicBool>,
    pub dev_out: Arc<(Mutex<DevOutbox>, Condvar)>,
}

impl Ctx {
    pub fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
    pub fn dash(&self) -> MutexGuard<'_, Dash> {
        self.dash.lock().unwrap()
    }

    /// Queue the latest telemetry `$`-frame for the device writer (never blocks).
    pub fn send_telem(&self, line: &str) {
        let (m, cv) = &*self.dev_out;
        m.lock().unwrap().telem = Some(line.to_string());
        cv.notify_one();
    }

    /// Queue the latest `@REL` relatives line for the device writer (never blocks).
    pub fn send_relatives(&self, line: &str) {
        let (m, cv) = &*self.dev_out;
        m.lock().unwrap().rel = Some(line.to_string());
        cv.notify_one();
    }

    pub fn ui_run<F: FnOnce(AppWindow) + Send + 'static>(&self, f: F) {
        let w = self.ui.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(u) = w.upgrade() {
                f(u);
            }
        });
    }

    pub fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.rt.spawn(fut);
    }
}
