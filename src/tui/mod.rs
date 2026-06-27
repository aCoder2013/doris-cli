pub mod app;
pub mod data;
pub mod event;
pub mod sql;
pub mod ui;

use anyhow::{Context, Result};
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Stdout};
use std::time::{Duration, Instant};

use crate::cli::Cli;
use crate::client::FeClient;
use crate::cluster_store::ClusterStore;
use crate::commands::resolve_config;
use crate::config::Config;

use self::app::App;

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("failed to enable terminal raw mode")?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend).context("failed to initialize terminal")?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

pub async fn run(cli: &Cli) -> Result<()> {
    let cfg = resolve_tui_config(cli)?;
    let fe = FeClient::connect(&cfg)?;
    let mut app = App::new(cfg);
    let mut terminal = TerminalGuard::enter()?;

    app.refresh(&fe).await;
    let mut last_refresh = Instant::now();
    loop {
        terminal
            .terminal
            .draw(|frame| ui::render(frame, &mut app))?;
        if app.should_quit {
            break;
        }

        if last_refresh.elapsed() >= Duration::from_secs(5) {
            app.refresh(&fe).await;
            last_refresh = Instant::now();
        }

        event::handle_events(&mut app, &fe).await?;
    }

    fe.disconnect().await.ok();
    Ok(())
}

fn resolve_tui_config(cli: &Cli) -> Result<Config> {
    if cli.fe_host.is_some()
        || cli.cluster.is_some()
        || cli.config.is_some()
        || cli.profile.is_some()
        || std::env::var("DORIS_CLI_CONFIG").is_ok()
        || crate::config::active_profile().is_some()
    {
        return resolve_config(cli);
    }

    let store = ClusterStore::load()?;
    if store.clusters.is_empty() {
        return resolve_config(cli);
    }
    if store.clusters.len() == 1 {
        return store
            .clusters
            .values()
            .next()
            .cloned()
            .context("saved cluster store is unexpectedly empty");
    }

    println!("Saved clusters:");
    let names = store.names();
    for (idx, name) in names.iter().enumerate() {
        let marker = if store.active.as_deref() == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        println!("  {}. {marker} {name}", idx + 1);
    }
    let default = store
        .active
        .as_ref()
        .and_then(|active| names.iter().position(|name| name == active))
        .map(|idx| idx + 1)
        .unwrap_or(1);
    let choice = crate::output::prompt_line("Select cluster", &default.to_string())?;
    let index = choice
        .parse::<usize>()
        .ok()
        .filter(|idx| *idx >= 1 && *idx <= names.len())
        .with_context(|| format!("invalid cluster selection '{choice}'"))?;
    let name = &names[index - 1];
    store
        .get(name)
        .with_context(|| format!("saved cluster '{name}' does not exist"))
}
