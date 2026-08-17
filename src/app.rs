use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, MouseEvent};

use crate::bench::{BenchEvent, TestKind, TestResult};
use crate::collect::{MemSnapshot, MountInfo, Snapshot, cpu_percent};
use crate::smart::SmartInfo;

// braille packs 2 samples per char column: 720 samples fill a 360-col
// terminal; 300 could never reach the left edge past 150 chars
pub const HISTORY: usize = 720;

/// a net/dsk row idle this long stops rendering unless `h` shows it
pub const IDLE_HIDE_SECS: f64 = 5.0;

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
    /// parent pid, 0 = none/unknown; thread rows carry their owner's pid
    pub ppid: i32,
    pub name: String,
    /// 100.0 = one full core
    pub cpu_pct: f64,
    pub rss: u64,
    pub io_bps: Option<f64>,
    /// Some = this row is a thread of `pid`, not a process
    pub tid: Option<u64>,
    /// pre-rendered tree rail ("├─ ", "│  └─ ", ...); empty for flat proc rows
    pub prefix: String,
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
    /// seconds since the device last moved a byte; drives the h toggle
    pub idle_secs: f64,
}

/// one physical interface with its live rx/tx rates
#[derive(Debug, Clone)]
pub struct NetRow {
    pub name: String,
    pub rx_bps: f64,
    pub tx_bps: f64,
    /// seconds since the interface last moved a byte; drives the h toggle
    pub idle_secs: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortBy {
    #[default]
    Cpu,
    Mem,
    Io,
    Name,
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

/// modal prompt opened by `k`: confirm before the signal goes out.
/// pid+name are captured at open time; the list resorts under us every snapshot
#[derive(Debug, Clone)]
pub struct KillPrompt {
    pub pid: i32,
    pub name: String,
}

#[derive(Default)]
pub struct App {
    pub quit: bool,
    pub cpu_history: VecDeque<f64>,
    pub core_percents: Vec<f64>,
    pub mem: MemSnapshot,
    pub net_rx: VecDeque<f64>,
    pub net_tx: VecDeque<f64>,
    /// what the proc panel shows: procs_all narrowed by the filter
    pub procs: Vec<ProcRow>,
    /// every process from the last snapshot; filter edits re-derive procs
    procs_all: Vec<ProcRow>,
    pub disk_io: VecDeque<f64>,
    pub disks: Vec<DiskRow>,
    pub mounts: Vec<MountInfo>,
    pub selected: usize,
    /// (pid, tid) under the highlight; the list resorts every snapshot, so
    /// the index alone would make the selection wander. tid None = process row
    pub selected_id: Option<(i32, Option<u64>)>,
    /// first row the proc panel shows; owned here so draw and mouse agree
    pub view_offset: usize,
    /// true while the mouse is dragging the proc scrollbar thumb
    pub drag_scroll: bool,
    /// `h` flips this: false (default) hides net/dsk rows idle > 5s
    pub show_idle: bool,
    /// net and dsk row viewports; draw clamps them and records the row
    /// capacity so the mouse maps against what was actually on screen
    pub net_offset: usize,
    pub dsk_offset: usize,
    pub net_rows_cap: usize,
    pub dsk_rows_cap: usize,
    /// scrollbar drags in flight on the net / dsk row lists
    pub drag_net: bool,
    pub drag_dsk: bool,
    pub sort: SortBy,
    pub status: Option<String>,
    pub bench: Option<BenchState>,
    /// set by the picker; the run loop takes it and spawns the bench thread
    pub bench_target: Option<std::path::PathBuf>,
    pub picker: Option<BenchPicker>,
    pub confirm_kill: Option<KillPrompt>,
    /// live substring filter for the proc list; empty = off
    pub filter: String,
    /// true while `f` captures keystrokes into the filter
    pub filter_edit: bool,
    /// `t`: interleave thread rows under their processes
    pub show_threads: bool,
    /// `e`: arrange procs as a ppid tree, btop style
    pub tree: bool,
    /// thread rows keyed by owning pid, rebuilt every snapshot; refilter
    /// weaves them into `procs` when show_threads is on
    threads_by_pid: HashMap<i32, Vec<ProcRow>>,
    pub smart: Vec<SmartInfo>,
    pub cpu_name: Option<String>,
    pub cpu_temp_c: Option<f64>,
    pub core_temps_c: Vec<f64>,
    pub gpu_name: Option<String>,
    pub gpu_util_pct: Option<f64>,
    pub load_avg: Option<[f64; 3]>,
    pub uptime_secs: Option<u64>,
    pub net_ifaces: Vec<NetRow>,
    /// per-interface rate history keyed by name: (rx, tx), HISTORY-capped;
    /// vanished interfaces are pruned so hotplug churn cannot leak
    pub net_hist: HashMap<String, (VecDeque<f64>, VecDeque<f64>)>,
    /// last instant each interface / disk moved a byte, for the h toggle
    net_last_active: HashMap<String, Instant>,
    disk_last_active: HashMap<String, Instant>,
    pub net_rx_total: u64,
    pub net_tx_total: u64,
    prev: Option<Snapshot>,
}

fn push_capped(hist: &mut VecDeque<f64>, v: f64) {
    hist.push_back(v);
    if hist.len() > HISTORY {
        hist.pop_front();
    }
}

/// seconds since `name` last moved a byte. never-active devices are born
/// past the grace period so they start hidden; active ones reset to zero
fn idle_secs(
    map: &mut HashMap<String, Instant>,
    name: &str,
    now: Instant,
    active_now: bool,
    ever_active: bool,
) -> f64 {
    let seen = map.entry(name.to_owned()).or_insert_with(|| {
        if ever_active {
            now
        } else {
            now.checked_sub(std::time::Duration::from_secs_f64(IDLE_HIDE_SECS + 1.0))
                .unwrap_or(now)
        }
    });
    if active_now {
        *seen = now;
    }
    now.duration_since(*seen).as_secs_f64()
}

/// one comparator shared by flat and tree mode so sibling order matches the list
fn cmp_rows(sort: SortBy, a: &ProcRow, b: &ProcRow) -> std::cmp::Ordering {
    match sort {
        SortBy::Cpu => b.cpu_pct.total_cmp(&a.cpu_pct).then(b.rss.cmp(&a.rss)),
        SortBy::Mem => b.rss.cmp(&a.rss),
        SortBy::Io => {
            let key = |p: &ProcRow| p.io_bps.unwrap_or(f64::NEG_INFINITY);
            key(b).total_cmp(&key(a)).then(b.rss.cmp(&a.rss))
        }
        // ascending, unlike the load sorts: a name is an identity, not a hotness
        SortBy::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
    }
}

/// threads only sort within their process; the tid tie-break keeps
/// equal threads from shuffling every tick
fn sort_threads(sort: SortBy, ts: &mut [ProcRow]) {
    match sort {
        SortBy::Name => {
            ts.sort_by(|a, b| (a.name.to_lowercase(), a.tid).cmp(&(b.name.to_lowercase(), b.tid)))
        }
        _ => ts.sort_by(|a, b| b.cpu_pct.total_cmp(&a.cpu_pct).then(a.tid.cmp(&b.tid))),
    }
}

/// DFS state for the ppid tree flatten; procs come in comparator-sorted,
/// so index order is sibling order everywhere
struct TreeWalk<'a> {
    procs: &'a [ProcRow],
    children: &'a HashMap<i32, Vec<usize>>,
    threads: &'a HashMap<i32, Vec<ProcRow>>,
    show_threads: bool,
    sort: SortBy,
    seen: HashSet<i32>,
    out: Vec<ProcRow>,
}

impl TreeWalk<'_> {
    /// emit idx with its rail+glyph, then its threads, then its child procs.
    /// the seen set breaks ppid cycles: a revisited pid is skipped
    fn walk(&mut self, idx: usize, rail: &str, glyph: &str) {
        let p = &self.procs[idx];
        if !self.seen.insert(p.pid) {
            return;
        }
        let mut row = p.clone();
        row.prefix = format!("{rail}{glyph}");
        self.out.push(row);
        // what the kids see: │ keeps the rail alive past a ├─, blanks past a └─
        let rail = match glyph {
            "" => rail.to_string(),
            "├─ " => format!("{rail}│  "),
            _ => format!("{rail}   "),
        };
        let pid = self.procs[idx].pid;
        // already-seen kids are cycle re-entries; drop them before picking └─
        let kids: Vec<usize> = self.children.get(&pid).map_or_else(Vec::new, |v| {
            v.iter()
                .copied()
                .filter(|&c| !self.seen.contains(&self.procs[c].pid))
                .collect()
        });
        let mut ts = if self.show_threads {
            self.threads.get(&pid).cloned().unwrap_or_default()
        } else {
            Vec::new()
        };
        sort_threads(self.sort, &mut ts);
        let n_threads = ts.len();
        let total = n_threads + kids.len();
        // threads first, one level deeper, then the child processes
        for (j, mut t) in ts.into_iter().enumerate() {
            t.prefix = format!("{rail}{}", if j + 1 == total { "└─ " } else { "├─ " });
            self.out.push(t);
        }
        for (j, &k) in kids.iter().enumerate() {
            let glyph = if n_threads + j + 1 == total {
                "└─ "
            } else {
                "├─ "
            };
            self.walk(k, &rail, glyph);
        }
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
        // so is the kill prompt: y/enter sends the signal, anything else backs out
        if let Some(kp) = &self.confirm_kill {
            match k.code {
                KeyCode::Enter | KeyCode::Char('y') => {
                    let kp = kp.clone();
                    self.confirm_kill = None;
                    self.kill_proc(&kp);
                }
                _ => self.confirm_kill = None,
            }
            return;
        }
        // filter input captures text; enter keeps the filter, esc drops it
        if self.filter_edit {
            match k.code {
                KeyCode::Enter => self.filter_edit = false,
                KeyCode::Esc => {
                    self.filter_edit = false;
                    self.filter.clear();
                    self.refilter();
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.refilter();
                }
                KeyCode::Char(c) => {
                    self.filter.push(c);
                    self.refilter();
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            // esc peels the filter first; a second esc quits
            KeyCode::Esc => {
                if self.filter.is_empty() {
                    self.quit = true;
                } else {
                    self.filter.clear();
                    self.refilter();
                }
            }
            KeyCode::Char('f') | KeyCode::Char('/') => self.filter_edit = true,
            KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::Down => {
                self.select((self.selected + 1).min(self.procs.len().saturating_sub(1)));
            }
            KeyCode::Char('c') => {
                self.sort = SortBy::Cpu;
                self.sort_procs();
                self.reanchor();
            }
            KeyCode::Char('m') => {
                self.sort = SortBy::Mem;
                self.sort_procs();
                self.reanchor();
            }
            KeyCode::Char('i') => {
                self.sort = SortBy::Io;
                self.sort_procs();
                self.reanchor();
            }
            KeyCode::Char('n') => {
                self.sort = SortBy::Name;
                self.sort_procs();
                self.reanchor();
            }
            KeyCode::Char('t') => {
                self.show_threads = !self.show_threads;
                self.refilter();
            }
            KeyCode::Char('e') => {
                self.tree = !self.tree;
                self.refilter();
            }
            KeyCode::Char('h') => self.show_idle = !self.show_idle,
            KeyCode::Char('b') => {
                let running = self
                    .bench
                    .as_ref()
                    .is_some_and(|b| b.error.is_none() && b.results.len() < 4);
                if !running {
                    self.open_bench_picker();
                }
            }
            KeyCode::Char('k') => {
                if let Some(p) = self.procs.get(self.selected) {
                    // a thread row targets its owning process; you can't
                    // SIGTERM a single thread anyway
                    let name = if p.tid.is_some() {
                        self.procs_all
                            .iter()
                            .find(|q| q.pid == p.pid)
                            .map_or_else(|| p.name.clone(), |q| q.name.clone())
                    } else {
                        p.name.clone()
                    };
                    self.confirm_kill = Some(KillPrompt { pid: p.pid, name });
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

    /// SIGTERM, not SIGKILL: give the process a chance to clean up.
    /// success is silent; the row vanishes on the next snapshot
    fn kill_proc(&mut self, kp: &KillPrompt) {
        // SAFETY: plain syscall, no memory involved
        if unsafe { libc::kill(kp.pid, libc::SIGTERM) } != 0 {
            let err = std::io::Error::last_os_error();
            self.status = Some(format!("kill {} ({}) failed: {}", kp.name, kp.pid, err));
        }
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

            let prev_ifaces: HashMap<&str, &crate::collect::NetIface> = prev
                .net
                .interfaces
                .iter()
                .map(|i| (i.name.as_str(), i))
                .collect();
            // every interface earns a row (h hides the idle ones), busiest
            // lifetime traffic first so the live nic tops the list
            let mut lively: Vec<&crate::collect::NetIface> = s.net.interfaces.iter().collect();
            lively.sort_by_key(|i| std::cmp::Reverse(i.rx_bytes + i.tx_bytes));
            self.net_ifaces = lively
                .iter()
                .map(|i| {
                    let p = prev_ifaces.get(i.name.as_str());
                    let rx_bps = p
                        .map(|q| i.rx_bytes.saturating_sub(q.rx_bytes) as f64 / dt)
                        .unwrap_or(0.0);
                    let tx_bps = p
                        .map(|q| i.tx_bytes.saturating_sub(q.tx_bytes) as f64 / dt)
                        .unwrap_or(0.0);
                    let (rx_h, tx_h) = self.net_hist.entry(i.name.clone()).or_default();
                    push_capped(rx_h, rx_bps);
                    push_capped(tx_h, tx_bps);
                    let idle_secs = idle_secs(
                        &mut self.net_last_active,
                        &i.name,
                        s.taken,
                        rx_bps > 0.0 || tx_bps > 0.0,
                        i.rx_bytes + i.tx_bytes > 0,
                    );
                    NetRow {
                        name: i.name.clone(),
                        rx_bps,
                        tx_bps,
                        idle_secs,
                    }
                })
                .collect();
            self.net_hist
                .retain(|name, _| s.net.interfaces.iter().any(|i| &i.name == name));
            self.net_last_active
                .retain(|name, _| s.net.interfaces.iter().any(|i| &i.name == name));

            let prev_by_pid: HashMap<i32, &crate::collect::ProcessInfo> =
                prev.procs.iter().map(|p| (p.pid, p)).collect();
            self.procs_all = s
                .procs
                .iter()
                .map(|p| {
                    let prev_p = prev_by_pid.get(&p.pid);
                    ProcRow {
                        pid: p.pid,
                        ppid: p.ppid,
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
                        tid: None,
                        prefix: String::new(),
                    }
                })
                .collect();

            self.threads_by_pid.clear();
            if s.procs.iter().any(|p| !p.threads.is_empty()) {
                // same delta trick as procs, keyed by (pid, tid) since tids
                // are only unique within a process on linux
                let prev_ns: HashMap<(i32, u64), u64> = prev
                    .procs
                    .iter()
                    .flat_map(|p| p.threads.iter().map(|t| ((p.pid, t.tid), t.cpu_ns)))
                    .collect();
                for p in &s.procs {
                    if p.threads.is_empty() {
                        continue;
                    }
                    let rows = p
                        .threads
                        .iter()
                        .map(|t| ProcRow {
                            pid: p.pid,
                            // a thread belongs to its process, tree-wise
                            ppid: p.pid,
                            // unnamed threads still need a label
                            name: if t.name.is_empty() {
                                format!("tid {}", t.tid)
                            } else {
                                t.name.clone()
                            },
                            cpu_pct: prev_ns
                                .get(&(p.pid, t.tid))
                                .map(|q| t.cpu_ns.saturating_sub(*q) as f64 / (dt * 1e7))
                                .unwrap_or(0.0),
                            rss: 0,
                            io_bps: None,
                            tid: Some(t.tid),
                            prefix: String::new(),
                        })
                        .collect();
                    self.threads_by_pid.insert(p.pid, rows);
                }
            }

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
                            idle_secs: idle_secs(
                                &mut self.disk_last_active,
                                &d.name,
                                s.taken,
                                false,
                                d.read_bytes + d.written_bytes > 0,
                            ),
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
                        idle_secs: idle_secs(
                            &mut self.disk_last_active,
                            &d.name,
                            s.taken,
                            read_bps > 0.0 || write_bps > 0.0 || d_ops > 0.0,
                            d.read_bytes + d.written_bytes > 0,
                        ),
                    }
                })
                .collect();
            self.disk_last_active
                .retain(|name, _| s.disks.iter().any(|d| &d.name == name));
            push_capped(&mut self.disk_io, total_bps);
            self.refilter();
        }
        self.mem = s.mem;
        self.mounts = s.mounts.clone();
        self.cpu_name = s.cpu_name.clone();
        self.cpu_temp_c = s.cpu_temp_c;
        self.core_temps_c = s.core_temps_c.clone();
        self.gpu_name = s.gpu_name.clone();
        self.gpu_util_pct = s.gpu_util_pct;
        self.load_avg = s.load_avg;
        self.uptime_secs = s.uptime_secs;
        self.net_rx_total = s.net.rx_bytes;
        self.net_tx_total = s.net.tx_bytes;
        self.status = None;
        self.prev = Some(s);
    }

    fn sort_procs(&mut self) {
        // thread rows ride under their parent, never sort against processes:
        // strip them, sort the processes, weave the threads back in
        self.procs.retain(|p| p.tid.is_none());
        let sort = self.sort;
        self.procs.sort_by(|a, b| cmp_rows(sort, a, b));
        // a live filter punches holes in the hierarchy; stay flat until it clears
        if self.tree && self.filter.is_empty() {
            self.build_tree();
            return;
        }
        if !self.show_threads {
            return;
        }
        let parents = std::mem::take(&mut self.procs);
        let mut out = Vec::with_capacity(parents.len());
        for p in parents {
            let pid = p.pid;
            out.push(p);
            let Some(ts) = self.threads_by_pid.get(&pid) else {
                continue;
            };
            let mut ts = ts.clone();
            sort_threads(sort, &mut ts);
            // flat mode still rails the threads; the last one closes the box
            let n = ts.len();
            for (i, t) in ts.iter_mut().enumerate() {
                t.prefix = if i + 1 == n {
                    "└─ ".into()
                } else {
                    "├─ ".into()
                };
            }
            out.extend(ts);
        }
        self.procs = out;
    }

    /// ppid hierarchy flattened depth-first. roots: no parent, parent gone,
    /// or parent == self. cycle members never reach a root, so whatever the
    /// DFS missed gets swept in afterwards as extra roots — nothing vanishes
    fn build_tree(&mut self) {
        let procs = std::mem::take(&mut self.procs);
        let pids: HashSet<i32> = procs.iter().map(|p| p.pid).collect();
        let mut children: HashMap<i32, Vec<usize>> = HashMap::new();
        let mut roots = Vec::new();
        for (i, p) in procs.iter().enumerate() {
            if p.ppid <= 0 || p.ppid == p.pid || !pids.contains(&p.ppid) {
                roots.push(i);
            } else {
                children.entry(p.ppid).or_default().push(i);
            }
        }
        let mut w = TreeWalk {
            procs: &procs,
            children: &children,
            threads: &self.threads_by_pid,
            show_threads: self.show_threads,
            sort: self.sort,
            seen: HashSet::with_capacity(procs.len()),
            out: Vec::with_capacity(procs.len()),
        };
        for &r in &roots {
            w.walk(r, "", "");
        }
        // cycle members unreachable from any root; sweep them in as roots.
        // check seen fresh each step: one walk can absorb later stragglers
        for (i, p) in procs.iter().enumerate() {
            if !w.seen.contains(&p.pid) {
                w.walk(i, "", "");
            }
        }
        self.procs = w.out;
    }

    /// re-derive the visible list from procs_all, then sort and reanchor.
    /// matches on a case-insensitive name substring, or on the pid digits
    fn refilter(&mut self) {
        if self.filter.is_empty() {
            self.procs = self.procs_all.clone();
        } else {
            let needle = self.filter.to_lowercase();
            self.procs = self
                .procs_all
                .iter()
                .filter(|p| {
                    p.name.to_lowercase().contains(&needle) || p.pid.to_string().contains(&needle)
                })
                .cloned()
                .collect();
        }
        self.sort_procs();
        self.reanchor();
    }

    /// move the highlight and remember which row sits under it
    pub fn select(&mut self, idx: usize) {
        self.selected = idx;
        self.selected_id = self.procs.get(idx).map(|p| (p.pid, p.tid));
    }

    /// net rows the h toggle lets through, panel order preserved
    pub fn visible_net(&self) -> Vec<&NetRow> {
        self.net_ifaces
            .iter()
            .filter(|r| self.show_idle || r.idle_secs <= IDLE_HIDE_SECS)
            .collect()
    }

    /// disk rows the h toggle lets through, panel order preserved
    pub fn visible_disks(&self) -> Vec<&DiskRow> {
        self.disks
            .iter()
            .filter(|r| self.show_idle || r.idle_secs <= IDLE_HIDE_SECS)
            .collect()
    }

    /// first visible row; draw clamps it each frame and the mouse handler
    /// maps clicks through it, so clicks hit what was actually on screen
    pub fn scroll_viewport(&mut self, visible: usize) -> usize {
        let last = self.procs.len().saturating_sub(1);
        self.view_offset = self.view_offset.min(last);
        // follow the selection only when it walks off the window
        if self.selected < self.view_offset {
            self.view_offset = self.selected;
        } else if visible > 0 && self.selected >= self.view_offset + visible {
            self.view_offset = self.selected + 1 - visible;
        }
        self.view_offset
    }

    /// after a resort, chase the anchored row to its new index. a dead row
    /// keeps the old (clamped) spot and adopts whatever sits there now.
    /// a drag in flight wins over the anchor: the pointer sets the position
    fn reanchor(&mut self) {
        if self.drag_scroll {
            self.selected = self.selected.min(self.procs.len().saturating_sub(1));
            self.selected_id = self.procs.get(self.selected).map(|p| (p.pid, p.tid));
            return;
        }
        match self
            .selected_id
            .and_then(|(pid, tid)| self.procs.iter().position(|p| p.pid == pid && p.tid == tid))
        {
            Some(idx) => {
                // shift the viewport with the row so it stays glued to the
                // same screen line; the list slides underneath instead
                let delta = idx as isize - self.selected as isize;
                self.view_offset = (self.view_offset as isize + delta).max(0) as usize;
                self.selected = idx;
            }
            None => {
                self.selected = self.selected.min(self.procs.len().saturating_sub(1));
                self.selected_id = self.procs.get(self.selected).map(|p| (p.pid, p.tid));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::{
        CpuSnapshot, CpuTimes, DiskStats, MountInfo, NetSnapshot, ProcessInfo, Snapshot, ThreadInfo,
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
            interfaces: vec![crate::collect::NetIface {
                name: "eth0".into(),
                rx_bytes: 1_000,
                tx_bytes: 500,
            }],
        };
        a.procs = vec![ProcessInfo {
            pid: 1,
            ppid: 0,
            name: "alpha".into(),
            cpu_ns: 0,
            rss: 100,
            disk_read: Some(1 << 20),
            disk_written: Some(0),
            threads: Vec::new(),
        }];
        let mut b = snap(150, 950);
        b.taken = base + Duration::from_secs(1);
        b.net = NetSnapshot {
            rx_bytes: 101_000,
            tx_bytes: 50_500,
            interfaces: vec![crate::collect::NetIface {
                name: "eth0".into(),
                rx_bytes: 101_000,
                tx_bytes: 50_500,
            }],
        };
        b.procs = vec![
            ProcessInfo {
                pid: 1,
                ppid: 0,
                name: "alpha".into(),
                cpu_ns: 500_000_000,
                rss: 100,
                disk_read: Some(3 << 20),
                disk_written: Some(1 << 20),
                threads: Vec::new(),
            },
            ProcessInfo {
                pid: 2,
                ppid: 0,
                name: "beta".into(),
                cpu_ns: 9_999,
                rss: 9_000,
                disk_read: None,
                disk_written: None,
                threads: Vec::new(),
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

    fn row(pid: i32, name: &str) -> ProcRow {
        ProcRow {
            pid,
            ppid: 0,
            name: name.into(),
            cpu_pct: 0.0,
            rss: 0,
            io_bps: None,
            tid: None,
            prefix: String::new(),
        }
    }

    #[test]
    fn k_opens_kill_prompt_and_any_other_key_backs_out() {
        let mut app = App::default();
        // no procs -> no prompt
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('k'))));
        assert!(app.confirm_kill.is_none());

        app.procs = vec![row(11, "alpha"), row(22, "beta")];
        app.selected = 1;
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('k'))));
        let kp = app.confirm_kill.as_ref().expect("prompt opens");
        assert_eq!((kp.pid, kp.name.as_str()), (22, "beta"));

        // esc backs out without quitting; the prompt swallows the key
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Esc)));
        assert!(app.confirm_kill.is_none());
        assert!(!app.quit, "esc closes the prompt, not the app");

        // any non-confirm key backs out too, and does not reach the sort keys
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('k'))));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('m'))));
        assert!(app.confirm_kill.is_none());
        assert_eq!(app.sort, SortBy::Cpu, "swallowed key must not sort");
    }

    #[test]
    fn kill_confirm_signals_the_process() {
        use std::os::unix::process::ExitStatusExt;
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep");
        let mut app = App {
            procs: vec![row(child.id() as i32, "sleep")],
            ..App::default()
        };
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('k'))));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('y'))));
        assert!(app.confirm_kill.is_none());
        let status = child.wait().expect("child reaped");
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(app.status.is_none(), "success is silent");
    }

    #[test]
    fn kill_failure_sets_status() {
        // i32::MAX is far past any real pid range -> ESRCH, no signal sent
        let mut app = App {
            procs: vec![row(i32::MAX, "ghost")],
            ..App::default()
        };
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('k'))));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Enter)));
        let st = app.status.as_ref().expect("failure surfaces");
        assert!(st.contains("ghost") && st.contains("failed"), "{st}");
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
        // the per-interface row mirrors the aggregate for the single iface
        assert_eq!(app.net_ifaces.len(), 1);
        assert_eq!(app.net_ifaces[0].name, "eth0");
        assert!((app.net_ifaces[0].rx_bps - 100_000.0).abs() < 1_000.0);
        assert!((app.net_ifaces[0].tx_bps - 50_000.0).abs() < 500.0);
    }

    #[test]
    fn idle_rows_hide_after_grace_and_h_reveals() {
        let mut app = App::default();
        let (a, mut b) = pair();
        let base = a.taken;
        let iface = |name: &str, rx: u64, tx: u64| crate::collect::NetIface {
            name: name.into(),
            rx_bytes: rx,
            tx_bytes: tx,
        };
        app.on_event(AppEvent::Snapshot(a));
        b.net.interfaces = vec![
            iface("gif0", 0, 0),           // never active: born hidden
            iface("en0", 101_000, 50_500), // rate > 0 this tick
            iface("utun4", 7_000, 3_000),  // lifetime traffic, idle now... rate 0
        ];
        app.on_event(AppEvent::Snapshot(b));
        // busiest lifetime first: en0, utun4, gif0
        let names: Vec<&str> = app.net_ifaces.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["en0", "utun4", "gif0"]);
        // en0 active; utun4 first sight with lifetime traffic -> grace; gif0 born idle
        let vis: Vec<&str> = app.visible_net().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(vis, ["en0", "utun4"], "gif0 hidden from birth");

        // 6 quiet seconds later utun4 exceeds the grace period
        let mut c = snap(200, 1000);
        c.taken = base + Duration::from_secs(7);
        c.net = NetSnapshot {
            rx_bytes: 108_000,
            tx_bytes: 53_500,
            interfaces: vec![
                iface("gif0", 0, 0),
                iface("en0", 101_000, 50_500), // now idle too, but only 6s
                iface("utun4", 7_000, 3_000),
            ],
            ..Default::default()
        };
        app.on_event(AppEvent::Snapshot(c));
        let vis: Vec<&str> = app.visible_net().iter().map(|r| r.name.as_str()).collect();
        assert_eq!(vis, Vec::<&str>::new(), "everything idle past grace hides");

        // h shows them all again
        key(&mut app, KeyCode::Char('h'));
        assert!(app.show_idle);
        assert_eq!(app.visible_net().len(), 3);
        key(&mut app, KeyCode::Char('h'));
        assert!(!app.show_idle, "h toggles back; hiding is the default");
    }

    #[test]
    fn net_iface_history_accumulates_and_prunes() {
        let mut app = App::default();
        let (a, b) = pair();
        let base = a.taken;
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        let (rx_h, tx_h) = app.net_hist.get("eth0").expect("history exists");
        assert_eq!(rx_h.len(), 1);
        assert!((rx_h[0] - 100_000.0).abs() < 1_000.0);
        assert!((tx_h[0] - 50_000.0).abs() < 500.0);

        // eth0 goes away, wlan0 appears -> old history is pruned
        let mut c = snap(200, 1000);
        c.taken = base + Duration::from_secs(2);
        c.net = NetSnapshot {
            rx_bytes: 102_000,
            tx_bytes: 51_000,
            interfaces: vec![crate::collect::NetIface {
                name: "wlan0".into(),
                rx_bytes: 1_000,
                tx_bytes: 500,
            }],
            ..Default::default()
        };
        app.on_event(AppEvent::Snapshot(c));
        assert!(app.net_hist.get("eth0").is_none(), "vanished iface pruned");
        assert!(app.net_hist.get("wlan0").is_some());
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
    fn selection_follows_pid_across_resort() {
        let mut app = App::default();
        let (a, b) = pair();
        let base = a.taken;
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        // cpu sort put alpha first; anchor the highlight on beta (pid 2)
        assert_eq!(app.procs[0].name, "alpha");
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Down)));
        assert_eq!((app.selected, app.selected_id), (1, Some((2, None))));

        // next tick beta burns cpu and alpha idles -> the order flips
        let mut c = snap(200, 1000);
        c.taken = base + Duration::from_secs(2);
        c.procs = vec![
            ProcessInfo {
                pid: 1,
                ppid: 0,
                name: "alpha".into(),
                cpu_ns: 500_000_000, // unchanged since b -> 0%
                rss: 100,
                disk_read: Some(3 << 20),
                disk_written: Some(1 << 20),
                threads: Vec::new(),
            },
            ProcessInfo {
                pid: 2,
                ppid: 0,
                name: "beta".into(),
                cpu_ns: 800_009_999,
                rss: 9_000,
                disk_read: None,
                disk_written: None,
                threads: Vec::new(),
            },
        ];
        app.on_event(AppEvent::Snapshot(c));
        assert_eq!(app.procs[0].name, "beta");
        assert_eq!(app.selected, 0, "highlight chased beta to the top");
        assert_eq!(app.selected_id, Some((2, None)));

        // beta dies: keep the spot, adopt whoever sits there now
        let mut d = snap(250, 1050);
        d.taken = base + Duration::from_secs(3);
        d.procs = vec![ProcessInfo {
            pid: 1,
            ppid: 0,
            name: "alpha".into(),
            cpu_ns: 500_000_000,
            rss: 100,
            disk_read: Some(3 << 20),
            disk_written: Some(1 << 20),
            threads: Vec::new(),
        }];
        app.on_event(AppEvent::Snapshot(d));
        assert_eq!((app.selected, app.selected_id), (0, Some((1, None))));
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_event(AppEvent::Key(KeyEvent::from(code)));
    }

    #[test]
    fn filter_narrows_live_and_survives_snapshots() {
        let mut app = App::default();
        let (a, b) = pair();
        let base = a.taken;
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        assert_eq!(app.procs.len(), 2);

        // type a case-insensitive needle; the list narrows on every keystroke
        key(&mut app, KeyCode::Char('f'));
        assert!(app.filter_edit);
        for c in "BET".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.procs.len(), 1);
        assert_eq!(app.procs[0].name, "beta");
        assert_eq!(
            app.selected_id,
            Some((2, None)),
            "selection adopted the match"
        );

        // enter commits; the next snapshot must not resurrect hidden rows
        key(&mut app, KeyCode::Enter);
        assert!(!app.filter_edit);
        let mut c = snap(200, 1000);
        c.taken = base + Duration::from_secs(2);
        c.procs = vec![
            ProcessInfo {
                pid: 1,
                ppid: 0,
                name: "alpha".into(),
                cpu_ns: 600_000_000,
                rss: 100,
                disk_read: Some(3 << 20),
                disk_written: Some(1 << 20),
                threads: Vec::new(),
            },
            ProcessInfo {
                pid: 2,
                ppid: 0,
                name: "beta".into(),
                cpu_ns: 10_999,
                rss: 9_000,
                disk_read: None,
                disk_written: None,
                threads: Vec::new(),
            },
        ];
        app.on_event(AppEvent::Snapshot(c));
        assert_eq!(app.procs.len(), 1);
        assert_eq!(app.procs[0].name, "beta");
    }

    #[test]
    fn filter_edit_swallows_keys_and_esc_peels() {
        let mut app = App::default();
        let (a, b) = pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));

        // q and k are text while editing, not quit / kill
        key(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Char('q'));
        key(&mut app, KeyCode::Char('k'));
        assert!(!app.quit);
        assert!(app.confirm_kill.is_none());
        assert_eq!(app.filter, "qk");
        assert!(app.procs.is_empty(), "no proc matches qk");

        // backspace repairs the needle; pid digits match too
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Backspace);
        key(&mut app, KeyCode::Char('2'));
        assert_eq!(app.procs.len(), 1);
        assert_eq!(app.procs[0].pid, 2);

        // esc while editing drops the filter entirely
        key(&mut app, KeyCode::Esc);
        assert!(!app.filter_edit && app.filter.is_empty());
        assert_eq!(app.procs.len(), 2);

        // committed filter: first esc peels it, second esc quits
        key(&mut app, KeyCode::Char('/'));
        key(&mut app, KeyCode::Char('a'));
        key(&mut app, KeyCode::Enter);
        key(&mut app, KeyCode::Esc);
        assert!(!app.quit);
        assert!(app.filter.is_empty());
        key(&mut app, KeyCode::Esc);
        assert!(app.quit);
    }

    #[test]
    fn sort_key_keeps_the_selected_row() {
        let mut app = App::default();
        let (a, b) = pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        // cpu sort: [alpha, beta]; select alpha, then sort by mem flips the order
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Up)));
        assert_eq!(app.selected_id, Some((1, None)));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('m'))));
        assert_eq!(app.procs[0].name, "beta");
        assert_eq!(app.selected, 1, "alpha stays highlighted after the resort");
        assert_eq!(app.selected_id, Some((1, None)));
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

    #[test]
    fn sort_by_name_is_case_insensitive_ascending() {
        let mut app = App {
            procs: vec![row(1, "Zed"), row(2, "alpha"), row(3, "Beta")],
            ..App::default()
        };
        app.selected_id = Some((1, None));
        app.on_event(AppEvent::Key(KeyEvent::from(KeyCode::Char('n'))));
        assert_eq!(app.sort, SortBy::Name);
        let names: Vec<&str> = app.procs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Beta", "Zed"]);
        assert_eq!(app.selected, 2, "highlight followed Zed to the bottom");
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

    fn tproc(pid: i32, name: &str, cpu_ns: u64, rss: u64, threads: Vec<ThreadInfo>) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid: 0,
            name: name.into(),
            cpu_ns,
            rss,
            disk_read: None,
            disk_written: None,
            threads,
        }
    }

    fn tinfo(tid: u64, name: &str, cpu_ns: u64) -> ThreadInfo {
        ThreadInfo {
            tid,
            name: name.into(),
            cpu_ns,
        }
    }

    // two snapshots 1s apart: alpha at 50% with two threads (30% + 10%),
    // beta at 10% with none. cpu sort -> [alpha, beta]
    fn thread_pair() -> (Box<Snapshot>, Box<Snapshot>) {
        let base = Instant::now();
        let mut a = snap(100, 900);
        a.taken = base;
        a.procs = vec![
            tproc(
                1,
                "alpha",
                0,
                500,
                vec![tinfo(10, "worker", 0), tinfo(11, "", 0)],
            ),
            tproc(2, "beta", 0, 100, vec![]),
        ];
        let mut b = snap(150, 950);
        b.taken = base + Duration::from_secs(1);
        b.procs = vec![
            tproc(
                1,
                "alpha",
                500_000_000,
                500,
                vec![tinfo(10, "worker", 300_000_000), tinfo(11, "", 100_000_000)],
            ),
            tproc(2, "beta", 100_000_000, 100, vec![]),
        ];
        (a, b)
    }

    #[test]
    fn t_flattens_threads_under_parent_and_collapses() {
        let mut app = App::default();
        let (a, b) = thread_pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        assert_eq!(app.procs.len(), 2, "threads hidden until toggled");

        key(&mut app, KeyCode::Char('t'));
        assert!(app.show_threads);
        let ids: Vec<(i32, Option<u64>)> = app.procs.iter().map(|p| (p.pid, p.tid)).collect();
        // threads cpu-desc under alpha: worker (30%) before the unnamed one (10%)
        assert_eq!(ids, [(1, None), (1, Some(10)), (1, Some(11)), (2, None)]);
        assert_eq!(app.procs[2].name, "tid 11", "unnamed thread gets a label");
        assert_eq!(prefixes(&app), ["", "├─ ", "└─ ", ""]);
        assert_eq!(app.procs[1].rss, 0);
        assert!(app.procs[1].io_bps.is_none());

        // name sort flips the thread order: "tid 11" < "worker"
        key(&mut app, KeyCode::Char('n'));
        let names: Vec<&str> = app.procs.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["alpha", "tid 11", "worker", "beta"]);
        assert_eq!(app.procs[2].prefix, "└─ ");

        key(&mut app, KeyCode::Char('t'));
        assert_eq!(app.procs.len(), 2, "second t collapses");
        assert!(app.procs.iter().all(|p| p.tid.is_none()));
    }

    #[test]
    fn thread_cpu_pct_from_tid_delta() {
        let mut app = App::default();
        key(&mut app, KeyCode::Char('t'));
        let (a, b) = thread_pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        // worker burned 300ms in 1s, the unnamed thread 100ms
        let worker = app.procs.iter().find(|p| p.tid == Some(10)).unwrap();
        assert!((worker.cpu_pct - 30.0).abs() < 1.0);
        let unnamed = app.procs.iter().find(|p| p.tid == Some(11)).unwrap();
        assert!((unnamed.cpu_pct - 10.0).abs() < 1.0);
    }

    #[test]
    fn selection_follows_thread_across_resort_and_falls_back() {
        let mut app = App::default();
        key(&mut app, KeyCode::Char('t'));
        let (a, b) = thread_pair();
        let base = a.taken;
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        // [alpha, worker, tid 11, beta]; anchor on worker
        key(&mut app, KeyCode::Down);
        assert_eq!(app.selected_id, Some((1, Some(10))));

        // beta takes off and alpha idles; only tid 11 keeps burning ->
        // [beta, alpha, tid 11, worker]
        let mut c = snap(200, 1000);
        c.taken = base + Duration::from_secs(2);
        c.procs = vec![
            tproc(
                1,
                "alpha",
                500_000_000,
                500,
                vec![tinfo(10, "worker", 300_000_000), tinfo(11, "", 150_000_000)],
            ),
            tproc(2, "beta", 900_000_000, 100, vec![]),
        ];
        app.on_event(AppEvent::Snapshot(c));
        assert_eq!(app.procs[3].tid, Some(10));
        assert_eq!(app.selected, 3, "highlight chased the thread row");
        assert_eq!(app.selected_id, Some((1, Some(10))));

        // worker dies: keep the (clamped) spot, adopt whoever sits there
        let mut d = snap(250, 1050);
        d.taken = base + Duration::from_secs(3);
        d.procs = vec![
            tproc(
                1,
                "alpha",
                500_000_000,
                500,
                vec![tinfo(11, "", 200_000_000)],
            ),
            tproc(2, "beta", 1_000_000_000, 100, vec![]),
        ];
        app.on_event(AppEvent::Snapshot(d));
        assert_eq!(app.procs.len(), 3);
        assert_eq!((app.selected, app.selected_id), (2, Some((1, Some(11)))));
    }

    #[test]
    fn k_on_thread_row_targets_the_parent_process() {
        let mut app = App::default();
        key(&mut app, KeyCode::Char('t'));
        let (a, b) = thread_pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        // move onto worker (a thread of alpha) and hit k
        key(&mut app, KeyCode::Down);
        assert_eq!(app.procs[app.selected].tid, Some(10));
        key(&mut app, KeyCode::Char('k'));
        let kp = app.confirm_kill.as_ref().expect("prompt opens");
        assert_eq!((kp.pid, kp.name.as_str()), (1, "alpha"));
        key(&mut app, KeyCode::Esc);
    }

    #[test]
    fn filter_hides_threads_with_their_parent() {
        let mut app = App::default();
        key(&mut app, KeyCode::Char('t'));
        let (a, b) = thread_pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));

        // alpha filtered out -> its threads go with it
        key(&mut app, KeyCode::Char('f'));
        for c in "beta".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.procs.len(), 1);
        assert_eq!(app.procs[0].name, "beta");

        // visible parent brings its threads back
        for _ in 0..4 {
            key(&mut app, KeyCode::Backspace);
        }
        for c in "alpha".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        let ids: Vec<(i32, Option<u64>)> = app.procs.iter().map(|p| (p.pid, p.tid)).collect();
        assert_eq!(ids, [(1, None), (1, Some(10)), (1, Some(11))]);
    }

    #[test]
    fn t_while_filter_editing_is_text_not_a_toggle() {
        let mut app = App::default();
        key(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Char('t'));
        assert!(!app.show_threads);
        assert_eq!(app.filter, "t");
    }

    fn pproc(pid: i32, ppid: i32, name: &str, cpu_ns: u64) -> ProcessInfo {
        ProcessInfo {
            pid,
            ppid,
            name: name.into(),
            cpu_ns,
            rss: 0,
            disk_read: None,
            disk_written: None,
            threads: Vec::new(),
        }
    }

    // 1s apart; cpu desc: 1 (40%) > 3 (30%) > 2 (20%) > 4 (10%).
    // hierarchy: 1 -> {2, 3}, 3 -> {4}
    fn tree_pair() -> (Box<Snapshot>, Box<Snapshot>) {
        let base = Instant::now();
        let mut a = snap(100, 900);
        a.taken = base;
        a.procs = vec![
            pproc(1, 0, "one", 0),
            pproc(2, 1, "two", 0),
            pproc(3, 1, "three", 0),
            pproc(4, 3, "four", 0),
        ];
        let mut b = snap(150, 950);
        b.taken = base + Duration::from_secs(1);
        b.procs = vec![
            pproc(1, 0, "one", 400_000_000),
            pproc(2, 1, "two", 200_000_000),
            pproc(3, 1, "three", 300_000_000),
            pproc(4, 3, "four", 100_000_000),
        ];
        (a, b)
    }

    fn pids(app: &App) -> Vec<i32> {
        app.procs.iter().map(|p| p.pid).collect()
    }

    fn prefixes(app: &App) -> Vec<&str> {
        app.procs.iter().map(|p| p.prefix.as_str()).collect()
    }

    #[test]
    fn e_builds_the_ppid_tree_and_collapses() {
        let mut app = App::default();
        let (a, b) = tree_pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        assert_eq!(pids(&app), [1, 3, 2, 4], "flat cpu order first");

        key(&mut app, KeyCode::Char('e'));
        assert!(app.tree);
        // children of 1 sort by cpu: 3 before 2; 4 hangs off 3
        assert_eq!(pids(&app), [1, 3, 4, 2]);
        assert_eq!(prefixes(&app), ["", "├─ ", "│  └─ ", "└─ "]);

        key(&mut app, KeyCode::Char('e'));
        assert!(!app.tree);
        assert_eq!(pids(&app), [1, 3, 2, 4], "second e restores flat order");
        assert!(app.procs.iter().all(|p| p.prefix.is_empty()));
    }

    #[test]
    fn orphan_and_self_parent_become_roots() {
        let mut app = App::default();
        let base = Instant::now();
        let mut a = snap(100, 900);
        a.taken = base;
        a.procs = vec![pproc(5, 99, "orphan", 0), pproc(7, 7, "selfie", 0)];
        let mut b = snap(150, 950);
        b.taken = base + Duration::from_secs(1);
        b.procs = vec![
            pproc(5, 99, "orphan", 200_000_000),
            pproc(7, 7, "selfie", 100_000_000),
        ];
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        key(&mut app, KeyCode::Char('e'));
        assert_eq!(pids(&app), [5, 7], "both are roots, neither loops");
        assert_eq!(prefixes(&app), ["", ""]);
    }

    #[test]
    fn ppid_cycle_terminates_and_surfaces_every_pid() {
        let mut app = App::default();
        let base = Instant::now();
        let mut a = snap(100, 900);
        a.taken = base;
        a.procs = vec![
            pproc(1, 0, "root", 0),
            pproc(2, 3, "yin", 0),
            pproc(3, 2, "yang", 0),
        ];
        let mut b = snap(150, 950);
        b.taken = base + Duration::from_secs(1);
        b.procs = vec![
            pproc(1, 0, "root", 500_000_000),
            pproc(2, 3, "yin", 300_000_000),
            pproc(3, 2, "yang", 200_000_000),
        ];
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        key(&mut app, KeyCode::Char('e'));
        // 2 <-> 3 never reach a root; the sweep adopts 2 (higher cpu) as one
        assert_eq!(pids(&app), [1, 2, 3]);
        assert_eq!(app.procs.iter().filter(|p| p.pid == 2).count(), 1);
        assert_eq!(app.procs.iter().filter(|p| p.pid == 3).count(), 1);
    }

    #[test]
    fn filter_bypasses_the_tree_and_clearing_restores_it() {
        let mut app = App::default();
        let (a, b) = tree_pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        key(&mut app, KeyCode::Char('e'));
        assert_eq!(pids(&app), [1, 3, 4, 2]);

        // "t" matches two and three: flat filtered list, no rails
        key(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Char('t'));
        assert_eq!(pids(&app), [3, 2], "flat cpu order within the matches");
        assert!(app.procs.iter().all(|p| p.prefix.is_empty()));
        assert!(app.tree, "the toggle survives the filter");

        // esc drops the filter; the tree comes back
        key(&mut app, KeyCode::Esc);
        assert_eq!(pids(&app), [1, 3, 4, 2]);
        assert_eq!(prefixes(&app), ["", "├─ ", "│  └─ ", "└─ "]);
    }

    #[test]
    fn tree_threads_hang_under_their_proc_before_child_procs() {
        let mut app = App::default();
        key(&mut app, KeyCode::Char('t'));
        key(&mut app, KeyCode::Char('e'));
        let base = Instant::now();
        // 1 -> {2, 3}, 2 -> {4}; 2 also owns two threads
        let mut two_a = pproc(2, 1, "two", 0);
        two_a.threads = vec![tinfo(20, "w", 0), tinfo(21, "x", 0)];
        let mut a = snap(100, 900);
        a.taken = base;
        a.procs = vec![
            pproc(1, 0, "one", 0),
            two_a,
            pproc(3, 1, "three", 0),
            pproc(4, 2, "four", 0),
        ];
        let mut two_b = pproc(2, 1, "two", 400_000_000);
        two_b.threads = vec![tinfo(20, "w", 300_000_000), tinfo(21, "x", 100_000_000)];
        let mut b = snap(150, 950);
        b.taken = base + Duration::from_secs(1);
        b.procs = vec![
            pproc(1, 0, "one", 500_000_000),
            two_b,
            pproc(3, 1, "three", 300_000_000),
            pproc(4, 2, "four", 200_000_000),
        ];
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        let ids: Vec<(i32, Option<u64>)> = app.procs.iter().map(|p| (p.pid, p.tid)).collect();
        // threads of 2 come first (cpu desc), then its child proc 4
        assert_eq!(
            ids,
            [
                (1, None),
                (2, None),
                (2, Some(20)),
                (2, Some(21)),
                (4, None),
                (3, None)
            ]
        );
        // 2 has a sibling below (3), so its subtree keeps the │ rail
        assert_eq!(
            prefixes(&app),
            ["", "├─ ", "│  ├─ ", "│  ├─ ", "│  └─ ", "└─ "]
        );
    }

    #[test]
    fn selection_on_a_deep_child_survives_e_toggles() {
        let mut app = App::default();
        let (a, b) = tree_pair();
        app.on_event(AppEvent::Snapshot(a));
        app.on_event(AppEvent::Snapshot(b));
        key(&mut app, KeyCode::Char('e'));
        // [1, 3, 4, 2]: land on 4, the grandchild
        key(&mut app, KeyCode::Down);
        key(&mut app, KeyCode::Down);
        assert_eq!(app.selected_id, Some((4, None)));

        key(&mut app, KeyCode::Char('e'));
        assert_eq!(app.selected_id, Some((4, None)));
        assert_eq!(app.selected, 3, "4 is last in flat cpu order");

        key(&mut app, KeyCode::Char('e'));
        assert_eq!(app.selected_id, Some((4, None)));
        assert_eq!(app.selected, 2, "back to its tree slot");
    }

    #[test]
    fn e_while_filter_editing_is_text_not_a_toggle() {
        let mut app = App::default();
        key(&mut app, KeyCode::Char('f'));
        key(&mut app, KeyCode::Char('e'));
        assert!(!app.tree);
        assert_eq!(app.filter, "e");
    }

    #[test]
    fn viewport_follows_selection_only_off_window() {
        let mut app = App {
            procs: (0..100).map(|i| row(i, "p")).collect(),
            ..App::default()
        };
        // selection inside the window: the view stays put
        app.view_offset = 10;
        app.selected = 15;
        assert_eq!(app.scroll_viewport(20), 10);
        // below the window: scroll just enough to show it
        app.selected = 40;
        assert_eq!(app.scroll_viewport(20), 21);
        // above the window: snap to it
        app.selected = 5;
        assert_eq!(app.scroll_viewport(20), 5);
        // list shrank under the offset: clamp
        app.procs.truncate(3);
        app.selected = 2;
        assert_eq!(app.scroll_viewport(20), 2);
    }

    #[test]
    fn anchored_row_stays_on_its_screen_line_across_resorts() {
        let mut app = App {
            procs: (0..50).map(|i| row(i, "p")).collect(),
            ..App::default()
        };
        // user scrolled to a window starting at 10 and picked row 15:
        // screen line = 15 - 10 = 5
        app.view_offset = 10;
        app.select(15);
        // a resort pushes pid 15 down to index 30
        let mut moved = app.procs.clone();
        moved.swap(15, 30);
        app.procs = moved;
        app.reanchor();
        assert_eq!(app.selected, 30);
        assert_eq!(app.view_offset, 25, "same screen line: 30 - 25 == 5");
        assert_eq!(app.scroll_viewport(20), 25, "no extra follow-scroll");
        // and back up, clamped at the top of the list
        let mut moved = app.procs.clone();
        moved.swap(30, 2);
        app.procs = moved;
        app.reanchor();
        assert_eq!(app.selected, 2);
        assert_eq!(app.view_offset, 0, "cannot scroll above the first row");
    }
}
