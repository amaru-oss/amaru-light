//! Repository mirrors and one git worktree per ticket. Every clone has two
//! remotes, set by the daemon and never by the agent: `origin`, where branches
//! are pushed (the bot's fork, or the repository itself), and `upstream`, the
//! repository pull requests go to and ticket branches start from.

use crate::{forge::Target, process};
use anyhow::{Context, Result, bail};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

const GIT_TIMEOUT: Duration = Duration::from_secs(600);

/// The remote branches are pushed to: the bot's fork, or the repository
/// itself when there is no fork.
pub const PUSH_REMOTE: &str = "origin";
/// The remote pull requests go to, which ticket branches start from.
pub const UPSTREAM_REMOTE: &str = "upstream";
/// The fork remote of clones made before `origin` was the fork.
const OLD_FORK_REMOTE: &str = "fork";

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

    /// The remotes every clone and worktree has, and their URLs.
    fn remotes(&self, target: &Target) -> [(&'static str, String); 2] {
        [
            (PUSH_REMOTE, self.url(target.push_repo())),
            (UPSTREAM_REMOTE, self.url(&target.upstream)),
        ]
    }

    /// Clone the repository if needed, set its remotes and fetch upstream.
    pub async fn sync(&self, target: &Target) -> Result<PathBuf> {
        let mirror = self.mirror(&target.upstream);
        if !mirror.join(".git").is_dir() {
            let parent = mirror.parent().context("mirror has no parent")?;
            tokio::fs::create_dir_all(parent).await?;
            let dest = mirror.to_string_lossy().into_owned();
            let url = self.url(&target.upstream);
            git(
                parent,
                &["clone", "--quiet", "--origin", UPSTREAM_REMOTE, &url, &dest],
            )
            .await?;
        }
        self.set_remotes(&mirror, target).await?;
        git(&mirror, &["fetch", "--quiet", "--prune", UPSTREAM_REMOTE]).await?;
        ensure_identity(&mirror).await?;
        Ok(mirror)
    }

    /// Make sure the clone, and `worktree` when given, push to `origin` (the
    /// fork, or the repository without one) and take their base from
    /// `upstream`, whatever changed them since. Idempotent: it changes
    /// nothing on a clone that is already right.
    pub async fn ensure_remotes(&self, target: &Target, worktree: Option<&Path>) -> Result<()> {
        let mirror = self.mirror(&target.upstream);
        if self.set_remotes(&mirror, target).await? {
            // A clone from before `upstream` existed has no upstream refs
            // for the ticket's base yet.
            git(&mirror, &["fetch", "--quiet", "--prune", UPSTREAM_REMOTE]).await?;
        }
        for dir in std::iter::once(mirror.as_path()).chain(worktree) {
            for (name, url) in self.remotes(target) {
                let fetch = remote_url(dir, name, false).await;
                let push = remote_url(dir, name, true).await;
                if fetch.as_deref() != Some(url.as_str()) || push.as_deref() != Some(url.as_str()) {
                    bail!(
                        "{}: remote {name} is {} (push {}), not {url}",
                        dir.display(),
                        fetch.as_deref().unwrap_or("missing"),
                        push.as_deref().unwrap_or("missing"),
                    );
                }
            }
        }
        Ok(())
    }

    /// Set `origin` and `upstream` on the clone; true when anything changed.
    async fn set_remotes(&self, mirror: &Path, target: &Target) -> Result<bool> {
        let mut changed = migrate_remotes(mirror).await?;
        for (name, url) in self.remotes(target) {
            match remote_url(mirror, name, false).await {
                Some(current) if current == url => {}
                Some(_) => {
                    git(mirror, &["remote", "set-url", name, &url]).await?;
                    changed = true;
                }
                None => {
                    git(mirror, &["remote", "add", name, &url]).await?;
                    changed = true;
                }
            }
            if remote_url(mirror, name, true).await.as_deref() != Some(url.as_str()) {
                // A push URL of its own would send pushes elsewhere.
                let key = format!("remote.{name}.pushurl");
                let _ = git(mirror, &["config", "--unset-all", &key]).await;
                changed = true;
            }
        }
        Ok(changed)
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

    /// Push the worktree's HEAD to `branch` on `origin`, after making sure
    /// `origin` is still the push repository of `target`.
    pub async fn push(&self, worktree: &Path, target: &Target, branch: &str) -> Result<()> {
        self.ensure_remotes(target, Some(worktree)).await?;
        let refspec = format!("HEAD:refs/heads/{branch}");
        git(worktree, &["push", "--quiet", PUSH_REMOTE, &refspec]).await?;
        Ok(())
    }

    /// Commit subjects and a diff summary against `base_ref`.
    pub async fn summary(&self, worktree: &Path, base_ref: &str) -> Result<(String, String)> {
        let range = format!("{base_ref}..HEAD");
        let log = git(worktree, &["log", "--format=- %s", &range]).await?;
        let stat = git(worktree, &["diff", "--stat", &format!("{base_ref}...HEAD")]).await?;
        Ok((log, stat))
    }

    /// Delete a ticket's worktree with everything in it, build output
    /// included, and make the clone forget it. The branch stays. A worktree
    /// that is already gone, or that git no longer knows, is not an error.
    pub async fn remove(&self, repo: &str, worktree: &Path) -> Result<()> {
        let climbs = worktree
            .components()
            .any(|c| c == std::path::Component::ParentDir);
        if climbs
            || !worktree.starts_with(&self.root)
            || worktree.components().count() <= self.depth()
        {
            bail!("{} is not a worktree of this daemon", worktree.display());
        }
        let mirror = self.mirror(repo);
        let known = mirror.join(".git").is_dir();
        if known {
            let path = worktree.to_string_lossy().into_owned();
            // Twice --force also removes a locked worktree. When git cannot
            // remove it (it may not know it), the files go below.
            let args = ["worktree", "remove", "--force", "--force", &path];
            let _ = git(&mirror, &args).await;
        }
        match tokio::fs::remove_dir_all(worktree).await {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("cannot delete {}", worktree.display()));
            }
            _ => {}
        }
        if known {
            git(&mirror, &["worktree", "prune"]).await?;
        }
        Ok(())
    }

    /// Path components of a worktree's parent, `<root>/<owner>/<name>/worktrees`.
    fn depth(&self) -> usize {
        self.root.components().count() + 3
    }

    /// Every worktree directory on disk, with its repository `owner/name`,
    /// whether a ticket still uses it or not.
    pub fn worktrees(&self) -> Result<Vec<(String, PathBuf)>> {
        let mut found = Vec::new();
        for owner in subdirs(&self.root)? {
            for name in subdirs(&owner)? {
                let (Some(o), Some(n)) = (owner.file_name(), name.file_name()) else {
                    continue;
                };
                let repo = format!("{}/{}", o.to_string_lossy(), n.to_string_lossy());
                for dir in subdirs(&name.join("worktrees"))? {
                    found.push((repo.clone(), dir));
                }
            }
        }
        found.sort();
        Ok(found)
    }

    /// Where the worktree of `branch` is, used or not.
    pub fn path_of(&self, repo: &str, branch: &str) -> PathBuf {
        self.worktree_path(repo, branch)
    }
}

