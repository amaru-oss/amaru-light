use crate::{
    config::Settings, github::GitHubClient, joblog, providers::ProviderRunner, storage::Store,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command, time::Duration};

pub fn extract_json(text: &str) -> Result<Value> {
    let mut text = text.trim();
    if let Some(rest) = text.strip_prefix("```") {
        text = rest.strip_prefix("json").unwrap_or(rest).trim();
        text = text.strip_suffix("```").unwrap_or(text).trim();
    }
    if let Ok(data) = serde_json::from_str(text) {
        return Ok(data);
    }
    let start = text.find('{').context("Brain did not return valid JSON")?;
    let end = text.rfind('}').context("Brain did not return valid JSON")?;
    if end < start {
        bail!("Brain did not return valid JSON");
    }
    Ok(serde_json::from_str(&text[start..=end])?)
}

pub fn resolve_model(settings: &Settings, route: &str) -> Result<(String, Value)> {
    let mut candidates = Vec::new();
    if let Some(key) = settings.routing[route].as_str() {
        candidates.push(key);
    }
    if let Some(fallback) = settings.routing["fallback"].as_array() {
        for key in fallback.iter().filter_map(Value::as_str) {
            if !candidates.contains(&key) {
                candidates.push(key);
            }
        }
    }
    let runner = ProviderRunner::new(&settings.providers);
    let allowed = settings.project["models"]["allowed"].as_array();
    let mut rejected = Vec::new();
    for key in candidates {
        if allowed.is_some_and(|models| !models.iter().any(|model| model == key)) {
            rejected.push(format!("{key}: excluded by project models.allowed"));
            continue;
        }
        let spec = &settings.models[key];
        let (Some(provider), Some(model)) = (spec["provider"].as_str(), spec["model"].as_str())
        else {
            rejected.push(format!(
                "{key}: missing model or provider in config/models.yaml"
            ));
            continue;
        };
        let config = &settings.providers[provider];
        if config.is_null() {
            rejected.push(format!("{key}: provider '{provider}' is not configured"));
            continue;
        }
        let (available, reason) = runner.available(provider);
        if !available {
            rejected.push(format!("{key} ({provider}): {reason}"));
            continue;
        }
        if config["models"]
            .as_array()
            .is_some_and(|models| models.iter().any(|entry| entry == model))
        {
            return Ok((key.to_owned(), spec.clone()));
        }
        rejected.push(format!(
            "{key}: model '{model}' is not listed for provider '{provider}'"
        ));
    }
    let detail = if rejected.is_empty() {
        "no candidates in config/routing.yaml".to_owned()
    } else {
        rejected.join("; ")
    };
    bail!("No available model for route '{route}': {detail}")
}

