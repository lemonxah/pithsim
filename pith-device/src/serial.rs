use std::io::{Read, Write};
use std::time::{Duration, Instant};

#[derive(Clone, Default, Debug)]
pub struct PortInfo {
    pub device: String,
    pub label: String,
    pub is_dash: bool,
    pub manufacturer: String,
    pub product: String,
    pub vid: String,
    pub pid: String,
    pub is_esp: bool,
}

#[derive(Default)]
pub struct Serial {
    port: Option<Box<dyn serialport::SerialPort>>,
    line_buf: Vec<u8>,
}

#[cfg(target_os = "linux")]
fn read_sysfs(path: &str) -> String {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| s.lines().next().map(|l| l.to_string()))
        .unwrap_or_default()
}

impl Serial {
    #[cfg(target_os = "linux")]
    pub fn list() -> Vec<PortInfo> {
        let mut out = Vec::new();
        let dir = match std::fs::read_dir("/dev") {
            Ok(d) => d,
            Err(_) => return out,
        };
        for e in dir.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if !n.starts_with("ttyACM") && !n.starts_with("ttyUSB") {
                continue;
            }
            let dev = format!("/dev/{n}");
            let mut pi = PortInfo {
                device: dev.clone(),
                label: dev.clone(),
                ..Default::default()
            };
            let base = format!("/sys/class/tty/{n}/device/../");
            let vid = read_sysfs(&format!("{base}idVendor"));
            let pid = read_sysfs(&format!("{base}idProduct"));
            let prod = read_sysfs(&format!("{base}product"));
            let manu = read_sysfs(&format!("{base}manufacturer"));
            pi.vid = vid.clone();
            pi.pid = pid.clone();
            pi.product = prod.clone();
            pi.manufacturer = manu;
            let iface = read_sysfs(&format!("/sys/class/tty/{n}/device/interface"));
            if !prod.is_empty() {
                pi.label = format!("{dev} ({prod})");
            }
            if vid == "303a" && pid == "4002" {
                let is_data = iface.contains("Data");
                let is_cmd = iface.contains("Command");
                pi.is_dash = is_cmd || !is_data;
                let role = if is_cmd {
                    "Command"
                } else if is_data {
                    "Data"
                } else {
                    "Dashboard"
                };
                pi.label = format!("{dev} — Pith {role}");
            }
            pi.is_esp =
                vid == "303a" || (vid == "10c4" && pid == "ea60") || vid == "1a86" || vid == "0403";
            out.push(pi);
        }
        out
    }

    #[cfg(not(target_os = "linux"))]
    pub fn list() -> Vec<PortInfo> {
        let mut out = Vec::new();
        let ports = match serialport::available_ports() {
            Ok(p) => p,
            Err(_) => return out,
        };
        for p in ports {
            let mut pi = PortInfo {
                device: p.port_name.clone(),
                label: p.port_name.clone(),
                ..Default::default()
            };
            if let serialport::SerialPortType::UsbPort(u) = p.port_type {
                pi.vid = format!("{:04x}", u.vid);
                pi.pid = format!("{:04x}", u.pid);
                pi.manufacturer = u.manufacturer.unwrap_or_default();
                pi.product = u.product.unwrap_or_default();
                pi.is_dash = u.vid == 0x303a && u.pid == 0x4002;
                pi.is_esp = u.vid == 0x303a
                    || (u.vid == 0x10c4 && u.pid == 0xea60)
                    || u.vid == 0x1a86
                    || u.vid == 0x0403;
            }
            out.push(pi);
        }
        out
    }

    #[allow(dead_code)]
    pub fn open(&mut self, dev: &str) -> bool {
        self.close();
        match serialport::new(dev, 115200)
            .timeout(Duration::from_millis(50))
            .open()
        {
            Ok(p) => {
                self.port = Some(p);
                true
            }
            Err(_) => false,
        }
    }

    pub fn close(&mut self) {
        self.port = None;
        self.line_buf.clear();
    }

    pub fn is_open(&self) -> bool {
        self.port.is_some()
    }

    pub fn write(&mut self, data: &[u8]) -> bool {
        match self.port.as_mut() {
            Some(p) => p.write_all(data).is_ok(),
            None => false,
        }
    }

    pub fn write_str(&mut self, s: &str) -> bool {
        self.write(s.as_bytes())
    }

    fn read_some(&mut self, buf: &mut [u8]) -> usize {
        match self.port.as_mut() {
            Some(p) => p.read(buf).unwrap_or(0),
            None => 0,
        }
    }

    pub fn read_line(&mut self, timeout_ms: u64) -> String {
        let t0 = Instant::now();
        loop {
            if let Some(nl) = self.line_buf.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.line_buf.drain(..=nl).collect();
                let mut l = String::from_utf8_lossy(&line[..line.len() - 1]).to_string();
                while l.ends_with('\r') || l.ends_with(' ') {
                    l.pop();
                }
                if !l.is_empty() {
                    return l;
                }
                continue;
            }
            let mut b = [0u8; 256];
            let r = self.read_some(&mut b);
            if r > 0 {
                self.line_buf.extend_from_slice(&b[..r]);
            }
            if t0.elapsed().as_millis() as u64 > timeout_ms {
                return String::new();
            }
        }
    }

    pub fn drain(&mut self) {
        let mut b = [0u8; 512];
        for _ in 0..8 {
            if self.read_some(&mut b) == 0 {
                break;
            }
        }
        self.line_buf.clear();
    }
}