/// The directories in `dir`, none when it does not exist.
fn subdirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot list {}", dir.display())),
    };
    let mut dirs = Vec::new();
    for entry in entries {
        let entry = entry?;
        // A symbolic link is never followed: only real directories count.
        if entry.file_type()?.is_dir() {
            dirs.push(entry.path());
        }
    }
    Ok(dirs)
}

/// Bytes the files under `path` take, not following symbolic links.
pub fn disk_usage(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).map_or(0, |entries| {
        entries
            .filter_map(std::result::Result::ok)
            .map(|e| disk_usage(&e.path()))
            .sum()
    })
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

/// The fetch or push URL of a remote, or `None` when there is no such remote.
async fn remote_url(dir: &Path, name: &str, push: bool) -> Option<String> {
    let args: &[&str] = if push {
        &["remote", "get-url", "--push", name]
    } else {
        &["remote", "get-url", name]
    };
    git(dir, args).await.ok().map(|url| url.trim().to_owned())
}

/// Rename the remotes of a clone made when `origin` was the upstream
/// repository and `fork` the fork: the upstream becomes `upstream`, keeping
/// its fetched refs, and the fork becomes `origin`. True when it renamed.
async fn migrate_remotes(mirror: &Path) -> Result<bool> {
    let names = git(mirror, &["remote"]).await?;
    let has = |name: &str| names.lines().any(|l| l.trim() == name);
    if !has(OLD_FORK_REMOTE) {
        return Ok(false);
    }
    if has(UPSTREAM_REMOTE) {
        git(mirror, &["remote", "remove", OLD_FORK_REMOTE]).await?;
    } else {
        if has(PUSH_REMOTE) {
            git(mirror, &["remote", "rename", PUSH_REMOTE, UPSTREAM_REMOTE]).await?;
        }
        git(mirror, &["remote", "rename", OLD_FORK_REMOTE, PUSH_REMOTE]).await?;
    }
    Ok(true)
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
