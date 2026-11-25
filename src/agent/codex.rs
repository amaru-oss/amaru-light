//! Codex: `codex exec --json`, resumed with `codex exec resume <id>`.

use super::{AgentEvent, Driver, Turn, TurnResult};
use crate::settings::AgentSettings;
use serde_json::Value;

pub struct Codex;

fn item_event(item: &Value, result: &mut TurnResult) -> Option<AgentEvent> {
    match item["type"].as_str()? {
        "agent_message" => {
            let text = item["text"].as_str()?.to_owned();
            result.text.clone_from(&text);
            Some(AgentEvent::Text(text))
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
    use crate::settings::DriverKind;

    #[test]
    fn resumes_and_parses() {
        let settings = AgentSettings {
            driver: DriverKind::Codex,
            command: "codex".into(),
            args: vec![],
            model: None,
            env: std::collections::BTreeMap::new(),
            login: None,
            login_status: None,
        };
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
}
