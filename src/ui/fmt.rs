pub fn humanize(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn rate(bytes_per_sec: f64) -> String {
    let b = bytes_per_sec.max(0.0) as u64;
    if b < 1024 {
        format!("{b} B/s")
    } else {
        format!("{}/s", humanize(b))
    }
}

/// short uptime: `3d 4h`, `2h 5m`, `12m`, `45s`
pub fn duration_short(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, (secs / 3_600) % 24, (secs / 60) % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn humanize_bytes() {
        assert_eq!(humanize(512), "512 B");
        assert_eq!(humanize(2048), "2.0 KiB");
        assert_eq!(humanize(16 * 1024 * 1024 * 1024), "16.0 GiB");
    }

    #[test]
    fn rate_strings() {
        assert_eq!(rate(0.0), "0 B/s");
        assert_eq!(rate(1536.0), "1.5 KiB/s");
        assert_eq!(rate(2.5 * 1024.0 * 1024.0), "2.5 MiB/s");
    }

    #[test]
    fn duration_strings() {
        assert_eq!(duration_short(45), "45s");
        assert_eq!(duration_short(12 * 60), "12m");
        assert_eq!(duration_short(2 * 3600 + 5 * 60), "2h 5m");
        assert_eq!(duration_short(93_784), "1d 2h");
    }
}
