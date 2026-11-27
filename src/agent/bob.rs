//! IBM Bob Shell: `bob run --format stream-json`, resumed with `--resume <task>`.
//!
//! Bob streams the assistant's text in small pieces (`message` events), so
//! the driver joins them into whole messages. Its only session id, the task
//! id, comes with the final `result` event.

use super::{AgentEvent, Driver, Turn, TurnResult};
use crate::settings::AgentSettings;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Mutex};

/// The environment variable that picks Bob's model tier: fast, premium or
/// ultra. Bob's `--model` flag exists only for IBM's own developers.
pub const MODEL_TIER_ENV: &str = "BOB_SHELL_MODEL_TIER";

#[derive(Default)]
pub struct Bob {
    /// The assistant message being streamed.
    message: Mutex<Message>,
}

#[derive(Default)]
struct Message {
    text: String,
    /// A tool call ended the message; the next piece starts a new one.
    ended: bool,
    /// The ended message is not in the log yet.
    unlogged: bool,
}

impl Bob {
    fn message(&self) -> std::sync::MutexGuard<'_, Message> {
        self.message
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn stream(&self, piece: &str, result: &mut TurnResult) {
        let mut message = self.message();
        if message.ended {
            *message = Message::default();
        }
        message.text.push_str(piece);
        result.text.clone_from(&message.text);
    }

    /// A tool call ends the streamed message.
    fn end_message(&self) {
        let mut message = self.message();
        if !message.ended {
            message.ended = true;
            message.unlogged = !message.text.trim().is_empty();
        }
    }

    /// The message a tool call ended, once, when the call's result arrives:
    /// the log gets whole messages, not the pieces Bob streams.
    fn ended_message(&self) -> Option<AgentEvent> {
        let mut message = self.message();
        if !message.unlogged {
            return None;
        }
        message.unlogged = false;
        Some(AgentEvent::Text(message.text.clone()))
    }
}

fn finish(message: &Value, result: &mut TurnResult) -> AgentEvent {
    let stats = &message["stats"];
    if let Some(id) = stats["task_id"].as_str() {
        result.session_id = Some(id.to_owned());
    }
    if let Some(text) = message["last_message"].as_str() {
        result.text = text.to_owned();
    }
    if message["status"].as_str() != Some("success") {
        result.is_error = true;
    }
    result.cost_usd = stats["session_costs"].as_f64();
    AgentEvent::Finished
}

impl Driver for Bob {
    fn args(&self, turn: &Turn<'_>, settings: &AgentSettings) -> Vec<String> {
        // `bob run` approves every tool call by itself. `--trust` marks the
        // worktree as trusted, which Bob would otherwise ask about.
        let mut args: Vec<String> = ["run", "--format", "stream-json", "--trust"]
            .map(String::from)
            .into();
        if let Some(session) = turn.session {
            args.extend(["--resume".into(), session.to_owned()]);
        }
        args.extend(settings.args.iter().cloned());
        args
    }

    fn env(&self, settings: &AgentSettings) -> BTreeMap<String, String> {
        settings
            .model
            .iter()
            .map(|tier| (MODEL_TIER_ENV.to_owned(), tier.clone()))
            .collect()
    }

