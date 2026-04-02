use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub result_title: Style,
    pub result_index: Style,
    pub result_duration: Style,
    pub result_selected: Style,
    pub result_border: Style,
    pub player_title: Style,
    pub player_time: Style,
    pub player_button_play: Style,
    pub player_button_pause: Style,
    pub gauge_filled: Style,
    pub gauge_unfilled: Style,
    pub gauge_label: Style,
    pub input_prompt: Style,
    pub input_text: Style,
    pub input_cursor: String,
    pub help_key: Style,
    pub help_desc: Style,
    pub page_nav_active: Style,
    pub page_nav_inactive: Style,
    pub error_text: Style,
    pub searching_text: Style,
    pub empty_text: Style,
    pub loading_text: Style,
    pub scanning_text: Style,
}

impl Theme {
    pub fn dark() -> Self {
        Self {
            name: "Dark".into(),
            result_title: Style::default().fg(Color::White),
            result_index: Style::default().fg(Color::DarkGray),
            result_duration: Style::default().fg(Color::DarkGray),
            result_selected: Style::default()
                .fg(Color::Yellow)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
            result_border: Style::default().fg(Color::DarkGray),
            player_title: Style::default().fg(Color::Cyan),
            player_time: Style::default().fg(Color::Green),
            player_button_play: Style::default().fg(Color::Black).bg(Color::Green),
            player_button_pause: Style::default().fg(Color::Black).bg(Color::Yellow),
            gauge_filled: Style::default().fg(Color::Cyan).bg(Color::DarkGray),
            gauge_unfilled: Style::default().fg(Color::DarkGray),
            gauge_label: Style::default().fg(Color::White),
            input_prompt: Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            input_text: Style::default().fg(Color::White),
            input_cursor: "\u{2588}".into(),
            help_key: Style::default().fg(Color::Yellow),
            help_desc: Style::default().fg(Color::DarkGray),
            page_nav_active: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            page_nav_inactive: Style::default().fg(Color::DarkGray),
            error_text: Style::default().fg(Color::Red),
            searching_text: Style::default().fg(Color::Yellow),
            empty_text: Style::default().fg(Color::DarkGray),
            loading_text: Style::default().fg(Color::Yellow),
            scanning_text: Style::default().fg(Color::Yellow),
        }
    }

    pub fn light() -> Self {
        Self {
            name: "Light".into(),
            result_title: Style::default().fg(Color::Black),
            result_index: Style::default().fg(Color::DarkGray),
            result_duration: Style::default().fg(Color::DarkGray),
            result_selected: Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            result_border: Style::default().fg(Color::DarkGray),
            player_title: Style::default().fg(Color::Blue),
            player_time: Style::default().fg(Color::Green),
            player_button_play: Style::default().fg(Color::White).bg(Color::Green),
            player_button_pause: Style::default().fg(Color::White).bg(Color::Yellow),
            gauge_filled: Style::default().fg(Color::Cyan).bg(Color::White),
            gauge_unfilled: Style::default().fg(Color::White),
            gauge_label: Style::default().fg(Color::Black),
            input_prompt: Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD),
            input_text: Style::default().fg(Color::Black),
            input_cursor: "\u{2588}".into(),
            help_key: Style::default().fg(Color::Blue),
            help_desc: Style::default().fg(Color::DarkGray),
            page_nav_active: Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            page_nav_inactive: Style::default().fg(Color::DarkGray),
            error_text: Style::default().fg(Color::Red),
            searching_text: Style::default().fg(Color::Blue),
            empty_text: Style::default().fg(Color::DarkGray),
            loading_text: Style::default().fg(Color::Blue),
            scanning_text: Style::default().fg(Color::Blue),
        }
    }

    pub fn winamp() -> Self {
        Self {
            name: "Winamp".into(),
            result_title: Style::default().fg(Color::Green),
            result_index: Style::default().fg(Color::Rgb(0, 100, 0)),
            result_duration: Style::default().fg(Color::Rgb(0, 100, 0)),
            result_selected: Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
            result_border: Style::default().fg(Color::Rgb(0, 100, 0)),
            player_title: Style::default().fg(Color::Rgb(0, 255, 0)),
            player_time: Style::default().fg(Color::Rgb(0, 200, 0)),
            player_button_play: Style::default().fg(Color::Black).bg(Color::Rgb(0, 255, 0)),
            player_button_pause: Style::default().fg(Color::Black).bg(Color::Rgb(200, 255, 0)),
            gauge_filled: Style::default().fg(Color::Rgb(0, 255, 0)).bg(Color::Rgb(0, 50, 0)),
            gauge_unfilled: Style::default().fg(Color::Rgb(0, 50, 0)),
            gauge_label: Style::default().fg(Color::Rgb(0, 255, 0)),
            input_prompt: Style::default()
                .fg(Color::Rgb(0, 255, 0))
                .add_modifier(Modifier::BOLD),
            input_text: Style::default().fg(Color::Rgb(0, 255, 0)),
            input_cursor: "\u{2588}".into(),
            help_key: Style::default().fg(Color::Rgb(0, 255, 0)),
            help_desc: Style::default().fg(Color::Rgb(0, 100, 0)),
            page_nav_active: Style::default()
                .fg(Color::Rgb(0, 255, 0))
                .add_modifier(Modifier::BOLD),
            page_nav_inactive: Style::default().fg(Color::Rgb(0, 100, 0)),
            error_text: Style::default().fg(Color::Red),
            searching_text: Style::default().fg(Color::Rgb(0, 255, 0)),
            empty_text: Style::default().fg(Color::Rgb(0, 100, 0)),
            loading_text: Style::default().fg(Color::Rgb(0, 255, 0)),
            scanning_text: Style::default().fg(Color::Rgb(0, 255, 0)),
        }
    }

    pub fn builtins() -> Vec<Self> {
        vec![Self::dark(), Self::light(), Self::winamp()]
    }

    pub fn from_name(name: &str) -> Self {
        Self::builtins()
            .into_iter()
            .find(|t| t.name.eq_ignore_ascii_case(name))
            .unwrap_or_else(Self::dark)
    }
}
