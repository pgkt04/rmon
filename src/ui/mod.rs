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

pub fn draw(f: &mut Frame, app: &App) {
    let [cpu_area, net_area, dsk_area, proc_area, mem_area] = panels(f.area(), app);
    cpu::draw(f, app, cpu_area);
    net::draw(f, app, net_area);
    dsk::draw(f, app, dsk_area);
    proc::draw(f, app, proc_area);
    mem::draw(f, app, mem_area);
    if let Some(p) = &app.picker {
        picker::draw(f, p, f.area());
    }
}

/// one source of truth for the frame layout; the mouse handler hit-tests with it
fn panels(area: Rect, app: &App) -> [Rect; 5] {
    // dsk panel height must reserve a row for every line the panel pushes:
    // disks, mounts, smart rows, and width-wrapped bench lines
    let inner = Rect::new(0, 0, area.width.saturating_sub(2), 1);
    let [rows_area, _] = dsk::columns(inner);
    let bench_rows = app
        .bench
        .as_ref()
        .map_or(0, |b| dsk::bench_rows(b, rows_area.width));
    let content = app.disks.len() + app.mounts.len() + app.smart.len() + bench_rows;
    let dsk_rows = content.clamp(1, 8) as u16 + 2;
    // meters + borders; swap meter only exists when the host has swap
    let mem_rows = if app.mem.swap_total > 0 { 5 } else { 4 };
    Layout::vertical([
        Constraint::Fill(2),
        Constraint::Length(6),
        Constraint::Length(dsk_rows),
        Constraint::Fill(3),
        Constraint::Length(mem_rows),
    ])
    .areas(area)
}

/// wheel scrolls the proc list, click selects a row - btop style
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
    let [_, _, _, proc_area, _] = panels(frame, app);
    if !proc_area.contains(Position::new(m.column, m.row)) {
        return;
    }
    let last = app.procs.len().saturating_sub(1);
    match m.kind {
        MouseEventKind::ScrollUp => app.selected = app.selected.saturating_sub(1),
        MouseEventKind::ScrollDown => app.selected = (app.selected + 1).min(last),
        MouseEventKind::Down(MouseButton::Left) => {
            // inner starts one cell in (border), first content line is the header
            let first_row_y = proc_area.y + 2;
            // last content row sits just above the bottom border
            if m.row < first_row_y || m.row + 1 >= proc_area.y + proc_area.height {
                return;
            }
            let visible = (proc_area.height as usize).saturating_sub(3);
            let offset = app.selected.saturating_sub(visible.saturating_sub(1));
            let idx = offset + (m.row - first_row_y) as usize;
            if idx < app.procs.len() {
                app.selected = idx;
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
            },
            ProcRow {
                pid: 777,
                name: "rmon".into(),
                cpu_pct: 1.5,
                rss: 8 << 20,
                io_bps: None,
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
        let backend = TestBackend::new(100, 46);
        let mut term = Terminal::new(backend).unwrap();
        let app = fake_app();
        term.draw(|f| draw(f, &app)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains(" cpu "));
        assert!(text.contains("c00"));
        assert!(text.contains("c09"));
        assert!(text.contains(" net "));
        assert!(text.contains("↓"));
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
    fn cpu_title_shows_identity_and_gpu_meter() {
        let backend = TestBackend::new(100, 46);
        let mut term = Terminal::new(backend).unwrap();
        let app = fake_app();
        term.draw(|f| draw(f, &app)).unwrap();
        let text = buffer_text(&term);
        let title_row = text.lines().next().unwrap();
        assert!(title_row.contains("Apple M1 Pro"), "{title_row}");
        assert!(title_row.contains("44°C"), "{title_row}");
        // gpu meter row: label plus `{name} {util:.1}%` right text
        assert!(text.contains("gpu "));
        assert!(text.contains("Apple M1 Pro 22.0%"));
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
        term.draw(|f| draw(f, &app)).unwrap();
        let text = buffer_text(&term);
        assert!(text.contains(" cpu "));
        assert!(!text.contains("°C"));
        assert!(!text.contains("gpu"));
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
            })
            .collect();
        term.draw(|f| draw(f, &app)).unwrap();
        let [_, _, _, proc_area, _] = panels(Rect::new(0, 0, 100, 46), &app);
        let col = proc_right_column(&term, proc_area);
        assert!(col.iter().any(|s| s == "█"), "no thumb: {col:?}");
        assert!(col.iter().any(|s| s == "░"), "no track: {col:?}");
        assert_eq!(col.first().map(String::as_str), Some("█"), "{col:?}");

        app.selected = app.procs.len() - 1;
        term.draw(|f| draw(f, &app)).unwrap();
        let col = proc_right_column(&term, proc_area);
        assert_eq!(col.last().map(String::as_str), Some("█"), "{col:?}");
        assert_ne!(col.first().map(String::as_str), Some("█"), "{col:?}");
    }

    #[test]
    fn proc_scrollbar_absent_when_list_fits() {
        let backend = TestBackend::new(100, 46);
        let mut term = Terminal::new(backend).unwrap();
        let app = fake_app(); // two procs, plenty of rows
        term.draw(|f| draw(f, &app)).unwrap();
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
        term.draw(|f| draw(f, &app)).unwrap();
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
