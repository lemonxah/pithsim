pub mod build;
pub mod ota;

pub const APP_FW_VERSION: &str = "0.9.9";
// Monorepo: the firmware source lives in the `firmware/` subdir; releases are
// tagged `firmware-v*`; one release carries every device's image, told apart
// by asset name (`pithddu-<board>.bin` DDU, `pith-hb-<board>.bin` handbrake).
pub const FIRMWARE_GIT_URL: &str = "https://github.com/lemonxah/pithsim.git";
pub const FIRMWARE_RELEASES_URL: &str = "https://api.github.com/repos/lemonxah/pithsim/releases";

pub fn semver_cmp(a: &str, b: &str) -> i32 {
    fn parse(s: &str) -> [i32; 4] {
        let s = s
            .strip_prefix('v')
            .or_else(|| s.strip_prefix('V'))
            .unwrap_or(s);
        let mut p = [0i32; 4];
        for (i, part) in s.split('.').take(4).enumerate() {
            p[i] = crate::util::atoi(part);
        }
        p
    }
    let (a, b) = (parse(a), parse(b));
    for i in 0..4 {
        if a[i] != b[i] {
            return if a[i] < b[i] { -1 } else { 1 };
        }
    }
    0
}

#[cfg(target_os = "linux")]
pub fn can_build_firmware() -> bool {
    if std::env::var("IDF_PATH")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
    {
        return true;
    }
    if let Some(home) = std::env::var_os("HOME") {
        if std::path::Path::new(&home).join(".espressif").exists() {
            return true;
        }
    }
    crate::paths::repo_root().join("idf-env.sh").exists()
}

#[cfg(not(target_os = "linux"))]
pub fn can_build_firmware() -> bool {
    false
}
