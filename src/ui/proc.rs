use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use super::fmt::{humanize, rate};
use super::theme;
use crate::app::{App, SortBy};

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let sort = match app.sort {
        SortBy::Cpu => "cpu",
        SortBy::Mem => "mem",
        SortBy::Io => "io",
        SortBy::Name => "name",
    };
    let title = Line::from(vec![
        Span::styled(
            " proc ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{} procs, sort {sort} [c/m/i/n] [k]ill [f]ilter [t]hreads ",
                app.procs.len()
            ),
            Style::new().fg(theme::LABEL),
        ),
    ]);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(title);
    // filter readout on the bottom border; a block cursor marks live input
    if app.filter_edit || !app.filter.is_empty() {
        let cursor = if app.filter_edit { "█" } else { "" };
        block = block.title_bottom(Line::from(Span::styled(
            format!(" filter: {}{cursor} ", app.filter),
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        )));
    }
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
    let offset = app.scroll_viewport(visible);
    for (i, p) in app.procs.iter().enumerate().skip(offset).take(visible) {
        let thread = p.tid.is_some();
        let mut name = if thread {
            let glyph = if p.last_child { "└─" } else { "├─" };
            format!("{glyph} {}", p.name)
        } else {
            p.name.clone()
        };
        // char-boundary-safe cut; byte truncate panics on non-ascii names
        if name.len() > name_w {
            name = name.chars().take(name_w).collect();
        }
        // thread rows: tid in the pid column, blank mem/io (widths kept so
        // nothing shifts), dimmer name. macos "tids" are 10-digit pthread
        // handles that would blow the column: blank them, the name row
        // fallback ("tid N") still carries the identity
        let id = match p.tid {
            None => format!("{:>7} ", p.pid),
            Some(t) if t <= 9_999_999 => format!("{t:>7} "),
            Some(_) => format!("{:>7} ", ""),
        };
        let name_fg = if thread { theme::LABEL } else { theme::TITLE };
        let row = Line::from(vec![
            Span::styled(id, Style::new().fg(theme::LABEL)),
            Span::styled(format!("{name:<name_w$} "), Style::new().fg(name_fg)),
            Span::styled(
                if thread {
                    format!("{:>9} ", "")
                } else {
                    format!("{:>9} ", humanize(p.rss))
                },
                Style::new().fg(theme::LABEL),
            ),
            Span::styled(
                match (thread, p.io_bps) {
                    (true, _) => format!("{:>11} ", ""),
                    (false, Some(v)) => format!("{:>11} ", rate(v)),
                    (false, None) => format!("{:>11} ", "—"),
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

    // scroll position over the right border, only when the list overflows
    if let Some(track) = super::scrollbar_rect(area, app.procs.len()) {
        let mut state = ScrollbarState::new(app.procs.len()).position(app.selected);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(Some("░"))
                .thumb_symbol("█")
                .track_style(Style::new().fg(theme::BORDER))
                .thumb_style(Style::new().fg(theme::TITLE)),
            track,
            &mut state,
        );
    }
}
