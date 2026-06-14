use anyhow::{Context, Result};
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use std::path::Path;
use std::time::Duration;

use crate::client::FeClient;
use crate::config::{Config, DeployArchitecture};
use crate::output;
use crate::release;
use crate::ssh::Ssh;

/// Shared deploy context derived from config.
struct Ctx {
    cfg: Config,
    ssh: Ssh,
    install_dir: String,
    java_home: Option<String>,
    meta_dir: Option<String>,
    be_storage: Option<String>,
    priority_networks: Option<String>,
}

impl Ctx {
    fn new(cfg: &Config) -> Result<Self> {
        let deploy = cfg.deploy.clone();
        let install_dir = deploy
            .as_ref()
            .map(|d| d.install_dir.clone())
            .unwrap_or_else(|| "/opt/doris".into());
        Ok(Ctx {
            ssh: Ssh::from_cfg(cfg.ssh.as_ref()),
            install_dir,
            java_home: deploy.as_ref().and_then(|d| non_empty(&d.java_home)),
            meta_dir: deploy.as_ref().and_then(|d| non_empty(&d.meta_dir)),
            be_storage: deploy.as_ref().and_then(|d| non_empty(&d.be_storage)),
            priority_networks: deploy.as_ref().and_then(|d| non_empty(&d.priority_networks)),
            cfg: cfg.clone(),
        })
    }

    fn topology(&self) -> Result<&crate::config::Topology> {
        self.cfg
            .topology
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("no topology configured; run `dcli deploy init` first"))
    }

    /// Build a FeClient pointed at the leader FE.
    fn leader_client(&self) -> Result<FeClient> {
        let topo = self.topology()?;
        let leader = topo
            .leader()
            .ok_or_else(|| anyhow::anyhow!("topology has no frontends"))?;
        let mut cfg = self.cfg.clone();
        cfg.fe.host = leader.host.clone();
        FeClient::connect(&cfg)
    }

    fn is_separated(&self) -> bool {
        self.cfg
            .deploy
            .as_ref()
            .map(|d| d.architecture == DeployArchitecture::Separated)
            .unwrap_or(false)
    }

    fn separated(&self) -> Option<&crate::config::SeparatedDeployConfig> {
        self.cfg.deploy.as_ref()?.separated.as_ref()
    }

    fn export_java(&self) -> String {
        match &self.java_home {
            Some(j) => format!("export JAVA_HOME=\"{j}\"\n"),
            None => String::new(),
        }
    }
}

fn non_empty(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

// ----------------------------------------------------------------------------
// install
// ----------------------------------------------------------------------------

pub async fn install(cfg: &Config) -> Result<()> {
    let ctx = Ctx::new(cfg)?;
    let topo = ctx.topology()?.clone();
    let package = cfg
        .deploy
        .as_ref()
        .and_then(|d| non_empty(&d.package))
        .ok_or_else(|| anyhow::anyhow!("deploy.package is not set in config"))?;

    if !package.starts_with("http://") && !package.starts_with("https://") {
        release::ensure_local_package(Path::new(&package))?;
    }

    let hosts = topo.all_hosts();
    output::info(&format!(
        "distributing & extracting package to {} host(s): {}",
        hosts.len(),
        hosts.join(", ")
    ));

    // 1. Distribute + extract on every host (concurrently).
    let mut set = tokio::task::JoinSet::new();
    for host in hosts {
        let ssh = ctx.ssh.clone();
        let install_dir = ctx.install_dir.clone();
        let package = package.clone();
        set.spawn(async move {
            let r = distribute_and_extract(&ssh, &host, &install_dir, &package).await;
            (host, r)
        });
    }
    while let Some(joined) = set.join_next().await {
        let (host, r) = joined?;
        r.with_context(|| format!("install failed on {host}"))?;
        output::ok(&format!("extracted on {host}"));
    }

    // 2. Render fe.conf on FE hosts.
    for fe in &topo.frontends {
        let script = render_fe_conf_script(&ctx);
        ctx.ssh
            .run_checked(&fe.host, &script)
            .await
            .with_context(|| format!("rendering fe.conf on {}", fe.host))?;
        output::ok(&format!("rendered fe.conf on {}", fe.host));
    }

    // 3. Render be.conf on BE hosts.
    for be in &topo.backends {
        let script = render_be_conf_script(&ctx);
        ctx.ssh
            .run_checked(&be.host, &script)
            .await
            .with_context(|| format!("rendering be.conf on {}", be.host))?;
        output::ok(&format!("rendered be.conf on {}", be.host));
    }

    output::ok("install complete; run `dcli deploy start` next");
    Ok(())
}

async fn distribute_and_extract(
    ssh: &Ssh,
    host: &str,
    install_dir: &str,
    package: &str,
) -> Result<()> {
    let is_url = package.starts_with("http://") || package.starts_with("https://");
    let basename = Path::new(package)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "doris-package.tar.gz".into());
    let remote_pkg = format!("/tmp/{basename}");

    if is_url {
        let fetch = format!(
            "set -e\ncurl -fSL -o '{remote_pkg}' '{package}' 2>/dev/null || wget -q -O '{remote_pkg}' '{package}'\n"
        );
        ssh.run_checked(host, &fetch).await?;
    } else {
        anyhow::ensure!(
            Path::new(package).exists(),
            "package file not found locally: {package}"
        );
        ssh.upload(host, package, &remote_pkg).await?;
    }

    let extract = format!(
        "set -e\nmkdir -p '{install_dir}'\ntar xzf '{remote_pkg}' -C '{install_dir}' --strip-components=1\n"
    );
    ssh.run_checked(host, &extract).await?;
    Ok(())
}

