use anyhow::{Context, Result};

use crate::cli::{Cli, ProfileCmd};
use crate::config;
use crate::config::Config;
use crate::output;

pub async fn run(cli: &Cli, cmd: &ProfileCmd) -> Result<()> {
    match cmd {
        ProfileCmd::List => list(),
        ProfileCmd::Current => current(),
        ProfileCmd::Add {
            name,
            from_current,
            force,
        } => add(cli, name, *from_current, *force),
        ProfileCmd::Use { name } => use_profile(name),
        ProfileCmd::Remove { name, yes } => remove(name, *yes),
        ProfileCmd::Show { name } => show(cli, name.as_deref()),
    }
}

fn list() -> Result<()> {
    let profiles = config::list_profiles()?;
    if profiles.is_empty() {
        output::info("no profiles yet (create one with `dcli profile add <name>`)");
        return Ok(());
    }
    let active = config::active_profile();
    for name in &profiles {
        if active.as_deref() == Some(name.as_str()) {
            println!("* {name}");
        } else {
            println!("  {name}");
        }
    }
    Ok(())
}

fn current() -> Result<()> {
    match config::active_profile() {
        Some(name) => {
            let path = config::profile_path(&name);
            output::ok(&format!(
                "active profile: {name}{}",
                path.map(|p| format!(" ({})", p.display())).unwrap_or_default()
            ));
        }
        None => output::info("no active profile (using default ~/.doris-cli/cluster.yaml)"),
    }
    Ok(())
}

fn add(cli: &Cli, name: &str, from_current: bool, force: bool) -> Result<()> {
    validate_name(name)?;
    let path = config::profile_path(name)
        .context("could not determine profiles dir (no home dir?)")?;
    if path.exists() && !force {
        output::warn(&format!(
            "profile '{name}' already exists at {} (use --force to overwrite)",
            path.display()
        ));
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    let contents = if from_current {
        let cfg = crate::commands::resolve_config(cli)
            .context("failed to load current config for --from-current")?;
        serde_yaml::to_string(&cfg)?
    } else {
        Config::sample_yaml().to_string()
    };
    std::fs::write(&path, contents)
        .with_context(|| format!("failed to write {}", path.display()))?;
    output::ok(&format!("created profile '{name}' at {}", path.display()));
    output::info(&format!("activate it with `dcli profile use {name}`"));
    Ok(())
}

fn use_profile(name: &str) -> Result<()> {
    config::set_active_profile(name)?;
    output::ok(&format!("switched active profile to '{name}'"));
    Ok(())
}

fn remove(name: &str, yes: bool) -> Result<()> {
    let path = config::profile_path(name)
        .context("could not determine profiles dir (no home dir?)")?;
    anyhow::ensure!(path.exists(), "profile '{name}' does not exist");
    if !yes {
        output::confirm(&format!("delete profile '{name}' ({})?", path.display()))?;
    }
    std::fs::remove_file(&path)
        .with_context(|| format!("failed to remove {}", path.display()))?;
    // Clear the active pointer if it referenced the removed profile.
    if config::active_profile().as_deref() == Some(name) {
        config::clear_active_profile()?;
        output::info("removed profile was active; reverted to default cluster.yaml");
    }
    output::ok(&format!("removed profile '{name}'"));
    Ok(())
}

fn show(cli: &Cli, name: Option<&str>) -> Result<()> {
    // Prefer the explicit arg, then a global --profile flag, then the active profile.
    let target = name
        .map(str::to_string)
        .or_else(|| cli.profile.clone())
        .or_else(config::active_profile);
    let cfg = match &target {
        Some(n) => Config::load(None, Some(n))
            .with_context(|| format!("failed to load profile '{n}'"))?,
        None => {
            output::info("no profile selected; showing default config");
            Config::load(cli.config.as_deref(), None)?
        }
    };
    if let Some(n) = &target {
        output::info(&format!("profile: {n}"));
    }
    println!("{}", serde_yaml::to_string(&cfg)?);
    Ok(())
}

/// Reject names that would escape the profiles directory or break filenames.
fn validate_name(name: &str) -> Result<()> {
    anyhow::ensure!(!name.is_empty(), "profile name must not be empty");
    anyhow::ensure!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "profile name '{name}' may only contain letters, digits, '-', '_', '.'"
    );
    anyhow::ensure!(
        name != "." && name != ".." && !name.contains('/'),
        "invalid profile name '{name}'"
    );
    Ok(())
}
