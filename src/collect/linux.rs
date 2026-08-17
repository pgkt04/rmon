use super::{
    CollectError, CpuSnapshot, CpuTimes, DiskStats, MemSnapshot, NetSnapshot, ProcessInfo,
    ThreadInfo,
};
#[cfg(target_os = "linux")]
use super::{Collector, MountInfo, Snapshot};

/// /proc/stat "cpu" lines: user nice system idle iowait irq softirq steal ...
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_proc_stat(s: &str) -> Result<CpuSnapshot, CollectError> {
    let mut total = None;
    let mut per_core = Vec::new();
    for line in s.lines() {
        if !line.starts_with("cpu") {
            continue;
        }
        let mut it = line.split_ascii_whitespace();
        let name = it.next().unwrap_or("");
        let v: Vec<u64> = it.map(|f| f.parse().unwrap_or(0)).collect();
        if v.len() < 8 {
            return Err(CollectError::Parse(format!("short cpu line: {line}")));
        }
        let t = CpuTimes {
            busy: v[0] + v[1] + v[2] + v[5] + v[6] + v[7],
            idle: v[3] + v[4],
        };
        if name == "cpu" {
            total = Some(t);
        } else {
            per_core.push(t);
        }
    }
    let Some(total) = total else {
        return Err(CollectError::Parse("no cpu line in /proc/stat".into()));
    };
    Ok(CpuSnapshot { total, per_core })
}

// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_meminfo(s: &str) -> Result<MemSnapshot, CollectError> {
    let mut total = 0u64;
    let mut available = 0u64;
    let mut swap_total = 0u64;
    let mut swap_free = 0u64;
    for line in s.lines() {
        let Some((key, rest)) = line.split_once(':') else {
            continue;
        };
        let kb: u64 = rest
            .trim()
            .trim_end_matches("kB")
            .trim()
            .parse()
            .unwrap_or(0);
        match key {
            "MemTotal" => total = kb * 1024,
            "MemAvailable" => available = kb * 1024,
            "SwapTotal" => swap_total = kb * 1024,
            "SwapFree" => swap_free = kb * 1024,
            _ => {}
        }
    }
    if total == 0 {
        return Err(CollectError::Parse("no MemTotal in /proc/meminfo".into()));
    }
    Ok(MemSnapshot {
        total,
        used: total.saturating_sub(available),
        available,
        swap_total,
        swap_used: swap_total.saturating_sub(swap_free),
    })
}

/// /proc/net/dev: two header lines, then `iface: <8 rx fields> <8 tx fields>`
/// rx bytes is field 1, tx bytes is field 9; loopback is excluded
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_net_dev(s: &str) -> NetSnapshot {
    let mut net = NetSnapshot::default();
    let mut best = 0u64;
    for line in s.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let f: Vec<u64> = rest
            .split_ascii_whitespace()
            .map(|x| x.parse().unwrap_or(0))
            .collect();
        if f.len() >= 9 {
            net.rx_bytes += f[0];
            net.tx_bytes += f[8];
            if f[0] + f[8] > best {
                best = f[0] + f[8];
                net.iface = Some(name.trim().to_owned());
            }
        }
    }
    net
}

/// one /proc/[pid]/stat line; comm sits in parens and may contain anything,
/// so split at the LAST ')'
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_pid_stat(pid: i32, s: &str, clk_tck: u64, page_size: u64) -> Option<ProcessInfo> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let name = s.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = s.get(close + 1..)?.split_ascii_whitespace().collect();
    // fields after comm, 0-indexed: state=0, utime=11, stime=12, rss pages=21
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    let rss_pages: u64 = rest.get(21)?.parse().ok()?;
    Some(ProcessInfo {
        pid,
        name,
        cpu_ns: (utime + stime).saturating_mul(1_000_000_000 / clk_tck.max(1)),
        rss: rss_pages * page_size,
        disk_read: None,
        disk_written: None,
        threads: Vec::new(),
    })
}

