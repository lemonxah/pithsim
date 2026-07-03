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
    /// Latest-wins outbox for the handbrake device thread (see
    /// [`crate::hb::HbOutbound`] for why a single slot, not a queue).
    pub hb_out: Arc<(Mutex<Option<crate::hb::HbOutbound>>, Condvar)>,
    /// Latest-wins outbox for the pedal device thread (config pushes are
    /// rare UI actions, not a high-rate stream, so a plain mutex — no
    /// condvar wakeup needed since the loop already polls at ~50 Hz for
    /// the effects engine).
    pub pedals_out: Arc<Mutex<Option<crate::pedals::PedalsOutbound>>>,
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

    /// Queue a command for the handbrake device thread (latest wins).
    pub fn send_hb(&self, cmd: crate::hb::HbOutbound) {
        let (m, cv) = &*self.hb_out;
        *m.lock().unwrap() = Some(cmd);
        cv.notify_one();
    }

    /// Queue a command for the pedal device thread (latest wins).
    pub fn send_pedals(&self, cmd: crate::pedals::PedalsOutbound) {
        *self.pedals_out.lock().unwrap() = Some(cmd);
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
