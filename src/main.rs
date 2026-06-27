mod cli;
mod client;
mod cluster_store;
mod commands;
mod config;
mod output;
mod release;
mod ssh;
mod tui;

use clap::Parser;

use cli::Cli;

#[tokio::main]
async fn main() {
    init_tracing();
    let cli = Cli::parse();
    if let Err(err) = commands::dispatch(cli).await {
        eprintln!("error: {err:#}");
        std::process::exit(1);
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter =
        EnvFilter::try_from_env("DORIS_CLI_LOG").unwrap_or_else(|_| EnvFilter::new("warn"));
    fmt().with_env_filter(filter).with_target(false).init();
}
