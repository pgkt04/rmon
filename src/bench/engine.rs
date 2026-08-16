use std::alloc::{Layout, alloc, dealloc};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::{BenchConfig, BenchEvent, BenchReport, Lcg, TestKind, TestResult, percentile};

const SEQ_BLOCK: usize = 1 << 20;
const RAND_BLOCK: usize = 4096;
const ALIGN: usize = 4096;
/// emit a progress event roughly this often
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// floor for the random tests so secs=0 configs still measure something
const MIN_RAND_OPS: usize = 64;

/// page-aligned buffer for O_DIRECT-compatible io
struct AlignedBuf {
    ptr: *mut u8,
    layout: Layout,
    len: usize,
}

impl AlignedBuf {
    fn new_filled(len: usize) -> io::Result<Self> {
        // alloc with a zero-size layout is UB; make the SAFETY claim checkable
        debug_assert!(len > 0, "invariant: AlignedBuf needs a non-zero length");
        let layout = Layout::from_size_align(len, ALIGN)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
        // SAFETY: layout has non-zero size (asserted above); null check below
        let ptr = unsafe { alloc(layout) };
        if ptr.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "aligned alloc failed",
            ));
        }
        let mut buf = Self { ptr, layout, len };
        // pseudorandom fill: zeros would let compressing filesystems cheat
        let mut rng = Lcg(0x9E37_79B9_7F4A_7C15);
        for b in buf.as_mut_slice() {
            *b = (rng.next() >> 24) as u8;
        }
        Ok(buf)
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr is valid for len bytes for the lifetime of self
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: ptr is valid for len bytes; exclusive via &mut self
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for AlignedBuf {
    fn drop(&mut self) {
        // SAFETY: ptr/layout came from alloc above
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

/// removes the bench file even on early-return/error paths
struct FileGuard(PathBuf);

impl Drop for FileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// open with direct io; Ok((file, true)) when direct io engaged,
/// Ok((file, false)) after a buffered fallback (e.g. tmpfs)
fn open_bench_file(path: &Path) -> io::Result<(File, bool)> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // fs without O_DIRECT support (e.g. tmpfs) falls through to buffered
        if let Ok(f) = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(true)
            .custom_flags(libc::O_DIRECT)
            .open(path)
        {
            return Ok((f, true));
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: fd is open; F_NOCACHE flips the page-cache bypass flag
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_NOCACHE, 1) };
        if rc == 0 {
            return Ok((file, true));
        }
    }
    Ok((file, false))
}

/// open the device READ-ONLY BY CONSTRUCTION: read(true) only — no write,
/// no create, no truncate anywhere on this path.
/// Ok((file, true)) when page-cache-bypassing (raw) io is in effect.
fn open_device(path: &Path) -> io::Result<(File, bool)> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if let Ok(f) = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECT)
            .open(path)
        {
            return Ok((f, true));
        }
        // regular file standing in for a device, or fs refusing O_DIRECT
        let file = OpenOptions::new().read(true).open(path)?;
        Ok((file, false))
    }
    #[cfg(not(target_os = "linux"))]
    {
        // macOS: plain O_RDONLY — the /dev/rdiskN character device already
        // bypasses the page cache, so O_DIRECT is neither available nor needed
        let file = OpenOptions::new().read(true).open(path)?;
        let raw = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rdisk"));
        Ok((file, raw))
    }
}

