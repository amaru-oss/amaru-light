//! Typed configuration for the v2 daemon.
//!
//! Two kinds of YAML files exist: one daemon file per machine and one
//! project file per repository. Both reject unknown keys, so a typo is an
//! error instead of a silent default.

use crate::forge::Reaction;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

/// A duration written as `30s`, `45m`, `2h` or `7d`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span(pub Duration);

impl Span {
    pub const fn secs(secs: u64) -> Self {
        Self(Duration::from_secs(secs))
    }
}

fn parse_span(text: &str) -> Result<Duration> {
    let text = text.trim();
    if text == "0" {
        return Ok(Duration::ZERO);
    }
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (digits, unit) = text.split_at(split);
    let value: u64 = digits
        .parse()
        .with_context(|| format!("invalid duration '{text}'"))?;
    let factor = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => bail!("invalid duration unit in '{text}': use s, m, h or d"),
    };
    Ok(Duration::from_secs(value.saturating_mul(factor)))
}

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let text = String::deserialize(de)?;
        parse_span(&text)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for Span {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}s", self.0.as_secs())
    }
}

/// How long the worktree of a blocked ticket is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retention {
    For(Duration),
    Forever,
}

impl<'de> Deserialize<'de> for Retention {
    fn deserialize<D: serde::Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        let text = String::deserialize(de)?;
        if text.trim() == "forever" {
            return Ok(Self::Forever);
        }
        parse_span(&text)
            .map(Self::For)
            .map_err(serde::de::Error::custom)
    }
}

/// One agent CLI the daemon can drive.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSettings {
    /// Which driver understands this CLI's output.
    pub driver: DriverKind,
    /// The executable, for example `claude`.
    pub command: String,
    /// Extra arguments, for example the permission mode.
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Extra environment variables for the agent process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Arguments that sign the CLI in, run from the web page. Each driver
    /// has a default; an empty list turns signing in from the page off.
    #[serde(default)]
    pub login: Option<Vec<String>>,
    /// Arguments that sign the CLI in with an API key read from stdin, for
    /// example `[login, --with-api-key]` for Codex. The page sends the key,
    /// then closes stdin. An empty list turns this off.
    #[serde(default)]
    pub login_with_key: Option<Vec<String>>,
    /// Arguments that tell whether the CLI is signed in: exit code 0 means
    /// it is. Each driver has a default.
    #[serde(default)]
    pub login_status: Option<Vec<String>>,
}

impl AgentSettings {
    /// The arguments that sign the CLI in.
    pub fn login_args(&self) -> Vec<String> {
        self.login.clone().unwrap_or_else(|| match self.driver {
            DriverKind::Claude => strings(&["auth", "login"]),
            DriverKind::Codex => strings(&["login", "--device-auth"]),
        })
    }

    /// The arguments that sign the CLI in with an API key on stdin. Claude
    /// Code reads its key from `ANTHROPIC_API_KEY` instead, so it has none.
    pub fn key_login_args(&self) -> Vec<String> {
        self.login_with_key
            .clone()
            .unwrap_or_else(|| match self.driver {
                DriverKind::Claude => Vec::new(),
                DriverKind::Codex => strings(&["login", "--with-api-key"]),
            })
    }

    /// The arguments that report whether the CLI is signed in.
    pub fn status_args(&self) -> Vec<String> {
        self.login_status
            .clone()
            .unwrap_or_else(|| match self.driver {
                DriverKind::Claude => strings(&["auth", "status", "--text"]),
                DriverKind::Codex => strings(&["login", "status"]),
            })
    }
}

fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    Claude,
    Codex,
}

