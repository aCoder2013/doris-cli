use clap::{Args, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "dcli",
    version,
    about = "Full-scenario operations CLI for Apache Doris (deploy · scale · maintain)",
    propagate_version = true
)]
pub struct Cli {
    /// Path to the cluster config file (defaults to ~/.doris-cli/cluster.yaml).
    #[arg(short, long, global = true)]
    pub config: Option<PathBuf>,

    /// Use a named cluster profile (~/.doris-cli/profiles/<name>.yaml) for this command.
    #[arg(short = 'p', long, global = true)]
    pub profile: Option<String>,

    /// Output format: table or json.
    #[arg(short, long, global = true, default_value = "table")]
    pub format: String,

    /// Override FE host (skips needing a config file for read-only commands).
    #[arg(long, global = true)]
    pub fe_host: Option<String>,

    /// Override FE query (MySQL) port.
    #[arg(long, global = true, default_value_t = 9030)]
    pub fe_port: u16,

    /// Override FE user.
    #[arg(long, global = true, default_value = "root")]
    pub user: String,

    /// Override FE password.
    #[arg(long, global = true, default_value = "")]
    pub password: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage the doris-cli configuration file.
    #[command(subcommand)]
    Config(ConfigCmd),

    /// Manage named cluster profiles (multi-cluster switching).
    #[command(subcommand)]
    Profile(ProfileCmd),

    /// Inspect overall cluster state (frontends & backends).
    #[command(subcommand)]
    Cluster(ClusterCmd),

    /// Day-2 operations: tablet health, repair, balance, decommission status.
    #[command(subcommand)]
    Ops(OpsCmd),

    /// Scale the cluster up or down (backends & frontends).
    #[command(subcommand)]
    Scale(ScaleCmd),

    /// Deploy / lifecycle management of FE & BE processes (experimental).
    #[command(subcommand)]
    Deploy(DeployCmd),
}

#[derive(Debug, Subcommand)]
pub enum ConfigCmd {
    /// Write a sample config to the default location.
    Init {
        /// Overwrite if the file already exists.
        #[arg(long)]
        force: bool,
    },
    /// Print the resolved configuration.
    Show,
}

