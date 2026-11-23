use crate::gitops::command_output;
use anyhow::{Context, Result, bail};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
#[derive(Clone)]
pub struct GitHubClient {
    pub repository: String,
    pub cwd: PathBuf,
    pub cli: String,
}
impl GitHubClient {
    pub fn new(repository: &str, cwd: &Path, cli: &str) -> Self {
        Self {
            repository: repository.into(),
            cwd: cwd.into(),
            cli: cli.into(),
        }
    }
    fn run(&self, args: &[&str]) -> Result<String> {
        let o = command_output(
            Command::new(&self.cli).args(args).current_dir(&self.cwd),
            90,
        )?;
        if !o.status.success() {
            bail!(
                "GitHub command failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&o.stdout).trim().into())
    }
    fn json(&self, args: &[&str]) -> Result<Value> {
        let raw = self.run(args)?;
        Ok(serde_json::from_str(if raw.is_empty() {
            "null"
        } else {
            &raw
        })?)
    }
    fn array(&self, args: &[&str]) -> Result<Vec<Value>> {
        let v = self.json(args)?;
        Ok(v.as_array().context("expected GitHub array")?.clone())
    }
    pub fn auth_status(&self) -> Result<String> {
        self.run(&["auth", "status"])
    }
    pub fn list_milestones(&self) -> Result<Vec<Value>> {
        let raw = self.run(&[
            "api",
            &format!(
                "repos/{}/milestones?state=all&per_page=100",
                self.repository
            ),
            "--paginate",
            "--jq",
            ".[] | @json",
        ])?;
        raw.lines()
            .filter(|s| !s.trim().is_empty())
            .map(|s| Ok(serde_json::from_str(s)?))
            .collect()
    }
    pub fn resolve_milestone(milestones: &[Value], selector: &str) -> Result<Value> {
        let selector = selector.trim();
        if selector.chars().all(|c| c.is_ascii_digit()) {
            let re = Regex::new(&format!(r"(?i)^m{}\b", regex::escape(selector)))?;
            let matches: Vec<_> = milestones
                .iter()
                .filter(|m| re.is_match(m["title"].as_str().unwrap_or("")))
                .collect();
            if matches.len() == 1 {
                return Ok(matches[0].clone());
            }
        }
        let matches: Vec<_> = milestones
            .iter()
            .filter(|m| {
                m["number"]
                    .as_i64()
                    .is_some_and(|n| selector.parse::<i64>() == Ok(n))
                    || m["title"]
                        .as_str()
                        .unwrap_or("")
                        .eq_ignore_ascii_case(selector)
            })
            .collect();
        if matches.len() != 1 {
            bail!("milestone '{selector}' missing or ambiguous");
        }
        Ok(matches[0].clone())
    }
    pub fn list_issues(&self, title: &str) -> Result<Vec<Value>> {
        self.array(&[
            "issue",
            "list",
            "--repo",
            &self.repository,
            "--milestone",
            title,
            "--state",
            "all",
            "--limit",
            "500",
            "--json",
            "number,title,body,state,url,labels,assignees,milestone,comments",
        ])
    }
    pub fn get_issue(&self, n: i64) -> Result<Value> {
        self.json(&[
            "issue",
            "view",
            &n.to_string(),
            "--repo",
            &self.repository,
            "--json",
            "number,title,body,state,url,labels,assignees,milestone,comments",
        ])
    }
    pub fn discover_exit_issue(&self, issues: &[Value], config: &Value) -> Result<Option<Value>> {
        if let Some(n) = config["exit_issue_number"].as_i64() {
            return Ok(Some(match issues.iter().find(|i| i["number"] == n) {
                Some(i) => i.clone(),
                None => self.get_issue(n)?,
            }));
        }
        let Some(label) = config["exit_issue_label"].as_str() else {
            return Ok(None);
        };
        let mut found: Vec<Value> = issues
            .iter()
            .filter(|i| crate::sources::label_names(i).iter().any(|s| s == label))
            .cloned()
            .collect();
        if found.is_empty() {
            found = self.array(&[
                "issue",
                "list",
                "--repo",
                &self.repository,
                "--state",
                "all",
                "--label",
                label,
                "--limit",
                "100",
                "--json",
                "number,title,body,state,url,labels,assignees,milestone,comments",
            ])?;
        }
        if found.len() > 1 {
            bail!("multiple exit issues for label '{label}'");
        }
        Ok(found.pop())
    }
    pub fn comment(&self, n: i64, body: &str) -> Result<String> {
        self.run(&[
            "issue",
            "comment",
            &n.to_string(),
            "--repo",
            &self.repository,
            "--body",
            body,
        ])
    }
    fn api(&self, path: &str) -> Result<Value> {
        self.json(&["api", &format!("repos/{}/{path}", self.repository)])
    }
    pub fn blocked_by(&self, n: i64) -> Result<Vec<Value>> {
        Ok(self
            .api(&format!("issues/{n}/dependencies/blocked_by"))?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }
    pub fn sub_issues(&self, n: i64) -> Result<Vec<Value>> {
        Ok(self
            .api(&format!("issues/{n}/sub_issues"))?
            .as_array()
            .cloned()
            .unwrap_or_default())
    }
    pub fn parent_issue(&self, n: i64) -> Result<Option<Value>> {
        match self.api(&format!("issues/{n}/parent")) {
            Ok(v) => Ok(if v.is_null() { None } else { Some(v) }),
            Err(e)
                if ["404", "Not Found", "No parent"]
                    .iter()
                    .any(|s| e.to_string().contains(s)) =>
            {
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }
    pub fn merged_prs(&self, limit: usize) -> Result<Vec<Value>> {
        self.array(&[
            "pr",
            "list",
            "--repo",
            &self.repository,
            "--state",
            "merged",
            "--limit",
            &limit.to_string(),
            "--json",
            "number,title,baseRefName,headRefName,mergedAt,url",
        ])
    }
    pub fn all_prs(&self) -> Result<Vec<Value>> {
        self.array(&[
            "pr",
            "list",
            "--repo",
            &self.repository,
            "--state",
            "all",
            "--limit",
            "300",
            "--json",
            "number,title,state,baseRefName,headRefName,headRepository,url,mergedAt",
        ])
    }
    pub fn verify_fork(&self, repository: &str) -> Result<()> {
        let fork = self.json(&["api", &format!("repos/{repository}")])?;
        let upstream = self.repository.trim_end_matches('/');
        if fork["fork"] != true
            || !fork["parent"]["full_name"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(upstream))
        {
            bail!("{repository} is not a GitHub fork of {upstream}");
        }
        Ok(())
    }
    pub fn fork_pr_for_head(
        &self,
        repository: &str,
        branch: &str,
        base: &str,
    ) -> Result<Option<Value>> {
        let (owner, _) = repository
            .split_once('/')
            .context("invalid fork repository")?;
        let pulls = self.array(&[
            "api",
            "--method",
            "GET",
            &format!("repos/{}/pulls", self.repository.trim_end_matches('/')),
            "-f",
            "state=all",
            "-f",
            &format!("head={owner}:{branch}"),
            "-f",
            &format!("base={base}"),
        ])?;
        Ok(pulls
            .into_iter()
            .filter(|pr| {
                pr["head"]["repo"]["full_name"]
                    .as_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(repository))
                    && pr["head"]["ref"] == branch
                    && pr["base"]["ref"] == base
                    && (pr["state"] == "open" || !pr["merged_at"].is_null())
            })
            .min_by_key(|pr| pr["state"] != "open")
            .map(|pr| {
                json!({
                    "number":pr["number"],"url":pr["html_url"],
                    "state":if pr["state"]=="open" {"OPEN"} else {"MERGED"}
                })
            }))
    }
    pub fn pr_for_head(&self, branch: &str) -> Result<Option<Value>> {
        let prs = self.array(&[
            "pr",
            "list",
            "--repo",
            &self.repository,
            "--state",
            "all",
            "--head",
            branch,
            "--limit",
            "5",
            "--json",
            "number,title,state,baseRefName,headRefName,url,mergedAt",
        ])?;
        Ok(prs
            .iter()
            .find(|p| p["state"] == "OPEN")
            .or(prs.first())
            .cloned())
    }
    pub fn pr_view(&self, n: i64) -> Result<Value> {
        self.json(&[
            "pr",
            "view",
            &n.to_string(),
            "--repo",
            &self.repository,
            "--json",
            "number,title,state,baseRefName,headRefName,url,mergedAt,mergeable,isDraft",
        ])
    }
    pub fn pr_checks(&self, n: i64) -> Result<String> {
        let args = [
            "pr",
            "checks",
            &n.to_string(),
            "--repo",
            &self.repository,
            "--json",
            "bucket",
        ];
        let o = command_output(
            Command::new(&self.cli).args(args).current_dir(&self.cwd),
            90,
        )?;
        let err = String::from_utf8_lossy(&o.stderr);
        if err.to_ascii_lowercase().contains("no checks reported") {
            return Ok("none".into());
        }
        let data: Value =
            serde_json::from_slice(&o.stdout).context(format!("invalid check response: {err}"))?;
        let checks = data.as_array().context("expected checks array")?;
        if checks
            .iter()
            .any(|c| ["fail", "cancel"].contains(&c["bucket"].as_str().unwrap_or("")))
        {
            return Ok("fail".into());
        }
        if checks.iter().any(|c| c["bucket"] == "pending") {
            return Ok("pending".into());
        }
        if !o.status.success() {
            bail!("GitHub checks failed: {err}");
        }
        Ok(if checks.is_empty() { "none" } else { "pass" }.into())
    }
    pub fn create_pr(&self, base: &str, head: &str, title: &str, body: &str) -> Result<String> {
        self.run(&[
            "pr",
            "create",
            "--repo",
            &self.repository,
            "--base",
            base,
            "--head",
            head,
            "--title",
            title,
            "--body",
            body,
        ])
    }
    pub fn create_fork_pr(
        &self,
        base: &str,
        repository: &str,
        branch: &str,
        title: &str,
        body: &str,
    ) -> Result<String> {
        let (owner, name) = repository
            .split_once('/')
            .context("invalid fork repository")?;
        self.run(&[
            "api",
            "--method",
            "POST",
            &format!("repos/{}/pulls", self.repository.trim_end_matches('/')),
            "-f",
            &format!("base={base}"),
            "-f",
            &format!("head={owner}:{branch}"),
            "-f",
            &format!("head_repo={name}"),
            "-f",
            &format!("title={title}"),
            "-f",
            &format!("body={body}"),
            "--jq",
            ".html_url",
        ])
    }
    pub fn pr_comment(&self, n: i64, body: &str) -> Result<String> {
        self.run(&[
            "pr",
            "comment",
            &n.to_string(),
            "--repo",
            &self.repository,
            "--body",
            body,
        ])
    }
    pub fn merge_pr(&self, n: i64, method: &str) -> Result<String> {
        if !["squash", "merge", "rebase"].contains(&method) {
            bail!("invalid merge method");
        }
        self.run(&[
            "pr",
            "merge",
            &n.to_string(),
            "--repo",
            &self.repository,
            &format!("--{method}"),
            "--delete-branch",
        ])
    }
    pub fn close_issue(&self, n: i64, comment: &str) -> Result<String> {
        self.run(&[
            "issue",
            "close",
            &n.to_string(),
            "--repo",
            &self.repository,
            "--comment",
            comment,
        ])
    }
    pub fn snapshot(&self, selector: &str, config: &Value) -> Result<Value> {
        let milestone = Self::resolve_milestone(&self.list_milestones()?, selector)?;
        let issues = self.list_issues(milestone["title"].as_str().context("milestone title")?)?;
        let exit = self.discover_exit_issue(&issues, config)?;
        Ok(json!({"milestone":milestone,"issues":issues,"exit_issue":exit}))
    }
    pub fn selector_from_prompt(prompt: &str) -> Option<String> {
        for pattern in [
            r"(?i)\bmilestone\s*#?\s*(\d+)\b",
            r#"(?i)\bmilestone\s+["']([^"']+)["']"#,
        ] {
            if let Some(c) = Regex::new(pattern).ok()?.captures(prompt) {
                return Some(c[1].trim().into());
            }
        }
        None
    }
    pub fn format_snapshot(snapshot: &Value) -> String {
        let m = &snapshot["milestone"];
        let mut text = format!(
            "Milestone #{}: {}\nDescription: {}\n\nISSUES (authoritative GitHub context):\n",
            m["number"],
            m["title"].as_str().unwrap_or(""),
            m["description"].as_str().unwrap_or("")
        );
        for issue in snapshot["issues"].as_array().into_iter().flatten() {
            text.push_str(&format!(
                "### Issue #{} [{}] {}\nURL: {}\nLabels: {}\nBody:\n{}\nComments:\n{}\n\n",
                issue["number"],
                issue["state"].as_str().unwrap_or(""),
                issue["title"].as_str().unwrap_or(""),
                issue["url"].as_str().unwrap_or(""),
                crate::sources::label_names(issue).join(", "),
                issue["body"].as_str().unwrap_or(""),
                issue["comments"]
            ));
        }
        text.push_str(&format!("EXIT ISSUE: {}", snapshot["exit_issue"]));
        text
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn numeric_milestone_prefers_title() {
        let m = vec![
            json!({"number":5,"title":"M4 Local MVP"}),
            json!({"number":6,"title":"M5 Delivery"}),
        ];
        assert_eq!(
            GitHubClient::resolve_milestone(&m, "5").unwrap()["number"],
            6
        );
        assert!(GitHubClient::resolve_milestone(&m, "99").is_err());
    }
    #[test]
    fn selectors() {
        assert_eq!(
            GitHubClient::selector_from_prompt("Continue Milestone #5"),
            Some("5".into())
        );
        assert_eq!(
            GitHubClient::selector_from_prompt("Milestone 'Local MVP'"),
            Some("Local MVP".into())
        );
    }
}
