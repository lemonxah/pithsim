//! Device identity (and, later, the runtime pin map). The serial is derived from
//! the factory eFuse MAC — globally unique per chip — so it's deterministic every
//! boot without needing NVS persistence (matches the legacy "PITH-XXXXXXXXXXXX").

use std::sync::OnceLock;

use esp_idf_svc::sys;

static SERIAL: OnceLock<String> = OnceLock::new();

/// Stable device serial, e.g. "PITH-84F703A1B2C3".
pub fn serial() -> &'static str {
    SERIAL
        .get_or_init(|| {
            let mut mac = [0u8; 6];
            unsafe { sys::esp_efuse_mac_get_default(mac.as_mut_ptr()) };
            format!(
                "PITH-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
            )
        })
        .as_str()
}