/// byte size of the bench target: device ioctls on macOS, lseek END otherwise
/// (lseek works for linux block devices and for regular files on both)
fn probe_size(file: &File) -> io::Result<u64> {
    use std::os::fd::AsRawFd;
    #[cfg(target_os = "macos")]
    {
        // sys/disk.h: _IOR('d', 24, uint32_t) / _IOR('d', 25, uint64_t)
        const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
        const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x4008_6419;
        let mut blk_size: u32 = 0;
        let mut blk_count: u64 = 0;
        // SAFETY: fd is a valid open descriptor owned by `file`; the pointer
        // arguments are correctly-sized stack variables for these _IOR requests
        let r1 = unsafe { libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut blk_size) };
        // SAFETY: same fd invariant; blk_count is a u64 as DKIOCGETBLOCKCOUNT requires
        let r2 = unsafe { libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKCOUNT, &mut blk_count) };
        if r1 == 0 && r2 == 0 {
            return Ok(u64::from(blk_size) * blk_count);
        }
        // regular file: ioctl fails with ENOTTY; fall through to lseek
    }
    // SAFETY: fd is a valid open descriptor owned by `file`
    let off = unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_END) };
    if off < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(off as u64)
}

/// io block size of the target; 4096 default (linux and non-device files)
fn probe_block_size(file: &File) -> usize {
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x4004_6418;
        let mut blk_size: u32 = 0;
        // SAFETY: fd is a valid open descriptor owned by `file`; blk_size is a
        // u32 stack variable as DKIOCGETBLOCKSIZE requires
        let rc = unsafe { libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut blk_size) };
        if rc == 0 && blk_size > 0 {
            return blk_size as usize;
        }
    }
    let _ = file;
    ALIGN
}

pub fn run(cfg: &BenchConfig, emit: &mut dyn FnMut(BenchEvent)) {
    match run_inner(cfg, emit) {
        Ok(report) => emit(BenchEvent::Finished(report)),
        Err(e) => {
            let mut msg = e.to_string();
            if cfg.device.is_some() && e.kind() == io::ErrorKind::PermissionDenied {
                msg.push_str(" (try sudo)");
            }
            emit(BenchEvent::Error(msg));
        }
    }
}

fn run_inner(cfg: &BenchConfig, emit: &mut dyn FnMut(BenchEvent)) -> io::Result<BenchReport> {
    if let Some(device) = &cfg.device {
        return run_device(device, cfg, emit);
    }
    // size must cover whole blocks
    let size = (cfg.size / SEQ_BLOCK as u64).max(1) * SEQ_BLOCK as u64;
    let path = cfg
        .target_dir
        .join(format!("rmon-bench-{}.tmp", std::process::id()));
    let _guard = FileGuard(path.clone());
    let (file, direct) = open_bench_file(&path)?;

    let mut buf = AlignedBuf::new_filled(SEQ_BLOCK)?;
    let results = vec![
        seq_test(TestKind::SeqWrite, &file, &mut buf, size, emit)?,
        seq_test(TestKind::SeqRead, &file, &mut buf, size, emit)?,
        rand_test(
            TestKind::RandRead,
            &file,
            &mut buf,
            size,
            RAND_BLOCK,
            cfg,
            emit,
        )?,
        rand_test(
            TestKind::RandWrite,
            &file,
            &mut buf,
            size,
            RAND_BLOCK,
            cfg,
            emit,
        )?,
    ];

    Ok(BenchReport {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        target: cfg.target_dir.to_string_lossy().into_owned(),
        size,
        direct,
        results,
    })
}

/// read-only raw device bench: SeqRead + RandRead over min(device size, cap).
/// the device file is opened without write access (see open_device); there is
/// no code path here that writes, creates, or truncates anything.
fn run_device(
    device: &Path,
    cfg: &BenchConfig,
    emit: &mut dyn FnMut(BenchEvent),
) -> io::Result<BenchReport> {
    let (file, direct) = open_device(device)?;
    let probed = probe_size(&file)?;
    // cap: cfg.size (cli sets it to --size-mb, or a 1 GiB device default);
    // span must cover whole seq blocks
    let size = (probed.min(cfg.size) / SEQ_BLOCK as u64) * SEQ_BLOCK as u64;
    if size == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("device too small to bench ({probed} bytes)"),
        ));
    }
    // alignment invariant: raw device reads must be block-multiples; clamp the
    // rand-read block up when the device block exceeds 4096 (buf is SEQ_BLOCK)
    let rand_block = probe_block_size(&file).clamp(RAND_BLOCK, SEQ_BLOCK);

    let mut buf = AlignedBuf::new_filled(SEQ_BLOCK)?;
    let results = vec![
        seq_test(TestKind::SeqRead, &file, &mut buf, size, emit)?,
        rand_test(
            TestKind::RandRead,
            &file,
            &mut buf,
            size,
            rand_block,
            cfg,
            emit,
        )?,
    ];

    Ok(BenchReport {
        ts: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        target: device.to_string_lossy().into_owned(),
        size,
        direct,
        results,
    })
}

