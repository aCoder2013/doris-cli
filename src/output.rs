use crate::client::fe::QueryResult;
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Table,
    Json,
}

impl Format {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "json" => Format::Json,
            _ => Format::Table,
        }
    }
}

/// Render a [`QueryResult`] either as a pretty table or JSON array of objects.
pub fn render(result: &QueryResult, format: Format) {
    match format {
        Format::Table => render_table(result),
        Format::Json => render_json(result),
    }
}

fn render_table(result: &QueryResult) {
    if result.columns.is_empty() {
        println!("(no rows)");
        return;
    }
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(result.columns.clone());
    for row in &result.rows {
        table.add_row(row.clone());
    }
    println!("{table}");
    println!("{} row(s)", result.rows.len());
}

fn render_json(result: &QueryResult) {
    let mut arr = Vec::with_capacity(result.rows.len());
    for row in &result.rows {
        let mut obj = Map::new();
        for (i, col) in result.columns.iter().enumerate() {
            let cell = row.get(i).cloned().unwrap_or_default();
            obj.insert(col.clone(), Value::String(cell));
        }
        arr.push(Value::Object(obj));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&Value::Array(arr)).unwrap_or_default()
    );
}

/// Prompt for a line of input with a default shown in brackets.
pub fn prompt_line(prompt: &str, default: &str) -> std::io::Result<String> {
    use std::io::Write;
    if default.is_empty() {
        print!("{prompt}: ");
    } else {
        print!("{prompt} [{default}]: ");
    }
    std::io::stdout().flush()?;
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let trimmed = input.trim();
    Ok(if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    })
}

/// Ask a yes/no question; returns Err if the user declines.
pub fn confirm(prompt: &str) -> anyhow::Result<()> {
    use std::io::Write;
    print!("{prompt} [y/N] ");
    std::io::stdout().flush().ok();
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    anyhow::ensure!(
        matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        "aborted by user"
    );
    Ok(())
}

/// Print a short status line that respects color-free terminals.
pub fn ok(msg: &str) {
    println!("✔ {msg}");
}

pub fn info(msg: &str) {
    println!("• {msg}");
}

pub fn warn(msg: &str) {
    eprintln!("⚠ {msg}");
}
