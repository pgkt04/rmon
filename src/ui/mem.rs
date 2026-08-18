use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use super::fmt::humanize;
use super::meter::meter;
use super::theme;
use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(Line::from(Span::styled(
            " mem ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 1 {
        return;
    }

    let pct = |part: u64| {
        if app.mem.total == 0 {
            0.0
        } else {
            part as f64 * 100.0 / app.mem.total as f64
        }
    };
    let spct = if app.mem.swap_total == 0 {
        0.0
    } else {
        app.mem.swap_used as f64 * 100.0 / app.mem.swap_total as f64
    };
    let lines = vec![
        meter(
            "used ",
            pct(app.mem.used),
            &format!(
                "{:>9} / {}",
                humanize(app.mem.used),
                humanize(app.mem.total)
            ),
            inner.width,
        ),
        meter(
            "avail",
            pct(app.mem.available),
            &format!(
                "{:>9} / {}",
                humanize(app.mem.available),
                humanize(app.mem.total)
            ),
            inner.width,
        ),
        // swap always shows; 0 B / 0 B means the host has none allocated
        // (macos grows swap on demand, so the row appearing later would jump
        // the layout)
        meter(
            "swap ",
            spct,
            &format!(
                "{:>9} / {}",
                humanize(app.mem.swap_used),
                humanize(app.mem.swap_total)
            ),
            inner.width,
        ),
    ];
    f.render_widget(Paragraph::new(lines), inner);
}
