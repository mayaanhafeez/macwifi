use ratatui::style::Color;

#[derive(Clone, Debug)]
pub struct Notification {
    pub text: String,
    pub kind: Kind,
    pub ttl: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Info,
    Error,
}

impl Notification {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: Kind::Info,
            ttl: 30,
        }
    }
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: Kind::Error,
            ttl: 50,
        }
    }
    pub fn color(&self) -> Color {
        match self.kind {
            Kind::Info => Color::Green,
            Kind::Error => Color::Red,
        }
    }
}
