use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::BorderType;

pub fn user() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
}

pub fn shell() -> Style {
    Style::default().fg(Color::Blue)
}

pub fn assistant() -> Style {
    Style::default().fg(Color::Green)
}

pub fn thinking() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn tool() -> Style {
    Style::default().fg(Color::Magenta)
}

pub fn diff_added() -> Style {
    Style::default().fg(Color::Black).bg(Color::LightGreen)
}

pub fn diff_removed() -> Style {
    Style::default().fg(Color::Black).bg(Color::LightRed)
}

pub fn ok() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn fail() -> Style {
    Style::default().fg(Color::Red)
}

pub fn error() -> Style {
    Style::default().fg(Color::Red)
}

pub fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

pub fn accent() -> Style {
    Style::default().fg(Color::Cyan)
}

pub fn model() -> Style {
    Style::default().fg(Color::LightYellow)
}

pub fn path() -> Style {
    Style::default().fg(Color::LightGreen)
}

pub fn mode() -> Style {
    Style::default().fg(Color::LightMagenta)
}

pub fn reply() -> Style {
    Style::default().fg(Color::Rgb(255, 140, 0))
}

pub fn selection() -> Style {
    Style::default().add_modifier(Modifier::REVERSED)
}

pub fn border_idle() -> Style {
    dim()
}

pub fn border_active() -> Style {
    accent()
}

pub fn border_type() -> BorderType {
    BorderType::Rounded
}
