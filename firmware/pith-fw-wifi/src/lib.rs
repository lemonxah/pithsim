//! Shared firmware WiFi/UDP transport for every Pith device (DDU, handbrake,
//! pedals) — the wireless alternative to the USB command/axis channel (wire
//! protocol in `pith_core::net`, dashboard end in `dashboard/src/wifi.rs`).
//!
//! Runs on its own thread: connects as a STA with NVS-stored credentials,
//! broadcasts a discovery beacon, and once a dashboard subscribes streams the
//! device's joystick axis (if it has one) + status and relays the same
//! `@`-command protocol the USB channel speaks. Inbound non-`@SUB` lines
//! (commands for axis devices, or `$` telemetry frames for the DDU) land in
//! [`WifiShared::rx`] for the firmware's main loop to route; replies go back
//! via [`WifiShared::tx`].
//!
//! Device-specific behaviour is a small [`WifiOpts`] (the beacon `kind` and
//! whether to stream an axis) — everything else is identical across devices.
//!
//! Credentials are provisioned over USB (`@WIFI <ssid> <pass>`): the command
//! drops them into [`WifiShared::new_creds`] and this thread persists them to
//! NVS and (re)connects, no reboot.
//!
//! COMPILE-VERIFIED, bench-pending: needs a real device + network to validate.
//! The game axis over USB HID is unaffected and remains the default.

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
use esp_idf_svc::wifi::{BlockingWifi, ClientConfiguration, Configuration, EspWifi};

use pith_core::net;

const CREDS_NS: &str = "wifi";
const KEY_SSID: &str = "ssid";
const KEY_PASS: &str = "pass";
const BEACON_INTERVAL: Duration = Duration::from_secs(1);
const AXIS_INTERVAL: Duration = Duration::from_millis(5); // ~200 Hz axis stream
const STATE_INTERVAL: Duration = Duration::from_millis(250);

/// Hooks into the device's own OTA state machine, for firmware updates over
/// WiFi (`@OTAWIFI <tcp_port> <size>`): the device connects BACK to the
/// dashboard over TCP (reliable, unlike the UDP channel) and streams the
/// image into these. They map straight onto each firmware's `ota::begin` /
/// `ota::feed` (the DDU wraps its transport-tagged variants).
#[derive(Clone, Copy)]
pub struct OtaHooks {
    pub begin: fn(i32),
    pub feed: fn(&[u8]) -> bool,
    /// True while an OTA is in flight (the firmware's `ota::ACTIVE`).
    pub active: fn() -> bool,
}

/// Per-device transport options.
pub struct WifiOpts {
    /// Beacon device kind: `"ddu"`, `"handbrake"`, or `"pedals"`.
    pub kind: &'static str,
    /// Whether this device streams a joystick axis (pedals/handbrake) or not
    /// (the DDU, which receives telemetry instead).
    pub stream_axis: bool,
    /// OTA-over-WiFi hooks; `None` disables `@OTAWIFI` for this device.
    pub ota: Option<OtaHooks>,
}

/// Shared between the main loop and the WiFi thread.
pub struct WifiShared {
    /// Latest joystick axis (main writes each tick; streamed when
    /// `WifiOpts::stream_axis`).
    pub axis: AtomicU16,
    /// Whether the STA link + IP are up.
    pub connected: AtomicBool,
    /// A short status line the device streams as `ST` (main updates it).
    pub state_line: Mutex<String>,
    /// Lines received over WiFi for main to route: `@`-commands (dispatch) or
    /// `$` telemetry frames (DDU feeds its display).
    pub rx: Mutex<Vec<String>>,
    /// Reply lines from main to send back to the dashboard.
    pub tx: Mutex<Vec<String>>,
    /// New credentials from a USB `@WIFI` provisioning command.
    pub new_creds: Mutex<Option<(String, String)>>,
}

