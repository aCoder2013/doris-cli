use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use std::collections::BTreeMap;

use crate::config::{Config, DeployArchitecture};
use crate::ssh::{self, Ssh};

/// Parsed machine facts for one host.
#[derive(Debug, Clone, Default)]
pub struct HostInfo {
    pub host: String,
    pub reachable: bool,
    pub error: Option<String>,
    pub raw: BTreeMap<String, String>,
}

impl HostInfo {
    pub fn get(&self, key: &str) -> &str {
        self.raw.get(key).map(|s| s.as_str()).unwrap_or("")
    }
    pub fn get_i64(&self, key: &str) -> i64 {
        self.get(key).parse().unwrap_or(0)
    }
}

/// Bash snippet run on each host to collect facts as key=value lines.
fn detect_script(install_dir: &str) -> String {
    format!(
        r#"
d="{install_dir}"; while [ ! -d "$d" ] && [ "$d" != "/" ]; do d=$(dirname "$d"); done
echo "os=$(. /etc/os-release 2>/dev/null; echo "${{PRETTY_NAME:-$(uname -s)}}")"
echo "kernel=$(uname -r)"
echo "arch=$(uname -m)"
echo "cpu_cores=$(nproc 2>/dev/null || echo 0)"
echo "mem_total_kb=$(awk '/MemTotal/{{print $2}}' /proc/meminfo 2>/dev/null || echo 0)"
echo "disk_avail_gb=$(df -BG "$d" 2>/dev/null | awk 'NR==2{{gsub(/G/,"",$4); print $4}}')"
echo "java_version=$(java -version 2>&1 | head -1 | tr -d '"')"
echo "java_home=${{JAVA_HOME:-}}"
echo "max_map_count=$(cat /proc/sys/vm/max_map_count 2>/dev/null || echo 0)"
echo "swappiness=$(cat /proc/sys/vm/swappiness 2>/dev/null || echo -1)"
echo "overcommit=$(cat /proc/sys/vm/overcommit_memory 2>/dev/null || echo -1)"
echo "ulimit_nofile=$(ulimit -n)"
id="{install_dir}"
if mkdir -p "$id" 2>/dev/null; then echo "install_dir_ok=yes"; else echo "install_dir_ok=no"; fi
"#
    )
}

/// Detect facts for all hosts in the topology, concurrently.
pub async fn detect_all(cfg: &Config) -> Result<Vec<HostInfo>> {
    let topo = cfg
        .topology
        .clone()
        .ok_or_else(|| anyhow::anyhow!("no topology configured; run `dcli deploy init` first"))?;
    let hosts = topo.all_hosts();
    anyhow::ensure!(!hosts.is_empty(), "topology has no hosts");

    let ssh = Ssh::from_cfg(cfg.ssh.as_ref());
    let install_dir = cfg
        .deploy
        .as_ref()
        .map(|d| d.install_dir.clone())
        .unwrap_or_else(|| "/opt/doris".into());
    let script = detect_script(&install_dir);

    let mut set = tokio::task::JoinSet::new();
    for host in hosts {
        let ssh = ssh.clone();
        let script = script.clone();
        set.spawn(async move {
            let mut info = HostInfo {
                host: host.clone(),
                ..Default::default()
            };
            match ssh.run(&host, &script).await {
                Ok(out) if out.ok() => {
                    info.reachable = true;
                    for line in out.stdout.lines() {
                        if let Some((k, v)) = line.split_once('=') {
                            info.raw.insert(k.trim().to_string(), v.trim().to_string());
                        }
                    }
                }
                Ok(out) => {
                    let stderr = out.stderr.trim();
                    let hint = ssh::ssh_failure_hint(&host, ssh.username(), stderr);
                    info.error = Some(if stderr.is_empty() {
                        hint
                    } else {
                        format!("{stderr}\n  hint: {hint}")
                    });
                }
                Err(e) => {
                    info.error = Some(e.to_string());
                }
            }
            info
        });
    }

    let mut results = Vec::new();
    while let Some(joined) = set.join_next().await {
        results.push(joined?);
    }
    results.sort_by(|a, b| a.host.cmp(&b.host));
    Ok(results)
}

