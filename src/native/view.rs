//! App -> the surface document. Tern lays it out and draws it; rmon only says
//! what is on screen. ids name the thing they show (a pid, a disk) so a row
//! keeps its node across resorts and the per-tick diff stays small.
//! kinds: https://docs.stencil.so/tern/elements/index.html

use std::collections::VecDeque;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;
use serde_json::{Value, json};

use crate::app::{App, AppEvent, BenchPicker, BenchState, KillPrompt, ProcRow, SortBy};
use crate::collect::BatteryState;
use crate::fetch::FetchInfo;
use crate::tsp::doc::Node;
use crate::tsp::wire::Event;
use crate::ui::fmt::{duration_short, humanize, rate};
use crate::ui::{braille_cell, braille_grid};

pub const SF: &str = "rmon";
const PROC_LIST: &str = "proc.list";
const PICK_LIST: &str = "layer.pick.list";
const FILTER: &str = "proc.filter";

/// palette tokens for the ratatui load gradient at 0%, 10%, .. 100%
pub const GRADIENT: [&str; 11] = [
    "g0", "g1", "g2", "g3", "g4", "g5", "g6", "g7", "g8", "g9", "g10",
];

pub struct Ctx {
    /// pane size in cells: the pty keeps the grid's size under the surface
    pub cols: usize,
    pub rows: usize,
    /// the terminal takes the `t` palette, so rmon's own color tokens resolve
    pub palette: bool,
}

impl Ctx {
    /// a series color: rmon's palette token, or the nearest semantic one
    fn series(&self, name: &'static str) -> &'static str {
        if self.palette {
            return name;
        }
        match name {
            "rx" => "info",
            "tx" => "warning",
            _ => "accent",
        }
    }

    /// a load color: a ratatui gradient stop, or green/yellow/red without
    /// the palette
    fn load(&self, pct: f64) -> &'static str {
        if !self.palette {
            return load_tone(pct);
        }
        GRADIENT[(pct.clamp(0.0, 100.0) / 10.0).round() as usize]
    }
}

/// Tern draws its sheets for a 13px font on a 16px line and zooms them with
/// the font, so their px turn into grid lines and cells at fixed ratios.
/// measured on Tern 0.4.5: a card head is lh+12px and its body ends in 10px,
/// list items and table rows are lh+6px, table heads 24px, the dock 48px
mod metric {
    pub const LINE_PX: f64 = 16.0;
    pub const DOCK: f64 = 48.0 / LINE_PX;
    pub const MAIN_PAD: f64 = 12.0 / LINE_PX;
    pub const BLOCK_GAP: f64 = 16.0 / LINE_PX;
    pub const CARD: f64 = 1.0 + 22.0 / LINE_PX;
    pub const INNER_GAP: f64 = 8.0 / LINE_PX;
    pub const ROW: f64 = 1.0 + 6.0 / LINE_PX;
    pub const TABLE_HEAD: f64 = 24.0 / LINE_PX;
    pub const KV_ROW: f64 = 1.0 + 1.0 / LINE_PX;
    pub const INPUT: f64 = 1.0 + 12.0 / LINE_PX;
    /// `gap: md` between the dsk and mem cards
    pub const COL_GAP: f64 = 10.0 / LINE_PX;
    /// a card body's 36px + 12px side padding in cells, plus a cell of slack
    pub const CARD_CELLS: usize = 8;
}

/// "c00 " + a meter + " 100.0% 100°"
const CORE_CELLS: usize = 26;
const GPU_ROWS: usize = 3;
/// the ratatui net panel: 8 rows less its borders
const NET_ROWS: usize = 6;
const MOUNT_ROWS: usize = 6;
/// the fewest proc rows worth a band
const PROC_MIN: usize = 5;

/// what each panel gets of the pane, the way the ratatui layout splits the
/// terminal: cpu takes 30%, gpu and net fixed rows, the dsk|proc band the
/// rest. Tern sizes cards to their content and never shrinks them, so every
/// panel is cut to its share here or the pane scrolls
struct Plan {
    graph_cells: usize,
    graph_rows: usize,
    core_cols: usize,
    /// the core meters float in a box over the graph's middle, as in the
    /// ratatui panel; else they sit under the graph
    cores_mid: bool,
    box_cells: usize,
    gpu_cells: usize,
    gpu_rows: usize,
    net_rows: usize,
    spark_cells: usize,
    band: f64,
    proc_rows: usize,
    disk_rows: usize,
    mount_rows: usize,
    mem_cols: usize,
    io_cells: usize,
    io_rows: usize,
}

