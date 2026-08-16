use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::theme;

/// one-line btop-style meter: `label ⣿⣿⣿⣀⣀⣀ text`
/// filled cells take the gradient color of their own position
pub fn meter(label: &str, pct: f64, text: &str, width: u16) -> Line<'static> {
    let pct = pct.clamp(0.0, 100.0);
    let mut spans = vec![Span::styled(
        format!("{label} "),
        Style::new().fg(theme::LABEL),
    )];

    let fixed = label.len() + text.len() + 2; // label, two separator spaces
    let bar_w = (width as usize).saturating_sub(fixed);
    if bar_w > 0 {
        let filled = ((pct / 100.0) * bar_w as f64).round() as usize;
        for i in 0..filled {
            let cell_pct = (i as f64 + 0.5) * 100.0 / bar_w as f64;
            spans.push(Span::styled(
                "⣿",
                Style::new().fg(theme::gradient(cell_pct)),
            ));
        }
        spans.push(Span::styled(
            "⣀".repeat(bar_w - filled),
            Style::new().fg(theme::METER_EMPTY),
        ));
    }

    spans.push(Span::styled(
        format!(" {text}"),
        Style::new().fg(theme::TITLE),
    ));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn meter_fills_by_percent() {
        let line = content(&meter("c00", 50.0, "50.0%", 20));
        // 20 - (3 + 5 + 2) = 10 bar cells, half filled
        assert_eq!(line.chars().filter(|c| *c == '⣿').count(), 5);
        assert_eq!(line.chars().filter(|c| *c == '⣀').count(), 5);
        assert_eq!(line.chars().count(), 20);
    }

    #[test]
    fn meter_survives_tiny_width() {
        let line = content(&meter("c00", 50.0, "50.0%", 4));
        // no room for a bar: label and text only
        assert!(!line.contains('⣿'));
    }
}