/// Render detected facts as a table.
pub fn render_detect(infos: &[HostInfo]) {
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec![
            "Host", "OS", "Arch", "CPU", "Mem(GB)", "Disk(GB)", "Java", "max_map_count",
        ]);
    for i in infos {
        if !i.reachable {
            table.add_row(vec![
                i.host.clone(),
                format!("UNREACHABLE: {}", i.error.clone().unwrap_or_default()),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
                "-".into(),
            ]);
            continue;
        }
        let mem_gb = i.get_i64("mem_total_kb") / 1024 / 1024;
        table.add_row(vec![
            i.host.clone(),
            i.get("os").to_string(),
            i.get("arch").to_string(),
            i.get("cpu_cores").to_string(),
            mem_gb.to_string(),
            i.get("disk_avail_gb").to_string(),
            shorten_java(i.get("java_version")),
            i.get("max_map_count").to_string(),
        ]);
    }
    println!("{table}");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warn,
    Fail,
}

impl Severity {
    fn label(&self) -> &'static str {
        match self {
            Severity::Ok => "OK",
            Severity::Warn => "WARN",
            Severity::Fail => "FAIL",
        }
    }
}

/// One precheck finding.
struct Check {
    name: &'static str,
    value: String,
    severity: Severity,
    note: String,
}

/// Validate a host against Doris requirements/recommendations.
fn check_host(i: &HostInfo) -> Vec<Check> {
    let mut checks = Vec::new();

    let cpu = i.get_i64("cpu_cores");
    checks.push(Check {
        name: "cpu_cores",
        value: cpu.to_string(),
        severity: if cpu == 0 {
            Severity::Fail
        } else if cpu < 4 {
            Severity::Warn
        } else {
            Severity::Ok
        },
        note: if cpu < 4 { "recommend >= 8 cores".into() } else { String::new() },
    });

    let mem_gb = i.get_i64("mem_total_kb") / 1024 / 1024;
    checks.push(Check {
        name: "memory",
        value: format!("{mem_gb} GB"),
        severity: if mem_gb < 8 { Severity::Warn } else { Severity::Ok },
        note: if mem_gb < 8 { "recommend >= 16 GB".into() } else { String::new() },
    });

    let disk: i64 = i.get("disk_avail_gb").parse().unwrap_or(0);
    checks.push(Check {
        name: "disk_avail",
        value: format!("{disk} GB"),
        severity: if disk > 0 && disk < 20 { Severity::Warn } else { Severity::Ok },
        note: if disk > 0 && disk < 20 { "low free space on install path".into() } else { String::new() },
    });

    let java = i.get("java_version");
    checks.push(Check {
        name: "java",
        value: if java.is_empty() { "not found".into() } else { shorten_java(java) },
        severity: if java.is_empty() { Severity::Fail } else { Severity::Ok },
        note: if java.is_empty() {
            "install JDK 8 (Doris 2.0) or JDK 17 (Doris 2.1+/3.x) and set JAVA_HOME".into()
        } else {
            String::new()
        },
    });

    let mmc = i.get_i64("max_map_count");
    checks.push(Check {
        name: "vm.max_map_count",
        value: mmc.to_string(),
        severity: if mmc < 2_000_000 { Severity::Fail } else { Severity::Ok },
        note: if mmc < 2_000_000 {
            "set: sysctl -w vm.max_map_count=2000000".into()
        } else {
            String::new()
        },
    });

    let swap = i.get("swappiness").parse::<i64>().unwrap_or(-1);
    checks.push(Check {
        name: "vm.swappiness",
        value: swap.to_string(),
        severity: if swap > 10 { Severity::Warn } else { Severity::Ok },
        note: if swap > 10 { "recommend 0: sysctl -w vm.swappiness=0".into() } else { String::new() },
    });

    let nofile = i.get_i64("ulimit_nofile");
    checks.push(Check {
        name: "ulimit -n",
        value: nofile.to_string(),
        severity: if nofile < 65536 { Severity::Warn } else { Severity::Ok },
        note: if nofile < 65536 {
            "recommend >= 1000000 (limits.conf nofile)".into()
        } else {
            String::new()
        },
    });

    let install_ok = i.get("install_dir_ok");
    if install_ok.is_empty() {
        // older detect script without this field
    } else {
        checks.push(Check {
            name: "install_dir",
            value: install_ok.into(),
            severity: if install_ok == "yes" { Severity::Ok } else { Severity::Fail },
            note: if install_ok == "yes" {
                String::new()
            } else {
                "install_dir not writable; use ~/doris or run SSH as root".into()
            },
        });
    }

    checks
}