fn plan(app: &App, cx: &Ctx) -> Plan {
    use metric::*;
    let rows = cx.rows as f64;
    let inner = cx.cols.saturating_sub(CARD_CELLS);
    // the band splits 45/55 around a 10px gap
    let dsk_cells = (cx.cols.saturating_sub(2) * 45 / 100).saturating_sub(CARD_CELLS);

    let target = ((rows * 0.3 - CARD).floor().max(0.0) as usize).clamp(3, 16);
    let n = app.core_percents.len();
    let load = usize::from(app.load_avg.is_some());
    // two meter columns like the ratatui overlay, more when the rows are short
    let core_cols = [1, 2, 4, 8]
        .into_iter()
        .filter(|k| *k >= 2 || n < 2)
        .find(|k| n.div_ceil(*k) + load <= target)
        .unwrap_or(8);
    let core_lines = n.div_ceil(core_cols) + load;
    // a headless card: the meters plus 12px of padding a side, 10px over and under
    let box_cells = core_cols * CORE_CELLS + 4;
    let box_lines = (core_lines as f64 + 20.0 / LINE_PX).ceil() as usize;
    let cores_mid = core_lines > 0 && inner >= box_cells + 20;
    let graph_cells = inner;
    let graph_min = if cores_mid { box_lines.max(3) } else { 3 };
    let cpu_card = |g: usize| {
        let body = if cores_mid || core_lines == 0 {
            g as f64
        } else {
            (g + core_lines) as f64 + INNER_GAP
        };
        let status = if app.status.is_some() {
            1.0 + INNER_GAP
        } else {
            0.0
        };
        CARD + body + status + BLOCK_GAP
    };
    let has_gpu = app.gpu_util_pct.is_some() || !app.gpu_hist.is_empty();

    // the dsk side needs a disk row, the mounts, smart and a bench; mem packs
    // into two columns when there is room
    let m = &app.mem;
    let mem_meters = 2 + usize::from(m.compressed > 0) + usize::from(m.swap_total > 0);
    let mem_cols = if dsk_cells >= 70 { 2 } else { 1 };
    let mem_card = CARD + mem_meters.div_ceil(mem_cols) as f64;
    let mount_rows = app.mounts.len().min(MOUNT_ROWS);
    let mut fixed = 0.0;
    if mount_rows > 0 {
        fixed += mount_rows as f64 + INNER_GAP;
    }
    if !app.smart.is_empty() {
        fixed += KV_ROW * app.smart.len() as f64 + INNER_GAP;
    }
    if let Some(b) = &app.bench {
        fixed += bench_lines(b) + INNER_GAP;
    }
    let disks = app.visible_disks().len();
    let table = |k: usize| {
        if k == 0 {
            0.0
        } else {
            (TABLE_HEAD + ROW * k as f64).ceil() + INNER_GAP
        }
    };
    let left_min = mem_card + COL_GAP + CARD + fixed + table(disks.min(1));
    let filter = if app.filter_edit || !app.filter.is_empty() {
        INPUT + INNER_GAP
    } else {
        0.0
    };
    let proc_min = CARD + filter + ROW + INNER_GAP + ROW * PROC_MIN as f64;
    let band_min = left_min.max(proc_min).ceil();

    // short panes give up cpu graph rows, then the gpu and net rows; only a
    // pane too short for every minimum scrolls
    let mut graph_rows = target.max(graph_min);
    let mut gpu_rows = GPU_ROWS;
    let mut net_rows = app.visible_net().len().clamp(1, NET_ROWS);
    let band = loop {
        let mut used = DOCK + MAIN_PAD + cpu_card(graph_rows);
        if has_gpu {
            used += CARD + gpu_rows as f64 + BLOCK_GAP;
        }
        used += CARD + net_rows as f64 + BLOCK_GAP;
        // the band takes what is left, fraction and all: its rows fill the pane
        let band = rows - used;
        let short = band_min - band;
        if short <= 0.0 {
            break band;
        }
        if graph_rows > graph_min {
            graph_rows = graph_rows
                .saturating_sub(short.ceil() as usize)
                .max(graph_min);
        } else if has_gpu && gpu_rows > 1 {
            gpu_rows = 1;
        } else if net_rows > 1 {
            net_rows = 1;
        } else {
            break band_min;
        }
    };

    let proc_rows = (((band - CARD - filter - ROW - INNER_GAP) / ROW).floor() as usize).max(3);
    // spare dsk lines go to disk rows first, then to the io graph
    let mut left = band - mem_card - COL_GAP - CARD - fixed;
    let mut disk_rows = 0;
    if disks > 0 {
        let fit = ((left - TABLE_HEAD - INNER_GAP) / ROW).floor().max(1.0) as usize;
        disk_rows = disks.min(fit);
        left -= table(disk_rows);
    }
    let io_rows = match left.floor() as usize {
        r if r < 2 => 0,
        r => r.min(8),
    };
    Plan {
        graph_cells,
        graph_rows,
        core_cols,
        cores_mid,
        box_cells,
        gpu_cells: inner,
        gpu_rows,
        net_rows,
        // name, two rates and the gaps between them take 41 cells
        spark_cells: inner.saturating_sub(41) / 2,
        band,
        proc_rows,
        disk_rows,
        mount_rows,
        mem_cols,
        io_cells: dsk_cells,
        io_rows,
    }
}

/// the bench card's lines: a bare head, then the live line and the results
fn bench_lines(b: &BenchState) -> f64 {
    use metric::*;
    let live = usize::from(b.error.is_some()) + usize::from(b.running.is_some());
    let results = b.results.len() + usize::from(b.direct == Some(false));
    let mut lines = 1.0 + live as f64 * (1.0 + INNER_GAP);
    if results > 0 {
        lines += KV_ROW * results as f64 + INNER_GAP;
    }
    lines + INNER_GAP
}

/// the whole document, and the field that should own the caret
pub fn build(app: &App, cx: &Ctx) -> (Node, Option<String>) {
    let p = plan(app, cx);
    let mut main = vec![cpu(app, cx, &p)];
    main.extend(gpu(app, cx, &p));
    main.push(net(app, cx, &p));
    main.push(
        // the band grows into whatever `main` has left, both sides stretched
        // to it, so the panels reach the bottom of the pane like ratatui's
        Node::new("band", "row")
            .prop("gap", "md")
            .prop("align", "stretch")
            .prop("grow", 1)
            .child(
                // the dsk side is the one whose content can outgrow its
                // share (a bench, many mounts): cut it at the band
                Node::new("band.l", "col")
                    .prop("gap", "md")
                    .prop("basis", 0.45)
                    .prop("grow", 1)
                    .prop("max", json!({"h": format!("{}lines", p.band.ceil())}))
                    .child(dsk(app, cx, &p).prop("grow", 1))
                    .child(mem(app, &p)),
            )
            .child(
                Node::new("band.r", "col")
                    .prop("basis", 0.55)
                    .prop("grow", 1)
                    .child(procs(app, &p).prop("grow", 1)),
            ),
    );
    let doc = Node::new(SF, "col")
        .child(Node::new("main", "col").children(main))
        .child(Node::new("dock", "col").child(status(app)))
        .child(Node::new("layer", "col").children(overlays(app)));
    (doc, app.filter_edit.then(|| FILTER.to_string()))
}

/// pointer events the document asked for, turned into the same App changes
/// the keys make
pub fn on_event(app: &mut App, ev: Event) {
    match ev {
        Event::Select { id, item } if id == PICK_LIST => pick(app, &item),
        Event::Activate { id, item } if id == PICK_LIST => {
            pick(app, &item);
            press(app, KeyCode::Enter);
        }
        Event::Select { id, item } | Event::Activate { id, item } if id == PROC_LIST => {
            if let Some(i) =
                proc_key(&item).and_then(|k| app.procs.iter().position(|p| (p.pid, p.tid) == k))
            {
                app.select(i);
            }
        }
        Event::Action {
            act,
            value: Some(v),
        } if act == "key" => {
            let mut cs = v.chars();
            if let (Some(c), None) = (cs.next(), cs.next()) {
                // a click on a key hint means the hint, not a letter for the filter
                app.filter_edit = false;
                press(app, KeyCode::Char(c));
            }
        }
        _ => {}
    }
}

fn press(app: &mut App, code: KeyCode) {
    app.on_event(AppEvent::Key(KeyEvent::new(code, KeyModifiers::NONE)));
}