/// Machine-wide settings.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DaemonSettings {
    /// Database, mirrors and worktrees live here.
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default = "default_poll")]
    pub poll_interval: Span,
    /// Tickets in progress at once, never more than 2.
    #[serde(default = "default_parallel")]
    pub max_parallel: usize,
    /// The GitHub CLI; it reads the bot token from `GH_TOKEN`.
    #[serde(default = "default_gh")]
    pub gh: String,
    /// The bot account, used by the assignee selector.
    #[serde(default)]
    pub bot_login: Option<String>,
    pub agents: BTreeMap<String, AgentSettings>,
    /// Project files, relative to this file.
    pub projects: Vec<PathBuf>,
    /// The web interface that `amaru serve` starts.
    #[serde(default)]
    pub web: WebSettings,
}

/// The web interface and its HTTP API.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebSettings {
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Address and port to listen on. Only this machine can connect to the
    /// default; a container needs `0.0.0.0:8765` and a port mapping.
    #[serde(default = "default_listen")]
    pub listen: String,
}

impl Default for WebSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            listen: default_listen(),
        }
    }
}

fn default_listen() -> String {
    "127.0.0.1:8765".into()
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("data")
}
const fn default_poll() -> Span {
    Span::secs(60)
}
const fn default_parallel() -> usize {
    2
}
fn default_gh() -> String {
    "gh".into()
}

/// Which issues the autonomous mode picks up.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IntakeSettings {
    /// An issue with any of these labels is eligible.
    #[serde(default)]
    pub labels: Vec<String>,
    /// An issue assigned to this login is eligible.
    #[serde(default)]
    pub assignee: Option<String>,
    #[serde(default = "default_skip_labels")]
    pub skip_labels: Vec<String>,
    /// Labels that move an issue to the front, highest first.
    #[serde(default)]
    pub priority_labels: Vec<String>,
}

fn default_skip_labels() -> Vec<String> {
    vec!["amaru:hold".into(), "needs-human".into()]
}

/// A verification command run in the worktree.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSettings {
    pub name: String,
    pub run: String,
    #[serde(default = "default_check_timeout")]
    pub timeout: Span,
}

const fn default_check_timeout() -> Span {
    Span::secs(30 * 60)
}

/// Whether the daemon reviews its own work before publishing.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SelfReview {
    #[default]
    Off,
    SameAgent,
    OtherAgent,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewSettings {
    #[serde(default, rename = "self")]
    pub self_review: SelfReview,
    /// Agent used when `self` is `other_agent`.
    #[serde(default)]
    pub reviewer_agent: Option<String>,
    /// Act on reviews received on the pull request.
    #[serde(default = "yes")]
    pub respond: bool,
    /// Only these logins' reviews and comments reach the agent.
    #[serde(default)]
    pub reviewers: Vec<String>,
    #[serde(default = "default_review_rounds")]
    pub max_rounds: u32,
    /// Treat failing check runs on the pull request as review feedback.
    #[serde(default = "yes")]
    pub fix_ci: bool,
    /// The emoji put on each review and comment the agent picks up.
    #[serde(default)]
    pub reaction: Reaction,
}

impl Default for ReviewSettings {
    fn default() -> Self {
        Self {
            self_review: SelfReview::Off,
            reviewer_agent: None,
            respond: true,
            reviewers: Vec::new(),
            max_rounds: default_review_rounds(),
            fix_ci: true,
            reaction: Reaction::default(),
        }
    }
}

const fn yes() -> bool {
    true
}
const fn default_review_rounds() -> u32 {
    5
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceSettings {
    #[serde(default = "default_keep_blocked")]
    pub keep_blocked: Retention,
}

impl Default for WorkspaceSettings {
    fn default() -> Self {
        Self {
            keep_blocked: default_keep_blocked(),
        }
    }
}

const fn default_keep_blocked() -> Retention {
    Retention::For(Duration::from_secs(7 * 86_400))
}

/// Tickets of a project that live in Jira instead of GitHub issues; pull
/// requests still go to the GitHub repository.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JiraSettings {
    /// The Jira server, for example `https://issues.apache.org/jira`.
    pub url: String,
    /// The Jira project key, for example `CAMEL`.
    pub project: String,
    /// The environment variable holding a personal access token. Reading
    /// public issues needs none; commenting does.
    #[serde(default = "default_jira_token_env")]
    pub token_env: String,
    /// Sign in with this user name and the token as its password instead of
    /// sending the token as a bearer token.
    #[serde(default)]
    pub user: Option<String>,
    /// Extra issues `amaru serve` picks up, as a JQL condition, for example
    /// `assignee = currentUser() AND labels = amaru`.
    #[serde(default)]
    pub jql: Option<String>,
    /// Comment on the Jira issue when work starts, stops or has a pull
    /// request. Needs the token.
    #[serde(default = "yes")]
    pub comment: bool,
}

