use anyhow::Result;

use crate::cli::{Cli, ClusterCmd};
use crate::commands::connect;
use crate::output::{self, Format};

pub async fn run(cli: &Cli, cmd: &ClusterCmd, format: Format) -> Result<()> {
    let fe = connect(cli).await?;
    match cmd {
        ClusterCmd::Status => status(&fe, format).await,
        ClusterCmd::Frontends => {
            let r = fe.query("SHOW FRONTENDS").await?;
            output::render(&r, format);
            Ok(())
        }
        ClusterCmd::Backends => {
            let r = fe.query("SHOW BACKENDS").await?;
            output::render(&r, format);
            Ok(())
        }
    }
}

async fn status(fe: &crate::client::FeClient, format: Format) -> Result<()> {
    let frontends = fe.query("SHOW FRONTENDS").await?;
    let backends = fe.query("SHOW BACKENDS").await?;

    // Summarize backend availability.
    let alive_idx = backends.col("Alive");
    let decom_idx = backends.col("SystemDecommissioned");
    let mut be_total = 0;
    let mut be_alive = 0;
    let mut be_decom = 0;
    for row in &backends.rows {
        be_total += 1;
        if let Some(i) = alive_idx {
            if row.get(i).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false) {
                be_alive += 1;
            }
        }
        if let Some(i) = decom_idx {
            if row.get(i).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false) {
                be_decom += 1;
            }
        }
    }

    let fe_alive_idx = frontends.col("Alive");
    let mut fe_total = 0;
    let mut fe_alive = 0;
    for row in &frontends.rows {
        fe_total += 1;
        if let Some(i) = fe_alive_idx {
            if row.get(i).map(|v| v.eq_ignore_ascii_case("true")).unwrap_or(false) {
                fe_alive += 1;
            }
        }
    }

    if format == Format::Json {
        let summary = serde_json::json!({
            "frontends": { "total": fe_total, "alive": fe_alive },
            "backends": { "total": be_total, "alive": be_alive, "decommissioning": be_decom },
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("Cluster status");
    println!("  Frontends: {fe_alive}/{fe_total} alive");
    println!("  Backends:  {be_alive}/{be_total} alive, {be_decom} decommissioning");
    if be_alive < be_total {
        output::warn(&format!("{} backend(s) are DOWN", be_total - be_alive));
    }
    if fe_alive < fe_total {
        output::warn(&format!("{} frontend(s) are DOWN", fe_total - fe_alive));
    }
    Ok(())
}
