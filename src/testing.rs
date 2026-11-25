//! Test doubles shared by the lifecycle and scheduler tests: an in-memory
//! forge, a scripted agent CLI and local git remotes.

use crate::{
    db::Store,
    forge::{Feedback, Forge, Issue, NewPullRequest, PullRequest, Selector, Target, TicketSource},
    lifecycle::Services,
    process,
    prompts::Prompts,
    settings::Settings,
    ticket::{Origin, Ticket},
    worktree::Workspace,
};
use anyhow::Result;
use async_trait::async_trait;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

/// Answers like Claude Code and acts on the kind of prompt it receives.
const AGENT: &str = r#"#!/bin/sh
prompt=$(cat)
dir=$(dirname "$0")
n=$(ls "$dir"/prompt-* 2>/dev/null | wc -l)
printf '%s' "$prompt" > "$dir/prompt-$n"
echo "$*" > "$dir/args-$n"
c() { git -c user.name=agent -c user.email=agent@example.com "$@"; }
mkdir -p .amaru
case "$prompt" in
  *"You are working on issue"*) echo done >> feature.txt; c add feature.txt; c commit -qm feature
    echo '{"status":"done","summary":"Added the feature","complete":true}' > .amaru/result.json ;;
  *"project check"*) touch fixed.txt; c add fixed.txt; c commit -qm fix
    echo '{"status":"done","summary":"Fixed"}' > .amaru/result.json ;;
  *"received this feedback"*) echo renamed > feature.txt; c commit -qam rename
    echo '{"status":"done","summary":"Renamed","replies":[{"thread_id":"line-7","body":"renamed"}]}' > .amaru/result.json ;;
esac
echo '{"type":"system","subtype":"init","session_id":"sess-1"}'
echo '{"type":"result","is_error":false,"result":"ok","session_id":"sess-1"}'
"#;

#[derive(Debug, Default)]
pub struct ForgeState {
    pub comments: Vec<(u64, String)>,
    pub labels: Vec<(u64, String)>,
    pub prs: Vec<NewPullRequest>,
    pub feedback: Vec<Feedback>,
    pub replies: Vec<(String, String)>,
    pub merged: bool,
    /// Refuse to label issues, as for a bot without write access.
    pub deny_labels: bool,
    /// Issues the forge knows; unknown numbers are open, plain issues.
    pub issues: Vec<Issue>,
}

#[derive(Debug, Default)]
pub struct FakeForge(Mutex<ForgeState>);

impl FakeForge {
    pub fn state(&self) -> MutexGuard<'_, ForgeState> {
        self.0.lock().unwrap()
    }
    pub fn add_feedback(&self, f: Feedback) {
        self.state().feedback.push(f);
    }
    pub fn merge(&self) {
        self.state().merged = true;
    }
    pub fn set_blocked_by(&self, number: u64, by: Vec<u64>) {
        self.update_issue(number, |i| i.blocked_by = by);
    }
    pub fn update_issue(&self, number: u64, change: impl FnOnce(&mut Issue)) {
        let mut issue = self.lookup(number);
        change(&mut issue);
        let mut state = self.state();
        state.issues.retain(|i| i.number != number);
        state.issues.push(issue);
    }
    fn lookup(&self, number: u64) -> Issue {
        let known = self
            .state()
            .issues
            .iter()
            .find(|i| i.number == number)
            .cloned();
        known.unwrap_or_else(|| Issue {
            number,
            title: "Add a feature".into(),
            body: "Please add it".into(),
            open: true,
            ..Issue::default()
        })
    }
    fn pr_for(&self, index: usize) -> PullRequest {
        let merged = self.state().merged;
        PullRequest {
            number: 100 + index as u64,
            url: format!("https://example.com/pr/{}", 100 + index),
            open: !merged,
            merged,
            head_sha: "abc".into(),
        }
    }
}

#[async_trait]
impl TicketSource for FakeForge {
    async fn eligible(&self, _: &str, _: &Selector) -> Result<Vec<Issue>> {
        Ok(self
            .state()
            .issues
            .iter()
            .filter(|i| i.open)
            .cloned()
            .collect())
    }
    async fn milestone(&self, _: &str, milestone: &str) -> Result<Vec<Issue>> {
        let issues = self.state().issues.clone();
        Ok(issues
            .into_iter()
            .filter(|i| i.open && i.milestone.as_deref() == Some(milestone))
            .collect())
    }
    async fn issue(&self, _: &str, number: u64) -> Result<Issue> {
        let mut issue = self.lookup(number);
        let labels = self.state().labels.clone();
        issue.labels.extend(
            labels
                .into_iter()
                .filter(|(n, _)| *n == number)
                .map(|(_, l)| l),
        );
        Ok(issue)
    }
    async fn comment(&self, _: &str, number: u64, body: &str) -> Result<()> {
        self.state().comments.push((number, body.into()));
        Ok(())
    }
    async fn add_label(&self, _: &str, number: u64, label: &str) -> Result<()> {
        let mut state = self.state();
        anyhow::ensure!(!state.deny_labels, "Must have admin rights to Repository.");
        state.labels.push((number, label.into()));
        Ok(())
    }
    async fn remove_label(&self, _: &str, number: u64, label: &str) -> Result<()> {
        self.state()
            .labels
            .retain(|l| *l != (number, label.to_owned()));
        Ok(())
    }
}