/// one /proc/[pid]/task/[tid]/stat line; same layout as the pid stat, so the
/// comm splits at the LAST ')' too
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_tid_stat(tid: u64, s: &str, clk_tck: u64) -> Option<ThreadInfo> {
    let open = s.find('(')?;
    let close = s.rfind(')')?;
    let name = s.get(open + 1..close)?.to_string();
    let rest: Vec<&str> = s.get(close + 1..)?.split_ascii_whitespace().collect();
    // fields after comm, 0-indexed: utime=11, stime=12
    let utime: u64 = rest.get(11)?.parse().ok()?;
    let stime: u64 = rest.get(12)?.parse().ok()?;
    Some(ThreadInfo {
        tid,
        name,
        cpu_ns: (utime + stime).saturating_mul(1_000_000_000 / clk_tck.max(1)),
    })
}

/// every tid under /proc/[pid]/task, main thread included; threads exit
/// mid-scan constantly, so anything unreadable is skipped, never an error
#[cfg(target_os = "linux")]
fn read_task_threads(task_dir: &std::path::Path, clk_tck: u64) -> Vec<ThreadInfo> {
    let Ok(entries) = std::fs::read_dir(task_dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else { continue };
        let Some(tid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u64>().ok())
        else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        if let Some(t) = parse_tid_stat(tid, &stat, clk_tck) {
            out.push(t);
        }
    }
    out
}

/// /proc/[pid]/io: block-layer read_bytes/write_bytes lines
// Only used by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_pid_io(s: &str) -> Option<(u64, u64)> {
    let mut read = None;
    let mut write = None;
    for line in s.lines() {
        let (k, v) = match line.split_once(':') {
            Some(kv) => kv,
            None => continue,
        };
        match k {
            "read_bytes" => read = v.trim().parse().ok(),
            "write_bytes" => write = v.trim().parse().ok(),
            _ => {}
        }
    }
    Some((read?, write?))
}

/// /proc/diskstats: major minor name then at least 11 counter fields
/// sectors are 512 bytes regardless of device sector size
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_diskstats(s: &str) -> Vec<DiskStats> {
    const MS: u64 = 1_000_000;
    let mut out = Vec::new();
    for line in s.lines() {
        let f: Vec<&str> = line.split_ascii_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        out.push(DiskStats {
            name: f[2].to_string(),
            read_ops: n(3),
            read_bytes: n(5) * 512,
            written_bytes: n(9) * 512,
            write_ops: n(7),
            io_time_ns: Some((n(6) + n(10)) * MS),
            busy_time_ns: Some(n(12) * MS),
            weighted_ns: Some(n(13) * MS),
        });
    }
    out
}

/// real filesystems worth showing; pseudo/overlay mounts are noise
// Only used by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const REAL_FS: [&str; 9] = [
    "ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "f2fs", "vfat", "ntfs",
];

/// /proc/self/mounts octal-escapes special chars in paths (\040 = space);
/// undo that so statvfs sees the real path
// Only used by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            let v = u32::from(b[i + 1] - b'0') << 6
                | u32::from(b[i + 2] - b'0') << 3
                | u32::from(b[i + 3] - b'0');
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_mounts(s: &str) -> Vec<(String, String, String)> {
    s.lines()
        .filter_map(|line| {
            let mut it = line.split_ascii_whitespace();
            let dev = it.next()?;
            let point = it.next()?;
            let fs = it.next()?;
            REAL_FS
                .contains(&fs)
                .then(|| (dev.to_string(), unescape_mount(point), fs.to_string()))
        })
        .collect()
}

/// /proc/cpuinfo `model name` value; arm SBCs often omit it -> None
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_cpuinfo_model(s: &str) -> Option<String> {
    s.lines().find_map(|line| {
        let (key, val) = line.split_once(':')?;
        (key.trim() == "model name").then(|| val.trim().to_string())
    })
}

/// hwmon chips known to report the cpu die temperature
// Only used by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const CPU_HWMON: [&str; 4] = ["coretemp", "k10temp", "zenpower", "cpu_thermal"];

/// scan `base` (/sys/class/hwmon) for the first cpu chip; temp1_input is m°C
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_hwmon_cpu_temp(base: &std::path::Path) -> Option<(f64, Vec<f64>)> {
    let mut dirs: Vec<_> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    // read_dir order is arbitrary; sort by index so hwmon2 beats hwmon10
    dirs.sort_by_key(|p| hwmon_index(p));
    for dir in dirs {
        let Ok(name) = std::fs::read_to_string(dir.join("name")) else {
            continue;
        };
        if !CPU_HWMON.contains(&name.trim()) {
            continue;
        }
        // an unreadable/insane reading should not hide a later valid chip
        let Some(t) = read_milli_temp(&dir.join("temp1_input")) else {
            continue;
        };
        return Some((t, read_core_temps(&dir)));
    }
    None
}

