use anyhow::{Context, Result};
use std::collections::HashMap;

use crate::cli::{Cli, PadRowsetArgs, VersionGapsArgs};
use crate::client::be::{parse_version_range, BeClient, CompactionStatus};
use crate::client::FeClient;
use crate::commands::resolve_config;
use crate::output::{self, Format};

#[derive(Debug, Clone)]
struct BackendInfo {
    id: String,
    host: String,
    http_port: u16,
    alive: bool,
}

#[derive(Debug, Clone)]
struct ReplicaTarget {
    tablet_id: String,
    backend_id: String,
    status: String,
}

#[derive(Debug, Clone)]
struct VersionGap {
    tablet_id: String,
    backend_id: String,
    backend_host: String,
    status: String,
    compaction: CompactionStatus,
    ranges: Vec<(i64, i64)>,
    error: Option<String>,
}

pub async fn version_gaps(cli: &Cli, args: &VersionGapsArgs, format: Format) -> Result<()> {
    let cfg = resolve_config(cli)?;
    let fe = crate::commands::connect(cli).await?;
    let be = BeClient::new(cfg.fe.user.clone(), cfg.fe.password.clone())?;

    let targets = replica_targets(&fe, Some(&args.db), Some(&args.table), args.unhealthy_only).await?;
    let backends = load_backends(&fe, cfg.be.http_port).await?;
    let gaps = collect_gaps(&be, &backends, &targets).await;

    render_gaps(&gaps, format, false);
    Ok(())
}

pub async fn pad_rowset(cli: &Cli, args: &PadRowsetArgs, format: Format) -> Result<()> {
    let cfg = resolve_config(cli)?;
    let fe = crate::commands::connect(cli).await?;
    let be = BeClient::new(cfg.fe.user.clone(), cfg.fe.password.clone())?;

    let targets = if args.tablet_id.is_some() || args.backend_id.is_some() {
        let tablet_id = args
            .tablet_id
            .as_deref()
            .context("--tablet-id and --backend-id must be used together")?;
        let backend_id = args
            .backend_id
            .as_deref()
            .context("--tablet-id and --backend-id must be used together")?;
        vec![ReplicaTarget {
            tablet_id: tablet_id.to_string(),
            backend_id: backend_id.to_string(),
            status: "manual".into(),
        }]
    } else {
        let db = args
            .db
            .as_deref()
            .context("--db is required unless --tablet-id and --backend-id are set")?;
        let table = args
            .table
            .as_deref()
            .context("--table is required unless --tablet-id and --backend-id are set")?;
        replica_targets(&fe, Some(db), Some(table), true).await?
    };

    let backends = load_backends(&fe, cfg.be.http_port).await?;
    let gaps = collect_gaps(&be, &backends, &targets).await;
    let actionable: Vec<_> = gaps
        .iter()
        .filter(|g| g.error.is_none() && !g.ranges.is_empty())
        .collect();

    if actionable.is_empty() {
        render_gaps(&gaps, format, false);
        output::ok("no missing rowsets to pad");
        return Ok(());
    }

    render_gaps(&gaps, format, true);

    if args.dry_run {
        output::info("dry-run: no BE pad_rowset calls were made");
        return Ok(());
    }

    if !args.yes {
        output::warn(
            "pad_rowset inserts EMPTY rowsets — data in the missing versions is permanently lost on that replica",
        );
        output::warn(
            "only use this for single-replica tablets after BE recovery when ADMIN REPAIR cannot recover versions",
        );
        let total: usize = actionable.iter().map(|g| g.ranges.len()).sum();
        output::confirm(&format!(
            "pad {total} missing rowset range(s) across {} tablet replica(s)?",
            actionable.len()
        ))?;
    }

    let mut padded = 0usize;
    for gap in actionable {
        let backend = backends
            .get(&gap.backend_id)
            .with_context(|| format!("backend {} not found", gap.backend_id))?;
        let base_url = format!("http://{}:{}", backend.host, backend.http_port);
        for (start, end) in &gap.ranges {
            be.pad_rowset(&base_url, &gap.tablet_id, *start, *end)
                .await
                .with_context(|| {
                    format!(
                        "pad_rowset failed for tablet {} on backend {} versions [{start},{end}]",
                        gap.tablet_id, gap.backend_id
                    )
                })?;
            padded += 1;
            output::ok(&format!(
                "padded tablet {} backend {} [{start},{end}]",
                gap.tablet_id, gap.backend_id
            ));
        }
    }

    output::info("re-checking compaction status after pad_rowset…");
    let recheck = collect_gaps(&be, &backends, &targets).await;
    let remaining: usize = recheck
        .iter()
        .filter(|g| g.error.is_none())
        .map(|g| g.ranges.len())
        .sum();
    if remaining == 0 {
        output::ok(&format!("re-check passed: padded {padded} range(s), no missing rowsets remain"));
    } else {
        output::warn(&format!(
            "re-check: {remaining} missing rowset range(s) still present — inspect with `dcli ops version-gaps`"
        ));
    }
    Ok(())
}