#[async_trait]
impl Forge for FakeForge {
    async fn find_pr(&self, _: &Target, _: &str) -> Result<Option<PullRequest>> {
        Ok(None)
    }
    async fn open_pr(&self, _: &Target, pr: &NewPullRequest) -> Result<PullRequest> {
        let index = self.state().prs.len();
        self.state().prs.push(pr.clone());
        Ok(self.pr_for(index))
    }
    async fn pr(&self, _: &str, number: u64) -> Result<PullRequest> {
        Ok(self.pr_for(usize::try_from(number - 100)?))
    }
    async fn feedback(&self, _: &str, _: &PullRequest) -> Result<Vec<Feedback>> {
        Ok(self.state().feedback.clone())
    }
    async fn reply(&self, _: &str, _: u64, to: &Feedback, body: &str) -> Result<()> {
        self.state().replies.push((to.id.clone(), body.into()));
        Ok(())
    }
}

pub struct Fake {
    pub dir: tempfile::TempDir,
    pub forge: Arc<FakeForge>,
    pub svc: Arc<Services>,
}

async fn git(cwd: &Path, args: &[&str]) {
    process::capture("git", args, cwd, Duration::from_secs(60))
        .await
        .unwrap();
}

async fn upstream(root: &Path) {
    let seed = seed(root).await;
    let bare = root.join("remotes/me/app.git");
    std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
    let (seed, bare) = (seed.to_string_lossy(), bare.to_string_lossy());
    git(root, &["clone", "-q", "--bare", &seed, &bare]).await;
}

async fn seed(root: &Path) -> PathBuf {
    let seed = root.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&seed, &["init", "-q", "-b", "main"]).await;
    std::fs::write(seed.join("README"), "hi\n").unwrap();
    git(&seed, &["add", "README"]).await;
    git(
        &seed,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ],
    )
    .await;
    seed
}

fn write_settings(root: &Path, check: &str) -> PathBuf {
    let agent = root.join("agent/claude");
    std::fs::create_dir_all(agent.parent().unwrap()).unwrap();
    std::fs::write(&agent, AGENT).unwrap();
    std::fs::set_permissions(&agent, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let checks = if check.is_empty() {
        String::new()
    } else {
        format!("checks:\n  - {{name: check, run: \"{check}\"}}\n")
    };
    std::fs::write(root.join("project.yaml"), format!(
        "repo: me/app\nagent: claude\nintake: {{labels: [amaru]}}\nreview: {{reviewers: [me]}}\n{checks}"
    )).unwrap();
    let daemon = format!(
        "state_dir: state\npoll_interval: 1s\nagents:\n  claude: {{driver: claude, command: \"{}\"}}\nprojects: [project.yaml]\n",
        agent.display()
    );
    std::fs::write(root.join("daemon.yaml"), daemon).unwrap();
    root.join("daemon.yaml")
}

impl Fake {
    pub async fn new(check: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        upstream(dir.path()).await;
        let settings = Settings::load(&write_settings(dir.path(), check)).unwrap();
        let forge = Arc::new(FakeForge::default());
        let url_base = format!("file://{}/remotes/", dir.path().display());
        let svc = Arc::new(Services {
            store: Arc::new(Store::open(&settings.state_dir().join("amaru.db")).unwrap()),
            workspace: Workspace::new(&settings.state_dir()).with_url_base(&url_base),
            settings: Arc::new(settings),
            source: forge.clone(),
            forge: forge.clone(),
            prompts: Prompts::default(),
        });
        Self { dir, forge, svc }
    }

    pub async fn claim(&self, number: u64) -> Ticket {
        self.svc
            .store
            .claim("me/app", number, "Add a feature", Origin::Manual)
            .unwrap()
            .unwrap()
    }

    fn agent_files(&self, prefix: &str) -> Vec<String> {
        (0..)
            .map(|n| self.dir.path().join(format!("agent/{prefix}-{n}")))
            .take_while(|p| p.exists())
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect()
    }

    pub fn prompts(&self) -> Vec<String> {
        self.agent_files("prompt")
    }

    pub fn resumed_every_turn_after_the_first(&self) -> bool {
        let args = self.agent_files("args");
        !args[0].contains("--resume") && args[1..].iter().all(|a| a.contains("--resume sess-1"))
    }

    pub async fn upstream_has(&self, branch: &str) -> bool {
        let bare = self.dir.path().join("remotes/me/app.git");
        let out = process::capture(
            "git",
            &["branch", "--list", branch],
            &bare,
            Duration::from_secs(60),
        )
        .await
        .unwrap();
        out.contains(branch)
    }
}
