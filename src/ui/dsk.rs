use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use super::fmt::{humanize, rate};
use super::meter::meter;
use super::{BrailleGraph, theme};
use crate::app::{App, BenchState};

const IO: Color = Color::Rgb(190, 140, 255);

fn opt(v: Option<f64>, f: impl Fn(f64) -> String) -> String {
    v.map(f).unwrap_or_else(|| "    —".into())
}

/// bench result strings greedily packed into lines of at most `width` columns;
/// shared by the renderer and the row budgets so reserved rows and rendered
/// rows never disagree (a lone part longer than `width` still gets one line)
pub(super) fn bench_lines(b: &BenchState, width: u16) -> Vec<String> {
    if b.results.is_empty() {
        return Vec::new();
    }
    let mut parts: Vec<(&str, String)> = b
        .results
        .iter()
        .map(|r| match r.p99_us {
            Some(p99) => {
                let s = format!("{} {:.0}k iops p99 {}µs", r.kind.label(), r.iops / 1e3, p99);
                (" | ", s)
            }
            None => (
                " | ",
                format!("{} {}", r.kind.label(), rate(r.bytes_per_sec)),
            ),
        })
        .collect();
    // direct:false means the page cache was in play — say so
    if b.direct == Some(false) {
        parts.push((" ", "(cached)".to_string()));
    }
    let width = usize::from(width.max(1));
    let mut lines = Vec::new();
    let mut cur = String::from("bench:");
    for (sep, part) in parts {
        let sep = if cur.ends_with(':') { " " } else { sep };
        if cur.chars().count() + sep.len() + part.chars().count() <= width {
            cur.push_str(sep);
            cur.push_str(&part);
        } else {
            lines.push(cur);
            cur = part;
        }
    }
    lines.push(cur);
    lines
}

