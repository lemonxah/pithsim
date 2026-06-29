//! pith-shim — runs inside a sim's Proton/Wine prefix, reads the sim's shared
//! memory, and sends the Pith `$`-frame over UDP to the dashboard's Telemetry-UDP
//! server. The simplest way to get native ACC RPM / shift-lights with no SimHub
//! and no /dev/shm bridge.
//!
//! Usage (inside the prefix):  pith-shim.exe [host] [port]
//!   host  dashboard IP   (default 127.0.0.1)
//!   port  UDP port       (default 28909, match the Telemetry-UDP page)

#[cfg(windows)]
fn main() {
    use std::net::UdpSocket;
    use std::time::Duration;

    let args: Vec<String> = std::env::args().collect();
    let host = args.get(1).cloned().unwrap_or_else(|| "127.0.0.1".to_string());
    let port: u16 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(28909);

    let sock = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pith-shim: bind failed: {e}");
            return;
        }
    };
    println!("pith-shim → {host}:{port} (reading sim shared memory ~50 Hz)");

    let mut last_label = "";
    let mut ticks: u32 = 0;
    loop {
        if let Some(r) = pith_shm_bridge::read_frame() {
            // ~1 Hz: identify ourselves + (re)send car/track so the dashboard
            // matches the car library (LED profile) and resets the track map.
            if ticks % 50 == 0 || r.label != last_label {
                let _ = sock.send_to(format!("@SRC{} (shim)", r.label).as_bytes(), (host.as_str(), port));
                if let Some(car) = &r.car {
                    let _ = sock.send_to(format!("@CM{car}").as_bytes(), (host.as_str(), port));
                }
                if let Some(track) = &r.track {
                    let _ = sock.send_to(format!("@MAP{track}").as_bytes(), (host.as_str(), port));
                }
            }
            let _ = sock.send_to(r.frame.as_bytes(), (host.as_str(), port));
            // ~5 Hz: relatives/standings (positions + gaps change slowly vs the frame).
            if ticks % 10 == 0 {
                if let Some(rel) = &r.relatives {
                    let _ = sock.send_to(rel.as_bytes(), (host.as_str(), port));
                }
            }
            if r.label != last_label {
                last_label = r.label;
                println!("pith-shim: streaming {}", r.label);
            }
            // ~2 Hz debug readout so you can confirm what the source provides.
            if ticks % 100 == 0 {
                if let Some(t) = pith_core::simhub::parse_line(&r.frame) {
                    println!(
                        "pith-shim [{}] gear={} rpm={} kmh={} tc={} abs={} pit={} lights={} fuel={}",
                        r.label, t.gear as char, t.rpm, t.speed_kmh, t.tc, t.abs,
                        t.pit_limiter, t.headlights, t.fuel_dl,
                    );
                }
            }
        }
        ticks = ticks.wrapping_add(1);
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("pith-shim only runs on Windows / Proton-Wine (it reads the sim's shared memory).");
}
