mod detect;
mod lifecycle;

use anyhow::{Context, Result};

use crate::cli::{Cli, DeployCmd, DeployStepArgs, DownloadArgs, VersionsArgs};
use crate::commands::resolve_config;
use crate::config::{
    BeNode, Config, DeployArchitecture, DeployConfig, DeployMethod, FeNode, SeparatedDeployConfig,
    StorageVaultConfig, Topology,
};
use crate::output::{self, confirm, prompt_line};
use crate::release::{self, BinaryArch, DorisRelease};

pub async fn run(cli: &Cli, cmd: &DeployCmd) -> Result<()> {
    match cmd {
        DeployCmd::Init => init(cli).await,
        DeployCmd::Plan => plan(cli),
        DeployCmd::Versions(a) => versions(a).await,
        DeployCmd::Download(a) => download(cli, a).await,
        DeployCmd::Detect => {
            let cfg = resolve_config(cli)?;
            let infos = detect::detect_all(&cfg).await?;
            detect::render_detect(&infos);
            Ok(())
        }
        DeployCmd::Precheck => {
            let cfg = resolve_config(cli)?;
            run_precheck(&cfg).await?;
            Ok(())
        }
        DeployCmd::Install(a) => {
            let cfg = resolve_config(cli)?;
            ensure_dcli_supported(&cfg)?;
            confirm_step(a, "Distribute package and render configs on all hosts?")?;
            lifecycle::install(&cfg).await
        }
        DeployCmd::Start(a) => {
            let cfg = resolve_config(cli)?;
            ensure_dcli_supported(&cfg)?;
            confirm_step(a, "Start FE leader, follower/observer FEs, and BEs?")?;
            lifecycle::start(&cfg).await
        }
        DeployCmd::Stop(a) => {
            let cfg = resolve_config(cli)?;
            confirm_step(a, "Stop all FE/BE processes?")?;
            lifecycle::stop(&cfg).await
        }
        DeployCmd::Status => {
            let cfg = resolve_config(cli)?;
            lifecycle::status(&cfg).await
        }
        DeployCmd::Bootstrap(a) => bootstrap(cli, a).await,
    }
}

/// Returns true if all checks passed (no FAIL).
async fn run_precheck(cfg: &Config) -> Result<bool> {
    let infos = detect::detect_all(cfg).await?;
    let any_fail = detect::render_precheck(cfg, &infos);
    if any_fail {
        output::warn("one or more checks FAILED; fix them before deploying");
    } else {
        output::ok("all hosts passed prechecks");
    }
    Ok(!any_fail)
}

async fn bootstrap(cli: &Cli, args: &DeployStepArgs) -> Result<()> {
    let cfg = resolve_config(cli)?;
    ensure_dcli_supported(&cfg)?;
    output::info("step 1/3: precheck");
    let passed = run_precheck(&cfg).await?;
    if !passed && !args.yes {
        confirm("Prechecks failed. Continue anyway?")?;
    }
    output::info("step 2/3: install");
    lifecycle::install(&cfg).await?;
    output::info("step 3/3: start");
    lifecycle::start(&cfg).await?;
    output::ok("bootstrap complete");
    Ok(())
}

fn confirm_step(args: &DeployStepArgs, prompt: &str) -> Result<()> {
    if args.yes {
        return Ok(());
    }
    confirm(prompt)
}

fn ensure_dcli_supported(cfg: &Config) -> Result<()> {
    let deploy = cfg.deploy.as_ref();
    let method = deploy.map(|d| d.method).unwrap_or_default();
    if method.dcli_automated() {
        return Ok(());
    }
    anyhow::bail!(
        "deploy.method={} is not automated by dcli (SSH manual deploy only).\n\
         See: {}",
        method.label(),
        method.doc_url()
    );
}