/// panel rows the bench state occupies at `width`: live/error line + wrapped results
pub(super) fn bench_rows(b: &BenchState, width: u16) -> usize {
    usize::from(b.running.is_some() || b.error.is_some()) + bench_lines(b, width).len()
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let io_now = app.disk_io.back().copied().unwrap_or(0.0);
    let title = Line::from(vec![
        Span::styled(
            " dsk ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("io {} ", rate(io_now)), Style::new().fg(IO)),
        Span::styled("[b]ench ", Style::new().fg(theme::LABEL)),
    ]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 1 {
        return;
    }

    let rows_width = inner.width;

    let mut lines = Vec::new();
    // mounts, smart, and bench lines render after disks; cap disk rows so they
    // are never pushed below the panel on hosts with many synthetic disks
    let benched = app.bench.as_ref().map_or(0, |b| bench_rows(b, rows_width));
    let disk_budget = (inner.height as usize)
        .saturating_sub(app.mounts.len() + app.smart.len() + benched)
        .max(usize::from(!app.disks.is_empty()));
    // segments drop off as the panel narrows instead of clipping mid-number
    let w = rows_width as usize;
    for d in app.disks.iter().take(disk_budget) {
        let mut spans = vec![
            Span::styled(format!("{:<8}", d.name), Style::new().fg(theme::TITLE)),
            Span::styled(
                format!(" R {:>11} W {:>11}", rate(d.read_bps), rate(d.write_bps)),
                Style::new().fg(theme::LABEL),
            ),
        ];
        if w >= 47 {
            spans.push(Span::styled(
                format!(" {:>7.0} iops", d.iops),
                Style::new().fg(theme::LABEL),
            ));
        }
        if w >= 54 {
            spans.push(match d.util_pct {
                Some(u) => Span::styled(format!(" {u:>5.1}%"), Style::new().fg(theme::gradient(u))),
                None => Span::styled("     —".to_string(), Style::new().fg(theme::LABEL)),
            });
        }
        if w >= 71 {
            spans.push(Span::styled(
                format!(
                    " lat {} q {}",
                    opt(d.lat_ms, |v| format!("{v:>5.2}ms")),
                    opt(d.queue, |v| format!("{v:>4.1}")),
                ),
                Style::new().fg(theme::LABEL),
            ));
        }
        lines.push(Line::from(spans));
    }
    for m in &app.mounts {
        let used = m.total.saturating_sub(m.available);
        let pct = if m.total == 0 {
            0.0
        } else {
            used as f64 * 100.0 / m.total as f64
        };
        let mut label = m.mount_point.clone();
        if label.len() > 12 {
            label = label.chars().take(12).collect();
        }
        lines.push(meter(
            &format!("{label:<12}"),
            pct,
            &format!("{:>9} / {}", humanize(used), humanize(m.total)),
            rows_width,
        ));
    }
    for s in &app.smart {
        let mut head = s.device.clone();
        if let Some(m) = &s.model {
            head.push(' ');
            head.push_str(m);
        }
        if let Some(t) = s.temp_c {
            head.push_str(&format!(" {t}°C"));
        }
        let mut spans = vec![Span::styled(head, Style::new().fg(theme::LABEL))];
        match s.healthy {
            Some(true) => spans.push(Span::styled(" ok", Style::new().fg(theme::LABEL))),
            Some(false) => spans.push(Span::styled(" FAIL", Style::new().fg(Color::Red))),
            None => {}
        }
        let mut tail = String::new();
        if let Some(w) = s.wear_pct {
            tail.push_str(&format!(" wear {w}%"));
        }
        if let Some(h) = s.power_on_hours {
            tail.push_str(&format!(" {h}h"));
        }
        if !tail.is_empty() {
            spans.push(Span::styled(tail, Style::new().fg(theme::LABEL)));
        }
        lines.push(Line::from(spans));
    }
    if let Some(b) = &app.bench {
        if let Some(e) = &b.error {
            lines.push(Line::from(Span::styled(
                format!("bench error: {e}"),
                Style::new().fg(Color::Red),
            )));
        } else if let Some((kind, frac, bps)) = b.running {
            lines.push(Line::from(Span::styled(
                format!(
                    "bench {} {:>3.0}% {}",
                    kind.label(),
                    frac * 100.0,
                    rate(bps)
                ),
                Style::new().fg(IO),
            )));
        }
        for text in bench_lines(b, rows_width) {
            lines.push(Line::from(Span::styled(
                text,
                Style::new().fg(theme::TITLE),
            )));
        }
    }
    // rows sit on top; the aggregate io graph gets every leftover row below,
    // at full width - a tall skinny side column reads as noise
    let used = (lines.len() as u16).min(inner.height);
    let rows_area = Rect::new(inner.x, inner.y, inner.width, used);
    f.render_widget(Paragraph::new(lines), rows_area);
    let graph_area = Rect::new(
        inner.x,
        inner.y + used,
        inner.width,
        inner.height.saturating_sub(used),
    );
    if graph_area.height > 0 {
        let vals: Vec<f64> = app.disk_io.iter().copied().collect();
        let max = vals.iter().copied().fold(1024.0_f64, f64::max) * 1.1;
        f.render_widget(
            BrailleGraph {
                values: &vals,
                max,
                style: Style::new().fg(IO),
                gradient: false,
            },
            graph_area,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::{App, BenchState};
    use crate::bench::{TestKind, TestResult};
    use crate::smart::SmartInfo;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(width: u16, height: u16, app: &App) -> String {
        let backend = TestBackend::new(width, height);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, app, f.area())).unwrap();
        let buf = term.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                text.push_str(buf[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn smart_disk0(healthy: bool) -> SmartInfo {
        SmartInfo {
            device: "disk0".into(),
            model: Some("APPLE SSD".into()),
            healthy: Some(healthy),
            temp_c: Some(31),
            wear_pct: Some(4),
            power_on_hours: Some(2145),
        }
    }

    #[test]
    fn smart_row_renders() {
        let mut app = App::default();
        app.smart = vec![smart_disk0(true)];
        let text = render(90, 8, &app);
        assert!(text.contains("APPLE SSD"), "missing model: {text}");
        assert!(text.contains("31°C"), "missing temp: {text}");
        assert!(text.contains("ok"), "missing health: {text}");
    }

    #[test]
    fn unhealthy_is_marked() {
        let mut app = App::default();
        app.smart = vec![smart_disk0(false)];
        let text = render(90, 8, &app);
        assert!(text.contains("FAIL"), "missing FAIL: {text}");
    }

    #[test]
    fn bench_results_wrap() {
        let res = |kind, bps: f64, iops: f64, p99| TestResult {
            kind,
            bytes_per_sec: bps,
            iops,
            p50_us: None,
            p99_us: p99,
        };
        let mut app = App::default();
        app.bench = Some(BenchState {
            running: None,
            results: vec![
                res(TestKind::SeqWrite, 2.0e9, 0.0, None),
                res(TestKind::SeqRead, 2.4e9, 0.0, None),
                res(TestKind::RandRead, 0.0, 180_000.0, Some(85)),
                res(TestKind::RandWrite, 0.0, 95_000.0, Some(120)),
            ],
            error: None,
            direct: Some(false),
        });
        // 60 wide -> ~35-col rows column: the joined line cannot fit, must wrap
        let text = render(60, 10, &app);
        for label in ["seq write", "seq read", "rand read", "rand write"] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
        assert!(text.contains("(cached)"), "missing cached marker: {text}");
    }

    #[test]
    fn bench_lines_fit_width_and_match_row_budget() {
        let b = BenchState {
            running: None,
            results: vec![
                TestResult {
                    kind: TestKind::SeqWrite,
                    bytes_per_sec: 2.0e9,
                    iops: 0.0,
                    p50_us: None,
                    p99_us: None,
                },
                TestResult {
                    kind: TestKind::RandRead,
                    bytes_per_sec: 0.0,
                    iops: 180_000.0,
                    p50_us: None,
                    p99_us: Some(85),
                },
            ],
            error: None,
            direct: Some(false),
        };
        for width in [10u16, 20, 35, 80, 200] {
            let lines = bench_lines(&b, width);
            assert!(!lines.is_empty());
            for l in &lines {
                // only a lone unsplittable part may exceed the width
                assert!(
                    l.chars().count() <= usize::from(width) || !l.contains(" | "),
                    "packed line overflows {width}: {l:?}"
                );
            }
            assert_eq!(
                bench_rows(&b, width),
                lines.len(),
                "row budget disagrees with rendered lines at width {width}"
            );
        }
        // wide enough: everything packs onto the single classic line
        assert_eq!(bench_lines(&b, 200).len(), 1);
        assert!(bench_lines(&b, 200)[0].ends_with("(cached)"));
    }
}
