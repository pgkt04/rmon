use super::{
    CollectError, Collector, CpuSnapshot, CpuTimes, MemSnapshot, MountInfo, NetSnapshot,
    ProcessInfo, Snapshot,
};
use libc::{CTL_NET, NET_RT_IFLIST2, PF_ROUTE, c_int, c_uint, c_void, sysctl, sysctlbyname};

type KernReturn = c_int;
type MachPort = c_uint;
type NaturalT = c_uint;

const PROCESSOR_CPU_LOAD_INFO: c_int = 2;
const CPU_STATE_USER: usize = 0;
const CPU_STATE_SYSTEM: usize = 1;
const CPU_STATE_IDLE: usize = 2;
const CPU_STATE_NICE: usize = 3;
const CPU_STATE_MAX: usize = 4;
const HOST_VM_INFO64: c_int = 4;
const RTM_IFINFO2: u8 = 0x12;
const IFT_LOOP: u8 = 0x18;
const PROC_ALL_PIDS: u32 = 1;
const PROC_PIDTASKINFO: c_int = 4;
const PROC_NAME_LEN: usize = 64;

/// mach/vm_statistics.h vm_statistics64 — field order matters
#[repr(C)]
#[derive(Default)]
struct VmStatistics64 {
    free_count: u32,
    active_count: u32,
    inactive_count: u32,
    wire_count: u32,
    zero_fill_count: u64,
    reactivations: u64,
    pageins: u64,
    pageouts: u64,
    faults: u64,
    cow_faults: u64,
    lookups: u64,
    hits: u64,
    purges: u64,
    purgeable_count: u32,
    speculative_count: u32,
    decompressions: u64,
    compressions: u64,
    swapins: u64,
    swapouts: u64,
    compressor_page_count: u32,
    throttled_count: u32,
    external_page_count: u32,
    internal_page_count: u32,
    total_uncompressed_pages_in_compressor: u64,
}

/// fixed head of net/if.h if_msghdr2 (if_data64 follows it)
#[repr(C)]
struct IfMsghdr2Prefix {
    ifm_msglen: u16,
    ifm_version: u8,
    ifm_type: u8,
    ifm_addrs: c_int,
    ifm_flags: c_int,
    ifm_index: u16,
    ifm_snd_len: c_int,
    ifm_snd_maxlen: c_int,
    ifm_snd_drops: c_int,
    ifm_timer: c_int,
}

/// leading fields of net/if_var.h if_data64 — enough to reach the byte counters
#[repr(C)]
struct IfData64Head {
    ifi_type: u8,
    ifi_typelen: u8,
    ifi_physical: u8,
    ifi_addrlen: u8,
    ifi_hdrlen: u8,
    ifi_recvquota: u8,
    ifi_xmitquota: u8,
    ifi_unused1: u8,
    ifi_mtu: u32,
    ifi_metric: u32,
    ifi_baudrate: u64,
    ifi_ipackets: u64,
    ifi_ierrors: u64,
    ifi_opackets: u64,
    ifi_oerrors: u64,
    ifi_collisions: u64,
    ifi_ibytes: u64,
    ifi_obytes: u64,
}

/// sys/proc_info.h proc_taskinfo
#[repr(C)]
#[derive(Default)]
struct ProcTaskInfo {
    pti_virtual_size: u64,
    pti_resident_size: u64,
    pti_total_user: u64,
    pti_total_system: u64,
    pti_threads_user: u64,
    pti_threads_system: u64,
    pti_policy: i32,
    pti_faults: i32,
    pti_pageins: i32,
    pti_cow_faults: i32,
    pti_messages_sent: i32,
    pti_messages_received: i32,
    pti_syscalls_mach: i32,
    pti_syscalls_unix: i32,
    pti_csw: i32,
    pti_threadnum: i32,
    pti_numrunning: i32,
    pti_priority: i32,
}

const RUSAGE_INFO_V2: c_int = 2;

