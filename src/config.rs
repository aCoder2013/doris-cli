use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Storage-compute architecture per
/// <https://doris.apache.org/zh-CN/docs/4.x/install/choosing-deployment-mode>.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployArchitecture {
    /// Integrated storage-compute (FE + BE on local disks).
    #[default]
    Integrated,
    /// Separated storage-compute (cloud mode: Meta Service + shared storage).
    Separated,
}

/// How the cluster is operated. Only [`DeployMethod::Manual`] is fully automated by dcli.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployMethod {
    #[default]
    Manual,
    Kubernetes,
    Cloud,
}

impl DeployArchitecture {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Integrated => "integrated (存算一体)",
            Self::Separated => "separated (存算分离)",
        }
    }

    pub fn doc_url(&self) -> &'static str {
        match self {
            Self::Integrated => {
                "https://doris.apache.org/zh-CN/docs/4.x/install/deploy-manually/integrated-storage-compute-deploy-manually"
            }
            Self::Separated => {
                "https://doris.apache.org/zh-CN/docs/4.x/install/deploy-manually/separating-storage-compute-deploy-manually"
            }
        }
    }
}

impl DeployMethod {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Manual => "manual (手动部署)",
            Self::Kubernetes => "kubernetes (Doris Operator)",
            Self::Cloud => "cloud (云平台)",
        }
    }

    pub fn doc_url(&self) -> &'static str {
        match self {
            Self::Manual => {
                "https://doris.apache.org/zh-CN/docs/4.x/install/deploy-manually/intro"
            }
            Self::Kubernetes => "https://doris.apache.org/zh-CN/docs/4.x/install/deploy-on-kubernetes/intro",
            Self::Cloud => "https://doris.apache.org/zh-CN/docs/4.x/install/deploy-on-cloud/intro",
        }
    }

    pub fn dcli_automated(&self) -> bool {
        matches!(self, Self::Manual)
    }
}

