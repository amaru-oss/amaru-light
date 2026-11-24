//! Repository mirrors and one git worktree per ticket.

use crate::{forge::Target, process};
use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// Where the remote named for pushes lives: `fork` or `origin`.
const fn push_remote(target: &Target) -> &'static str {
    if target.fork.is_some() {
        "fork"
    } else {
        "origin"
    }
}

/// Mirrors under `<state>/repos/<owner>/<name>/mirror`, worktrees next to them.
#[derive(Clone, Debug)]
pub struct Workspace {
    root: PathBuf,
    /// Prefix of clone URLs; tests point it at local directories.
    url_base: String,
}

async fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    process::capture("git", args, cwd, GIT_TIMEOUT).await
}

impl Workspace {
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join("repos"),
            url_base: "https://github.com/".into(),
        }
    }

    pub fn with_url_base(mut self, url_base: &str) -> Self {
        url_base.clone_into(&mut self.url_base);
        self
    }

    fn url(&self, repo: &str) -> String {
        format!("{}{repo}.git", self.url_base)
    }

    pub fn mirror(&self, repo: &str) -> PathBuf {
        self.root.join(repo).join("mirror")
    }

    fn worktree_path(&self, repo: &str, branch: &str) -> PathBuf {
        self.root
            .join(repo)
            .join("worktrees")
            .join(branch.replace('/', "-"))
    }

    /// Clone the repository if needed, set the fork remote and fetch.
    pub async fn sync(&self, target: &Target) -> Result<PathBuf> {
        let mirror = self.mirror(&target.upstream);
        if !mirror.join(".git").is_dir() {
            let parent = mirror.parent().context("mirror has no parent")?;
            tokio::fs::create_dir_all(parent).await?;
            let dest = mirror.to_string_lossy().into_owned();
            git(
                parent,
                &["clone", "--quiet", &self.url(&target.upstream), &dest],
            )
            .await?;
        }
        if let Some(fork) = &target.fork {
            let url = self.url(fork);
            if git(&mirror, &["remote", "set-url", "fork", &url])
                .await
                .is_err()
            {
                git(&mirror, &["remote", "add", "fork", &url]).await?;
            }
        }
        git(&mirror, &["fetch", "--quiet", "--prune", "origin"]).await?;
        ensure_identity(&mirror).await?;
        Ok(mirror)
    }

    /// Create (or reuse after a restart) the worktree for a ticket branch.
    pub async fn create(&self, target: &Target, branch: &str, base_ref: &str) -> Result<PathBuf> {
        let mirror = self.mirror(&target.upstream);
        let path = self.worktree_path(&target.upstream, branch);
        if !path.join(".git").exists() {
            add_worktree(&mirror, &path, branch, base_ref).await?;
        }
        exclude_amaru(&mirror).await?;
        Ok(path)
    }

    /// Commits on the worktree's HEAD that `base_ref` does not have.
    pub async fn ahead(&self, worktree: &Path, base_ref: &str) -> Result<u64> {
        let range = format!("{base_ref}..HEAD");
        let out = git(worktree, &["rev-list", "--count", &range]).await?;
        out.trim().parse().context("unexpected rev-list output")
    }

    /// Commit anything the agent left uncommitted; true when a commit was made.
    pub async fn commit_leftovers(&self, worktree: &Path, message: &str) -> Result<bool> {
        if git(worktree, &["status", "--porcelain"])
            .await?
            .trim()
            .is_empty()
        {
            return Ok(false);
        }
        git(worktree, &["add", "--all"]).await?;
        git(worktree, &["commit", "--quiet", "-m", message]).await?;
        Ok(true)
    }

    pub async fn push(&self, worktree: &Path, target: &Target, branch: &str) -> Result<()> {
        let refspec = format!("HEAD:refs/heads/{branch}");
        git(
            worktree,
            &["push", "--quiet", push_remote(target), &refspec],
        )
        .await?;
        Ok(())
    }

    /// Commit subjects and a diff summary against `base_ref`.
    pub async fn summary(&self, worktree: &Path, base_ref: &str) -> Result<(String, String)> {
        let range = format!("{base_ref}..HEAD");
        let log = git(worktree, &["log", "--format=- %s", &range]).await?;
        let stat = git(worktree, &["diff", "--stat", &format!("{base_ref}...HEAD")]).await?;
        Ok((log, stat))
    }

    pub async fn remove(&self, repo: &str, worktree: &Path) -> Result<()> {
        let path = worktree.to_string_lossy().into_owned();
        git(
            &self.mirror(repo),
            &["worktree", "remove", "--force", &path],
        )
        .await?;
        Ok(())
    }
}

/// Check out `branch` at `path`, creating the branch from `base_ref` if it
/// does not exist. An existing branch keeps its commits, for example after
/// the worktree of a blocked ticket was cleaned up.
async fn add_worktree(mirror: &Path, path: &Path, branch: &str, base_ref: &str) -> Result<()> {
    let dest = path.to_string_lossy().into_owned();
    let local = format!("refs/heads/{branch}");
    git(mirror, &["worktree", "prune"]).await?;
    if git(mirror, &["rev-parse", "--verify", "--quiet", &local])
        .await
        .is_ok()
    {
        git(mirror, &["worktree", "add", "--quiet", &dest, branch]).await?;
    } else {
        git(
            mirror,
            &["worktree", "add", "--quiet", "-b", branch, &dest, base_ref],
        )
        .await?;
    }
    Ok(())
}

/// Give commits an author when the machine has no git identity configured.
async fn ensure_identity(mirror: &Path) -> Result<()> {
    if git(mirror, &["config", "user.email"]).await.is_err() {
        git(mirror, &["config", "user.name", "Amaru Light"]).await?;
        let email = "amaru-light@users.noreply.github.com";
        git(mirror, &["config", "user.email", email]).await?;
    }
    Ok(())
}

/// Keep the agent's result file out of commits in every worktree.
async fn exclude_amaru(mirror: &Path) -> Result<()> {
    let file = mirror.join(".git/info/exclude");
    let current = tokio::fs::read_to_string(&file).await.unwrap_or_default();
    if !current.lines().any(|l| l.trim() == ".amaru/") {
        if let Some(dir) = file.parent() {
            tokio::fs::create_dir_all(dir).await?;
        }
        tokio::fs::write(&file, format!("{current}\n.amaru/\n")).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
