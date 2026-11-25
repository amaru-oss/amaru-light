//! Jira issues as a ticket source, through its REST API (version 2, which
//! Jira Server and Data Center, such as the ASF's, serve).
//!
//! A Jira project is tied to one GitHub repository in the project file. The
//! ticket keeps that repository and the number of the Jira key, so
//! `CAMEL-12345` becomes ticket number 12345 of `apache/camel`. Pull requests
//! and reviews stay on GitHub.
//!
//! Reading public issues needs no account. Comments need a personal access
//! token and are best-effort, as labels are on GitHub: an issue tracker the
//! bot may not write to must not stop the work. Jira issues are never
//! labelled.

use super::{Issue, Selector, TicketSource};
use crate::{
    process::{self, Spec},
    settings::JiraSettings,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{fmt, sync::Arc, time::Duration};

/// The fields the daemon reads from an issue.
const FIELDS: &str =
    "summary,description,labels,assignee,fixVersions,created,status,issuelinks,comment";

/// How requests sign in.
#[derive(Clone, PartialEq, Eq)]
pub enum Auth {
    Anonymous,
    /// A personal access token sent as a bearer token.
    Bearer(String),
    /// A user name with the token as its password.
    Basic {
        user: String,
        token: String,
    },
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the token.
        f.write_str(match self {
            Self::Anonymous => "Anonymous",
            Self::Bearer(_) => "Bearer",
            Self::Basic { .. } => "Basic",
        })
    }
}

/// JSON over HTTP, so tests can answer without a server.
#[async_trait]
pub trait Http: Send + Sync + fmt::Debug {
    async fn get(&self, url: &str, auth: &Auth) -> Result<Value>;
    async fn post(&self, url: &str, auth: &Auth, body: &Value) -> Result<Value>;
}

/// HTTP through `curl`. The token goes through stdin, never the command line.
#[derive(Clone, Debug)]
pub struct Curl {
    program: String,
    timeout: Duration,
}

impl Default for Curl {
    fn default() -> Self {
        Self {
            program: "curl".into(),
            timeout: Duration::from_secs(90),
        }
    }
}

/// A double-quoted value of a curl config file.
fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// The curl config for one request.
fn curl_config(url: &str, auth: &Auth, body: Option<&Value>) -> String {
    let mut lines = vec![
        format!("url = {}", quoted(url)),
        "header = \"Accept: application/json\"".to_owned(),
    ];
    match auth {
        Auth::Anonymous => {}
        Auth::Bearer(token) => {
            lines.push(format!(
                "header = {}",
                quoted(&format!("Authorization: Bearer {token}"))
            ));
        }
        Auth::Basic { user, token } => {
            lines.push(format!("user = {}", quoted(&format!("{user}:{token}"))));
        }
    }
    if let Some(body) = body {
        lines.push("header = \"Content-Type: application/json\"".to_owned());
        lines.push(format!("data-binary = {}", quoted(&body.to_string())));
    }
    lines.join("\n") + "\n"
}

impl Curl {
    async fn request(&self, url: &str, auth: &Auth, body: Option<&Value>) -> Result<Value> {
        let spec = Spec {
            program: self.program.clone(),
            args: ["-sS", "--fail-with-body", "--config", "-"]
                .map(str::to_owned)
                .to_vec(),
            cwd: std::env::temp_dir(),
            stdin: Some(curl_config(url, auth, body)),
            timeout: self.timeout,
            ..Spec::default()
        };
        let mut text = String::new();
        let output = process::run(&spec, None, &mut |line| {
            text.push_str(line);
            text.push('\n');
        })
        .await?;
        if !output.success() {
            bail!(
                "{url}: {} {}",
                output.stderr.trim(),
                process::tail(text.trim(), 500)
            );
        }
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_str(&text).with_context(|| format!("{url} returned invalid JSON"))
    }
}

#[async_trait]
impl Http for Curl {
    async fn get(&self, url: &str, auth: &Auth) -> Result<Value> {
        self.request(url, auth, None).await
    }
    async fn post(&self, url: &str, auth: &Auth, body: &Value) -> Result<Value> {
        self.request(url, auth, Some(body)).await
    }
}

