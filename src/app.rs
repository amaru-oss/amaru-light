//! Wiring the daemon's parts together from the settings.

use crate::{
    db::Store, forge::github::GitHub, lifecycle::Services, prompts::Prompts, settings::Settings,
    worktree::Workspace,
};
use anyhow::Result;
use std::{path::Path, sync::Arc};

/// Build the services for a daemon file.
pub fn services(config: &Path) -> Result<Arc<Services>> {
    let settings = Settings::load(config)?;
    let state = settings.state_dir();
    let github = Arc::new(GitHub::new(settings.daemon.gh.clone()));
    let prompts_dir = settings.base_dir.join("prompts");
    Ok(Arc::new(Services {
        store: Arc::new(Store::open(&state.join("amaru.db"))?),
        workspace: Workspace::new(&state),
        prompts: Prompts::new(prompts_dir.is_dir().then_some(prompts_dir)),
        source: github.clone(),
        forge: github,
        settings: Arc::new(settings),
    }))
}

/// Split `owner/name#42` into the repository and the issue number.
pub fn parse_issue(text: &str) -> Option<(String, u64)> {
    let (repo, number) = text.split_once('#')?;
    let number = number.parse().ok()?;
    repo.contains('/').then(|| (repo.to_owned(), number))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_issue_references() {
        assert_eq!(parse_issue("me/app#42"), Some(("me/app".into(), 42)));
        assert_eq!(parse_issue("app#42"), None);
        assert_eq!(parse_issue("me/app#x"), None);
    }

    #[test]
    fn builds_services_from_the_examples() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/daemon.yaml");
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("daemon.yaml");
        std::fs::copy(&path, &copy).unwrap();
        std::fs::copy(
            path.with_file_name("project.yaml"),
            dir.path().join("project.yaml"),
        )
        .unwrap();
        let svc = services(&copy).unwrap();
        assert_eq!(svc.settings.projects.len(), 1);
        assert!(dir.path().join("data/amaru.db").exists());
    }
}
