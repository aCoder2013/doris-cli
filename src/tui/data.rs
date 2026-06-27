#![allow(dead_code)]

use std::time::SystemTime;

use anyhow::Result;

use crate::client::fe::QueryResult;
use crate::client::FeClient;

pub const SHOW_FRONTENDS_SQL: &str = "SHOW FRONTENDS";
pub const SHOW_BACKENDS_SQL: &str = "SHOW BACKENDS";
pub const TABLET_HEALTH_SQL: &str = "SHOW PROC '/cluster_health/tablet_health'";

pub type FeSnapshot = FrontendsSnapshot;
pub type BeSnapshot = BackendsSnapshot;

#[derive(Debug, Clone)]
pub struct ClusterSnapshot {
    pub loaded_at: SystemTime,
    pub frontends: QueryResult,
    pub backends: QueryResult,
    pub tablet_health: QueryResult,
    pub summary: ClusterSummary,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct ClusterSummary {
    pub fe_total: usize,
    pub fe_alive: usize,
    pub fe_down: usize,
    pub be_total: usize,
    pub be_alive: usize,
    pub be_down: usize,
    pub be_decommissioning: usize,
    pub tablet_health_rows: usize,
    pub tablet_problem_rows: usize,
}

#[derive(Debug, Clone)]
pub struct OverviewSnapshot {
    pub loaded_at: SystemTime,
    pub frontends: FrontendSummary,
    pub backends: BackendSummary,
}

#[derive(Debug, Clone)]
pub struct FrontendsSnapshot {
    pub loaded_at: SystemTime,
    pub summary: FrontendSummary,
    pub nodes: Vec<FrontendNodeSnapshot>,
    pub raw: QueryResult,
}

#[derive(Debug, Clone)]
pub struct BackendsSnapshot {
    pub loaded_at: SystemTime,
    pub summary: BackendSummary,
    pub nodes: Vec<BackendNodeSnapshot>,
    pub raw: QueryResult,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct FrontendSummary {
    pub total: usize,
    pub alive: usize,
    pub down: usize,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq)]
pub struct BackendSummary {
    pub total: usize,
    pub alive: usize,
    pub down: usize,
    pub decommissioning: usize,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct FrontendNodeSnapshot {
    pub name: Option<String>,
    pub host: Option<String>,
    pub role: Option<String>,
    pub alive: bool,
    pub is_master: bool,
    pub version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct BackendNodeSnapshot {
    pub id: Option<String>,
    pub host: Option<String>,
    pub alive: bool,
    pub system_decommissioned: bool,
    pub tablet_num: Option<String>,
    pub data_used_capacity: Option<String>,
    pub max_disk_used_pct: Option<String>,
    pub version: Option<String>,
    pub error: Option<String>,
}

pub async fn load_cluster_snapshot(fe: &FeClient) -> Result<ClusterSnapshot> {
    let (frontends, backends, tablet_health) =
        tokio::try_join!(load_frontends(fe), load_backends(fe), async {
            Ok::<_, anyhow::Error>(fe.query(TABLET_HEALTH_SQL).await.unwrap_or_default())
        },)?;
    let loaded_at = latest_of(&[frontends.loaded_at, backends.loaded_at, SystemTime::now()]);
    let summary = ClusterSummary {
        fe_total: frontends.summary.total,
        fe_alive: frontends.summary.alive,
        fe_down: frontends.summary.down,
        be_total: backends.summary.total,
        be_alive: backends.summary.alive,
        be_down: backends.summary.down,
        be_decommissioning: backends.summary.decommissioning,
        tablet_health_rows: tablet_health.rows.len(),
        tablet_problem_rows: count_tablet_problem_rows(&tablet_health),
    };

    Ok(ClusterSnapshot {
        loaded_at,
        frontends: frontends.raw,
        backends: backends.raw,
        tablet_health,
        summary,
    })
}

pub async fn load_overview(fe: &FeClient) -> Result<OverviewSnapshot> {
    let (frontends, backends) = tokio::try_join!(load_frontends(fe), load_backends(fe))?;
    Ok(OverviewSnapshot {
        loaded_at: latest_of(&[frontends.loaded_at, backends.loaded_at]),
        frontends: frontends.summary,
        backends: backends.summary,
    })
}

pub async fn load_frontends(fe: &FeClient) -> Result<FrontendsSnapshot> {
    let raw = fe.query(SHOW_FRONTENDS_SQL).await?;
    Ok(frontends_from_result(raw))
}

pub async fn load_fe(fe: &FeClient) -> Result<FeSnapshot> {
    load_frontends(fe).await
}

pub async fn load_backends(fe: &FeClient) -> Result<BackendsSnapshot> {
    let raw = fe.query(SHOW_BACKENDS_SQL).await?;
    Ok(backends_from_result(raw))
}

pub async fn load_be(fe: &FeClient) -> Result<BeSnapshot> {
    load_backends(fe).await
}

pub fn frontends_from_result(raw: QueryResult) -> FrontendsSnapshot {
    let nodes = raw
        .rows
        .iter()
        .map(|row| FrontendNodeSnapshot {
            name: cell(&raw, row, "Name"),
            host: cell(&raw, row, "Host"),
            role: cell(&raw, row, "Role"),
            alive: bool_cell(&raw, row, "Alive"),
            is_master: bool_cell(&raw, row, "IsMaster"),
            version: cell(&raw, row, "Version"),
            error: cell(&raw, row, "ErrMsg"),
        })
        .collect::<Vec<_>>();
    let summary = summarize_frontends(&nodes);

    FrontendsSnapshot {
        loaded_at: SystemTime::now(),
        summary,
        nodes,
        raw,
    }
}

pub fn backends_from_result(raw: QueryResult) -> BackendsSnapshot {
    let nodes = raw
        .rows
        .iter()
        .map(|row| BackendNodeSnapshot {
            id: cell(&raw, row, "BackendId"),
            host: cell(&raw, row, "Host"),
            alive: bool_cell(&raw, row, "Alive"),
            system_decommissioned: bool_cell(&raw, row, "SystemDecommissioned"),
            tablet_num: cell(&raw, row, "TabletNum"),
            data_used_capacity: cell(&raw, row, "DataUsedCapacity"),
            max_disk_used_pct: cell(&raw, row, "MaxDiskUsedPct"),
            version: cell(&raw, row, "Version"),
            error: cell(&raw, row, "ErrMsg"),
        })
        .collect::<Vec<_>>();
    let summary = summarize_backends(&nodes);

    BackendsSnapshot {
        loaded_at: SystemTime::now(),
        summary,
        nodes,
        raw,
    }
}

fn summarize_frontends(nodes: &[FrontendNodeSnapshot]) -> FrontendSummary {
    let total = nodes.len();
    let alive = nodes.iter().filter(|node| node.alive).count();
    FrontendSummary {
        total,
        alive,
        down: total.saturating_sub(alive),
    }
}

fn summarize_backends(nodes: &[BackendNodeSnapshot]) -> BackendSummary {
    let total = nodes.len();
    let alive = nodes.iter().filter(|node| node.alive).count();
    let decommissioning = nodes
        .iter()
        .filter(|node| node.system_decommissioned)
        .count();
    BackendSummary {
        total,
        alive,
        down: total.saturating_sub(alive),
        decommissioning,
    }
}

fn cell(result: &QueryResult, row: &[String], name: &str) -> Option<String> {
    result
        .col(name)
        .and_then(|idx| row.get(idx))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("NULL"))
}

fn bool_cell(result: &QueryResult, row: &[String], name: &str) -> bool {
    result
        .col(name)
        .and_then(|idx| row.get(idx))
        .map(|value| {
            value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("yes") || value == "1"
        })
        .unwrap_or(false)
}

fn latest_of(times: &[SystemTime]) -> SystemTime {
    times
        .iter()
        .copied()
        .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal))
        .unwrap_or_else(SystemTime::now)
}

