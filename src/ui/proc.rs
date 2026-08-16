use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use super::fmt::{humanize, rate};
use super::theme;
use crate::app::{App, SortBy};

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let sort = match app.sort {
        SortBy::Cpu => "cpu",
        SortBy::Mem => "mem",
        SortBy::Io => "io",
    };
    let title = Line::from(vec![
        Span::styled(
            " proc ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} procs, sort {sort} [c/m/i] ", app.procs.len()),
            Style::new().fg(theme::LABEL),
        ),
    ]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }

    // pid(7) + spaces + mem(9) + io(12) + cpu%(7) columns; name gets the rest
    let name_w = (inner.width as usize)
        .saturating_sub(7 + 1 + 9 + 7 + 12 + 2)
        .max(8);
    let mut lines = vec![Line::from(Span::styled(
        format!(
            "{:>7} {:<name_w$} {:>9} {:>11} {:>6}%",
            "pid", "name", "mem", "io/s", "cpu"
        ),
        Style::new().fg(theme::LABEL).add_modifier(Modifier::BOLD),
    ))];

    let visible = inner.height as usize - 1;
    let offset = app.selected.saturating_sub(visible.saturating_sub(1));
    for (i, p) in app.procs.iter().enumerate().skip(offset).take(visible) {
        let mut name = p.name.clone();
        // char-boundary-safe cut; byte truncate panics on non-ascii names
        if name.len() > name_w {
            name = name.chars().take(name_w).collect();
        }
        let row = Line::from(vec![
            Span::styled(format!("{:>7} ", p.pid), Style::new().fg(theme::LABEL)),
            Span::styled(format!("{name:<name_w$} "), Style::new().fg(theme::TITLE)),
            Span::styled(
                format!("{:>9} ", humanize(p.rss)),
                Style::new().fg(theme::LABEL),
            ),
            Span::styled(
                match p.io_bps {
                    Some(v) => format!("{:>11} ", rate(v)),
                    None => format!("{:>11} ", "—"),
                },
                Style::new().fg(theme::LABEL),
            ),
            Span::styled(
                format!("{:>6.1}%", p.cpu_pct),
                Style::new().fg(theme::gradient(p.cpu_pct)),
            ),
        ]);
        let row = if i == app.selected {
            row.style(Style::new().bg(theme::SELECTED_BG))
        } else {
            row
        };
        lines.push(row);
    }
    f.render_widget(Paragraph::new(lines), inner);

    // btop-style scroll position over the right border, only when the list overflows
    if app.procs.len() > visible {
        let mut state = ScrollbarState::new(app.procs.len()).position(app.selected);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("░"))
                .thumb_symbol("█")
                .track_style(Style::new().fg(theme::BORDER))
                .thumb_style(Style::new().fg(theme::TITLE)),
            area.inner(Margin::new(0, 1)),
            &mut state,
        );
    }
}
