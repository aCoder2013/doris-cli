use anyhow::{Context, Result};
use serde::Deserialize;
use std::time::Duration;

/// Subset of the BE `/api/compaction/show` response we care about.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CompactionStatus {
    #[serde(default)]
    pub rowsets: Vec<String>,
    #[serde(default)]
    pub missing_rowsets: Vec<String>,
}

/// Client for the Doris BE HTTP API (webserver, default port 8040).
///
/// One client serves many backends; each call takes the backend's `base_url`
/// (e.g. `http://10.0.0.11:8040`) so we can fan out across the cluster.
pub struct BeClient {
    http: reqwest::Client,
    user: String,
    password: String,
}

impl BeClient {
    pub fn new(user: String, password: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .context("failed to build BE HTTP client")?;
        Ok(Self {
            http,
            user,
            password,
        })
    }

    /// Fetch compaction status (rowsets / missing_rowsets) for a tablet.
    pub async fn compaction_show(
        &self,
        base_url: &str,
        tablet_id: &str,
    ) -> Result<CompactionStatus> {
        let url = format!("{base_url}/api/compaction/show?tablet_id={tablet_id}");
        let resp = self
            .http
            .get(&url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .with_context(|| format!("BE request failed: {url}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::ensure!(status.is_success(), "HTTP {status} from {url}: {body}");
        serde_json::from_str(&body)
            .with_context(|| format!("failed to parse compaction status from {url}: {body}"))
    }

    /// Pad one empty rowset `[start_version, end_version]` on a tablet,
    /// making the version chain continuous again (the padded data is empty).
    pub async fn pad_rowset(
        &self,
        base_url: &str,
        tablet_id: &str,
        start_version: i64,
        end_version: i64,
    ) -> Result<()> {
        let url = format!(
            "{base_url}/api/pad_rowset?tablet_id={tablet_id}&start_version={start_version}&end_version={end_version}"
        );
        let resp = self
            .http
            .post(&url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .with_context(|| format!("BE request failed: {url}"))?;
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::ensure!(status.is_success(), "HTTP {status} from {url}: {body}");
        // Body looks like {"msg":"OK","code":0}; treat a non-zero code or non-OK msg as failure.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(code) = v.get("code").and_then(|c| c.as_i64()) {
                anyhow::ensure!(code == 0, "BE returned code {code}: {body}");
            } else if let Some(msg) = v.get("msg").and_then(|m| m.as_str()) {
                anyhow::ensure!(
                    msg.eq_ignore_ascii_case("OK"),
                    "BE returned msg '{msg}': {body}"
                );
            }
        }
        Ok(())
    }
}

/// Parse a leading version range like `"[35-35] 2 DATA OVERLAPPING ..."` into `(35, 35)`.
pub fn parse_version_range(s: &str) -> Option<(i64, i64)> {
    let s = s.trim();
    let open = s.find('[')?;
    let close = s[open..].find(']')? + open;
    let inner = &s[open + 1..close];
    let (a, b) = inner.split_once('-')?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_version() {
        assert_eq!(parse_version_range("[35-35]"), Some((35, 35)));
    }

    #[test]
    fn parses_range_with_trailing_metadata() {
        assert_eq!(
            parse_version_range("[40-42] 5 DATA OVERLAPPING 574.00 B"),
            Some((40, 42))
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse_version_range("not a version"), None);
        assert_eq!(parse_version_range("[abc-def]"), None);
        assert_eq!(parse_version_range("[12]"), None);
    }

    #[test]
    fn deserializes_compaction_status() {
        let json = r#"{"rowsets":["[0-34] 10 DATA"],"missing_rowsets":["[35-35]","[40-42] 2 DATA"]}"#;
        let s: CompactionStatus = serde_json::from_str(json).unwrap();
        assert_eq!(s.missing_rowsets.len(), 2);
        let ranges: Vec<_> = s
            .missing_rowsets
            .iter()
            .filter_map(|r| parse_version_range(r))
            .collect();
        assert_eq!(ranges, vec![(35, 35), (40, 42)]);
    }
}