/// Remote helper that removes existing key lines then appends `key = value`.
fn set_conf_fn() -> &'static str {
    "set_conf(){ sed -i \"\\#^[[:space:]]*$2[[:space:]]*=#d\" \"$1\"; echo \"$2 = $3\" >> \"$1\"; }\n"
}

fn render_fe_conf_script(ctx: &Ctx) -> String {
    let f = format!("{}/fe/conf/fe.conf", ctx.install_dir);
    let mut s = String::from("set -e\n");
    s.push_str(&format!("touch '{f}'\n"));
    s.push_str(set_conf_fn());
    if let Some(pn) = &ctx.priority_networks {
        s.push_str(&format!("set_conf '{f}' priority_networks '{pn}'\n"));
    }
    if let Some(md) = &ctx.meta_dir {
        s.push_str(&format!("set_conf '{f}' meta_dir '{md}'\n"));
    }
    s.push_str(&format!(
        "set_conf '{f}' http_port '{}'\n",
        ctx.cfg.fe.http_port
    ));
    s.push_str(&format!(
        "set_conf '{f}' query_port '{}'\n",
        ctx.cfg.fe.query_port
    ));
    s.push_str(&format!(
        "set_conf '{f}' edit_log_port '{}'\n",
        ctx.cfg.fe.edit_log_port
    ));
    if ctx.is_separated() {
        s.push_str(&format!("set_conf '{f}' deploy_mode 'cloud'\n"));
        if let Some(sep) = ctx.separated() {
            if let Some(id) = sep.cluster_id {
                s.push_str(&format!("set_conf '{f}' cluster_id '{id}'\n"));
            }
            s.push_str(&format!(
                "set_conf '{f}' meta_service_endpoint '{}'\n",
                sep.meta_service_endpoint
            ));
        }
    }
    s
}

fn render_be_conf_script(ctx: &Ctx) -> String {
    let f = format!("{}/be/conf/be.conf", ctx.install_dir);
    let mut s = String::from("set -e\n");
    s.push_str(&format!("touch '{f}'\n"));
    s.push_str(set_conf_fn());
    if let Some(pn) = &ctx.priority_networks {
        s.push_str(&format!("set_conf '{f}' priority_networks '{pn}'\n"));
    }
    if let Some(st) = &ctx.be_storage {
        s.push_str(&format!("mkdir -p '{st}'\n"));
        s.push_str(&format!("set_conf '{f}' storage_root_path '{st}'\n"));
    }
    if ctx.is_separated() {
        s.push_str(&format!("set_conf '{f}' deploy_mode 'cloud'\n"));
        let cache = ctx
            .separated()
            .and_then(|sep| non_empty(&sep.file_cache_path))
            .unwrap_or_else(|| {
                format!(
                    "[{{\"path\":\"{}/file_cache\",\"total_size\":-1}}]",
                    ctx.install_dir
                )
            });
        s.push_str(&format!("mkdir -p '{}/file_cache'\n", ctx.install_dir));
        s.push_str(&format!("set_conf '{f}' file_cache_path '{cache}'\n"));
    }
    s.push_str(&format!(
        "set_conf '{f}' webserver_port '{}'\n",
        ctx.cfg.be.http_port
    ));
    s.push_str(&format!(
        "set_conf '{f}' heartbeat_service_port '{}'\n",
        ctx.cfg.be.heartbeat_port
    ));
    s
}

