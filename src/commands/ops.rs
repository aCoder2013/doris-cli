use anyhow::Result;

use crate::cli::{BalanceCmd, Cli, OpsCmd, RepairArgs, TabletsArgs};
use crate::client::fe::QueryResult;
use crate::client::FeClient;
use crate::commands::connect;
use crate::output::{self, Format};

pub async fn run(cli: &Cli, cmd: &OpsCmd, format: Format) -> Result<()> {
    let fe = connect(cli).await?;
    match cmd {
        OpsCmd::Health => health(&fe, format).await,
        OpsCmd::Tablets(args) => tablets(&fe, args, format).await,
        OpsCmd::Repair(args) => repair(&fe, args, false).await,
        OpsCmd::CancelRepair(args) => repair(&fe, args, true).await,
        OpsCmd::DecommissionStatus => decommission_status(&fe, format).await,
        OpsCmd::Balance(b) => balance(&fe, b, format).await,
    }
}

async fn health(fe: &FeClient, format: Format) -> Result<()> {
    let r = fe
        .query("SHOW PROC '/cluster_health/tablet_health'")
        .await?;
    output::render(&r, format);

    // Highlight problem databases in table mode.
    if format == Format::Table {
        let problem_cols = [
            "ReplicaMissingNum",
            "VersionIncompleteNum",
            "UnrecoverableNum",
            "NeedFurtherRepairNum",
        ];
        let mut flagged = false;
        for row in &r.rows {
            for c in problem_cols {
                if let Some(v) = r.col(c).and_then(|i| row.get(i)) {
                    if v.parse::<i64>().unwrap_or(0) > 0 {
                        let db = r
                            .col("DbName")
                            .and_then(|i| row.get(i))
                            .cloned()
                            .unwrap_or_default();
                        output::warn(&format!("db {db}: {c} = {v}"));
                        flagged = true;
                    }
                }
            }
        }
        if !flagged {
            output::ok("all tablets healthy");
        }
    }
    Ok(())
}

async fn tablets(fe: &FeClient, args: &TabletsArgs, format: Format) -> Result<()> {
    let target = qualified(&args.db, &args.table);
    let mut sql = format!("ADMIN SHOW REPLICA STATUS FROM {target}");
    if args.unhealthy_only {
        sql.push_str(" WHERE STATUS != \"OK\"");
    }
    let r: QueryResult = fe.query(&sql).await?;
    output::render(&r, format);
    if format == Format::Table && r.rows.is_empty() {
        output::ok("no unhealthy replicas found");
    }
    Ok(())
}

async fn repair(fe: &FeClient, args: &RepairArgs, cancel: bool) -> Result<()> {
    let target = qualified(&args.db, &args.table);
    let verb = if cancel { "ADMIN CANCEL REPAIR TABLE" } else { "ADMIN REPAIR TABLE" };
    let mut sql = format!("{verb} {target}");
    if let Some(parts) = &args.partitions {
        let list = parts
            .split(',')
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        sql.push_str(&format!(" PARTITION ({list})"));
    }
    fe.exec(&sql).await?;
    if cancel {
        output::ok(&format!("cancelled repair for {target}"));
    } else {
        output::ok(&format!(
            "scheduled high-priority repair for {target}; check progress with `dcli ops tablets --db {} --table {}`",
            args.db, args.table
        ));
    }
    Ok(())
}

async fn decommission_status(fe: &FeClient, format: Format) -> Result<()> {
    let r = fe.query("SHOW BACKENDS").await?;
    // Filter to decommissioning backends and project the relevant columns.
    let want = [
        "BackendId",
        "Host",
        "Alive",
        "SystemDecommissioned",
        "TabletNum",
        "DataUsedCapacity",
    ];
    let decom_idx = r.col("SystemDecommissioned");
    let mut filtered = QueryResult {
        columns: want
            .iter()
            .filter(|c| r.col(c).is_some())
            .map(|c| c.to_string())
            .collect(),
        ..Default::default()
    };
    for row in &r.rows {
        let is_decom = decom_idx
            .and_then(|i| row.get(i))
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        if !is_decom {
            continue;
        }
        let projected = filtered
            .columns
            .iter()
            .map(|c| r.col(c).and_then(|i| row.get(i)).cloned().unwrap_or_default())
            .collect();
        filtered.rows.push(projected);
    }
    output::render(&filtered, format);
    if format == Format::Table {
        if filtered.rows.is_empty() {
            output::info("no backends are currently decommissioning");
        } else {
            output::info("decommission completes when TabletNum reaches 0, then the BE is auto-dropped");
        }
    }
    Ok(())
}

async fn balance(fe: &FeClient, cmd: &BalanceCmd, format: Format) -> Result<()> {
    match cmd {
        BalanceCmd::Show => {
            let r = fe
                .query("ADMIN SHOW FRONTEND CONFIG LIKE '%disable_balance%'")
                .await?;
            output::render(&r, format);
        }
        BalanceCmd::Enable => {
            fe.exec("ADMIN SET FRONTEND CONFIG (\"disable_balance\" = \"false\")")
                .await?;
            output::ok("tablet balancing enabled (disable_balance = false)");
        }
        BalanceCmd::Disable => {
            fe.exec("ADMIN SET FRONTEND CONFIG (\"disable_balance\" = \"true\")")
                .await?;
            output::ok("tablet balancing disabled (disable_balance = true)");
        }
    }
    Ok(())
}

/// Quote a db.table reference defensively with backticks.
fn qualified(db: &str, table: &str) -> String {
    format!("`{}`.`{}`", db.trim_matches('`'), table.trim_matches('`'))
}