impl JiraSettings {
    /// The token from the environment, when one is set.
    pub fn token(&self) -> Option<String> {
        std::env::var(&self.token_env)
            .ok()
            .filter(|t| !t.trim().is_empty())
    }

    /// The Jira key of an issue number, for example `CAMEL-12345`.
    pub fn key(&self, number: u64) -> String {
        format!("{}-{number}", self.project)
    }

    pub fn browse_url(&self, number: u64) -> String {
        format!(
            "{}/browse/{}",
            self.url.trim_end_matches('/'),
            self.key(number)
        )
    }

    /// The issue number of a key of this project, `CAMEL-12345` or its
    /// browse URL.
    pub fn parse(&self, text: &str) -> Option<u64> {
        let text = text.trim().trim_end_matches('/');
        let key = text.rsplit('/').next().unwrap_or(text);
        let (project, number) = key.rsplit_once('-')?;
        if !project.eq_ignore_ascii_case(&self.project) {
            return None;
        }
        number.parse().ok()
    }
}

fn default_jira_token_env() -> String {
    "JIRA_TOKEN".into()
}

/// Settings for one repository.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSettings {
    /// `owner/name` on GitHub.
    pub repo: String,
    #[serde(default = "default_base")]
    pub base_branch: String,
    /// Push branches to this fork and open pull requests upstream.
    #[serde(default)]
    pub fork: Option<String>,
    #[serde(default = "default_prefix")]
    pub branch_prefix: String,
    /// Take this project's tickets from Jira instead of GitHub issues.
    #[serde(default)]
    pub jira: Option<JiraSettings>,
    #[serde(default)]
    pub intake: IntakeSettings,
    /// Name of the agent in the daemon file.
    pub agent: String,
    #[serde(default = "default_step_timeout")]
    pub step_timeout: Span,
    #[serde(default)]
    pub checks: Vec<CheckSettings>,
    #[serde(default = "default_fix_rounds")]
    pub max_fix_rounds: u32,
    #[serde(default)]
    pub review: ReviewSettings,
    #[serde(default)]
    pub workspace: WorkspaceSettings,
}

impl ProjectSettings {
    /// How people name an issue: `CAMEL-12345` or `owner/name#42`.
    pub fn issue_name(&self, number: u64) -> String {
        match &self.jira {
            Some(jira) => jira.key(number),
            None => format!("{}#{number}", self.repo),
        }
    }

    /// The issue's web page.
    pub fn issue_url(&self, number: u64) -> String {
        match &self.jira {
            Some(jira) => jira.browse_url(number),
            None => format!("https://github.com/{}/issues/{number}", self.repo),
        }
    }
}

fn default_base() -> String {
    "main".into()
}
fn default_prefix() -> String {
    "amaru/".into()
}
const fn default_step_timeout() -> Span {
    Span::secs(45 * 60)
}
const fn default_fix_rounds() -> u32 {
    3
}

/// The daemon file with every project it lists, loaded and checked.
#[derive(Clone, Debug)]
pub struct Settings {
    pub daemon: DaemonSettings,
    pub projects: Vec<ProjectSettings>,
    /// Directory of the daemon file; relative paths resolve against it.
    pub base_dir: PathBuf,
}

/// The most tickets the daemon will work on at once.
pub const MAX_PARALLEL: usize = 2;

fn read_yaml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    serde_yaml::from_str(&text).with_context(|| format!("invalid {}", path.display()))
}

