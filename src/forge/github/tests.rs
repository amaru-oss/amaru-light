use super::*;
use std::path::Path;

/// A fake `gh` that logs its arguments and answers from canned JSON.
const FAKE: &str = r#"#!/bin/sh
echo "$*" >> "$(dirname "$0")/calls.log"
case "$*" in
  *"labels=amaru"*) echo '[{"number":1,"title":"One","body":"Depends on #5","state":"open","labels":[{"name":"amaru"}],"assignees":[],"created_at":"2026-01-01T00:00:00Z"},{"number":9,"title":"A PR","state":"open","pull_request":{}}]' ;;
  *"assignee=bot"*) echo '[{"number":1,"title":"One","body":"Depends on #5","state":"open","labels":[],"assignees":[{"login":"bot"}],"created_at":"2026-01-01T00:00:00Z"},{"number":2,"title":"Two","body":"","state":"open","labels":[],"assignees":[{"login":"bot"}],"milestone":{"title":"M1","due_on":"2026-02-01T00:00:00Z"},"created_at":"2026-01-02T00:00:00Z"}]' ;;
  *"dependencies/blocked_by"*) echo '[]' ;;
  *"issues/5") echo '{"number":5,"title":"Five","state":"open"}' ;;
  *"pulls -f head="*) echo '[{"number":30,"html_url":"u","state":"open","head":{"sha":"abc"}}]' ;;
  *"pulls/30/reviews"*) echo '[{"id":1,"state":"APPROVED","body":"","user":{"login":"me"}},{"id":2,"node_id":"PRR_2","state":"CHANGES_REQUESTED","body":"redo","user":{"login":"me"}}]' ;;
  *"pulls/30/comments"*) echo '[{"id":3,"body":"rename","path":"a.rs","line":4,"user":{"login":"me"}}]' ;;
  *"issues/30/comments"*) echo '[]' ;;
  *"check-runs"*) echo '{"check_runs":[{"id":7,"name":"ci","conclusion":"failure","output":{"title":"tests","summary":"2 failed"}},{"id":8,"name":"lint","conclusion":"success"}]}' ;;
  *) echo '{}' ;;
esac
"#;

fn fake(dir: &Path) -> GitHub {
    let script = dir.join("gh");
    std::fs::write(&script, FAKE).unwrap();
    let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&script, mode).unwrap();
    GitHub::new(script.to_string_lossy().into_owned())
}

fn calls(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default()
}

#[tokio::test]
async fn merges_label_and_assignee_selectors_without_prs() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake(dir.path());
    let selector = Selector {
        labels: vec!["amaru".into()],
        assignee: Some("bot".into()),
    };
    let issues = gh.eligible("me/app", &selector).await.unwrap();
    let numbers: Vec<u64> = issues.iter().map(|i| i.number).collect();
    assert_eq!(numbers, [1, 2]);
    assert_eq!(issues[0].blocked_by, [5]);
    assert_eq!(
        issues[1].milestone_due.as_deref(),
        Some("2026-02-01T00:00:00Z")
    );
}

#[tokio::test]
async fn collects_feedback_and_failed_checks() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake(dir.path());
    let target = Target {
        upstream: "org/app".into(),
        fork: Some("bot/app".into()),
    };
    let pr = gh.find_pr(&target, "amaru/1-x").await.unwrap().unwrap();
    assert!(calls(dir.path()).contains("head=bot:amaru/1-x"));
    let found = gh.feedback("org/app", &pr).await.unwrap();
    let ids: Vec<&str> = found.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["review-2", "line-3", "check-7-abc"]);
    assert_eq!(found[0].node_id.as_deref(), Some("PRR_2"));
    assert_eq!(found[1].path.as_deref(), Some("a.rs"));
    assert!(found[2].body.contains("2 failed"));
}

#[tokio::test]
async fn replies_in_line_threads_and_encodes_labels() {
    let dir = tempfile::tempdir().unwrap();
    let gh = fake(dir.path());
    let line = Feedback {
        id: "line-3".into(),
        kind: FeedbackKind::LineComment,
        author: "me".into(),
        body: String::new(),
        path: None,
        line: None,
        node_id: Some("PRRC_3".into()),
    };
    gh.reply("org/app", 30, &line, "done").await.unwrap();
    gh.react(&line, Reaction::Eyes).await.unwrap();
    gh.react(&line, Reaction::Off).await.unwrap();
    gh.remove_label("org/app", 1, "amaru:working")
        .await
        .unwrap();
    let log = calls(dir.path());
    assert!(log.contains("pulls/30/comments/3/replies -f body=done"));
    assert!(log.contains("labels/amaru%3Aworking"));
    assert!(log.contains("graphql -f query=mutation"));
    assert!(log.contains("content: EYES"));
    assert!(log.contains("-f id=PRRC_3"));
    assert_eq!(log.matches("graphql").count(), 1);
}

