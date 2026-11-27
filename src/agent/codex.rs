//! Codex: `codex exec --json`, resumed with `codex exec resume <id>`.

use super::{AgentEvent, Driver, Turn, TurnResult};
use crate::settings::AgentSettings;
use serde_json::Value;

pub struct Codex;

fn is_retry(message: &Value) -> bool {
    message["message"]
        .as_str()
        .is_some_and(|m| m.starts_with("Reconnecting"))
}

fn item_event(item: &Value, result: &mut TurnResult) -> Option<AgentEvent> {
    match item["type"].as_str()? {
        "agent_message" => {
            let text = item["text"].as_str()?.to_owned();
            result.text.clone_from(&text);
            Some(AgentEvent::Text(text))
        }
        "error" => {
            let notice = item["message"].as_str()?.to_owned();
            result.retry = Some(notice.clone());
            Some(AgentEvent::Retry(notice))
        }
        "command_execution" => item["command"]
            .as_str()
            .map(|c| AgentEvent::Tool(c.to_owned())),
        _ => None,
    }
}

impl Driver for Codex {
    fn args(&self, turn: &Turn<'_>, settings: &AgentSettings) -> Vec<String> {
        let mut args = vec!["exec".to_owned(), "--json".to_owned()];
        if let Some(model) = &settings.model {
            args.extend(["--model".into(), model.clone()]);
        }
        args.extend(settings.args.iter().cloned());
        if let Some(session) = turn.session {
            args.extend(["resume".into(), session.to_owned()]);
        }
        // Read the prompt from stdin.
        args.push("-".into());
        args
    }

    fn parse(&self, line: &str, result: &mut TurnResult) -> Option<AgentEvent> {
        let message: Value = serde_json::from_str(line).ok()?;
        match message["type"].as_str()? {
            "thread.started" => {
                let id = message["thread_id"].as_str()?.to_owned();
                result.session_id = Some(id.clone());
                Some(AgentEvent::Session(id))
            }
            "item.completed" => item_event(&message["item"], result),
            "turn.completed" => Some(AgentEvent::Finished),
            // Codex reports its reconnects as errors; the turn goes on.
            "error" if is_retry(&message) => {
                let notice = message["message"].as_str().unwrap_or_default().to_owned();
                result.retry = Some(notice.clone());
                Some(AgentEvent::Retry(notice))
            }
            "turn.failed" | "error" => {
                result.is_error = true;
                Some(AgentEvent::Finished)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resumes_and_parses() {
        let settings: AgentSettings =
            serde_yaml::from_str("driver: codex\ncommand: codex\n").unwrap();
        let args = Codex.args(
            &Turn {
                prompt: "x",
                session: Some("t1"),
            },
            &settings,
        );
        assert_eq!(args, ["exec", "--json", "resume", "t1", "-"]);
        let mut result = TurnResult::default();
        for line in [
            r#"{"type":"thread.started","thread_id":"t1"}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
            r#"{"type":"turn.completed","usage":{}}"#,
        ] {
            Codex.parse(line, &mut result);
        }
        assert_eq!(result.session_id.as_deref(), Some("t1"));
        assert_eq!(result.text, "done");
        assert!(!result.is_error);
    }

    #[test]
    fn a_reconnect_is_a_retry_not_a_failure() {
        let mut result = TurnResult::default();
        let notice = r#"{"type":"error","message":"Reconnecting... waiting for network (Connection failed)"}"#;
        assert_eq!(
            Codex.parse(notice, &mut result),
            Some(AgentEvent::Retry(
                "Reconnecting... waiting for network (Connection failed)".into()
            ))
        );
        let fallback = r#"{"type":"item.completed","item":{"type":"error","message":"Falling back to HTTPS"}}"#;
        assert!(matches!(
            Codex.parse(fallback, &mut result),
            Some(AgentEvent::Retry(_))
        ));
        Codex.parse(r#"{"type":"turn.completed","usage":{}}"#, &mut result);
        assert!(!result.is_error);
        assert_eq!(result.retry.as_deref(), Some("Falling back to HTTPS"));
        Codex.parse(
            r#"{"type":"error","message":"quota exceeded"}"#,
            &mut result,
        );
        assert!(result.is_error);
    }
}