#[derive(Debug, Subcommand)]
pub enum ProfileCmd {
    /// List available profiles, marking the active one.
    List,
    /// Show the currently active profile.
    Current,
    /// Create a new profile (writes a sample config to profiles/<name>.yaml).
    Add {
        /// Profile name.
        name: String,
        /// Seed the new profile from the current config instead of the sample.
        #[arg(long)]
        from_current: bool,
        /// Overwrite if the profile already exists.
        #[arg(long)]
        force: bool,
    },
    /// Switch the active profile used by subsequent commands.
    Use {
        /// Profile name.
        name: String,
    },
    /// Remove a profile.
    Remove {
        /// Profile name.
        name: String,
        /// Skip the confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Print the resolved configuration of a profile (defaults to active/current).
    Show {
        /// Profile name (defaults to the active profile).
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ClusterCmd {
    /// Summary of FE and BE health.
    Status,
    /// List frontends (SHOW FRONTENDS).
    Frontends,
    /// List backends (SHOW BACKENDS).
    Backends,
}

#[derive(Debug, Subcommand)]
pub enum OpsCmd {
    /// Cluster-wide tablet health summary (replica miss / version miss / unhealthy).
    Health,
    /// List unhealthy / version-incomplete tablets, optionally scoped to a table.
    Tablets(TabletsArgs),
    /// Repair a table/partitions (ADMIN REPAIR TABLE) to recover missing replicas/versions.
    Repair(RepairArgs),
    /// Cancel a pending repair (ADMIN CANCEL REPAIR TABLE).
    CancelRepair(RepairArgs),
    /// Show progress of backends currently being decommissioned.
    DecommissionStatus,
    /// Show or toggle tablet load-balancing (disable_balance).
    #[command(subcommand)]
    Balance(BalanceCmd),
    /// Inspect missing rowset versions via BE compaction/show (read-only).
    VersionGaps(VersionGapsArgs),
    /// Pad empty rowsets on BE to close version gaps (single-replica recovery; data in gaps is lost).
    PadRowset(PadRowsetArgs),
}

#[derive(Debug, Args)]
pub struct TabletsArgs {
    /// Database name.
    #[arg(long)]
    pub db: String,
    /// Table name.
    #[arg(long)]
    pub table: String,
    /// Only show tablets that are not healthy (default: true).
    #[arg(long, default_value_t = true)]
    pub unhealthy_only: bool,
}

#[derive(Debug, Args)]
pub struct VersionGapsArgs {
    /// Database name.
    #[arg(long)]
    pub db: String,
    /// Table name.
    #[arg(long)]
    pub table: String,
    /// Only inspect replicas with STATUS != OK (default: true).
    #[arg(long, default_value_t = true)]
    pub unhealthy_only: bool,
}

#[derive(Debug, Args)]
pub struct PadRowsetArgs {
    /// Database name (with --table; omit when using --tablet-id + --backend-id).
    #[arg(long)]
    pub db: Option<String>,
    /// Table name.
    #[arg(long)]
    pub table: Option<String>,
    /// Target a single tablet (requires --backend-id).
    #[arg(long)]
    pub tablet_id: Option<String>,
    /// Backend id for --tablet-id.
    #[arg(long)]
    pub backend_id: Option<String>,
    /// Show planned pads only; do not call BE pad_rowset.
    #[arg(long)]
    pub dry_run: bool,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct RepairArgs {
    /// Database name.
    #[arg(long)]
    pub db: String,
    /// Table name.
    #[arg(long)]
    pub table: String,
    /// Optional comma-separated partition list. Repairs the whole table if omitted.
    #[arg(long)]
    pub partitions: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum BalanceCmd {
    /// Show current balance state.
    Show,
    /// Enable tablet balancing.
    Enable,
    /// Disable tablet balancing (useful during maintenance / scaling).
    Disable,
}

#[derive(Debug, Subcommand)]
pub enum ScaleCmd {
    /// Add backends (ALTER SYSTEM ADD BACKEND).
    AddBe(BeHostsArgs),
    /// Safely remove backends, migrating data first (ALTER SYSTEM DECOMMISSION BACKEND).
    DecommissionBe(BeHostsArgs),
    /// Cancel an in-progress decommission (ALTER SYSTEM CANCEL DECOMMISSION BACKEND).
    CancelDecommission(BeHostsArgs),
    /// Forcefully drop backends WITHOUT data migration (dangerous).
    DropBe(BeHostsArgs),
    /// Add a frontend (ALTER SYSTEM ADD FOLLOWER/OBSERVER).
    AddFe(FeNodeArgs),
    /// Drop a frontend (ALTER SYSTEM DROP FOLLOWER/OBSERVER).
    DropFe(FeNodeArgs),
}

#[derive(Debug, Args)]
pub struct BeHostsArgs {
    /// Comma-separated host list. Uses the configured heartbeat port unless host:port given.
    #[arg(long)]
    pub hosts: String,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct FeNodeArgs {
    /// FE host. Uses configured edit_log_port unless host:port given.
    #[arg(long)]
    pub host: String,
    /// FE role: follower or observer.
    #[arg(long, default_value = "follower")]
    pub role: String,
    /// Skip the confirmation prompt.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

#[derive(Debug, Subcommand)]
pub enum DeployCmd {
    /// Interactive wizard: enter FE/BE IPs, pick the leader, write topology to config.
    Init,
    /// Print the planned deploy steps without executing.
    Plan,
    /// List official Doris releases (parsed from apache/doris GitHub).
    Versions(VersionsArgs),
    /// Download an official Doris binary package to the local cache.
    Download(DownloadArgs),
    /// SSH into every host and report detected machine configuration.
    Detect,
    /// Detect + validate each host against Doris requirements (CPU/mem/JDK/sysctl/limits).
    Precheck,
    /// Distribute the package, extract it, and render fe.conf/be.conf on all hosts.
    Install(DeployStepArgs),
    /// Start the cluster: leader FE, then follower/observer FEs, then BEs (auto-registered).
    Start(DeployStepArgs),
    /// Stop FE/BE processes on all hosts.
    Stop(DeployStepArgs),
    /// Show running FE/BE processes on all hosts.
    Status,
    /// One-shot: precheck -> install -> start -> register backends.
    Bootstrap(DeployStepArgs),
}

#[derive(Debug, Args)]
pub struct VersionsArgs {
    /// Number of releases to show.
    #[arg(long, default_value_t = 15)]
    pub limit: usize,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// Doris release: exact (4.1.1), `latest`, or `stable`.
    #[arg(long = "release", default_value = "latest")]
    pub release: String,
    /// Binary arch: auto | x64 | x64-noavx2 | arm64.
    #[arg(long, default_value = "auto")]
    pub arch: String,
    /// Output directory (default: ~/.doris-cli/packages).
    #[arg(long)]
    pub output: Option<PathBuf>,
    /// Write the downloaded path into deploy.package in cluster.yaml.
    #[arg(long)]
    pub write_config: bool,
}

#[derive(Debug, Args)]
pub struct DeployStepArgs {
    /// Skip confirmation prompts.
    #[arg(short = 'y', long)]
    pub yes: bool,
}
