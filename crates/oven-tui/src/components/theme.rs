use ratatui::style::{Color, Modifier, Style};

pub fn user() -> Style {
    Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD)
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

pub fn effort() -> Style {
    Style::default().fg(Color::LightBlue)
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
