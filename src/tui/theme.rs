use ratatui::{
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, BorderType},
};

#[derive(Clone, Copy)]
pub struct Theme {
    pub background: Color,
    pub panel: Color,
    pub text: Color,
    pub muted: Color,
    pub border: Color,
    pub selection: Color,
    pub accent: Color,
    pub secondary: Color,
    pub success: Color,
    pub warning: Color,
    pub error: Color,
}

impl Theme {
    pub fn named(name: &str) -> Self {
        match name {
            // https://github.com/catppuccin/palette — Macchiato
            "macchiato" => Self {
                background: Color::Rgb(36, 39, 58),
                panel: Color::Rgb(30, 32, 48),
                text: Color::Rgb(202, 211, 245),
                muted: Color::Rgb(165, 173, 203),
                border: Color::Rgb(73, 77, 100),
                selection: Color::Rgb(54, 58, 79),
                accent: Color::Rgb(198, 160, 246),
                secondary: Color::Rgb(138, 173, 244),
                success: Color::Rgb(166, 218, 149),
                warning: Color::Rgb(238, 212, 159),
                error: Color::Rgb(237, 135, 150),
            },
            "dark" => Self {
                background: Color::Black,
                panel: Color::Black,
                text: Color::White,
                muted: Color::Gray,
                border: Color::DarkGray,
                selection: Color::DarkGray,
                accent: Color::Rgb(255, 45, 85),
                secondary: Color::LightBlue,
                success: Color::LightGreen,
                warning: Color::Yellow,
                error: Color::LightRed,
            },
            "light" => Self {
                background: Color::White,
                panel: Color::Rgb(245, 245, 249),
                text: Color::Black,
                muted: Color::DarkGray,
                border: Color::Gray,
                selection: Color::Rgb(226, 229, 240),
                accent: Color::Rgb(100, 60, 160),
                secondary: Color::Blue,
                success: Color::Rgb(40, 110, 55),
                warning: Color::Rgb(140, 90, 0),
                error: Color::Red,
            },
            _ => Self {
                background: Color::Reset,
                panel: Color::Reset,
                text: Color::Reset,
                muted: Color::Gray,
                border: Color::DarkGray,
                selection: Color::DarkGray,
                accent: Color::Rgb(255, 45, 85),
                secondary: Color::LightBlue,
                success: Color::Green,
                warning: Color::Yellow,
                error: Color::Red,
            },
        }
    }

    pub fn base(self) -> Style {
        Style::default().fg(self.text).bg(self.background)
    }
    pub fn subdued(self) -> Style {
        self.base().fg(self.muted)
    }
    pub fn selected(self, focused: bool) -> Style {
        self.base()
            .fg(if focused { self.accent } else { self.text })
            .bg(self.selection)
            .add_modifier(Modifier::BOLD)
    }
    pub fn block<'a>(self, title: impl Into<Line<'a>>, focused: bool) -> Block<'a> {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .style(self.base())
            .border_style(Style::default().fg(if focused { self.accent } else { self.border }))
            .title_style(Style::default().fg(if focused { self.accent } else { self.muted }))
            .title(title)
    }
}
