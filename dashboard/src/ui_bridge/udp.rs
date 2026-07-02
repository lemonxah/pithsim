//! Telemetry-UDP page bridge: push the configured port + supported-game list
//! into the `TelemetryUdp` global. Live status (listening / packets / source) is
//! pushed from the UDP listener loop itself.

use slint::ComponentHandle;

use super::{model, sstr};
use crate::state::State;
use crate::{AppWindow, TelemetryUdp};
use pith_sim::decoders::supported_games;

/// Push the persisted config (port) + the static supported-game list. Called
/// once at startup and after the port changes.
pub fn push_udp_cfg(ui: &AppWindow, s: &State) {
    let g = ui.global::<TelemetryUdp>();
    g.set_port(s.udp_port as i32);
    let games: Vec<slint::SharedString> = supported_games().iter().map(|n| sstr(n)).collect();
    g.set_supported(model(games));
    g.set_local_ip(sstr(&local_ip()));
    // Active-connector config.
    g.set_acc_on(s.acc_enabled);
    g.set_acc_host(sstr(&s.acc_host));
    g.set_acc_port(s.acc_port as i32);
    g.set_acc_password(sstr(&s.acc_password));
    g.set_ac_on(s.ac_enabled);
    g.set_ac_host(sstr(&s.ac_host));
    g.set_ac_port(s.ac_port as i32);
    g.set_gt7_on(s.gt7_enabled);
    g.set_gt7_host(sstr(&s.gt7_host));
    g.set_shm_on(s.shm_enabled);
}

/// Best-effort LAN IPv4 of this host, for the "point the game here" hint. Opens a
/// UDP socket toward a public address (no packet is sent) and reads back the
/// local address the OS would route from. Falls back to a placeholder.
fn local_ip() -> String {
    use std::net::UdpSocket;
    UdpSocket::bind(("0.0.0.0", 0))
        .and_then(|s| {
            s.connect(("8.8.8.8", 80))?;
            s.local_addr()
        })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "this PC's IP".to_string())
}
