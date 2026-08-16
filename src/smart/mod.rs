//! SMART health data via `smartctl -j`.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SmartInfo {
    pub device: String, // short display name, e.g. "disk0" or "sda"
    pub model: Option<String>,
    pub healthy: Option<bool>, // smart_status.passed
    pub temp_c: Option<u64>,   // temperature.current
    pub wear_pct: Option<u64>, // nvme percentage_used
    pub power_on_hours: Option<u64>,
}

/// Locate `smartctl`: `PATH` first, then well-known brew/system dirs.
pub fn find_smartctl() -> Option<PathBuf> {
    let fallbacks = [
        "/opt/homebrew/bin",
        "/opt/homebrew/sbin",
        "/usr/local/sbin",
        "/usr/sbin",
    ];
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&path_var)
        .chain(fallbacks.iter().map(PathBuf::from))
        .map(|dir| dir.join("smartctl"))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Parse `smartctl -j -a` output. None if JSON invalid or all data fields absent.
pub fn parse_report(json: &str, device: &str) -> Option<SmartInfo> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let info = SmartInfo {
        device: device.to_string(),
        model: v
            .get("model_name")
            .and_then(|m| m.as_str())
            .map(String::from),
        healthy: v
            .pointer("/smart_status/passed")
            .and_then(serde_json::Value::as_bool),
        temp_c: v
            .pointer("/temperature/current")
            .and_then(serde_json::Value::as_u64),
        wear_pct: v
            .pointer("/nvme_smart_health_information_log/percentage_used")
            .and_then(serde_json::Value::as_u64),
        power_on_hours: v
            .pointer("/power_on_time/hours")
            .and_then(serde_json::Value::as_u64),
    };
    let useless = info.model.is_none()
        && info.healthy.is_none()
        && info.temp_c.is_none()
        && info.wear_pct.is_none()
        && info.power_on_hours.is_none();
    if useless { None } else { Some(info) }
}

/// Parse `smartctl --scan -j` output into `(name, type)` pairs.
pub fn parse_scan(json: &str) -> Vec<(String, String)> {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let Some(devices) = v.get("devices").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    devices
        .iter()
        .filter_map(|d| {
            let name = d.get("name")?.as_str()?;
            let dev_type = d.get("type")?.as_str()?;
            Some((name.to_string(), dev_type.to_string()))
        })
        .collect()
}

/// Scan for devices, then collect a SMART report for each. Shells out.
/// smartctl's exit code is a bitmask, so it is ignored: stdout that parses
/// as JSON with usable fields wins; anything else is skipped.
pub fn collect(smartctl: &Path) -> Vec<SmartInfo> {
    let scan = match std::process::Command::new(smartctl)
        .args(["--scan", "-j"])
        .output()
    {
        Ok(out) => String::from_utf8_lossy(&out.stdout).into_owned(),
        Err(_) => return Vec::new(),
    };
    parse_scan(&scan)
        .into_iter()
        .filter_map(|(name, dev_type)| {
            let out = std::process::Command::new(smartctl)
                .args(["-j", "-a", &name, "-d", &dev_type])
                .output()
                .ok()?;
            let json = String::from_utf8_lossy(&out.stdout);
            // short display name: trailing path component ("/dev/disk0" -> "disk0",
            // IOService ".../NS_01@1" -> "NS_01@1")
            let short = name.rsplit('/').next().unwrap_or(&name);
            parse_report(&json, short)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_real_disk0_fixture() {
        let info = parse_report(include_str!("testdata_disk0.json"), "disk0").unwrap();
        assert_eq!(info.device, "disk0");
        assert_eq!(info.model.as_deref(), Some("APPLE SSD AP1024R"));
        assert_eq!(info.healthy, Some(true));
        assert_eq!(info.temp_c, Some(30));
        assert_eq!(info.wear_pct, Some(4));
        assert_eq!(info.power_on_hours, Some(2145));
    }

    #[test]
    fn garbage_json_is_none() {
        assert_eq!(parse_report("not json", "d"), None);
    }

    #[test]
    fn empty_object_is_none() {
        assert_eq!(parse_report("{}", "d"), None);
    }

    #[test]
    fn partial_fields_survive() {
        let info = parse_report(r#"{"temperature":{"current":42}}"#, "d").unwrap();
        assert_eq!(info.device, "d");
        assert_eq!(info.temp_c, Some(42));
        assert_eq!(info.model, None);
        assert_eq!(info.healthy, None);
        assert_eq!(info.wear_pct, None);
        assert_eq!(info.power_on_hours, None);
    }

    #[test]
    fn scan_parse() {
        assert_eq!(
            parse_scan(r#"{"devices":[{"name":"/dev/sda","type":"sat"}]}"#),
            vec![("/dev/sda".to_string(), "sat".to_string())]
        );
        assert_eq!(parse_scan("{}"), Vec::new());
    }
}
