//! The result file the agent writes at the end of every step.
//!
//! The daemon never parses the agent's prose. Each step ends with the agent
//! writing `.amaru/result.json` in the worktree, which is validated here.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Path of the result file, relative to the worktree.
pub const RESULT_FILE: &str = ".amaru/result.json";

/// Appended to every prompt so the agent knows how to report back.
pub const INSTRUCTIONS: &str = r#"When you finish, write the file `.amaru/result.json` in the repository root
(create the directory if needed; it is ignored by git) with exactly this JSON shape:

{
  "status": "done" | "blocked" | "needs_clarification",
  "summary": "what you changed and why, in a few sentences",
  "complete": true or false,
  "question": "only for needs_clarification: the question for the issue author",
  "replies": [{"thread_id": "id from the feedback list", "body": "how you addressed it"}]
}

- "done": the work for this step is finished and committed on the current branch.
- "blocked": you cannot proceed; explain why in "summary".
- "needs_clarification": the issue is ambiguous; ask one precise question.
- "complete": true only if every requirement of the issue is met.
- "replies": only when you addressed review feedback; otherwise [].
Do not push, merge, switch branches or change issues and pull requests."#;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Done,
    Blocked,
    NeedsClarification,
}

/// A reply to one review thread.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub thread_id: String,
    pub body: String,
}

/// The validated contents of the result file.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub status: Status,
    pub summary: String,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub question: Option<String>,
    #[serde(default)]
    pub replies: Vec<Reply>,
}

pub fn path(worktree: &Path) -> PathBuf {
    worktree.join(RESULT_FILE)
}

/// Remove a stale result before a new step starts.
pub fn clear(worktree: &Path) -> Result<()> {
    match std::fs::remove_file(path(worktree)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

/// Read and validate the result file.
pub fn read(worktree: &Path) -> Result<Report> {
    let file = path(worktree);
    let text = std::fs::read_to_string(&file)
        .with_context(|| format!("the agent did not write {RESULT_FILE}"))?;
    let report: Report =
        serde_json::from_str(&text).with_context(|| format!("{RESULT_FILE} is not valid"))?;
    if report.summary.trim().is_empty() {
        anyhow::bail!("{RESULT_FILE} has an empty summary");
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, text: &str) {
        std::fs::create_dir_all(dir.join(".amaru")).unwrap();
        std::fs::write(path(dir), text).unwrap();
    }

    #[test]
    fn reads_a_valid_report() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            r#"{"status":"done","summary":"Added X","complete":true,"replies":[{"thread_id":"1","body":"fixed"}]}"#,
        );
        let report = read(dir.path()).unwrap();
        assert_eq!(report.status, Status::Done);
        assert!(report.complete);
        assert_eq!(report.replies[0].body, "fixed");
    }

    #[test]
    fn rejects_missing_invalid_and_unknown() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(dir.path()).is_err());
        write(dir.path(), r#"{"status":"finished","summary":"x"}"#);
        assert!(read(dir.path()).is_err());
        write(dir.path(), r#"{"status":"done","summary":"x","extra":1}"#);
        assert!(read(dir.path()).is_err());
        write(dir.path(), r#"{"status":"done","summary":" "}"#);
        assert!(read(dir.path()).is_err());
    }

    #[test]
    fn clear_tolerates_a_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        clear(dir.path()).unwrap();
        write(dir.path(), "{}");
        clear(dir.path()).unwrap();
        assert!(!path(dir.path()).exists());
    }
}
