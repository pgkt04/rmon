mod confirm;
mod cpu;
mod dsk;
mod fmt;
mod graph;
mod mem;
mod meter;
mod net;
mod picker;
mod proc;
mod theme;

pub use graph::BrailleGraph;

use ratatui::Frame;
use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Constraint, Layout, Position, Rect};

use crate::app::App;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [cpu_area, net_area, dsk_area, proc_area, mem_area] = panels(f.area(), app);
    cpu::draw(f, app, cpu_area);
    net::draw(f, app, net_area);
    dsk::draw(f, app, dsk_area);
    proc::draw(f, app, proc_area);
    mem::draw(f, app, mem_area);
    if let Some(p) = &app.picker {
        picker::draw(f, p, f.area());
    }
    if let Some(kp) = &app.confirm_kill {
        confirm::draw(f, kp, f.area());
    }
}

/// one source of truth for the frame layout; the mouse handler hit-tests with it
fn panels(area: Rect, app: &App) -> [Rect; 5] {
    // meters + borders; swap meter only exists when the host has swap
    let mem_rows = if app.mem.swap_total > 0 { 5 } else { 4 };
    let [cpu_area, net_area, band, mem_area] = Layout::vertical([
        // a 0-100 scaled graph in a huge box is mostly blank at idle; keep
        // the cpu box short and dense
        Constraint::Percentage(30),
        // per-interface rows on top, aggregate graph below; tall enough for
        // a couple interfaces plus a graph on typical terminals
        Constraint::Length(12),
        Constraint::Fill(3),
        Constraint::Length(mem_rows),
    ])
    .areas(area);
    // dsk and proc share the tall middle band; the split keeps
    // the proc name column from swimming in blank space on wide terminals
    let [dsk_area, proc_area] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(band);
    [cpu_area, net_area, dsk_area, proc_area, mem_area]
}

/// right border column, one row in from each corner; None when the list fits
fn scrollbar_rect(proc_area: Rect, n_procs: usize) -> Option<Rect> {
    // border 2 + header 1, same math as the proc draw
    let visible = (proc_area.height as usize).saturating_sub(3);
    if n_procs <= visible {
        return None;
    }
    Some(Rect {
        x: proc_area.right() - 1,
        y: proc_area.y + 1,
        width: 1,
        height: proc_area.height - 2,
    })
}

/// map a pointer row on the scrollbar track to a proc index
fn scroll_to(app: &mut App, row: u16, track: Rect) {
    let len = app.procs.len();
    if len < 2 || track.height < 2 {
        return;
    }
    // pointer may be above/below the track mid-drag; pin it first
    let row = row.clamp(track.y, track.y + track.height - 1);
    app.select((((row - track.y) as usize * (len - 1)) / (track.height - 1) as usize).min(len - 1));
}

/// right-border track for a row list starting at screen row `y` with `cap`
/// visible rows; None while everything fits
fn rows_scrollbar(panel: Rect, y: u16, cap: usize, n: usize) -> Option<Rect> {
    (n > cap && cap > 0).then(|| Rect {
        x: panel.right() - 1,
        y,
        width: 1,
        height: cap as u16,
    })
}

/// the one scrollbar look, shared by every panel
fn draw_scrollbar(f: &mut Frame, track: Rect, n: usize, pos: usize) {
    use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
    let mut state = ScrollbarState::new(n).position(pos);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("░"))
            .thumb_symbol("█")
            .track_style(ratatui::style::Style::new().fg(theme::BORDER))
            .thumb_style(ratatui::style::Style::new().fg(theme::TITLE)),
        track,
        &mut state,
    );
}

