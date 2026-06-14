use anyhow::{Context, Result};
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Binary CPU/arch variant for official Doris packages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryArch {
    X64,
    X64NoAvx2,
    Arm64,
}

impl BinaryArch {
    pub fn slug(&self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::X64NoAvx2 => "x64-noavx2",
            Self::Arm64 => "arm64",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "x64" | "amd64" | "x86_64" => Some(Self::X64),
            "x64-noavx2" | "noavx2" => Some(Self::X64NoAvx2),
            "arm64" | "aarch64" => Some(Self::Arm64),
            _ => None,
        }
    }

    /// Best-effort local architecture detection.
    pub fn detect_local() -> Self {
        match std::env::consts::ARCH {
            "aarch64" | "arm64" => Self::Arm64,
            _ => Self::X64,
        }
    }
}

/// Release channel hint aligned with doris.apache.org/download.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionChannel {
    Latest,
    Stable,
}

#[derive(Debug, Clone)]
pub struct DorisRelease {
    pub tag: String,
    pub version: String,
    pub published_at: String,
    pub binaries: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct GhRelease {
    tag_name: String,
    published_at: String,
    prerelease: bool,
    draft: bool,
    body: Option<String>,
}

const GH_API: &str = "https://api.github.com/repos/apache/doris/releases";

/// Fetch releases from the official GitHub API (download links are in release notes).
pub async fn fetch_releases(limit: usize) -> Result<Vec<DorisRelease>> {
    let client = reqwest::Client::builder()
        .user_agent("doris-cli/0.1 (https://github.com/acoder2013/doris-cli)")
        .build()?;
    let url = format!("{GH_API}?per_page={}", limit.min(100));
    let raw: Vec<GhRelease> = client
        .get(&url)
        .send()
        .await
        .context("failed to fetch apache/doris releases")?
        .error_for_status()
        .context("GitHub API returned an error")?
        .json()
        .await
        .context("failed to parse GitHub releases JSON")?;

    let mut out = Vec::new();
    for r in raw {
        if r.draft || r.prerelease {
            continue;
        }
        let version = normalize_version(&r.tag_name);
        let parsed = parse_release_body(r.body.as_deref().unwrap_or_default());
        out.push(DorisRelease {
            tag: r.tag_name,
            version,
            published_at: r.published_at,
            binaries: parsed.binaries,
        });
    }
    Ok(out)
}

struct ParsedBody {
    binaries: BTreeMap<String, String>,
}

/// Parse markdown links from a GitHub release body.
fn parse_release_body(body: &str) -> ParsedBody {
    let mut binaries = BTreeMap::new();
    for line in body.lines() {
        let lower = line.to_ascii_lowercase();
        if let Some(url) = extract_markdown_url(line) {
            if url.contains("-bin-") && url.ends_with(".tar.gz") {
                if let Some(arch) = arch_from_line(&lower, &url) {
                    binaries.insert(arch, url);
                }
            }
        } else if lower.contains("binary(") || lower.contains("binary (") {
            // header line without URL on same line — skip
        }
    }
    ParsedBody { binaries }
}

fn extract_markdown_url(line: &str) -> Option<String> {
    let start = line.find("](http")?;
    let rest = &line[start + 2..];
    let end = rest.find(')')?;
    Some(rest[..end].to_string())
}

fn arch_from_line(line_lower: &str, url: &str) -> Option<String> {
    if line_lower.contains("arm64") || url.contains("-arm64") {
        Some("arm64".into())
    } else if line_lower.contains("noavx2") || url.contains("-noavx2") {
        Some("x64-noavx2".into())
    } else if line_lower.contains("x64") || url.contains("-x64") {
        Some("x64".into())
    } else {
        None
    }
}

/// Normalize tag names like `4.0.4-release` → `4.0.4`.
pub fn normalize_version(tag: &str) -> String {
    let mut v = tag.trim_start_matches('v').to_string();
    if let Some(idx) = v.find('-') {
        let suffix = &v[idx + 1..];
        if suffix.chars().all(|c| c.is_ascii_alphabetic() || c == '_') {
            v.truncate(idx);
        }
    }
    v
}

fn parse_version_parts(v: &str) -> Option<(u32, u32, u32)> {
    let mut it = v.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Pick the newest release overall.
pub fn pick_latest(releases: &[DorisRelease]) -> Option<&DorisRelease> {
    releases.first()
}

/// Pick the highest 4.0.x release (official "stable" line on doris.apache.org/download).
pub fn pick_stable(releases: &[DorisRelease]) -> Option<&DorisRelease> {
    releases
        .iter()
        .filter(|r| r.version.starts_with("4.0."))
        .max_by(|a, b| {
            let pa = parse_version_parts(&a.version).unwrap_or((0, 0, 0));
            let pb = parse_version_parts(&b.version).unwrap_or((0, 0, 0));
            pa.cmp(&pb)
        })
}

pub fn find_release<'a>(releases: &'a [DorisRelease], version: &str) -> Option<&'a DorisRelease> {
    let want = normalize_version(version);
    releases
        .iter()
        .find(|r| r.version == want || r.tag == want || r.tag == format!("v{want}"))
}

