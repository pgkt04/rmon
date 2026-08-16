use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::Widget;

use super::theme;

// braille column filled bottom-up: dot bits for 0..=4 dots
const LEFT: [u8; 5] = [0x00, 0x40, 0x44, 0x46, 0x47];
const RIGHT: [u8; 5] = [0x00, 0x80, 0xA0, 0xB0, 0xB8];

pub fn braille_cell(left: usize, right: usize) -> char {
    let bits = LEFT[left.min(4)] | RIGHT[right.min(4)];
    // 0x2800 + any u8 is always a valid braille codepoint
    char::from_u32(0x2800 + bits as u32).expect("invariant: braille block is contiguous")
}

/// btop-style history graph; values 0..=max, newest last, right-aligned
pub struct BrailleGraph<'a> {
    pub values: &'a [f64],
    pub max: f64,
    pub style: Style,
    /// color each column by its load instead of `style`
    pub gradient: bool,
}

impl Widget for BrailleGraph<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.is_empty() || self.max <= 0.0 {
            return;
        }
        let max_levels = area.height as usize * 4;
        let cols = area.width as usize * 2; // 2 value columns per char
        let take = self.values.len().min(cols);
        let vals = &self.values[self.values.len() - take..];

        let mut levels = vec![0usize; cols];
        let mut col_pct = vec![0f64; area.width as usize];
        let start = cols - take;
        for (i, v) in vals.iter().enumerate() {
            let frac = (v / self.max).clamp(0.0, 1.0);
            levels[start + i] = (frac * max_levels as f64).round() as usize;
            let cx = (start + i) / 2;
            col_pct[cx] = col_pct[cx].max(frac * 100.0);
        }

        for row in 0..area.height {
            // bottom text row holds levels 0..4, the one above 4..8, ...
            let row_base = (area.height - 1 - row) as usize * 4;
            for cx in 0..area.width as usize {
                let l = levels[cx * 2].saturating_sub(row_base).min(4);
                let r = levels[cx * 2 + 1].saturating_sub(row_base).min(4);
                if let Some(cell) = buf.cell_mut((area.x + cx as u16, area.y + row)) {
                    cell.set_char(braille_cell(l, r));
                    let style = if self.gradient {
                        Style::new().fg(theme::gradient(col_pct[cx]))
                    } else {
                        self.style
                    };
                    cell.set_style(style);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;
    use ratatui::widgets::Widget;

    #[test]
    fn braille_dot_levels() {
        assert_eq!(braille_cell(0, 0), '\u{2800}');
        assert_eq!(braille_cell(4, 4), '⣿');
        assert_eq!(braille_cell(1, 0), '⡀'); // one dot, bottom left
        assert_eq!(braille_cell(0, 1), '⢀'); // one dot, bottom right
        assert_eq!(braille_cell(9, 9), '⣿'); // clamps
    }

    #[test]
    fn renders_right_aligned() {
        let values = vec![100.0, 100.0]; // exactly one char cell
        let mut buf = Buffer::empty(Rect::new(0, 0, 4, 1));
        let g = BrailleGraph {
            values: &values,
            max: 100.0,
            style: Style::default(),
            gradient: false,
        };
        g.render(Rect::new(0, 0, 4, 1), &mut buf);
        assert_eq!(buf[(3, 0)].symbol(), "⣿");
        assert_eq!(buf[(0, 0)].symbol(), "\u{2800}");
    }

    #[test]
    fn tall_graph_fills_bottom_rows_first() {
        let values = vec![50.0, 50.0]; // half of an 8-level column
        let mut buf = Buffer::empty(Rect::new(0, 0, 1, 2));
        let g = BrailleGraph {
            values: &values,
            max: 100.0,
            style: Style::default(),
            gradient: false,
        };
        g.render(Rect::new(0, 0, 1, 2), &mut buf);
        assert_eq!(buf[(0, 1)].symbol(), "⣿"); // bottom row full
        assert_eq!(buf[(0, 0)].symbol(), "\u{2800}"); // top row empty
    }
}
