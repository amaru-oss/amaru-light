use anyhow::{Context, Result, bail};
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Capture both pipes concurrently so verbose commands cannot deadlock. No shell is involved.
pub fn command_output(command: &mut Command, timeout: u64) -> Result<Output> {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting command")?;
    let mut stdout = child.stdout.take().context("stdout pipe")?;
    let mut stderr = child.stderr.take().context("stderr pipe")?;
    let out = thread::spawn(move || {
        let mut b = Vec::new();
        stdout.read_to_end(&mut b).map(|_| b)
    });
    let err = thread::spawn(move || {
        let mut b = Vec::new();
        stderr.read_to_end(&mut b).map(|_| b)
    });
    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= Duration::from_secs(timeout) {
            #[cfg(unix)]
            // The child leads its own process group; terminate check descendants as well.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            let _ = out.join();
            let _ = err.join();
            bail!("command timed out after {timeout}s");
        }
        thread::sleep(Duration::from_millis(20));
    };
    Ok(Output {
        status,
        stdout: out
            .join()
            .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??,
        stderr: err
            .join()
            .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??,
    })
}
pub fn slugify(text: &str, max_len: usize) -> String {
    let mut s = String::new();
    let mut dash = false;
    for c in text.to_ascii_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            if dash && !s.is_empty() {
                s.push('-');
            }
            s.push(c);
            dash = false;
        } else {
            dash = true;
        }
    }
    s.truncate(s.len().min(max_len));
    while s.ends_with('-') {
        s.pop();
    }
    if s.is_empty() { "work".into() } else { s }
}
#[derive(Clone)]
pub struct Git {
    pub repo: PathBuf,
}
impl Git {
    pub fn new(repo: &Path) -> Self {
        Self { repo: repo.into() }
    }
    pub fn run(&self, args: &[&str], cwd: Option<&Path>, check: bool) -> Result<Output> {
        let output = command_output(
            Command::new("git")
                .arg("-C")
                .arg(cwd.unwrap_or(&self.repo))
                .args(args),
            300,
        )?;
        if check && !output.status.success() {
            bail!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(output)
    }
    pub fn out(&self, args: &[&str], cwd: Option<&Path>) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.run(args, cwd, true)?.stdout)
            .trim()
            .into())
    }
    pub fn has_remote(&self) -> Result<bool> {
        Ok(self.out(&["remote"], None)?.lines().any(|s| s == "origin"))
    }
    pub fn fetch(&self) -> Result<()> {
        self.run(&["fetch", "origin", "--prune"], None, true)?;
        Ok(())
    }
    pub fn remote_branches(&self, pattern: &str) -> Result<Vec<String>> {
        self.remote_branches_at("origin", pattern)
    }
    pub fn remote_branches_at(&self, remote: &str, pattern: &str) -> Result<Vec<String>> {
        Ok(self
            .out(&["ls-remote", "--heads", remote, pattern], None)?
            .lines()
            .filter_map(|s| s.split_once("refs/heads/").map(|(_, s)| s.to_owned()))
            .collect())
    }
    pub fn current_branch(&self, cwd: &Path) -> Result<String> {
        let o = self.run(&["symbolic-ref", "--short", "-q", "HEAD"], Some(cwd), false)?;
        if o.status.success() {
            Ok(String::from_utf8_lossy(&o.stdout).trim().into())
        } else {
            self.out(&["rev-parse", "--short", "HEAD"], Some(cwd))
        }
    }
    pub fn is_clean(&self, cwd: &Path) -> Result<bool> {
        Ok(self.out(&["status", "--porcelain"], Some(cwd))?.is_empty())
    }
    pub fn push(&self, branch: &str, cwd: &Path) -> Result<()> {
        self.push_branch(&["push", "-u", "origin"], branch, cwd)
    }
    pub fn push_to_fork(&self, fork_url: &str, branch: &str, cwd: &Path) -> Result<()> {
        self.push_branch(&["push", fork_url], branch, cwd)
    }
    fn push_branch(&self, command: &[&str], branch: &str, cwd: &Path) -> Result<()> {
        let refs = format!("refs/heads/{branch}:refs/heads/{branch}");
        let mut args = command.to_vec();
        args.push(&refs);
        self.run(&args, Some(cwd), true)?;
        Ok(())
    }
    pub fn has_local_branch(&self, branch: &str) -> Result<bool> {
        Ok(self
            .run(
                &[
                    "show-ref",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
                None,
                false,
            )?
            .status
            .success())
    }
    pub fn create_remote_branch(&self, branch: &str, from: &str) -> Result<()> {
        self.run(
            &["push", "origin", &format!("{from}:refs/heads/{branch}")],
            None,
            true,
        )?;
        Ok(())
    }
    pub fn add_worktree(&self, path: &Path, branch: &str, base: &str) -> Result<()> {
        if path.exists() {
            if self.current_branch(path)? != branch {
                bail!("worktree exists on another branch");
            }
            return Ok(());
        }
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let path = path.to_str().context("non-UTF8 worktree")?;
        if self.has_local_branch(branch)? {
            self.run(&["worktree", "add", path, branch], None, true)?;
        } else {
            let remote = format!("origin/{branch}");
            let from = if self
                .run(
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("refs/remotes/{remote}"),
                    ],
                    None,
                    false,
                )?
                .status
                .success()
            {
                &remote
            } else {
                base
            };
            self.run(&["worktree", "add", "-b", branch, path, from], None, true)?;
        }
        Ok(())
    }
    pub fn remove_worktree(&self, path: &Path) -> Result<bool> {
        if !path.exists() {
            return Ok(true);
        }
        if !self.is_clean(path)? {
            return Ok(false);
        }
        self.run(
            &[
                "worktree",
                "remove",
                path.to_str().context("non-UTF8 path")?,
            ],
            None,
            true,
        )?;
        Ok(true)
    }
    pub fn ahead_count(&self, base: &str, cwd: &Path) -> Result<u64> {
        Ok(self
            .out(
                &["rev-list", "--count", &format!("{base}..HEAD")],
                Some(cwd),
            )?
            .parse()?)
    }
    pub fn commits(&self, base: &str, cwd: &Path) -> Result<String> {
        self.out(
            &["log", "--format=%h %s", &format!("{base}..HEAD")],
            Some(cwd),
        )
    }
    pub fn diff_stat(&self, base: &str, cwd: &Path) -> Result<String> {
        self.out(&["diff", "--stat", &format!("{base}...HEAD")], Some(cwd))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn slugs() {
        assert_eq!(slugify("Fix: hello / WORLD!", 40), "fix-hello-world");
        assert_eq!(slugify("!!!", 40), "work");
        assert_eq!(slugify("hello-world", 6), "hello");
    }
}