fn read_milli_temp(path: &std::path::Path) -> Option<f64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .map(|milli| milli / 1000.0)
        .filter(|t| (0.0..150.0).contains(t))
}

/// coretemp labels its inputs `Core 0`, `Core 1`, ...; chips without such
/// labels (k10temp Tctl only) yield an empty vec
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn read_core_temps(dir: &std::path::Path) -> Vec<f64> {
    let mut cores: Vec<(u32, f64)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    for e in entries.flatten() {
        let fname = e.file_name();
        let Some(fname) = fname.to_str() else {
            continue;
        };
        let Some(n) = fname
            .strip_prefix("temp")
            .and_then(|r| r.strip_suffix("_label"))
        else {
            continue;
        };
        let Ok(label) = std::fs::read_to_string(e.path()) else {
            continue;
        };
        let Some(core_ix) = label.trim().strip_prefix("Core ") else {
            continue;
        };
        let Ok(core_ix) = core_ix.parse::<u32>() else {
            continue;
        };
        if let Some(t) = read_milli_temp(&dir.join(format!("temp{n}_input"))) {
            cores.push((core_ix, t));
        }
    }
    cores.sort_by_key(|(ix, _)| *ix);
    cores.into_iter().map(|(_, t)| t).collect()
}

/// numeric suffix of e.g. hwmon12; unknown names sort last
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn hwmon_index(p: &std::path::Path) -> u64 {
    p.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| {
            n.trim_start_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()
        })
        .unwrap_or(u64::MAX)
}

/// scan `base` (/sys/class/drm) card dirs for gpu_busy_percent (amdgpu only;
/// nvidia does not expose it -> None); name comes from the uevent DRIVER= line
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn read_drm_gpu(base: &std::path::Path) -> Option<(String, f64)> {
    let mut cards: Vec<_> = std::fs::read_dir(base)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            // card0, card1, ... but not connector entries like card0-eDP-1
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix("card"))
                .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
        })
        .collect();
    cards.sort();
    for card in cards {
        let dev = card.join("device");
        let Ok(busy) = std::fs::read_to_string(dev.join("gpu_busy_percent")) else {
            continue;
        };
        let Ok(util) = busy.trim().parse::<f64>() else {
            continue;
        };
        let name = std::fs::read_to_string(dev.join("uevent"))
            .ok()
            .and_then(|u| {
                u.lines()
                    .find_map(|l| l.strip_prefix("DRIVER=").map(|d| d.trim().to_string()))
            })
            .unwrap_or_else(|| "gpu".to_string());
        return Some((name, util));
    }
    None
}

#[cfg(target_os = "linux")]
pub struct LinuxCollector;