pub fn resolve_binary_url(release: &DorisRelease, arch: BinaryArch) -> Result<String> {
    release
        .binaries
        .get(arch.slug())
        .cloned()
        .with_context(|| {
            format!(
                "no {} binary for Doris {}; available: {}",
                arch.slug(),
                release.version,
                release.binaries.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })
}

/// Default local cache: ~/.doris-cli/packages/
pub fn packages_dir() -> Result<PathBuf> {
    let home = dirs::home_dir().context("could not resolve home directory")?;
    Ok(home.join(".doris-cli").join("packages"))
}

pub fn cached_package_path(version: &str, arch: BinaryArch) -> PathBuf {
    packages_dir()
        .unwrap_or_else(|_| PathBuf::from("/tmp"))
        .join(format!("apache-doris-{version}-bin-{}.tar.gz", arch.slug()))
}

fn size_sidecar_path(package: &Path) -> PathBuf {
    PathBuf::from(format!("{}.size", package.display()))
}

fn read_size_sidecar(package: &Path) -> Option<u64> {
    let sidecar = size_sidecar_path(package);
    let raw = std::fs::read_to_string(&sidecar).ok()?;
    raw.trim().parse().ok()
}

fn write_size_sidecar(package: &Path, size: u64) -> Result<()> {
    let sidecar = size_sidecar_path(package);
    std::fs::write(&sidecar, size.to_string())
        .with_context(|| format!("failed to write {}", sidecar.display()))?;
    Ok(())
}

fn remove_package_artifacts(path: &Path) {
    std::fs::remove_file(path).ok();
    std::fs::remove_file(size_sidecar_path(path)).ok();
}

/// Verify a local `.tar.gz` package (size + gzip integrity).
pub fn verify_package_file(path: &Path) -> Result<()> {
    let meta = std::fs::metadata(path)
        .with_context(|| format!("package not found: {}", path.display()))?;
    let size = meta.len();
    anyhow::ensure!(size > 0, "package file is empty: {}", path.display());

    if let Some(expected) = read_size_sidecar(path) {
        anyhow::ensure!(
            size == expected,
            "package size mismatch: got {} bytes, expected {} bytes",
            size,
            expected
        );
    }

    verify_gzip_integrity(path)?;
    Ok(())
}

fn verify_gzip_integrity(path: &Path) -> Result<()> {
    let output = std::process::Command::new("gzip")
        .arg("-t")
        .arg(path)
        .output()
        .with_context(|| format!("failed to run gzip -t on {}", path.display()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let detail = if stderr.is_empty() {
        "gzip integrity check failed (truncated or corrupt archive)".to_string()
    } else {
        stderr
    };
    anyhow::bail!(detail)
}

pub fn package_incomplete_message(path: &Path, detail: &str, version: Option<&str>, arch: Option<BinaryArch>) -> String {
    let mut msg = format!(
        "安装包不完整或已损坏: {}\n  原因: {}\n  请删除后重新下载:",
        path.display(),
        detail
    );
    if let (Some(v), Some(a)) = (version, arch) {
        msg.push_str(&format!(
            "\n    dcli deploy download --release {v} --arch {}",
            a.slug()
        ));
    } else {
        msg.push_str("\n    dcli deploy download --release <version> --arch x64");
    }
    msg
}

async fn fetch_content_length(client: &reqwest::Client, url: &str) -> Option<u64> {
    client
        .head(url)
        .send()
        .await
        .ok()
        .and_then(|r| r.content_length())
}

/// Verify local package before install; returns actionable error if corrupt.
pub fn ensure_local_package(path: &Path) -> Result<()> {
    match verify_package_file(path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let detail = e.to_string();
            anyhow::bail!(package_incomplete_message(path, &detail, None, None))
        }
    }
}

/// Download a release binary to the cache dir; returns the local path.
pub async fn download_binary(
    release: &DorisRelease,
    arch: BinaryArch,
    dest: Option<&Path>,
) -> Result<PathBuf> {
    let url = resolve_binary_url(release, arch)?;
    let path = dest
        .map(Path::to_path_buf)
        .unwrap_or_else(|| cached_package_path(&release.version, arch));
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let client = reqwest::Client::builder()
        .user_agent("doris-cli/0.1")
        .build()?;
    let expected_size = fetch_content_length(&client, &url).await;

    if path.exists() {
        if verify_package_file(&path).is_ok() {
            if let (Some(exp), Some(sidecar)) = (expected_size, read_size_sidecar(&path)) {
                if exp != sidecar {
                    eprintln!(
                        "⚠ 本地包大小与远端不一致 (本地 {} / 远端 {})，将重新下载",
                        format_bytes(sidecar),
                        format_bytes(exp)
                    );
                    remove_package_artifacts(&path);
                } else {
                    eprintln!("• 使用本地缓存: {}", path.display());
                    return Ok(path);
                }
            } else {
                eprintln!("• 使用本地缓存: {}", path.display());
                return Ok(path);
            }
        } else {
            eprintln!("⚠ 本地安装包不完整或已损坏，将重新下载");
            eprintln!(
                "  {}",
                package_incomplete_message(&path, "cached file failed integrity check", Some(&release.version), Some(arch))
            );
            remove_package_artifacts(&path);
        }
    }

    eprintln!(
        "• 开始下载 Doris {} ({}) …\n  {}",
        release.version,
        arch.slug(),
        url
    );

    let mut resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("failed to download {url}"))?
        .error_for_status()
        .with_context(|| format!("download failed for {url}"))?;

    let total = resp.content_length().or(expected_size);
    let mut file = tokio::fs::File::create(&path)
        .await
        .with_context(|| format!("failed to create {}", path.display()))?;

    let started = Instant::now();
    let mut last_render = started;
    let mut downloaded: u64 = 0;

    while let Some(chunk) = resp
        .chunk()
        .await
        .context("failed while reading download stream")?
    {
        if chunk.is_empty() {
            break;
        }
        downloaded += chunk.len() as u64;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .context("failed while writing package file")?;

        let now = Instant::now();
        if now.duration_since(last_render) >= Duration::from_millis(200) {
            render_download_progress(downloaded, total, started);
            last_render = now;
        }
    }

    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .context("failed to flush package file")?;
    render_download_progress(downloaded, total, started);
    finish_download_progress();

    if let Some(exp) = total {
        if downloaded != exp {
            remove_package_artifacts(&path);
            anyhow::bail!(
                "{}",
                package_incomplete_message(
                    &path,
                    &format!(
                        "download incomplete: got {} bytes, expected {}",
                        downloaded,
                        exp
                    ),
                    Some(&release.version),
                    Some(arch),
                )
            );
        }
        write_size_sidecar(&path, exp)?;
    }

    if let Err(e) = verify_package_file(&path) {
        remove_package_artifacts(&path);
        anyhow::bail!(
            "{}",
            package_incomplete_message(&path, &e.to_string(), Some(&release.version), Some(arch))
        );
    }

    Ok(path)
}

fn format_bytes(n: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if n >= GB {
        format!("{:.1} GB", n as f64 / GB as f64)
    } else if n >= MB {
        format!("{:.1} MB", n as f64 / MB as f64)
    } else if n >= KB {
        format!("{:.1} KB", n as f64 / KB as f64)
    } else {
        format!("{n} B")
    }
}

fn render_download_progress(downloaded: u64, total: Option<u64>, started: Instant) {
    let elapsed = started.elapsed().as_secs_f64().max(0.001);
    let speed_mbps = downloaded as f64 / elapsed / (1024.0 * 1024.0);
    let line = if let Some(total) = total {
        let pct = if total == 0 {
            0.0
        } else {
            (downloaded as f64 / total as f64) * 100.0
        };
        format!(
            "\r• 下载中: {:5.1}%  {} / {}  {:.1} MB/s",
            pct,
            format_bytes(downloaded),
            format_bytes(total),
            speed_mbps
        )
    } else {
        format!(
            "\r• 下载中: {}  {:.1} MB/s",
            format_bytes(downloaded),
            speed_mbps
        )
    };
    eprint!("{line}");
    let _ = std::io::stderr().flush();
}

fn finish_download_progress() {
    eprintln!();
}

pub fn render_versions_table(releases: &[DorisRelease], limit: usize) {
    let latest = pick_latest(releases).map(|r| r.version.clone());
    let stable = pick_stable(releases).map(|r| r.version.clone());

    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(vec!["Version", "Tag", "Published", "Binaries", "Channel"]);

    for r in releases.iter().take(limit) {
        let mut channel = Vec::new();
        if latest.as_deref() == Some(r.version.as_str()) {
            channel.push("latest");
        }
        if stable.as_deref() == Some(r.version.as_str()) {
            channel.push("stable");
        }
        table.add_row(vec![
            r.version.clone(),
            r.tag.clone(),
            r.published_at.chars().take(10).collect(),
            r.binaries.keys().cloned().collect::<Vec<_>>().join(", "),
            channel.join(", "),
        ]);
    }
    println!("{table}");
    println!(
        "\nOfficial guide: https://doris.apache.org/zh-CN/docs/4.x/install/choosing-deployment-mode"
    );
    println!("Download page:  https://doris.apache.org/download");
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_BODY: &str = r#"
- Binary(x64):
 - [apache-doris-4.1.1-bin-x64.tar.gz](https://download.velodb.io/apache-doris-4.1.1-bin-x64.tar.gz)
- Binary(x64-noavx2):
 - [apache-doris-4.1.1-bin-x64-noavx2.tar.gz](https://download.velodb.io/apache-doris-4.1.1-bin-x64-noavx2.tar.gz)
- Binary(arm64):
 - [apache-doris-4.1.1-bin-arm64.tar.gz](https://download.velodb.io/apache-doris-4.1.1-bin-arm64.tar.gz)
"#;

    #[test]
    fn parses_binary_urls() {
        let p = parse_release_body(SAMPLE_BODY);
        assert_eq!(p.binaries.len(), 3);
        assert!(p.binaries["x64"].contains("4.1.1-bin-x64"));
    }

    #[test]
    fn normalizes_tag() {
        assert_eq!(normalize_version("4.0.4-release"), "4.0.4");
        assert_eq!(normalize_version("v4.1.1"), "4.1.1");
    }

    #[test]
    fn rejects_truncated_gzip() {
        let dir = std::env::temp_dir().join("doris-cli-test");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join("truncated.tar.gz");
        std::fs::write(&path, b"not a real gzip").unwrap();
        assert!(verify_package_file(&path).is_err());
        std::fs::remove_file(&path).ok();
    }
}