/// sys/resource.h rusage_info_v2 — only the tail fields matter here,
/// but the full layout must match for the kernel write to land correctly
#[repr(C)]
#[derive(Default)]
struct RusageInfoV2 {
    ri_uuid: [u8; 16],
    ri_user_time: u64,
    ri_system_time: u64,
    ri_pkg_idle_wkups: u64,
    ri_interrupt_wkups: u64,
    ri_pageins: u64,
    ri_wired_size: u64,
    ri_resident_size: u64,
    ri_phys_footprint: u64,
    ri_proc_start_abstime: u64,
    ri_proc_exit_abstime: u64,
    ri_child_user_time: u64,
    ri_child_system_time: u64,
    ri_child_pkg_idle_wkups: u64,
    ri_child_interrupt_wkups: u64,
    ri_child_pageins: u64,
    ri_child_elapsed_abstime: u64,
    ri_diskio_bytesread: u64,
    ri_diskio_byteswritten: u64,
}

#[repr(C)]
struct TimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_host_self() -> MachPort;
    fn host_statistics64(
        host: MachPort,
        flavor: c_int,
        info: *mut c_void,
        count: *mut c_uint,
    ) -> KernReturn;
    fn host_processor_info(
        host: MachPort,
        flavor: c_int,
        ncpu: *mut NaturalT,
        info: *mut *mut c_int,
        info_count: *mut NaturalT,
    ) -> KernReturn;
    fn vm_deallocate(task: MachPort, address: usize, size: usize) -> KernReturn;
    fn proc_listpids(kind: u32, typeinfo: u32, buffer: *mut c_void, buffersize: c_int) -> c_int;
    fn proc_pidinfo(
        pid: c_int,
        flavor: c_int,
        arg: u64,
        buffer: *mut c_void,
        buffersize: c_int,
    ) -> c_int;
    fn proc_name(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
    // buffer is a PLAIN pointer to the flavor's struct (cast to *mut c_void);
    // a double pointer here corrupts the caller's stack
    fn proc_pid_rusage(pid: c_int, flavor: c_int, buffer: *mut c_void) -> c_int;
    fn mach_timebase_info(info: *mut TimebaseInfo) -> c_int;
    // mach_task_self() is a C macro over this static
    static mach_task_self_: MachPort;
}

pub struct MacCollector;

impl Collector for MacCollector {
    fn collect(&mut self) -> Result<Snapshot, CollectError> {
        let brand = cached_cpu_brand();
        Ok(Snapshot {
            cpu: cpu_snapshot()?,
            mem: mem_snapshot()?,
            net: net_snapshot()?,
            disks: super::macos_iokit::disks()?,
            mounts: mounts_snapshot(),
            procs: procs_snapshot()?,
            cpu_name: brand.clone(),
            cpu_temp_c: super::macos_sensors::cpu_temp(),
            // one SoC: the gpu carries the cpu brand (Activity Monitor does the same)
            gpu_name: brand,
            gpu_util_pct: super::macos_sensors::gpu_util(),
            load_avg: super::load_avg(),
            uptime_secs: uptime_secs(),
            taken: std::time::Instant::now(),
        })
    }
}

/// brand string never changes per boot; ask the kernel once
fn cached_cpu_brand() -> Option<String> {
    static BRAND: std::sync::LazyLock<Option<String>> =
        std::sync::LazyLock::new(super::macos_sensors::cpu_brand);
    BRAND.clone()
}

