//! Preparing the agent CLIs before the daemon starts work.
//!
//! Each agent can name setup steps (argument lists for its command, such as
//! installing a plugin) and an instructions file that becomes the CLI's own
//! `CLAUDE.md` or `AGENTS.md`. Both run at every start, because the home
//! directory that holds them is usually a volume that outlives the image.
//! A failure is reported and the daemon starts anyway: an agent without a
//! plugin can still work, and the next start tries again.

use crate::{
    process::{self, Exit, Spec},
    settings::{AgentSettings, DriverKind, Settings},
};
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// How long one setup step may take.
const STEP_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// Prepare every agent; return how many steps failed. Each step and each
/// failure is reported on stderr.
pub async fn prepare(settings: &Settings) -> usize {
    let mut failed = 0;
    for (name, agent) in &settings.daemon.agents {
        if let Some(file) = &agent.instructions {
            let source = settings.base_dir.join(file);
            match install_instructions(agent, &source) {
                Ok(target) => eprintln!(
                    "setup {name}: instructions {} -> {}",
                    source.display(),
                    target.display()
                ),
                Err(e) => {
                    eprintln!("setup {name}: instructions failed: {e:#}");
                    failed += 1;
                }
            }
        }
        for args in &agent.setup {
            let line = format!("{} {}", agent.command, args.join(" "));
            match run_step(agent, args).await {
                Ok(()) => eprintln!("setup {name}: ok: {line}"),
                Err(e) => {
                    eprintln!("setup {name}: failed: {line}: {e:#}");
                    failed += 1;
                }
            }
        }
    }
    failed
}

async fn run_step(agent: &AgentSettings, args: &[String]) -> Result<()> {
    let spec = Spec {
        program: agent.command.clone(),
        args: args.to_vec(),
        env: agent.env.clone(),
        cwd: home(agent).unwrap_or_else(std::env::temp_dir),
        timeout: STEP_TIMEOUT,
        ..Spec::default()
    };
    let mut text = String::new();
    let output = process::run(&spec, None, &mut |line| {
        text.push_str(line);
        text.push('\n');
    })
    .await?;
    match output.exit {
        Exit::Code(0) => Ok(()),
        exit => {
            let detail = format!("{}\n{}", text.trim(), output.stderr.trim());
            bail!("{exit:?}: {}", process::tail(detail.trim(), 2_000))
        }
    }
}

/// Copy the instructions file to where the CLI reads its user-wide
/// instructions, unless it is already there. Return that path.
fn install_instructions(agent: &AgentSettings, source: &Path) -> Result<PathBuf> {
    let target = instructions_path(agent)?;
    let text =
        std::fs::read(source).with_context(|| format!("cannot read {}", source.display()))?;
    if std::fs::read(&target).ok().as_deref() == Some(text.as_slice()) {
        return Ok(target);
    }
    if let Some(dir) = target.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    std::fs::write(&target, text).with_context(|| format!("cannot write {}", target.display()))?;
    Ok(target)
}

/// The CLI's user-wide instructions file. The agent's `env` wins over the
/// daemon's environment, as it does for the CLI itself.
pub fn instructions_path(agent: &AgentSettings) -> Result<PathBuf> {
    let (variable, dir, file) = match agent.driver {
        DriverKind::Claude => (Some("CLAUDE_CONFIG_DIR"), ".claude", "CLAUDE.md"),
        DriverKind::Codex => (Some("CODEX_HOME"), ".codex", "AGENTS.md"),
        // Bob reads AGENTS.md only from the workspace; every file in
        // ~/.bob/rules is a user-wide rule.
        DriverKind::Bob => (None, ".bob/rules", "amaru.md"),
    };
    let dir = match variable.and_then(|v| variable_of(agent, v)) {
        Some(dir) => dir,
        None => home(agent)
            .context("HOME is not set, so the instructions have nowhere to go")?
            .join(dir),
    };
    Ok(dir.join(file))
}

/// The home directory the CLI sees.
fn home(agent: &AgentSettings) -> Option<PathBuf> {
    variable_of(agent, "HOME")
}

/// A non-empty variable as the CLI sees it: the agent's `env` first.
fn variable_of(agent: &AgentSettings, name: &str) -> Option<PathBuf> {
    agent
        .env
        .get(name)
        .map(std::ffi::OsString::from)
        .or_else(|| std::env::var_os(name))
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests;