pub struct Engine<'a> {
    pub settings: &'a Settings,
    pub store: &'a Store,
    pub project_root: PathBuf,
    pub artifacts: PathBuf,
    pub mode: String,
}
impl<'a> Engine<'a> {
    pub fn new(
        settings: &'a Settings,
        store: &'a Store,
        workdir: Option<PathBuf>,
        mode: &str,
    ) -> Result<Self> {
        let artifacts = settings.artifacts_dir();
        fs::create_dir_all(&artifacts)?;
        Ok(Self {
            settings,
            store,
            project_root: workdir.unwrap_or_else(|| settings.project_root()),
            artifacts,
            mode: mode.into(),
        })
    }
    pub fn model(&self, route: &str) -> Result<(String, Value)> {
        resolve_model(self.settings, route)
    }
    pub fn repo_info(&self) -> Value {
        let mut branch = None;
        if self.project_root.is_dir() {
            for args in [
                vec!["symbolic-ref", "--short", "-q", "HEAD"],
                vec!["rev-parse", "--short", "HEAD"],
            ] {
                if let Ok(out) = Command::new("git")
                    .arg("-C")
                    .arg(&self.project_root)
                    .args(args)
                    .output()
                    && out.status.success()
                {
                    branch = Some(String::from_utf8_lossy(&out.stdout).trim().to_string());
                    break;
                }
            }
        }
        json!({"cwd":self.project_root,"exists":self.project_root.is_dir(),"branch":branch,"agents_md":self.project_root.join("AGENTS.md").is_file(),"claude_md":self.project_root.join("CLAUDE.md").is_file()})
    }
    pub fn call(&self, route: &str, prompt: &str, job_id: i64) -> Result<String> {
        let (key, spec) = self.model(route)?;
        let provider = spec["provider"].as_str().context("Missing provider")?;
        let model = spec["model"].as_str().context("Missing model")?;
        let path = joblog::log_path(&self.artifacts, job_id);
        if !path.exists() {
            self.store
                .artifact(job_id, "job_log", &path.to_string_lossy())?;
        }
        joblog::append(
            &path,
            &format!(
                "\n=== {route} - {key} - cwd={} ===\n",
                self.project_root.display()
            ),
        )?;
        let timeout = Duration::from_secs(
            self.settings.providers[provider]["timeout_seconds"]
                .as_u64()
                .unwrap_or(3600),
        );
        ProviderRunner::new(&self.settings.providers).run(
            provider,
            model,
            prompt,
            timeout,
            &self.project_root,
            Some(&path),
        )
    }
    pub fn call_json(&self, route: &str, prompt: &str, job_id: i64) -> Result<Value> {
        extract_json(&self.call(route, prompt, job_id)?)
    }
    fn selector(&self, prompt: &str) -> Option<String> {
        let selector = &self.settings.project["github"]["milestone"];
        if let Some(s) = selector.as_str()
            && !s.is_empty()
        {
            return Some(s.into());
        }
        if let Some(n) = selector.as_i64() {
            return Some(n.to_string());
        }
        GitHubClient::selector_from_prompt(prompt)
    }
    fn client(&self) -> Result<GitHubClient> {
        let repository = self.settings.project["project"]["repository"]
            .as_str()
            .context("project.repository must be owner/repo")?;
        Ok(GitHubClient::new(
            repository,
            &self.project_root,
            self.settings.project["github"]["cli"]
                .as_str()
                .unwrap_or("gh"),
        ))
    }
    pub fn github_context(&self, prompt: &str, job_id: i64) -> Result<String> {
        let Some(selector) = self.selector(prompt) else {
            return Ok(String::new());
        };
        let snapshot = self
            .client()?
            .snapshot(&selector, &self.settings.project["github"])?;
        let text = GitHubClient::format_snapshot(&snapshot);
        let path = self
            .artifacts
            .join(format!("job-{job_id}-github-milestone.md"));
        fs::write(&path, &text)?;
        self.store
            .artifact(job_id, "github_milestone_snapshot", &path.to_string_lossy())?;
        self.store.event(job_id,"GITHUB_MILESTONE_DISCOVERED",&json!({"milestone_number":snapshot["milestone"]["number"],"milestone_title":snapshot["milestone"]["title"],"issue_count":snapshot["issues"].as_array().map(Vec::len).unwrap_or(0),"exit_issue":snapshot["exit_issue"]["number"]}))?;
        Ok(text)
    }
    pub fn plan(
        &self,
        job_id: i64,
        prompt: &str,
        github: bool,
        max_tasks: Option<usize>,
        context: &str,
    ) -> Result<Value> {
        let gh = if github {
            self.github_context(prompt, job_id)?
        } else {
            String::new()
        };
        let planning = format!(
            r#"You are the planning brain of Mini-Amaru. Create a practical bounded execution plan.
Return ONLY JSON: {{"summary":"...","tasks":[{{"title":"...","description":"...","type":"simple_implementation|complex_implementation|debugging|code_review"}}]}}.
For GitHub milestones cover EVERY OPEN issue, preserve issue numbers/URLs and never implement CLOSED issues. Include final verification/report. Tasks run in order and must have acceptance criteria. Never invent repository facts or claim completion while issues remain open.
Project: {}
Current branch: {}
GitHub context: {gh}
{context}
User request: {prompt}"#,
            self.settings.project["project"],
            self.repo_info()["branch"]
        );
        let mut data = self.call_json("planning", &planning, job_id)?;
        let tasks = data["tasks"]
            .as_array_mut()
            .context("Plan tasks must be an array")?;
        if tasks.is_empty() {
            bail!("Plan contains no tasks");
        }
        let limit = max_tasks.unwrap_or_else(|| {
            self.settings.runtime["execution"]["max_tasks"]
                .as_u64()
                .unwrap_or(100) as usize
        });
        if limit == 0 {
            bail!("Task limit must be greater than zero");
        }
        tasks.truncate(limit);
        for task in tasks.iter() {
            if task["title"].as_str().is_none() || task["description"].as_str().is_none() {
                bail!("Task needs title and description");
            }
        }
        self.store.add_tasks(job_id, tasks)?;
        let task_count = tasks.len();
        let path = self.artifacts.join(format!("job-{job_id}-plan.json"));
        let serialized = serde_json::to_string_pretty(&data)?;
        fs::write(&path, &serialized)?;
        self.store
            .artifact(job_id, "plan", &path.to_string_lossy())?;
        self.store
            .set_job(job_id, "RUNNING", Some(&serialized), None)?;
        self.store
            .event(job_id, "PLAN_CREATED", &json!({"task_count":task_count}))?;
        Ok(data)
    }
    fn skills(&self) -> Result<String> {
        let mut names = self.settings.project["skills"]["defaults"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if self.settings.project["execution"]["require_tests"]
            .as_bool()
            .unwrap_or(true)
        {
            names.push(json!("engineering/testing"));
        }
        let configured = self.settings.project["skills"]["directory"]
            .as_str()
            .unwrap_or("../skills");
        let dir = PathBuf::from(configured);
        let dir = if dir.is_absolute() {
            dir
        } else {
            self.settings
                .project_path
                .parent()
                .context("Project config parent missing")?
                .join(dir)
        };
        let mut text = String::new();
        for name in names.iter().filter_map(Value::as_str) {
            let path = dir.join(if name.ends_with(".md") {
                name.to_string()
            } else {
                format!("{name}.md")
            });
            if path.is_file() {
                text.push_str(&format!(
                    "\n## SKILL: {name}\n{}",
                    fs::read_to_string(path)?
                ));
            }
        }
        Ok(text)
    }
    pub fn execute_task(&self, job_id: i64, task: &Value, context: &str) -> Result<String> {
        let route = task["task_type"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or("simple_implementation");
        let rule = if self.mode == "work" {
            "Commit finished work on the CURRENT branch following AGENTS.md. Do not push, merge, switch branches, or alter issues or pull requests; the orchestrator does that."
        } else {
            "Do not commit, push, merge, or alter issues."
        };
        let recent: Vec<Value> = self
            .store
            .tasks(job_id)?
            .into_iter()
            .filter_map(|t| t["summary"].as_str().map(|s| json!(s)))
            .collect();
        let recent = &recent[recent.len().saturating_sub(5)..];
        let prompt = format!(
            "You are a software engineering agent. Perform ONLY the assigned task. Inspect before editing. Do not claim completion without evidence. {rule}\nReturn a concise report: status (COMPLETED or BLOCKED), summary, files changed, tests/results, risks.\nPROJECT: {}\nTASK:\n{}\n{}\nRECENT COMPLETED TASK SUMMARIES: {}\nSKILLS:\n{}\nWORKING DIRECTORY: {}\nCURRENT BRANCH: {}\nFollow the checkout's AGENTS.md / CLAUDE.md and skills.\n{context}",
            self.settings.project["project"],
            task["title"].as_str().unwrap_or(""),
            task["description"].as_str().unwrap_or(""),
            serde_json::to_string(recent)?,
            self.skills()?,
            self.project_root.display(),
            self.repo_info()["branch"]
        );
        let (_, spec) = self.model(route)?;
        let run = self.store.start_run(
            task["id"].as_i64().context("Task id missing")?,
            spec["provider"].as_str().unwrap_or(""),
            spec["model"].as_str().unwrap_or(""),
        )?;
        match self.call(route, &prompt, job_id) {
            Ok(output) => {
                let path = self.artifacts.join(format!(
                    "job-{job_id}-task-{}-attempt-{}.md",
                    task["id"],
                    task["attempts"].as_i64().unwrap_or(0)
                ));
                fs::write(&path, &output)?;
                self.store
                    .artifact(job_id, "task_result", &path.to_string_lossy())?;
                self.store
                    .finish_run(run, "SUCCEEDED", Some(&path.to_string_lossy()), 0)?;
                Ok(output)
            }
            Err(e) => {
                self.store.finish_run(run, "FAILED", None, 1)?;
                Err(e)
            }
        }
    }
    pub fn publish_report(&self, job_id: i64, report: &str) -> Result<String> {
        if !self.settings.project["github"]["publish_report"]
            .as_bool()
            .unwrap_or(true)
        {
            return Ok("Report publication disabled".into());
        }
        if self.settings.project["project"]["repository"]
            .as_str()
            .is_none()
        {
            return Ok("Report saved locally; repository not configured".into());
        }
        let job = self.store.job(job_id)?.context("Job missing")?;
        let Some(selector) = self.selector(job["prompt"].as_str().unwrap_or("")) else {
            return Ok("Report saved locally; no milestone".into());
        };
        let client = self.client()?;
        let snapshot = client.snapshot(&selector, &self.settings.project["github"])?;
        let Some(number) = snapshot["exit_issue"]["number"].as_i64() else {
            return Ok("Report saved locally; no exit issue".into());
        };
        Ok(format!(
            "Report published on issue #{number}: {}",
            client.comment(number, report)?
        ))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_agent_json() {
        for text in [
            r#"{"x":1}"#,
            "```json\n{\"x\":1}\n```",
            "Here is the answer: {\"x\":1} done",
        ] {
            assert_eq!(extract_json(text).unwrap()["x"], 1);
        }
    }
    #[test]
    fn rejects_bad_json() {
        assert!(extract_json("no answer").is_err());
        assert!(extract_json("} before {").is_err());
    }
}