#[cfg(target_os = "linux")]
impl Collector for LinuxCollector {
    fn collect(&mut self, threads: bool) -> Result<Snapshot, CollectError> {
        let cpu = parse_proc_stat(&std::fs::read_to_string("/proc/stat")?)?;
        let mem = parse_meminfo(&std::fs::read_to_string("/proc/meminfo")?)?;
        let net = parse_net_dev(&std::fs::read_to_string("/proc/net/dev")?);

        // SAFETY: sysconf with a valid constant is always safe to call
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
        // SAFETY: same
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
        let mut procs = Vec::new();
        for entry in std::fs::read_dir("/proc")? {
            let Ok(entry) = entry else { continue };
            let name = entry.file_name();
            let Some(pid) = name.to_str().and_then(|s| s.parse::<i32>().ok()) else {
                continue;
            };
            // processes vanish mid-scan; skip errors instead of failing the tick
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            if let Some(mut p) = parse_pid_stat(pid, &stat, clk_tck, page) {
                // /proc/[pid]/io needs ptrace-level access; None when denied
                if let Ok(io) = std::fs::read_to_string(entry.path().join("io"))
                    && let Some((r, w)) = parse_pid_io(&io)
                {
                    p.disk_read = Some(r);
                    p.disk_written = Some(w);
                }
                // opt-in: this is one dir listing + a file per thread, per tick
                if threads {
                    p.threads = read_task_threads(&entry.path().join("task"), clk_tck);
                }
                procs.push(p);
            }
        }
        // keep whole devices only; partitions have no /sys/block entry
        let disks: Vec<_> = parse_diskstats(&std::fs::read_to_string("/proc/diskstats")?)
            .into_iter()
            .filter(|d| std::path::Path::new("/sys/block").join(&d.name).exists())
            .collect();

        let mut mounts = Vec::new();
        for (_dev, point, _fs) in parse_mounts(&std::fs::read_to_string("/proc/self/mounts")?) {
            let c_point = match std::ffi::CString::new(point.clone()) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let mut vfs: libc::statvfs = unsafe { std::mem::zeroed() };
            // SAFETY: c_point is a valid NUL-terminated path; vfs is an out-param
            if unsafe { libc::statvfs(c_point.as_ptr(), &mut vfs) } != 0 {
                continue; // mount may need permissions; skip, never fail the tick
            }
            mounts.push(MountInfo {
                mount_point: point,
                total: vfs.f_blocks as u64 * vfs.f_frsize as u64,
                available: vfs.f_bavail as u64 * vfs.f_frsize as u64,
            });
        }

        // sensor absence is normal (vm, arm sbc, nvidia): None hides the UI
        let cpu_name = std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .as_deref()
            .and_then(parse_cpuinfo_model);
        let hwmon = read_hwmon_cpu_temp(std::path::Path::new("/sys/class/hwmon"));
        let (gpu_name, gpu_util_pct) = read_drm_gpu(std::path::Path::new("/sys/class/drm")).unzip();

        Ok(Snapshot {
            cpu,
            mem,
            net,
            disks,
            mounts,
            procs,
            cpu_name,
            cpu_temp_c: hwmon.as_ref().map(|(t, _)| *t),
            core_temps_c: hwmon.map(|(_, c)| c).unwrap_or_default(),
            gpu_name,
            gpu_util_pct,
            load_avg: super::load_avg(),
            uptime_secs: std::fs::read_to_string("/proc/uptime")
                .ok()
                .and_then(|s| parse_uptime(&s)),
            taken: std::time::Instant::now(),
        })
    }
}

