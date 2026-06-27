use anyhow::{Context, Result};

use crate::cli::{Cli, ClustersCmd};
use crate::cluster_store::ClusterStore;
use crate::config::{BeConfig, BeNode, Config, FeConfig, FeNode, Topology};
use crate::output;

pub async fn run(cli: &Cli, cmd: &ClustersCmd) -> Result<()> {
    match cmd {
        ClustersCmd::List => list(),
        ClustersCmd::Add {
            name,
            from_current,
            force,
        } => add(cli, name.as_deref(), *from_current, *force),
        ClustersCmd::Use { name } => use_cluster(name),
        ClustersCmd::Remove { name, yes } => remove(name, *yes),
        ClustersCmd::Show {
            name,
            reveal_secrets,
        } => show(name.as_deref(), *reveal_secrets),
    }
}

fn list() -> Result<()> {
    let store = ClusterStore::load()?;
    if store.clusters.is_empty() {
        output::info("no saved clusters yet (create one with `dcli clusters add`)");
        return Ok(());
    }
    for name in store.names() {
        if store.active.as_deref() == Some(name.as_str()) {
            println!("* {name}");
        } else {
            println!("  {name}");
        }
    }
    Ok(())
}

fn add(cli: &Cli, name: Option<&str>, from_current: bool, force: bool) -> Result<()> {
    let mut store = ClusterStore::load()?;
    let name = match name {
        Some(name) => name.to_string(),
        None => output::prompt_line("Cluster name", "")?,
    };
    validate_name(&name)?;
    if store.clusters.contains_key(&name) && !force {
        anyhow::bail!("saved cluster '{name}' already exists (use --force to overwrite)");
    }

    let cfg = if from_current {
        crate::commands::resolve_config(cli)?
    } else {
        prompt_config(&name)?
    };
    store.clusters.insert(name.clone(), cfg);
    if store.active.is_none() {
        store.active = Some(name.clone());
    }
    store.save()?;
    output::ok(&format!("saved encrypted cluster metadata for '{name}'"));
    output::info(&format!(
        "use it with `dcli --cluster {name} tui` or `dcli clusters use {name}`"
    ));
    Ok(())
}

fn use_cluster(name: &str) -> Result<()> {
    validate_name(name)?;
    let mut store = ClusterStore::load()?;
    anyhow::ensure!(
        store.clusters.contains_key(name),
        "saved cluster '{name}' does not exist"
    );
    store.active = Some(name.to_string());
    store.save()?;
    output::ok(&format!("active saved cluster: {name}"));
    Ok(())
}

fn remove(name: &str, yes: bool) -> Result<()> {
    validate_name(name)?;
    let mut store = ClusterStore::load()?;
    anyhow::ensure!(
        store.clusters.contains_key(name),
        "saved cluster '{name}' does not exist"
    );
    if !yes {
        output::confirm(&format!("delete saved cluster '{name}'?"))?;
    }
    store.clusters.remove(name);
    if store.active.as_deref() == Some(name) {
        store.active = store.clusters.keys().next().cloned();
    }
    store.save()?;
    output::ok(&format!("removed saved cluster '{name}'"));
    Ok(())
}

fn show(name: Option<&str>, reveal_secrets: bool) -> Result<()> {
    let store = ClusterStore::load()?;
    let name = match name {
        Some(name) => name.to_string(),
        None => store
            .active
            .clone()
            .context("no active saved cluster; pass a name")?,
    };
    let mut cfg = store
        .get(&name)
        .with_context(|| format!("saved cluster '{name}' does not exist"))?;
    if !reveal_secrets && !cfg.fe.password.is_empty() {
        cfg.fe.password = "******".to_string();
    }
    output::info(&format!("saved cluster: {name}"));
    println!("{}", serde_yaml::to_string(&cfg)?);
    Ok(())
}

fn prompt_config(default_name: &str) -> Result<Config> {
    let logical_name = output::prompt_line("Display name", default_name)?;
    let fe_host = output::prompt_line("FE host", "127.0.0.1")?;
    let query_port = prompt_u16("FE query port", 9030)?;
    let http_port = prompt_u16("FE HTTP port", 8030)?;
    let edit_log_port = prompt_u16("FE edit log port", 9010)?;
    let user = output::prompt_line("FE user", "root")?;
    let password = output::prompt_line("FE password", "")?;
    let be_heartbeat_port = prompt_u16("BE heartbeat port", 9050)?;
    let be_http_port = prompt_u16("BE HTTP port", 8040)?;
    let frontend_hosts = output::prompt_line("Topology FE hosts, comma-separated", &fe_host)?;
    let backend_hosts = output::prompt_line("Topology BE hosts, comma-separated", "")?;

    let frontends = split_hosts(&frontend_hosts)
        .into_iter()
        .enumerate()
        .map(|(idx, host)| FeNode {
            host,
            role: if idx == 0 { "leader" } else { "follower" }.to_string(),
        })
        .collect::<Vec<_>>();
    let backends = split_hosts(&backend_hosts)
        .into_iter()
        .map(|host| BeNode { host })
        .collect::<Vec<_>>();
    let topology = if frontends.is_empty() && backends.is_empty() {
        None
    } else {
        Some(Topology {
            frontends,
            backends,
        })
    };

    Ok(Config {
        name: logical_name,
        fe: FeConfig {
            host: fe_host,
            query_port,
            http_port,
            edit_log_port,
            user,
            password,
        },
        be: BeConfig {
            heartbeat_port: be_heartbeat_port,
            http_port: be_http_port,
        },
        ssh: None,
        deploy: None,
        topology,
    })
}

fn prompt_u16(prompt: &str, default: u16) -> Result<u16> {
    let raw = output::prompt_line(prompt, &default.to_string())?;
    raw.parse::<u16>()
        .with_context(|| format!("{prompt} must be a valid port"))
}

fn split_hosts(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn validate_name(name: &str) -> Result<()> {
    anyhow::ensure!(!name.is_empty(), "cluster name must not be empty");
    anyhow::ensure!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "cluster name '{name}' may only contain letters, digits, '-', '_', '.'"
    );
    anyhow::ensure!(
        name != "." && name != ".." && !name.contains('/'),
        "invalid cluster name '{name}'"
    );
    Ok(())
}