impl WifiShared {
    pub fn new() -> Self {
        WifiShared {
            axis: AtomicU16::new(0),
            connected: AtomicBool::new(false),
            state_line: Mutex::new(String::new()),
            rx: Mutex::new(Vec::new()),
            tx: Mutex::new(Vec::new()),
            new_creds: Mutex::new(None),
        }
    }
}

impl Default for WifiShared {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn the WiFi thread. Takes ownership of the radio modem + a clone of the
/// default NVS partition (shared with the WiFi driver's own storage and the
/// device's other NVS users).
pub fn spawn(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs_part: EspDefaultNvsPartition,
    shared: Arc<WifiShared>,
    serial: String,
    fw: String,
    opts: WifiOpts,
) {
    let _ = std::thread::Builder::new()
        .stack_size(8192)
        .name("wifi".into())
        .spawn(move || wifi_thread(modem, sysloop, nvs_part, shared, serial, fw, opts));
}

fn creds_nvs(part: &EspDefaultNvsPartition) -> Option<EspDefaultNvs> {
    EspDefaultNvs::new(part.clone(), CREDS_NS, true).ok()
}

fn load_creds(nvs: &EspDefaultNvs) -> Option<(String, String)> {
    let mut sbuf = [0u8; 64];
    let mut pbuf = [0u8; 96];
    let ssid = nvs.get_str(KEY_SSID, &mut sbuf).ok()??.to_string();
    let pass = nvs.get_str(KEY_PASS, &mut pbuf).ok()??.to_string();
    if ssid.is_empty() {
        None
    } else {
        Some((ssid, pass))
    }
}

fn save_creds(nvs: &mut EspDefaultNvs, ssid: &str, pass: &str) {
    let _ = nvs.set_str(KEY_SSID, ssid);
    let _ = nvs.set_str(KEY_PASS, pass);
}

#[allow(clippy::too_many_arguments)]
fn wifi_thread(
    modem: Modem,
    sysloop: EspSystemEventLoop,
    nvs_part: EspDefaultNvsPartition,
    shared: Arc<WifiShared>,
    serial: String,
    fw: String,
    opts: WifiOpts,
) {
    let mut creds_store = creds_nvs(&nvs_part);

    // Wait for credentials (from NVS, or provisioned over USB via @WIFI).
    let (mut ssid, mut pass) = loop {
        if let Some((s, p)) = shared.new_creds.lock().unwrap().take() {
            if let Some(nvs) = creds_store.as_mut() {
                save_creds(nvs, &s, &p);
            }
            break (s, p);
        }
        if let Some(c) = creds_store.as_ref().and_then(load_creds) {
            break c;
        }
        std::thread::sleep(Duration::from_secs(2));
    };

    // Build the driver ONCE (it owns the modem), then (re)configure/connect
    // in the loop below — surviving AP drops, wrong credentials, and live
    // re-provisioning, without a reboot. The thread never gives up: a pedal
    // that loses its AP keeps working over USB and rejoins when it can.
    let espwifi = match EspWifi::new(modem, sysloop.clone(), Some(nvs_part)) {
        Ok(w) => w,
        Err(e) => {
            log::warn!("wifi: driver init failed ({e:?}) — staying on USB");
            return;
        }
    };
    let mut wifi = match BlockingWifi::wrap(espwifi, sysloop) {
        Ok(w) => w,
        Err(e) => {
            log::warn!("wifi: wrap failed ({e:?}) — staying on USB");
            return;
        }
    };

    loop {
        // Pick up newer credentials if the user re-provisioned meanwhile.
        if let Some((s, p)) = shared.new_creds.lock().unwrap().take() {
            if let Some(nvs) = creds_store.as_mut() {
                save_creds(nvs, &s, &p);
            }
            ssid = s;
            pass = p;
        }

        log::info!("wifi: connecting to \"{ssid}\"");
        match connect(&mut wifi, &ssid, &pass) {
            Ok(()) => {
                shared.connected.store(true, Ordering::SeqCst);
                log::info!("wifi: up");
                // Runs until the link drops or new credentials arrive.
                run_udp(&wifi, &shared, &serial, &fw, &opts);
                shared.connected.store(false, Ordering::SeqCst);
                log::warn!("wifi: link lost or reconfiguring — reconnecting");
                let _ = wifi.disconnect();
            }
            Err(e) => {
                log::warn!(
                    "wifi: connect failed ({e:?}) — retrying in {}s",
                    RECONNECT_BACKOFF.as_secs()
                );
                std::thread::sleep(RECONNECT_BACKOFF);
            }
        }
    }
}

/// Retry backoff for failed connects — long enough not to hammer the radio /
/// AP, short enough that a router reboot self-heals quickly.
const RECONNECT_BACKOFF: Duration = Duration::from_secs(10);

/// (Re)configure + bring up the STA link, blocking until it has an IP.
fn connect(
    wifi: &mut BlockingWifi<EspWifi<'static>>,
    ssid: &str,
    pass: &str,
) -> Result<(), esp_idf_svc::sys::EspError> {
    let cfg = ClientConfiguration {
        ssid: ssid.try_into().unwrap_or_default(),
        password: pass.try_into().unwrap_or_default(),
        ..Default::default()
    };
    wifi.set_configuration(&Configuration::Client(cfg))?;
    if !wifi.is_started().unwrap_or(false) {
        wifi.start()?;
    }
    wifi.connect()?;
    wifi.wait_netif_up()?;
    Ok(())
}

/// The UDP loop: beacon + subscribe + optional axis + state stream + command
/// relay + `@OTAWIFI` handling. Returns when the STA link drops or new
/// credentials arrive, so the caller can reconnect.
fn run_udp(
    wifi: &BlockingWifi<EspWifi<'static>>,
    shared: &Arc<WifiShared>,
    serial: &str,
    fw: &str,
    opts: &WifiOpts,
) {
    let sock = match UdpSocket::bind(("0.0.0.0", net::DEVICE_PORT)) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("wifi: UDP bind failed ({e})");
            return;
        }
    };
    let _ = sock.set_read_timeout(Some(Duration::from_millis(5)));
    let _ = sock.set_broadcast(true);
    log::info!("wifi: UDP up on {} (kind={})", net::DEVICE_PORT, opts.kind);

    let mut dashboard: Option<std::net::SocketAddr> = None;
    let mut last_beacon = Instant::now() - BEACON_INTERVAL;
    let mut last_link_check = Instant::now();
    let mut last_axis = Instant::now();
    let mut last_state = Instant::now();
    let mut buf = [0u8; 2048];

    loop {
        // Exit conditions: link dropped (poll ~1 Hz) or re-provisioned.
        if last_link_check.elapsed() >= Duration::from_secs(1) {
            last_link_check = Instant::now();
            if !wifi.is_connected().unwrap_or(false) {
                return;
            }
            if shared.new_creds.lock().unwrap().is_some() {
                return;
            }
        }

        // Inbound: @SUB establishes the dashboard address; @OTAWIFI starts a
        // TCP firmware pull; everything else (commands, or `$` telemetry
        // frames for the DDU) goes to main via rx.
        if let Ok((n, src)) = sock.recv_from(&mut buf) {
            if let Ok(text) = std::str::from_utf8(&buf[..n]) {
                for line in text.lines() {
                    let line = line.trim();
                    if line == net::SUBSCRIBE_CMD {
                        dashboard =
                            Some(std::net::SocketAddr::new(src.ip(), net::DISCOVERY_PORT));
                    } else if let Some(rest) = line.strip_prefix("@OTAWIFI") {
                        handle_ota_wifi(opts, src.ip(), rest, &sock, dashboard, serial);
                    } else if !line.is_empty() {
                        shared.rx.lock().unwrap().push(line.to_string());
                    }
                }
            }
        }

        // Beacon (broadcast) so the dashboard can find us.
        if last_beacon.elapsed() >= BEACON_INTERVAL {
            last_beacon = Instant::now();
            let b = net::beacon(opts.kind, serial, fw);
            let _ = sock.send_to(b.as_bytes(), ("255.255.255.255", net::DISCOVERY_PORT));
        }

        if let Some(dash) = dashboard {
            if opts.stream_axis && last_axis.elapsed() >= AXIS_INTERVAL {
                last_axis = Instant::now();
                let ax = net::axis_packet(serial, shared.axis.load(Ordering::Relaxed));
                let _ = sock.send_to(ax.as_bytes(), dash);
            }
            if last_state.elapsed() >= STATE_INTERVAL {
                last_state = Instant::now();
                let body = shared.state_line.lock().unwrap().clone();
                if !body.is_empty() {
                    let st = format!("{}{serial} {body}", net::STATE_PREFIX);
                    let _ = sock.send_to(st.as_bytes(), dash);
                }
            }
            // Relay any replies main produced for wifi-received commands.
            let replies: Vec<String> = shared.tx.lock().unwrap().drain(..).collect();
            for r in replies {
                let line = format!("{}{serial} {}", net::REPLY_PREFIX, r.trim());
                let _ = sock.send_to(line.as_bytes(), dash);
            }
        }
    }
}