async fn versions(args: &VersionsArgs) -> Result<()> {
    let releases = release::fetch_releases(args.limit.max(5)).await?;
    anyhow::ensure!(!releases.is_empty(), "no Doris releases found");
    release::render_versions_table(&releases, args.limit);
    Ok(())
}

async fn download(cli: &Cli, args: &DownloadArgs) -> Result<()> {
    let releases = release::fetch_releases(30).await?;
    anyhow::ensure!(!releases.is_empty(), "no Doris releases found");

    let picked = resolve_release_choice(&releases, &args.release)?;
    let arch = if args.arch.eq_ignore_ascii_case("auto") {
        BinaryArch::detect_local()
    } else {
        BinaryArch::parse(&args.arch)
            .with_context(|| format!("unknown arch '{}'; use auto|x64|x64-noavx2|arm64", args.arch))?
    };

    let path = release::download_binary(picked, arch, args.output.as_deref()).await?;
    output::ok(&format!("saved to {}", path.display()));

    if args.write_config {
        let mut cfg = resolve_config(cli).unwrap_or_else(|_| {
            Config::from_fe("127.0.0.1".into(), 9030, "root".into(), String::new())
        });
        let deploy = cfg.deploy.get_or_insert_with(default_deploy);
        deploy.version = Some(picked.version.clone());
        deploy.arch = Some(arch.slug().to_string());
        deploy.package = Some(path.display().to_string());
        write_config(cli, &cfg)?;
        output::ok("updated deploy.package in cluster config");
    }
    Ok(())
}

fn resolve_release_choice<'a>(
    releases: &'a [DorisRelease],
    version: &str,
) -> Result<&'a DorisRelease> {
    let v = version.trim().to_ascii_lowercase();
    match v.as_str() {
        "latest" => release::pick_latest(releases).context("no latest release found"),
        "stable" => release::pick_stable(releases).context("no stable (4.0.x) release found"),
        _ => release::find_release(releases, version)
            .with_context(|| format!("version '{version}' not found; try `dcli deploy versions`")),
    }
}

fn default_deploy() -> DeployConfig {
    DeployConfig {
        architecture: DeployArchitecture::default(),
        method: DeployMethod::default(),
        version: None,
        arch: None,
        install_dir: "/opt/doris".into(),
        package: None,
        java_home: None,
        meta_dir: None,
        be_storage: None,
        priority_networks: None,
        separated: None,
    }
}

fn write_config(cli: &Cli, cfg: &Config) -> Result<()> {
    let path = Config::resolve_path(cli.config.as_deref())
        .context("could not determine config path")?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let yaml = serde_yaml::to_string(cfg)?;
    std::fs::write(&path, yaml).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn prompt_deploy_architecture() -> Result<DeployArchitecture> {
    let raw = prompt_line(
        "Architecture (integrated=存算一体 / separated=存算分离)",
        "integrated",
    )?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "integrated" | "i" | "一体" | "存算一体" => Ok(DeployArchitecture::Integrated),
        "separated" | "s" | "分离" | "存算分离" | "cloud" => Ok(DeployArchitecture::Separated),
        other => anyhow::bail!("unknown architecture '{other}'; use integrated or separated"),
    }
}

fn prompt_deploy_method() -> Result<DeployMethod> {
    let raw = prompt_line(
        "Deploy method (manual / kubernetes / cloud)",
        "manual",
    )?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "manual" | "m" | "手动" => Ok(DeployMethod::Manual),
        "kubernetes" | "k8s" | "k" => Ok(DeployMethod::Kubernetes),
        "cloud" | "c" | "云" => Ok(DeployMethod::Cloud),
        other => anyhow::bail!("unknown method '{other}'; use manual, kubernetes, or cloud"),
    }
}