/// map a pointer row on a viewport track to a row offset (top row index)
fn offset_for(row: u16, track: Rect, n: usize, cap: usize) -> usize {
    let span = n.saturating_sub(cap);
    if span == 0 || track.height < 2 {
        return 0;
    }
    let row = row.clamp(track.y, track.y + track.height - 1);
    ((row - track.y) as usize * span)
        .div_ceil((track.height - 1) as usize)
        .min(span)
}
/// wheel scrolls the proc list, click selects a row, the scrollbar drags
pub fn handle_mouse(app: &mut App, m: MouseEvent, frame: Rect) {
    // the picker is modal: wheel moves, click runs a row, click outside closes
    if let Some(p) = &mut app.picker {
        let n = p.entries.len();
        let area = picker::popup_rect(frame, n);
        match m.kind {
            MouseEventKind::ScrollUp => p.selected = p.selected.saturating_sub(1),
            MouseEventKind::ScrollDown => {
                p.selected = (p.selected + 1).min(n.saturating_sub(1));
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if !area.contains(Position::new(m.column, m.row)) {
                    app.picker = None;
                    return;
                }
                let first = area.y + 1;
                if m.row >= first && m.row + 1 < area.y + area.height {
                    let idx = (m.row - first) as usize;
                    if idx < n {
                        let target = p.entries[idx].path.clone();
                        app.picker = None;
                        app.bench = Some(crate::app::BenchState::default());
                        app.bench_target = Some(target);
                    }
                }
            }
            _ => {}
        }
        return;
    }
    // the kill prompt is modal too: a click outside backs out, everything
    // else is keyboard-only so a stray click cannot confirm a kill
    if let Some(kp) = &app.confirm_kill {
        if let MouseEventKind::Down(MouseButton::Left) = m.kind
            && !confirm::popup_rect(frame, kp).contains(Position::new(m.column, m.row))
        {
            app.confirm_kill = None;
        }
        return;
    }
    let [_, net_area, dsk_area, proc_area, _] = panels(frame, app);
    // drag/release can wander outside the panel, so handle them before the hit-test
    match m.kind {
        MouseEventKind::Drag(MouseButton::Left) if app.drag_scroll => {
            if let Some(track) = scrollbar_rect(proc_area, app.procs.len()) {
                scroll_to(app, m.row, track);
            }
            return;
        }
        MouseEventKind::Drag(MouseButton::Left) if app.drag_net => {
            let n = app.visible_net().len();
            if let Some(track) = rows_scrollbar(net_area, net_area.y + 1, app.net_rows_cap, n) {
                app.net_offset = offset_for(m.row, track, n, app.net_rows_cap);
            }
            return;
        }
        MouseEventKind::Drag(MouseButton::Left) if app.drag_dsk => {
            let n = app.visible_disks().len();
            if let Some(track) = rows_scrollbar(dsk_area, dsk_area.y + 1, app.dsk_rows_cap, n) {
                app.dsk_offset = offset_for(m.row, track, n, app.dsk_rows_cap);
            }
            return;
        }
        MouseEventKind::Up(MouseButton::Left) => {
            app.drag_scroll = false;
            app.drag_net = false;
            app.drag_dsk = false;
            return;
        }
        _ => {}
    }
    // net and dsk rows have no selection; the wheel moves their viewport and
    // the border track drags it
    if net_area.contains(Position::new(m.column, m.row)) {
        let n = app.visible_net().len();
        let max = n.saturating_sub(app.net_rows_cap);
        match m.kind {
            MouseEventKind::ScrollUp => app.net_offset = app.net_offset.saturating_sub(1),
            MouseEventKind::ScrollDown => app.net_offset = (app.net_offset + 1).min(max),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(track) = rows_scrollbar(net_area, net_area.y + 1, app.net_rows_cap, n)
                    && track.contains(Position::new(m.column, m.row))
                {
                    app.drag_net = true;
                    app.net_offset = offset_for(m.row, track, n, app.net_rows_cap);
                }
            }
            _ => {}
        }
        return;
    }
    if dsk_area.contains(Position::new(m.column, m.row)) {
        let n = app.visible_disks().len();
        let max = n.saturating_sub(app.dsk_rows_cap);
        match m.kind {
            MouseEventKind::ScrollUp => app.dsk_offset = app.dsk_offset.saturating_sub(1),
            MouseEventKind::ScrollDown => app.dsk_offset = (app.dsk_offset + 1).min(max),
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(track) = rows_scrollbar(dsk_area, dsk_area.y + 1, app.dsk_rows_cap, n)
                    && track.contains(Position::new(m.column, m.row))
                {
                    app.drag_dsk = true;
                    app.dsk_offset = offset_for(m.row, track, n, app.dsk_rows_cap);
                }
            }
            _ => {}
        }
        return;
    }
    if !proc_area.contains(Position::new(m.column, m.row)) {
        return;
    }
    let last = app.procs.len().saturating_sub(1);
    match m.kind {
        MouseEventKind::ScrollUp => app.select(app.selected.saturating_sub(1)),
        MouseEventKind::ScrollDown => app.select((app.selected + 1).min(last)),
        MouseEventKind::Down(MouseButton::Left) => {
            // grabbing the scrollbar must not select the row under it
            if let Some(track) = scrollbar_rect(proc_area, app.procs.len())
                && track.contains(Position::new(m.column, m.row))
            {
                app.drag_scroll = true;
                scroll_to(app, m.row, track);
                return;
            }
            // inner starts one cell in (border), first content line is the header
            let first_row_y = proc_area.y + 2;
            // last content row sits just above the bottom border
            if m.row < first_row_y || m.row + 1 >= proc_area.y + proc_area.height {
                return;
            }
            // map through the offset the last frame actually drew, not a
            // recomputed one: the list churns every snapshot and a derived
            // offset can disagree with what the user was aiming at
            let idx = app.view_offset + (m.row - first_row_y) as usize;
            if idx < app.procs.len() {
                app.select(idx);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{DiskRow, ProcRow};
    use crate::collect::{MemSnapshot, MountInfo};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    pub(super) fn fake_app() -> App {
        let mut app = App::default();
        app.cpu_history = (0..120)
            .map(|i| (i as f64 * 0.35).sin().abs() * 85.0)
            .collect();
        app.core_percents = vec![42.4, 49.0, 32.7, 22.0, 13.0, 9.0, 8.0, 2.9, 1.0, 0.0];
        app.mem = MemSnapshot {
            total: 16 << 30,
            used: 10 << 30,
            available: 6 << 30,
            swap_total: 4 << 30,
            swap_used: 3 << 30,
        };
        app.net_rx = (0..60).map(|i| (i % 10) as f64 * 200_000.0).collect();
        app.net_tx = (0..60).map(|i| (i % 7) as f64 * 60_000.0).collect();
        app.procs = vec![
            ProcRow {
                pid: 4242,
                name: "kernel_task".into(),
                cpu_pct: 42.0,
                rss: 1 << 30,
                io_bps: Some(2.0 * 1024.0 * 1024.0),
                tid: None,
                ppid: 0,
                prefix: String::new(),
            },
            ProcRow {
                pid: 777,
                name: "rmon".into(),
                cpu_pct: 1.5,
                rss: 8 << 20,
                io_bps: None,
                tid: None,
                ppid: 0,
                prefix: String::new(),
            },
        ];
        app.disk_io = (0..60).map(|i| (i % 8) as f64 * 3_000_000.0).collect();
        app.disks = vec![DiskRow {
            name: "disk0".into(),
            read_bps: 12.0 * 1024.0 * 1024.0,
            write_bps: 3.0 * 1024.0 * 1024.0,
            iops: 240.0,
            util_pct: Some(37.5),
            queue: None,
            lat_ms: Some(0.42),
            idle_secs: 0.0,
        }];
        app.mounts = vec![MountInfo {
            mount_point: "/".into(),
            total: 1 << 40,
            available: 600 << 30,
        }];
        app.bench = Some(crate::app::BenchState {
            running: Some((crate::bench::TestKind::SeqWrite, 0.5, 2e9)),
            results: vec![],
            error: None,
            direct: None,
        });
        app.cpu_name = Some("Apple M1 Pro".into());
        app.cpu_temp_c = Some(44.2);
        app.gpu_name = Some("Apple M1 Pro".into());
        app.gpu_util_pct = Some(22.0);
        app.core_temps_c = vec![41.0, 43.5, 44.0, 42.2, 40.9, 39.0, 38.5, 38.0, 37.7, 37.2];
        app.net_ifaces = vec![
            crate::app::NetRow {
                name: "en0".into(),
                rx_bps: (1 << 20) as f64,
                tx_bps: (400 << 10) as f64,
                idle_secs: 0.0,
            },
            crate::app::NetRow {
                name: "utun3".into(),
                rx_bps: (8 << 10) as f64,
                tx_bps: (2 << 10) as f64,
                idle_secs: 0.0,
            },
        ];
        app.net_hist = [("en0", 400_000.0), ("utun3", 4_000.0)]
            .into_iter()
            .map(|(n, base)| {
                let wave = |k: u64| (0..60).map(|i| ((i * k) % 9) as f64 * base).collect();
                (n.to_string(), (wave(3), wave(5)))
            })
            .collect();
        app.net_rx_total = 12 << 30;
        app.net_tx_total = 800 << 20;
        app.load_avg = Some([2.14, 1.82, 1.53]);
        app.uptime_secs = Some(93_784);
        app
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn frame_shows_all_panels() {
        // wide enough that the dsk rows (45% of the band) show every segment
        let backend = TestBackend::new(190, 46);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = fake_app();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains(" cpu "));
        assert!(text.contains("c00"));
        assert!(text.contains("c09"));
        assert!(text.contains(" net "));
        assert!(text.contains("en0"));
        assert!(text.contains("↓"));
        assert!(text.contains("(12.0 GiB)"));
        assert!(text.contains("(800.0 MiB)"));
        assert!(text.contains("↑"));
        assert!(text.contains(" dsk "));
        assert!(text.contains("disk0"));
        assert!(text.contains("iops"));
        assert!(text.contains("bench seq write"));
        assert!(text.contains(" proc "));
        assert!(text.contains("kernel_task"));
        assert!(text.contains("4242"));
        assert!(text.contains("io/s"));
        assert!(text.contains("2.0 MiB/s"));
        assert!(text.contains(" mem "));
        assert!(text.contains("10.0 GiB / 16.0 GiB"));
        assert!(text.contains("swap"));
        assert!(text.contains("3.0 GiB / 4.0 GiB"));
        assert!(text.contains('⣿'));
    }

    #[test]
    fn net_rows_carry_their_own_sparklines() {
        let backend = TestBackend::new(190, 46);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = fake_app();
        term.draw(|f| draw(f, &mut app)).unwrap();
        // find the en0 row and check for braille cells past the rates text
        let buf = term.backend().buffer();
        let [_, net_area, _, _, _] = panels(Rect::new(0, 0, 190, 46), &app);
        let mut found = false;
        for y in net_area.y + 1..net_area.y + net_area.height - 1 {
            let line: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            if line.contains("en0") {
                let tail: String = line.chars().skip(40).collect();
                found = tail.chars().any(|c| ('\u{2800}'..='\u{28FF}').contains(&c));
            }
        }
        assert!(found, "no braille sparkline on the en0 row");
    }

    /// enough interfaces that the net rows overflow their 4-row window
    fn crowded_net_app() -> App {
        let mut app = fake_app();
        app.net_ifaces = (0..10)
            .map(|i| crate::app::NetRow {
                name: format!("en{i}"),
                rx_bps: 1000.0,
                tx_bps: 1000.0,
                idle_secs: 0.0,
            })
            .collect();
        app
    }

    #[test]
    fn net_wheel_scrolls_and_scrollbar_drags_the_viewport() {
        let mut app = crowded_net_app();
        let frame = Rect::new(0, 0, 80, 40);
        // draw records the row capacity the mouse maps against
        let backend = TestBackend::new(80, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let [_, net_area, _, _, _] = panels(frame, &app);
        assert!(app.net_rows_cap > 0 && app.net_rows_cap < 10);

        // wheel inside the panel moves the viewport, clamped at the overflow
        let (cx, cy) = (net_area.x + 2, net_area.y + 2);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, cx, cy), frame);
        assert_eq!(app.net_offset, 1);
        for _ in 0..20 {
            handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, cx, cy), frame);
        }
        assert_eq!(app.net_offset, 10 - app.net_rows_cap, "clamped");
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, cx, cy), frame);
        assert_eq!(app.net_offset, 10 - app.net_rows_cap - 1);

        // grab the top of the border track: jump to 0 and start a drag
        let track =
            rows_scrollbar(net_area, net_area.y + 1, app.net_rows_cap, 10).expect("overflows");
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), track.x, track.y),
            frame,
        );
        assert!(app.drag_net);
        assert_eq!(app.net_offset, 0);
        // drag to the bottom: max offset, even off-column
        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                0,
                track.y + track.height - 1,
            ),
            frame,
        );
        assert_eq!(app.net_offset, 10 - app.net_rows_cap);
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Up(MouseButton::Left), 0, 0),
            frame,
        );
        assert!(!app.drag_net);
    }

    #[test]
    fn dsk_wheel_scrolls_its_viewport() {
        let mut app = fake_app();
        app.disks = (0..12)
            .map(|i| DiskRow {
                name: format!("disk{i}"),
                read_bps: 1.0,
                write_bps: 1.0,
                iops: 1.0,
                util_pct: None,
                queue: None,
                lat_ms: None,
                idle_secs: 0.0,
            })
            .collect();
        let frame = Rect::new(0, 0, 80, 40);
        let backend = TestBackend::new(80, 40);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let [_, _, dsk_area, _, _] = panels(frame, &app);
        assert!(app.dsk_rows_cap > 0 && app.dsk_rows_cap < 12);
        let (cx, cy) = (dsk_area.x + 2, dsk_area.y + 2);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, cx, cy), frame);
        assert_eq!(app.dsk_offset, 1);
        for _ in 0..30 {
            handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, cx, cy), frame);
        }
        assert_eq!(app.dsk_offset, 12 - app.dsk_rows_cap, "clamped");
    }

    fn mouse(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        }
    }

    #[test]
    fn wheel_scrolls_proc_selection() {
        let mut app = fake_app();
        let frame = Rect::new(0, 0, 80, 40);
        let [_, _, _, proc_area, _] = panels(frame, &app);
        let (cx, cy) = (proc_area.x + 2, proc_area.y + 2);
        assert_eq!(app.selected, 0);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, cx, cy), frame);
        assert_eq!(app.selected, 1);
        // clamped at the last row
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, cx, cy), frame);
        assert_eq!(app.selected, app.procs.len() - 1);
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollUp, cx, cy), frame);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn wheel_outside_proc_is_ignored() {
        let mut app = fake_app();
        let frame = Rect::new(0, 0, 80, 40);
        // cpu panel starts at the top
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 2, 1), frame);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn click_selects_row_and_borders_do_not() {
        let mut app = fake_app();
        let frame = Rect::new(0, 0, 80, 40);
        let [_, _, _, proc_area, _] = panels(frame, &app);
        let down = MouseEventKind::Down(MouseButton::Left);
        // second visible row
        handle_mouse(
            &mut app,
            mouse(down, proc_area.x + 2, proc_area.y + 3),
            frame,
        );
        assert_eq!(app.selected, 1);
        // header line is not a row
        handle_mouse(
            &mut app,
            mouse(down, proc_area.x + 2, proc_area.y + 1),
            frame,
        );
        assert_eq!(app.selected, 1);
        // click below the list is ignored (only 2 fake procs)
        handle_mouse(
            &mut app,
            mouse(down, proc_area.x + 2, proc_area.y + 5),
            frame,
        );
        assert_eq!(app.selected, 1);
    }

    #[test]
    fn click_maps_through_the_drawn_offset_not_the_selection() {
        let mut app = overflowing_app();
        let frame = Rect::new(0, 0, 80, 40);
        let [_, _, _, proc_area, _] = panels(frame, &app);
        // the last frame drew rows 10.. while the selection sits deep at 50
        app.selected = 50;
        app.view_offset = 10;
        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                proc_area.x + 2,
                proc_area.y + 2,
            ),
            frame,
        );
        assert_eq!(app.selected, 10, "first visible row is what was clicked");
    }

    /// enough procs that the proc panel scrolls at any sane frame size
    fn overflowing_app() -> App {
        let mut app = fake_app();
        app.procs = (0..100)
            .map(|i| ProcRow {
                pid: i,
                name: format!("p{i}"),
                cpu_pct: 0.0,
                rss: 0,
                io_bps: None,
                tid: None,
                ppid: 0,
                prefix: String::new(),
            })
            .collect();
        app
    }

    #[test]
    fn kill_prompt_swallows_mouse_and_click_outside_closes() {
        let mut app = overflowing_app();
        let frame = Rect::new(0, 0, 80, 40);
        app.confirm_kill = Some(crate::app::KillPrompt {
            pid: 1,
            name: "p1".into(),
        });
        let area = confirm::popup_rect(frame, app.confirm_kill.as_ref().unwrap());
        // wheel must not scroll the list behind the modal
        handle_mouse(&mut app, mouse(MouseEventKind::ScrollDown, 2, 2), frame);
        assert_eq!(app.selected, 0);
        // a click inside is inert: only y/enter may confirm
        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x + 1,
                area.y + 1,
            ),
            frame,
        );
        assert!(app.confirm_kill.is_some());
        // a click outside backs out
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), 0, 0),
            frame,
        );
        assert!(app.confirm_kill.is_none());
        assert_eq!(app.selected, 0, "the closing click must not select a row");
    }

    #[test]
    fn scrollbar_rect_matches_draw_position() {
        let frame = Rect::new(0, 0, 80, 40);
        let app = overflowing_app();
        let [_, _, _, proc_area, _] = panels(frame, &app);
        let track = scrollbar_rect(proc_area, app.procs.len()).unwrap();
        // right border column, one row in from each corner (matches the draw margin)
        assert_eq!(
            track,
            Rect::new(
                proc_area.right() - 1,
                proc_area.y + 1,
                1,
                proc_area.height - 2
            )
        );
        // border 2 + header 1; the bar only exists once the list overflows
        let visible = (proc_area.height as usize).saturating_sub(3);
        assert!(scrollbar_rect(proc_area, visible).is_none());
        assert!(scrollbar_rect(proc_area, visible + 1).is_some());
    }

    #[test]
    fn scrollbar_click_jumps_selection() {
        let mut app = overflowing_app();
        let frame = Rect::new(0, 0, 80, 40);
        let [_, _, _, proc_area, _] = panels(frame, &app);
        let track = scrollbar_rect(proc_area, app.procs.len()).unwrap();
        let down = MouseEventKind::Down(MouseButton::Left);
        app.selected = 5;
        handle_mouse(&mut app, mouse(down, track.x, track.y), frame);
        assert_eq!(app.selected, 0);
        assert!(app.drag_scroll);
        handle_mouse(
            &mut app,
            mouse(down, track.x, track.y + track.height - 1),
            frame,
        );
        assert_eq!(app.selected, app.procs.len() - 1);
    }

    #[test]
    fn scrollbar_grab_is_not_a_row_click() {
        let mut app = overflowing_app();
        let frame = Rect::new(0, 0, 80, 40);
        let [_, _, _, proc_area, _] = panels(frame, &app);
        let track = scrollbar_rect(proc_area, app.procs.len()).unwrap();
        let len = app.procs.len();
        // second track row sits on the first data row; a row click would pick
        // index 0, the track ratio jumps much further down the list
        let row = track.y + 1;
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), track.x, row),
            frame,
        );
        let mapped = (row - track.y) as usize * (len - 1) / (track.height - 1) as usize;
        assert_eq!(app.selected, mapped);
        assert_ne!(app.selected, 0);
    }

    #[test]
    fn scrollbar_drag_follows_row_and_release_stops_it() {
        let mut app = overflowing_app();
        let frame = Rect::new(0, 0, 80, 40);
        let [_, _, _, proc_area, _] = panels(frame, &app);
        let track = scrollbar_rect(proc_area, app.procs.len()).unwrap();
        let len = app.procs.len();
        let drag = MouseEventKind::Drag(MouseButton::Left);
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Down(MouseButton::Left), track.x, track.y),
            frame,
        );
        assert!(app.drag_scroll);
        let mid = track.y + track.height / 2;
        handle_mouse(&mut app, mouse(drag, track.x, mid), frame);
        assert_eq!(
            app.selected,
            (mid - track.y) as usize * (len - 1) / (track.height - 1) as usize
        );
        // pointer wanders off the column mid-drag; the row still drives it
        handle_mouse(&mut app, mouse(drag, 0, track.y + track.height - 1), frame);
        assert_eq!(app.selected, len - 1);
        // above the track clamps to the top
        handle_mouse(&mut app, mouse(drag, 0, 0), frame);
        assert_eq!(app.selected, 0);
        // release outside the panel still ends the drag
        handle_mouse(
            &mut app,
            mouse(MouseEventKind::Up(MouseButton::Left), 0, 0),
            frame,
        );
        assert!(!app.drag_scroll);
        handle_mouse(
            &mut app,
            mouse(drag, track.x, track.y + track.height - 1),
            frame,
        );
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn drag_without_grab_is_ignored() {
        let mut app = overflowing_app();
        let frame = Rect::new(0, 0, 80, 40);
        let [_, _, _, proc_area, _] = panels(frame, &app);
        let track = scrollbar_rect(proc_area, app.procs.len()).unwrap();
        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Drag(MouseButton::Left),
                track.x,
                track.y + track.height - 1,
            ),
            frame,
        );
        assert_eq!(app.selected, 0);
        assert!(!app.drag_scroll);
    }

    #[test]
    fn right_border_click_ignored_when_list_fits() {
        let mut app = fake_app(); // two procs, no scrollbar
        let frame = Rect::new(0, 0, 80, 40);
        let [_, _, _, proc_area, _] = panels(frame, &app);
        assert!(scrollbar_rect(proc_area, app.procs.len()).is_none());
        // a scrollbar grab here would jump to the last proc; a row click lands
        // below the two rows and is ignored either way
        handle_mouse(
            &mut app,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                proc_area.right() - 1,
                proc_area.y + proc_area.height - 2,
            ),
            frame,
        );
        assert_eq!(app.selected, 0);
        assert!(!app.drag_scroll);
    }

    #[test]
    fn cpu_title_shows_identity_and_gpu_meter() {
        let backend = TestBackend::new(100, 46);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = fake_app();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&term);
        let title_row = text.lines().next().unwrap();
        assert!(title_row.contains("Apple M1 Pro"), "{title_row}");
        assert!(title_row.contains("44°C"), "{title_row}");
        assert!(title_row.contains("up 1d 2h"), "{title_row}");
        // gpu meter row: label plus `{name} {util:.1}%` right text
        assert!(text.contains("gpu "));
        assert!(text.contains("Apple M1 Pro 22.0%"));
        assert!(text.contains("load 2.14 1.82 1.53"));
        // per-core temp rides on the core meter text
        assert!(text.contains("41°"));
    }

    #[test]
    fn cpu_panel_hides_missing_sensors() {
        let backend = TestBackend::new(100, 46);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = fake_app();
        app.cpu_name = None;
        app.cpu_temp_c = None;
        app.gpu_name = None;
        app.gpu_util_pct = None;
        app.load_avg = None;
        app.uptime_secs = None;
        term.draw(|f| draw(f, &mut app)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains(" cpu "));
        assert!(!text.contains("°C"));
        assert!(!text.contains("gpu"));
        assert!(!text.contains("load "));
        assert!(!text.contains("up "));
    }

    /// the proc panel's right border column, borders excluded (scrollbar track)
    fn proc_right_column(term: &Terminal<TestBackend>, proc_area: Rect) -> Vec<String> {
        let buf = term.backend().buffer();
        let x = proc_area.x + proc_area.width - 1;
        (proc_area.y + 1..proc_area.y + proc_area.height - 1)
            .map(|y| buf[(x, y)].symbol().to_string())
            .collect()
    }

    #[test]
    fn proc_scrollbar_thumb_tracks_selection() {
        let backend = TestBackend::new(100, 46);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = fake_app();
        app.procs = (0..200)
            .map(|i| ProcRow {
                pid: i,
                name: format!("p{i}"),
                cpu_pct: 0.0,
                rss: 0,
                io_bps: None,
                tid: None,
                ppid: 0,
                prefix: String::new(),
            })
            .collect();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let [_, _, _, proc_area, _] = panels(Rect::new(0, 0, 100, 46), &app);
        let col = proc_right_column(&term, proc_area);
        assert!(col.iter().any(|s| s == "█"), "no thumb: {col:?}");
        assert!(col.iter().any(|s| s == "░"), "no track: {col:?}");
        assert_eq!(col.first().map(String::as_str), Some("█"), "{col:?}");

        app.selected = app.procs.len() - 1;
        term.draw(|f| draw(f, &mut app)).unwrap();
        let col = proc_right_column(&term, proc_area);
        assert_eq!(col.last().map(String::as_str), Some("█"), "{col:?}");
        assert_ne!(col.first().map(String::as_str), Some("█"), "{col:?}");
    }

    #[test]
    fn proc_scrollbar_absent_when_list_fits() {
        let backend = TestBackend::new(100, 46);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = fake_app(); // two procs, plenty of rows
        term.draw(|f| draw(f, &mut app)).unwrap();
        let [_, _, _, proc_area, _] = panels(Rect::new(0, 0, 100, 46), &app);
        let col = proc_right_column(&term, proc_area);
        assert!(col.iter().all(|s| s != "█" && s != "░"), "{col:?}");
    }
}

