use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Paragraph};

use super::meter::meter;
use super::{BrailleGraph, theme};
use crate::app::App;

pub fn draw(f: &mut Frame, app: &App, area: Rect) {
    let total = app.cpu_history.back().copied().unwrap_or(0.0);
    // ` cpu Apple M1 Pro 44°C ` — identity parts render only when known
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

    // core meters sit in a compact grid under the graph, btop style; the gpu
    // meter row takes one line from the graph so nothing gets clipped:
    // grid <= height/2 and gpu <= 1 always fit the inner budget (height >= 2)
    let gpu_rows = app.gpu_util_pct.is_some() as u16;
    let ncores = app.core_percents.len() as u16;
    let cols: u16 = if ncores > 8 { 2 } else { 1 };
    let grid_rows = ncores.div_ceil(cols).min(inner.height / 2);
    let [graph_area, cores_area, gpu_area] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(grid_rows),
        Constraint::Length(gpu_rows),
    ])
    .areas(inner);

    let vals: Vec<f64> = app.cpu_history.iter().copied().collect();
    f.render_widget(
        BrailleGraph {
            values: &vals,
            max: 100.0,
            style: Style::default(),
            gradient: true,
        },
        graph_area,
    );

    if let Some(util) = app.gpu_util_pct
        && gpu_area.height > 0
    {
        let pct = format!("{util:.1}%");
        // truncate the name so label, bar, and right text share the row
        let max_name = (gpu_area.width as usize).saturating_sub(3 + 2 + pct.len() + 9);
        let name: String = app
            .gpu_name
            .as_deref()
            .unwrap_or("")
            .chars()
            .take(max_name)
            .collect();
        let text = if name.is_empty() {
            pct
        } else {
            format!("{name} {pct}")
        };
        f.render_widget(
            Paragraph::new(meter("gpu", util, &text, gpu_area.width)),
            gpu_area,
        );
    }

    if grid_rows == 0 {
        return;
    }
    let col_areas = Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols as usize])
        .split(cores_area);
    for (i, pct) in app.core_percents.iter().enumerate() {
        // column-major: fill the first column top to bottom, then the next
        let col = i as u16 / grid_rows;
        let row = i as u16 % grid_rows;
        if col >= cols {
            break; // terminal too short for every core
        }
        let cell = col_areas[col as usize];
        let line_area = Rect {
            y: cell.y + row,
            height: 1,
            ..cell
        };
        // one space of breathing room between grid columns
        let width = line_area.width.saturating_sub(1);
        let line = meter(&format!("c{i:02}"), *pct, &format!("{pct:5.1}%"), width);
        f.render_widget(Paragraph::new(line), line_area);
    }
}