/// seconds since boot via kern.boottime (a timeval of the boot moment)
fn uptime_secs() -> Option<u64> {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = size_of::<libc::timeval>();
    // SAFETY: kern.boottime fills exactly one timeval; len carries the size
    let rc = unsafe {
        sysctlbyname(
            c"kern.boottime".as_ptr(),
            &mut tv as *mut _ as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || tv.tv_sec <= 0 {
        return None;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    now.checked_sub(tv.tv_sec as u64)
}

fn mounts_snapshot() -> Vec<MountInfo> {
    // getmntinfo shares one static buffer across threads (racy under parallel
    // collect() calls); getfsstat into a caller-owned buffer is thread-safe.
    // MNT_NOWAIT = 2
    // SAFETY: null buffer asks for the current mount count only
    let n = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, 2) };
    if n <= 0 {
        return Vec::new();
    }
    let mut buf: Vec<libc::statfs> = Vec::with_capacity(n as usize);
    let cap = (buf.capacity() * size_of::<libc::statfs>()) as c_int;
    // SAFETY: buf has capacity for cap bytes; getfsstat copies at most that much
    let n = unsafe { libc::getfsstat(buf.as_mut_ptr(), cap, 2) };
    if n <= 0 {
        return Vec::new();
    }
    // SAFETY: getfsstat initialized exactly n entries
    unsafe { buf.set_len(n as usize) };
    let mut out = Vec::new();
    for m in &buf {
        // SAFETY: f_mntonname/f_fstypename are NUL-terminated fixed arrays
        let point = unsafe { std::ffi::CStr::from_ptr(m.f_mntonname.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        let fs = unsafe { std::ffi::CStr::from_ptr(m.f_fstypename.as_ptr()) }.to_string_lossy();
        // keep the root volume and external/user volumes; hide system noise
        let keep = point == "/" || point.starts_with("/Volumes/");
        if !keep || !(fs == "apfs" || fs == "hfs" || fs == "exfat" || fs == "msdos") {
            continue;
        }
        let total = m.f_blocks * m.f_bsize as u64;
        if total == 0 {
            continue;
        }
        out.push(MountInfo {
            mount_point: point,
            total,
            available: m.f_bavail * m.f_bsize as u64,
        });
    }
    out
}

fn cpu_snapshot() -> Result<CpuSnapshot, CollectError> {
    let mut ncpu: NaturalT = 0;
    let mut info: *mut c_int = std::ptr::null_mut();
    let mut info_count: NaturalT = 0;
    // SAFETY: out-params per the host_processor_info contract; buffer freed below
    let kr = unsafe {
        host_processor_info(
            mach_host_self(),
            PROCESSOR_CPU_LOAD_INFO,
            &mut ncpu,
            &mut info,
            &mut info_count,
        )
    };
    if kr != 0 {
        return Err(CollectError::Sys {
            call: "host_processor_info",
            code: kr as i64,
        });
    }
    // SAFETY: kernel returned info_count ints at info (ncpu * CPU_STATE_MAX)
    let ticks = unsafe { std::slice::from_raw_parts(info, info_count as usize) };
    let mut total = CpuTimes::default();
    let mut per_core = Vec::with_capacity(ncpu as usize);
    for chunk in ticks.chunks_exact(CPU_STATE_MAX) {
        // ticks are u32 in the kernel; going through u32 avoids sign-extension at counter wrap
        let t = CpuTimes {
            busy: chunk[CPU_STATE_USER] as u32 as u64
                + chunk[CPU_STATE_SYSTEM] as u32 as u64
                + chunk[CPU_STATE_NICE] as u32 as u64,
            idle: chunk[CPU_STATE_IDLE] as u32 as u64,
        };
        total.busy += t.busy;
        total.idle += t.idle;
        per_core.push(t);
    }
    // SAFETY: buffer was vm_allocated by the kernel for this task
    unsafe {
        vm_deallocate(
            mach_task_self_,
            info as usize,
            info_count as usize * size_of::<c_int>(),
        );
    }
    Ok(CpuSnapshot { total, per_core })
}

fn mem_snapshot() -> Result<MemSnapshot, CollectError> {
    let mut total: u64 = 0;
    let mut len = size_of::<u64>();
    // SAFETY: hw.memsize yields a u64; len tells the kernel the buffer size
    let rc = unsafe {
        sysctlbyname(
            c"hw.memsize".as_ptr(),
            &mut total as *mut _ as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(CollectError::Sys {
            call: "sysctlbyname(hw.memsize)",
            code: rc as i64,
        });
    }

    let mut vm = VmStatistics64::default();
    let mut count = (size_of::<VmStatistics64>() / size_of::<u32>()) as c_uint;
    // SAFETY: count is the buffer size in u32 units, per host_statistics64 contract
    let kr = unsafe {
        host_statistics64(
            mach_host_self(),
            HOST_VM_INFO64,
            &mut vm as *mut _ as *mut c_void,
            &mut count,
        )
    };
    if kr != 0 {
        return Err(CollectError::Sys {
            call: "host_statistics64",
            code: kr as i64,
        });
    }

    // vm.swapusage layout from sys/sysctl.h
    #[repr(C)]
    #[derive(Default)]
    struct XswUsage {
        total: u64,
        avail: u64,
        used: u64,
        pagesize: u32,
        encrypted: u32,
    }
    let mut xsw = XswUsage::default();
    let mut xsw_len = size_of::<XswUsage>();
    // SAFETY: buffer is a properly sized xsw_usage; len tells the kernel its size
    let rc = unsafe {
        sysctlbyname(
            c"vm.swapusage".as_ptr(),
            &mut xsw as *mut _ as *mut c_void,
            &mut xsw_len,
            std::ptr::null_mut(),
            0,
        )
    };
    // no swap info is not fatal; the meter just stays hidden
    let (swap_total, swap_used) = if rc == 0 {
        (xsw.total, xsw.used)
    } else {
        (0, 0)
    };

    // SAFETY: sysconf with a valid constant is always safe to call
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    // rough Activity-Monitor-style "used"; good enough for a meter
    let used =
        (vm.active_count as u64 + vm.wire_count as u64 + vm.compressor_page_count as u64) * page;
    Ok(MemSnapshot {
        total,
        used: used.min(total),
        available: total.saturating_sub(used),
        swap_total,
        swap_used,
    })
}

fn net_snapshot() -> Result<NetSnapshot, CollectError> {
    let mut mib = [CTL_NET, PF_ROUTE, 0, 0, NET_RT_IFLIST2, 0];
    let mut len: usize = 0;
    // SAFETY: null buffer asks for the required size
    let rc = unsafe {
        sysctl(
            mib.as_mut_ptr(),
            6,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(CollectError::Sys {
            call: "sysctl(NET_RT_IFLIST2 size)",
            code: rc as i64,
        });
    }
    let mut buf = vec![0u8; len];
    // SAFETY: buf is len bytes; the kernel updates len to what it wrote
    let rc = unsafe {
        sysctl(
            mib.as_mut_ptr(),
            6,
            buf.as_mut_ptr() as *mut c_void,
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return Err(CollectError::Sys {
            call: "sysctl(NET_RT_IFLIST2)",
            code: rc as i64,
        });
    }

    let mut net = NetSnapshot::default();
    let mut off = 0usize;
    while off + size_of::<IfMsghdr2Prefix>() <= len {
        // SAFETY: bounds-checked above; the routing message stream is packed,
        // so read the header unaligned
        let hdr =
            unsafe { std::ptr::read_unaligned(buf.as_ptr().add(off) as *const IfMsghdr2Prefix) };
        if hdr.ifm_msglen == 0 {
            break;
        }
        if hdr.ifm_type == RTM_IFINFO2
            && off + size_of::<IfMsghdr2Prefix>() + size_of::<IfData64Head>() <= len
        {
            // SAFETY: bounds-checked; unaligned read for the same reason
            let d = unsafe {
                std::ptr::read_unaligned(
                    buf.as_ptr().add(off + size_of::<IfMsghdr2Prefix>()) as *const IfData64Head
                )
            };
            if d.ifi_type != IFT_LOOP {
                net.rx_bytes += d.ifi_ibytes;
                net.tx_bytes += d.ifi_obytes;
            }
        }
        off += hdr.ifm_msglen as usize;
    }
    Ok(net)
}

fn procs_snapshot() -> Result<Vec<ProcessInfo>, CollectError> {
    // SAFETY: null buffer asks for the byte count needed
    let bytes = unsafe { proc_listpids(PROC_ALL_PIDS, 0, std::ptr::null_mut(), 0) };
    if bytes <= 0 {
        return Err(CollectError::Sys {
            call: "proc_listpids(size)",
            code: bytes as i64,
        });
    }
    let mut pids = vec![0i32; bytes as usize / 4 + 64];
    // SAFETY: buffer sized above with headroom for new processes
    let bytes = unsafe {
        proc_listpids(
            PROC_ALL_PIDS,
            0,
            pids.as_mut_ptr() as *mut c_void,
            (pids.len() * 4) as c_int,
        )
    };
    if bytes <= 0 {
        return Err(CollectError::Sys {
            call: "proc_listpids",
            code: bytes as i64,
        });
    }
    pids.truncate(bytes as usize / 4);

    let mut tb = TimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: plain out-param
    unsafe { mach_timebase_info(&mut tb) };
    let (numer, denom) = (tb.numer.max(1) as u128, tb.denom.max(1) as u128);

    let mut procs = Vec::with_capacity(pids.len());
    for &pid in &pids {
        if pid <= 0 {
            continue;
        }
        let mut ti = ProcTaskInfo::default();
        let sz = size_of::<ProcTaskInfo>() as c_int;
        // SAFETY: buffer is exactly PROC_PIDTASKINFO-sized
        let got = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTASKINFO,
                0,
                &mut ti as *mut _ as *mut c_void,
                sz,
            )
        };
        if got != sz {
            continue; // no permission for other users' processes without root
        }
        let mut name_buf = [0u8; PROC_NAME_LEN];
        // SAFETY: buffer is PROC_NAME_LEN bytes
        unsafe {
            proc_name(
                pid,
                name_buf.as_mut_ptr() as *mut c_void,
                PROC_NAME_LEN as u32,
            )
        };
        // the buffer beyond the first NUL is garbage — cut there, do not trim
        let end = name_buf
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(PROC_NAME_LEN);
        let name = String::from_utf8_lossy(&name_buf[..end]).into_owned();
        let cpu_ns = ((ti.pti_total_user as u128 + ti.pti_total_system as u128) * numer / denom)
            .min(u64::MAX as u128) as u64;
        let mut ru = RusageInfoV2::default();
        // SAFETY: buffer is exactly a rusage_info_v2; the kernel fills it on rc==0
        let rc = unsafe { proc_pid_rusage(pid, RUSAGE_INFO_V2, &mut ru as *mut _ as *mut c_void) };
        let (disk_read, disk_written) = if rc == 0 {
            (
                Some(ru.ri_diskio_bytesread),
                Some(ru.ri_diskio_byteswritten),
            )
        } else {
            (None, None) // other users' processes need root
        };
        procs.push(ProcessInfo {
            pid,
            name,
            cpu_ns,
            rss: ti.pti_resident_size,
            disk_read,
            disk_written,
        });
    }
    Ok(procs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{Collector, cpu_percent};

    #[test]
    fn collects_sane_values() {
        let mut c = MacCollector;
        let a = c.collect().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let b = c.collect().unwrap();

        assert!(!a.cpu.per_core.is_empty());
        assert_eq!(a.cpu.per_core.len(), b.cpu.per_core.len());
        let p = cpu_percent(a.cpu.total, b.cpu.total);
        assert!((0.0..=100.0).contains(&p));

        assert!(a.mem.total > 0);
        assert!(a.mem.used <= a.mem.total);
        assert!(a.mem.available <= a.mem.total);
    }

    #[test]
    fn net_and_procs_are_sane() {
        let mut c = MacCollector;
        let s = c.collect().unwrap();
        // this machine always has non-loopback traffic and processes
        assert!(s.net.rx_bytes > 0);
        assert!(s.net.tx_bytes > 0);
        assert!(!s.procs.is_empty());
        let me = std::process::id() as i32;
        let self_proc = s.procs.iter().find(|p| p.pid == me);
        let self_proc = self_proc.expect("invariant: this test process is running");
        assert!(!self_proc.name.is_empty());
        assert!(!self_proc.name.contains('\0'));
        assert!(self_proc.rss > 0);
    }

    #[test]
    fn disks_and_mounts_are_sane() {
        let mut c = MacCollector;
        let s = c.collect().unwrap();
        // every mac has at least one physical whole disk with counters
        assert!(!s.disks.is_empty());
        assert!(
            s.disks
                .iter()
                .any(|d| d.read_bytes > 0 && d.io_time_ns.is_some())
        );
        // root volume is always mounted
        let root = s.mounts.iter().find(|m| m.mount_point == "/");
        let root = root.expect("invariant: root mount exists");
        assert!(root.total > 0 && root.available > 0 && root.available < root.total);
        // system-noise mounts are filtered
        assert!(
            !s.mounts
                .iter()
                .any(|m| m.mount_point.starts_with("/System/Volumes/"))
        );
    }

    #[test]
    fn own_process_reports_disk_io() {
        // force some real writeback so the counters are non-zero-able,
        // then check our own pid carries Some counters
        let path = std::env::temp_dir().join("rmon_io_probe");
        std::fs::write(&path, vec![7u8; 1 << 20]).unwrap();
        let mut c = MacCollector;
        let s = c.collect().unwrap();
        let _ = std::fs::remove_file(&path);
        let me = std::process::id() as i32;
        let p = s.procs.iter().find(|p| p.pid == me).unwrap();
        assert!(p.disk_read.is_some() && p.disk_written.is_some());
    }
}