// ----------------------------------------------------------------------------
// start
// ----------------------------------------------------------------------------

pub async fn start(cfg: &Config) -> Result<()> {
    let ctx = Ctx::new(cfg)?;
    let topo = ctx.topology()?.clone();
    let leader = topo
        .leader()
        .ok_or_else(|| anyhow::anyhow!("topology has no frontends"))?
        .clone();

    // 1. Start the leader FE.
    output::info(&format!("starting leader FE on {}", leader.host));
    let start_leader = format!(
        "{}cd '{}/fe'\nbin/start_fe.sh --daemon\n",
        ctx.export_java(),
        ctx.install_dir
    );
    ctx.ssh
        .run_checked(&leader.host, &start_leader)
        .await
        .with_context(|| format!("starting leader FE on {}", leader.host))?;

    // 2. Wait for the leader to accept SQL.
    output::info("waiting for leader FE to become ready...");
    let fe = wait_for_leader(&ctx).await?;
    output::ok("leader FE is ready");

    // 3. Register + start the remaining FEs (followers/observers).
    let helper = format!("{}:{}", leader.host, cfg.fe.edit_log_port);
    for node in topo.non_leader_fes() {
        let role = match node.role.to_ascii_lowercase().as_str() {
            "observer" => "OBSERVER",
            _ => "FOLLOWER",
        };
        let addr = format!("{}:{}", node.host, cfg.fe.edit_log_port);
        fe.exec(&format!("ALTER SYSTEM ADD {role} \"{addr}\""))
            .await
            .with_context(|| format!("registering {role} {addr}"))?;
        let start_fe = format!(
            "{}cd '{}/fe'\nbin/start_fe.sh --helper '{}' --daemon\n",
            ctx.export_java(),
            ctx.install_dir,
            helper
        );
        ctx.ssh
            .run_checked(&node.host, &start_fe)
            .await
            .with_context(|| format!("starting {role} on {}", node.host))?;
        output::ok(&format!("started {role} {}", node.host));
    }

    // 4. Start all BEs, then register them.
    let mut set = tokio::task::JoinSet::new();
    for be in &topo.backends {
        let ssh = ctx.ssh.clone();
        let host = be.host.clone();
        let start_be = format!(
            "{}cd '{}/be'\nbin/start_be.sh --daemon\n",
            ctx.export_java(),
            ctx.install_dir
        );
        set.spawn(async move {
            let r = ssh.run_checked(&host, &start_be).await;
            (host, r)
        });
    }
    while let Some(joined) = set.join_next().await {
        let (host, r) = joined?;
        r.with_context(|| format!("starting BE on {host}"))?;
        output::ok(&format!("started BE {host}"));
    }

    if !topo.backends.is_empty() {
        let list = topo
            .backends
            .iter()
            .map(|b| format!("\"{}:{}\"", b.host, cfg.be.heartbeat_port))
            .collect::<Vec<_>>()
            .join(", ");
        fe.exec(&format!("ALTER SYSTEM ADD BACKEND {list}"))
            .await
            .context("registering backends")?;
        output::ok(&format!("registered {} backend(s)", topo.backends.len()));
    }

    if ctx.is_separated() {
        if let Some(sep) = ctx.separated() {
            if let Some(vault) = &sep.storage_vault {
                create_storage_vault(&fe, vault).await?;
            } else {
                output::warn(
                    "separated mode: no storage_vault configured; create one with CREATE STORAGE VAULT",
                );
            }
        }
    }

    output::ok("cluster started; verify with `dcli cluster status`");
    Ok(())
}

