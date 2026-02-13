use ratatui::prelude::{Color, Modifier, Style};

pub const COLOR_BG: Color = Color::Rgb(12, 14, 20);
pub const COLOR_BORDER: Color = Color::Rgb(84, 90, 108);
pub const COLOR_TEXT: Color = Color::Rgb(236, 238, 244);
pub const COLOR_MUTED: Color = Color::Rgb(150, 158, 180);
pub const COLOR_HEADER_BG: Color = Color::Rgb(34, 39, 52);
pub const COLOR_ROW_A: Color = Color::Rgb(20, 24, 33);
pub const COLOR_ROW_B: Color = Color::Rgb(16, 20, 29);
pub const COLOR_ROW_SELECTED: Color = Color::Rgb(42, 51, 71);
pub const COLOR_TRACK: Color = Color::Rgb(82, 86, 98);
pub const COLOR_SEPARATOR: Color = Color::Rgb(98, 106, 127);
pub const COLOR_ACCENT_CPU: Color = Color::Rgb(255, 186, 92);
pub const COLOR_ACCENT_THREAD: Color = Color::Rgb(255, 139, 72);
pub const COLOR_ACCENT_GPU: Color = Color::Rgb(255, 169, 90);
pub const COLOR_ACCENT_VRAM: Color = Color::Rgb(255, 205, 124);
pub const COLOR_ACCENT_PROC: Color = Color::Rgb(255, 191, 101);
pub const COLOR_OK: Color = Color::Rgb(255, 214, 130);
pub const COLOR_WARN: Color = Color::Rgb(255, 163, 94);
pub const COLOR_HOT: Color = Color::Rgb(255, 111, 111);

pub fn style_for_usage_with_base(usage: f32, base: Color) -> Style {
    if usage >= 90.0 {
        Style::default().fg(COLOR_HOT).add_modifier(Modifier::BOLD)
    } else if usage >= 70.0 {
        Style::default().fg(COLOR_WARN)
    } else if usage >= 40.0 {
        Style::default().fg(COLOR_OK)
    } else {
        Style::default().fg(base)
    }
}

pub fn style_for_temp(temp_c: Option<f32>) -> Style {
    match temp_c {
        Some(temp) if temp >= 85.0 => Style::default().fg(COLOR_HOT).add_modifier(Modifier::BOLD),
        Some(temp) if temp >= 70.0 => Style::default().fg(COLOR_WARN),
        Some(_) => Style::default().fg(COLOR_OK),
        None => Style::default().fg(COLOR_MUTED),
    }
}
