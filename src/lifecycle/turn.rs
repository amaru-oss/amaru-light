//! One agent turn: run, keep the session, and read the result file.

use super::{Cancelled, Job, blocked};
use crate::{
    agent::{Agent, AgentEvent, Turn, TurnResult, Workplace},
    contract::{self, Report},
    process::Exit,
    process::tail,
};
use anyhow::Result;
use serde_json::json;

/// Whose session a turn runs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Session {
    /// The ticket's own session, resumed and kept.
    Ticket,
    /// A separate session, for example an independent reviewer.
    Separate,
}

fn event_payload(event: &AgentEvent) -> Option<(&'static str, serde_json::Value)> {
    match event {
        AgentEvent::Session(id) => Some(("AGENT_SESSION", json!({ "session": id }))),
        AgentEvent::Tool(name) => Some(("AGENT_TOOL", json!({ "tool": name }))),
        AgentEvent::Text(text) => Some(("AGENT_TEXT", json!({ "text": tail(text, 500) }))),
        AgentEvent::Finished => None,
    }
}

/// The first line of `text`, cut to about `limit` bytes, for the log.
fn one_line(text: &str, limit: usize) -> String {
    let line = text.lines().next().unwrap_or_default();
    let end = (0..=limit.min(line.len()))
        .rev()
        .find(|i| line.is_char_boundary(*i))
        .unwrap_or(0);
    if end < text.len() {
        format!("{}…", &line[..end])
    } else {
        line.to_owned()
    }
}