fn is_repo_name(name: &str) -> bool {
    name.split_once('/').is_some_and(|(owner, repo)| {
        [owner, repo].iter().all(|part| {
            !part.is_empty()
                && !part.starts_with('.')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    })
}

impl Settings {
    pub fn load(path: &Path) -> Result<Self> {
        let daemon: DaemonSettings = read_yaml(path)?;
        let base_dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let projects = daemon
            .projects
            .iter()
            .map(|p| read_yaml(&base_dir.join(p)))
            .collect::<Result<Vec<ProjectSettings>>>()?;
        let settings = Self {
            daemon,
            projects,
            base_dir,
        };
        settings.validate()?;
        Ok(settings)
    }

    pub fn state_dir(&self) -> PathBuf {
        self.base_dir.join(&self.daemon.state_dir)
    }

    pub fn project(&self, repo: &str) -> Option<&ProjectSettings> {
        self.projects
            .iter()
            .find(|p| p.repo.eq_ignore_ascii_case(repo))
    }

    /// The project and issue number of a Jira key or browse URL.
    pub fn jira_issue(&self, text: &str) -> Option<(&ProjectSettings, u64)> {
        self.projects.iter().find_map(|p| {
            let number = p.jira.as_ref()?.parse(text)?;
            Some((p, number))
        })
    }

    pub fn agent(&self, name: &str) -> Result<&AgentSettings> {
        self.daemon
            .agents
            .get(name)
            .with_context(|| format!("agent '{name}' is not defined in the daemon file"))
    }

    fn validate(&self) -> Result<()> {
        if !(1..=MAX_PARALLEL).contains(&self.daemon.max_parallel) {
            bail!("max_parallel must be between 1 and {MAX_PARALLEL}");
        }
        if self
            .daemon
            .web
            .listen
            .parse::<std::net::SocketAddr>()
            .is_err()
        {
            bail!(
                "web.listen '{}' must be an address and a port, such as 127.0.0.1:8765",
                self.daemon.web.listen
            );
        }
        for project in &self.projects {
            self.validate_project(project)?;
        }
        Ok(())
    }

    fn validate_project(&self, project: &ProjectSettings) -> Result<()> {
        let repo = &project.repo;
        if !is_repo_name(repo) {
            bail!("repo '{repo}' must be owner/name");
        }
        if let Some(fork) = &project.fork
            && (!is_repo_name(fork) || fork.eq_ignore_ascii_case(repo))
        {
            bail!("{repo}: fork must be owner/name and differ from repo");
        }
        self.agent(&project.agent)?;
        if project.review.self_review == SelfReview::OtherAgent {
            let reviewer = project.review.reviewer_agent.as_deref();
            self.agent(reviewer.context("review.reviewer_agent is required for other_agent")?)?;
        }
        if let Some(jira) = &project.jira {
            let key = &jira.project;
            if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                bail!("{repo}: jira.project '{key}' must be a Jira project key");
            }
            if !jira.url.starts_with("https://") && !jira.url.starts_with("http://") {
                bail!("{repo}: jira.url must be an http(s) URL");
            }
            // A Jira project may be claimed by hand only.
            return Ok(());
        }
        if project.intake.labels.is_empty() && project.intake.assignee.is_none() {
            bail!("{repo}: intake needs labels, an assignee, or both");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAEMON: &str =
        "agents:\n  claude:\n    driver: claude\n    command: claude\nprojects: [demo.yaml]\n";
    const PROJECT: &str = "repo: me/demo\nagent: claude\nintake:\n  labels: [amaru]\n";

    fn load(daemon: &str, project: &str) -> Result<Settings> {
        let dir = tempfile::tempdir()?;
        std::fs::write(dir.path().join("daemon.yaml"), daemon)?;
        std::fs::write(dir.path().join("demo.yaml"), project)?;
        Settings::load(&dir.path().join("daemon.yaml"))
    }

    #[test]
    fn loads_with_defaults() {
        let settings = load(DAEMON, PROJECT).unwrap();
        assert_eq!(settings.daemon.max_parallel, 2);
        assert_eq!(settings.daemon.poll_interval, Span::secs(60));
        let project = settings.project("ME/demo").unwrap();
        assert_eq!(project.base_branch, "main");
        assert_eq!(project.review.self_review, SelfReview::Off);
        assert!(project.review.respond);
        assert_eq!(project.review.reaction, Reaction::ThumbsUp);
        assert_eq!(
            project.workspace.keep_blocked,
            Retention::For(Duration::from_secs(604_800))
        );
    }

    #[test]
    fn reads_the_review_reaction() {
        let eyes = format!("{PROJECT}review:\n  reaction: eyes\n");
        let settings = load(DAEMON, &eyes).unwrap();
        assert_eq!(
            settings.project("me/demo").unwrap().review.reaction,
            Reaction::Eyes
        );
        let off = format!("{PROJECT}review:\n  reaction: off\n");
        let settings = load(DAEMON, &off).unwrap();
        let review = &settings.project("me/demo").unwrap().review;
        assert_eq!(review.reaction.github_name(), None);
        assert!(load(DAEMON, &format!("{PROJECT}review:\n  reaction: '+1'\n")).is_err());
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = load(DAEMON, &format!("{PROJECT}chekcs: []\n")).unwrap_err();
        assert!(format!("{err:#}").contains("unknown field"));
    }

    #[test]
    fn rejects_more_than_two_slots() {
        assert!(load(&format!("{DAEMON}max_parallel: 3\n"), PROJECT).is_err());
    }

    #[test]
    fn rejects_unknown_agent_and_missing_intake() {
        assert!(
            load(
                DAEMON,
                "repo: me/demo\nagent: codex\nintake: {labels: [a]}\n"
            )
            .is_err()
        );
        assert!(load(DAEMON, "repo: me/demo\nagent: claude\n").is_err());
    }

    #[test]
    fn reads_jira_projects() {
        let jira = "repo: apache/camel\nagent: claude\njira:\n  url: https://issues.apache.org/jira/\n  project: CAMEL\n";
        let settings = load(DAEMON, jira).unwrap();
        let project = settings.project("apache/camel").unwrap();
        let config = project.jira.as_ref().unwrap();
        assert_eq!(config.token_env, "JIRA_TOKEN");
        assert!(config.comment);
        assert_eq!(project.issue_name(12_345), "CAMEL-12345");
        assert_eq!(
            project.issue_url(7),
            "https://issues.apache.org/jira/browse/CAMEL-7"
        );
        for text in [
            "CAMEL-12345",
            " camel-12345 ",
            "https://issues.apache.org/jira/browse/CAMEL-12345",
        ] {
            let (found, number) = settings.jira_issue(text).unwrap();
            assert_eq!((found.repo.as_str(), number), ("apache/camel", 12_345));
        }
        assert!(settings.jira_issue("KAFKA-1").is_none());
        assert!(settings.jira_issue("CAMEL-x").is_none());
        let bad = jira.replace("CAMEL", "CAMEL KAFKA");
        assert!(load(DAEMON, &bad).is_err());
    }

    #[test]
    fn parses_spans_and_retention() {
        assert_eq!(parse_span("45m").unwrap(), Duration::from_secs(2_700));
        assert_eq!(parse_span("0").unwrap(), Duration::ZERO);
        assert!(parse_span("5 weeks").is_err());
        let forever: Retention = serde_yaml::from_str("forever").unwrap();
        assert_eq!(forever, Retention::Forever);
    }
}

#[cfg(test)]
mod example_tests {
    use super::*;

    #[test]
    fn example_files_load() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/daemon.yaml");
        let settings = Settings::load(&path).unwrap();
        assert_eq!(settings.projects.len(), 1);
        assert_eq!(settings.projects[0].checks[0].run, "make check");
    }
}
