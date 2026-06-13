use anyhow::{Context, Result};

use crate::cli::{Cli, ConfigCmd};
use crate::commands::resolve_config;
use crate::config::Config;
use crate::output;

pub async fn run(cli: &Cli, cmd: &ConfigCmd) -> Result<()> {
    match cmd {
        ConfigCmd::Init { force } => init(cli, *force),
        ConfigCmd::Show => show(cli),
    }
}

fn init(cli: &Cli, force: bool) -> Result<()> {
    let path = Config::resolve_path(cli.config.as_deref())
        .context("could not determine config path")?;
    if path.exists() && !force {
        output::warn(&format!(
            "config already exists at {} (use --force to overwrite)",
            path.display()
        ));
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&path, Config::sample_yaml())
        .with_context(|| format!("failed to write {}", path.display()))?;
    output::ok(&format!("wrote sample config to {}", path.display()));
    Ok(())
}

fn show(cli: &Cli) -> Result<()> {
    let cfg = resolve_config(cli)?;
    println!("{}", serde_yaml::to_string(&cfg)?);
    Ok(())
}
