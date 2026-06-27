use crate::client::FeClient;
use crate::config::Config;

use super::data::{self, ClusterSnapshot};
use super::sql::SqlPane;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Overview,
    Frontends,
    Backends,
    Sql,
    Ops,
    Logs,
}

impl Tab {
    pub const ALL: [Self; 6] = [
        Self::Overview,
        Self::Frontends,
        Self::Backends,
        Self::Sql,
        Self::Ops,
        Self::Logs,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Overview => "Overview",
            Self::Frontends => "FE",
            Self::Backends => "BE",
            Self::Sql => "SQL",
            Self::Ops => "Ops",
            Self::Logs => "Logs",
        }
    }
}

pub struct App {
    pub cfg: Config,
    pub active_tab: usize,
    pub snapshot: Option<ClusterSnapshot>,
    pub status: String,
    pub should_quit: bool,
    pub sql: SqlPane,
    pub table_offset: usize,
}

impl App {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            active_tab: 0,
            snapshot: None,
            status: "Loading cluster snapshot...".to_string(),
            should_quit: false,
            sql: SqlPane::default(),
            table_offset: 0,
        }
    }

    pub fn tab(&self) -> Tab {
        Tab::ALL[self.active_tab]
    }

    pub fn next_tab(&mut self) {
        self.active_tab = (self.active_tab + 1) % Tab::ALL.len();
        self.table_offset = 0;
    }

    pub fn previous_tab(&mut self) {
        self.active_tab = if self.active_tab == 0 {
            Tab::ALL.len() - 1
        } else {
            self.active_tab - 1
        };
        self.table_offset = 0;
    }

    pub fn scroll_down(&mut self) {
        match self.tab() {
            Tab::Sql => self.sql.scroll_down(),
            _ => self.table_offset = self.table_offset.saturating_add(1),
        }
    }

    pub fn scroll_up(&mut self) {
        match self.tab() {
            Tab::Sql => self.sql.scroll_up(),
            _ => self.table_offset = self.table_offset.saturating_sub(1),
        }
    }

    pub async fn refresh(&mut self, fe: &FeClient) {
        match data::load_cluster_snapshot(fe).await {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.status = "Snapshot refreshed".to_string();
            }
            Err(err) => {
                self.status = format!("Refresh failed: {err:#}");
            }
        }
    }

    pub async fn run_sql(&mut self, fe: &FeClient) {
        self.sql.run(fe).await;
        self.status = self.sql.status.clone();
    }
}