impl Job<'_> {
    async fn run_agent(
        &self,
        agent: &str,
        prompt: &str,
        session: Option<&str>,
    ) -> Result<TurnResult> {
        let agent_name = agent;
        let agent = Agent::new(self.svc.settings.agent(agent)?.clone());
        let cwd = self.worktree()?;
        let place = Workplace {
            cwd: &cwd,
            timeout: self.project.step_timeout.0,
            idle: Some(self.project.idle_timeout.0).filter(|d| !d.is_zero()),
            cancel: self.cancel.clone(),
        };
        let (store, id) = (&self.svc.store, self.ticket.id);
        let how = session.map_or_else(|| "new session".to_owned(), |s| format!("session {s}"));
        self.log(&format!("{agent_name} turn started ({how})"));
        let result = agent
            .run(&Turn { prompt, session }, place, &mut |event| {
                if let AgentEvent::Tool(tool) = event {
                    self.log(&format!("{agent_name} runs {}", one_line(tool, 120)));
                }
                if let Some((kind, payload)) = event_payload(event) {
                    // The event log is best effort; the turn result is what counts.
                    let _ = store.event(id, kind, &payload);
                }
            })
            .await?;
        self.record_turn(agent_name, &result)?;
        Ok(result)
    }

    /// Log the end of an agent turn and record it as an event.
    fn record_turn(&self, agent: &str, result: &TurnResult) -> Result<()> {
        let cost = result
            .cost_usd
            .map(|c| format!(", ${c:.2}"))
            .unwrap_or_default();
        let turns = result
            .turns
            .map(|t| format!(", {t} turns"))
            .unwrap_or_default();
        let exit = result
            .exit
            .as_ref()
            .map_or_else(|| "no exit".to_owned(), |e| format!("{e:?}"));
        self.log(&format!("{agent} turn ended ({exit}{turns}{cost})"));
        self.event(
            "AGENT_TURN",
            &json!({ "exit": format!("{:?}", result.exit), "error": result.is_error,
                     "cost_usd": result.cost_usd, "turns": result.turns }),
        )
    }

    /// The prompt that rebuilds context when a session cannot be resumed.
    async fn context_pack(&self, prompt: &str) -> Result<String> {
        let (log, stat) = self
            .svc
            .workspace
            .summary(&self.worktree()?, &self.base_ref()?)
            .await?;
        let mut vars = self.issue_vars();
        vars.extend([("log".to_owned(), log), ("stat".to_owned(), stat)]);
        Ok(format!(
            "{}{prompt}",
            self.svc.prompts.text("context", &as_refs(&vars))
        ))
    }

    async fn attempt(&mut self, agent: &str, prompt: &str, mode: Session) -> Result<TurnResult> {
        let session = match mode {
            Session::Ticket => self.ticket.session_id.clone(),
            Session::Separate => None,
        };
        let mut result = self.run_agent(agent, prompt, session.as_deref()).await?;
        let lost_session =
            session.is_some() && !result.succeeded() && result.exit == Some(Exit::Code(1));
        if lost_session {
            self.event("SESSION_LOST", &json!({ "stderr": result.stderr }))?;
            let fresh = self.context_pack(prompt).await?;
            result = self.run_agent(agent, &fresh, None).await?;
        }
        // Keep the session even when the turn failed: whatever runs next,
        // a retry or a human unblock, resumes the session the agent was in.
        if mode == Session::Ticket && result.session_id.is_some() {
            self.ticket.session_id.clone_from(&result.session_id);
            self.save()?;
        }
        self.judge(&result)?;
        Ok(result)
    }

    /// Turn a failed run into the right stop.
    fn judge(&self, result: &TurnResult) -> Result<()> {
        match result.exit {
            Some(Exit::Cancelled) => Err(Cancelled.into()),
            Some(Exit::TimedOut) => Err(blocked(format!(
                "the agent did not finish within {}",
                self.project.step_timeout
            ))),
            // Not blocked: the daemon retries in the same session, which is
            // what gets an agent past a dropped connection, and blocks the
            // ticket after a few stalls in a row.
            Some(Exit::Stalled) => Err(anyhow::anyhow!(
                "the agent printed nothing for {} and was stopped",
                self.project.idle_timeout
            )),
            _ if !result.succeeded() => {
                let detail = if result.text.is_empty() {
                    &result.stderr
                } else {
                    &result.text
                };
                Err(blocked(format!(
                    "the agent failed: {}",
                    tail(detail, 1_000)
                )))
            }
            _ => Ok(()),
        }
    }

    /// Run a turn and return the validated result file.
    pub(super) async fn turn(
        &mut self,
        agent: &str,
        prompt: &str,
        mode: Session,
    ) -> Result<Report> {
        let worktree = self.worktree()?;
        contract::clear(&worktree)?;
        let first = self.attempt(agent, prompt, mode).await?;
        match contract::read(&worktree) {
            Ok(report) => Ok(report),
            Err(error) => {
                let ask = self
                    .svc
                    .prompts
                    .render("missing_result", &[("error", &format!("{error:#}"))]);
                let session = first.session_id.clone();
                let retry = self.run_agent(agent, &ask, session.as_deref()).await?;
                if !retry.succeeded() {
                    return Err(blocked(format!(
                        "the agent did not report its result: {error:#}"
                    )));
                }
                contract::read(&worktree).map_err(|e| blocked(format!("{e:#}")))
            }
        }
    }

    /// `#42`, or the Jira key, for prompts.
    pub(super) fn issue_name(&self) -> String {
        let t = &self.ticket;
        t.key.clone().unwrap_or_else(|| format!("#{}", t.number))
    }

    /// Placeholders that describe the ticket's issue and branch.
    pub(super) fn issue_vars(&self) -> Vec<(String, String)> {
        let t = &self.ticket;
        let latest = match &t.key {
            Some(_) => format!(
                "The issue as it reads now, with its comments:\n\n{}",
                t.body
            ),
            None => format!(
                "Read the issue's latest comments with\n`gh issue view {} --repo {} --comments`.",
                t.number, t.repo
            ),
        };
        let conventions = t.key.as_ref().map_or_else(String::new, |key| {
            format!(
                "\n- The issue is {key} in Jira. Start the subject of every commit message with `{key}: `, as the project requires."
            )
        });
        vec![
            ("repo".into(), t.repo.clone()),
            ("number".into(), t.number.to_string()),
            ("issue".into(), self.issue_name()),
            ("latest".into(), latest),
            ("conventions".into(), conventions),
            ("title".into(), t.title.clone()),
            ("body".into(), t.body.clone()),
            ("branch".into(), t.branch.clone().unwrap_or_default()),
            ("base".into(), t.base_ref().unwrap_or_default()),
        ]
    }
}

/// Borrow owned placeholder pairs for `Prompts`.
pub(super) fn as_refs(vars: &[(String, String)]) -> Vec<(&str, &str)> {
    vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect()
}

#[cfg(test)]
mod tests {
    use super::one_line;

    #[test]
    fn log_lines_keep_the_first_line_only() {
        assert_eq!(one_line("cargo test", 120), "cargo test");
        assert_eq!(
            one_line("apply_patch <<EOF\n*** Begin", 120),
            "apply_patch <<EOF…"
        );
        assert_eq!(one_line("abcdef", 3), "abc…");
        assert_eq!(one_line("é", 1), "…");
    }
}
