use anyhow::{Context, Result};
use mysql_async::prelude::*;
use mysql_async::{Opts, OptsBuilder, Pool, Row, Value};

use crate::config::Config;

/// A tabular result set returned from FE: column headers plus rows of stringified cells.
#[derive(Debug, Clone, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl QueryResult {
    /// Index of a column by (case-insensitive) name.
    pub fn col(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c.eq_ignore_ascii_case(name))
    }

    /// Get a cell value by row index and column name.
    #[allow(dead_code)]
    pub fn get(&self, row: usize, name: &str) -> Option<&str> {
        let idx = self.col(name)?;
        self.rows.get(row).and_then(|r| r.get(idx)).map(|s| s.as_str())
    }
}

/// Client for talking to a Doris FE over the MySQL protocol and HTTP.
/// Some HTTP fields/methods are reserved for upcoming REST-based features.
#[allow(dead_code)]
pub struct FeClient {
    pool: Pool,
    http: reqwest::Client,
    http_base: String,
    user: String,
    password: String,
}

impl FeClient {
    pub fn connect(cfg: &Config) -> Result<Self> {
        let opts: Opts = OptsBuilder::default()
            .ip_or_hostname(cfg.fe.host.clone())
            .tcp_port(cfg.fe.query_port)
            .user(Some(cfg.fe.user.clone()))
            .pass(Some(cfg.fe.password.clone()))
            // Doris exposes information_schema; connect there to avoid db selection issues.
            .db_name(Some("information_schema"))
            .into();
        let pool = Pool::new(opts);

        let http = reqwest::Client::builder()
            .build()
            .context("failed to build HTTP client")?;
        let http_base = format!("http://{}:{}", cfg.fe.host, cfg.fe.http_port);

        Ok(Self {
            pool,
            http,
            http_base,
            user: cfg.fe.user.clone(),
            password: cfg.fe.password.clone(),
        })
    }

    /// Run a query and collect the result into a [`QueryResult`].
    pub async fn query(&self, sql: &str) -> Result<QueryResult> {
        let mut conn = self
            .pool
            .get_conn()
            .await
            .context("failed to get FE connection (check host/port/credentials)")?;
        let rows: Vec<Row> = conn
            .query(sql)
            .await
            .with_context(|| format!("query failed: {sql}"))?;

        let mut result = QueryResult::default();
        if let Some(first) = rows.first() {
            result.columns = first
                .columns_ref()
                .iter()
                .map(|c| c.name_str().to_string())
                .collect();
        }
        for row in &rows {
            let mut out = Vec::with_capacity(row.len());
            for i in 0..row.len() {
                out.push(value_to_string(row.as_ref(i).unwrap_or(&Value::NULL)));
            }
            result.rows.push(out);
        }
        Ok(result)
    }

    /// Execute a statement that returns no rows (DDL / ADMIN / ALTER SYSTEM).
    pub async fn exec(&self, sql: &str) -> Result<()> {
        let mut conn = self
            .pool
            .get_conn()
            .await
            .context("failed to get FE connection (check host/port/credentials)")?;
        conn.query_drop(sql)
            .await
            .with_context(|| format!("statement failed: {sql}"))?;
        Ok(())
    }

    /// Perform a GET against the FE HTTP API, returning the body as text.
    #[allow(dead_code)]
    pub async fn http_get(&self, path: &str) -> Result<String> {
        let url = format!("{}/{}", self.http_base, path.trim_start_matches('/'));
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .with_context(|| format!("HTTP GET failed: {url}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::ensure!(status.is_success(), "HTTP {status} from {url}: {body}");
        Ok(body)
    }

    #[allow(dead_code)]
    pub async fn disconnect(self) -> Result<()> {
        self.pool.disconnect().await.ok();
        Ok(())
    }
}

fn value_to_string(v: &Value) -> String {
    match v {
        Value::NULL => "NULL".to_string(),
        Value::Bytes(b) => String::from_utf8_lossy(b).to_string(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Date(y, m, d, h, mi, s, _us) => {
            format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02}")
        }
        Value::Time(neg, d, h, mi, s, _us) => {
            let sign = if *neg { "-" } else { "" };
            format!("{sign}{}d {h:02}:{mi:02}:{s:02}", d)
        }
    }
}
