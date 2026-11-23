//! Persistent delivery state machines. Agent output is advisory; software owns side effects.
use crate::{
    config::Settings,
    engine::Engine,
    github::GitHubClient,
    gitops::{Git, command_output, slugify},
    policy::Policy,
    runtime, sources,
    storage::Store,
};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    process::Command,
};
fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k].as_str().unwrap_or("")
}
fn array(v: &Value, k: &str) -> Vec<Value> {
    v[k].as_array().cloned().unwrap_or_default()
}
fn number(v: &Value, k: &str) -> i64 {
    v[k].as_i64().unwrap_or(0)
}
fn state_data(store: &Store, id: i64) -> Result<Value> {
    let row = store.job(id)?.context("job missing")?;
    Ok(job_state_data(&row))
}
fn job_state_data(row: &Value) -> Value {
    row["state_data"]
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| json!({}))
}
pub(crate) fn requires_upstream_merge(settings: &Settings, job: &Value) -> bool {
    !settings.project["git"]["fork_repository"].is_null()
        || !job_state_data(job)["fork_repository"].is_null()
}
fn save_data(store: &Store, id: i64, data: &Value) -> Result<()> {
    store.set_meta(id, &json!({"state_data":data}))
}
fn github(settings: &Settings) -> Result<GitHubClient> {
    let repo = settings.project["project"]["repository"]
        .as_str()
        .context("project.repository is required for delivery")?;
    Ok(GitHubClient::new(
        repo,
        &settings.project_root(),
        settings.project["github"]["cli"].as_str().unwrap_or("gh"),
    ))
}
fn selector(settings: &Settings, goal: &str, explicit: Option<&str>) -> Option<String> {
    explicit
        .map(str::to_owned)
        .or_else(|| {
            let v = &settings.project["github"]["milestone"];
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_i64().map(|n| n.to_string()))
        })
        .or_else(|| GitHubClient::selector_from_prompt(goal))
}
fn cancelled(store: &Store, id: Option<i64>) -> Result<bool> {
    Ok(match id {
        Some(id) => store.job(id)?.is_some_and(|j| j["status"] == "CANCELLING"),
        None => false,
    })
}
pub fn create_loop_job(
    settings: &Settings,
    store: &Store,
    goal: &str,
    milestone: Option<&str>,
    status: &str,
) -> Result<i64> {
    let id = store.create_job(&settings.project_name(), goal, status, "loop", None)?;
    store.set_meta(
        id,
        &json!({"state":"SYNC","state_data":{"selector":milestone,"backlog":[],"skipped":[]}}),
    )?;
    Ok(id)
}
fn preview_base(git: &Git, policy: &Policy, item: &Value) -> Result<String> {
    let Some(parent) = item["parent"].as_i64() else {
        return Ok(policy.base_branch.clone());
    };
    let mut found = git.remote_branches(&format!("refs/heads/*/{parent}-*"))?;
    found.sort();
    Ok(found.into_iter().next().unwrap_or_else(|| {
        let action = if policy.fork_repository.is_some() || !policy.create_integration {
            "requires upstream branch"
        } else {
            "would create"
        };
        format!(
            "({action} feat/{parent}-{})",
            slugify(item["parent_title"].as_str().unwrap_or("integration"), 40)
        )
    }))
}
pub fn dry_run(settings: &Settings, goal: &str, milestone: Option<&str>) -> Result<Value> {
    let policy = Policy::new(settings)?;
    let git = Git::new(&settings.project_root());
    let gh = github(settings)?;
    let mut result = json!({"goal":goal,"merged_prs":gh.merged_prs(5)?,"source":null,"plan":[]});
    if let Some(selector) = selector(settings, goal, milestone) {
        let obs = sources::observe(&gh, &selector, &policy)?;
        result["source"] = json!({"milestone":obs["milestone"],"closed":obs["closed"],"summary":obs["summary"],"exit_issue":obs["exit_issue"]});
        for item in array(&obs, "items") {
            let mut entry = json!({"ref":item["ref"],"title":item["title"],"eligible":item["eligible"],"reasons":item["reasons"],"parent":item["parent"]});
            if item["eligible"] == true {
                entry["branch"] = item["branch"].clone();
                entry["base"] = json!(preview_base(&git, &policy, &item)?);
            }
            result["plan"].as_array_mut().unwrap().push(entry);
        }
    }
    Ok(result)
}
pub fn should_close(data: &Value, base: &str, main: &str) -> bool {
    data["completes"] == true && base != main && data["item"]["kind"] == "issue"
}
pub fn refs_line(item: &Value, base: &str, main: &str, completes: bool) -> String {
    if item["kind"] != "issue" {
        return String::new();
    }
    let gated = sources::label_names(item)
        .iter()
        .any(|l| l == "awaiting-owner-approval");
    format!(
        "{} #{}",
        if completes && base == main && !gated {
            "Closes"
        } else {
            "Refs"
        },
        item["number"]
    )
}
pub fn run_check(command: &str, cwd: &Path, timeout: u64) -> Value {
    let Some(args) = shlex::split(command) else {
        return json!({"command":command,"returncode":127,"tail":"invalid command quoting"});
    };
    let Some((program, args)) = args.split_first() else {
        return json!({"command":command,"returncode":127,"tail":"empty check command"});
    };
    match command_output(Command::new(program).args(args).current_dir(cwd), timeout) {
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let tail: String = text
                .chars()
                .rev()
                .take(4000)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
            json!({"command":command,"returncode":out.status.code().unwrap_or(1),"tail":tail})
        }
        Err(e) => {
            json!({"command":command,"returncode":if e.to_string().contains("timed out"){124}else{127},"tail":e.to_string()})
        }
    }
}
struct Work<'a> {
    settings: &'a Settings,
    store: &'a Store,
    id: i64,
    loop_id: Option<i64>,
    policy: Policy,
    git: Git,
    gh: GitHubClient,
    data: Value,
}
impl<'a> Work<'a> {
    fn new(
        settings: &'a Settings,
        store: &'a Store,
        id: i64,
        loop_id: Option<i64>,
    ) -> Result<Self> {
        let policy = Policy::new(settings)?;
        let data = state_data(store, id)?;
        if data["fork_repository"] != json!(policy.fork_repository) {
            bail!(
                "The work item's fork repository differs from git.fork_repository. Restore its original configuration or start a new loop"
            );
        }
        Ok(Self {
            settings,
            store,
            id,
            loop_id,
            policy,
            git: Git::new(&settings.project_root()),
            gh: github(settings)?,
            data,
        })
    }
    fn save(&self) -> Result<()> {
        save_data(self.store, self.id, &self.data)
    }
    fn wt(&self) -> PathBuf {
        PathBuf::from(s(&self.data, "worktree"))
    }
    fn engine(&self) -> Result<Engine<'_>> {
        Engine::new(self.settings, self.store, Some(self.wt()), "work")
    }
    fn context(&self) -> String {
        format!(
            "WORK ITEM {}: {}\n{}\n{}\nBRANCH: {} (base {}).\nPRs ALREADY MERGED FOR THIS ITEM (do not redo): {}\nScope: ONLY this work item. Meet acceptance criteria, update docs, run checks. Commit on this branch following AGENTS.md. Never push or merge.",
            self.data["item"]["ref"],
            s(&self.data["item"], "title"),
            s(&self.data["item"], "url"),
            s(&self.data["item"], "body"),
            s(&self.data, "branch"),
            s(&self.data, "base"),
            self.data["item"]["prior_prs"]
        )
    }
    fn check_stop(&self) -> Result<()> {
        if cancelled(self.store, self.loop_id)? {
            bail!("CANCELLED");
        }
        Ok(())
    }
    fn fix(&self, title: &str, description: &str, kind: &str) -> Result<()> {
        self.check_stop()?;
        self.store.add_tasks(
            self.id,
            &[json!({"title":title,"description":description,"type":kind})],
        )?;
        let failures = runtime::run_pending_tasks(
            self.settings,
            self.store,
            &self.engine()?,
            self.id,
            &self.context(),
        )?;
        if !failures.is_empty() {
            bail!("BLOCKED: correction failed: {}", failures.join("; "));
        }
        Ok(())
    }
    fn commit_leftovers(&self) -> Result<()> {
        if self.git.is_clean(&self.wt())? {
            return Ok(());
        }
        self.fix(
            "Commit pending changes",
            "Review and commit pending changes following AGENTS.md. Do not push.",
            "simple_implementation",
        )?;
        if !self.git.is_clean(&self.wt())? {
            bail!("BLOCKED: uncommitted changes remain after requesting a commit");
        }
        Ok(())
    }
    fn work_branch(&self, item: &Value) -> Result<String> {
        let mut branch = item["branch"]
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                format!(
                    "{}{}",
                    self.policy.free_prefix,
                    slugify(s(item, "title"), 40)
                )
            });
        if let Some(fork) = self.policy.fork_repository.as_deref() {
            self.gh.verify_fork(fork)?;
            if let Some(saved) = self.data["branch"].as_str() {
                branch = saved.to_owned();
            } else {
                let fork_url = format!("https://github.com/{fork}.git");
                let stem = branch.clone();
                let mut suffix = 2;
                while self.git.has_local_branch(&branch)?
                    || !self
                        .git
                        .remote_branches(&format!("refs/heads/{branch}"))?
                        .is_empty()
                    || !self
                        .git
                        .remote_branches_at(&fork_url, &format!("refs/heads/{branch}"))?
                        .is_empty()
                {
                    branch = format!("{stem}-{suffix}");
                    suffix += 1;
                }
            }
        }
        Ok(branch)
    }
    fn prepare(&mut self) -> Result<String> {
        if self.git.has_remote()? {
            self.git.fetch()?;
        }
        let item = &self.data["item"];
        let base = if !s(&self.data, "base").is_empty() {
            s(&self.data, "base").to_owned()
        } else if let Some(parent) = item["parent"].as_i64() {
            let mut found = self
                .git
                .remote_branches(&format!("refs/heads/*/{parent}-*"))?;
            found.sort();
            if let Some(found) = found.into_iter().next() {
                found
            } else {
                if self.policy.fork_repository.is_some() {
                    bail!(
                        "BLOCKED: ask an upstream maintainer to create the integration branch for issue #{parent}"
                    );
                }
                if !self.policy.create_integration {
                    bail!("BLOCKED: parent integration branch missing");
                }
                let name = format!(
                    "feat/{parent}-{}",
                    slugify(item["parent_title"].as_str().unwrap_or("integration"), 40)
                );
                self.git
                    .create_remote_branch(&name, &format!("origin/{}", self.policy.base_branch))?;
                self.git.fetch()?;
                self.store.event(
                    self.id,
                    "INTEGRATION_BRANCH_CREATED",
                    &json!({"branch":name,"from":self.policy.base_branch}),
                )?;
                name
            }
        } else {
            self.policy.base_branch.clone()
        };
        let branch = self.work_branch(item)?;
        let root = self.settings.project_root();
        let repo = root.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
        let wt = self
            .policy
            .worktrees_dir
            .join(format!("{repo}-{}", slugify(&branch, 60)));
        self.git
            .add_worktree(&wt, &branch, &format!("origin/{base}"))?;
        self.data["base"] = json!(base);
        self.data["branch"] = json!(branch);
        self.data["worktree"] = json!(wt.to_string_lossy());
        self.save()?;
        self.store
            .set_meta(self.id, &json!({"branch":branch,"base_branch":base}))?;
        self.store.event(
            self.id,
            "WORK_PREPARED",
            &json!({"branch":branch,"base":base,"worktree":wt.to_string_lossy()}),
        )?;
        Ok("EXECUTE".into())
    }
    fn execute(&self) -> Result<String> {
        self.check_stop()?;
        for t in self.store.tasks(self.id)? {
            if t["status"] == "RUNNING" {
                self.store
                    .update_task(number(&t, "id"), "PENDING", None, None, false)?;
            }
        }
        let engine = self.engine()?;
        let failures = if self.store.tasks(self.id)?.is_empty() {
            runtime::execute_plan(
                self.settings,
                self.store,
                &engine,
                self.id,
                &format!(
                    "{}\n\n{}",
                    s(&self.data["item"], "title"),
                    s(&self.data["item"], "body")
                ),
                false,
                Some(self.policy.max_tasks_per_item),
                "Plan only this single work item, not the entire milestone.",
                &self.context(),
            )?
            .1
        } else {
            runtime::run_pending_tasks(
                self.settings,
                self.store,
                &engine,
                self.id,
                &self.context(),
            )?
        };
        if !failures.is_empty()
            || self
                .store
                .tasks(self.id)?
                .iter()
                .any(|t| t["status"] != "COMPLETED")
        {
            bail!("BLOCKED: work item tasks failed: {}", failures.join("; "));
        }
        Ok("VERIFY".into())
    }
    fn verify(&mut self) -> Result<String> {
        self.commit_leftovers()?;
        let wt = self.wt();
        let base = format!("origin/{}", s(&self.data, "base"));
        if self.git.ahead_count(&base, &wt)? == 0 {
            bail!("BLOCKED: agent produced no commit");
        }
        let mut check = Value::Null;
        if let Some(command) = &self.policy.check_command {
            check = run_check(command, &wt, self.policy.check_timeout);
            if check["returncode"] != 0 {
                self.fix("Fix failing project check",&format!("`{command}` failed: {}. Fix the cause, not the check; commit the correction.",check["tail"]),"debugging")?;
                self.commit_leftovers()?;
                check = run_check(command, &wt, self.policy.check_timeout);
                if check["returncode"] != 0 {
                    bail!("BLOCKED: project check still fails: {}", check["tail"]);
                }
            }
        }
        self.data["check"] = check;
        self.save()?;
        self.store.event(
            self.id,
            "VERIFIED",
            &json!({"check":self.data["check"],"commits":self.git.commits(&base,&wt)?}),
        )?;
        if self.policy.publish_mode == "manual" && self.data["publish_approved"] != true {
            self.store.set_meta(self.id, &json!({"state":"PUBLISH"}))?;
            return Ok("WAITING_PR".into());
        }
        Ok("PUBLISH".into())
    }
    fn pr_text(&self) -> Result<Value> {
        let item = &self.data["item"];
        let base = format!("origin/{}", s(&self.data, "base"));
        let prompt = format!(
            "Write concise professional PR text. Return ONLY JSON {{\"title\":\"conventional commit style <=90 chars\",\"outcome\":\"problem and resulting behavior\",\"verification\":\"actual evidence only\",\"integration\":\"remaining work\",\"completes_issue\":false}}. completes_issue true ONLY if EVERY acceptance criterion is met.\nWORK ITEM: {}\nPRIOR PRS: {}\nCOMMITS: {}\nDIFF STAT: {}\nCHECK: {}",
            item,
            item["prior_prs"],
            self.git.commits(&base, &self.wt())?,
            self.git.diff_stat(&base, &self.wt())?,
            self.data["check"]
        );
        let fallback = json!({"title":format!("feat: {}",s(item,"title")),"outcome":s(item,"title"),"verification":"Only the recorded check result was validated.","integration":"See the linked work item for remaining criteria.","completes_issue":false});
        let mut meta = match self.engine()?.call_json("summarization", &prompt, self.id) {
            Ok(v) if v.is_object() => v,
            Ok(_) => fallback.clone(),
            Err(e) => {
                self.store
                    .event(self.id, "PR_TEXT_FALLBACK", &json!({"error":e.to_string()}))?;
                fallback.clone()
            }
        };
        for key in ["title", "outcome", "verification", "integration"] {
            if s(&meta, key).trim().is_empty() {
                meta[key] = fallback[key].clone();
            }
        }
        meta["title"] = json!(
            s(&meta, "title")
                .lines()
                .next()
                .unwrap_or("feat: implementation")
                .chars()
                .take(100)
                .collect::<String>()
        );
        Ok(meta)
    }
    fn publish(&mut self) -> Result<String> {
        let wt = self.wt();
        let branch = s(&self.data, "branch");
        let base = s(&self.data, "base");
        if let Some(fork) = self.policy.fork_repository.as_deref() {
            self.gh.verify_fork(fork)?;
        }
        self.push_branch(&wt)?;
        let existing = if let Some(fork) = self.policy.fork_repository.as_deref() {
            self.gh.fork_pr_for_head(fork, branch, base)?
        } else {
            self.gh.pr_for_head(branch)?
        };
        if let Some(pr) = existing.filter(|p| p["state"] == "OPEN" || p["state"] == "MERGED") {
            self.data["pr"] =
                json!({"number":pr["number"],"url":pr["url"],"merged":pr["state"]=="MERGED"});
        } else {
            let meta = self.pr_text()?;
            let completes = meta["completes_issue"] == true;
            self.data["completes"] = json!(completes);
            let evidence = if self.data["check"].is_null() {
                "- check_command not configured: **not validated** by Mini-Amaru.".into()
            } else {
                format!(
                    "- `{}` in the issue worktree: exit {} (local run, not CI).",
                    s(&self.data["check"], "command"),
                    self.data["check"]["returncode"]
                )
            };
            let body = format!(
                "## Outcome\n\n{}\n\n{}\n\n## Verification\n\n{evidence}\n\n{}\n\n## Integration and remaining work\n\nBase: `{}`. {}\n\n_Opened by Mini-Amaru (job #{})._\n",
                s(&meta, "outcome"),
                refs_line(
                    &self.data["item"],
                    s(&self.data, "base"),
                    &self.policy.base_branch,
                    completes
                ),
                s(&meta, "verification"),
                s(&self.data, "base"),
                s(&meta, "integration"),
                self.id
            );
            let title = s(&meta, "title");
            let base = s(&self.data, "base");
            let branch = s(&self.data, "branch");
            let output = if let Some(fork) = self.policy.fork_repository.as_deref() {
                self.gh.create_fork_pr(base, fork, branch, title, &body)?
            } else {
                self.gh.create_pr(base, branch, title, &body)?
            };
            let url = output.lines().last().context("empty PR URL")?.trim();
            let n: i64 = url
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .context("PR number")?
                .parse()?;
            self.data["pr"] = json!({"number":n,"url":url});
            self.store.event(self.id, "PR_OPENED", &self.data["pr"])?;
        }
        self.save()?;
        self.store.set_meta(
            self.id,
            &json!({"pr_number":self.data["pr"]["number"],"pr_url":self.data["pr"]["url"]}),
        )?;
        Ok(
            if self.data["pr"]["merged"] == true || !self.policy.review_enabled {
                "MERGE"
            } else {
                "REVIEW"
            }
            .into(),
        )
    }
    fn review(&mut self) -> Result<String> {
        let rounds = self.data["review"]["rounds"].as_u64().unwrap_or(0);
        let diff = self.git.out(
            &["diff", &format!("origin/{}...HEAD", s(&self.data, "base"))],
            Some(&self.wt()),
        )?;
        let prompt = format!(
            "You are an independent code reviewer. Do NOT modify files or make commits. Read AGENTS.md. Return ONLY JSON {{\"verdict\":\"APPROVE\"|\"REQUEST_CHANGES\",\"summary\":\"concrete assessment\",\"findings\":[\"actionable problem\"],\"completes_issue\":false}}. Completes only when every acceptance criterion is met.\nITEM: {}\nCHECK: {}\nDIFF:\n{}",
            self.data["item"],
            self.data["check"],
            diff.chars().take(120000).collect::<String>()
        );
        let mut verdict = None;
        let before = self.git.out(&["rev-parse", "HEAD"], Some(&self.wt()))?;
        for _ in 0..2 {
            match self.engine()?.call_json("code_review", &prompt, self.id) {
                Ok(v) if ["APPROVE", "REQUEST_CHANGES"].contains(&s(&v, "verdict")) => {
                    verdict = Some(v);
                    break;
                }
                other => {
                    self.store.event(
                        self.id,
                        "REVIEW_UNREADABLE",
                        &json!({"response":format!("{other:?}")}),
                    )?;
                }
            }
        }
        let verdict = verdict.context("BLOCKED: reviewer did not return valid verdict")?;
        if !self.git.is_clean(&self.wt())?
            || before != self.git.out(&["rev-parse", "HEAD"], Some(&self.wt()))?
        {
            bail!("BLOCKED: reviewer modified the worktree or HEAD");
        }
        let findings = array(&verdict, "findings");
        let message = format!(
            "**Mini-Amaru review: {}** (round {})\n\n{}\n\nFindings:\n{}",
            s(&verdict, "verdict"),
            rounds + 1,
            s(&verdict, "summary"),
            findings
                .iter()
                .map(|v| format!("- {}", v.as_str().unwrap_or("invalid finding")))
                .collect::<Vec<_>>()
                .join("\n")
        );
        self.gh
            .pr_comment(number(&self.data["pr"], "number"), &message)?;
        self.data["review"] = json!({"rounds":rounds,"verdict":verdict["verdict"],"message":message,"findings":findings});
        if verdict["completes_issue"] == false {
            self.data["completes"] = json!(false);
        }
        self.save()?;
        self.store.set_meta(
            self.id,
            &json!({"review_verdict":verdict["verdict"],"review_message":message}),
        )?;
        self.store.event(
            self.id,
            "REVIEWED",
            &json!({"verdict":verdict["verdict"],"findings":findings}),
        )?;
        if verdict["verdict"] == "APPROVE" {
            return Ok("MERGE".into());
        }
        if rounds >= self.policy.review_max_rounds {
            bail!("BLOCKED: review correction limit reached");
        }
        self.data["review"]["rounds"] = json!(rounds + 1);
        self.save()?;
        self.fix(
            "Address review findings",
            &format!("Fix these concrete review findings and commit; never push: {findings:?}"),
            "debugging",
        )?;
        self.commit_leftovers()?;
        if let Some(command) = &self.policy.check_command {
            let check = run_check(command, &self.wt(), self.policy.check_timeout);
            if check["returncode"] != 0 {
                bail!("BLOCKED: checks fail after review corrections");
            }
            self.data["check"] = check;
        }
        self.push_branch(&self.wt())?;
        self.save()?;
        Ok("REVIEW".into())
    }
    fn push_branch(&self, worktree: &Path) -> Result<()> {
        let branch = s(&self.data, "branch");
        if let Some(fork) = self.policy.fork_repository.as_deref() {
            self.git
                .push_to_fork(&format!("https://github.com/{fork}.git"), branch, worktree)
        } else {
            self.git.push(branch, worktree)
        }
    }
    fn merge(&mut self) -> Result<String> {
        let n = number(&self.data["pr"], "number");
        let mut merged = self.data["pr"]["merged"] == true;
        if !merged && self.policy.review_enabled {
            if self.data["review"]["verdict"] != "APPROVE" {
                bail!("BLOCKED: no approving review");
            }
            if self.policy.can_auto_merge(s(&self.data, "base")) {
                let checks = self.gh.pr_checks(n)?;
                if checks == "fail" {
                    bail!("BLOCKED: PR checks failed");
                }
                if checks != "pending" {
                    self.gh.merge_pr(n, &self.policy.merge_method)?;
                    merged = true;
                }
            }
        }
        let state = if merged { "MERGED" } else { "WAITING" };
        self.data["merge_state"] = json!(state);
        self.save()?;
        self.store
            .set_meta(self.id, &json!({"merge_state":state}))?;
        self.store.event(
            self.id,
            &format!("MERGE_{state}"),
            &json!({"pr":self.data["pr"]["url"],"base":self.data["base"]}),
        )?;
        if merged {
            if self.policy.fork_repository.is_none()
                && should_close(&self.data, s(&self.data, "base"), &self.policy.base_branch)
                && let Err(e) = self.gh.close_issue(
                    number(&self.data["item"], "number"),
                    &format!(
                        "Merged in {} into `{}`.",
                        s(&self.data["pr"], "url"),
                        s(&self.data, "base")
                    ),
                )
            {
                self.store.event(
                    self.id,
                    "ISSUE_CLOSE_FAILED",
                    &json!({"error":e.to_string()}),
                )?;
            }
            if self.git.has_remote()? {
                self.git.fetch()?;
            }
        }
        Ok(if merged { "COMPLETED" } else { "WAITING_MERGE" }.into())
    }
    fn run(&mut self) -> Result<String> {
        let row = self.store.job(self.id)?.context("work job missing")?;
        let mut state = row["state"].as_str().unwrap_or("PREPARE").to_owned();
        let mut note = String::new();
        loop {
            if [
                "COMPLETED",
                "WAITING_MERGE",
                "WAITING_PR",
                "BLOCKED",
                "FAILED",
                "CANCELLED",
            ]
            .contains(&state.as_str())
            {
                break;
            }
            let result = (|| {
                self.check_stop()?;
                self.store
                    .event(self.id, "STATE", &json!({"state":state}))?;
                match state.as_str() {
                    "PREPARE" => self.prepare(),
                    "EXECUTE" => self.execute(),
                    "VERIFY" => self.verify(),
                    "PUBLISH" => self.publish(),
                    "REVIEW" => self.review(),
                    "MERGE" => self.merge(),
                    _ => bail!("unknown work state {state}"),
                }
            })();
            match result {
                Ok(next) => {
                    if next != "WAITING_PR" {
                        self.store.set_meta(self.id, &json!({"state":next}))?;
                    }
                    state = next;
                }
                Err(e) => {
                    note = e.to_string();
                    state = if note.eq_ignore_ascii_case("cancelled")
                        || cancelled(self.store, self.loop_id)?
                    {
                        "CANCELLED"
                    } else if note.contains("BLOCKED:") {
                        "BLOCKED"
                    } else {
                        "FAILED"
                    }
                    .into();
                    break;
                }
            }
        }
        let report = format!(
            "# Work item {} — {state}\n\n**Title:** {}\n**Branch:** {} → {}\n**PR:** {}\n**Review:** {}\n**Merge:** {}\n**Check:** {}\n\n{note}",
            self.data["item"]["ref"],
            s(&self.data["item"], "title"),
            s(&self.data, "branch"),
            s(&self.data, "base"),
            s(&self.data["pr"], "url"),
            s(&self.data["review"], "verdict"),
            s(&self.data, "merge_state"),
            self.data["check"]
        );
        self.store.set_job(self.id, &state, None, Some(&report))?;
        if ["COMPLETED", "FAILED", "CANCELLED", "WAITING_MERGE"].contains(&state.as_str()) {
            self.store.set_meta(self.id, &json!({"state":state}))?;
        }
        self.store.event(
            self.id,
            &format!("WORK_{state}"),
            &json!({"note":note,"pr":self.data["pr"]["url"]}),
        )?;
        if ["COMPLETED", "WAITING_MERGE", "WAITING_PR"].contains(&state.as_str())
            && !s(&self.data, "worktree").is_empty()
        {
            self.git.remove_worktree(&self.wt())?;
        }
        Ok(state)
    }
}
struct DeliveryLoop<'a> {
    settings: &'a Settings,
    store: &'a Store,
    id: i64,
    goal: String,
    policy: Policy,
    git: Git,
    gh: GitHubClient,
    data: Value,
    obs: Value,
    recent: Vec<Value>,
    outcome: String,
}
impl<'a> DeliveryLoop<'a> {
    fn new(settings: &'a Settings, store: &'a Store, id: i64) -> Result<Self> {
        let row = store.job(id)?.context("loop missing")?;
        let mut data = state_data(store, id)?;
        for key in ["backlog", "skipped"] {
            if !data[key].is_array() {
                data[key] = json!([]);
            }
        }
        let obs = data["observation"].clone();
        Ok(Self {
            settings,
            store,
            id,
            goal: s(&row, "prompt").into(),
            policy: Policy::new(settings)?,
            git: Git::new(&settings.project_root()),
            gh: github(settings)?,
            data,
            obs,
            recent: Vec::new(),
            outcome: String::new(),
        })
    }
    fn save(&self) -> Result<()> {
        save_data(self.store, self.id, &self.data)
    }
    fn sync(&mut self) -> Result<String> {
        crate::workspace::ensure_project(self.settings)?;
        if self.git.has_remote()? {
            self.git.fetch()?;
        }
        self.recent = self.gh.merged_prs(10)?;
        for child in self.store.children(self.id)? {
            if child["status"] != "WAITING_MERGE" || child["pr_number"].is_null() {
                continue;
            }
            let pr = self.gh.pr_view(number(&child, "pr_number"))?;
            let cid = number(&child, "id");
            match s(&pr, "state") {
                "MERGED" => {
                    let d = state_data(self.store, cid)?;
                    if self.policy.fork_repository.is_none()
                        && d["fork_repository"].is_null()
                        && should_close(&d, s(&child, "base_branch"), &self.policy.base_branch)
                    {
                        let _ = self.gh.close_issue(
                            number(&child, "issue_number"),
                            &format!(
                                "Merged in {} into `{}`.",
                                s(&pr, "url"),
                                s(&child, "base_branch")
                            ),
                        );
                    }
                    self.store.set_job(cid, "COMPLETED", None, None)?;
                    self.store
                        .set_meta(cid, &json!({"merge_state":"MERGED","state":"COMPLETED"}))?;
                    self.store.event(
                        self.id,
                        "PR_MERGED_EXTERNALLY",
                        &json!({"child":cid,"pr":pr["url"]}),
                    )?;
                }
                "CLOSED" => {
                    self.store
                        .set_job(cid, "BLOCKED", None, Some("PR closed without merge"))?;
                    self.store
                        .set_meta(cid, &json!({"merge_state":"CLOSED","state":"BLOCKED"}))?;
                    self.store.event(
                        self.id,
                        "PR_CLOSED_UNMERGED",
                        &json!({"child":cid,"pr":pr["url"]}),
                    )?;
                }
                _ => {}
            }
        }
        Ok("OBSERVE".into())
    }
    fn source_selector(&self) -> Option<String> {
        selector(self.settings, &self.goal, self.data["selector"].as_str())
    }
    fn observe(&mut self) -> Result<String> {
        let children = self.store.children(self.id)?;
        let done: HashSet<String> = children
            .iter()
            .filter(|c| {
                ["WAITING_MERGE", "WAITING_PR"].contains(&s(c, "status"))
                    || (c["status"] == "COMPLETED" && s(c, "ref").starts_with("task#"))
            })
            .map(|c| s(c, "ref").to_owned())
            .collect();
        let mut obs = if let Some(sel) = self.source_selector() {
            sources::observe(&self.gh, &sel, &self.policy)?
        } else {
            let items: Vec<Value> = array(&self.data, "backlog")
                .into_iter()
                .map(|mut i| {
                    let eligible = !done.contains(s(&i, "ref"));
                    i["eligible"] = json!(eligible);
                    i["reasons"] = if eligible { json!([]) } else { json!(["done"]) };
                    i
                })
                .collect();
            let n = items.iter().filter(|i| i["eligible"] == true).count();
            json!({"source":"free_backlog","milestone":null,"closed":items.len()-n,"items":items,"summary":{"open":n,"actionable":n,"eligible":n,"blocked":0,"gated":0,"waiting_pr":0,"parents_ready_for_integration":[]}})
        };
        let mut passes: HashMap<String, usize> = HashMap::new();
        for c in &children {
            *passes.entry(s(c, "ref").to_owned()).or_default() += 1;
        }
        let waiting: HashSet<String> = children
            .iter()
            .filter(|c| ["WAITING_MERGE", "WAITING_PR"].contains(&s(c, "status")))
            .map(|c| s(c, "ref").to_owned())
            .collect();
        let skipped: HashSet<String> = array(&self.data, "skipped")
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        for item in obs["items"]
            .as_array_mut()
            .context("observation items missing")?
        {
            if item["eligible"] != true {
                continue;
            }
            let rf = s(item, "ref").to_owned();
            let reason = if waiting.contains(&rf) {
                Some("awaiting_human_in_loop".into())
            } else if item["kind"] == "free" && done.contains(&rf) {
                Some("already_done_in_loop".into())
            } else if passes.get(&rf).copied().unwrap_or(0) >= self.policy.max_passes_per_item {
                Some(format!("pass_limit:{}", passes[&rf]))
            } else if skipped.contains(&rf) {
                Some("skipped_by_decision".into())
            } else {
                None
            };
            if let Some(reason) = reason {
                item["eligible"] = json!(false);
                item["reasons"]
                    .as_array_mut()
                    .context("missing reasons")?
                    .push(json!(reason));
            }
        }
        let eligible: Vec<_> = array(&obs, "items")
            .into_iter()
            .filter(|i| i["eligible"] == true)
            .map(|i| i["ref"].clone())
            .collect();
        obs["summary"]["eligible"] = json!(eligible.len());
        self.store.event(
            self.id,
            "OBSERVED",
            &json!({"summary":obs["summary"],"milestone":obs["milestone"],"eligible":eligible}),
        )?;
        self.data["observation"] = obs.clone();
        self.obs = obs;
        self.save()?;
        Ok("DECIDE".into())
    }
    fn facts(&self) -> Result<Value> {
        let kids:Vec<Value>=self.store.children(self.id)?.into_iter().map(|c|json!({"ref":c["ref"],"status":c["status"],"pr":c["pr_url"],"review":c["review_verdict"],"merge":c["merge_state"]})).collect();
        let items = array(&self.obs, "items");
        Ok(
            json!({"source":self.obs["source"],"milestone":self.obs["milestone"],"summary":self.obs["summary"],"eligible":items.iter().filter(|i|i["eligible"]==true).collect::<Vec<_>>(),"not_eligible":items.iter().filter(|i|i["eligible"]!=true).collect::<Vec<_>>(),"waiting_merge":kids.iter().filter(|k|k["status"]=="WAITING_MERGE").collect::<Vec<_>>(),"history":kids.iter().rev().take(8).collect::<Vec<_>>(),"recent_merged_prs":self.recent.iter().take(5).collect::<Vec<_>>(),"iteration":self.store.job(self.id)?.context("loop missing")?["iteration"],"max_iterations":self.policy.max_iterations}),
        )
    }
    fn decide(&mut self) -> Result<String> {
        let row = self.store.job(self.id)?.context("loop missing")?;
        if row["iteration"].as_u64().unwrap_or(0) >= self.policy.max_iterations {
            self.outcome = format!("iteration limit {} reached", self.policy.max_iterations);
            return Ok("STOPPED".into());
        }
        if self.obs.is_null() {
            self.observe()?;
        }
        let items = array(&self.obs, "items");
        let eligible: HashSet<String> = items
            .iter()
            .filter(|i| i["eligible"] == true)
            .map(|i| s(i, "ref").to_owned())
            .collect();
        let mut allowed = json!({"wait":{"description":"Pause until a human resolves a gate or merges a PR."},"stop":{"description":"Stop because progress is blocked or unsafe."}});
        if !eligible.is_empty() {
            allowed["start_work"] =
                json!({"description":"Run ONE eligible item.","args":{"ref":"one eligible ref"}});
        }
        let free = self.source_selector().is_none();
        if free && eligible.is_empty() {
            allowed["plan_items"] = json!({"description":"Create 1-8 bounded work items for the goal.","args":{"items":"[{title,description}]"}});
        }
        if self.obs["summary"]["actionable"] == 0 {
            allowed["finish"] = json!({"description":"Goal complete, no actionable work remains."});
        }
        let kids_waiting = self
            .store
            .children(self.id)?
            .iter()
            .any(|c| ["WAITING_MERGE", "WAITING_PR"].contains(&s(c, "status")));
        let fallback = if let Some(item) = items.iter().find(|i| i["eligible"] == true) {
            json!({"action":"start_work","args":{"ref":item["ref"]},"reason":"First eligible item in source order."})
        } else if !allowed["finish"].is_null() {
            json!({"action":"finish","args":{},"reason":"No actionable work remains."})
        } else if free {
            json!({"action":"stop","args":{},"reason":"No external source and brain unavailable."})
        } else {
            json!({"action":if kids_waiting||self.obs["summary"]["waiting_pr"].as_u64().unwrap_or(0)>0{"wait"}else{"stop"},"args":{},"reason":"No eligible item currently."})
        };
        let engine = Engine::new(self.settings, self.store, None, "job")?;
        let decision = crate::brain::decide(
            &self.goal,
            &self.facts()?,
            &allowed,
            |p| engine.call_json(&self.policy.brain_route, p, self.id),
            |d| {
                match s(d, "action") {
                    "start_work" => {
                        if !eligible.contains(s(&d["args"], "ref")) {
                            bail!("ref is not eligible");
                        }
                    }
                    "plan_items" => {
                        let list = d["args"]["items"]
                            .as_array()
                            .context("items must be array")?;
                        if list.is_empty()
                            || list.len() > 8
                            || list.iter().any(|i| s(i, "title").trim().is_empty())
                        {
                            bail!("expected 1-8 items with title");
                        }
                    }
                    _ => {}
                }
                Ok(())
            },
            fallback,
        );
        self.store.event(self.id, "BRAIN_DECISION", &decision)?;
        match s(&decision, "action") {
            "start_work" => {
                self.data["current"] = decision["args"]["ref"].clone();
                self.save()?;
                Ok("WORK".into())
            }
            "plan_items" => {
                let start = array(&self.data, "backlog").len();
                for (index, item) in array(&decision["args"], "items").into_iter().enumerate() {
                    self.data["backlog"].as_array_mut().unwrap().push(json!({"ref":format!("task#{}",start+index+1),"kind":"free","number":null,"title":item["title"],"body":item["description"].as_str().unwrap_or(""),"labels":[],"parent":null,"is_parent":false,"branch":null}));
                }
                self.save()?;
                Ok("OBSERVE".into())
            }
            action => {
                self.outcome = s(&decision, "reason").into();
                Ok(match action {
                    "wait" => "WAITING",
                    "finish" => "DONE",
                    _ => "BLOCKED",
                }
                .into())
            }
        }
    }
    fn work(&mut self) -> Result<String> {
        let rf = s(&self.data, "current").to_owned();
        if self.obs.is_null() {
            self.observe()?;
        }
        let item = array(&self.obs, "items")
            .into_iter()
            .find(|i| i["ref"] == rf)
            .context("current item missing from observation")?;
        let child = self.store.children(self.id)?.into_iter().find(|c| {
            c["ref"] == rf
                && ![
                    "COMPLETED",
                    "WAITING_MERGE",
                    "WAITING_PR",
                    "BLOCKED",
                    "FAILED",
                    "CANCELLED",
                ]
                .contains(&s(c, "status"))
        });
        let cid = if let Some(c) = child {
            number(&c, "id")
        } else {
            let cid = self.store.create_job(
                &self.settings.project_name(),
                &format!("{rf}: {}", s(&item, "title")),
                "RUNNING",
                "work",
                Some(self.id),
            )?;
            self.store.set_meta(cid,&json!({"ref":rf,"issue_number":item["number"],"state":"PREPARE","state_data":{"item":item,"fork_repository":self.policy.fork_repository}}))?;
            let row = self.store.job(self.id)?.context("loop missing")?;
            self.store
                .set_meta(self.id, &json!({"iteration":number(&row,"iteration")+1}))?;
            cid
        };
        self.store
            .event(self.id, "WORK_STARTED", &json!({"ref":rf,"child":cid}))?;
        let status = Work::new(self.settings, self.store, cid, Some(self.id))?.run()?;
        if let Some(data) = self.data.as_object_mut() {
            data.remove("current");
        }
        self.save()?;
        match status.as_str() {
            "COMPLETED" => Ok("SYNC".into()),
            "WAITING_MERGE" | "WAITING_PR" => {
                if self.policy.on_waiting == "continue" {
                    Ok("SYNC".into())
                } else {
                    self.outcome = format!("{rf} awaits human action ({status})");
                    Ok("WAITING".into())
                }
            }
            "CANCELLED" => Ok("CANCELLED".into()),
            _ => {
                let report = self.store.job(cid)?.context("work missing")?["report"].clone();
                if self.policy.on_blocked == "brain" {
                    let engine = Engine::new(self.settings, self.store, None, "job")?;
                    let d = crate::brain::decide(
                        &self.goal,
                        &json!({"blocked_item":rf,"report":report,"facts":self.facts()?}),
                        &json!({"skip_item":{"description":"Skip item and continue"},"stop":{"description":"Stop for human attention"}}),
                        |p| engine.call_json(&self.policy.brain_route, p, self.id),
                        |_| Ok(()),
                        json!({"action":"stop","args":{},"reason":"Blocked item: safe default is to stop."}),
                    );
                    self.store.event(self.id, "BRAIN_DECISION", &d)?;
                    if d["action"] == "skip_item" {
                        self.data["skipped"].as_array_mut().unwrap().push(json!(rf));
                        self.save()?;
                        return Ok("SYNC".into());
                    }
                }
                self.outcome = format!("{rf} {status}: {report}");
                Ok("BLOCKED".into())
            }
        }
    }
    fn finish(&self, state: &str) -> Result<()> {
        let status = match state {
            "DONE" => "COMPLETED",
            "WAITING" => "WAITING_MERGE",
            other => other,
        };
        let rows = self
            .store
            .children(self.id)?
            .iter()
            .map(|c| {
                format!(
                    "| {} | #{} | {} | {} | {} | {} |",
                    s(c, "ref"),
                    c["id"],
                    s(c, "status"),
                    s(c, "pr_url"),
                    s(c, "review_verdict"),
                    s(c, "merge_state")
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let report = format!(
            "# Mini-Amaru loop report — Job {}\n\n**Goal:** {}\n**End state:** {state}\n**Reason:** {}\n\n## Work items\n| ref | job | status | PR | review | merge |\n|---|---|---|---|---|---|\n{rows}\n\n## Last observation\n{}\n",
            self.id, self.goal, self.outcome, self.obs["summary"]
        );
        self.store.set_job(self.id, status, None, Some(&report))?;
        self.store.set_meta(self.id, &json!({"state":state}))?;
        std::fs::create_dir_all(self.settings.artifacts_dir())?;
        let path = self
            .settings
            .artifacts_dir()
            .join(format!("job-{}-report.md", self.id));
        std::fs::write(&path, report)?;
        self.store
            .artifact(self.id, "final_report", &path.to_string_lossy())?;
        self.store.event(
            self.id,
            "LOOP_FINISHED",
            &json!({"state":state,"outcome":self.outcome}),
        )?;
        Ok(())
    }
    fn run(&mut self) -> Result<()> {
        let row = self.store.job(self.id)?.context("loop missing")?;
        if cancelled(self.store, Some(self.id))? {
            self.outcome = "Cancelled before start".into();
            return self.finish("CANCELLED");
        }
        let resumed = number(&row, "iteration") > 0;
        let state = s(&row, "state");
        let mut state = if ["SYNC", "OBSERVE", "DECIDE", "WORK"].contains(&state) {
            state.to_owned()
        } else {
            "SYNC".into()
        };
        self.store.set_job(self.id, "RUNNING", None, None)?;
        self.store.event(
            self.id,
            "LOOP_STARTED",
            &json!({"goal":self.goal,"selector":self.data["selector"],"resumed":resumed}),
        )?;
        loop {
            if [
                "DONE",
                "WAITING",
                "STOPPED",
                "BLOCKED",
                "CANCELLED",
                "FAILED",
            ]
            .contains(&state.as_str())
            {
                break;
            }
            if cancelled(self.store, Some(self.id))? {
                state = "CANCELLED".into();
                self.outcome = "Cancelled by user".into();
                break;
            }
            self.store
                .event(self.id, "STATE", &json!({"state":state}))?;
            let next = match state.as_str() {
                "SYNC" => self.sync(),
                "OBSERVE" => self.observe(),
                "DECIDE" => self.decide(),
                "WORK" => self.work(),
                _ => Err(anyhow!("unknown loop state")),
            };
            match next {
                Ok(next) => {
                    state = next;
                    self.store.set_meta(self.id, &json!({"state":state}))?;
                }
                Err(e) => {
                    self.outcome = e.to_string();
                    state = "FAILED".into();
                }
            }
        }
        self.finish(&state)
    }
}
pub fn run_loop(
    settings: &Settings,
    goal: &str,
    job_id: Option<i64>,
    milestone: Option<&str>,
) -> Result<i64> {
    let store = Store::open(settings.db_path())?;
    let id = match job_id {
        Some(id) => id,
        None => create_loop_job(settings, &store, goal, milestone, "RUNNING")?,
    };
    DeliveryLoop::new(settings, &store, id)?.run()?;
    Ok(id)
}
pub fn approve_pr(settings: &Settings, job_id: i64) -> Result<()> {
    let store = Store::open(settings.db_path())?;
    let job = store.job(job_id)?.context("job missing")?;
    if job["kind"] != "work" || job["pr_number"].is_null() {
        bail!("job is not a work item with a PR");
    }
    if ["COMPLETED", "RUNNING"].contains(&s(&job, "status")) {
        bail!("job is {}", s(&job, "status"));
    }
    let policy = Policy::new(settings)?;
    let data = state_data(&store, job_id)?;
    if requires_upstream_merge(settings, &job) {
        bail!("Fork PRs must be merged by an upstream maintainer on GitHub");
    }
    let gh = github(settings)?;
    let n = number(&job, "pr_number");
    let pr = gh.pr_view(n)?;
    if pr["state"] != "OPEN" {
        bail!("PR is not open");
    }
    let checks = gh.pr_checks(n)?;
    if checks == "fail" || checks == "pending" {
        bail!("PR checks are {checks}; approval requires settled checks");
    }
    gh.pr_comment(n, "Approved by a human via the Mini-Amaru UI.")?;
    gh.merge_pr(n, &policy.merge_method)?;
    if !job["issue_number"].is_null()
        && should_close(&data, s(&job, "base_branch"), &policy.base_branch)
    {
        let _ = gh.close_issue(
            number(&job, "issue_number"),
            &format!(
                "Merged in {} into `{}`.",
                s(&pr, "url"),
                s(&job, "base_branch")
            ),
        );
    }
    store.set_job(job_id, "COMPLETED", None, None)?;
    store.set_meta(job_id,&json!({"merge_state":"MERGED","state":"COMPLETED","review_verdict":job["review_verdict"].as_str().unwrap_or("HUMAN_APPROVED")}))?;
    store.event(job_id, "HUMAN_APPROVED_MERGED", &json!({"pr":pr["url"]}))?;
    let git = Git::new(&settings.project_root());
    if git.has_remote()? {
        git.fetch()?;
    }
    Ok(())
}
pub fn create_pr_now(settings: &Settings, job_id: i64) -> Result<()> {
    let store = Store::open(settings.db_path())?;
    let row = store.job(job_id)?.context("job missing")?;
    if row["kind"] != "work" || !row["pr_number"].is_null() || s(&row, "branch").is_empty() {
        bail!("job is not an unpublished work item with a branch");
    }
    if !["WAITING_PR", "BLOCKED", "QUEUED_PR"].contains(&s(&row, "status")) {
        bail!("job is {}", s(&row, "status"));
    }
    let mut work = Work::new(settings, &store, job_id, row["parent_job_id"].as_i64())?;
    if s(&work.data, "worktree").is_empty() {
        bail!("worktree path missing");
    }
    work.git.add_worktree(
        &work.wt(),
        s(&work.data, "branch"),
        &format!("origin/{}", s(&work.data, "base")),
    )?;
    work.data["publish_approved"] = json!(true);
    work.save()?;
    store.set_job(job_id, "RUNNING", None, None)?;
    store.set_meta(job_id, &json!({"state":"PUBLISH"}))?;
    work.run()?;
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closing_requires_complete_issue() {
        let d = json!({"completes":true,"item":{"kind":"issue","number":12}});
        assert!(should_close(&d, "feat/integration", "main"));
        assert!(!should_close(&d, "main", "main"));
        let item = json!({"kind":"issue","number":12,"labels":[]});
        assert_eq!(refs_line(&item, "main", "main", true), "Closes #12");
        assert_eq!(
            refs_line(&item, "feat/integration", "main", true),
            "Refs #12"
        );
        assert_eq!(refs_line(&item, "main", "main", false), "Refs #12");
    }
    #[test]
    fn check_exit_and_output() {
        let dir = tempfile::tempdir().unwrap();
        let c = run_check("sh -c 'printf verification; exit 7'", dir.path(), 2);
        assert_eq!(c["returncode"], 7);
        assert_eq!(c["tail"], "verification");
    }
}
#[cfg(all(test, unix))]
mod integration_tests {
    use super::*;
    use std::fs;
    fn sh(cwd: &Path, args: &[&str]) {
        let o = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&o.stderr)
        );
    }
    fn fixture(root: &Path, review: &str) -> Settings {
        let origin = root.join("origin.git");
        let repo = root.join("checkout");
        fs::create_dir(&repo).unwrap();
        sh(root, &["init", "--bare", origin.to_str().unwrap()]);
        sh(&repo, &["init", "-b", "main"]);
        fs::write(repo.join("initial"), "initial\n").unwrap();
        sh(&repo, &["add", "initial"]);
        sh(
            &repo,
            &[
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.com",
                "commit",
                "-m",
                "initial",
            ],
        );
        sh(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        sh(&repo, &["push", "-u", "origin", "main"]);
        let gh = root.join("fake-gh.sh");
        let agent = root.join("fake-agent.sh");
        let script=r##"#!/bin/sh
state='@ROOT@'
case "$1 $2" in
 'api repos/o/r/milestones?state=all&per_page=100') printf '%s\n' '{"number":1,"title":"M1 Delivery"}';;
 'api repos/o/r/issues/1/dependencies/blocked_by'|'api repos/o/r/issues/1/sub_issues') printf '[]';;
 'api repos/o/r/issues/1/parent') printf 'HTTP 404 Not Found' >&2; exit 1;;
 'api repos/contributor/r') printf '{"fork":true,"parent":{"full_name":"o/r"}}';;
 'api --method')
   case "$3 $4" in
    'GET repos/o/r/pulls')
      test "$8" = 'head=contributor:feat/1-delivery-test' && test "${10}" = 'base=main' || exit 3
      if test -f "$state/published"; then
        printf '[{"number":1,"state":"open","html_url":"https://github.com/o/r/pull/1","head":{"ref":"feat/1-delivery-test","repo":{"full_name":"contributor/r"}},"base":{"ref":"main"}}]'
      else
        printf '[{"number":9,"state":"open","html_url":"https://github.com/o/r/pull/9","head":{"ref":"feat/1-delivery-test","repo":{"full_name":"unrelated/r"}},"base":{"ref":"main"}}]'
      fi;;
    'POST repos/o/r/pulls')
      test "$6" = 'base=main' && test "$8" = 'head=contributor:feat/1-delivery-test' && test "${10}" = 'head_repo=r' || exit 3
      test ! -f "$state/published" || exit 4
      touch "$state/published"; printf 'https://github.com/o/r/pull/1\n';;
    *) exit 5;;
   esac;;
 'issue list') if test -f "$state/merged"; then printf '[]'; else printf '[{"number":1,"title":"Delivery test","body":"Implement the thing.","state":"OPEN","url":"https://github.com/o/r/issues/1","labels":[]}]'; fi;;
 'pr list') if test -f "$state/published"; then
   if test -f "$state/merged"; then status=MERGED; else status=OPEN; fi
   for arg in "$@"; do if test "$arg" = merged && test "$status" = OPEN; then printf '[]'; exit 0; fi; done
   printf '[{"number":1,"title":"feat: delivery test","state":"%s","headRefName":"feat/1-delivery-test","baseRefName":"main","url":"https://github.com/o/r/pull/1"}]' "$status"
  elif test -f "$state/other-fork"; then
   for arg in "$@"; do if test "$arg" = merged; then printf '[]'; exit 0; fi; done
   printf '[{"number":9,"title":"Unrelated","state":"OPEN","headRefName":"feat/1-delivery-test","headRepository":{"nameWithOwner":"unrelated/r"}}]'
  else printf '[]'; fi;;
 'pr create') touch "$state/published"; printf 'https://github.com/o/r/pull/1\n';;
 'pr view') if test -f "$state/merged"; then status=MERGED; else status=OPEN; fi; printf '{"number":1,"state":"%s","url":"https://github.com/o/r/pull/1"}' "$status";;
 'pr comment') touch "$state/reviewed";;
 'pr checks') if test -f "$state/pending"; then printf '[{"bucket":"pending"}]'; else printf '[{"bucket":"pass"}]'; fi;;
 'pr merge') touch "$state/merged";;
 'issue close') touch "$state/closed";;
 *) printf 'unexpected fake gh command: %s\n' "$*" >&2; exit 2;;
