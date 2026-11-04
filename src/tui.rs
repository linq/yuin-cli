use std::io::{self, IsTerminal};

use anyhow::{Result, ensure};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, List, ListItem, ListState, Paragraph, Wrap},
};

const PAGES: [&str; 5] = ["Search", "Playlists", "Charts", "My Library", "Downloads"];

pub fn run() -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "The TUI needs an interactive terminal; use `yuin --help` for command-line options"
    );
    ratatui::run(|terminal| -> io::Result<()> {
        let mut selected = 0;
        loop {
            terminal.draw(|frame| draw(frame, selected))?;
            match event::read()? {
                Event::Key(key) if key.is_press() => match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => break,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => break,
                    KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => {
                        selected = (selected + 1) % PAGES.len()
                    }
                    KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => {
                        selected = (selected + PAGES.len() - 1) % PAGES.len()
                    }
                    KeyCode::Char(number @ '1'..='5') => {
                        selected = (number as usize) - ('1' as usize)
                    }
                    _ => {}
                },
                Event::Resize(_, _) => {}
                _ => {}
            }
        }
        Ok(())
    })?;
    Ok(())
}

fn draw(frame: &mut Frame, selected: usize) {
    let accent = Color::Rgb(255, 45, 85);
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(2),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(" Yuin · 余音 / Terminal Edition")
            .style(Style::default().fg(accent))
            .block(Block::bordered()),
        rows[0],
    );
    let columns = Layout::horizontal([Constraint::Length(20), Constraint::Min(10)]).split(rows[1]);
    let items = PAGES
        .iter()
        .enumerate()
        .map(|(index, title)| ListItem::new(format!("{}  {title}", index + 1)));
    let menu = List::new(items)
        .block(Block::bordered().title(" Browse "))
        .highlight_style(Style::default().fg(accent))
        .highlight_symbol("› ");
    frame.render_stateful_widget(
        menu,
        columns[0],
        &mut ListState::default().with_selected(Some(selected)),
    );
    frame.render_widget(Paragraph::new("\n Project initialized.\n\n Catalog, music sources, playback, and downloads are not connected yet.\n\n This shell provides navigation only. The implementation plan is in docs/TECHNICAL_DESIGN.zh-CN.md.")
        .wrap(Wrap { trim: false }).block(Block::bordered().title(format!(" {} ", PAGES[selected]))), columns[1]);
    frame.render_widget(
        Paragraph::new(" ↑/↓ or j/k: navigate   1–5: page   Tab: next   q / Esc: quit "),
        rows[2],
    );
}
