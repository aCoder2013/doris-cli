use anyhow::{Context, Result};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::config::SshConfig;

/// Result of a remote command.
#[derive(Debug, Clone)]
pub struct CmdOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOutput {
    pub fn ok(&self) -> bool {
        self.code == 0
    }
}

/// Thin wrapper that shells out to the system `ssh`/`scp`, reusing the user's
/// keys/agent/config. Avoids heavy native SSH build dependencies.
#[derive(Debug, Clone)]
pub struct Ssh {
    user: String,
    port: u16,
    key: Option<String>,
    connect_timeout: u32,
}

impl Ssh {
    pub fn from_cfg(cfg: Option<&SshConfig>) -> Self {
        match cfg {
            Some(s) => Ssh {
                user: s.user.clone(),
                port: s.port,
                key: s.key.clone(),
                connect_timeout: 10,
            },
            None => Ssh {
                user: default_local_ssh_user(),
                port: 22,
                key: None,
                connect_timeout: 10,
            },
        }
    }

    /// Best default private key path for prompts / sample config.
    pub fn default_key_hint() -> String {
        discover_ssh_key()
            .unwrap_or_else(|| "~/.ssh/id_ed25519".into())
    }

    pub fn username(&self) -> &str {
        &self.user
    }

    fn common_opts(&self) -> Vec<String> {
        let mut a = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            "StrictHostKeyChecking=accept-new".into(),
            "-o".into(),
            format!("ConnectTimeout={}", self.connect_timeout),
        ];
        if let Some(k) = resolve_ssh_key(self.key.as_ref()) {
            a.push("-i".into());
            a.push(k);
        }
        a
    }

    fn target(&self, host: &str) -> String {
        format!("{}@{}", self.user, host)
    }

    /// Run a bash script on the remote host (script is piped via stdin to `bash -s`).
    pub async fn run(&self, host: &str, script: &str) -> Result<CmdOutput> {
        let mut args = self.common_opts();
        args.push("-p".into());
        args.push(self.port.to_string());
        args.push(self.target(host));
        args.push("bash -s".into());

        let mut child = Command::new("ssh")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("failed to spawn ssh (is openssh installed?)")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(script.as_bytes()).await.ok();
            stdin.shutdown().await.ok();
        }
        let out = child
            .wait_with_output()
            .await
            .context("ssh process failed")?;
        Ok(CmdOutput {
            code: out.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&out.stdout).to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        })
    }

    /// Run a script and return an error if the remote exit code is non-zero.
    pub async fn run_checked(&self, host: &str, script: &str) -> Result<String> {
        let out = self.run(host, script).await?;
        anyhow::ensure!(
            out.ok(),
            "remote command on {host} failed (exit {}):\n{}",
            out.code,
            out.stderr.trim()
        );
        Ok(out.stdout)
    }

    /// Copy a local file to the remote host via scp.
    pub async fn upload(&self, host: &str, local: &str, remote: &str) -> Result<()> {
        let mut args = self.common_opts();
        // scp uses uppercase -P for the port.
        args.push("-P".into());
        args.push(self.port.to_string());
        args.push(expand_tilde(local));
        args.push(format!("{}:{}", self.target(host), remote));

        let status = Command::new("scp")
            .args(&args)
            .stdin(Stdio::null())
            .status()
            .await
            .context("failed to spawn scp")?;
        anyhow::ensure!(status.success(), "scp to {host}:{remote} failed");
        Ok(())
    }
}

fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().to_string();
        }
    }
    if path == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

/// Find the first usable private key in ~/.ssh (ed25519 preferred).
pub fn discover_ssh_key() -> Option<String> {
    let home = dirs::home_dir()?;
    let ssh_dir = home.join(".ssh");
    for name in ["id_ed25519", "id_rsa", "id_ecdsa"] {
        let p = ssh_dir.join(name);
        if p.is_file() {
            return Some(format!("~/.ssh/{name}"));
        }
    }
    None
}

/// Resolve which key to pass to ssh. Skips missing configured paths (avoids openssh warnings).
pub fn resolve_ssh_key(configured: Option<&String>) -> Option<String> {
    if let Some(k) = configured {
        let trimmed = k.trim();
        if trimmed.is_empty() {
            return discover_ssh_key().as_deref().map(expand_tilde);
        }
        let expanded = expand_tilde(trimmed);
        if std::path::Path::new(&expanded).is_file() {
            return Some(expanded);
        }
        // Configured path missing — fall back to agent / other keys.
        return discover_ssh_key().as_deref().map(expand_tilde);
    }
    discover_ssh_key().as_deref().map(expand_tilde)
}

fn default_local_ssh_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .unwrap_or_else(|_| "root".into())
}

/// Actionable hint when SSH to a host fails during deploy precheck.
pub fn ssh_failure_hint(host: &str, user: &str, stderr: &str) -> String {
    let mut hints = Vec::new();
    if stderr.contains("not accessible") || stderr.contains("No such file") {
        hints.push(format!(
            "SSH key missing; set ssh.key in config or run `dcli deploy init` again (detected: {})",
            default_key_hint_display()
        ));
    }
    if stderr.contains("Permission denied") {
        hints.push(format!(
            "try ssh.user='{local}' instead of '{user}' for localhost/WSL",
            local = default_local_ssh_user()
        ));
        hints.push(format!("test manually: ssh -o BatchMode=yes {user}@{host} true"));
    }
    if hints.is_empty() {
        hints.push(format!("test manually: ssh {user}@{host} true"));
    }
    hints.join("; ")
}

fn default_key_hint_display() -> String {
    Ssh::default_key_hint()
}
