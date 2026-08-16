use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType};
use std::collections::VecDeque;

use super::fmt::{humanize, rate};
use super::{BrailleGraph, theme};
use crate::app::App;

const RX: Color = Color::Rgb(120, 200, 255);
const TX: Color = Color::Rgb(255, 170, 110);

fn graph(f: &mut Frame, hist: &VecDeque<f64>, color: Color, area: Rect) {
    let vals: Vec<f64> = hist.iter().copied().collect();
    // auto-scale to the visible peak; 1 KiB/s floor keeps idle graphs flat
    let max = vals.iter().copied().fold(1024.0_f64, f64::max) * 1.1;
    f.render_widget(
        BrailleGraph {
            values: &vals,
            max,
            style: Style::new().fg(color),
            gradient: false,
        },
        area,
    );
}

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let rx = app.net_rx.back().copied().unwrap_or(0.0);
    let tx = app.net_tx.back().copied().unwrap_or(0.0);
    let mut title = vec![Span::styled(
        " net ",
        Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
    )];
    if let Some(iface) = &app.net_iface {
        title.push(Span::styled(
            format!("{iface} "),
            Style::new().fg(theme::LABEL),
        ));
    }
    // rate now, cumulative since boot in parens
    title.push(Span::styled(
        format!("↓ {} ({}) ", rate(rx), humanize(app.net_rx_total)),
        Style::new().fg(RX),
    ));
    title.push(Span::styled(
        format!("↑ {} ({}) ", rate(tx), humanize(app.net_tx_total)),
        Style::new().fg(TX),
    ));
    let title = Line::from(title);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme::BORDER))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 1 {
        return;
    }
    // download stacks over upload at full width; side-by-side halves
    // read as one confusing band
    let [rx_area, tx_area] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(inner);
    graph(f, &app.net_rx, RX, rx_area);
    graph(f, &app.net_tx, TX, tx_area);
}
