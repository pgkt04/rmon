use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::fmt::duration_short;
use super::meter::meter;
use super::{BrailleGraph, theme};
use crate::app::App;
use crate::collect::BatteryState;

/// per-column width inside the core overlay: `c00 ⣿⣿⣀… 46.6%`
const CORE_COL_W: u16 = 24;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let total = app.cpu_history.back().copied().unwrap_or(0.0);
    // ` cpu Apple M1 Pro 44°C 14.8% up 3d 4h ` — parts render only when known
    let mut ident = String::new();
    if let Some(name) = &app.cpu_name {
        ident.push_str(name);
        ident.push(' ');
    }
    if let Some(t) = app.cpu_temp_c {
        ident.push_str(&format!("{t:.0}°C "));
    }
    let mut title = vec![Span::styled(
        " cpu ",
        Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
    )];
    if !ident.is_empty() {
        title.push(Span::styled(ident, Style::new().fg(theme::LABEL)));
    }
    title.push(Span::styled(
        format!("{total:5.1}% "),
        Style::new().fg(theme::gradient(total)),
    ));
    if let Some(up) = app.uptime_secs {
        title.push(Span::styled(
            format!("up {} ", duration_short(up)),
            Style::new().fg(theme::LABEL),
        ));
    }
    // btop-style battery: `BAT▼ 80% ⣿⣿⣿⣿⣿⣿⣿⣿⣀⣀ 2h 30m 12.3W`; ▲ charging
    // ▼ discharging ■ full ○ held on ac at zero current. absent on desktops
    if let Some(b) = app.battery {
        let sym = match b.state {
            BatteryState::Charging => '▲',
            BatteryState::Discharging => '▼',
            BatteryState::Full => '■',
            BatteryState::Unknown => '○',
        };
        title.push(Span::styled(
            format!("BAT{sym} {:.0}% ", b.percent),
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        ));
        // 10-cell meter in the house braille style, gradient reversed so
        // the low-charge end burns red
        const BAT_METER_W: usize = 10;
        for i in 1..=BAT_METER_W {
            let y = (i as f64 * 100.0 / BAT_METER_W as f64).round();
            let (glyph, style) = if b.percent >= y {
                ("⣿", Style::new().fg(theme::gradient(100.0 - y)))
            } else {
                ("⣀", Style::new().fg(theme::METER_EMPTY))
            };
            title.push(Span::styled(glyph, style));
        }
        let mut rest = String::new();
        if let Some(secs) = b.secs_left {
            rest.push_str(&format!(" {}", duration_short(secs)));
        }
        // idle on ac reads ~0 W; noise, not information
        if let Some(w) = b.watts.filter(|w| *w >= 0.05) {
            rest.push_str(&format!(" {w:.1}W"));
        }
        rest.push(' ');
        title.push(Span::styled(rest, Style::new().fg(theme::TITLE)));
    }
    // current refresh cadence, so +/- has visible feedback
    let upd = app.refresh_ms();
    let upd = if upd >= 1000 {
        format!("upd {:.1}s ", upd as f64 / 1000.0)
    } else {
        format!("upd {upd}ms ")
    };
    title.push(Span::styled(
        upd,
        Style::new().fg(theme::LABEL).add_modifier(Modifier::DIM),
    ));
    title.push(Span::styled(
        "[+/-] [s]ys ",
        Style::new().fg(theme::LABEL).add_modifier(Modifier::DIM),
    ));
    let title = Line::from(title);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(title);
    if let Some(err) = &app.status {
        block = block.title_bottom(Line::from(Span::styled(
            format!(" {err} "),
            Style::new().fg(Color::Red),
        )));
    }
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }

    // the history graph owns the whole box
    let vals: Vec<f64> = app.cpu_history.iter().copied().collect();
    f.render_widget(
        BrailleGraph {
            values: &vals,
            max: 100.0,
            style: Style::default(),
            gradient: true,
        },
        inner,
    );

    // core meters and load avg float in a box over the graph's right
    // side instead of stealing full-width rows under it
    overlay(f, app, inner);
}

fn overlay(f: &mut Frame, app: &App, inner: Rect) {
    let ncores = app.core_percents.len() as u16;
    let content_rows = ncores.div_ceil(2) + u16::from(app.load_avg.is_some());
    if content_rows == 0 {
        return;
    }
    // per-core temps take 5 extra cells per column when a sensor reports them
    let col = if app.core_temps_c.is_empty() {
        CORE_COL_W
    } else {
        CORE_COL_W + 5
    };
    let w = (col * 2 + 3).min(inner.width); // 2 cols + borders + gap
    let h = (content_rows + 2).min(inner.height);
    if w < col || h < 3 {
        return; // not enough room to say anything useful
    }
    // dead center; the graph flows right-to-left underneath and shows on
    // both sides of the box
    let area = Rect::new(
        inner.x + (inner.width - w) / 2,
        inner.y + (inner.height - h) / 2,
        w,
        h,
    );
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER));
    let box_inner = block.inner(area);
    f.render_widget(block, area);

    let rows = ncores.div_ceil(2);
    let col_w = box_inner.width.saturating_sub(1) / 2;
    let mut lines: Vec<Line> = Vec::new();
    for row in 0..rows {
        // column-major: c0..c4 left, c5..c9 right
        let mut spans = Vec::new();
        for col in 0..2u16 {
            let i = (row + col * rows) as usize;
            let Some(pct) = app.core_percents.get(i) else {
                continue;
            };
            if col == 1 {
                spans.push(Span::raw(" "));
            }
            // right text carries the temp when the sensor reports this core
            // (NaN = logical cpu with no mapped sensor -> stay blank)
            let text = match app.core_temps_c.get(i).filter(|t| !t.is_nan()) {
                Some(t) => format!("{pct:5.1}% {t:3.0}°"),
                None => format!("{pct:5.1}%"),
            };
            spans.extend(meter(&format!("c{i:02}"), *pct, &text, col_w).spans);
        }
        lines.push(Line::from(spans));
    }
    if let Some([one, five, fifteen]) = app.load_avg {
        lines.push(Line::from(Span::styled(
            format!("load {one:.2} {five:.2} {fifteen:.2}"),
            Style::new().fg(theme::LABEL),
        )));
    }
    f.render_widget(Paragraph::new(lines), box_inner);
}
