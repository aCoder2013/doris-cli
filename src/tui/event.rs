use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use std::time::Duration;

use crate::client::FeClient;

use super::app::{App, Tab};

pub async fn handle_events(app: &mut App, fe: &FeClient) -> Result<()> {
    if !event::poll(Duration::from_millis(100))? {
        return Ok(());
    }

    let ev = event::read()?;
    if let Event::Key(key) = ev {
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }

        if app.tab() == Tab::Sql && app.sql.handle_key(key) {
            return Ok(());
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
            KeyCode::Tab | KeyCode::Right => app.next_tab(),
            KeyCode::BackTab | KeyCode::Left => app.previous_tab(),
            KeyCode::Char('r') => app.refresh(fe).await,
            KeyCode::Char('j') | KeyCode::Down => app.scroll_down(),
            KeyCode::Char('k') | KeyCode::Up => app.scroll_up(),
            KeyCode::Enter
                if app.tab() == Tab::Sql && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                app.run_sql(fe).await;
            }
            KeyCode::F(5) => app.refresh(fe).await,
            _ => {}
        }
    }

    Ok(())
}