async fn replica_targets(
    fe: &FeClient,
    db: Option<&str>,
    table: Option<&str>,
    unhealthy_only: bool,
) -> Result<Vec<ReplicaTarget>> {
    let (db, table) = match (db, table) {
        (Some(d), Some(t)) => (d, t),
        _ => anyhow::bail!("both db and table are required"),
    };
    let target = qualified(db, table);
    let mut sql = format!("ADMIN SHOW REPLICA STATUS FROM {target}");
    if unhealthy_only {
        sql.push_str(" WHERE STATUS != \"OK\"");
    }
    let r = fe.query(&sql).await?;
    let tablet_idx = r
        .col("TabletId")
        .context("TabletId column missing from ADMIN SHOW REPLICA STATUS")?;
    let backend_idx = r
        .col("BackendId")
        .context("BackendId column missing from ADMIN SHOW REPLICA STATUS")?;
    let status_idx = r.col("Status");

    let mut out = Vec::new();
    for row in &r.rows {
        let tablet_id = row
            .get(tablet_idx)
            .cloned()
            .filter(|s| !s.is_empty())
            .context("empty TabletId in replica status")?;
        let backend_id = row
            .get(backend_idx)
            .cloned()
            .filter(|s| !s.is_empty())
            .context("empty BackendId in replica status")?;
        let status = status_idx
            .and_then(|i| row.get(i))
            .cloned()
            .unwrap_or_else(|| "UNKNOWN".into());
        out.push(ReplicaTarget {
            tablet_id,
            backend_id,
            status,
        });
    }
    Ok(out)
}

async fn load_backends(fe: &FeClient, default_http_port: u16) -> Result<HashMap<String, BackendInfo>> {
    let r = fe.query("SHOW BACKENDS").await?;
    let id_idx = r.col("BackendId").context("BackendId missing from SHOW BACKENDS")?;
    let host_idx = r.col("Host").context("Host missing from SHOW BACKENDS")?;
    let http_idx = r.col("HttpPort");
    let alive_idx = r.col("Alive");

    let mut map = HashMap::new();
    for row in &r.rows {
        let id = row.get(id_idx).cloned().unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let host = row.get(host_idx).cloned().unwrap_or_default();
        let http_port = http_idx
            .and_then(|i| row.get(i))
            .and_then(|s| s.parse().ok())
            .unwrap_or(default_http_port);
        let alive = alive_idx
            .and_then(|i| row.get(i))
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        map.insert(
            id.clone(),
            BackendInfo {
                id,
                host,
                http_port,
                alive,
            },
        );
    }
    Ok(map)
}

async fn collect_gaps(
    be: &BeClient,
    backends: &HashMap<String, BackendInfo>,
    targets: &[ReplicaTarget],
) -> Vec<VersionGap> {
    let mut gaps = Vec::with_capacity(targets.len());
    for t in targets {
        gaps.push(inspect_one(be, backends, t).await);
    }
    gaps
}

async fn inspect_one(
    be: &BeClient,
    backends: &HashMap<String, BackendInfo>,
    target: &ReplicaTarget,
) -> VersionGap {
    let backend = match backends.get(&target.backend_id) {
        Some(b) => b,
        None => {
            return VersionGap {
                tablet_id: target.tablet_id.clone(),
                backend_id: target.backend_id.clone(),
                backend_host: String::new(),
                status: target.status.clone(),
                compaction: CompactionStatus::default(),
                ranges: Vec::new(),
                error: Some(format!("backend {} not found in SHOW BACKENDS", target.backend_id)),
            };
        }
    };

    if !backend.alive {
        return VersionGap {
            tablet_id: target.tablet_id.clone(),
            backend_id: target.backend_id.clone(),
            backend_host: backend.host.clone(),
            status: target.status.clone(),
            compaction: CompactionStatus::default(),
            ranges: Vec::new(),
            error: Some(format!("backend {} ({}) is not alive", backend.id, backend.host)),
        };
    }

    let base_url = format!("http://{}:{}", backend.host, backend.http_port);
    match be.compaction_show(&base_url, &target.tablet_id).await {
        Ok(compaction) => {
            let ranges: Vec<_> = compaction
                .missing_rowsets
                .iter()
                .filter_map(|r| parse_version_range(r))
                .collect();
            VersionGap {
                tablet_id: target.tablet_id.clone(),
                backend_id: target.backend_id.clone(),
                backend_host: backend.host.clone(),
                status: target.status.clone(),
                compaction,
                ranges,
                error: None,
            }
        }
        Err(e) => VersionGap {
            tablet_id: target.tablet_id.clone(),
            backend_id: target.backend_id.clone(),
            backend_host: backend.host.clone(),
            status: target.status.clone(),
            compaction: CompactionStatus::default(),
            ranges: Vec::new(),
            error: Some(e.to_string()),
        },
    }
}

fn render_gaps(gaps: &[VersionGap], format: Format, emphasize_danger: bool) {
    if format == Format::Json {
        let arr: Vec<_> = gaps
            .iter()
            .map(|g| {
                serde_json::json!({
                    "tablet_id": g.tablet_id,
                    "backend_id": g.backend_id,
                    "backend_host": g.backend_host,
                    "status": g.status,
                    "rowsets": g.compaction.rowsets,
                    "missing_rowsets": g.compaction.missing_rowsets,
                    "ranges": g.ranges.iter().map(|(a,b)| serde_json::json!([a, b])).collect::<Vec<_>>(),
                    "error": g.error,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&arr).unwrap_or_default());
        return;
    }

    if emphasize_danger {
        output::warn("the following missing versions will be filled with EMPTY rowsets (data is lost)");
    }

    let mut any = false;
    for g in gaps {
        if let Some(err) = &g.error {
            output::warn(&format!(
                "tablet {} backend {} ({}): {err}",
                g.tablet_id, g.backend_id, g.backend_host
            ));
            continue;
        }
        if g.ranges.is_empty() {
            continue;
        }
        any = true;
        println!(
            "tablet {} | backend {} ({}) | status {} | missing: {}",
            g.tablet_id,
            g.backend_id,
            g.backend_host,
            g.status,
            g.compaction.missing_rowsets.join(", ")
        );
    }
    if !any && !emphasize_danger {
        output::ok("no missing rowsets reported by BE compaction/show");
    }
}

fn qualified(db: &str, table: &str) -> String {
    format!("`{}`.`{}`", db.trim_matches('`'), table.trim_matches('`'))
}