/// Run prechecks on all hosts; returns true if any FAIL was found.
pub fn render_precheck(cfg: &Config, infos: &[HostInfo]) -> bool {
    let mut any_fail = render_cluster_precheck(cfg);
    for i in infos {
        println!("\nHost {}:", i.host);
        if !i.reachable {
            println!("  FAIL  unreachable: {}", i.error.clone().unwrap_or_default());
            any_fail = true;
            continue;
        }
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec!["Check", "Value", "Status", "Note"]);
        for c in check_host(i) {
            if c.severity == Severity::Fail {
                any_fail = true;
            }
            table.add_row(vec![
                c.name.to_string(),
                c.value,
                c.severity.label().to_string(),
                c.note,
            ]);
        }
        println!("{table}");
    }
    any_fail
}

fn render_cluster_precheck(cfg: &Config) -> bool {
    let deploy = match cfg.deploy.as_ref() {
        Some(d) => d,
        None => return false,
    };
    if deploy.architecture != DeployArchitecture::Separated {
        return false;
    }
    let mut any_fail = false;
    println!("\nSeparated-mode prerequisites:");
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Check", "Value", "Status", "Note"]);

    let sep = deploy.separated.as_ref();
    let endpoint = sep.map(|s| s.meta_service_endpoint.as_str()).unwrap_or("");
    let ep_ok = !endpoint.is_empty() && endpoint.contains(':');
    if !ep_ok {
        any_fail = true;
    }
    table.add_row(vec![
        "meta_service_endpoint".into(),
        if endpoint.is_empty() {
            "(not set)".into()
        } else {
            endpoint.into()
        },
        if ep_ok {
            "OK".into()
        } else {
            "FAIL".into()
        },
        if ep_ok {
            String::new()
        } else {
            "set deploy.separated.meta_service_endpoint (host:5000)".into()
        },
    ]);

    let fdb = sep
        .and_then(|s| s.fdb_cluster.as_ref())
        .map(|s| s.as_str())
        .unwrap_or("");
    table.add_row(vec![
        "fdb_cluster".into(),
        if fdb.is_empty() { "(not set)".into() } else { fdb.into() },
        if fdb.is_empty() { "WARN".into() } else { "OK".into() },
        if fdb.is_empty() {
            "FoundationDB 7.1.x required before Meta Service".into()
        } else {
            String::new()
        },
    ]);

    let vault = sep.and_then(|s| s.storage_vault.as_ref());
    table.add_row(vec![
        "storage_vault".into(),
        vault.map(|v| v.name.clone()).unwrap_or_else(|| "(not set)".into()),
        if vault.is_some() { "OK".into() } else { "WARN".into() },
        if vault.is_some() {
            String::new()
        } else {
            "configure deploy.separated.storage_vault or create manually".into()
        },
    ]);

    println!("{table}");
    any_fail
}

fn shorten_java(v: &str) -> String {
    // e.g. `openjdk version 17.0.10 2024-...` -> `17.0.10`
    v.split_whitespace()
        .find(|t| t.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
        .unwrap_or(v)
        .to_string()
}
