//! Dashboard WiFi/UDP device transport — the PC end of the wireless link
//! (`docs/pedals.md` §4, protocol in `pith_core::net`). It:
//!   - listens for device discovery beacons and subscribes to each device,
//!   - routes a wireless device's joystick axis into a software virtual
//!     joystick (`crate::vjoy`) so the game reads it with no USB cable,
//!   - forwards the live `$` telemetry frame to any wireless DDU.
//!
//! **The virtual joystick only exists while WiFi input mode is ON**
//! (`State::wifi_input_enabled`). With it OFF (the default), devices are
//! still discovered — so you can provision/see them — but their axis is not
//! routed and no virtual joystick is created; the device's own USB HID axis
//! is what the game reads. This avoids double-reporting the axis (once over
//! USB, once via the virtual joystick).

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pith_core::net::{self, DevicePacket, DEVICE_PORT, DISCOVERY_PORT, SUBSCRIBE_CMD};

use crate::ctx::Ctx;
use crate::vjoy::{VirtualJoystick, MAX_AXES};

const STALE_AFTER: Duration = Duration::from_secs(5);
const DDU_FORWARD_INTERVAL: Duration = Duration::from_millis(33); // ~30 Hz
const RECV_TIMEOUT: Duration = Duration::from_millis(200);

struct WirelessDevice {
    kind: String,
    addr: SocketAddr,
    last_seen: Instant,
}

/// Something to send to a wireless device, queued via [`Ctx::send_wifi`].
pub enum WifiOut {
    /// One protocol line (`@CFG{json}`, `@ACT{json}`, `?`, …) to the device
    /// with this serial. Fire-and-forget; the device's `RE` reply is surfaced
    /// via the Pedals config-status line.
    Line { serial: String, line: String },
    /// Push a firmware image over WiFi: the dashboard serves `image` on an
    /// ephemeral TCP port and tells the device (`@OTAWIFI <port> <size>`) to
    /// pull it (TCP for reliability; UDP only carries the trigger).
    Ota { serial: String, image: Vec<u8> },
}

/// Background loop: owns the UDP socket, the (lazily-created) virtual
/// joystick, and the discovered-device table.
pub fn wifi_loop(ctx: Arc<Ctx>) {
    let sock = match UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT)) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("wifi: cannot bind UDP {DISCOVERY_PORT} ({e}) — wireless transport off");
            return;
        }
    };
    let _ = sock.set_read_timeout(Some(RECV_TIMEOUT));
    let _ = sock.set_broadcast(true);
    eprintln!("wifi: listening for Pith devices on UDP {DISCOVERY_PORT}");

    let mut joystick: Option<VirtualJoystick> = None;
    let mut devices: HashMap<String, WirelessDevice> = HashMap::new();
    let mut axis_map: HashMap<String, usize> = HashMap::new();
    let mut buf = [0u8; 2048];
    let mut last_forward = Instant::now();

    while ctx.running.load(Ordering::SeqCst) {
        let enabled = ctx.lock().wifi_input_enabled;
        // WiFi mode off (or just turned off) → tear the virtual joystick down
        // so the game falls back to the device's own USB HID axis.
        if !enabled && joystick.is_some() {
            joystick = None;
            axis_map.clear();
            eprintln!("wifi: input mode off — virtual joystick removed");
        }

        match sock.recv_from(&mut buf) {
            Ok((n, src)) => {
                if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                    for line in text.lines() {
                        handle_packet(
                            &ctx,
                            &sock,
                            src,
                            line,
                            enabled,
                            &mut joystick,
                            &mut devices,
                            &mut axis_map,
                        );
                    }
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => eprintln!("wifi: recv error {e}"),
        }

        // Drain queued outbound commands / OTA pushes to wireless devices.
        let outbox: Vec<WifiOut> = std::mem::take(&mut *ctx.wifi_out.lock().unwrap());
        for out in outbox {
            match out {
                WifiOut::Line { serial, line } => {
                    if let Some(dev) = devices.get(&serial) {
                        let _ = sock.send_to(line.as_bytes(), dev.addr);
                    }
                }
                WifiOut::Ota { serial, image } => {
                    if let Some(dev) = devices.get(&serial) {
                        ota_push(&ctx, &sock, dev.addr, &serial, image);
                    } else {
                        eprintln!("wifi: OTA target {serial} not on the network");
                    }
                }
            }
        }

        if last_forward.elapsed() >= DDU_FORWARD_INTERVAL {
            last_forward = Instant::now();
            expire_stale(&ctx, &mut devices, &mut axis_map);
            forward_telem_to_ddus(&ctx, &sock, &devices);
        }
    }
}

/// Serve `image` on an ephemeral TCP port and trigger the device to pull it.
/// Runs on a short-lived thread so the UDP loop keeps servicing axis/beacon
/// traffic during the (multi-second) flash.
fn ota_push(ctx: &Arc<Ctx>, sock: &UdpSocket, dev_addr: SocketAddr, serial: &str, image: Vec<u8>) {
    let listener = match std::net::TcpListener::bind(("0.0.0.0", 0)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("wifi: OTA listener bind failed: {e}");
            return;
        }
    };
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            eprintln!("wifi: OTA listener addr: {e}");
            return;
        }
    };
    let cmd = format!("@OTAWIFI {port} {}", image.len());
    if sock.send_to(cmd.as_bytes(), dev_addr).is_err() {
        eprintln!("wifi: OTA trigger send failed");
        return;
    }
    let serial = serial.to_string();
    let c = ctx.clone();
    std::thread::spawn(move || {
        let _ = listener.set_nonblocking(false);
        // The device connects back within a few seconds or not at all.
        let deadline = Instant::now() + Duration::from_secs(10);
        listener.set_nonblocking(true).ok();
        let stream = loop {
            match listener.accept() {
                Ok((s, _)) => break Some(s),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() > deadline {
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(e) => {
                    eprintln!("wifi: OTA accept error: {e}");
                    break None;
                }
            }
        };
        let msg = match stream {
            Some(mut s) => {
                let _ = s.set_nonblocking(false);
                use std::io::Write;
                match s.write_all(&image).and_then(|_| s.flush()) {
                    Ok(()) => format!("WiFi OTA sent to {serial} — device will flash + reboot"),
                    Err(e) => format!("WiFi OTA transfer to {serial} failed: {e}"),
                }
            }
            None => format!("WiFi OTA: {serial} never connected back"),
        };
        eprintln!("wifi: {msg}");
        c.ui_run(move |u| {
            use slint::ComponentHandle;
            u.global::<crate::Pedals>()
                .set_config_status(crate::ui_bridge::sstr(&msg));
        });
    });
}

