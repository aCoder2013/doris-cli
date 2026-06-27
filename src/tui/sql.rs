use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::Instant;
use tui_textarea::{Input, Key, TextArea};

use crate::client::fe::QueryResult;
use crate::client::FeClient;

pub struct SqlPane {
    pub editor: TextArea<'static>,
    pub result: Option<QueryResult>,
    pub status: String,
    pub result_offset: usize,
}

impl Default for SqlPane {
    fn default() -> Self {
        let mut editor = TextArea::default();
        editor.insert_str("SHOW BACKENDS;");
        Self {
            editor,
            result: None,
            status: "Ctrl+Enter executes SQL".to_string(),
            result_offset: 0,
        }
    }
}

impl SqlPane {
    pub fn sql(&self) -> String {
        self.editor.lines().join("\n")
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Enter {
            return false;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('r') {
            return false;
        }
        let input = Input::from(key);
        match input.key {
            Key::Esc | Key::Tab => false,
            Key::Up if key.modifiers.contains(KeyModifiers::ALT) => {
                self.scroll_up();
                true
            }
            Key::Down if key.modifiers.contains(KeyModifiers::ALT) => {
                self.scroll_down();
                true
            }
            _ => {
                self.editor.input(input);
                true
            }
        }
    }

    pub async fn run(&mut self, fe: &FeClient) {
        let sql = self.sql();
        if sql.trim().is_empty() {
            self.status = "SQL is empty".to_string();
            return;
        }

        self.status = "Running SQL...".to_string();
        let started = Instant::now();
        match fe.query(&sql).await {
            Ok(result) => {
                let rows = result.rows.len();
                self.result = Some(result);
                self.result_offset = 0;
                self.status = format!("SQL completed in {:?}, {} row(s)", started.elapsed(), rows);
            }
            Err(err) => {
                self.status = format!("SQL failed: {err:#}");
            }
        }
    }

    pub fn scroll_down(&mut self) {
        self.result_offset = self.result_offset.saturating_add(1);
    }

    pub fn scroll_up(&mut self) {
        self.result_offset = self.result_offset.saturating_sub(1);
    }
}