/// Percent-encode a query parameter.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// A JQL string literal.
fn literal(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn text(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn items(value: &Value) -> &[Value] {
    value.as_array().map_or(&[], Vec::as_slice)
}

fn is_open(fields: &Value) -> bool {
    fields["status"]["statusCategory"]["key"] != "done"
}

/// Turn the daemon's Markdown comments into Jira wiki markup: inline code
/// and quoted lines.
pub fn to_wiki(markdown: &str) -> String {
    let mut out = Vec::new();
    let mut quoting = false;
    for line in markdown.lines() {
        let quote = line
            .strip_prefix("> ")
            .or_else(|| (line == ">").then_some(""));
        match (quote, quoting) {
            (Some(_), false) => {
                out.push("{quote}".to_owned());
                quoting = true;
            }
            (None, true) => {
                out.push("{quote}".to_owned());
                quoting = false;
            }
            _ => {}
        }
        out.push(inline_code(quote.unwrap_or(line)));
    }
    if quoting {
        out.push("{quote}".to_owned());
    }
    out.join("\n")
}

fn inline_code(line: &str) -> String {
    let parts: Vec<&str> = line.split('`').collect();
    if parts.len().is_multiple_of(2) {
        // An unmatched backtick: leave the line alone.
        return line.to_owned();
    }
    parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            if i % 2 == 1 {
                format!("{{{{{part}}}}}")
            } else {
                (*part).to_owned()
            }
        })
        .collect()
}

/// The issues of one Jira project.
#[derive(Clone, Debug)]
pub struct Jira {
    settings: JiraSettings,
    auth: Auth,
    http: Arc<dyn Http>,
}

impl Jira {
    /// A source that signs in with the token from the environment, if any.
    pub fn new(settings: JiraSettings, http: Arc<dyn Http>) -> Self {
        let auth = match (settings.token(), &settings.user) {
            (None, _) => Auth::Anonymous,
            (Some(token), None) => Auth::Bearer(token),
            (Some(token), Some(user)) => Auth::Basic {
                user: user.clone(),
                token,
            },
        };
        Self::with_auth(settings, http, auth)
    }

    pub const fn with_auth(settings: JiraSettings, http: Arc<dyn Http>, auth: Auth) -> Self {
        Self {
            settings,
            auth,
            http,
        }
    }

    fn api(&self, path: &str) -> String {
        format!(
            "{}/rest/api/2/{path}",
            self.settings.url.trim_end_matches('/')
        )
    }

    /// The number of a key of this project.
    fn number(&self, key: &str) -> Option<u64> {
        let (project, number) = key.rsplit_once('-')?;
        project
            .eq_ignore_ascii_case(&self.settings.project)
            .then(|| number.parse().ok())
            .flatten()
    }

    /// Open issues of this project the issue is blocked by or depends on.
    fn blockers(&self, fields: &Value) -> Vec<u64> {
        let mut found = Vec::new();
        for link in items(&fields["issuelinks"]) {
            let kind = &link["type"];
            let inward = text(&kind["inward"]).to_lowercase();
            let outward = text(&kind["outward"]).to_lowercase();
            let other = if link["inwardIssue"].is_object() && inward.contains("blocked by") {
                &link["inwardIssue"]
            } else if link["outwardIssue"].is_object() && outward.starts_with("depends") {
                &link["outwardIssue"]
            } else {
                continue;
            };
            if let Some(n) = self.number(&text(&other["key"]))
                && is_open(&other["fields"])
                && !found.contains(&n)
            {
                found.push(n);
            }
        }
        found
    }

    fn to_issue(&self, raw: &Value) -> Result<Issue> {
        let key = text(&raw["key"]);
        let number = self
            .number(&key)
            .with_context(|| format!("'{key}' is not an issue of {}", self.settings.project))?;
        let fields = &raw["fields"];
        let version = items(&fields["fixVersions"]).first();
        Ok(Issue {
            number,
            key: Some(key.clone()),
            title: text(&fields["summary"]),
            body: body(&key, fields),
            labels: items(&fields["labels"]).iter().map(text).collect(),
            assignees: fields["assignee"]["name"]
                .as_str()
                .map(str::to_owned)
                .into_iter()
                .collect(),
            milestone: version.map(|v| text(&v["name"])),
            milestone_due: version.and_then(|v| v["releaseDate"].as_str().map(str::to_owned)),
            open: is_open(fields),
            created_at: text(&fields["created"]),
            blocked_by: self.blockers(fields),
        })
    }