/// Top-level cluster configuration, normally loaded from `~/.doris-cli/cluster.yaml`
/// or a path passed via `--config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Logical cluster name (for display only).
    #[serde(default = "default_cluster_name")]
    pub name: String,
    pub fe: FeConfig,
    #[serde(default)]
    pub be: BeConfig,
    #[serde(default)]
    pub ssh: Option<SshConfig>,
    #[serde(default)]
    pub deploy: Option<DeployConfig>,
    /// Cluster topology: which hosts are FE (and the leader) and which are BE.
    #[serde(default)]
    pub topology: Option<Topology>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Topology {
    #[serde(default)]
    pub frontends: Vec<FeNode>,
    #[serde(default)]
    pub backends: Vec<BeNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeNode {
    pub host: String,
    /// One of: leader | follower | observer.
    #[serde(default = "default_fe_role")]
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeNode {
    pub host: String,
}

impl Topology {
    /// The designated leader FE host, falling back to the first frontend.
    pub fn leader(&self) -> Option<&FeNode> {
        self.frontends
            .iter()
            .find(|f| f.role.eq_ignore_ascii_case("leader"))
            .or_else(|| self.frontends.first())
    }

    /// FEs that are not the leader (followers/observers), to be added via --helper.
    pub fn non_leader_fes(&self) -> Vec<&FeNode> {
        let leader = self.leader().map(|l| l.host.clone());
        self.frontends
            .iter()
            .filter(|f| Some(&f.host) != leader.as_ref())
            .collect()
    }

    /// All unique hosts in the topology.
    pub fn all_hosts(&self) -> Vec<String> {
        let mut hosts: Vec<String> = self
            .frontends
            .iter()
            .map(|f| f.host.clone())
            .chain(self.backends.iter().map(|b| b.host.clone()))
            .collect();
        hosts.sort();
        hosts.dedup();
        hosts
    }
}

fn default_fe_role() -> String {
    "follower".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeConfig {
    /// FE host used for the MySQL/query connection.
    pub host: String,
    /// MySQL protocol query port (default 9030).
    #[serde(default = "default_query_port")]
    pub query_port: u16,
    /// FE HTTP port (default 8030).
    #[serde(default = "default_fe_http_port")]
    pub http_port: u16,
    /// FE edit-log port, needed when adding follower/observer FEs (default 9010).
    #[serde(default = "default_edit_log_port")]
    pub edit_log_port: u16,
    #[serde(default = "default_user")]
    pub user: String,
    #[serde(default)]
    pub password: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeConfig {
    /// BE heartbeat service port, used in ADD/DROP BACKEND (default 9050).
    #[serde(default = "default_heartbeat_port")]
    pub heartbeat_port: u16,
    /// BE HTTP/webserver port (default 8040).
    #[serde(default = "default_be_http_port")]
    pub http_port: u16,
}

impl Default for BeConfig {
    fn default() -> Self {
        Self {
            heartbeat_port: default_heartbeat_port(),
            http_port: default_be_http_port(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshConfig {
    #[serde(default = "default_ssh_user")]
    pub user: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    /// Path to the private key. Falls back to the system ssh agent/defaults if unset.
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeployConfig {
    /// Integrated vs separated storage-compute architecture.
    #[serde(default)]
    pub architecture: DeployArchitecture,
    /// Manual vs Kubernetes vs cloud deployment method.
    #[serde(default)]
    pub method: DeployMethod,
    /// Doris release version (e.g. 4.1.1). Informational; package may override.
    #[serde(default)]
    pub version: Option<String>,
    /// Binary architecture: x64 | x64-noavx2 | arm64.
    #[serde(default)]
    pub arch: Option<String>,
    /// Base directory where Doris is installed on remote hosts.
    #[serde(default = "default_install_dir")]
    pub install_dir: String,
    /// Local path or http(s) URL to the Doris distribution tarball
    /// (apache-doris-x.y.z-bin.tar.gz).
    #[serde(default)]
    pub package: Option<String>,
    /// JAVA_HOME exported before start scripts run (Doris requires it).
    #[serde(default)]
    pub java_home: Option<String>,
    /// FE metadata dir; defaults to <install_dir>/fe/doris-meta on the remote.
    #[serde(default)]
    pub meta_dir: Option<String>,
    /// BE storage root path(s); defaults to <install_dir>/be/storage on the remote.
    #[serde(default)]
    pub be_storage: Option<String>,
    /// CIDR for priority_networks (recommended on multi-NIC hosts), e.g. 10.0.0.0/24.
    #[serde(default)]
    pub priority_networks: Option<String>,
    /// Extra settings required for separated (cloud) mode.
    #[serde(default)]
    pub separated: Option<SeparatedDeployConfig>,
}

/// Cloud / separated-mode settings from the official manual deploy guide.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeparatedDeployConfig {
    /// Unique cluster id (fe.conf `cluster_id`). Auto-generated if unset.
    #[serde(default)]
    pub cluster_id: Option<i64>,
    /// Meta Service endpoint(s), e.g. `10.0.0.1:5000` or comma-separated list.
    pub meta_service_endpoint: String,
    /// Hosts that run Meta Service (for plan/status; optional).
    #[serde(default)]
    pub meta_service_hosts: Vec<String>,
    /// BE file_cache_path JSON for be.conf (separated mode).
    #[serde(default)]
    pub file_cache_path: Option<String>,
    /// FoundationDB cluster connection string (for precheck hints).
    #[serde(default)]
    pub fdb_cluster: Option<String>,
    /// Storage Vault to create after bootstrap (optional).
    #[serde(default)]
    pub storage_vault: Option<StorageVaultConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageVaultConfig {
    pub name: String,
    /// `S3` or `hdfs`.
    #[serde(rename = "type")]
    pub vault_type: String,
    #[serde(default)]
    pub properties: BTreeMap<String, String>,
}

fn default_cluster_name() -> String {
    "default".into()
}
fn default_query_port() -> u16 {
    9030
}
fn default_fe_http_port() -> u16 {
    8030
}
fn default_edit_log_port() -> u16 {
    9010
}
fn default_user() -> String {
    "root".into()
}
fn default_heartbeat_port() -> u16 {
    9050
}
fn default_be_http_port() -> u16 {
    8040
}
fn default_ssh_user() -> String {
    "root".into()
}
fn default_ssh_port() -> u16 {
    22
}
fn default_install_dir() -> String {
    "/opt/doris".into()
}

impl Config {
    /// Resolve the config path in priority order:
    /// explicit `--config`, then `--profile <name>`, then `DORIS_CLI_CONFIG`,
    /// then the active profile (set via `dcli profile use`), then `~/.doris-cli/cluster.yaml`.
    pub fn resolve_path(explicit: Option<&Path>, profile: Option<&str>) -> Option<PathBuf> {
        if let Some(p) = explicit {
            return Some(p.to_path_buf());
        }
        if let Some(name) = profile {
            return profile_path(name);
        }
        if let Ok(env) = std::env::var("DORIS_CLI_CONFIG") {
            return Some(PathBuf::from(env));
        }
        if let Some(name) = active_profile() {
            if let Some(p) = profile_path(&name) {
                return Some(p);
            }
        }
        dirs::home_dir().map(|h| h.join(".doris-cli").join("cluster.yaml"))
    }

    pub fn load(explicit: Option<&Path>, profile: Option<&str>) -> Result<Self> {
        let path = Self::resolve_path(explicit, profile)
            .context("could not determine config path (no home dir?)")?;
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!(
                "failed to read config file: {} (use --config or DORIS_CLI_CONFIG when running as another user, e.g. sudo)",
                path.display()
            ))?;
        let cfg: Config = serde_yaml::from_str(&raw)
            .with_context(|| format!("failed to parse config file: {}", path.display()))?;
        Ok(cfg)
    }

    /// Build a config purely from CLI flags (no file), used by `--fe-host` style overrides.
    pub fn from_fe(host: String, query_port: u16, user: String, password: String) -> Self {
        Config {
            name: default_cluster_name(),
            fe: FeConfig {
                host,
                query_port,
                http_port: default_fe_http_port(),
                edit_log_port: default_edit_log_port(),
                user,
                password,
            },
            be: BeConfig::default(),
            ssh: None,
            deploy: None,
            topology: None,
        }
    }

    pub fn sample_yaml() -> &'static str {
        SAMPLE_YAML
    }
}

/// Base directory for doris-cli state (`~/.doris-cli`).
pub fn config_home() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".doris-cli"))
}

/// Directory holding named cluster profiles (`~/.doris-cli/profiles`).
pub fn profiles_dir() -> Option<PathBuf> {
    config_home().map(|h| h.join("profiles"))
}

/// File recording the name of the currently active profile.
fn active_profile_file() -> Option<PathBuf> {
    config_home().map(|h| h.join("active_profile"))
}

/// Path to a named profile's YAML file.
pub fn profile_path(name: &str) -> Option<PathBuf> {
    profiles_dir().map(|d| d.join(format!("{name}.yaml")))
}

/// The currently active profile name, if any.
pub fn active_profile() -> Option<String> {
    let p = active_profile_file()?;
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Set the active profile (must exist).
pub fn set_active_profile(name: &str) -> Result<()> {
    let path =
        profile_path(name).context("could not determine profiles dir (no home dir?)")?;
    anyhow::ensure!(
        path.exists(),
        "profile '{name}' does not exist (create it with `dcli profile add {name}`)"
    );
    let marker = active_profile_file().context("could not determine config home")?;
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    std::fs::write(&marker, name)
        .with_context(|| format!("failed to write {}", marker.display()))?;
    Ok(())
}

/// Clear the active profile pointer (fall back to default cluster.yaml).
pub fn clear_active_profile() -> Result<()> {
    if let Some(marker) = active_profile_file() {
        if marker.exists() {
            std::fs::remove_file(&marker)
                .with_context(|| format!("failed to remove {}", marker.display()))?;
        }
    }
    Ok(())
}

/// List available profile names (sorted), reading `*.yaml` from the profiles dir.
pub fn list_profiles() -> Result<Vec<String>> {
    let dir = match profiles_dir() {
        Some(d) => d,
        None => return Ok(Vec::new()),
    };
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("failed to read {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
            }
        }
    }
    names.sort();
    Ok(names)
}

const SAMPLE_YAML: &str = r#"# doris-cli cluster configuration
name: prod
fe:
  host: 127.0.0.1
  query_port: 9030      # MySQL protocol port
  http_port: 8030       # FE HTTP port
  edit_log_port: 9010   # used when adding follower/observer FEs
  user: root
  password: ""
be:
  heartbeat_port: 9050
  http_port: 8040
ssh:                    # optional, only needed for deploy/remote ops
  user: root
  port: 22
  key: ~/.ssh/id_ed25519   # blank = auto-detect id_ed25519/id_rsa; omit -i if missing
deploy:                 # optional, only needed for deploy
  architecture: integrated  # integrated | separated (存算一体 | 存算分离)
  method: manual            # manual | kubernetes | cloud
  version: ""               # e.g. 4.1.1 (use `dcli deploy versions` / `download`)
  arch: ""                  # x64 | x64-noavx2 | arm64 (blank = auto-detect)
  install_dir: /opt/doris
  package: ""           # local path or http(s) URL to apache-doris-x.y.z-bin.tar.gz
  java_home: ""         # e.g. /usr/lib/jvm/java-17-openjdk
  meta_dir: ""          # default <install_dir>/fe/doris-meta
  be_storage: ""        # default <install_dir>/be/storage
  priority_networks: "" # e.g. 10.0.0.0/24 (recommended on multi-NIC hosts)
  separated:            # only for architecture=separated
    cluster_id: 0       # auto-generated if 0/omitted
    meta_service_endpoint: "10.0.0.1:5000"
    meta_service_hosts: []
    file_cache_path: ""
    fdb_cluster: ""
    storage_vault:
      name: s3_vault
      type: S3
      properties:
        s3.endpoint: "https://s3.example.com"
        s3.bucket: doris
topology:               # who is FE/BE and which FE is the leader
  frontends:
    - host: 10.0.0.1
      role: leader      # leader | follower | observer
    - host: 10.0.0.2
      role: follower
  backends:
    - host: 10.0.0.11
    - host: 10.0.0.12
"#;
