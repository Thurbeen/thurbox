use ratatui as rendering;
use serde::Serialize;

pub fn color() -> rendering::style::Color {
    rendering::style::Color::Reset
}

pub fn style() -> ratatui::style::Style {
    ratatui::style::Style::default()
}

#[derive(Serialize)]
pub struct Data;
