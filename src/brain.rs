use anyhow::{Result, bail};
use serde_json::{Value, json};

pub fn check(data: &Value, allowed: &Value) -> Result<()> {
    if !data.is_object() {
        bail!("Expected a JSON object");
    }
    let name = data["action"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing action"))?;
    if allowed.get(name).is_none() {
        bail!("Action '{name}' is not allowed");
    }
    if !data["args"].is_null() && !data["args"].is_object() {
        bail!("args must be an object");
    }
    Ok(())
}

pub fn decide<F, V>(
    goal: &str,
    facts: &Value,
    allowed: &Value,
    mut call_json: F,
    validate: V,
    mut fallback: Value,
) -> Value
where
    F: FnMut(&str) -> Result<Value>,
    V: Fn(&Value) -> Result<()>,
{
    let facts: String = serde_json::to_string_pretty(facts)
        .unwrap_or_default()
        .chars()
        .take(30000)
        .collect();
    let mut error = String::new();
    for _ in 0..2 {
        let prompt = format!(
            r#"You are the orchestration brain of Mini-Amaru. A deterministic runtime observes facts, enforces guardrails and performs every side effect. Choose ONLY the next action from ALLOWED ACTIONS. Never execute commands, invent facts, or choose blocked, gated, or in-flight work. Prefer eligible work that unblocks dependencies. If nothing safe remains, wait/stop/finish.
Return ONLY JSON: {{"action":"name","args":{{}},"reason":"short explanation"}}.
GOAL: {goal}
ALLOWED ACTIONS: {allowed}
FACTS (authoritative): {facts}
PREVIOUS ANSWER ERROR: {error}"#
        );
        let result = call_json(&prompt).and_then(|mut data| {
            check(&data, allowed)?;
            if data["args"].is_null() {
                data["args"] = json!({});
            }
            validate(&data)?;
            data["source"] = json!("brain");
            Ok(data)
        });
        match result {
            Ok(data) => return data,
            Err(e) => error = e.to_string(),
        }
    }
    fallback["source"] = json!("fallback");
    fallback["reason"] = json!(format!(
        "[fallback: {error}] {}",
        fallback["reason"].as_str().unwrap_or("")
    ));
    fallback
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reject_unknown_action_and_args() {
        let allowed = json!({"wait":{}});
        assert!(check(&json!({"action":"destroy"}), &allowed).is_err());
        assert!(check(&json!({"action":"wait","args":[]}), &allowed).is_err());
    }
    #[test]
    fn fallback_when_invalid() {
        let d = decide(
            "goal",
            &json!({}),
            &json!({"wait":{}}),
            |_| Ok(json!({"action":"destroy"})),
            |_| Ok(()),
            json!({"action":"wait","args":{},"reason":"safe"}),
        );
        assert_eq!(d["source"], "fallback");
        assert_eq!(d["action"], "wait");
    }
    #[test]
    fn retries_with_validation() {
        let mut n = 0;
        let d = decide(
            "goal",
            &json!({}),
            &json!({"start_work":{}}),
            |_| {
                n += 1;
                Ok(json!({"action":"start_work","args":{"ref":n}}))
            },
            |d| {
                if d["args"]["ref"] == 2 {
                    Ok(())
                } else {
                    bail!("ineligible")
                }
            },
            json!({"action":"wait"}),
        );
        assert_eq!(d["source"], "brain");
        assert_eq!(n, 2);
    }
}
