//! Driving agent CLIs as resumable, streaming sessions.
//!
//! The daemon starts one session per ticket and resumes it for every later
//! step, so the agent keeps its context: the code it read, the plan it made
//! and the feedback it already handled.

mod bob;
mod claude;
mod codex;

use crate::{
    process::{self, Cancel, Exit, Spec},
    settings::{AgentSettings, DriverKind},
};
use anyhow::Result;
use std::{collections::BTreeMap, path::Path, time::Duration};

/// One request to the agent.
#[derive(Clone, Copy, Debug)]
pub struct Turn<'a> {
    pub prompt: &'a str,
    /// Resume this session instead of starting a new one.
    pub session: Option<&'a str>,
}

/// What the agent reported while it worked.
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    Session(String),
    Text(String),
    Tool(String),
    /// The CLI is retrying, for example reconnecting to the model. Not progress.
    Retry(String),
    Finished,
}

/// The end of a turn.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TurnResult {
    pub session_id: Option<String>,
    /// The agent's last message.
    pub text: String,
    /// The CLI reported a failure inside the session.
    pub is_error: bool,
    pub cost_usd: Option<f64>,
    pub turns: Option<u64>,
    /// How the process ended; `None` until it has.
    pub exit: Option<Exit>,
    pub stderr: String,
    /// The latest retry notice, such as `Reconnecting... waiting for network`.
    pub retry: Option<String>,
}

impl TurnResult {
    /// The process exited cleanly and the CLI reported no error.
    pub fn succeeded(&self) -> bool {
        self.exit == Some(Exit::Code(0)) && !self.is_error
    }
}

/// How to talk to one agent CLI.
pub trait Driver: Send + Sync {
    /// Arguments for a turn; the prompt goes to stdin.
    fn args(&self, turn: &Turn<'_>, settings: &AgentSettings) -> Vec<String>;
    /// Read one line of the CLI's JSON stream.
    fn parse(&self, line: &str, result: &mut TurnResult) -> Option<AgentEvent>;
    /// Environment variables the driver sets from the settings, under the
    /// agent's own `env`.
    fn env(&self, _settings: &AgentSettings) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
}

/// A configured agent CLI.
pub struct Agent {
    settings: AgentSettings,
    driver: Box<dyn Driver>,
}

impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("command", &self.settings.command)
            .finish_non_exhaustive()
    }
}

/// Where a turn runs and how long it may take.
#[derive(Clone, Debug)]
pub struct Workplace<'a> {
    pub cwd: &'a Path,
    pub timeout: Duration,
    /// Stop the turn once the agent prints nothing for this long.
    pub idle: Option<Duration>,
    pub cancel: Option<Cancel>,
}

impl Agent {
    pub fn new(settings: AgentSettings) -> Self {
        let driver: Box<dyn Driver> = match settings.driver {
            DriverKind::Claude => Box::new(claude::Claude),
            DriverKind::Codex => Box::new(codex::Codex),
            DriverKind::Bob => Box::<bob::Bob>::default(),
        };
        Self { settings, driver }
    }

    /// Run one turn, passing each event to `on_event` as it arrives.
    pub async fn run(
        &self,
        turn: &Turn<'_>,
        place: Workplace<'_>,
        on_event: &mut (dyn FnMut(&AgentEvent) + Send),
    ) -> Result<TurnResult> {
        let mut env = self.driver.env(&self.settings);
        env.extend(self.settings.env.clone());
        let spec = Spec {
            program: self.settings.command.clone(),
            args: self.driver.args(turn, &self.settings),
            env,
            cwd: place.cwd.to_path_buf(),
            stdin: Some(turn.prompt.to_owned()),
            timeout: place.timeout,
            idle: place.idle,
        };
        let mut result = TurnResult {
            session_id: turn.session.map(str::to_owned),
            ..TurnResult::default()
        };
        let output = process::run_watched(&spec, place.cancel, &mut |line| {
            let event = self.driver.parse(line, &mut result);
            if let Some(event) = &event {
                on_event(event);
            }
            !matches!(event, Some(AgentEvent::Retry(_)))
        })
        .await?;
        result.exit = Some(output.exit);
        result.stderr = output.stderr;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A fake CLI that echoes its arguments and prompt as Claude stream-json.
    const FAKE: &str = r#"prompt=$(cat); printf '{"type":"system","subtype":"init","session_id":"s-1"}\n'; printf '{"type":"assistant","message":{"content":[{"type":"text","text":"%s"}]}}\n' "$prompt"; printf '{"type":"result","is_error":false,"result":"args: %s","session_id":"s-1","num_turns":2}\n' "$*""#;

    fn fake_claude(dir: &Path) -> Agent {
        let script = dir.join("fake-claude");
        std::fs::write(&script, format!("#!/bin/sh\n{FAKE}\n")).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
        std::fs::set_permissions(&script, mode).unwrap();
        Agent::new(AgentSettings {
            driver: DriverKind::Claude,
            command: script.to_string_lossy().into_owned(),
            args: vec![],
            model: None,
            env: BTreeMap::new(),
            login: None,
            login_with_key: None,
            login_status: None,
            setup: Vec::new(),
            instructions: None,
            install: None,
        })
    }

    #[tokio::test]
    async fn runs_a_turn_and_collects_events() {
        let dir = tempfile::tempdir().unwrap();
        let agent = fake_claude(dir.path());
        let mut events = Vec::new();
        let place = Workplace {
            cwd: dir.path(),
            timeout: Duration::from_secs(10),
            idle: None,
            cancel: None,
        };
        let turn = Turn {
            prompt: "do it",
            session: Some("s-1"),
        };
        let result = agent
            .run(&turn, place, &mut |e| events.push(e.clone()))
            .await
            .unwrap();
        assert!(result.succeeded());
        assert_eq!(result.session_id.as_deref(), Some("s-1"));
        assert!(result.text.contains("--resume s-1"));
        assert_eq!(result.turns, Some(2));
        assert_eq!(events[0], AgentEvent::Session("s-1".into()));
        assert_eq!(events[1], AgentEvent::Text("do it".into()));
        assert_eq!(events[2], AgentEvent::Finished);
    }
}
