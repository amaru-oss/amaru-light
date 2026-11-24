//! Project checks run in a ticket's worktree.

use crate::{
    process::{self, Cancel, Exit},
    settings::CheckSettings,
};
use anyhow::Result;
use serde::Serialize;
use std::path::Path;

/// The first check that failed.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Failure {
    pub name: String,
    pub command: String,
    pub exit: String,
    /// The end of the combined output.
    pub output: String,
}

/// Run every check in order and stop at the first failure.
pub async fn run(
    checks: &[CheckSettings],
    worktree: &Path,
    cancel: Option<Cancel>,
) -> Result<Option<Failure>> {
    for check in checks {
        let (exit, output) = process::shell(
            &check.run,
            worktree.to_path_buf(),
            check.timeout.0,
            cancel.clone(),
        )
        .await?;
        if exit == Exit::Cancelled {
            anyhow::bail!("cancelled");
        }
        if exit != Exit::Code(0) {
            return Ok(Some(Failure {
                name: check.name.clone(),
                command: check.run.clone(),
                exit: format!("{exit:?}"),
                output,
            }));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::Span;

    fn check(name: &str, run: &str) -> CheckSettings {
        CheckSettings {
            name: name.into(),
            run: run.into(),
            timeout: Span::secs(10),
        }
    }

    #[tokio::test]
    async fn stops_at_the_first_failure() {
        let dir = tempfile::tempdir().unwrap();
        let checks = [
            check("ok", "true"),
            check("tests", "echo 2 failed; exit 1"),
            check("never", "touch ran"),
        ];
        let failure = run(&checks, dir.path(), None).await.unwrap().unwrap();
        assert_eq!(failure.name, "tests");
        assert_eq!(failure.output, "2 failed\n");
        assert!(!dir.path().join("ran").exists());
        assert!(run(&checks[..1], dir.path(), None).await.unwrap().is_none());
    }
}
