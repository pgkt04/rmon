use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::fmt::{duration_short, humanize};
use super::theme;
use crate::app::App;
use crate::fetch::FetchInfo;

/// upper bound on live_lines rows; popup_rect sizes against it so the rect
/// never depends on App state the mouse handler can't see
const MAX_LIVE_LINES: usize = 4;

fn live_lines(app: &App) -> Vec<(String, String)> {
    let mut out = Vec::with_capacity(MAX_LIVE_LINES);
    if let Some(c) = &app.cpu_name {
        out.push(("cpu".into(), c.clone()));
    }
    if let Some(g) = &app.gpu_name {
        out.push(("gpu".into(), g.clone()));
    }
    if app.mem.total > 0 {
        out.push((
            "memory".into(),
            format!("{} / {}", humanize(app.mem.used), humanize(app.mem.total)),
        ));
    }
    if let Some(u) = app.uptime_secs {
        out.push(("uptime".into(), duration_short(u)));
    }
    out
}

fn logo_width(info: &FetchInfo) -> usize {
    info.logo
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
}

/// centered, sized to logo + info columns; deterministic from info alone so
/// the mouse handler and draw always agree. live values just clip if huge
pub fn popup_rect(frame: Rect, info: &FetchInfo) -> Rect {
    let info_w = info
        .lines
        .iter()
        .map(|(l, v)| l.chars().count() + 2 + v.chars().count())
        .max()
        .unwrap_or(0)
        .max(30); // room for the live memory/cpu rows
    let w = ((logo_width(info) + 2 + info_w + 2) as u16).min(frame.width);
    let rows = info.logo.len().max(info.lines.len() + MAX_LIVE_LINES);
    let h = (rows as u16 + 2).min(frame.height);
    Rect::new(
        frame.x + (frame.width.saturating_sub(w)) / 2,
        frame.y + (frame.height.saturating_sub(h)) / 2,
        w,
        h,
    )
}

pub fn draw(f: &mut Frame, app: &App, info: &FetchInfo, frame: Rect) {
    let area = popup_rect(frame, info);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::TITLE))
        .title(Line::from(Span::styled(
            " system ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let mut rows = info.lines.clone();
    rows.extend(live_lines(app));
    let logo_w = logo_width(info);
    let n = info.logo.len().max(rows.len());
    let lines: Vec<Line> = (0..n)
        .map(|i| {
            let raw = info.logo.get(i).copied().unwrap_or("");
            let pad = logo_w - raw.chars().count() + 2;
            let color = info.palette[i % info.palette.len()];
            let mut spans = vec![Span::styled(
                format!("{raw}{:pad$}", ""),
                Style::new().fg(color),
            )];
            if let Some((label, value)) = rows.get(i) {
                spans.push(Span::styled(
                    format!("{label}: "),
                    Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
                ));
                spans.push(Span::styled(value.clone(), Style::new().fg(theme::LABEL)));
            }
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
