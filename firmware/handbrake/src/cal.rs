//! Calibration persistence: one fixed-size raw blob in NVS. Small enough that
//! per-key storage (like a bigger device's pin/layout config) would just be
//! more ceremony for no benefit.

use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition};
use pith_hb_core::Calibration;

const NAMESPACE: &str = "hb";
const KEY: &str = "cal";

pub struct CalStore {
    nvs: Option<EspDefaultNvs>,
}

/// Open the calibration store on a clone of the shared default NVS partition.
/// The partition is taken once in `main` (the WiFi driver needs it too) — the
/// singleton `EspDefaultNvsPartition::take()` can only succeed once.
pub fn init(part: Option<EspDefaultNvsPartition>) -> CalStore {
    let nvs = part
        .and_then(|p| {
            EspDefaultNvs::new(p, NAMESPACE, true)
                .map_err(|e| log::warn!("NVS unavailable, calibration won't persist: {e}"))
                .ok()
        });
    CalStore { nvs }
}

impl CalStore {
    pub fn load(&self) -> Calibration {
        let Some(nvs) = self.nvs.as_ref() else {
            return Calibration::default();
        };
        let mut buf = [0u8; pith_hb_core::calibration::BLOB_LEN];
        match nvs.get_raw(KEY, &mut buf) {
            Ok(Some(bytes)) => Calibration::from_bytes(bytes).unwrap_or_default(),
            _ => Calibration::default(),
        }
    }

    pub fn save(&mut self, cal: &Calibration) -> bool {
        match self.nvs.as_mut() {
            Some(nvs) => nvs.set_raw(KEY, &cal.to_bytes()).unwrap_or(false),
            None => false,
        }
    }

    pub fn reset(&mut self) -> bool {
        self.save(&Calibration::default())
    }
}