/// /proc/uptime: `<uptime seconds> <idle seconds>`
// Only called by the cfg(linux) LinuxCollector, but unit-tested on every OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn parse_uptime(s: &str) -> Option<u64> {
    s.split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
        .map(|v| v as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = include_str!("../../tests/fixtures/proc_stat.txt");
    const MEMINFO: &str = include_str!("../../tests/fixtures/proc_meminfo.txt");
    const NET_DEV: &str = include_str!("../../tests/fixtures/proc_net_dev.txt");
    const PID_STAT: &str = include_str!("../../tests/fixtures/pid_stat.txt");
    const DISKSTATS: &str = include_str!("../../tests/fixtures/proc_diskstats.txt");
    const PID_IO: &str = include_str!("../../tests/fixtures/pid_io.txt");
    const MOUNTS: &str = include_str!("../../tests/fixtures/proc_mounts.txt");

    #[test]
    fn proc_stat_totals_and_cores() {
        let cpu = parse_proc_stat(STAT).unwrap();
        // busy = user+nice+system+irq+softirq+steal = 100+10+50+0+5+5
        assert_eq!(
            cpu.total,
            CpuTimes {
                busy: 170,
                idle: 840
            }
        );
        assert_eq!(cpu.per_core.len(), 2);
        // cpu0: 50+5+25+0+3+2 busy, 400+20 idle
        assert_eq!(
            cpu.per_core[0],
            CpuTimes {
                busy: 85,
                idle: 420
            }
        );
    }

    #[test]
    fn proc_stat_rejects_garbage() {
        assert!(parse_proc_stat("cpu  1 2\n").is_err());
        assert!(parse_proc_stat("").is_err());
    }

    #[test]
    fn meminfo_bytes() {
        let m = parse_meminfo(MEMINFO).unwrap();
        assert_eq!(m.total, 16_384_000 * 1024);
        assert_eq!(m.available, 8_192_000 * 1024);
        assert_eq!(m.used, m.total - m.available);
        assert_eq!(m.swap_total, 2_097_152 * 1024);
        assert_eq!(m.swap_used, (2_097_152 - 1_048_576) * 1024);
    }

    #[test]
    fn meminfo_missing_total_is_error() {
        assert!(parse_meminfo("MemFree: 5 kB\n").is_err());
    }

    #[test]
    fn uptime_first_field_in_seconds() {
        assert_eq!(parse_uptime("93784.21 501223.94\n"), Some(93_784));
        assert_eq!(parse_uptime("garbage"), None);
        assert_eq!(parse_uptime(""), None);
    }

    #[test]
    fn net_dev_sums_interfaces_without_loopback() {
        let net = parse_net_dev(NET_DEV);
        assert_eq!(net.rx_bytes, 1_000_000 + 250_000);
        assert_eq!(net.tx_bytes, 500_000 + 125_000);
        // eth0 carries the most traffic in the fixture
        assert_eq!(net.iface.as_deref(), Some("eth0"));
    }

    #[test]
    fn pid_stat_parses_comm_with_spaces() {
        // clk_tck 100 -> 1 tick = 10_000_000 ns; page 4096
        let p = parse_pid_stat(12345, PID_STAT, 100, 4096).unwrap();
        assert_eq!(p.pid, 12345);
        assert_eq!(p.name, "tmux: server");
        assert_eq!(p.cpu_ns, (1500 + 500) * 10_000_000);
        assert_eq!(p.rss, 2560 * 4096);
    }

    #[test]
    fn pid_stat_rejects_garbage() {
        assert!(parse_pid_stat(1, "not a stat line", 100, 4096).is_none());
        assert!(parse_pid_stat(1, "1 (x) S 2 3", 100, 4096).is_none());
    }

    #[test]
    fn tid_stat_parses_thread_name_and_cpu() {
        // realistic tokio worker line; comm holds the thread name, not the
        // process name. clk_tck 100 -> 1 tick = 10_000_000 ns
        let line = "12347 (tokio-runtime-w) S 1 12345 12345 0 -1 4194368 100 0 0 0 42 8 0 0 20 0 9 0 12000 100000000 2560 18446744073709551615 1 1 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0\n";
        let t = parse_tid_stat(12347, line, 100).unwrap();
        assert_eq!(t.tid, 12347);
        assert_eq!(t.name, "tokio-runtime-w");
        assert_eq!(t.cpu_ns, (42 + 8) * 10_000_000);
        // parens/spaces inside comm still split at the last ')'
        let weird = "9 (a) b) S 1 2 3 0 -1 0 0 0 0 0 7 3 0 0 20 0 1 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0\n";
        let t = parse_tid_stat(9, weird, 100).unwrap();
        assert_eq!(t.name, "a) b");
        assert_eq!(t.cpu_ns, (7 + 3) * 10_000_000);
    }

    #[test]
    fn tid_stat_rejects_garbage() {
        assert!(parse_tid_stat(1, "not a stat line", 100).is_none());
        assert!(parse_tid_stat(1, "1 (x) S 2 3", 100).is_none());
        assert!(parse_tid_stat(1, "", 100).is_none());
    }

    #[test]
    fn diskstats_normalizes_units() {
        let disks = parse_diskstats(DISKSTATS);
        assert_eq!(disks.len(), 4); // parser keeps everything; the collector filters
        let d = &disks[0];
        assert_eq!(d.name, "nvme0n1");
        assert_eq!(d.read_bytes, 800_000 * 512);
        assert_eq!(d.written_bytes, 400_000 * 512);
        assert_eq!(d.read_ops, 5000);
        assert_eq!(d.write_ops, 3000);
        assert_eq!(d.busy_time_ns, Some(7_000 * 1_000_000));
        assert_eq!(d.io_time_ns, Some((4_000 + 6_000) * 1_000_000));
        assert_eq!(d.weighted_ns, Some(12_000 * 1_000_000));
    }

    #[test]
    fn diskstats_skips_short_lines() {
        assert!(parse_diskstats("1 2 bad 3\n").is_empty());
    }

    #[test]
    fn mounts_keep_real_filesystems_only() {
        let m = parse_mounts(MOUNTS);
        let points: Vec<&str> = m.iter().map(|(_, p, _)| p.as_str()).collect();
        assert_eq!(points, vec!["/", "/data", "/boot"]);
        assert_eq!(m[0].2, "ext4");
    }

    #[test]
    fn mounts_unescape_octal_paths() {
        let m = parse_mounts("/dev/sdb1 /mnt/My\\040Disk vfat rw 0 0\n");
        assert_eq!(m[0].1, "/mnt/My Disk");
        // trailing backslash and short sequences pass through untouched
        assert_eq!(unescape_mount("a\\04"), "a\\04");
        assert_eq!(unescape_mount("tab\\011end"), "tab\tend");
    }

    #[test]
    fn pid_io_reads_block_layer_bytes() {
        // read_bytes/write_bytes, not rchar/wchar: block layer, not syscalls
        assert_eq!(parse_pid_io(PID_IO), Some((12_288, 323_932_160)));
    }

    #[test]
    fn pid_io_rejects_garbage() {
        assert_eq!(parse_pid_io(""), None);
        assert_eq!(parse_pid_io("rchar: 5\n"), None);
    }

    /// unique-per-test scratch dir; recreated fresh each run
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rmon_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn cpuinfo_model_name() {
        let s = "processor\t: 0\nvendor_id\t: AuthenticAMD\nmodel name\t: AMD Ryzen 9 5950X 16-Core Processor\nflags\t: fpu vme\n";
        assert_eq!(
            parse_cpuinfo_model(s).as_deref(),
            Some("AMD Ryzen 9 5950X 16-Core Processor")
        );
    }

    #[test]
    fn cpuinfo_without_model_is_none() {
        // arm SBCs often lack a model name line entirely
        assert_eq!(
            parse_cpuinfo_model("processor\t: 0\nBogoMIPS\t: 48.00\n"),
            None
        );
    }

    #[test]
    fn hwmon_picks_coretemp() {
        let base = scratch("hwmon");
        std::fs::create_dir(base.join("hwmon0")).unwrap();
        std::fs::write(base.join("hwmon0/name"), "nvme\n").unwrap();
        std::fs::create_dir(base.join("hwmon1")).unwrap();
        std::fs::write(base.join("hwmon1/name"), "coretemp\n").unwrap();
        std::fs::write(base.join("hwmon1/temp1_input"), "44000\n").unwrap();
        assert_eq!(read_hwmon_cpu_temp(&base), Some((44.0, vec![])));
        // per-core labels appear in coretemp style
        std::fs::write(base.join("hwmon1/temp2_label"), "Core 0\n").unwrap();
        std::fs::write(base.join("hwmon1/temp2_input"), "41000\n").unwrap();
        std::fs::write(base.join("hwmon1/temp3_label"), "Core 1\n").unwrap();
        std::fs::write(base.join("hwmon1/temp3_input"), "43000\n").unwrap();
        std::fs::write(base.join("hwmon1/temp4_label"), "Package id 0\n").unwrap();
        std::fs::write(base.join("hwmon1/temp4_input"), "45000\n").unwrap();
        assert_eq!(read_hwmon_cpu_temp(&base), Some((44.0, vec![41.0, 43.0])));
    }

    #[test]
    fn hwmon_absent_is_none() {
        let base = scratch("hwmon_absent");
        std::fs::create_dir(base.join("hwmon0")).unwrap();
        std::fs::write(base.join("hwmon0/name"), "nvme\n").unwrap();
        assert_eq!(read_hwmon_cpu_temp(&base), None);
        // missing base dir entirely is also fine
        assert_eq!(read_hwmon_cpu_temp(&base.join("nope")), None);
    }

    #[test]
    fn gpu_busy_percent_reads() {
        let base = scratch("drm");
        std::fs::create_dir_all(base.join("card0/device")).unwrap();
        std::fs::write(base.join("card0/device/gpu_busy_percent"), "22\n").unwrap();
        std::fs::write(
            base.join("card0/device/uevent"),
            "DRIVER=amdgpu\nPCI_CLASS=38000\n",
        )
        .unwrap();
        assert_eq!(read_drm_gpu(&base), Some(("amdgpu".to_string(), 22.0)));
        // nvidia exposes no gpu_busy_percent: absent file means None
        let empty = scratch("drm_empty");
        std::fs::create_dir_all(empty.join("card0/device")).unwrap();
        assert_eq!(read_drm_gpu(&empty), None);
    }
}
