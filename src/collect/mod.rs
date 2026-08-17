pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
mod macos_iokit;
#[cfg(target_os = "macos")]
mod macos_sensors;

use std::time::Instant;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CollectError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse: {0}")]
    // Constructed only by the linux parsers; outside tests those are cfg(linux)-only.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Parse(String),
    #[error("{call} failed: {code}")]
    // Constructed only by the macos ffi collectors.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    Sys { call: &'static str, code: i64 },
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuTimes {
    pub busy: u64,
    pub idle: u64,
}

#[derive(Debug, Clone, Default)]
pub struct CpuSnapshot {
    pub total: CpuTimes,
    pub per_core: Vec<CpuTimes>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MemSnapshot {
    /// all fields in bytes
    pub total: u64,
    pub used: u64,
    pub available: u64,
    pub swap_total: u64,
    pub swap_used: u64,
}

#[derive(Debug, Clone, Default)]
pub struct NetSnapshot {
    /// cumulative bytes, loopback excluded
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// busiest non-loopback interface by cumulative traffic
    pub iface: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DiskStats {
    pub name: String,
    pub read_bytes: u64,
    pub written_bytes: u64,
    pub read_ops: u64,
    pub write_ops: u64,
    /// cumulative device-busy time; None when the platform lacks it
    pub busy_time_ns: Option<u64>,
    /// cumulative read+write service time; None when the driver lacks it
    pub io_time_ns: Option<u64>,
    /// linux weighted io time (queue-depth source); None on macos
    pub weighted_ns: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct MountInfo {
    pub mount_point: String,
    pub total: u64,
    pub available: u64,
}

#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub tid: u64,
    /// thread name; empty when the OS has none
    pub name: String,
    /// cumulative cpu time in ns (user + system)
    pub cpu_ns: u64,
}

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: i32,
    /// parent pid; 0 = unknown/no parent
    pub ppid: i32,
    pub name: String,
    /// cumulative cpu time in ns (user + system)
    pub cpu_ns: u64,
    /// resident set size in bytes
    pub rss: u64,
    /// cumulative bytes actually read from / written to the block layer;
    /// None when the process is not ours to inspect
    pub disk_read: Option<u64>,
    pub disk_written: Option<u64>,
    /// per-thread breakdown; only filled when the caller asked for it
    /// (it costs a pile of extra syscalls), empty on per-process errors too
    pub threads: Vec<ThreadInfo>,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub cpu: CpuSnapshot,
    pub mem: MemSnapshot,
    pub net: NetSnapshot,
    pub disks: Vec<DiskStats>,
    pub mounts: Vec<MountInfo>,
    pub procs: Vec<ProcessInfo>,
    /// static per boot but cheap; collectors fill every tick
    pub cpu_name: Option<String>,
    pub cpu_temp_c: Option<f64>,
    /// per-core (or per-sensor-group) temps in reported order; empty = unknown
    pub core_temps_c: Vec<f64>,
    pub gpu_name: Option<String>,
    pub gpu_util_pct: Option<f64>,
    pub load_avg: Option<[f64; 3]>,
    pub uptime_secs: Option<u64>,
    pub taken: Instant,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            cpu: CpuSnapshot::default(),
            mem: MemSnapshot::default(),
            net: NetSnapshot::default(),
            disks: Vec::new(),
            mounts: Vec::new(),
            procs: Vec::new(),
            cpu_name: None,
            cpu_temp_c: None,
            core_temps_c: Vec::new(),
            gpu_name: None,
            gpu_util_pct: None,
            load_avg: None,
            uptime_secs: None,
            taken: Instant::now(),
        }
    }
}

pub trait Collector: Send {
    /// `threads`: also collect per-thread info; skipped when false because
    /// it's hundreds of extra file reads/syscalls per tick
    fn collect(&mut self, threads: bool) -> Result<Snapshot, CollectError>;
}

/// 1/5/15 minute load averages; same libc call on linux and macos
pub fn load_avg() -> Option<[f64; 3]> {
    let mut l = [0f64; 3];
    // SAFETY: getloadavg writes at most 3 doubles into a 3-double buffer
    let n = unsafe { libc::getloadavg(l.as_mut_ptr(), 3) };
    (n == 3).then_some(l)
}
/// busy percent between two readings, clamped 0..=100
pub fn cpu_percent(prev: CpuTimes, curr: CpuTimes) -> f64 {
    let busy = curr.busy.saturating_sub(prev.busy);
    let idle = curr.idle.saturating_sub(prev.idle);
    let total = busy + idle;
    if total == 0 {
        return 0.0;
    }
    busy as f64 * 100.0 / total as f64
}

pub fn new_collector() -> Box<dyn Collector> {
    #[cfg(target_os = "linux")]
    {
        Box::new(linux::LinuxCollector)
    }
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacCollector)
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("rmon supports linux and macos only");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_from_deltas() {
        let prev = CpuTimes {
            busy: 100,
            idle: 900,
        };
        let curr = CpuTimes {
            busy: 150,
            idle: 950,
        };
        // 50 busy over 100 total ticks
        assert!((cpu_percent(prev, curr) - 50.0).abs() < 1e-9);
    }

    #[test]
    fn percent_zero_delta_is_zero() {
        let t = CpuTimes {
            busy: 100,
            idle: 900,
        };
        assert_eq!(cpu_percent(t, t), 0.0);
    }

    #[test]
    fn percent_survives_counter_wrap() {
        // macOS per-core counters are u32 and wrap; a wrapped reading
        // must clamp, not panic or go negative
        let prev = CpuTimes {
            busy: u32::MAX as u64,
            idle: 100,
        };
        let curr = CpuTimes { busy: 5, idle: 110 };
        let p = cpu_percent(prev, curr);
        assert!((0.0..=100.0).contains(&p));
    }
}