fn seq_test(
    kind: TestKind,
    file: &File,
    buf: &mut AlignedBuf,
    size: u64,
    emit: &mut dyn FnMut(BenchEvent),
) -> io::Result<TestResult> {
    let start = Instant::now();
    // seed one interval back: even sub-100ms tests emit at least one progress
    let mut last_progress = start.checked_sub(PROGRESS_EVERY).unwrap_or(start);
    let mut off = 0u64;
    while off < size {
        match kind {
            TestKind::SeqWrite => file.write_all_at(buf.as_slice(), off)?,
            _ => file.read_exact_at(buf.as_mut_slice(), off)?,
        }
        off += SEQ_BLOCK as u64;
        if last_progress.elapsed() >= PROGRESS_EVERY {
            last_progress = Instant::now();
            emit(BenchEvent::Progress {
                kind,
                frac: off as f64 / size as f64,
                bytes_per_sec: off as f64 / start.elapsed().as_secs_f64().max(1e-9),
            });
        }
    }
    if kind == TestKind::SeqWrite {
        // land it on the platter; part of the honest write cost
        file.sync_all()?;
    }
    let secs = start.elapsed().as_secs_f64().max(1e-9);
    let result = TestResult {
        kind,
        bytes_per_sec: size as f64 / secs,
        iops: (size / SEQ_BLOCK as u64) as f64 / secs,
        p50_us: None,
        p99_us: None,
    };
    emit(BenchEvent::TestDone(result.clone()));
    Ok(result)
}