async fn prompt_version_and_download() -> Result<(String, String, String)> {
    let releases = release::fetch_releases(10).await?;
    anyhow::ensure!(!releases.is_empty(), "could not fetch Doris releases");

    println!("\nAvailable Doris versions (from apache/doris releases):");
    release::render_versions_table(&releases, 8);

    let default_ver = release::pick_latest(&releases)
        .map(|r| r.version.clone())
        .unwrap_or_else(|| "latest".into());
    let ver_input = prompt_line(
        "Version to download (exact, latest, or stable)",
        &default_ver,
    )?;
    let picked = resolve_release_choice(&releases, &ver_input)?;

    let arch_input = prompt_line("Binary arch (auto/x64/x64-noavx2/arm64)", "auto")?;
    let arch = if arch_input.eq_ignore_ascii_case("auto") {
        BinaryArch::detect_local()
    } else {
        BinaryArch::parse(&arch_input)
            .with_context(|| format!("unknown arch '{arch_input}'"))?
    };

    let path = release::download_binary(picked, arch, None).await?;
    Ok((picked.version.clone(), arch.slug().to_string(), path.display().to_string()))
}

fn prompt_separated_config() -> Result<SeparatedDeployConfig> {
    println!(
        "\nSeparated (cloud) mode requires Meta Service + shared storage.\n\
         Guide: https://doris.apache.org/zh-CN/docs/4.x/install/deploy-manually/separating-storage-compute-deploy-manually\n"
    );
    let meta = prompt_line("Meta Service endpoint (host:port)", "127.0.0.1:5000")?;
    let ms_hosts = prompt_line("Meta Service hosts (comma-separated, optional)", "")?;
    let cluster_id_raw = prompt_line("cluster_id (blank = auto-generate)", "")?;
    let cluster_id = if cluster_id_raw.trim().is_empty() {
        None
    } else {
        Some(cluster_id_raw.parse().context("cluster_id must be an integer")?)
    };
    let fdb = prompt_line("FoundationDB cluster string (optional)", "")?;
    let file_cache = prompt_line("BE file_cache_path JSON (optional)", "")?;

    let vault_yn = prompt_line("Configure a Storage Vault now? (y/n)", "n")?;
    let storage_vault = if matches!(vault_yn.to_ascii_lowercase().as_str(), "y" | "yes") {
        let name = prompt_line("Storage Vault name", "s3_vault")?;
        let vault_type = prompt_line("Vault type (S3/hdfs)", "S3")?;
        let endpoint = prompt_line("s3.endpoint or fs.defaultFS", "")?;
        let bucket = prompt_line("s3.bucket (S3 only, optional)", "")?;
        let mut properties = std::collections::BTreeMap::new();
        if vault_type.eq_ignore_ascii_case("s3") {
            properties.insert("s3.endpoint".into(), endpoint);
            if !bucket.is_empty() {
                properties.insert("s3.bucket".into(), bucket);
            }
        } else {
            properties.insert("fs.defaultFS".into(), endpoint);
        }
        Some(StorageVaultConfig {
            name,
            vault_type,
            properties,
        })
    } else {
        None
    };

    Ok(SeparatedDeployConfig {
        cluster_id,
        meta_service_endpoint: meta,
        meta_service_hosts: split_hosts(&ms_hosts),
        file_cache_path: opt(file_cache),
        fdb_cluster: opt(fdb),
        storage_vault,
    })
}

fn split_hosts(s: &str) -> Vec<String> {
    s.split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

fn random_cluster_id() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0) as u64;
    ((seed >> 15) as i64) | (seed as i64 & 0x7fff)
}

