use anyhow::{bail, Result};

use crate::cli::{BeHostsArgs, Cli, FeNodeArgs, ScaleCmd};
use crate::client::FeClient;
use crate::commands::{connect, resolve_config};
use crate::output::{self, confirm};

pub async fn run(cli: &Cli, cmd: &ScaleCmd) -> Result<()> {
    let cfg = resolve_config(cli)?;
    let fe = connect(cli).await?;
    match cmd {
        ScaleCmd::AddBe(a) => add_be(&fe, cfg.be.heartbeat_port, a).await,
        ScaleCmd::DecommissionBe(a) => decommission_be(&fe, cfg.be.heartbeat_port, a).await,
        ScaleCmd::CancelDecommission(a) => cancel_decommission(&fe, cfg.be.heartbeat_port, a).await,
        ScaleCmd::DropBe(a) => drop_be(&fe, cfg.be.heartbeat_port, a).await,
        ScaleCmd::AddFe(a) => add_fe(&fe, cfg.fe.edit_log_port, a).await,
        ScaleCmd::DropFe(a) => drop_fe(&fe, cfg.fe.edit_log_port, a).await,
    }
}

async fn add_be(fe: &FeClient, default_port: u16, args: &BeHostsArgs) -> Result<()> {
    let list = host_list(&args.hosts, default_port);
    let sql = format!("ALTER SYSTEM ADD BACKEND {}", quoted_csv(&list));
    fe.exec(&sql).await?;
    output::ok(&format!("added {} backend(s): {}", list.len(), list.join(", ")));
    Ok(())
}

async fn decommission_be(fe: &FeClient, default_port: u16, args: &BeHostsArgs) -> Result<()> {
    let list = host_list(&args.hosts, default_port);
    if !args.yes {
        confirm(&format!(
            "Decommission {} backend(s)? Data will be migrated first (safe). [{}]",
            list.len(),
            list.join(", ")
        ))?;
    }
    let sql = format!("ALTER SYSTEM DECOMMISSION BACKEND {}", quoted_csv(&list));
    fe.exec(&sql).await?;
    output::ok(&format!(
        "decommission started for {} backend(s); track with `dcli ops decommission-status`",
        list.len()
    ));
    Ok(())
}

async fn cancel_decommission(fe: &FeClient, default_port: u16, args: &BeHostsArgs) -> Result<()> {
    let list = host_list(&args.hosts, default_port);
    let sql = format!(
        "ALTER SYSTEM CANCEL DECOMMISSION BACKEND {}",
        quoted_csv(&list)
    );
    fe.exec(&sql).await?;
    output::ok(&format!("cancelled decommission for {}", list.join(", ")));
    Ok(())
}

async fn drop_be(fe: &FeClient, default_port: u16, args: &BeHostsArgs) -> Result<()> {
    let list = host_list(&args.hosts, default_port);
    if !args.yes {
        output::warn("DROP BACKEND removes the node WITHOUT migrating data; replicas on it are lost.");
        output::warn("Prefer `scale decommission-be` for safe removal.");
        confirm(&format!("Force-drop {} backend(s)? [{}]", list.len(), list.join(", ")))?;
    }
    // Doris intentionally requires the misspelled DROPP to avoid accidental data loss.
    let sql = format!("ALTER SYSTEM DROPP BACKEND {}", quoted_csv(&list));
    fe.exec(&sql).await?;
    output::ok(&format!("dropped {} backend(s)", list.len()));
    Ok(())
}

async fn add_fe(fe: &FeClient, default_port: u16, args: &FeNodeArgs) -> Result<()> {
    let role = fe_role(&args.role)?;
    let node = with_port(&args.host, default_port);
    let sql = format!("ALTER SYSTEM ADD {role} \"{node}\"");
    fe.exec(&sql).await?;
    output::ok(&format!("added {role} {node}"));
    output::info("start the new FE with `--helper <existing_fe_host>:<edit_log_port>` on first boot");
    Ok(())
}

async fn drop_fe(fe: &FeClient, default_port: u16, args: &FeNodeArgs) -> Result<()> {
    let role = fe_role(&args.role)?;
    let node = with_port(&args.host, default_port);
    if !args.yes {
        confirm(&format!("Drop {role} {node}?"))?;
    }
    let sql = format!("ALTER SYSTEM DROP {role} \"{node}\"");
    fe.exec(&sql).await?;
    output::ok(&format!("dropped {role} {node}"));
    Ok(())
}

fn fe_role(role: &str) -> Result<&'static str> {
    match role.to_ascii_lowercase().as_str() {
        "follower" => Ok("FOLLOWER"),
        "observer" => Ok("OBSERVER"),
        other => bail!("invalid FE role '{other}' (expected follower or observer)"),
    }
}

/// Parse a comma-separated host list, appending the default port where missing.
fn host_list(hosts: &str, default_port: u16) -> Vec<String> {
    hosts
        .split(',')
        .map(|h| h.trim())
        .filter(|h| !h.is_empty())
        .map(|h| with_port(h, default_port))
        .collect()
}

fn with_port(host: &str, default_port: u16) -> String {
    if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:{default_port}")
    }
}

fn quoted_csv(items: &[String]) -> String {
    items
        .iter()
        .map(|i| format!("\"{i}\""))
        .collect::<Vec<_>>()
        .join(", ")
}