async fn create_storage_vault(
    fe: &FeClient,
    vault: &crate::config::StorageVaultConfig,
) -> Result<()> {
    let mut props: Vec<String> = vault
        .properties
        .iter()
        .map(|(k, v)| format!("\"{k}\"=\"{v}\""))
        .collect();
    props.insert(0, format!("\"type\"=\"{}\"", vault.vault_type));
    let sql = format!(
        "CREATE STORAGE VAULT IF NOT EXISTS {} PROPERTIES ({})",
        vault.name,
        props.join(", ")
    );
    fe.exec(&sql)
        .await
        .with_context(|| format!("creating storage vault '{}'", vault.name))?;
    fe.exec(&format!(
        "SET {} AS DEFAULT STORAGE VAULT",
        vault.name
    ))
    .await
    .with_context(|| format!("setting default storage vault '{}'", vault.name))?;
    output::ok(&format!("created storage vault '{}'", vault.name));
    Ok(())
}

async fn wait_for_leader(ctx: &Ctx) -> Result<FeClient> {
    let mut last_err = None;
    for attempt in 1..=30 {
        match ctx.leader_client() {
            Ok(fe) => match fe.query("SHOW FRONTENDS").await {
                Ok(_) => return Ok(fe),
                Err(e) => last_err = Some(e),
            },
            Err(e) => last_err = Some(e),
        }
        if attempt % 5 == 0 {
            output::info(&format!("still waiting for leader FE ({attempt}/30)"));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("leader FE did not become ready in time")))
}

// ----------------------------------------------------------------------------
// stop / status
// ----------------------------------------------------------------------------

pub async fn stop(cfg: &Config) -> Result<()> {
    let ctx = Ctx::new(cfg)?;
    let topo = ctx.topology()?.clone();

    for be in &topo.backends {
        let script = format!("cd '{}/be' && bin/stop_be.sh || true\n", ctx.install_dir);
        ctx.ssh.run(&be.host, &script).await.ok();
        output::ok(&format!("stopped BE {}", be.host));
    }
    for fe in &topo.frontends {
        let script = format!("cd '{}/fe' && bin/stop_fe.sh || true\n", ctx.install_dir);
        ctx.ssh.run(&fe.host, &script).await.ok();
        output::ok(&format!("stopped FE {}", fe.host));
    }
    Ok(())
}

pub async fn status(cfg: &Config) -> Result<()> {
    let ctx = Ctx::new(cfg)?;
    let topo = ctx.topology()?.clone();

    let script = "fe=$(pgrep -af 'DorisFE|PaloFe' | head -1)\n\
                  be=$(pgrep -af 'doris_be|palo_be' | head -1)\n\
                  echo \"fe=$([ -n \"$fe\" ] && echo running || echo stopped)\"\n\
                  echo \"be=$([ -n \"$be\" ] && echo running || echo stopped)\"\n";

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Host", "Roles", "FE", "BE"]);

    for host in topo.all_hosts() {
        let is_fe = topo.frontends.iter().any(|f| f.host == host);
        let is_be = topo.backends.iter().any(|b| b.host == host);
        let role = topo
            .frontends
            .iter()
            .find(|f| f.host == host)
            .map(|f| f.role.clone());
        let out = ctx.ssh.run(&host, script).await;
        let (fe_state, be_state) = match out {
            Ok(o) if o.ok() => {
                let mut fe_s = "-".to_string();
                let mut be_s = "-".to_string();
                for line in o.stdout.lines() {
                    if let Some(v) = line.strip_prefix("fe=") {
                        if is_fe {
                            fe_s = v.to_string();
                        }
                    } else if let Some(v) = line.strip_prefix("be=") {
                        if is_be {
                            be_s = v.to_string();
                        }
                    }
                }
                (fe_s, be_s)
            }
            _ => ("unreachable".into(), "unreachable".into()),
        };
        let mut roles = Vec::new();
        if is_fe {
            roles.push(format!("FE/{}", role.unwrap_or_else(|| "follower".into())));
        }
        if is_be {
            roles.push("BE".into());
        }
        table.add_row(vec![host, roles.join(","), fe_state, be_state]);
    }
    println!("{table}");
    Ok(())
}