#[allow(clippy::too_many_arguments)]
fn handle_packet(
    ctx: &Arc<Ctx>,
    sock: &UdpSocket,
    src: SocketAddr,
    line: &str,
    enabled: bool,
    joystick: &mut Option<VirtualJoystick>,
    devices: &mut HashMap<String, WirelessDevice>,
    axis_map: &mut HashMap<String, usize>,
) {
    let Some(pkt) = net::parse_device_packet(line) else {
        return;
    };
    match pkt {
        DevicePacket::Beacon { kind, serial, .. } => {
            let addr = SocketAddr::new(src.ip(), DEVICE_PORT);
            let is_new = !devices.contains_key(&serial);
            devices.insert(
                serial.clone(),
                WirelessDevice {
                    kind: kind.clone(),
                    addr,
                    last_seen: Instant::now(),
                },
            );
            // Subscribe so the device starts streaming axis/state to us.
            let _ = sock.send_to(SUBSCRIBE_CMD.as_bytes(), addr);
            if is_new {
                eprintln!("wifi: discovered {kind} {serial} at {}", src.ip());
                publish_devices(ctx, devices);
            }
        }
        DevicePacket::Axis { serial, value } => {
            if !enabled {
                return; // USB mode: don't route the axis / make a joystick
            }
            if let Some(dev) = devices.get_mut(&serial) {
                dev.last_seen = Instant::now();
            }
            // Create the virtual joystick lazily on the first routed axis.
            if joystick.is_none() {
                match VirtualJoystick::new("Pith Wireless Pedals", MAX_AXES) {
                    Ok(js) => {
                        eprintln!("wifi: virtual joystick created");
                        *joystick = Some(js);
                    }
                    Err(e) => {
                        eprintln!("wifi: virtual joystick create failed ({e}) — axis not routed");
                        return;
                    }
                }
            }
            if let Some(js) = joystick.as_ref() {
                let next = axis_map.len().min(MAX_AXES - 1);
                let axis = *axis_map.entry(serial).or_insert(next);
                let _ = js.set_axis(axis, value);
            }
        }
        DevicePacket::State { serial, .. } => {
            if let Some(dev) = devices.get_mut(&serial) {
                dev.last_seen = Instant::now();
            }
        }
        DevicePacket::Reply { serial, text } => {
            if let Some(dev) = devices.get_mut(&serial) {
                dev.last_seen = Instant::now();
            }
            // Surface command replies (config pushes, OTA progress) in the
            // Pedals status line so wireless operations aren't silent.
            let msg = format!("{serial}: {text}");
            ctx.ui_run(move |u| {
                use slint::ComponentHandle;
                u.global::<crate::Pedals>()
                    .set_config_status(crate::ui_bridge::sstr(&msg));
            });
        }
    }
}

/// Drop devices we haven't heard from recently.
fn expire_stale(
    ctx: &Arc<Ctx>,
    devices: &mut HashMap<String, WirelessDevice>,
    axis_map: &mut HashMap<String, usize>,
) {
    let before = devices.len();
    devices.retain(|serial, d| {
        let alive = d.last_seen.elapsed() < STALE_AFTER;
        if !alive {
            axis_map.remove(serial);
        }
        alive
    });
    if devices.len() != before {
        publish_devices(ctx, devices);
    }
}

/// Send the latest merged `$` telemetry frame to every wireless DDU.
fn forward_telem_to_ddus(
    ctx: &Arc<Ctx>,
    sock: &UdpSocket,
    devices: &HashMap<String, WirelessDevice>,
) {
    let has_ddu = devices.values().any(|d| d.kind == "ddu");
    if !has_ddu {
        return;
    }
    let frame = {
        let (m, _) = &*ctx.dev_out;
        m.lock().unwrap().telem.clone()
    };
    let Some(frame) = frame else { return };
    for dev in devices.values().filter(|d| d.kind == "ddu") {
        let _ = sock.send_to(frame.as_bytes(), dev.addr);
    }
}

/// Mirror the discovered-device table into `State` for the UI.
fn publish_devices(ctx: &Arc<Ctx>, devices: &HashMap<String, WirelessDevice>) {
    let mut list: Vec<(String, String, String)> = devices
        .iter()
        .map(|(serial, d)| (d.kind.clone(), serial.clone(), d.addr.ip().to_string()))
        .collect();
    list.sort();
    ctx.lock().wifi_devices = list.clone();

    // Mirror into the Slint model for the Pedals page's wireless card.
    let rows: Vec<slint::SharedString> = list
        .into_iter()
        .map(|(kind, serial, ip)| format!("{kind}   {serial}   {ip}").into())
        .collect();
    ctx.ui_run(move |u| {
        use slint::ComponentHandle;
        u.global::<crate::Pedals>()
            .set_wifi_devices(std::rc::Rc::new(slint::VecModel::from(rows)).into());
    });
}