fn pick(app: &mut App, item: &str) {
    if let Some(p) = &mut app.picker
        && let Some(i) = item
            .strip_prefix(PICK_LIST)
            .and_then(|s| s.strip_prefix('.'))
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|i| *i < p.entries.len())
    {
        p.selected = i;
    }
}

fn proc_id(p: &ProcRow) -> String {
    match p.tid {
        None => format!("proc.p{}", p.pid),
        Some(t) => format!("proc.t{}.{t}", p.pid),
    }
}

fn proc_key(id: &str) -> Option<(i32, Option<u64>)> {
    if let Some(pid) = id.strip_prefix("proc.p") {
        return Some((pid.parse().ok()?, None));
    }
    let (pid, tid) = id.strip_prefix("proc.t")?.split_once('.')?;
    Some((pid.parse().ok()?, Some(tid.parse().ok()?)))
}

fn sp(t: impl Into<String>, s: &str) -> Value {
    if s.is_empty() {
        json!({"t": t.into()})
    } else {
        json!({"t": t.into(), "s": s})
    }
}

fn text(id: impl Into<String>, spans: Vec<Value>) -> Node {
    Node::new(id, "text").prop("spans", spans)
}

/// the same green/yellow/red split the ratatui gradient makes
fn load_tone(pct: f64) -> &'static str {
    if pct >= 80.0 {
        "error"
    } else if pct >= 50.0 {
        "warning"
    } else {
        "success"
    }
}

/// 0..=1, three decimals: finer steps are invisible and only cost diff bytes
fn frac(pct: f64) -> f64 {
    ((pct / 100.0).clamp(0.0, 1.0) * 1000.0).round() / 1000.0
}

fn ratio(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        frac(part as f64 * 100.0 / whole as f64)
    }
}