esac
"##.replace("@ROOT@",root.to_str().unwrap());
        fs::write(&gh, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&agent,format!(r##"#!/bin/sh
prompt=$(cat)
case "$prompt" in
 *"orchestration brain"*) printf '{{}}';;
 *"planning brain"*) printf '{{"summary":"Implement item","tasks":[{{"title":"Implement delivery","description":"Implement and test.","type":"simple_implementation"}}]}}';;
 *"Write concise professional PR text"*) printf '{{"title":"feat: delivery test","outcome":"Implements requested behavior","verification":"Project check passed","integration":"None","completes_issue":true}}';;
 *"independent code reviewer"*) printf '{{"verdict":"{review}","summary":"Checked implementation","findings":[],"completes_issue":true}}';;
 *) printf 'implemented\n' > delivery.txt
    git add delivery.txt
    git -c user.name=Agent -c user.email=agent@example.com commit -m 'feat: delivery test' >/dev/null || exit 1
    printf 'COMPLETED';;
esac
"##)).unwrap();
        Settings {
            root_dir: root.to_path_buf(),
            project_path: root.join("projects/test.yaml"),
            project: json!({"project":{"name":"test","repository":"o/r","root":repo},"github":{"cli":gh},"execution":{"check_command":"git diff --exit-code","require_tests":false},"loop":{"max_iterations":3,"worktrees_dir":"worktrees"},"publish":{"mode":"manual"},"review":{"enabled":true,"max_rounds":0},"merge":{"mode":"integration_only"}}),
            runtime: json!({"storage":{"database":"state.db","artifacts":"artifacts"},"execution":{"max_retries":0}}),
            providers: json!({"fake":{"enabled":true,"type":"cli","command":["sh",agent],"models":["fake"],"timeout_seconds":5}}),
            models: json!({"fake":{"provider":"fake","model":"fake"}}),
            routing: json!({"planning":"fake","simple_implementation":"fake","summarization":"fake","code_review":"fake"}),
        }
    }
    #[test]
    fn loop_manual_publish_approve_and_resume() {
        let temp = tempfile::tempdir().unwrap();
        let settings = fixture(temp.path(), "APPROVE");
        let preview = dry_run(&settings, "Milestone 1", None).unwrap();
        assert_eq!(preview["plan"][0]["eligible"], true);
        assert!(!settings.db_path().exists());
        assert!(!temp.path().join("worktrees").exists());
        let id = run_loop(&settings, "Milestone 1", None, None).unwrap();
        let store = Store::open(settings.db_path()).unwrap();
        assert_eq!(store.job(id).unwrap().unwrap()["status"], "WAITING_MERGE");
        let child = store.children(id).unwrap().remove(0);
        let cid = number(&child, "id");
        assert_eq!(child["status"], "WAITING_PR", "{child}");
        assert!(!temp.path().join("published").exists());
        create_pr_now(&settings, cid).unwrap();
        let child = store.job(cid).unwrap().unwrap();
        assert_eq!(child["status"], "WAITING_MERGE", "{child}");
        assert_eq!(child["review_verdict"], "APPROVE");
        assert!(!temp.path().join("merged").exists());
        fs::write(temp.path().join("pending"), "").unwrap();
        assert!(approve_pr(&settings, cid).is_err());
        assert!(!temp.path().join("merged").exists());
        fs::remove_file(temp.path().join("pending")).unwrap();
        approve_pr(&settings, cid).unwrap();
        assert_eq!(store.job(cid).unwrap().unwrap()["status"], "COMPLETED");
        assert_eq!(
            run_loop(&settings, "Milestone 1", Some(id), None).unwrap(),
            id
        );
        assert_eq!(store.job(id).unwrap().unwrap()["status"], "COMPLETED");
        assert_eq!(store.children(id).unwrap().len(), 1);
    }
    #[test]
    fn rejecting_review_blocks_merge() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = fixture(temp.path(), "REQUEST_CHANGES");
        settings.project["publish"]["mode"] = json!("auto");
        let id = run_loop(&settings, "Milestone 1", None, None).unwrap();
        let store = Store::open(settings.db_path()).unwrap();
        let child = store.children(id).unwrap().remove(0);
        assert_eq!(child["status"], "BLOCKED", "{child}");
        assert_eq!(child["review_verdict"], "REQUEST_CHANGES");
        assert!(!temp.path().join("merged").exists());
        assert_eq!(store.job(id).unwrap().unwrap()["status"], "BLOCKED");
    }

    #[test]
    fn publishes_to_fork_and_waits_for_upstream_merge() {
        let temp = tempfile::tempdir().unwrap();
        let mut settings = fixture(temp.path(), "APPROVE");
        settings.project["git"] = json!({"fork_repository":"contributor/r"});
        settings.project["merge"]["mode"] = json!("always");
        fs::write(temp.path().join("other-fork"), "").unwrap();
        let fork = temp.path().join("fork.git");
        sh(temp.path(), &["init", "--bare", fork.to_str().unwrap()]);
        let checkout = settings.project_root();
        sh(
            &checkout,
            &[
                "config",
                "--local",
                &format!("url.{}.insteadOf", fork.display()),
                "https://github.com/contributor/r.git",
            ],
        );

        let id = run_loop(&settings, "Milestone 1", None, None).unwrap();
        let store = Store::open(settings.db_path()).unwrap();
        let child = store.children(id).unwrap().remove(0);
        assert_eq!(child["status"], "WAITING_PR", "{child}");
        let cid = number(&child, "id");
        create_pr_now(&settings, cid).unwrap();
        let child = store.job(cid).unwrap().unwrap();
        assert_eq!(child["status"], "WAITING_MERGE", "{child}");
        assert_eq!(child["pr_url"], "https://github.com/o/r/pull/1");
        assert!(approve_pr(&settings, cid).is_err());
        assert!(!temp.path().join("merged").exists());
        let branch = "refs/heads/feat/1-delivery-test";
        sh(&fork, &["show-ref", "--verify", branch]);
        let upstream = Git::new(&checkout);
        assert!(upstream.remote_branches(branch).unwrap().is_empty());

        // Recover when GitHub created the PR before its number was saved locally.
        store.set_job(cid, "WAITING_PR", None, None).unwrap();
        store.set_meta(cid, &json!({"pr_number":null})).unwrap();
        create_pr_now(&settings, cid).unwrap();
        let child = store.job(cid).unwrap().unwrap();
        assert_eq!(child["status"], "WAITING_MERGE", "{child}");
        assert_eq!(child["pr_number"], 1);

        fs::write(temp.path().join("merged"), "").unwrap();
        run_loop(&settings, "Milestone 1", Some(id), None).unwrap();
        assert_eq!(store.job(cid).unwrap().unwrap()["status"], "COMPLETED");
        assert!(!temp.path().join("closed").exists());
    }
}