fn rand_test(
    kind: TestKind,
    file: &File,
    buf: &mut AlignedBuf,
    size: u64,
    block: usize,
    cfg: &BenchConfig,
    emit: &mut dyn FnMut(BenchEvent),
) -> io::Result<TestResult> {
    let blocks = size / block as u64;
    let budget = Duration::from_secs(cfg.secs_per_rand_test);
    let mut rng = Lcg(0xD1B5_4A32_D192_ED03);
    let mut latencies_us: Vec<u64> = Vec::with_capacity(65536);
    let start = Instant::now();
    // seed one interval back: even sub-100ms tests emit at least one progress
    let mut last_progress = start.checked_sub(PROGRESS_EVERY).unwrap_or(start);
    while start.elapsed() < budget || latencies_us.len() < MIN_RAND_OPS {
        let off = (rng.next() % blocks) * block as u64;
        let t0 = Instant::now();
        match kind {
            TestKind::RandWrite => file.write_all_at(&buf.as_slice()[..block], off)?,
            _ => file.read_exact_at(&mut buf.as_mut_slice()[..block], off)?,
        }
        latencies_us.push(t0.elapsed().as_micros().min(u128::from(u64::MAX)) as u64);
        if last_progress.elapsed() >= PROGRESS_EVERY {
            last_progress = Instant::now();
            let secs = start.elapsed().as_secs_f64().max(1e-9);
            emit(BenchEvent::Progress {
                kind,
                frac: (start.elapsed().as_secs_f64() / budget.as_secs_f64().max(1e-9)).min(1.0),
                bytes_per_sec: latencies_us.len() as f64 * block as f64 / secs,
            });
        }
    }
    if kind == TestKind::RandWrite {
        file.sync_all()?;
    }
    let secs = start.elapsed().as_secs_f64().max(1e-9);
    let ops = latencies_us.len() as f64;
    latencies_us.sort_unstable();
    let result = TestResult {
        kind,
        bytes_per_sec: ops * block as f64 / secs,
        iops: ops / secs,
        p50_us: Some(percentile(&latencies_us, 50.0)),
        p99_us: Some(percentile(&latencies_us, 99.0)),
    };
    emit(BenchEvent::TestDone(result.clone()));
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bench::{BenchConfig, BenchEvent, TestKind};

    #[test]
    fn tiny_bench_produces_all_results_and_cleans_up() {
        let dir = std::env::temp_dir().join("rmon_bench_test");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = BenchConfig {
            target_dir: dir.clone(),
            size: 4 << 20,         // 4 MiB: fast but multiple blocks
            secs_per_rand_test: 0, // engine clamps to at least ~200ms of ops
            device: None,
        };
        let mut events = Vec::new();
        run(&cfg, &mut |e| events.push(e));

        let finished = events.iter().find_map(|e| match e {
            BenchEvent::Finished(r) => Some(r.clone()),
            _ => None,
        });
        let report = finished.expect("bench must finish");
        assert_eq!(report.results.len(), 4);
        let kinds: Vec<TestKind> = report.results.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TestKind::SeqWrite,
                TestKind::SeqRead,
                TestKind::RandRead,
                TestKind::RandWrite
            ]
        );
        for r in &report.results {
            assert!(r.bytes_per_sec > 0.0, "{:?} rate must be positive", r.kind);
            assert!(r.iops > 0.0);
        }
        // random tests carry percentiles, sequential do not
        assert!(report.results[2].p50_us.is_some() && report.results[2].p99_us.is_some());
        assert!(report.results[0].p50_us.is_none());
        // progress was emitted
        assert!(
            events
                .iter()
                .any(|e| matches!(e, BenchEvent::Progress { .. }))
        );
        // the bench file is gone
        assert!(
            std::fs::read_dir(&dir).unwrap().next().is_none(),
            "bench must remove its temp file"
        );
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn device_size_probe_on_file() {
        let path = std::env::temp_dir().join(format!("rmon_probe_test_{}", std::process::id()));
        std::fs::write(&path, vec![0u8; 1 << 20]).unwrap();
        let file = File::open(&path).unwrap();
        assert_eq!(probe_size(&file).unwrap(), 1 << 20);
        drop(file);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn device_bench_runs_read_only_on_file() {
        // a 4 MiB regular file stands in for the device; the engine must never
        // open it with write access, so its content survives the run
        let path = std::env::temp_dir().join(format!("rmon_devbench_test_{}", std::process::id()));
        let mut content = vec![0u8; 4 << 20];
        content[0] = 0xAB;
        std::fs::write(&path, &content).unwrap();
        let mtime_before = std::fs::metadata(&path).unwrap().modified().unwrap();

        let cfg = BenchConfig {
            device: Some(path.clone()),
            secs_per_rand_test: 0,
            ..Default::default()
        };
        let mut events = Vec::new();
        run(&cfg, &mut |e| events.push(e));

        let done_kinds: Vec<TestKind> = events
            .iter()
            .filter_map(|e| match e {
                BenchEvent::TestDone(r) => Some(r.kind),
                _ => None,
            })
            .collect();
        assert_eq!(done_kinds, vec![TestKind::SeqRead, TestKind::RandRead]);
        let finished = events
            .iter()
            .filter(|e| matches!(e, BenchEvent::Finished(_)))
            .count();
        assert_eq!(finished, 1, "exactly one Finished event");

        // read-only proof: content and mtime untouched
        let after = std::fs::read(&path).unwrap();
        assert_eq!(after[0], 0xAB, "byte 0 must be unchanged");
        assert_eq!(after.len(), 4 << 20);
        let mtime_after = std::fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(mtime_before, mtime_after, "mtime must be unchanged");
        let _ = std::fs::remove_file(&path);
    }
}
