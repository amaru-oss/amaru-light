//! Claude Code: `claude -p --output-format stream-json`.

use super::{AgentEvent, Driver, Turn, TurnResult};
use crate::settings::AgentSettings;
use serde_json::Value;

pub struct Claude;

fn assistant_event(message: &Value) -> Option<AgentEvent> {
    let content = message["message"]["content"].as_array()?;
    content.iter().find_map(|part| match part["type"].as_str() {
        Some("text") => part["text"]
            .as_str()
            .map(|t| AgentEvent::Text(t.to_owned())),
        Some("tool_use") => part["name"]
            .as_str()
            .map(|n| AgentEvent::Tool(n.to_owned())),
        _ => None,
    })
}

fn finish(message: &Value, result: &mut TurnResult) -> AgentEvent {
    if let Some(id) = message["session_id"].as_str() {
        result.session_id = Some(id.to_owned());
    }
    result.text = message["result"].as_str().unwrap_or_default().to_owned();
    result.is_error = message["is_error"].as_bool().unwrap_or(false);
    result.cost_usd = message["total_cost_usd"].as_f64();
    result.turns = message["num_turns"].as_u64();
    AgentEvent::Finished
}

impl Driver for Claude {
    fn args(&self, turn: &Turn<'_>, settings: &AgentSettings) -> Vec<String> {
        let mut args: Vec<String> = ["-p", "--output-format", "stream-json", "--verbose"]
            .map(String::from)
            .into();
        if let Some(model) = &settings.model {
            args.extend(["--model".into(), model.clone()]);
        }
        if let Some(session) = turn.session {
            args.extend(["--resume".into(), session.to_owned()]);
        }
        args.extend(settings.args.iter().cloned());
        args
    }

    fn parse(&self, line: &str, result: &mut TurnResult) -> Option<AgentEvent> {
        let message: Value = serde_json::from_str(line).ok()?;
        match message["type"].as_str()? {
            "system" if message["subtype"] == "init" => {
                let id = message["session_id"].as_str()?.to_owned();
                result.session_id = Some(id.clone());
                Some(AgentEvent::Session(id))
            }
            "assistant" => assistant_event(&message),
            "result" => Some(finish(&message, result)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::DriverKind;

    fn settings() -> AgentSettings {
        AgentSettings {
            driver: DriverKind::Claude,
            command: "claude".into(),
            args: vec!["--permission-mode".into(), "bypassPermissions".into()],
            model: Some("sonnet".into()),
            env: std::collections::BTreeMap::new(),
            login: None,
            login_with_key: None,
            login_status: None,
        }
    }

    #[test]
    fn builds_new_and_resumed_turns() {
        let new = Claude.args(
            &Turn {
                prompt: "x",
                session: None,
            },
            &settings(),
        );
        assert_eq!(
            new[..4],
            ["-p", "--output-format", "stream-json", "--verbose"]
        );
        assert!(!new.contains(&"--resume".to_owned()));
        assert_eq!(new.last().map(String::as_str), Some("bypassPermissions"));
        let resumed = Claude.args(
            &Turn {
                prompt: "x",
                session: Some("abc"),
            },
            &settings(),
        );
        assert!(resumed.windows(2).any(|w| w == ["--resume", "abc"]));
    }

    #[test]
    fn parses_the_stream() {
        let mut result = TurnResult::default();
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"s1"}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
            r#"not json"#,
            r#"{"type":"result","is_error":true,"result":"boom","session_id":"s1","total_cost_usd":0.5,"num_turns":3}"#,
        ];
        let events: Vec<_> = lines
            .iter()
            .filter_map(|l| Claude.parse(l, &mut result))
            .collect();
        assert_eq!(
            events,
            [
                AgentEvent::Session("s1".into()),
                AgentEvent::Tool("Bash".into()),
                AgentEvent::Finished
            ]
        );
        assert!(result.is_error);
        assert_eq!(result.text, "boom");
        assert_eq!(result.cost_usd, Some(0.5));
    }
}