/// A fake `gh` for forks: `bot/app` and `org-bot/app` exist once created,
/// `bot/other` is a fork of something else, `bot/plain` is no fork at all.
const FORKS: &str = r#"#!/bin/sh
dir=$(dirname "$0")
echo "$*" >> "$dir/calls.log"
fork='{"full_name":"%s","fork":true,"parent":{"full_name":"org/app"},"source":{"full_name":"org/app"}}'
case "$*" in
  "api user") echo '{"login":"bot"}' ;;
  "api -X POST repos/org/app/forks"*"organization=org-bot") touch "$dir/created-org"; printf "$fork" org-bot/app ;;
  "api -X POST repos/org/app/forks -f name=app"*) touch "$dir/created-bot"; printf "$fork" bot/app ;;
  "api -X POST repos/org/app/forks -f name=taken") printf "$fork" bot/app ;;
  "api repos/bot/app") [ -f "$dir/created-bot" ] || { echo 'gh: Not Found (HTTP 404)' >&2; exit 1; }; printf "$fork" bot/app ;;
  "api repos/org-bot/app") [ -f "$dir/created-org" ] || { echo 'gh: Not Found (HTTP 404)' >&2; exit 1; }; printf "$fork" org-bot/app ;;
  "api repos/bot/other") echo '{"full_name":"bot/other","fork":true,"parent":{"full_name":"else/app"}}' ;;
  "api repos/bot/plain") echo '{"full_name":"bot/plain","fork":false}' ;;
  "api repos/bot/taken") echo 'gh: Not Found (HTTP 404)' >&2; exit 1 ;;
  "api repos/bot/down") echo 'gh: Bad Gateway (HTTP 502)' >&2; exit 1 ;;
  *) echo '{}' ;;
esac
"#;

fn forks(dir: &Path) -> GitHub {
    let script = dir.join("gh");
    std::fs::write(&script, FORKS).unwrap();
    let mode = std::os::unix::fs::PermissionsExt::from_mode(0o755);
    std::fs::set_permissions(&script, mode).unwrap();
    GitHub::new(script.to_string_lossy().into_owned())
        .with_fork_wait(Duration::from_millis(50), Duration::from_millis(10))
}

fn forked(fork: &str) -> Target {
    Target {
        upstream: "org/app".into(),
        fork: Some(fork.into()),
    }
}

#[tokio::test]
async fn creates_a_missing_fork_once() {
    let dir = tempfile::tempdir().unwrap();
    let gh = forks(dir.path());
    gh.ensure_fork(&forked("bot/app")).await.unwrap();
    gh.ensure_fork(&forked("bot/app")).await.unwrap();
    let log = calls(dir.path());
    assert_eq!(log.matches("/forks").count(), 1, "{log}");
    assert!(!log.contains("organization="));
    // A fork owned by an organization is made there.
    gh.ensure_fork(&forked("org-bot/app")).await.unwrap();
    assert!(calls(dir.path()).contains("-f name=app -f organization=org-bot"));
}

#[tokio::test]
async fn refuses_a_repository_that_is_not_a_fork_of_upstream() {
    let dir = tempfile::tempdir().unwrap();
    let gh = forks(dir.path());
    for (fork, says) in [
        ("bot/other", "is a fork of else/app"),
        ("bot/plain", "is not a fork"),
        ("bot/taken", "set `fork: bot/app`"),
    ] {
        let error = gh.ensure_fork(&forked(fork)).await.unwrap_err();
        assert!(error.is::<ForkError>(), "{fork}: {error:#}");
        assert!(format!("{error:#}").contains(says), "{fork}: {error:#}");
    }
    // Other failures may pass, so they are not a ForkError.
    let error = gh.ensure_fork(&forked("bot/down")).await.unwrap_err();
    assert!(!error.is::<ForkError>());
    let direct = Target {
        upstream: "org/app".into(),
        fork: None,
    };
    gh.ensure_fork(&direct).await.unwrap();
}