#[cfg(test)]
mod picker_tests {
    use super::tests::fake_app;
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent};

    fn open_picker(app: &mut App) {
        app.on_event(crate::app::AppEvent::Key(KeyEvent::from(KeyCode::Char(
            'b',
        ))));
        assert!(app.picker.is_some());
    }

    #[test]
    fn picker_popup_renders_entries_over_panels() {
        let backend = TestBackend::new(100, 46);
        let mut term = Terminal::new(backend).unwrap();
        let mut app = fake_app();
        app.bench = None; // fake_app has a running bench; clear so b opens the picker
        open_picker(&mut app);
        term.draw(|f| draw(f, &mut app)).unwrap();
        let text: String = {
            let buf = term.backend().buffer();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(text.contains("bench target"), "popup title renders");
        assert!(text.contains("free"), "mount rows show free space");
        assert!(text.contains("temp dir"), "fallback entry present");
    }

    #[test]
    fn picker_wheel_moves_and_click_starts() {
        let frame = Rect::new(0, 0, 100, 46);
        let mut app = fake_app();
        app.bench = None;
        open_picker(&mut app);
        let n = app.picker.as_ref().unwrap().entries.len();
        let area = picker::popup_rect(frame, n);
        let wheel = |kind| MouseEvent {
            kind,
            column: area.x + 2,
            row: area.y + 1,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        };
        handle_mouse(&mut app, wheel(MouseEventKind::ScrollDown), frame);
        assert_eq!(app.picker.as_ref().unwrap().selected, 1);
        handle_mouse(&mut app, wheel(MouseEventKind::ScrollUp), frame);
        assert_eq!(app.picker.as_ref().unwrap().selected, 0);
        // click on the second row starts a bench there
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x + 2,
            row: area.y + 2,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        };
        handle_mouse(&mut app, click, frame);
        assert!(app.picker.is_none());
        assert!(app.bench_target.is_some());
    }

    #[test]
    fn picker_click_outside_closes() {
        let frame = Rect::new(0, 0, 100, 46);
        let mut app = fake_app();
        app.bench = None;
        open_picker(&mut app);
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        };
        handle_mouse(&mut app, click, frame);
        assert!(app.picker.is_none());
        assert!(app.bench_target.is_none());
    }
}
