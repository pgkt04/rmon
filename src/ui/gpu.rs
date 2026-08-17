use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};

use super::{BrailleGraph, theme};
use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    // the layout hands us a zero-height area on gpu-less hosts
    if area.height == 0 {
        return;
    }
    let util = app
        .gpu_util_pct
        .or_else(|| app.gpu_hist.back().copied())
        .unwrap_or(0.0);
    let mut title = vec![Span::styled(
        " gpu ",
        Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
    )];
    if let Some(name) = &app.gpu_name {
        title.push(Span::styled(
            format!("{name} "),
            Style::new().fg(theme::LABEL),
        ));
    }
    title.push(Span::styled(
        format!("{util:5.1}% "),
        Style::new().fg(theme::gradient(util)),
    ));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(Line::from(title));
    let inner = block.inner(area);
    f.render_widget(block, area);
    // fixed 0..100 scale: it's a percentage, peak-scaling would just lie
    let vals: Vec<f64> = app.gpu_hist.iter().copied().collect();
    f.render_widget(
        BrailleGraph {
            values: &vals,
            max: 100.0,
            style: Style::default(),
            gradient: true,
        },
        inner,
    );
}
