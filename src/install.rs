//! Installing agent CLIs from the web page.
//!
//! Amaru does not ship the agent CLIs: their licenses differ, and some, such
//! as IBM Bob, do not allow redistributing them. The operator installs each
//! one from the page instead, which runs the agent's `install` command line
//! (by default `npm install --global <package>`) as the user that runs the
//! daemon. npm installs into `~/.local` unless a prefix is already set, so
//! the CLI lands in the home directory, on the container's home volume.

use crate::{
    login::{self, Login, LoginView, Logins, Then},
    settings::{AgentSettings, DriverKind},
    setup,
};
use anyhow::{Result, bail};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
};

/// Where npm installs when nothing else sets its prefix, under `HOME`.
pub const PREFIX: &str = ".local";

/// Start installing `name`, stopping an install already running for it.
/// Once the install has finished well, the agent's setup runs too (its
/// instructions file and `setup` steps), as at a daemon start, since a CLI
/// that was missing at the start could not be set up then. `base_dir` is
/// where the daemon file is.
pub fn start(
    installs: &Logins,
    base_dir: &Path,
    name: &str,
    agent: &AgentSettings,
) -> Result<LoginView> {
    let line = agent.install_command();
    let Some((program, args)) = line.split_first() else {
        bail!("installing from the page is turned off for agent {name}");
    };
    let mut cmd = login::program(program, args, &agent.env);
    cmd.stdin(Stdio::null());
    if let Some(prefix) = npm_prefix(agent) {
        cmd.env("NPM_CONFIG_PREFIX", prefix);
    }
    let then = set_up_after(base_dir, name, agent);
    Ok(installs.run_then(name, &mut cmd, Some(then))?.view())
}

fn set_up_after(base_dir: &Path, name: &str, agent: &AgentSettings) -> Then {
    let (base_dir, name, agent) = (base_dir.to_owned(), name.to_owned(), agent.clone());
    Box::new(move |login: Arc<Login>| {
        Box::pin(async move {
            if agent.instructions.is_none() && agent.setup.is_empty() {
                return String::new();
            }
            login.append(b"\nSetting the agent up\n");
            let failed = setup::prepare_agent(&base_dir, &name, &agent, &mut |line| {
                login.append(format!("{line}\n").as_bytes());
            })
            .await;
            match failed {
                0 => "set up".to_owned(),
                n => format!("but {n} setup step(s) failed"),
            }
        })
    })
}

/// `~/.local`, unless the agent's `env` or the daemon's environment already
/// gives npm a prefix, or there is no home directory.
fn npm_prefix(agent: &AgentSettings) -> Option<PathBuf> {
    let set = ["NPM_CONFIG_PREFIX", "npm_config_prefix"]
        .iter()
        .any(|v| variable(agent, v).is_some());
    if set {
        return None;
    }
    variable(agent, "HOME").map(|home| PathBuf::from(home).join(PREFIX))
}

/// A non-empty variable as the CLI sees it: the agent's `env` first.
fn variable(agent: &AgentSettings, name: &str) -> Option<OsString> {
    agent
        .env
        .get(name)
        .map(OsString::from)
        .or_else(|| std::env::var_os(name))
        .filter(|v| !v.is_empty())
}

/// What the operator accepts by installing the CLI, shown next to the
/// Install button when the terms are not plainly open source.
pub const fn notice(agent: &AgentSettings) -> Option<&'static str> {
    match agent.driver {
        DriverKind::Bob => Some(
            "IBM Bob Shell is IBM's program, licensed under IBM's International License \
             Agreement for Non-Warranted Programs. Installing it downloads it for you and \
             accepts that license; do not publish an image that contains it.",
        ),
        DriverKind::Claude => Some(
            "Claude Code is Anthropic's software; its use is subject to Anthropic's terms \
             (https://code.claude.com/docs/en/legal-and-compliance).",
        ),
        DriverKind::Codex => None,
    }
}

/// Whether the agent's `command` can be found: as a path, or on the `PATH`
/// the CLI sees.
pub fn installed(agent: &AgentSettings) -> bool {
    let command = Path::new(&agent.command);
    if command.components().count() > 1 {
        return executable(command);
    }
    variable(agent, "PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| executable(&dir.join(command))))
}

fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, time::Duration};

    fn agent(dir: &Path, install: Option<Vec<String>>) -> AgentSettings {
        AgentSettings {
            driver: DriverKind::Bob,
            command: "fake-bob".into(),
            args: vec![],
            model: None,
            env: BTreeMap::from([
                ("HOME".to_owned(), dir.display().to_string()),
                (
                    "PATH".to_owned(),
                    format!("{}:/usr/bin:/bin", dir.join(".local/bin").display()),
                ),
            ]),
            login: None,
            login_with_key: None,
            login_status: None,
            setup: Vec::new(),
            instructions: None,
            install,
        }
    }

    #[test]
    fn each_driver_installs_its_npm_package() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = agent(dir.path(), None);
        assert_eq!(
            a.install_command().last().map(String::as_str),
            Some("@ibm/bob")
        );
        a.driver = DriverKind::Codex;
        assert_eq!(a.install_command()[..3], ["npm", "install", "--global"]);
        assert_eq!(
            a.install_command().last().map(String::as_str),
            Some("@openai/codex")
        );
        a.driver = DriverKind::Claude;
        assert_eq!(
            a.install_command().last().map(String::as_str),
            Some("@anthropic-ai/claude-code")
        );
    }

    #[tokio::test]
    async fn installs_into_the_home_prefix_and_then_finds_the_command() {
        let dir = tempfile::tempdir().unwrap();
        // A stand-in for npm: put an executable in "$NPM_CONFIG_PREFIX/bin".
        let script = r#"mkdir -p "$NPM_CONFIG_PREFIX/bin" && printf '#!/bin/sh\n' > "$NPM_CONFIG_PREFIX/bin/fake-bob" && chmod +x "$NPM_CONFIG_PREFIX/bin/fake-bob" && echo installed"#;
        let mut a = agent(
            dir.path(),
            Some(vec!["sh".into(), "-c".into(), script.into()]),
        );
        // The setup step runs the freshly installed command.
        a.setup = vec![vec![]];
        assert!(!installed(&a));
        let installs = Logins::default();
        start(&installs, dir.path(), "bob", &a).unwrap();
        let mut view = installs.get("bob").unwrap();
        for _ in 0..100 {
            if !view.running {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            view = installs.get("bob").unwrap();
        }
        assert_eq!(view.exit.as_deref(), Some("finished, set up"));
        assert!(view.output.contains("setup bob: ok"));
        assert!(view.output.contains("installed"));
        assert!(installed(&a));
    }

    #[test]
    fn an_empty_install_turns_the_button_off_and_a_set_prefix_wins() {
        let dir = tempfile::tempdir().unwrap();
        let off = agent(dir.path(), Some(vec![]));
        assert!(start(&Logins::default(), dir.path(), "bob", &off).is_err());
        let mut a = agent(dir.path(), None);
        assert_eq!(npm_prefix(&a), Some(dir.path().join(".local")));
        a.env.insert("NPM_CONFIG_PREFIX".into(), "/opt/npm".into());
        assert_eq!(npm_prefix(&a), None);
    }
}