/// `@OTAWIFI <tcp_port> <size>`: pull a firmware image from the dashboard
/// over TCP (the sender of the UDP command) and stream it into the device's
/// OTA state machine. TCP gives the reliability a flash image needs — the
/// UDP command channel only carries the trigger. Blocks this thread for the
/// duration (beacons pause during the flash; the main loop keeps running and
/// reboots via its normal `ota::should_reboot` path when the image is in).
fn handle_ota_wifi(
    opts: &WifiOpts,
    dash_ip: std::net::IpAddr,
    args: &str,
    sock: &UdpSocket,
    dashboard: Option<std::net::SocketAddr>,
    serial: &str,
) {
    let reply = |msg: &str| {
        if let Some(dash) = dashboard {
            let line = format!("{}{serial} {msg}", net::REPLY_PREFIX);
            let _ = sock.send_to(line.as_bytes(), dash);
        }
    };
    let Some(hooks) = opts.ota else {
        reply("ERR ota unsupported");
        return;
    };
    let mut it = args.split_whitespace();
    let (Some(port), Some(size)) = (
        it.next().and_then(|t| t.parse::<u16>().ok()),
        it.next().and_then(|t| t.parse::<i32>().ok()),
    ) else {
        reply("ERR usage @OTAWIFI port size");
        return;
    };
    if size <= 0 {
        reply("ERR bad size");
        return;
    }

    log::info!("wifi: OTA pull from {dash_ip}:{port} ({size} bytes)");
    let stream = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::new(dash_ip, port),
        Duration::from_secs(5),
    );
    let mut stream = match stream {
        Ok(s) => s,
        Err(e) => {
            reply(&format!("ERR connect {e}"));
            return;
        }
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));

    (hooks.begin)(size);
    if !(hooks.active)() {
        reply("ERR ota begin failed");
        return;
    }
    reply("OK ota started");

    use std::io::Read;
    let mut chunk = [0u8; 4096];
    let mut received: i64 = 0;
    while received < size as i64 {
        match stream.read(&mut chunk) {
            Ok(0) => break, // EOF before full image — ota::check_timeout aborts
            Ok(n) => {
                received += n as i64;
                if !(hooks.feed)(&chunk[..n]) {
                    // The state machine stopped consuming (aborted/errored).
                    break;
                }
            }
            Err(e) => {
                log::warn!("wifi: OTA read error {e}");
                break;
            }
        }
    }
    log::info!("wifi: OTA transfer done ({received}/{size} bytes)");
}