    fn parse(&self, line: &str, result: &mut TurnResult) -> Option<AgentEvent> {
        let message: Value = serde_json::from_str(line).ok()?;
        match message["type"].as_str()? {
            "message" if message["role"] == "assistant" && message["isReasoning"] != true => {
                self.stream(message["content"].as_str()?, result);
                None
            }
            "tool_use" => {
                self.end_message();
                message["tool_name"]
                    .as_str()
                    .map(|n| AgentEvent::Tool(n.to_owned()))
            }
            "tool_result" => self.ended_message(),
            // Bob reports a failed task, such as one that hit --max-turns
            // or --max-cost, as an error event before it ends.
            "error" => {
                result.is_error = true;
                if let Some(text) = message["message"].as_str() {
                    result.text = text.to_owned();
                }
                None
            }
            "result" => Some(finish(&message, result)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::DriverKind;

    fn settings(model: Option<&str>) -> AgentSettings {
        AgentSettings {
            driver: DriverKind::Bob,
            command: "bob".into(),
            args: vec!["--accept-license".into()],
            model: model.map(str::to_owned),
            env: BTreeMap::new(),
            login: None,
            login_with_key: None,
            login_status: None,
            setup: Vec::new(),
            instructions: None,
            install: None,
        }
    }

    #[test]
    fn builds_new_and_resumed_turns() {
        let new = Turn {
            prompt: "x",
            session: None,
        };
        assert_eq!(
            Bob::default().args(&new, &settings(None)),
            [
                "run",
                "--format",
                "stream-json",
                "--trust",
                "--accept-license"
            ]
        );
        let resumed = Turn {
            prompt: "x",
            session: Some("task-1"),
        };
        let args = Bob::default().args(&resumed, &settings(None));
        assert!(args.windows(2).any(|w| w == ["--resume", "task-1"]));
    }

    #[test]
    fn the_model_is_a_tier_in_the_environment() {
        assert!(Bob::default().env(&settings(None)).is_empty());
        let env = Bob::default().env(&settings(Some("ultra")));
        assert_eq!(env.get(MODEL_TIER_ENV).map(String::as_str), Some("ultra"));
        let args = Bob::default().args(
            &Turn {
                prompt: "x",
                session: None,
            },
            &settings(Some("ultra")),
        );
        assert!(!args.contains(&"--model".to_owned()));
    }

    /// A turn as Bob streams it: reasoning, text in pieces, two tool calls.
    const STREAM: [&str; 11] = [
        r#"{"type":"message","role":"user","content":"do it"}"#,
        r#"{"type":"message","role":"assistant","isReasoning":true,"content":"hmm"}"#,
        r#"{"type":"message","role":"assistant","content":"Let me "}"#,
        r#"{"type":"message","role":"assistant","content":"look."}"#,
        r#"{"type":"tool_use","tool_name":"read_file","tool_id":"t1","parameters":{}}"#,
        r#"{"type":"tool_result","tool_id":"t1","status":"success","output":"..."}"#,
        r#"{"type":"tool_use","tool_name":"execute_command","tool_id":"t2","parameters":{}}"#,
        r#"{"type":"tool_result","tool_id":"t2","status":"success","output":"..."}"#,
        r#"not json"#,
        r#"{"type":"message","role":"assistant","content":"Done."}"#,
        r#"{"type":"result","status":"success","stats":{"task_id":"task-9","session_costs":0.25,"tool_calls":2}}"#,
    ];

    #[test]
    fn joins_streamed_text_into_messages() {
        let bob = Bob::default();
        let mut result = TurnResult::default();
        let events: Vec<_> = STREAM
            .iter()
            .filter_map(|l| bob.parse(l, &mut result))
            .collect();
        assert_eq!(
            events,
            [
                AgentEvent::Tool("read_file".into()),
                AgentEvent::Text("Let me look.".into()),
                AgentEvent::Tool("execute_command".into()),
                AgentEvent::Finished,
            ]
        );
        assert_eq!(result.text, "Done.");
        assert_eq!(result.session_id.as_deref(), Some("task-9"));
        assert_eq!(result.cost_usd, Some(0.25));
        assert!(!result.is_error);
    }

    #[test]
    fn an_error_event_fails_the_turn() {
        let bob = Bob::default();
        let mut result = TurnResult::default();
        for line in [
            r#"{"type":"error","severity":"error","message":"The task reached the maximum of 5 turns."}"#,
            r#"{"type":"result","status":"success","stats":{"task_id":"task-2"}}"#,
        ] {
            bob.parse(line, &mut result);
        }
        assert!(result.is_error);
        assert_eq!(result.text, "The task reached the maximum of 5 turns.");
        assert_eq!(result.session_id.as_deref(), Some("task-2"));
    }
}
