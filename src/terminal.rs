use anyhow::Result;
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{Stdout, stdout};

pub struct Tui {
    pub terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl Tui {
    pub fn init() -> Result<Self> {
        enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)?;
        let backend = CrosstermBackend::new(stdout());
        let mut terminal = Terminal::new(backend)?;
        terminal.clear()?;
        Ok(Self { terminal })
    }

    pub fn restore() -> Result<()> {
        disable_raw_mode()?;
        execute!(stdout(), LeaveAlternateScreen)?;
        Ok(())
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = Self::restore();
    }
}
