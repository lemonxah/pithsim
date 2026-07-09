//! Dashboard WiFi/UDP device transport — the PC end of the wireless link
//! (`docs/pedals.md` §4, protocol in `pith_core::net`). It:
//!   - listens for device discovery beacons and subscribes to each device,
//!   - routes a wireless device's joystick axis into a software virtual
//!     joystick (`crate::vjoy`) so the game reads it with no USB cable,
//!   - forwards the live `$` telemetry frame to any wireless DDU.
//!
//! **The virtual joystick only exists while a device kind's WiFi input is ON**
//! (`State::wifi_hb_input` / `State::wifi_pedals_input`, set per hardware on
//! the Wireless screen). With them OFF (the default), devices are still
//! discovered — so you can provision/see them — but their axis is not routed
//! and no virtual joystick is created; the device's own USB HID axis is what
//! the game reads. This avoids double-reporting the axis (once over USB, once
//! via the virtual joystick). DDU telemetry forwarding has its own switch
//! (`State::wifi_ddu_enabled`).

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
    fw: String,
    addr: SocketAddr,
    last_seen: Instant,
}

/// Something to send to a wireless device, queued via [`Ctx::send_wifi`].
pub enum WifiOut {
    /// One protocol line (`@CFG{json}`, `@ACT{json}`, `?`, …) to the device
    /// with this serial. Fire-and-forget; the device's `RE` reply is surfaced
    /// via the Pedals config-status line.
    Line { serial: String, line: String },
    /// Request/reply: send `line`, then deliver the device's next `RE` packet
    /// text to `reply` instead of the status line. This is what lets a device
    /// page run its full USB command protocol over WiFi — see [`request`].
    Request {
        serial: String,
        line: String,
        reply: std::sync::mpsc::Sender<String>,
    },
    /// Push a firmware image over WiFi: the dashboard serves `image` on an
    /// ephemeral TCP port and tells the device (`@OTAWIFI <port> <size>`) to
    /// pull it (TCP for reliability; UDP only carries the trigger).
    Ota { serial: String, image: Vec<u8> },
}

