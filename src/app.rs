use std::collections::{HashMap, VecDeque};

use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent};

use crate::bench::{BenchEvent, TestKind, TestResult};
use crate::collect::{MemSnapshot, MountInfo, Snapshot, cpu_percent};
use crate::smart::SmartInfo;

pub const HISTORY: usize = 300;

pub enum AppEvent {
    Snapshot(Box<Snapshot>),
    Key(KeyEvent),
    CollectError(String),
    Bench(BenchEvent),
    Smart(Vec<SmartInfo>),
    /// handled in the run loop, which knows the frame size; never reaches on_event
    Mouse(MouseEvent),
    /// the input thread lost the tty; exit instead of running headless forever
    Quit,
}

#[derive(Default)]
pub struct BenchState {
    /// (test, fraction done, bytes/sec) while a test runs
    pub running: Option<(TestKind, f64, f64)>,
    pub results: Vec<TestResult>,
    pub error: Option<String>,
    /// Finished report's direct-io flag; false means the page cache was in play
    pub direct: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct ProcRow {
    pub pid: i32,
    pub name: String,
    /// 100.0 = one full core
    pub cpu_pct: f64,
    pub rss: u64,
    pub io_bps: Option<f64>,
}

#[derive(Debug, Clone)]
pub struct DiskRow {
    pub name: String,
    pub read_bps: f64,
    pub write_bps: f64,
    pub iops: f64,
    pub util_pct: Option<f64>,
    pub queue: Option<f64>,
    pub lat_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    Cpu,
    Mem,
    Io,
}

/// one row in the bench target prompt
#[derive(Debug, Clone)]
pub struct BenchTarget {
    pub path: std::path::PathBuf,
    /// free bytes for mount rows; None for the temp dir entry
    pub available: Option<u64>,
}

/// modal prompt opened by `b`: pick which filesystem to benchmark
#[derive(Debug, Clone)]
pub struct BenchPicker {
    pub entries: Vec<BenchTarget>,
    pub selected: usize,
}

#[derive(Default)]
pub struct App {
    pub quit: bool,
    pub cpu_history: VecDeque<f64>,
    pub core_percents: Vec<f64>,
    pub mem: MemSnapshot,
    pub net_rx: VecDeque<f64>,
    pub net_tx: VecDeque<f64>,
    pub procs: Vec<ProcRow>,
    pub disk_io: VecDeque<f64>,
    pub disks: Vec<DiskRow>,
    pub mounts: Vec<MountInfo>,
    pub selected: usize,
    pub sort: SortBy,
    pub status: Option<String>,
    pub bench: Option<BenchState>,
    /// set by the picker; the run loop takes it and spawns the bench thread
    pub bench_target: Option<std::path::PathBuf>,
    pub picker: Option<BenchPicker>,
    pub smart: Vec<SmartInfo>,
    pub cpu_name: Option<String>,
    pub cpu_temp_c: Option<f64>,
    pub gpu_name: Option<String>,
    pub gpu_util_pct: Option<f64>,
    prev: Option<Snapshot>,
}

fn push_capped(hist: &mut VecDeque<f64>, v: f64) {
    hist.push_back(v);
    if hist.len() > HISTORY {
        hist.pop_front();
    }
}

impl App {
    pub fn on_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Key(k) => self.on_key(k),
            AppEvent::Snapshot(s) => self.apply(*s),
            AppEvent::CollectError(e) => self.status = Some(e),
            AppEvent::Smart(v) => self.smart = v,
            AppEvent::Mouse(_) => {} // run loop consumes these before on_event
            AppEvent::Quit => self.quit = true,
            AppEvent::Bench(ev) => {
                let st = self.bench.get_or_insert_with(BenchState::default);
                match ev {
                    BenchEvent::Progress {
                        kind,
                        frac,
                        bytes_per_sec,
                    } => {
                        st.running = Some((kind, frac, bytes_per_sec));
                    }
                    BenchEvent::TestDone(r) => st.results.push(r),
                    BenchEvent::Finished(r) => {
                        st.running = None;
                        st.direct = Some(r.direct);
                    }
                    BenchEvent::Error(e) => {
                        st.running = None;
                        st.error = Some(e);
                    }
                }
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent) {
        // raw mode turns ctrl+c into a plain key event; honor it from anywhere
        if k.code == KeyCode::Char('c')
            && k.modifiers
                .contains(ratatui::crossterm::event::KeyModifiers::CONTROL)
        {
            self.quit = true;
            return;
        }
        // the picker is modal: it swallows every key until it closes
        if let Some(p) = &mut self.picker {
            match k.code {
                KeyCode::Up => p.selected = p.selected.saturating_sub(1),
                KeyCode::Down => {
                    p.selected = (p.selected + 1).min(p.entries.len().saturating_sub(1));
                }
                KeyCode::Enter => {
                    let target = p.entries[p.selected].path.clone();
                    self.picker = None;
                    self.bench = Some(BenchState::default());
                    self.bench_target = Some(target);
                }
                KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('b') => self.picker = None,
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.procs.len().saturating_sub(1));
            }
            KeyCode::Char('c') => {
                self.sort = SortBy::Cpu;
                self.sort_procs();
            }
            KeyCode::Char('m') => {
                self.sort = SortBy::Mem;
                self.sort_procs();
            }
            KeyCode::Char('i') => {
                self.sort = SortBy::Io;
                self.sort_procs();
            }
            KeyCode::Char('b') => {
                let running = self
                    .bench
                    .as_ref()
                    .is_some_and(|b| b.error.is_none() && b.results.len() < 4);
                if !running {
                    self.open_bench_picker();
                }
            }
            _ => {}
        }
    }