enum Ink {
    /// each cell column in the gradient color of its load, as ratatui does
    Load,
    Series(&'static str),
}

/// a braille history graph as text rows of the ratatui graph's cells, so it
/// scrolls a column per tick. (a `chart` replays its bar entry animation on
/// every change, and only a stylesheet can stop that.) `max: None` scales
/// to the peak in view over the ratatui 1 KiB/s floor
fn graph(
    cx: &Ctx,
    id: &str,
    hist: &VecDeque<f64>,
    max: Option<f64>,
    cells: usize,
    rows: usize,
    ink: Ink,
) -> Node {
    let (grid, col_pct) = graph_grid(hist, max, cells, rows);
    graph_cols(cx, id, &grid, &col_pct, &ink, 0..cells)
}

fn graph_grid(
    hist: &VecDeque<f64>,
    max: Option<f64>,
    cells: usize,
    rows: usize,
) -> (Vec<Vec<char>>, Vec<f64>) {
    let take = hist.len().min(cells * 2);
    let vals: Vec<f64> = hist.iter().skip(hist.len() - take).copied().collect();
    let max = max.unwrap_or_else(|| vals.iter().copied().fold(1024.0, f64::max) * 1.1);
    braille_grid(&vals, max, cells, rows)
}

/// cell columns `cols` of a graph grid as a column of text rows
fn graph_cols(
    cx: &Ctx,
    id: &str,
    grid: &[Vec<char>],
    col_pct: &[f64],
    ink: &Ink,
    cols: std::ops::Range<usize>,
) -> Node {
    let blank = braille_cell(0, 0);
    let lines = grid.iter().enumerate().map(|(r, line)| {
        let mut spans = Vec::new();
        let mut run = String::new();
        let mut tok = "";
        for x in cols.clone() {
            let ch = line[x];
            let t = match (ink, ch == blank) {
                (_, true) => "",
                (Ink::Load, false) => cx.load(col_pct[x]),
                (Ink::Series(s), false) => cx.series(s),
            };
            if t != tok && !run.is_empty() {
                spans.push(sp(std::mem::take(&mut run), tok));
            }
            tok = t;
            run.push(ch);
        }
        if !run.is_empty() {
            spans.push(sp(run, tok));
        }
        // a cell too many cuts the oldest end, never wraps the graph
        Node::new(format!("{id}.{r}"), "text")
            .prop("spans", spans)
            .prop("wrap", "none")
            .prop("truncate", "start")
    });
    Node::new(id, "col").prop("align", "end").children(lines)
}

fn card(id: &str, head: Vec<Value>) -> Node {
    Node::new(id, "card").prop("head", head)
}

fn cpu(app: &App, cx: &Ctx, p: &Plan) -> Node {
    let total = app.cpu_history.back().copied().unwrap_or(0.0);
    let mut head = vec![sp("cpu", "strong")];
    let ident: Vec<String> = app
        .cpu_name
        .iter()
        .cloned()
        .chain(app.cpu_temp_c.map(|t| format!("{t:.0}°C")))
        .collect();
    if !ident.is_empty() {
        head.push(sp(format!("  {}", ident.join(" ")), "muted"));
    }
    head.push(sp(format!("  {total:.1}%"), load_tone(total)));
    if let Some(up) = app.uptime_secs {
        head.push(sp(format!("  up {}", duration_short(up)), "muted"));
    }
    if let Some(b) = app.battery {
        let sym = match b.state {
            BatteryState::Charging => '▲',
            BatteryState::Discharging => '▼',
            BatteryState::Full => '■',
            BatteryState::Unknown => '○',
        };
        let low = b.percent < 20.0 && b.state == BatteryState::Discharging;
        head.push(sp(
            format!("  BAT{sym} {:.0}%", b.percent),
            if low { "warning" } else { "strong" },
        ));
        let mut rest = String::new();
        if let Some(secs) = b.secs_left {
            rest.push_str(&format!(" {}", duration_short(secs)));
        }
        // idle on ac reads ~0 W; noise, not information
        if let Some(w) = b.watts.filter(|w| *w >= 0.05) {
            rest.push_str(&format!(" {w:.1}W"));
        }
        if !rest.is_empty() {
            head.push(sp(rest, "muted"));
        }
    }

    let mut c = card("cpu", head);
    if p.cores_mid {
        // the graph spans the card; the meters box covers its middle and the
        // history shows on both sides of it, as in the ratatui panel
        let (grid, col_pct) =
            graph_grid(&app.cpu_history, Some(100.0), p.graph_cells, p.graph_rows);
        let left = p.graph_cells.saturating_sub(p.box_cells) / 2;
        let right = (left + p.box_cells).min(p.graph_cells);
        let w = json!({"w": format!("{}ch", p.box_cells)});
        c = c.child(
            Node::new("cpu.body", "row")
                .prop("justify", "between")
                .child(graph_cols(
                    cx,
                    "cpu.hl",
                    &grid,
                    &col_pct,
                    &Ink::Load,
                    0..left,
                ))
                .child(
                    Node::new("cpu.box", "card")
                        .prop("min", w.clone())
                        .prop("max", w)
                        .prop("shrink", 0)
                        .child(cores(app, p)),
                )
                .child(graph_cols(
                    cx,
                    "cpu.hr",
                    &grid,
                    &col_pct,
                    &Ink::Load,
                    right..p.graph_cells,
                )),
        );
    } else {
        let hist = graph(
            cx,
            "cpu.hist",
            &app.cpu_history,
            Some(100.0),
            p.graph_cells,
            p.graph_rows,
            Ink::Load,
        );
        c = c.child(hist).child(cores(app, p));
    }
    if let Some(err) = &app.status {
        c = c.child(text("cpu.status", vec![sp(err.clone(), "error")]));
    }
    c
}

/// per-core meters, column-major like the ratatui overlay, with the load
/// average under them
fn cores(app: &App, p: &Plan) -> Node {
    let n = app.core_percents.len();
    let lines = n.div_ceil(p.core_cols);
    let mut col = Node::new("cpu.cores", "col");
    for l in 0..lines {
        let mut row = Node::new(format!("cpu.cl{l}"), "row").prop("gap", "md");
        for k in 0..p.core_cols {
            let i = l + k * lines;
            let Some(pct) = app.core_percents.get(i) else {
                continue;
            };
            let label = match app.core_temps_c.get(i).filter(|t| !t.is_nan()) {
                Some(t) => format!("{pct:5.1}% {t:3.0}°"),
                None => format!("{pct:5.1}%"),
            };
            let id = format!("cpu.c{i}");
            row = row.child(
                Node::new(&id, "row")
                    .prop("gap", "sm")
                    .prop("basis", 1.0 / p.core_cols as f64)
                    .prop("grow", 1)
                    .child(text(
                        format!("{id}.l"),
                        vec![sp(format!("c{i:02}"), "muted mono")],
                    ))
                    .child(
                        Node::new(format!("{id}.m"), "meter")
                            .prop("value", frac(*pct))
                            .prop("tone", load_tone(*pct))
                            .prop("label", vec![sp(label, "mono")])
                            .prop("grow", 1),
                    ),
            );
        }
        col = col.child(row);
    }
    if let Some([one, five, fifteen]) = app.load_avg {
        col = col.child(Node::new("cpu.load", "kv").prop("layout", "inline").prop(
            "items",
            json!([{"k": "load", "v": format!("{one:.2} {five:.2} {fifteen:.2}")}]),
        ));
    }
    col
}

fn gpu(app: &App, cx: &Ctx, p: &Plan) -> Option<Node> {
    if app.gpu_util_pct.is_none() && app.gpu_hist.is_empty() {
        return None;
    }
    let util = app
        .gpu_util_pct
        .or_else(|| app.gpu_hist.back().copied())
        .unwrap_or(0.0);
    let mut head = vec![sp("gpu", "strong")];
    if let Some(name) = &app.gpu_name {
        head.push(sp(format!("  {name}"), "muted"));
    }
    head.push(sp(format!("  {util:.1}%"), load_tone(util)));
    // fixed 0..100 scale: it's a percentage, peak-scaling would just lie
    Some(card("gpu", head).child(graph(
        cx,
        "gpu.hist",
        &app.gpu_hist,
        Some(100.0),
        p.gpu_cells,
        p.gpu_rows,
        Ink::Load,
    )))
}

fn net(app: &App, cx: &Ctx, p: &Plan) -> Node {
    let rows = app.visible_net();
    let hidden = app.net_ifaces.len() - rows.len();
    let rx = app.net_rx.back().copied().unwrap_or(0.0);
    let tx = app.net_tx.back().copied().unwrap_or(0.0);
    let mut count = format!("  {} ifaces", rows.len());
    if hidden > 0 {
        count.push_str(&format!(" (+{hidden} idle)"));
    }
    let head = vec![
        sp("net", "strong"),
        sp(count, "muted"),
        // aggregate rate now, cumulative since boot in parens
        sp(
            format!("  ↓ {} ({})", rate(rx), humanize(app.net_rx_total)),
            cx.series("rx"),
        ),
        sp(
            format!("  ↑ {} ({})", rate(tx), humanize(app.net_tx_total)),
            cx.series("tx"),
        ),
    ];
    let c = card("net", head);
    if rows.is_empty() {
        return c.child(text("net.none", vec![sp("no active interfaces", "muted")]));
    }
    let mut list = scroller("net.rows", p.net_rows as f64);
    for i in rows {
        let id = format!("net.i.{}", i.name);
        let mut row = Node::new(&id, "row")
            .prop("gap", "md")
            .child(
                text(format!("{id}.n"), vec![sp(i.name.clone(), "strong")])
                    .prop("min", json!({"w": "10ch"})),
            )
            .child(
                text(
                    format!("{id}.r"),
                    vec![sp("↓ ", cx.series("rx")), sp(rate(i.rx_bps), "mono")],
                )
                .prop("min", json!({"w": "13ch"})),
            )
            .child(
                text(
                    format!("{id}.t"),
                    vec![sp("↑ ", cx.series("tx")), sp(rate(i.tx_bps), "mono")],
                )
                .prop("min", json!({"w": "13ch"})),
            );
        if let Some((rx_h, tx_h)) = app.net_hist.get(&i.name) {
            let spark = |side: &str, h: &VecDeque<f64>, series| {
                graph(
                    cx,
                    &format!("{id}.{side}"),
                    h,
                    None,
                    p.spark_cells,
                    1,
                    Ink::Series(series),
                )
                .prop("grow", 1)
            };
            row = row
                .child(spark("rs", rx_h, "rx"))
                .child(spark("ts", tx_h, "tx"));
        }
        list = list.child(row);
    }
    c.child(list)
}

/// a column cut at `lines`; with the stylesheet the cut scrolls, so `h`
/// showing every idle interface or disk cannot push the procs off screen
fn scroller(id: &str, lines: f64) -> Node {
    Node::new(id, "col")
        .prop("role", "rmon.scroll")
        .prop("max", json!({"h": format!("{}lines", lines.ceil())}))
}

fn dsk(app: &App, cx: &Ctx, p: &Plan) -> Node {
    let io = app.disk_io.back().copied().unwrap_or(0.0);
    let disks = app.visible_disks();
    let hidden = app.disks.len() - disks.len();
    let mut head = vec![
        sp("dsk", "strong"),
        sp(format!("  io {}", rate(io)), cx.series("io")),
    ];
    if hidden > 0 {
        head.push(sp(format!("  +{hidden} idle"), "muted"));
    }
    let mut c = card("dsk", head);
    if !disks.is_empty() {
        let dash = || json!("—");
        let rows: Vec<Value> = disks
            .iter()
            .map(|d| {
                json!({"id": d.name, "cells": {
                    "name": d.name,
                    "r": rate(d.read_bps),
                    "w": rate(d.write_bps),
                    "iops": format!("{:.0}", d.iops),
                    "util": d.util_pct.map_or_else(dash, |u| json!({"meter": {
                        "value": frac(u), "tone": load_tone(u), "title": format!("{u:.1}% busy"),
                    }})),
                    "lat": d.lat_ms.map_or_else(dash, |v| json!(format!("{v:.2}ms"))),
                    "q": d.queue.map_or_else(dash, |v| json!(format!("{v:.1}"))),
                }})
            })
            .collect();
        // narrow panels drop columns lowest priority first
        let table = Node::new("dsk.disks", "table").prop("rows", rows).prop(
            "cols",
            json!([
                {"id": "name", "head": "disk", "priority": 9},
                {"id": "r", "head": "read", "align": "end", "priority": 8},
                {"id": "w", "head": "write", "align": "end", "priority": 7},
                {"id": "iops", "head": "iops", "align": "end", "priority": 5},
                {"id": "util", "head": "util", "grow": 1, "priority": 4},
                {"id": "lat", "head": "lat", "align": "end", "priority": 2},
                {"id": "q", "head": "queue", "align": "end", "priority": 1},
            ]),
        );
        let lines = metric::TABLE_HEAD + metric::ROW * p.disk_rows as f64;
        c = c.child(scroller("dsk.scroll", lines).child(table));
    }
    if !app.mounts.is_empty() {
        // one meter line per mount, as in the ratatui panel
        let gauges = app.mounts.iter().map(|m| {
            let used = m.total.saturating_sub(m.available);
            gauge(
                &format!("dsk.m.{}", m.mount_point),
                &m.mount_point,
                12,
                used,
                m.total,
                true,
            )
        });
        c = c.child(scroller("dsk.mounts", p.mount_rows as f64).children(gauges));
    }
    if !app.smart.is_empty() {
        let items: Vec<Value> = app
            .smart
            .iter()
            .map(|s| {
                let mut v = Vec::new();
                if let Some(m) = &s.model {
                    v.push(sp(format!("{m} "), "muted"));
                }
                if let Some(t) = s.temp_c {
                    v.push(sp(format!("{t}°C "), ""));
                }
                match s.healthy {
                    Some(true) => v.push(sp("ok", "success")),
                    Some(false) => v.push(sp("FAIL", "error strong")),
                    None => {}
                }
                if let Some(w) = s.wear_pct {
                    v.push(sp(format!(" wear {w}%"), "muted"));
                }
                if let Some(h) = s.power_on_hours {
                    v.push(sp(format!(" {h}h"), "muted"));
                }
                json!({"k": s.device, "v": v})
            })
            .collect();
        c = c.child(Node::new("dsk.smart", "kv").prop("items", items));
    }
    if let Some(b) = &app.bench {
        c = c.child(bench(b));
    }
    // the aggregate io graph gets every line left, as in the ratatui panel
    if p.io_rows > 0 {
        c = c.child(graph(
            cx,
            "dsk.io",
            &app.disk_io,
            None,
            p.io_cells,
            p.io_rows,
            Ink::Series("io"),
        ));
    }
    c
}

fn bench(b: &BenchState) -> Node {
    let status = if b.error.is_some() {
        "error"
    } else if b.direct.is_some() {
        "done"
    } else {
        "running"
    };
    let mut c = Node::new("dsk.bench", "card")
        .prop("head", vec![sp("bench", "strong")])
        .prop("status", status)
        .prop("variant", "bare");
    if let Some(e) = &b.error {
        c = c.child(text(
            "dsk.bench.err",
            vec![sp(format!("bench error: {e}"), "error")],
        ));
    }
    if let Some((kind, done, bps)) = b.running {
        c = c.child(
            Node::new("dsk.bench.run", "progress")
                .prop("value", (done * 1000.0).round() / 1000.0)
                .prop(
                    "label",
                    vec![sp(
                        format!("{} {:>3.0}% {}", kind.label(), done * 100.0, rate(bps)),
                        "mono",
                    )],
                ),
        );
    }
    if !b.results.is_empty() {
        let mut items: Vec<Value> = b
            .results
            .iter()
            .map(|r| {
                let v = match r.p99_us {
                    Some(p99) => format!("{:.0}k iops  p99 {p99}µs", r.iops / 1e3),
                    None => rate(r.bytes_per_sec),
                };
                json!({"k": r.kind.label(), "v": v})
            })
            .collect();
        // direct:false means the page cache was in play — say so
        if b.direct == Some(false) {
            items.push(json!({"k": "cache", "v": [sp("page cache in play", "warning")]}));
        }
        c = c.child(Node::new("dsk.bench.res", "kv").prop("items", items));
    }
    c
}

/// `label  ▬▬▬▬▬▬▬  used / total` on one line; capacity gauges warn when full
fn gauge(id: &str, label: &str, label_w: usize, part: u64, whole: u64, capacity: bool) -> Node {
    let mut meter = Node::new(format!("{id}.m"), "meter")
        .prop("value", ratio(part, whole))
        .prop("label", humanize(part))
        .prop("total", format!("/ {}", humanize(whole)))
        .prop("grow", 1);
    if capacity {
        meter = meter.prop("thresholds", json!({"warn": 0.8, "bad": 0.95}));
    }
    let w = json!({"w": format!("{label_w}ch")});
    Node::new(id, "row")
        .prop("gap", "md")
        .child(
            text(format!("{id}.l"), vec![sp(label, "muted mono")])
                .prop("truncate", "middle")
                .prop("min", w.clone())
                .prop("max", w),
        )
        .child(meter)
}

fn mem(app: &App, p: &Plan) -> Node {
    let m = &app.mem;
    let mut gauges = vec![
        gauge("mem.used", "used", 5, m.used, m.total, true),
        gauge("mem.avail", "avail", 5, m.available, m.total, false),
    ];
    // compression pool before swap: macos compresses long before it swaps
    if m.compressed > 0 {
        gauges.push(gauge("mem.cmprs", "cmprs", 5, m.compressed, m.total, false));
    }
    if m.swap_total > 0 {
        gauges.push(gauge(
            "mem.swap",
            "swap",
            5,
            m.swap_used,
            m.swap_total,
            true,
        ));
    }
    // one child, so the lines sit flush without the card's row gap
    let lines = gauges.len().div_ceil(p.mem_cols);
    let mut rows = Node::new("mem.rows", "col");
    let mut gauges = gauges.into_iter();
    for l in 0..lines {
        let line = Node::new(format!("mem.l{l}"), "row")
            .prop("gap", "lg")
            .children(
                gauges
                    .by_ref()
                    .take(p.mem_cols)
                    .map(|g| g.prop("basis", 1.0 / p.mem_cols as f64).prop("grow", 1)),
            );
        rows = rows.child(line);
    }
    card("mem", vec![sp("mem", "strong")]).child(rows)
}

fn procs(app: &App, plan: &Plan) -> Node {
    let sort = match app.sort {
        SortBy::Cpu => "cpu",
        SortBy::Mem => "mem",
        SortBy::Io => "io",
        SortBy::Name => "name",
    };
    let mut head = vec![
        sp("proc", "strong"),
        sp(
            format!("  {} procs · sort {sort}", app.procs.len()),
            "muted",
        ),
    ];
    if app.tree {
        head.push(sp("  tree", "accent"));
    }
    if app.show_threads {
        head.push(sp("  threads", "accent"));
    }
    let mut c = card("proc", head);
    if app.filter_edit || !app.filter.is_empty() {
        c = c.child(
            Node::new(FILTER, "input")
                .prop("text", app.filter.clone())
                .prop("cursor", app.filter.encode_utf16().count())
                .prop("prompt", vec![sp("filter ", "muted")])
                .prop("placeholder", "name or pid")
                .prop("readonly", !app.filter_edit),
        );
    }
    // a lone item lays out like the list's rows, so the columns line up
    c = c.child(
        Node::new("proc.head", "item")
            .prop(
                "label",
                vec![
                    sp(format!("{:>7} ", "pid"), "muted mono"),
                    sp("name", "muted"),
                ],
            )
            .prop(
                "value",
                vec![sp(
                    format!("{:>9} {:>11} {:>7}", "mem", "io/s", "cpu%"),
                    "muted mono",
                )],
            ),
    );
    let empty = if app.filter.is_empty() {
        "no processes"
    } else {
        "no process matches"
    };
    // the list scrolls itself inside the rows the plan left it
    let mut list = Node::new(PROC_LIST, "list")
        .prop("max", json!({"lines": plan.proc_rows}))
        .prop("empty", vec![sp(empty, "muted")])
        .children(app.procs.iter().map(proc_item));
    if let Some(p) = app.procs.get(app.selected) {
        list = list.prop("selected", proc_id(p));
    }
    c.child(list)
}

fn proc_item(p: &ProcRow) -> Node {
    let thread = p.tid.is_some();
    // macos "tids" are 10-digit pthread handles that would blow the column;
    // the "tid N" name fallback still carries the identity
    let ident = match p.tid {
        None => format!("{:>7} ", p.pid),
        Some(t) if t <= 9_999_999 => format!("{t:>7} "),
        Some(_) => format!("{:>8}", ""),
    };
    let mut label = vec![sp(ident, "muted mono")];
    if !p.prefix.is_empty() {
        label.push(sp(p.prefix.clone(), "muted mono"));
    }
    label.push(sp(p.name.clone(), if thread { "muted" } else { "" }));
    let (mem, io) = match (thread, p.io_bps) {
        (true, _) => (String::new(), String::new()),
        (false, Some(v)) => (humanize(p.rss), rate(v)),
        (false, None) => (humanize(p.rss), "—".to_string()),
    };
    Node::new(proc_id(p), "item").prop("label", label).prop(
        "value",
        vec![
            sp(format!("{mem:>9} {io:>11} "), "muted mono"),
            sp(
                format!("{:>6.1}%", p.cpu_pct),
                &format!("mono {}", load_tone(p.cpu_pct)),
            ),
        ],
    )
}

/// key hints as clickable segments; a click presses the key
fn status(app: &App) -> Node {
    let seg = |key: char, label: &str, on: bool, priority: u32| {
        Node::new(format!("dock.k.{key}"), "seg")
            .prop(
                "spans",
                vec![
                    sp(key.to_string(), "key"),
                    sp(format!(" {label}"), if on { "accent" } else { "" }),
                ],
            )
            .prop("priority", priority)
            .prop("actions", json!({"click": format!("key={key}")}))
    };
    let upd = app.refresh_ms();
    let upd = if upd >= 1000 {
        format!("upd {:.1}s", upd as f64 / 1000.0)
    } else {
        format!("upd {upd}ms")
    };
    let step = |key: char, label: &str| {
        Node::new(format!("dock.k.{key}"), "seg")
            .prop(
                "spans",
                vec![
                    sp(key.to_string(), "strong mono"),
                    sp(format!(" {label}"), ""),
                ],
            )
            .prop("side", "right")
            .prop("priority", 3)
            .prop("actions", json!({"click": format!("key={key}")}))
    };
    Node::new("dock.st", "status").children([
        seg('q', "quit", false, 9),
        seg('c', "cpu", app.sort == SortBy::Cpu, 8),
        seg('m', "mem", app.sort == SortBy::Mem, 8),
        seg('i', "io", app.sort == SortBy::Io, 8),
        seg('n', "name", app.sort == SortBy::Name, 8),
        seg('f', "filter", app.filter_edit || !app.filter.is_empty(), 7),
        seg('k', "kill", false, 6),
        seg('t', "threads", app.show_threads, 4),
        seg('e', "tree", app.tree, 4),
        seg('h', "idle", app.show_idle, 3),
        seg('b', "bench", false, 5),
        seg('s', "system", false, 2),
        step('-', "faster"),
        Node::new("dock.upd", "seg")
            .prop("spans", vec![sp(upd, "muted mono")])
            .prop("side", "right")
            .prop("priority", 5),
        step('+', "slower"),
    ])
}

fn hint(id: &str, keys: &[(&str, &str)]) -> Node {
    let mut spans = Vec::new();
    for (i, (key, label)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(sp("   ", ""));
        }
        spans.push(sp(*key, "key"));
        spans.push(sp(format!(" {label}"), "muted"));
    }
    text(id, spans)
}

