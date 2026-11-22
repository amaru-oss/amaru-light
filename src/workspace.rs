//! Prepare runtime storage and a missing project checkout before running agents.
use crate::{
    config::Settings,
    gitops::{Git, command_output},
};
use anyhow::{Context, Result, bail};
use std::{fs, path::Path, process::Command};

fn create_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("Cannot create directory {}", path.display()))
}

pub fn ensure_runtime_dirs(settings: &Settings) -> Result<()> {
    if let Some(parent) = settings.db_path().parent() {
        create_dir(parent)?;
    }
    create_dir(&settings.artifacts_dir())?;
    let worktrees = settings.root_dir.join(
        settings.project["loop"]["worktrees_dir"]
            .as_str()
            .unwrap_or("data/worktrees"),
    );
    create_dir(&worktrees)
}

pub fn ensure_project(settings: &Settings) -> Result<()> {
    ensure_runtime_dirs(settings)?;
    let root = settings.project_root();
    let repository = settings.project["project"]["repository"]
        .as_str()
        .unwrap_or("")
        .trim()
        .trim_end_matches('/');
    if root.exists() {
        if !root.is_dir() {
            bail!("Project path is not a directory: {}", root.display());
        }
        // Existing checkouts and scratch directories belong to the user. Never replace them.
        if repository.is_empty() || fs::read_dir(&root)?.next().is_some() {
            return Ok(());
        }
    }
    if repository.is_empty() {
        return create_dir(&root);
    }
    let parent = root
        .parent()
        .context("Project checkout has no parent directory")?;
    create_dir(parent)?;
    let cli = settings.project["github"]["cli"].as_str().unwrap_or("gh");
    let output = command_output(Command::new(cli).args(["repo", "clone", repository]).arg(&root).current_dir(parent).env("GIT_TERMINAL_PROMPT", "0"), 300)
        .with_context(|| format!("Cannot clone {repository} into {}. Install and authenticate the configured GitHub CLI ({cli})", root.display()))?;
    if !output.status.success() {
        bail!(
            "Cannot clone {repository} into {}. Check GitHub authentication and repository access: {}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let git = Git::new(&root);
    if !root.join(".git").exists()
        || !git
            .run(&["rev-parse", "--git-dir"], None, false)?
            .status
            .success()
    {
        bail!(
            "Clone command did not create a Git checkout at {}",
            root.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn settings(root: &Path) -> Settings {
        Settings {
            root_dir: root.to_owned(),
            project_path: root.join("projects/test.yaml"),
            project: json!({"project":{"name":"test","root":root.join("repos/project")},"loop":{"worktrees_dir":"custom/worktrees"}}),
            runtime: json!({"storage":{"database":"nested/state.db","artifacts":"nested/artifacts"}}),
            providers: json!({}),
            models: json!({}),
            routing: json!({}),
        }
    }
    #[test]
    fn creates_runtime_and_scratch_directories_idempotently() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let settings = settings(dir.path());
        ensure_project(&settings)?;
        fs::write(settings.project_root().join("keep.txt"), "keep")?;
        ensure_project(&settings)?;
        assert!(settings.db_path().parent().unwrap().is_dir());
        assert!(settings.artifacts_dir().is_dir());
        assert!(dir.path().join("custom/worktrees").is_dir());
        assert_eq!(
            fs::read_to_string(settings.project_root().join("keep.txt"))?,
            "keep"
        );
        Ok(())
    }
    #[test]
    fn refuses_a_file_as_project_root() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let settings = settings(dir.path());
        create_dir(settings.project_root().parent().unwrap())?;
        fs::write(settings.project_root(), "keep")?;
        assert!(
            ensure_project(&settings)
                .unwrap_err()
                .to_string()
                .contains("not a directory")
        );
        assert_eq!(fs::read_to_string(settings.project_root())?, "keep");
        Ok(())
    }
    #[cfg(unix)]
    fn fake_cli(root: &Path, script: &str) -> Result<std::path::PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let path = root.join("gh-fixture");
        fs::write(&path, format!("#!/bin/sh\n{script}\n"))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        Ok(path)
    }
    #[test]
    #[cfg(unix)]
    fn clones_missing_checkout_and_preserves_existing_work() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut settings = settings(dir.path());
        let origin = dir.path().join("origin");
        create_dir(&origin)?;
        let git = Git::new(&origin);
        git.run(&["init", "-b", "main"], None, true)?;
        fs::write(origin.join("README.md"), "repository content")?;
        git.run(&["add", "README.md"], None, true)?;
        git.run(
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "-m",
                "initial",
            ],
            None,
            true,
        )?;
        let cli = fake_cli(
            dir.path(),
            &format!(
                "test \"$1 $2 $3\" = 'repo clone owner/project' || exit 9\nprintf cloned >> '{}'/clone-count\nexec git clone '{}' \"$4\"",
                dir.path().display(),
                origin.display()
            ),
        )?;
        settings.project["github"] = json!({"cli":cli});
        settings.project["project"]["repository"] = json!("owner/project/");
        ensure_project(&settings)?;
        assert_eq!(
            fs::read_to_string(settings.project_root().join("README.md"))?,
            "repository content"
        );
        assert_eq!(
            Git::new(&settings.project_root()).current_branch(&settings.project_root())?,
            "main"
        );
        fs::write(settings.project_root().join("README.md"), "local change")?;
        ensure_project(&settings)?;
        assert_eq!(
            fs::read_to_string(settings.project_root().join("README.md"))?,
            "local change"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("clone-count"))?,
            "cloned"
        );
        Ok(())
    }
    #[test]
    #[cfg(unix)]
    fn failed_and_empty_successful_clones_are_errors() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let mut settings = settings(dir.path());
        settings.project["project"]["repository"] = json!("owner/project");
        let cli = fake_cli(dir.path(), "echo 'access denied' >&2; exit 1")?;
        settings.project["github"] = json!({"cli":cli});
        assert!(
            ensure_project(&settings)
                .unwrap_err()
                .to_string()
                .contains("access denied")
        );
        assert!(!settings.project_root().exists());
        fake_cli(dir.path(), "exit 0")?;
        assert!(
            ensure_project(&settings)
                .unwrap_err()
                .to_string()
                .contains("did not create a Git checkout")
        );
        create_dir(&settings.project_root())?;
        fs::write(settings.project_root().join("existing.txt"), "preserve")?;
        ensure_project(&settings)?;
        assert_eq!(
            fs::read_to_string(settings.project_root().join("existing.txt"))?,
            "preserve"
        );
        Ok(())
    }
}
