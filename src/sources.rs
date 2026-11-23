use crate::{github::GitHubClient, gitops::slugify, policy::Policy};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeSet;
pub fn label_names(issue: &Value) -> Vec<String> {
    issue["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().or_else(|| v["name"].as_str()).map(str::to_owned))
        .collect()
}
pub fn branch_name(number: i64, title: &str, labels: &[String], prefixes: &Value) -> String {
    let prefix = labels
        .iter()
        .find_map(|label| {
            prefixes[label].as_str().or(match label.as_str() {
                "bug" => Some("fix"),
                "documentation" | "migration-adr" => Some("docs"),
                "spike" => Some("spike"),
                _ => None,
            })
        })
        .unwrap_or("feat");
    format!("{prefix}/{number}-{}", slugify(title, 40))
}
pub fn eligibility(
    issue: &Value,
    skip: &[String],
    open_blockers: &[i64],
    off_base: &[i64],
    open_subs: &[i64],
    prs: &[Value],
    is_parent: bool,
) -> Vec<String> {
    let mut reasons = Vec::new();
    for label in label_names(issue) {
        if skip.contains(&label) {
            reasons.push(format!("gated:{label}"));
        }
    }
    if !open_subs.is_empty() {
        reasons.push(format!(
            "has_open_sub_issues:{}",
            open_subs
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(",")
        ));
    } else if is_parent {
        reasons.push("parent_needs_integration_pr".into());
    }
    for n in open_blockers {
        reasons.push(format!("blocked_by:#{n}"));
    }
    for n in off_base {
        reasons.push(format!("blocker_not_on_base:#{n}"));
    }
    for pr in prs {
        if pr["state"] == "OPEN" {
            reasons.push(format!("pr_open:#{}", pr["number"]));
        }
    }
    reasons
}
pub fn free_branch(branch: &str, prs: &[Value]) -> String {
    let mut candidate = branch.to_owned();
    let mut n = 2;
    while prs.iter().any(|p| p["headRefName"] == candidate) {
        candidate = format!("{branch}-{n}");
        n += 1;
    }
    candidate
}
pub fn observe(gh: &GitHubClient, selector: &str, policy: &Policy) -> Result<Value> {
    let milestone = GitHubClient::resolve_milestone(&gh.list_milestones()?, selector)?;
    let mut issues = gh.list_issues(milestone["title"].as_str().context("milestone title")?)?;
    issues.sort_by_key(|i| i["number"].as_i64().unwrap_or(0));
    let mut prs = gh.all_prs()?;
    if let Some(fork) = policy.fork_repository.as_deref() {
        prs.retain(|pr| {
            pr["headRepository"]["nameWithOwner"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(fork))
        });
    }
    let mut items = Vec::new();
    let mut closed = 0;
    let mut actionable = 0;
    let mut eligible = 0;
    let mut gated = 0;
    let mut blocked = 0;
    let mut waiting = 0;
    let mut parents_ready = Vec::new();
    for issue in &issues {
        if issue["state"] != "OPEN" {
            closed += 1;
            continue;
        }
        let n = issue["number"].as_i64().context("issue number")?;
        let labels = label_names(issue);
        let subs = gh.sub_issues(n)?;
        let open_subs: Vec<_> = subs
            .iter()
            .filter(|s| s["state"] == "open")
            .filter_map(|s| s["number"].as_i64())
            .collect();
        let parent = gh.parent_issue(n)?;
        let mut open = BTreeSet::new();
        let mut off = BTreeSet::new();
        let mut owners = vec![n];
        if let Some(p) = &parent {
            owners.push(p["number"].as_i64().context("parent number")?);
        }
        for owner in owners {
            for b in gh.blocked_by(owner)? {
                let bn = b["number"].as_i64().context("blocker number")?;
                if b["state"] == "open" {
                    open.insert(bn);
                } else if let Some(bp) = gh.parent_issue(bn)?
                    && bp["state"] == "open"
                    && parent.as_ref().map(|p| &p["number"]) != Some(&bp["number"])
                {
                    off.insert(bn);
                }
            }
        }
        let issue_prs: Vec<_> = prs
            .iter()
            .filter(|p| {
                p["headRefName"]
                    .as_str()
                    .unwrap_or("")
                    .split_once('/')
                    .map(|(_, s)| s.starts_with(&format!("{n}-")))
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let prior: Vec<_> = issue_prs
            .iter()
            .filter(|p| p["state"] == "MERGED")
            .map(|p| json!({"number":p["number"],"title":p["title"],"base":p["baseRefName"]}))
            .collect();
        let reasons = eligibility(
            issue,
            &policy.skip_labels,
            &open.into_iter().collect::<Vec<_>>(),
            &off.into_iter().collect::<Vec<_>>(),
            &open_subs,
            &issue_prs,
            !subs.is_empty(),
        );
        let is_gated = reasons.iter().any(|r| r.starts_with("gated:"));
        if !subs.is_empty() && open_subs.is_empty() && !is_gated {
            parents_ready.push(n);
        }
        if is_gated {
            gated += 1;
        } else if subs.is_empty() {
            actionable += 1;
            if reasons.iter().any(|r| r.starts_with("pr_open")) {
                waiting += 1;
            } else if reasons
                .iter()
                .any(|r| r.starts_with("blocked_by") || r.starts_with("blocker_not_on_base"))
            {
                blocked += 1;
            }
        }
        if reasons.is_empty() {
            eligible += 1;
        }
        let title = issue["title"].as_str().unwrap_or("");
        items.push(json!({"ref":format!("issue#{n}"),"kind":"issue","number":n,"title":title,"body":issue["body"].as_str().unwrap_or(""),"url":issue["url"],"labels":labels,"parent":parent.as_ref().map(|p|&p["number"]),"parent_title":parent.as_ref().map(|p|&p["title"]),"is_parent":!subs.is_empty(),"eligible":reasons.is_empty(),"reasons":reasons,"branch":free_branch(&branch_name(n,title,&labels,&policy.prefixes),&prs),"prior_pr_count":prior.len(),"prior_prs":prior.iter().rev().take(8).rev().collect::<Vec<_>>()}));
    }
    Ok(
        json!({"source":"github_milestone","milestone":{"number":milestone["number"],"title":milestone["title"]},"closed":closed,"summary":{"open":items.len(),"actionable":actionable,"eligible":eligible,"blocked":blocked,"gated":gated,"waiting_pr":waiting,"parents_ready_for_integration":parents_ready},"items":items,"exit_issue":null}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gates_and_prior_prs() {
        let i = json!({"labels":[{"name":"owner-task"}]});
        assert_eq!(
            eligibility(&i, &["owner-task".into()], &[12], &[], &[], &[], false),
            vec!["gated:owner-task", "blocked_by:#12"]
        );
        assert!(
            eligibility(
                &json!({}),
                &[],
                &[],
                &[],
                &[],
                &[json!({"state":"MERGED","number":2})],
                false
            )
            .is_empty()
        );
        assert!(!eligibility(&json!({}), &[], &[], &[], &[], &[], true).is_empty());
    }
    #[test]
    fn branch_mapping_and_uniqueness() {
        assert_eq!(
            branch_name(12, "Fix bug", &["bug".into()], &json!({})),
            "fix/12-fix-bug"
        );
        assert_eq!(
            free_branch("fix/12-bug", &[json!({"headRefName":"fix/12-bug"})]),
            "fix/12-bug-2"
        );
    }
}