    /// mounts by free space, biggest first, with the temp dir as the last resort
    fn open_bench_picker(&mut self) {
        let mut entries: Vec<BenchTarget> = self
            .mounts
            .iter()
            .map(|m| BenchTarget {
                path: std::path::PathBuf::from(&m.mount_point),
                available: Some(m.available),
            })
            .collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.available));
        entries.push(BenchTarget {
            path: std::env::temp_dir(),
            available: None,
        });
        self.picker = Some(BenchPicker {
            entries,
            selected: 0,
        });
    }

    fn apply(&mut self, s: Snapshot) {
        if let Some(prev) = &self.prev {
            let dt = s.taken.duration_since(prev.taken).as_secs_f64().max(1e-3);

            push_capped(
                &mut self.cpu_history,
                cpu_percent(prev.cpu.total, s.cpu.total),
            );
            self.core_percents = s
                .cpu
                .per_core
                .iter()
                .zip(&prev.cpu.per_core)
                .map(|(c, p)| cpu_percent(*p, *c))
                .collect();

            push_capped(
                &mut self.net_rx,
                s.net.rx_bytes.saturating_sub(prev.net.rx_bytes) as f64 / dt,
            );
            push_capped(
                &mut self.net_tx,
                s.net.tx_bytes.saturating_sub(prev.net.tx_bytes) as f64 / dt,
            );

            let prev_by_pid: HashMap<i32, &crate::collect::ProcessInfo> =
                prev.procs.iter().map(|p| (p.pid, p)).collect();
            self.procs = s
                .procs
                .iter()
                .map(|p| {
                    let prev_p = prev_by_pid.get(&p.pid);
                    ProcRow {
                        pid: p.pid,
                        name: p.name.clone(),
                        // a pid unseen last tick gets 0 rather than its lifetime total
                        cpu_pct: prev_p
                            .map(|q| p.cpu_ns.saturating_sub(q.cpu_ns) as f64 / (dt * 1e7))
                            .unwrap_or(0.0),
                        rss: p.rss,
                        io_bps: prev_p.and_then(|q| {
                            match (p.disk_read, q.disk_read, p.disk_written, q.disk_written) {
                                (Some(cr), Some(pr), Some(cw), Some(pw)) => Some(
                                    (cr.saturating_sub(pr) + cw.saturating_sub(pw)) as f64 / dt,
                                ),
                                _ => None,
                            }
                        }),
                    }
                })
                .collect();

            let prev_disks: HashMap<&str, &crate::collect::DiskStats> =
                prev.disks.iter().map(|d| (d.name.as_str(), d)).collect();
            let dt_ns = dt * 1e9;
            let mut total_bps = 0.0;
            self.disks = s
                .disks
                .iter()
                .map(|d| {
                    let Some(p) = prev_disks.get(d.name.as_str()) else {
                        // first sighting: no rates yet
                        return DiskRow {
                            name: d.name.clone(),
                            read_bps: 0.0,
                            write_bps: 0.0,
                            iops: 0.0,
                            util_pct: None,
                            queue: None,
                            lat_ms: None,
                        };
                    };
                    let read_bps = d.read_bytes.saturating_sub(p.read_bytes) as f64 / dt;
                    let write_bps = d.written_bytes.saturating_sub(p.written_bytes) as f64 / dt;
                    let d_ops = (d.read_ops.saturating_sub(p.read_ops)
                        + d.write_ops.saturating_sub(p.write_ops))
                        as f64;
                    // synthetic volumes (macos apfs) mirror the physical disk's
                    // traffic but lack time counters; skip them so the aggregate
                    // graph does not double-count
                    if d.io_time_ns.is_some() {
                        total_bps += read_bps + write_bps;
                    }
                    let delta = |curr: Option<u64>, prev: Option<u64>| match (curr, prev) {
                        (Some(c), Some(p)) => Some(c.saturating_sub(p)),
                        _ => None,
                    };
                    let busy = delta(d.busy_time_ns, p.busy_time_ns);
                    let io = delta(d.io_time_ns, p.io_time_ns);
                    let weighted = delta(d.weighted_ns, p.weighted_ns);
                    DiskRow {
                        name: d.name.clone(),
                        read_bps,
                        write_bps,
                        iops: d_ops / dt,
                        util_pct: busy.map(|b| (b as f64 * 100.0 / dt_ns).min(100.0)),
                        queue: weighted.map(|w| w as f64 / dt_ns),
                        lat_ms: io.and_then(|t| (d_ops > 0.0).then(|| t as f64 / 1e6 / d_ops)),
                    }
                })
                .collect();
            push_capped(&mut self.disk_io, total_bps);
            self.sort_procs();
            self.selected = self.selected.min(self.procs.len().saturating_sub(1));
        }
        self.mem = s.mem;
        self.mounts = s.mounts.clone();
        self.cpu_name = s.cpu_name.clone();
        self.cpu_temp_c = s.cpu_temp_c;
        self.gpu_name = s.gpu_name.clone();
        self.gpu_util_pct = s.gpu_util_pct;
        self.status = None;
        self.prev = Some(s);
    }

    fn sort_procs(&mut self) {
        match self.sort {
            SortBy::Cpu => self
                .procs
                .sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct).then(b.rss.cmp(&a.rss))),
            SortBy::Mem => self.procs.sort_by(|a, b| b.rss.cmp(&a.rss)),
            SortBy::Io => self.procs.sort_by(|a, b| {
                let key = |p: &ProcRow| p.io_bps.unwrap_or(f64::NEG_INFINITY);
                key(b).total_cmp(&key(a)).then(b.rss.cmp(&a.rss))
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{
        CpuSnapshot, CpuTimes, DiskStats, MountInfo, NetSnapshot, ProcessInfo, Snapshot,
    };
    use ratatui::crossterm::event::{KeyCode, KeyEvent};
    use std::time::{Duration, Instant};

    fn snap(busy: u64, idle: u64) -> Box<Snapshot> {
        Box::new(Snapshot {
            cpu: CpuSnapshot {
                total: CpuTimes { busy, idle },
                per_core: vec![CpuTimes { busy, idle }],
            },
            ..Default::default()
        })
    }

    // two snapshots exactly 1s apart with controlled net/proc counters
    fn pair() -> (Box<Snapshot>, Box<Snapshot>) {
        let base = Instant::now();
        let mut a = snap(100, 900);
        a.taken = base;
        a.net = NetSnapshot {
            rx_bytes: 1_000,
            tx_bytes: 500,
        };
        a.procs = vec![ProcessInfo {
            pid: 1,
            name: "alpha".into(),
            cpu_ns: 0,
            rss: 100,
            disk_read: Some(1 << 20),
            disk_written: Some(0),
        }];
        let mut b = snap(150, 950);
        b.taken = base + Duration::from_secs(1);
        b.net = NetSnapshot {
            rx_bytes: 101_000,
            tx_bytes: 50_500,
        };
        b.procs = vec![
            ProcessInfo {
                pid: 1,
                name: "alpha".into(),
                cpu_ns: 500_000_000,
                rss: 100,
                disk_read: Some(3 << 20),
                disk_written: Some(1 << 20),
            },
            ProcessInfo {
                pid: 2,
                name: "beta".into(),
                cpu_ns: 9_999,
                rss: 9_000,
                disk_read: None,
                disk_written: None,
            },
        ];
        (a, b)
    }

    fn disk(name: &str, rb: u64, wb: u64, ops: u64, busy_ns: Option<u64>) -> DiskStats {
        DiskStats {
            name: name.into(),
            read_bytes: rb,
            written_bytes: wb,
            read_ops: ops,
            write_ops: ops,
            busy_time_ns: busy_ns,
            io_time_ns: busy_ns,
            weighted_ns: busy_ns.map(|b| b * 2),
        }
    }

    #[test]
    fn q_quits() {
        let mut app = App::default();
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('q'))));
        assert!(app.quit);
    }

    #[test]
    fn first_snapshot_yields_no_history() {
        let mut app = App::default();
        app.on_event(AppEvent::Snapshot(snap(100, 900)));
        assert!(app.cpu_history.is_empty());
        assert!(app.net_rx.is_empty());
    }

    #[test]
    fn second_snapshot_yields_percent() {
        let mut app = App::default();
        app.on_event(AppEvent::Snapshot(snap(100, 900)));
        app.on_event(AppEvent::Snapshot(snap(150, 950)));
        assert_eq!(app.cpu_history.len(), 1);
        assert!((app.cpu_history[0] - 50.0).abs() < 1e-9);
        assert_eq!(app.core_percents.len(), 1);
    }

    #[test]
    fn history_is_capped() {
        let mut app = App::default();
        for i in 0..(HISTORY as u64 + 10) {
            app.on_event(AppEvent::Snapshot(snap(i * 10, i * 10)));
        }
        assert_eq!(app.cpu_history.len(), HISTORY);
    }

    #[test]
    fn net_rates_from_deltas() {
        let mut app = App::default();
        let (a, b) = pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        // 100_000 rx and 50_000 tx bytes over 1s
        assert!((app.net_rx[0] - 100_000.0).abs() < 1_000.0);
        assert!((app.net_tx[0] - 50_000.0).abs() < 500.0);
    }

    #[test]
    fn proc_cpu_pct_and_default_sort() {
        let mut app = App::default();
        let (a, b) = pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        // alpha burned 0.5s cpu in 1s -> ~50%; new pid 2 has no prev -> 0
        assert_eq!(app.procs[0].name, "alpha");
        assert!((app.procs[0].cpu_pct - 50.0).abs() < 1.0);
        assert!((app.procs[1].cpu_pct - 0.0).abs() < 1e-9);
    }

    #[test]
    fn sort_by_mem_and_selection_clamps() {
        let mut app = App::default();
        let (a, b) = pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('m'))));
        assert_eq!(app.procs[0].name, "beta"); // 9_000 rss beats 100
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Down)));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Down)));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Down)));
        assert_eq!(app.selected, 1); // clamped to last row
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Up)));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Up)));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn disk_rates_from_deltas() {
        let mut app = App::default();
        let (mut a, mut b) = pair();
        a.disks = vec![disk("d0", 0, 0, 0, Some(0))];
        // +10 MiB read, +5 MiB written, +100+100 ops, +500ms busy over 1s
        b.disks = vec![disk("d0", 10 << 20, 5 << 20, 100, Some(500_000_000))];
        b.mounts = vec![MountInfo {
            mount_point: "/".into(),
            total: 100,
            available: 40,
        }];
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));

        let d = &app.disks[0];
        assert!((d.read_bps - (10 << 20) as f64).abs() < 20_000.0);
        assert!((d.write_bps - (5 << 20) as f64).abs() < 10_000.0);
        assert!((d.iops - 200.0).abs() < 2.0);
        assert!((d.util_pct.unwrap() - 50.0).abs() < 1.0);
        assert!((d.queue.unwrap() - 1.0).abs() < 0.1); // weighted 1s over 1s
        assert!((d.lat_ms.unwrap() - 2.5).abs() < 0.1); // 500ms / 200 ops
        assert!((app.disk_io[0] - (15 << 20) as f64).abs() < 30_000.0);
        assert_eq!(app.mounts[0].mount_point, "/");
    }

    #[test]
    fn disk_missing_counters_stay_none() {
        let mut app = App::default();
        let (mut a, mut b) = pair();
        a.disks = vec![disk("d0", 0, 0, 0, None)];
        b.disks = vec![disk("d0", 1024, 0, 1, None)];
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        let d = &app.disks[0];
        assert!(d.util_pct.is_none() && d.lat_ms.is_none());
        // macos-style: weighted follows busy in this helper, so also None -> no queue
        assert!(d.queue.is_none());
        // synthetic disks (no time counters) stay out of the aggregate graph
        assert_eq!(app.disk_io[0], 0.0);
    }

    #[test]
    fn proc_io_rates_from_deltas() {
        let mut app = App::default();
        let (a, b) = pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        // alpha: (3MiB-1MiB) + (1MiB-0) = 3MiB over 1s
        let alpha = app.procs.iter().find(|p| p.pid == 1).unwrap();
        assert!((alpha.io_bps.unwrap() - (3 << 20) as f64).abs() < 10_000.0);
        // beta had no counters -> None, not 0
        let beta = app.procs.iter().find(|p| p.pid == 2).unwrap();
        assert!(beta.io_bps.is_none());
    }

    #[test]
    fn sort_by_io_puts_none_last() {
        let mut app = App::default();
        let (a, b) = pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('i'))));
        assert_eq!(app.sort, SortBy::Io);
        assert_eq!(app.procs[0].pid, 1);
        assert_eq!(app.procs[1].pid, 2);
    }

    fn mount(point: &str, available: u64) -> MountInfo {
        MountInfo {
            mount_point: point.into(),
            total: available * 2,
            available,
        }
    }

    #[test]
    fn b_opens_picker_sorted_by_free_space() {
        let mut app = App {
            mounts: vec![mount("/small", 10), mount("/big", 1000)],
            ..Default::default()
        };
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('b'))));
        let p = app.picker.as_ref().expect("picker opens");
        assert_eq!(p.entries[0].path, std::path::PathBuf::from("/big"));
        assert_eq!(p.entries[1].path, std::path::PathBuf::from("/small"));
        // temp dir is always the last resort entry
        assert_eq!(p.entries.last().unwrap().path, std::env::temp_dir());
        assert_eq!(p.selected, 0);
        assert!(app.bench_target.is_none(), "prompt, do not start");
    }

    #[test]
    fn picker_enter_starts_bench_at_selection() {
        let mut app = App {
            mounts: vec![mount("/big", 1000)],
            ..Default::default()
        };
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('b'))));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Down)));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Enter)));
        assert!(app.picker.is_none());
        assert_eq!(app.bench_target, Some(std::env::temp_dir()));
        assert!(app.bench.is_some(), "bench state resets on start");
    }

    #[test]
    fn picker_selection_clamps() {
        let mut app = App {
            mounts: vec![mount("/a", 1)],
            ..Default::default()
        };
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('b'))));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Up)));
        assert_eq!(app.picker.as_ref().unwrap().selected, 0);
        for _ in 0..9 {
            app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Down)));
        }
        assert_eq!(app.picker.as_ref().unwrap().selected, 1); // 2 entries
    }

    #[test]
    fn picker_escape_closes_without_bench_and_q_does_not_quit() {
        let mut app = App::default();
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('b'))));
        assert!(app.picker.is_some());
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('q'))));
        assert!(app.picker.is_none());
        assert!(!app.quit, "q closes the picker, not the app");
        assert!(app.bench_target.is_none());
        // esc works the same way
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('b'))));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Esc)));
        assert!(app.picker.is_none() && !app.quit);
    }

    #[test]
    fn b_ignored_while_bench_runs() {
        let mut app = App {
            bench: Some(BenchState::default()), // running
            ..Default::default()
        };
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('b'))));
        assert!(app.picker.is_none(), "no picker while a bench runs");
    }

    #[test]
    fn quit_event_exits_even_mid_picker() {
        let mut app = App::default();
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('b'))));
        app.on_event(AppEvent::Quit);
        assert!(app.quit, "tty loss must exit despite the modal picker");
    }

    #[test]
    fn ctrl_c_quits_even_mid_picker() {
        use ratatui::crossterm::event::KeyModifiers;
        let mut app = App::default();
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('b'))));
        assert!(app.picker.is_some());
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        app.on_event(AppEvent::Key(ctrl_c));
        assert!(app.quit);
        // plain c still sorts, does not quit
        let mut app2 = App::default();
        app2.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('c'))));
        assert!(!app2.quit);
        assert_eq!(app2.sort, SortBy::Cpu);
    }

    #[test]
    fn bench_events_update_state() {
        use crate::bench::{BenchEvent, BenchReport, TestKind, TestResult};
        let mut app = App {
            bench: Some(BenchState::default()),
            ..Default::default()
        };
        app.on_event(AppEvent::Bench(BenchEvent::Progress {
            kind: TestKind::SeqWrite,
            frac: 0.5,
            bytes_per_sec: 2e9,
        }));
        let st = app.bench.as_ref().unwrap();
        assert_eq!(st.running.unwrap().0, TestKind::SeqWrite);

        let result = TestResult {
            kind: TestKind::SeqWrite,
            bytes_per_sec: 2e9,
            iops: 2000.0,
            p50_us: None,
            p99_us: None,
        };
        app.on_event(AppEvent::Bench(BenchEvent::TestDone(result.clone())));
        assert_eq!(app.bench.as_ref().unwrap().results.len(), 1);

        app.on_event(AppEvent::Bench(BenchEvent::Finished(BenchReport {
            ts: 0,
            target: "/".into(),
            size: 1,
            direct: true,
            results: vec![result],
        })));
        let st = app.bench.as_ref().unwrap();
        assert!(st.running.is_none(), "finished clears the running marker");
    }

    #[test]
    fn smart_event_replaces_list() {
        let info = |device: &str| crate::smart::SmartInfo {
            device: device.into(),
            model: None,
            healthy: Some(true),
            temp_c: None,
            wear_pct: None,
            power_on_hours: None,
        };
        let mut app = App::default();
        app.on_event(AppEvent::Smart(vec![info("disk0")]));
        assert_eq!(app.smart.len(), 1);
        assert_eq!(app.smart[0].device, "disk0");

        app.on_event(AppEvent::Smart(vec![info("disk1"), info("disk2")]));
        assert_eq!(app.smart.len(), 2, "second event replaces, not appends");
        assert_eq!(app.smart[0].device, "disk1");
    }
}