fn overlays(app: &App) -> Vec<Node> {
    let mut out = Vec::new();
    if let Some(p) = &app.picker {
        out.push(picker(p));
    }
    if let Some(kp) = &app.confirm_kill {
        out.push(kill(kp));
    }
    if let Some(fi) = &app.fetch {
        out.push(system(app, fi));
    }
    out
}

fn picker(p: &BenchPicker) -> Node {
    let items = p.entries.iter().enumerate().map(|(i, e)| {
        let label = match e.available {
            Some(_) => vec![sp(e.path.display().to_string(), "path")],
            None => vec![
                sp("temp dir ", ""),
                sp(e.path.display().to_string(), "path"),
            ],
        };
        Node::new(format!("{PICK_LIST}.{i}"), "item")
            .prop("label", label)
            .prop(
                "value",
                e.available
                    .map_or(String::new(), |a| format!("{} free", humanize(a))),
            )
    });
    Node::new("layer.pick", "overlay")
        .prop("modal", true)
        .prop("size", "md")
        .prop("head", "bench target")
        .child(
            Node::new(PICK_LIST, "list")
                .prop("selected", format!("{PICK_LIST}.{}", p.selected))
                .children(items),
        )
        .child(hint(
            "layer.pick.h",
            &[("enter", "runs"), ("esc", "closes")],
        ))
}