/// Interactive wizard: collect FE/BE IPs, choose the leader, persist to the config file.
async fn init(cli: &Cli) -> Result<()> {
    println!("doris-cli deploy wizard — define your cluster topology\n");
    println!(
        "Deployment modes: https://doris.apache.org/zh-CN/docs/4.x/install/choosing-deployment-mode\n"
    );

    // Start from existing config if present, else a fresh skeleton.
    let mut cfg = resolve_config(cli).unwrap_or_else(|_| {
        Config::from_fe("127.0.0.1".into(), 9030, "root".into(), String::new())
    });

    let architecture = prompt_deploy_architecture()?;
    let method = prompt_deploy_method()?;
    if !method.dcli_automated() {
        output::warn(&format!(
            "dcli only automates SSH manual deploy; for {} see {}",
            method.label(),
            method.doc_url()
        ));
        output::info("you can still save topology/config here, then follow the official guide");
    }

    let fe_hosts = prompt_line("FE hosts (comma-separated IPs)", "")?;
    let fe_list: Vec<String> = fe_hosts
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    anyhow::ensure!(!fe_list.is_empty(), "at least one FE host is required");

    let leader = if fe_list.len() == 1 {
        fe_list[0].clone()
    } else {
        let l = prompt_line(
            &format!("Which FE is the leader? ({})", fe_list.join("/")),
            &fe_list[0],
        )?;
        anyhow::ensure!(fe_list.contains(&l), "leader must be one of the FE hosts");
        l
    };

    let frontends: Vec<FeNode> = fe_list
        .iter()
        .map(|h| FeNode {
            host: h.clone(),
            role: if *h == leader {
                "leader".into()
            } else {
                "follower".into()
            },
        })
        .collect();

    let be_hosts = prompt_line("BE hosts (comma-separated IPs)", "")?;
    let backends: Vec<BeNode> = be_hosts
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .map(|host| BeNode { host })
        .collect();

    let separated = if architecture == DeployArchitecture::Separated {
        Some(prompt_separated_config()?)
    } else {
        None
    };

    let install_dir = prompt_line("Install directory", "/opt/doris")?;
    let dl_yn = prompt_line("Download official Doris package now? (y/n)", "y")?;
    let (version, arch_slug, package) = if matches!(dl_yn.to_ascii_lowercase().as_str(), "y" | "yes") {
        let (v, a, p) = prompt_version_and_download().await?;
        (Some(v), Some(a), Some(p))
    } else {
        let package = prompt_line("Package path or URL (apache-doris-*-bin.tar.gz)", "")?;
        let version = prompt_line("Doris version (optional)", "")?;
        let arch = prompt_line("Binary arch (optional)", "")?;
        (opt(version), opt(arch), opt(package))
    };
    let java_home = prompt_line("JAVA_HOME (blank to auto-detect)", "")?;
    let priority_networks = prompt_line("priority_networks CIDR (blank to skip)", "")?;
    let ssh_user = prompt_line("SSH user", "root")?;
    let ssh_key = prompt_line("SSH private key path", "~/.ssh/id_rsa")?;
    let fe_user = prompt_line("Doris FE user", "root")?;
    let fe_password = prompt_line("Doris FE password (blank if none)", "")?;

    let mut sep_cfg = separated;
    if let Some(ref mut s) = sep_cfg {
        if s.cluster_id.is_none() {
            s.cluster_id = Some(random_cluster_id());
        }
    }

    cfg.fe.host = leader.clone();
    cfg.fe.user = fe_user;
    cfg.fe.password = fe_password;
    cfg.topology = Some(Topology {
        frontends,
        backends,
    });
    cfg.ssh = Some(crate::config::SshConfig {
        user: ssh_user,
        port: 22,
        key: Some(ssh_key),
    });
    cfg.deploy = Some(DeployConfig {
        architecture,
        method,
        version,
        arch: arch_slug,
        install_dir,
        package,
        java_home: opt(java_home),
        meta_dir: None,
        be_storage: None,
        priority_networks: opt(priority_networks),
        separated: sep_cfg,
    });

    write_config(cli, &cfg)?;

    let path = Config::resolve_path(cli.config.as_deref())
        .context("could not determine config path")?;
    output::ok(&format!("wrote topology to {}", path.display()));
    println!(
        "\nLeader FE: {leader}\nFEs: {}\nBEs: {}\n",
        cfg.topology
            .as_ref()
            .map(|t| t.frontends.iter().map(|f| f.host.clone()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default(),
        cfg.topology
            .as_ref()
            .map(|t| t.backends.iter().map(|b| b.host.clone()).collect::<Vec<_>>().join(", "))
            .unwrap_or_default(),
    );
    output::info("next: `dcli deploy precheck`, then `dcli deploy bootstrap`");
    if architecture == DeployArchitecture::Separated {
        output::info("separated mode: deploy FDB + Meta Service before `dcli deploy start`");
    }
    Ok(())
}

fn opt(s: String) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn plan(cli: &Cli) -> Result<()> {
    let cfg = resolve_config(cli)?;
    let deploy = cfg.deploy.as_ref();
    let architecture = deploy.map(|d| d.architecture).unwrap_or_default();
    let method = deploy.map(|d| d.method).unwrap_or_default();
    let install = deploy
        .map(|d| d.install_dir.clone())
        .unwrap_or_else(|| "/opt/doris".into());

    println!("Deploy plan for cluster '{}':\n", cfg.name);
    println!(
        "  architecture: {}  ({})",
        architecture.label(),
        architecture.doc_url()
    );
    println!("  method:       {}  ({})", method.label(), method.doc_url());
    if let Some(d) = deploy {
        if let Some(v) = &d.version {
            println!("  version:      {v}");
        }
        if let Some(a) = &d.arch {
            println!("  binary arch:  {a}");
        }
        if let Some(p) = &d.package {
            println!("  package:      {p}");
        }
    }
    if !method.dcli_automated() {
        output::warn("dcli does not automate install/start for this method; use the official guide");
        return Ok(());
    }

    if let Some(topo) = &cfg.topology {
        if let Some(leader) = topo.leader() {
            println!("\n  Leader FE: {}", leader.host);
        }
        for fe in &topo.frontends {
            println!("    FE {} ({})", fe.host, fe.role);
        }
        for be in &topo.backends {
            println!("    BE {}", be.host);
        }
        println!();
    } else {
        output::warn("no topology configured; run `dcli deploy init` first");
    }
    println!("  install_dir: {install}");

    if architecture == DeployArchitecture::Separated {
        if let Some(sep) = deploy.and_then(|d| d.separated.as_ref()) {
            println!("  meta_service: {}", sep.meta_service_endpoint);
            if !sep.meta_service_hosts.is_empty() {
                println!("  ms hosts:     {}", sep.meta_service_hosts.join(", "));
            }
            if let Some(id) = sep.cluster_id {
                println!("  cluster_id:   {id}");
            }
        }
        println!("\n  Separated-mode prerequisites (manual, before bootstrap):");
        println!("    1. FoundationDB 7.1.x cluster");
        println!("    2. S3 or HDFS shared storage");
        println!("    3. Meta Service (doris_cloud) on port 5000");
        println!("    4. Optional: recycler process");
        println!("\n  Steps performed by `dcli deploy bootstrap` (after prerequisites):");
        println!("    1. precheck  — SSH detect + validate CPU/mem/JDK/sysctl/limits");
        println!("    2. install   — scp package, extract, render fe.conf/be.conf (deploy_mode=cloud)");
        println!("    3. start     — leader FE → follower/observer FEs → BEs");
        println!("    4. register  — ALTER SYSTEM ADD FOLLOWER/OBSERVER/BACKEND");
        println!("    5. vault     — CREATE STORAGE VAULT (if configured)");
    } else {
        println!("\n  Steps performed by `dcli deploy bootstrap`:");
        println!("    1. precheck  — SSH detect + validate CPU/mem/JDK/sysctl/limits");
        println!("    2. install   — scp package, extract, render fe.conf/be.conf");
        println!("    3. start     — leader FE → follower/observer FEs (--helper) → BEs");
        println!("    4. register  — ALTER SYSTEM ADD FOLLOWER/OBSERVER/BACKEND");
    }
    Ok(())
}
