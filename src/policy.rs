use crate::config::Settings;
use anyhow::{Result, bail};
use serde_json::Value;
use std::path::PathBuf;
#[derive(Clone, Debug)]
pub struct Policy {
    pub base_branch: String,
    pub free_prefix: String,
    pub prefixes: Value,
    pub max_iterations: u64,
    pub max_passes_per_item: usize,
    pub max_tasks_per_item: usize,
    pub skip_labels: Vec<String>,
    pub create_integration: bool,
    pub worktrees_dir: PathBuf,
    pub on_blocked: String,
    pub on_waiting: String,
    pub brain_route: String,
    pub check_command: Option<String>,
    pub check_timeout: u64,
    pub review_enabled: bool,
    pub review_max_rounds: u64,
    pub merge_mode: String,
    pub merge_method: String,
    pub publish_mode: String,
}
fn s(v: &Value, k: &str, d: &str) -> String {
    v[k].as_str().unwrap_or(d).into()
}
impl Policy {
    pub fn new(settings: &Settings) -> Result<Self> {
        let p = &settings.project;
        let g = &p["git"];
        let l = &p["loop"];
        let r = &p["review"];
        let m = &p["merge"];
        let policy = Self {
            base_branch: s(g, "base_branch", "main"),
            free_prefix: s(g, "branch_prefix", "amaru/"),
            prefixes: g["branch_prefixes"].clone(),
            max_iterations: l["max_iterations"].as_u64().unwrap_or(20),
            max_passes_per_item: l["max_passes_per_item"].as_u64().unwrap_or(3) as usize,
            max_tasks_per_item: l["max_tasks_per_item"].as_u64().unwrap_or(8) as usize,
            skip_labels: l["skip_labels"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|s| s.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_else(|| {
                    vec![
                        "owner-task".into(),
                        "awaiting-owner-approval".into(),
                        "milestone-exit".into(),
                    ]
                }),
            create_integration: l["create_integration_branch"].as_bool().unwrap_or(true),
            worktrees_dir: settings
                .root_dir
                .join(s(l, "worktrees_dir", "data/worktrees")),
            on_blocked: s(l, "on_blocked", "stop"),
            on_waiting: s(l, "on_waiting", "stop"),
            brain_route: s(l, "brain_route", "planning"),
            check_command: p["execution"]["check_command"].as_str().map(str::to_owned),
            check_timeout: p["execution"]["check_timeout_seconds"]
                .as_u64()
                .unwrap_or(1800),
            review_enabled: r["enabled"].as_bool().unwrap_or(true),
            review_max_rounds: r["max_rounds"].as_u64().unwrap_or(1),
            merge_mode: s(m, "mode", "integration_only"),
            merge_method: s(m, "method", "squash"),
            publish_mode: s(&p["publish"], "mode", "auto"),
        };
        if !["manual", "integration_only", "always"].contains(&policy.merge_mode.as_str()) {
            bail!("invalid merge.mode");
        }
        if !["auto", "manual"].contains(&policy.publish_mode.as_str()) {
            bail!("invalid publish.mode");
        }
        if !["squash", "merge", "rebase"].contains(&policy.merge_method.as_str()) {
            bail!("invalid merge.method");
        }
        if !["stop", "brain"].contains(&policy.on_blocked.as_str())
            || !["stop", "continue"].contains(&policy.on_waiting.as_str())
        {
            bail!("invalid loop stop policy");
        }
        Ok(policy)
    }
    pub fn can_auto_merge(&self, base: &str) -> bool {
        self.merge_mode == "always"
            || (self.merge_mode == "integration_only" && base != self.base_branch)
    }
}
