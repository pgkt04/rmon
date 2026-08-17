use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use super::theme;
use crate::app::KillPrompt;

/// small centered popup; one line of content
pub fn popup_rect(frame: Rect, kp: &KillPrompt) -> Rect {
    // wide enough for the prompt line AND the 39-char title, clamped to the frame
    let want = (kp.name.len() + 24) as u16;
    let w = want.clamp(41, 70).min(frame.width);
    let h = 3.min(frame.height);
    Rect::new(
        frame.x + (frame.width.saturating_sub(w)) / 2,
        frame.y + (frame.height.saturating_sub(h)) / 2,
        w,
        h,
    )
}

pub fn draw(f: &mut Frame, kp: &KillPrompt, frame: Rect) {
    let area = popup_rect(frame, kp);
    f.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::TITLE))
        .title(Line::from(Span::styled(
            " kill? ─ y/enter confirms, esc closes ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let line = Line::from(vec![
        Span::styled(
            " SIGTERM ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ({})", kp.name, kp.pid),
            Style::new().fg(theme::LABEL),
        ),
    ]);
    f.render_widget(Paragraph::new(line), inner);
}
