use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::fmt::humanize;
use super::theme;
use crate::app::BenchPicker;

/// centered popup rect sized to the entries; clamps to the frame
pub fn popup_rect(frame: Rect, entries: usize) -> Rect {
    let w = (frame.width * 6 / 10).clamp(24, 70).min(frame.width);
    let h = (entries as u16 + 2).min(frame.height);
    Rect::new(
        frame.x + (frame.width.saturating_sub(w)) / 2,
        frame.y + (frame.height.saturating_sub(h)) / 2,
        w,
        h,
    )
}

pub fn draw(f: &mut Frame, p: &BenchPicker, frame: Rect) {
    let area = popup_rect(frame, p.entries.len());
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::TITLE))
        .title(Line::from(Span::styled(
            " bench target ─ enter runs, esc closes ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let lines: Vec<Line> = p
        .entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let text = match e.available {
                Some(avail) => format!("{}  {} free", e.path.display(), humanize(avail)),
                None => format!("temp dir ({})", e.path.display()),
            };
            let style = if i == p.selected {
                Style::new()
                    .fg(theme::TITLE)
                    .bg(theme::SELECTED_BG)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(theme::LABEL)
            };
            Line::from(Span::styled(format!(" {text} "), style))
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}