/// Send `line` to wireless device `serial` and wait up to `timeout` for its
/// reply. The synchronous bridge device threads use to run their USB command
/// protocol over WiFi (one in-flight request per device; a newer request
/// supersedes a stale one).
pub fn request(ctx: &Arc<Ctx>, serial: &str, line: &str, timeout: Duration) -> Option<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    ctx.send_wifi(WifiOut::Request {
        serial: serial.to_string(),
        line: line.to_string(),
        reply: tx,
    });
    rx.recv_timeout(timeout).ok()
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
    // Last button mask applied per serial, so a BT packet only emits the
    // buttons that actually changed (and refresh packets emit nothing).
    let mut button_masks: HashMap<String, u32> = HashMap::new();
    // In-flight request/reply bridges by serial (see [`request`]).
    let mut pending: HashMap<String, std::sync::mpsc::Sender<String>> = HashMap::new();
    let mut buf = [0u8; 2048];
    let mut last_forward = Instant::now();

    while ctx.running.load(Ordering::SeqCst) {
        // Per-hardware enables (Wireless screen): axis/button routing is opted
        // into per device kind; DDU telemetry forwarding has its own switch.
        let (ddu_en, ddu_in, hb_en, ped_en) = {
            let s = ctx.lock();
            (
                s.wifi_ddu_enabled,
                s.wifi_ddu_input,
                s.wifi_hb_input,
                s.wifi_pedals_input,
            )
        };
        // Every input kind off (or just turned off) → tear the virtual joystick
        // down so the game falls back to the devices' own USB HID inputs.
        if !hb_en && !ped_en && !ddu_in && joystick.is_some() {
            joystick = None;
            axis_map.clear();
            button_masks.clear();
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
                            (hb_en, ped_en, ddu_in),
                            &mut joystick,
                            &mut devices,
                            &mut axis_map,
                            &mut button_masks,
                            &mut pending,
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
                WifiOut::Request {
                    serial,
                    line,
                    reply,
                } => {
                    if let Some(dev) = devices.get(&serial) {
                        let _ = sock.send_to(line.as_bytes(), dev.addr);
                        pending.insert(serial, reply);
                    }
                    // Unknown device: `reply` drops here, so the caller's
                    // recv_timeout fails immediately instead of waiting.
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
            expire_stale(
                &ctx,
                &mut devices,
                &mut axis_map,
                &mut button_masks,
                joystick.as_ref(),
            );
            if ddu_en {
                forward_telem_to_ddus(&ctx, &sock, &devices);
            }
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
    (hb_en, ped_en, ddu_in): (bool, bool, bool),
    joystick: &mut Option<VirtualJoystick>,
    devices: &mut HashMap<String, WirelessDevice>,
    axis_map: &mut HashMap<String, usize>,
    button_masks: &mut HashMap<String, u32>,
    pending: &mut HashMap<String, std::sync::mpsc::Sender<String>>,
) {
    let Some(pkt) = net::parse_device_packet(line) else {
        return;
    };
    match pkt {
        DevicePacket::Beacon { kind, serial, fw } => {
            let addr = SocketAddr::new(src.ip(), DEVICE_PORT);
            let is_new = !devices.contains_key(&serial);
            devices.insert(
                serial.clone(),
                WirelessDevice {
                    kind: kind.clone(),
                    fw,
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
            // Route only if this device kind's wireless input is enabled on
            // the Wireless screen (USB mode otherwise — don't double-report).
            let kind_enabled = match devices.get(&serial).map(|d| d.kind.as_str()) {
                Some("handbrake") => hb_en,
                Some("pedals") => ped_en,
                _ => false, // unknown / not beaconed yet / ddu (no axis)
            };
            if !kind_enabled {
                return;
            }
            if let Some(dev) = devices.get_mut(&serial) {
                dev.last_seen = Instant::now();
            }
            // Create the virtual joystick lazily on the first routed axis.
            if joystick.is_none() {
                match VirtualJoystick::new("Pith Wireless", MAX_AXES) {
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
        DevicePacket::Buttons { serial, mask } => {
            // The DDU's 32-button touch box over WiFi — routed into the same
            // virtual joystick, gated by the DDU's WiFi-input toggle.
            let is_ddu = devices.get(&serial).map(|d| d.kind.as_str()) == Some("ddu");
            if !ddu_in || !is_ddu {
                return;
            }
            if let Some(dev) = devices.get_mut(&serial) {
                dev.last_seen = Instant::now();
            }
            if joystick.is_none() {
                match VirtualJoystick::new("Pith Wireless", MAX_AXES) {
                    Ok(js) => {
                        eprintln!("wifi: virtual joystick created");
                        *joystick = Some(js);
                    }
                    Err(e) => {
                        eprintln!("wifi: virtual joystick create failed ({e}) — buttons not routed");
                        return;
                    }
                }
            }
            if let Some(js) = joystick.as_ref() {
                // Emit only the buttons that changed since the last packet.
                let prev = button_masks.insert(serial, mask).unwrap_or(0);
                let mut diff = prev ^ mask;
                while diff != 0 {
                    let i = diff.trailing_zeros() as usize;
                    diff &= diff - 1;
                    let _ = js.set_button(i, mask & (1 << i) != 0);
                }
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
            // A pending request gets the reply routed straight back to its
            // caller (the device thread running its protocol over WiFi)…
            if let Some(tx) = pending.remove(&serial) {
                if tx.send(text.clone()).is_ok() {
                    return;
                }
                // caller gave up (timeout) — fall through to the status line
            }
            // …otherwise surface it (config pushes, OTA progress) in the
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

/// Drop devices we haven't heard from recently. A vanished device's held
/// virtual-joystick buttons are released so nothing stays stuck pressed.
fn expire_stale(
    ctx: &Arc<Ctx>,
    devices: &mut HashMap<String, WirelessDevice>,
    axis_map: &mut HashMap<String, usize>,
    button_masks: &mut HashMap<String, u32>,
    joystick: Option<&VirtualJoystick>,
) {
    let before = devices.len();
    devices.retain(|serial, d| {
        let alive = d.last_seen.elapsed() < STALE_AFTER;
        if !alive {
            axis_map.remove(serial);
            if let Some(mut mask) = button_masks.remove(serial) {
                if let Some(js) = joystick {
                    while mask != 0 {
                        let i = mask.trailing_zeros() as usize;
                        mask &= mask - 1;
                        let _ = js.set_button(i, false);
                    }
                }
            }
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
    let mut list: Vec<(String, String, String, String)> = devices
        .iter()
        .map(|(serial, d)| {
            (
                d.kind.clone(),
                serial.clone(),
                d.addr.ip().to_string(),
                d.fw.clone(),
            )
        })
        .collect();
    list.sort();
    ctx.lock().wifi_devices = list.clone();

    // Mirror into the Slint model for the Wireless screen's device list.
    let rows: Vec<slint::SharedString> = list
        .into_iter()
        .map(|(kind, serial, ip, fw)| {
            if fw.is_empty() {
                format!("{kind}   {serial}   {ip}").into()
            } else {
                format!("{kind}   {serial}   {ip}   v{fw}").into()
            }
        })
        .collect();
    ctx.ui_run(move |u| {
        use slint::ComponentHandle;
        u.global::<crate::Wireless>()
            .set_devices(std::rc::Rc::new(slint::VecModel::from(rows)).into());
    });
}
