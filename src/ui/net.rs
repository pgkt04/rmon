use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};
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

pub fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    let vis = app.visible_net().len();
    let hidden = app.net_ifaces.len() - vis;
    let rx = app.net_rx.back().copied().unwrap_or(0.0);
    let tx = app.net_tx.back().copied().unwrap_or(0.0);
    let title = Line::from(vec![
        Span::styled(
            " net ",
            Style::new().fg(theme::TITLE).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if hidden > 0 {
                format!("{vis} ifaces (+{hidden} idle, [h] shows) ")
            } else {
                format!("{vis} ifaces [h]idle ")
            },
            Style::new().fg(theme::LABEL),
        ),
        // aggregate rate now, cumulative since boot in parens
        Span::styled(
            format!("↓ {} ({}) ", rate(rx), humanize(app.net_rx_total)),
            Style::new().fg(RX),
        ),
        Span::styled(
            format!("↑ {} ({}) ", rate(tx), humanize(app.net_tx_total)),
            Style::new().fg(TX),
        ),
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

    // per-interface rows only; the aggregate story lives in the title, so
    // each row's own sparkline gets the space instead of a shared graph
    let cap = (inner.height as usize).max(1);
    app.net_rows_cap = cap;
    app.net_offset = app.net_offset.min(vis.saturating_sub(cap));
    let offset = app.net_offset;
    let rows = app.visible_net();
    if rows.is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no active interfaces",
                Style::new().fg(theme::LABEL),
            ))),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
    }
    // name + rates text, then rx/tx sparklines fill the rest of the row
    const TEXT_W: u16 = 12 + 24 + 2;
    for (n, i) in rows.iter().skip(offset).take(cap).enumerate() {
        let row = Rect::new(inner.x, inner.y + n as u16, inner.width, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{:<12}", i.name), Style::new().fg(theme::TITLE)),
                Span::styled(
                    format!("↓ {:>9}  ↑ {:>9}", rate(i.rx_bps), rate(i.tx_bps)),
                    Style::new().fg(theme::LABEL),
                ),
            ])),
            row,
        );
        // sparklines only when there is real room for two of them
        let free = row.width.saturating_sub(TEXT_W);
        if free < 20 {
            continue;
        }
        let Some((rx_h, tx_h)) = app.net_hist.get(&i.name) else {
            continue;
        };
        let half = free / 2;
        let rx_area = Rect::new(row.x + TEXT_W, row.y, half.saturating_sub(1), 1);
        let tx_area = Rect::new(row.x + TEXT_W + half, row.y, half.saturating_sub(1), 1);
        graph(f, rx_h, RX, rx_area);
        graph(f, tx_h, TX, tx_area);
    }
    if let Some(track) = super::rows_scrollbar(area, inner.y, cap, rows.len()) {
        super::draw_scrollbar(f, track, rows.len(), offset);
    }
}
