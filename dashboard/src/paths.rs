use std::path::PathBuf;

pub fn app_dir() -> PathBuf {
    let home = std::env::var_os("HOME");
    let base = match home {
        Some(h) if !h.is_empty() => PathBuf::from(h),
        _ => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    };
    let p = base.join(".pithddu");
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn cache_dir() -> PathBuf {
    app_dir()
}

pub fn db_dir() -> PathBuf {
    cache_dir().join("lovely-car-data")
}

pub fn data_root() -> PathBuf {
    let db = db_dir();
    if db.exists() {
        if let Ok(rd) = std::fs::read_dir(&db) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() && p.join("data").exists() {
                    return p.join("data");
                }
            }
        }
    }
    db.join("lovely-car-data-main").join("data")
}

pub fn presets_path() -> PathBuf {
    app_dir().join("presets.json")
}
pub fn race_layout_path() -> PathBuf {
    app_dir().join("racelayout.json")
}
pub fn buttons_path() -> PathBuf {
    app_dir().join("buttons.json")
}
pub fn active_car_path() -> PathBuf {
    app_dir().join("activecar.json")
}
pub fn shift_cfg_path() -> PathBuf {
    app_dir().join("shiftcfg.json")
}
pub fn udp_cfg_path() -> PathBuf {
    app_dir().join("udp.json")
}
pub fn board_path() -> PathBuf {
    app_dir().join("board.txt")
}
pub fn commit_path() -> PathBuf {
    cache_dir().join("commit.txt")
}
pub fn manifest_cache_path() -> PathBuf {
    cache_dir().join("manifest.json")
}

pub fn repo_root() -> PathBuf {
    app_dir().join("firmware")
}

pub fn default_firmware_path() -> Option<String> {
    // The DDU firmware lives in the monorepo's `firmware/ddu` subdir; `espflash
    // save-image` writes `pithddu-<board>.bin` there. Pick the newest one.
    let dir = repo_root().join("firmware").join("ddu");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.starts_with("pithddu-") && name.ends_with(".bin") {
                let mtime = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                if newest.as_ref().map(|(t, _)| mtime > *t).unwrap_or(true) {
                    newest = Some((mtime, p));
                }
            }
        }
    }
    newest.map(|(_, p)| p.to_string_lossy().to_string())
}

pub fn read_file(p: &std::path::Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

pub fn file_size_str(p: &std::path::Path) -> String {
    match std::fs::metadata(p) {
        Ok(m) => {
            let n = m.len() as f64;
            if n >= 1024.0 * 1024.0 {
                format!("{:.1} MB", n / (1024.0 * 1024.0))
            } else {
                format!("{:.0} KB", n / 1024.0)
            }
        }
        Err(_) => "—".to_string(),
    }
}