fn count_tablet_problem_rows(result: &QueryResult) -> usize {
    let problem_cols = [
        "ReplicaMissingNum",
        "VersionIncompleteNum",
        "UnrecoverableNum",
        "NeedFurtherRepairNum",
    ];

    result
        .rows
        .iter()
        .filter(|row| {
            problem_cols.iter().any(|col| {
                result
                    .col(col)
                    .and_then(|idx| row.get(idx))
                    .and_then(|value| value.parse::<i64>().ok())
                    .map(|value| value > 0)
                    .unwrap_or(false)
            })
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_frontends() {
        let snapshot = frontends_from_result(QueryResult {
            columns: vec![
                "Name".into(),
                "Host".into(),
                "Alive".into(),
                "IsMaster".into(),
            ],
            rows: vec![
                vec![
                    "fe1".into(),
                    "127.0.0.1".into(),
                    "true".into(),
                    "true".into(),
                ],
                vec![
                    "fe2".into(),
                    "127.0.0.2".into(),
                    "false".into(),
                    "false".into(),
                ],
            ],
        });

        assert_eq!(snapshot.summary.total, 2);
        assert_eq!(snapshot.summary.alive, 1);
        assert_eq!(snapshot.summary.down, 1);
        assert!(snapshot.nodes[0].is_master);
    }

    #[test]
    fn summarizes_backends() {
        let snapshot = backends_from_result(QueryResult {
            columns: vec![
                "BackendId".into(),
                "Host".into(),
                "Alive".into(),
                "SystemDecommissioned".into(),
            ],
            rows: vec![
                vec![
                    "1".into(),
                    "127.0.0.1".into(),
                    "true".into(),
                    "false".into(),
                ],
                vec![
                    "2".into(),
                    "127.0.0.2".into(),
                    "false".into(),
                    "true".into(),
                ],
            ],
        });

        assert_eq!(snapshot.summary.total, 2);
        assert_eq!(snapshot.summary.alive, 1);
        assert_eq!(snapshot.summary.down, 1);
        assert_eq!(snapshot.summary.decommissioning, 1);
    }

    #[test]
    fn counts_tablet_problem_rows() {
        let result = QueryResult {
            columns: vec![
                "DbName".into(),
                "ReplicaMissingNum".into(),
                "VersionIncompleteNum".into(),
            ],
            rows: vec![
                vec!["ok".into(), "0".into(), "0".into()],
                vec!["bad".into(), "1".into(), "0".into()],
                vec!["also_bad".into(), "0".into(), "2".into()],
            ],
        };

        assert_eq!(count_tablet_problem_rows(&result), 2);
    }
}
