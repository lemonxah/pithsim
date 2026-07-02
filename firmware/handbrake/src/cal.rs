//! Calibration persistence: one fixed-size raw blob in NVS. Small enough that
//! per-key storage (like a bigger device's pin/layout config) would just be
//! more ceremony for no benefit.

use esp_idf_svc::nvs::{EspDefaultNvs, EspDefaultNvsPartition, EspNvsPartition};
use pith_hb_core::Calibration;

const NAMESPACE: &str = "hb";
const KEY: &str = "cal";

pub struct CalStore {
    nvs: Option<EspDefaultNvs>,
}

pub fn init() -> CalStore {
    let nvs = EspNvsPartition::<esp_idf_svc::nvs::NvsDefault>::take()
        .and_then(|p: EspDefaultNvsPartition| EspDefaultNvs::new(p, NAMESPACE, true))
        .map_err(|e| log::warn!("NVS unavailable, calibration won't persist: {e}"))
        .ok();
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
