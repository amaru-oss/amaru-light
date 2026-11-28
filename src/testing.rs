//! Test doubles shared by the lifecycle and scheduler tests: an in-memory
//! forge, a scripted agent CLI and local git remotes.

use crate::{
    db::Store,
    forge::{
        Feedback, Forge, ForkError, Issue, NewPullRequest, PullRequest, Reaction, Selector, Target,
        TicketSource,
    },
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
dir=$(dirname "$0")
if [ "$1 $2" = "auth status" ]; then
  if [ -f "$dir/signed-out" ]; then echo "Not logged in"; exit 1; fi
  echo "Logged in"; exit 0
fi
prompt=$(cat)
n=$(ls "$dir"/prompt-* 2>/dev/null | wc -l)
printf '%s' "$prompt" > "$dir/prompt-$n"
echo "$*" > "$dir/args-$n"
if [ -f "$dir/sign-out-now" ]; then
  rm "$dir/sign-out-now"; touch "$dir/signed-out"
  echo "Not logged in" >&2; exit 1
fi
if [ -f "$dir/stall" ]; then
  rm "$dir/stall"
  echo '{"type":"system","subtype":"init","session_id":"sess-1"}'
  sleep 30
fi
c() { git -c user.name=agent -c user.email=agent@example.com "$@"; }
if [ -f "$dir/tamper" ]; then
  git remote set-url origin "file://$dir/nowhere.git"
  git remote set-url --push upstream "file://$dir/nowhere.git"
fi
mkdir -p .amaru
case "$prompt" in
  *"stepped in on your work"*) touch guided.txt; c add guided.txt; c commit -qm guided
    echo '{"status":"done","summary":"Followed the guidance"}' > .amaru/result.json ;;
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
pub struct Denied {
    /// Labelling issues.
    pub labels: bool,
    /// Reacting to reviews and comments.
    pub reactions: bool,
}

#[derive(Debug, Default)]
pub struct ForgeState {
    pub comments: Vec<(u64, String)>,
    pub labels: Vec<(u64, String)>,
    pub prs: Vec<NewPullRequest>,
    pub feedback: Vec<Feedback>,
    pub replies: Vec<(String, String)>,
    /// Feedback ids reacted to, in order.
    pub reactions: Vec<String>,
    pub merged: bool,
    /// Refuse the fork, as when it is not a fork of the upstream repository.
    pub fork_unusable: bool,
    /// Calls the forge refuses, as for a bot without write access.
    pub denied: Denied,
    /// Issues the forge knows; unknown numbers are open, plain issues.
    pub issues: Vec<Issue>,
    /// Where the bare repositories live, so that a missing fork can be made.
    pub remotes: Option<PathBuf>,
    /// Forks made by `ensure_fork`.
    pub forks_created: Vec<String>,
    /// Calls to `ensure_fork`.
    pub fork_checks: usize,
    /// The `head` of each opened pull request.
    pub heads: Vec<String>,
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
        anyhow::ensure!(
            !state.denied.labels,
            "Must have admin rights to Repository."
        );
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
    async fn find_pr(&self, _: &Target, branch: &str) -> Result<Option<PullRequest>> {
        let index = self.state().prs.iter().position(|p| p.branch == branch);
        Ok(index.map(|i| self.pr_for(i)))
    }
    /// Like GitHub, make the fork as a copy of the upstream repository when
    /// it is missing.
    async fn ensure_fork(&self, target: &Target) -> Result<()> {
        let remotes = {
            let mut state = self.state();
            state.fork_checks += 1;
            if target.fork.is_some() && state.fork_unusable {
                return Err(ForkError("bot/app exists but is not a fork".into()).into());
            }
            state.remotes.clone()
        };
        let (Some(fork), Some(remotes)) = (&target.fork, remotes) else {
            return Ok(());
        };
        let dest = remotes.join(format!("{fork}.git"));
        if !dest.exists() {
            let source = remotes.join(format!("{}.git", target.upstream));
            std::fs::create_dir_all(dest.parent().unwrap_or(&remotes))?;
            let (source, dest) = (source.to_string_lossy(), dest.to_string_lossy());
            process::capture(
                "git",
                &["clone", "-q", "--bare", &source, &dest],
                &remotes,
                Duration::from_secs(60),
            )
            .await?;
            self.state().forks_created.push(fork.clone());
        }
        Ok(())
    }
    async fn open_pr(&self, target: &Target, pr: &NewPullRequest) -> Result<PullRequest> {
        let index = self.state().prs.len();
        self.state().prs.push(pr.clone());
        self.state().heads.push(target.head(&pr.branch));
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
    async fn react(&self, to: &Feedback, _: Reaction) -> Result<()> {
        let mut state = self.state();
        anyhow::ensure!(
            !state.denied.reactions,
            "Resource not accessible by integration"
        );
        state.reactions.push(to.id.clone());
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

fn write_settings(root: &Path, check: &str, extra: &str, daemon_extra: &str) -> PathBuf {
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
        "repo: me/app\nagent: claude\nintake: {{labels: [amaru]}}\nreview: {{reviewers: [me]}}\n{checks}{extra}"
    )).unwrap();
    let daemon = format!(
        "state_dir: state\npoll_interval: 1s\nagents:\n  claude: {{driver: claude, command: \"{}\"}}\nprojects: [project.yaml]\n{daemon_extra}",
        agent.display()
    );
    std::fs::write(root.join("daemon.yaml"), daemon).unwrap();
    root.join("daemon.yaml")
}

impl Fake {
    pub async fn new(check: &str) -> Self {
        Self::with_project(check, "").await
    }

    /// A project whose tickets come from the Jira project `APP`; the fake
    /// forge still answers for its issues.
    pub async fn jira() -> Self {
        Self::with_project(
            "",
            "jira: {url: \"https://jira.example.com\", project: APP}\n",
        )
        .await
    }

    pub async fn with_project(check: &str, extra: &str) -> Self {
        Self::with_files(check, extra, "").await
    }

    /// A project that follows the workflow `custom`, made of `steps`.
    pub async fn with_workflow(check: &str, steps: &str) -> Self {
        let daemon = format!("workflows:\n  custom: {steps}\n");
        Self::with_files(check, "workflow: custom\n", &daemon).await
    }

    async fn with_files(check: &str, extra: &str, daemon_extra: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        upstream(dir.path()).await;
        let path = write_settings(dir.path(), check, extra, daemon_extra);
        let settings = Settings::load(&path).unwrap();
        let forge = Arc::new(FakeForge::default());
        forge.state().remotes = Some(dir.path().join("remotes"));
        let url_base = format!("file://{}/remotes/", dir.path().display());
        let svc = Arc::new(Services {
            store: Arc::new(Store::open(&settings.state_dir().join("amaru.db")).unwrap()),
            workspace: Workspace::new(&settings.state_dir()).with_url_base(&url_base),
            settings: Arc::new(settings),
            source: forge.clone(),
            forge: forge.clone(),
            prompts: Prompts::default(),
            sign_ins: crate::login::SignIns::default(),
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

    /// Run a ticket until it waits for review, a human, or is done.
    pub async fn drive(&self, ticket: Ticket) -> Ticket {
        let mut job = crate::lifecycle::Job::new(&self.svc, ticket, None).unwrap();
        job.drive().await.unwrap();
        job.into_ticket()
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
        self.repo_has("me/app", branch).await
    }

    /// Whether the bare repository `owner/name` has `branch`.
    pub async fn repo_has(&self, repo: &str, branch: &str) -> bool {
        let bare = self.dir.path().join(format!("remotes/{repo}.git"));
        if !bare.exists() {
            return false;
        }
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