    /// Open issues of this project matching `condition`, oldest first.
    async fn search(&self, condition: &str) -> Result<Vec<Issue>> {
        let jql = format!(
            "project = {} AND statusCategory != Done AND ({condition}) ORDER BY created ASC",
            literal(&self.settings.project)
        );
        let url = self.api(&format!(
            "search?jql={}&maxResults=100&fields={FIELDS}",
            encode(&jql)
        ));
        let found = self.http.get(&url, &self.auth).await?;
        items(&found["issues"])
            .iter()
            .map(|raw| self.to_issue(raw))
            .collect()
    }

    /// Reach the server, and sign in when a token is set; describes the result.
    pub async fn check(&self) -> Result<String> {
        if self.auth == Auth::Anonymous {
            let info = self.http.get(&self.api("serverInfo"), &self.auth).await?;
            return Ok(format!(
                "Jira {} without a token: reads only, no comments",
                text(&info["version"])
            ));
        }
        let me = self.http.get(&self.api("myself"), &self.auth).await?;
        let comments = if self.settings.comment {
            "comments on"
        } else {
            "comments off"
        };
        Ok(format!("signed in as {}, {comments}", text(&me["name"])))
    }

    fn can_comment(&self) -> bool {
        self.settings.comment && self.auth != Auth::Anonymous
    }
}

/// The description followed by the discussion, as the agent reads it.
fn body(key: &str, fields: &Value) -> String {
    let mut body = text(&fields["description"]);
    let comments = items(&fields["comment"]["comments"]);
    if !comments.is_empty() {
        body.push_str(&format!("\n\n## Comments on {key}\n"));
    }
    for c in comments {
        let author = c["author"]["displayName"]
            .as_str()
            .or_else(|| c["author"]["name"].as_str())
            .unwrap_or("someone");
        body.push_str(&format!(
            "\n**{author}** ({}):\n\n{}\n",
            text(&c["created"]),
            text(&c["body"]).trim()
        ));
    }
    body
}

#[async_trait]
impl TicketSource for Jira {
    async fn eligible(&self, _: &str, selector: &Selector) -> Result<Vec<Issue>> {
        let mut clauses = Vec::new();
        if !selector.labels.is_empty() {
            let labels: Vec<String> = selector.labels.iter().map(|l| literal(l)).collect();
            clauses.push(format!("labels in ({})", labels.join(", ")));
        }
        if let Some(login) = &selector.assignee {
            clauses.push(format!("assignee = {}", literal(login)));
        }
        if let Some(jql) = &self.settings.jql {
            clauses.push(format!("({jql})"));
        }
        if clauses.is_empty() {
            // Claimed by hand only.
            return Ok(Vec::new());
        }
        self.search(&clauses.join(" OR ")).await
    }

    async fn milestone(&self, _: &str, milestone: &str) -> Result<Vec<Issue>> {
        self.search(&format!("fixVersion = {}", literal(milestone)))
            .await
    }

    async fn issue(&self, _: &str, number: u64) -> Result<Issue> {
        let key = self.settings.key(number);
        let url = self.api(&format!("issue/{key}?fields={FIELDS}"));
        let raw = self.http.get(&url, &self.auth).await?;
        self.to_issue(&raw)
    }

    async fn comment(&self, _: &str, number: u64, body: &str) -> Result<()> {
        if !self.can_comment() {
            return Ok(());
        }
        let key = self.settings.key(number);
        let url = self.api(&format!("issue/{key}/comment"));
        let payload = json!({ "body": to_wiki(body) });
        if let Err(e) = self.http.post(&url, &self.auth, &payload).await {
            // The work goes on without the note on the issue.
            crate::scheduler::log(&format!("{key}: cannot comment: {e:#}"));
        }
        Ok(())
    }

    async fn add_label(&self, _: &str, number: u64, label: &str) -> Result<()> {
        bail!(
            "{} is a Jira issue; Amaru does not label those ({label})",
            self.settings.key(number)
        )
    }

    async fn remove_label(&self, _: &str, _: u64, _: &str) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
