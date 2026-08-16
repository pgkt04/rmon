use ratatui::style::Color;

pub const BORDER: Color = Color::Rgb(60, 70, 90);
pub const TITLE: Color = Color::Rgb(180, 190, 220);
pub const LABEL: Color = Color::Rgb(140, 150, 170);
pub const METER_EMPTY: Color = Color::Rgb(50, 55, 70);
pub const SELECTED_BG: Color = Color::Rgb(45, 50, 65);

/// btop-style load gradient: green -> yellow -> red across 0..=100
pub fn gradient(pct: f64) -> Color {
    let p = pct.clamp(0.0, 100.0);
    let (a, b, t) = if p < 50.0 {
        ((80u8, 210u8, 120u8), (235u8, 210u8, 80u8), p / 50.0)
    } else {
        ((235u8, 210u8, 80u8), (230u8, 70u8, 70u8), (p - 50.0) / 50.0)
    };
    let lerp = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    Color::Rgb(lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gradient_endpoints() {
        assert_eq!(gradient(0.0), Color::Rgb(80, 210, 120));
        assert_eq!(gradient(50.0), Color::Rgb(235, 210, 80));
        assert_eq!(gradient(100.0), Color::Rgb(230, 70, 70));
        // out of range clamps
        assert_eq!(gradient(-5.0), gradient(0.0));
        assert_eq!(gradient(140.0), gradient(100.0));
    }
}