fn kill(kp: &KillPrompt) -> Node {
    Node::new("layer.kill", "overlay")
        .prop("modal", true)
        .prop("size", "sm")
        .prop("head", "kill?")
        .child(text(
            "layer.kill.t",
            vec![
                sp("SIGTERM ", "strong"),
                sp(format!("{} ({})", kp.name, kp.pid), ""),
            ],
        ))
        .child(hint(
            "layer.kill.h",
            &[("y", "confirms"), ("esc", "closes")],
        ))
}

fn system(app: &App, info: &FetchInfo) -> Node {
    let logo: Vec<String> = info
        .logo
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{}{l}\x1b[0m", sgr_fg(info.palette[i % info.palette.len()])))
        .collect();
    let items: Vec<Value> = info
        .lines
        .iter()
        .cloned()
        .chain(crate::fetch::live_lines(app))
        .map(|(k, v)| json!({"k": k, "v": v}))
        .collect();
    Node::new("layer.sys", "overlay")
        .prop("modal", true)
        .prop("size", "lg")
        .prop("head", "system")
        .child(
            Node::new("layer.sys.row", "row")
                .prop("gap", "lg")
                .prop("align", "start")
                // an ansi block wraps at its width; hold it at the art's widest
                // row, plus slack for the block's own inset
                .child(
                    Node::new("layer.sys.logo", "ansi")
                        .prop("text", logo.join("\n"))
                        .prop("min", json!({"w": format!("{}ch", logo_width(info) + 2)}))
                        .prop("shrink", 0),
                )
                .child(
                    Node::new("layer.sys.kv", "kv")
                        .prop("items", items)
                        .prop("grow", 1),
                ),
        )
        .child(hint("layer.sys.h", &[("esc", "closes")]))
}

fn logo_width(info: &FetchInfo) -> usize {
    info.logo
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
}

/// the logo palettes are terminal colors; `ansi` nodes draw them from Tern's theme
fn sgr_fg(c: Color) -> String {
    match c {
        Color::Indexed(n) => format!("\x1b[38;5;{n}m"),
        Color::Rgb(r, g, b) => format!("\x1b[38;2;{r};{g};{b}m"),
        Color::Black => "\x1b[30m".into(),
        Color::Red => "\x1b[31m".into(),
        Color::Green => "\x1b[32m".into(),
        Color::Yellow => "\x1b[33m".into(),
        Color::Blue => "\x1b[34m".into(),
        Color::Magenta => "\x1b[35m".into(),
        Color::Cyan => "\x1b[36m".into(),
        Color::Gray => "\x1b[37m".into(),
        Color::DarkGray => "\x1b[90m".into(),
        Color::LightRed => "\x1b[91m".into(),
        Color::LightGreen => "\x1b[92m".into(),
        Color::LightYellow => "\x1b[93m".into(),
        Color::LightBlue => "\x1b[94m".into(),
        Color::LightMagenta => "\x1b[95m".into(),
        Color::LightCyan => "\x1b[96m".into(),
        Color::White => "\x1b[97m".into(),
        Color::Reset => "\x1b[39m".into(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::collect::{
        CpuSnapshot, CpuTimes, DiskStats, MountInfo, NetIface, NetSnapshot, ProcessInfo, Snapshot,
        ThreadInfo,
    };

    const CX: Ctx = Ctx {
        cols: 160,
        rows: 50,
        palette: true,
    };

    fn row_text(row: &Node) -> String {
        row.p["spans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["t"].as_str().unwrap())
            .collect()
    }

    #[test]
    fn graph_rows_hold_exactly_their_cells_newest_at_the_right() {
        // a row one cell wider than planned would be cut with an ellipsis
        let hist: VecDeque<f64> = (0..500)
            .map(|i| if i == 499 { 100.0 } else { 0.0 })
            .collect();
        let g = graph(&CX, "g", &hist, Some(100.0), 40, 3, Ink::Load);
        assert_eq!(g.c.len(), 3);
        for row in &g.c {
            assert_eq!(row_text(row).chars().count(), 40);
        }
        // the newest sample fills the right dot column of the last cell, to the top
        let top = row_text(&g.c[0]);
        assert_eq!(top.chars().last(), Some('⢸'));
        let last = g.c[0].p["spans"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .clone();
        assert_eq!(last["s"], json!("g10"));
    }

    #[test]
    fn cpu_box_sits_over_the_middle_of_one_graph() {
        // the ratatui panel: one graph across the card, the core box over its
        // middle hiding those samples, older history out on the left
        let mut app = App::default();
        app.core_percents = vec![10.0; 10];
        app.load_avg = Some([1.0, 1.0, 1.0]);
        app.cpu_history = (0..crate::app::HISTORY).map(|_| 100.0).collect();
        let p = plan(&app, &CX);
        assert!(p.cores_mid);
        let (doc, _) = build(&app, &CX);
        let width = |id: &str| row_text(&find(&doc, id).unwrap().c[0]).chars().count();
        let (left, right) = (width("cpu.hl"), width("cpu.hr"));
        assert_eq!(left + p.box_cells + right, p.graph_cells);
        assert!(
            left.abs_diff(right) <= 1,
            "box off center: {left} | {right}"
        );
        assert!(row_text(&find(&doc, "cpu.hl").unwrap().c[0]).contains('⣿'));
    }

    fn snapshot(at: Instant, tick: u64) -> Box<Snapshot> {
        let proc = |pid: i32, ppid: i32, name: &str, threads: Vec<ThreadInfo>| ProcessInfo {
            pid,
            ppid,
            name: name.into(),
            cpu_ns: tick * pid as u64 * 10_000_000,
            rss: 1 << 20,
            disk_read: Some(tick << 12),
            disk_written: Some(0),
            threads,
        };
        Box::new(Snapshot {
            cpu: CpuSnapshot {
                total: CpuTimes {
                    busy: tick * 30,
                    idle: tick * 70,
                },
                per_core: vec![
                    CpuTimes {
                        busy: tick * 30,
                        idle: tick * 70
                    };
                    4
                ],
            },
            net: NetSnapshot {
                rx_bytes: tick << 20,
                tx_bytes: tick << 10,
                interfaces: vec![NetIface {
                    name: "en0".into(),
                    rx_bytes: tick << 20,
                    tx_bytes: tick << 10,
                }],
            },
            disks: vec![DiskStats {
                name: "disk0".into(),
                read_bytes: tick << 20,
                written_bytes: tick << 19,
                read_ops: tick * 10,
                write_ops: tick * 5,
                busy_time_ns: Some(tick * 100_000_000),
                io_time_ns: Some(tick * 50_000_000),
                weighted_ns: None,
            }],
            mounts: vec![MountInfo {
                mount_point: "/".into(),
                total: 100 << 30,
                available: 40 << 30,
            }],
            procs: vec![
                proc(1, 0, "launchd", Vec::new()),
                proc(
                    7,
                    1,
                    "rmon",
                    vec![ThreadInfo {
                        tid: 70,
                        name: "main".into(),
                        cpu_ns: tick,
                    }],
                ),
                proc(9, 1, "zsh", Vec::new()),
            ],
            taken: at + Duration::from_secs(tick),
            ..Default::default()
        })
    }

    fn live_app() -> App {
        let at = Instant::now();
        let mut app = App::default();
        app.on_event(AppEvent::Snapshot(snapshot(at, 1)));
        app.on_event(AppEvent::Snapshot(snapshot(at, 2)));
        app
    }

    fn ids(n: &Node, seen: &mut HashSet<String>) {
        assert!(seen.insert(n.id.clone()), "duplicate node id {}", n.id);
        n.c.iter().for_each(|c| ids(c, seen));
    }

    fn find<'a>(n: &'a Node, id: &str) -> Option<&'a Node> {
        if n.id == id {
            return Some(n);
        }
        n.c.iter().find_map(|c| find(c, id))
    }

    fn press_key(app: &mut App, c: char) {
        press(app, KeyCode::Char(c));
    }

    #[test]
    fn every_node_id_is_unique() {
        // Tern rejects an add whole when one id in it repeats
        let mut app = live_app();
        app.show_threads = true;
        app.select(1);
        app.on_event(AppEvent::Snapshot(snapshot(Instant::now(), 3)));
        app.picker = Some(BenchPicker {
            entries: vec![
                crate::app::BenchTarget {
                    path: "/".into(),
                    available: Some(1),
                },
                crate::app::BenchTarget {
                    path: "/tmp".into(),
                    available: None,
                },
            ],
            selected: 0,
        });
        app.confirm_kill = Some(KillPrompt {
            pid: 7,
            name: "rmon".into(),
        });
        app.fetch = Some(crate::fetch::collect());
        app.bench = Some(BenchState::default());
        let (doc, _) = build(&app, &CX);
        ids(&doc, &mut HashSet::new());
    }

    #[test]
    fn clicking_a_row_selects_that_process() {
        let mut app = live_app();
        let (doc, _) = build(&app, &CX);
        let list = find(&doc, PROC_LIST).unwrap();
        let target = &list.c[2];
        on_event(
            &mut app,
            Event::Select {
                id: PROC_LIST.into(),
                item: target.id.clone(),
            },
        );
        assert_eq!(app.selected, 2);
        let (doc, _) = build(&app, &CX);
        assert_eq!(
            find(&doc, PROC_LIST).unwrap().p["selected"],
            json!(target.id)
        );
    }

    #[test]
    fn thread_rows_select_by_pid_and_tid() {
        let mut app = live_app();
        let rmon = app.procs.iter().position(|p| p.pid == 7).unwrap();
        app.select(rmon);
        press_key(&mut app, 't');
        app.on_event(AppEvent::Snapshot(snapshot(Instant::now(), 3)));
        let t = app.procs.iter().position(|p| p.tid == Some(70)).unwrap();
        on_event(
            &mut app,
            Event::Select {
                id: PROC_LIST.into(),
                item: "proc.t7.70".into(),
            },
        );
        assert_eq!(app.selected, t);
    }

    #[test]
    fn hint_click_presses_the_key_even_mid_filter() {
        let mut app = live_app();
        press_key(&mut app, 'f');
        press_key(&mut app, 'z');
        assert!(app.filter_edit);
        on_event(
            &mut app,
            Event::Action {
                act: "key".into(),
                value: Some("e".into()),
            },
        );
        assert!(app.tree, "the click toggled the tree");
        assert_eq!(app.filter, "z", "and typed nothing into the filter");
    }

    #[test]
    fn filter_edit_puts_the_caret_in_the_field() {
        let mut app = live_app();
        assert_eq!(build(&app, &CX).1, None);
        press_key(&mut app, '/');
        let (doc, focus) = build(&app, &CX);
        assert_eq!(focus.as_deref(), Some(FILTER));
        assert!(find(&doc, FILTER).is_some());
    }

    #[test]
    fn picker_double_click_runs_that_target() {
        let mut app = live_app();
        press_key(&mut app, 'b');
        let n = app.picker.as_ref().unwrap().entries.len();
        on_event(
            &mut app,
            Event::Activate {
                id: PICK_LIST.into(),
                item: format!("{PICK_LIST}.{}", n - 1),
            },
        );
        assert!(app.picker.is_none());
        assert_eq!(app.bench_target, Some(std::env::temp_dir()));
    }
}
