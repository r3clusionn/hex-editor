//! The terminal loop: crossterm in raw mode on the alternate screen, drawing the frames that
//! [`App::render`] produces and feeding it key events.

use std::io::{self, Write};

use crossterm::cursor::{Hide, MoveTo, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

use crate::app::{App, Key, Screen, Style};

/// Puts the terminal back as it was, also when the editor panics.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

pub fn to_key(e: KeyEvent) -> Option<Key> {
    let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
    Some(match e.code {
        KeyCode::Char(c) if ctrl => Key::Ctrl(c.to_ascii_lowercase()),
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Tab => Key::Tab,
        KeyCode::F(1) => Key::F1,
        _ => return None,
    })
}

fn draw(out: &mut impl Write, screen: &Screen) -> io::Result<()> {
    queue!(out, Hide)?;
    for (y, line) in screen.lines.iter().enumerate() {
        queue!(out, MoveTo(0, y as u16))?;
        for span in line {
            match span.style {
                Style::Plain => {}
                Style::Dim => queue!(out, SetForegroundColor(Color::DarkGrey))?,
                Style::Offset => queue!(out, SetForegroundColor(Color::Cyan))?,
                Style::Modified => queue!(out, SetForegroundColor(Color::Yellow))?,
                Style::Cursor => queue!(out, SetAttribute(Attribute::Reverse))?,
                Style::Mirror => queue!(out, SetBackgroundColor(Color::DarkGrey))?,
                Style::Bar => queue!(out, SetAttribute(Attribute::Reverse), SetAttribute(Attribute::Bold))?,
                Style::Error => queue!(out, SetForegroundColor(Color::Red), SetAttribute(Attribute::Bold))?,
                Style::Prompt => queue!(out, SetAttribute(Attribute::Bold))?,
            }
            queue!(out, Print(&span.text))?;
            if span.style != Style::Plain {
                queue!(out, SetAttribute(Attribute::Reset), ResetColor)?;
            }
        }
    }
    out.flush()
}

/// Runs the editor until the user quits.
pub fn run(mut app: App) -> io::Result<()> {
    terminal::enable_raw_mode()?;
    let _restore = Restore;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    while !app.quit {
        let (w, h) = terminal::size()?;
        let screen = app.render(w as usize, h as usize);
        draw(&mut out, &screen)?;
        match event::read()? {
            // Windows reports a release as well as a press; act on presses and repeats only.
            Event::Key(k) if k.kind != KeyEventKind::Release => {
                if let Some(key) = to_key(k) {
                    app.handle_key(key);
                }
            }
            _ => {}
        }
    }
    Ok(())
}
