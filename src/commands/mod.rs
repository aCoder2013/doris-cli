pub mod cluster;
pub mod config_cmd;
pub mod deploy;
pub mod ops;
pub mod profile;
pub mod scale;
mod version_gap;

use anyhow::Result;

use crate::cli::{Cli, Command};
use crate::client::FeClient;
use crate::config::Config;
use crate::output::Format;

/// Resolve a [`Config`] from CLI flags: explicit `--fe-host` wins, otherwise load the file.
pub fn resolve_config(cli: &Cli) -> Result<Config> {
    if let Some(host) = &cli.fe_host {
        return Ok(Config::from_fe(
            host.clone(),
            cli.fe_port,
            cli.user.clone(),
            cli.password.clone(),
        ));
    }
    Config::load(cli.config.as_deref(), cli.profile.as_deref())
}

/// Connect to the FE using a resolved config.
pub async fn connect(cli: &Cli) -> Result<FeClient> {
    let cfg = resolve_config(cli)?;
    FeClient::connect(&cfg)
}

pub async fn dispatch(cli: Cli) -> Result<()> {
    let format = Format::parse(&cli.format);
    match &cli.command {
        Command::Config(cmd) => config_cmd::run(&cli, cmd).await,
        Command::Profile(cmd) => profile::run(&cli, cmd).await,
        Command::Cluster(cmd) => cluster::run(&cli, cmd, format).await,
        Command::Ops(cmd) => ops::run(&cli, cmd, format).await,
        Command::Scale(cmd) => scale::run(&cli, cmd).await,
        Command::Deploy(cmd) => deploy::run(&cli, cmd).await,
    }
}
