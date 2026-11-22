#![cfg(unix)]

use anyhow::{Context, Result};
use mini_amaru::storage::Store;
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn fixture(root: &Path, fail: bool) -> Result<()> {
    fs::create_dir_all(root.join("projects"))?;
    fs::create_dir_all(root.join("config"))?;
    fs::create_dir_all(root.join("checkout"))?;
    let git = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root.join("checkout"))
        .output()?;
    anyhow::ensure!(git.status.success(), "git init failed");
    fs::write(
        root.join("agent.sh"),
        if fail {
            "#!/bin/sh\ncat >/dev/null\necho 'provider deliberately failed' >&2\nexit 9\n"
        } else {
            r#"#!/bin/sh
prompt=$(cat)
case "$prompt" in
  *"planning brain"*) printf '%s\n' '{"summary":"Test plan","tasks":[{"title":"Write evidence","description":"Write a result in the checkout","type":"simple_implementation"}]}' ;;
  *) printf '%s\n' 'agent executed in checkout' > evidence.txt
     printf '%s\n' 'COMPLETED: fake implementation and verification' ;;
esac
"#
        },
    )?;
    let files: Vec<(&str, Value)> = vec![
        (
            "projects/test.yaml",
            json!({"project":{"name":"cli-test","root":"../checkout"},"github":{"publish_report":false},"execution":{"require_tests":false}}),
        ),
        (
            "config/runtime.yaml",
            json!({"storage":{"database":"data/test.db","artifacts":"data/artifacts"},"execution":{"max_tasks":5,"max_retries":0,"continue_on_task_failure":false}}),
        ),
        (
            "config/providers.yaml",
            json!({"providers":{"fake":{"enabled":true,"type":"cli","command":["/bin/sh",root.join("agent.sh")],"models":["fake-model"],"timeout_seconds":5}}}),
        ),
        (
            "config/models.yaml",
            json!({"models":{"test-model":{"provider":"fake","model":"fake-model"}}}),
        ),
        (
            "config/routing.yaml",
            json!({"routing":{"planning":"test-model","simple_implementation":"test-model","fallback":[]}}),
        ),
    ];
    for (name, value) in files {
        fs::write(root.join(name), value.to_string())?;
    }
    Ok(())
}

fn cli(root: &Path, args: &[&str]) -> Result<Output> {
    Ok(Command::new(env!("CARGO_BIN_EXE_mini-amaru"))
        .current_dir(root)
        .args(args)
        .output()?)
}

fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn planning_falls_back_and_reports_unavailable_candidates() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    fixture(root, false)?;
    fs::write(
        root.join("config/routing.yaml"),
        "routing:\n  planning: missing-primary\n  simple_implementation: test-model\n  fallback: [test-model]\n",
    )?;
    let config = "projects/test.yaml";
    let output = cli(root, &["doctor", "-c", config])?;
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("[OK] planning: test-model (fake)"));
    success(&cli(root, &["run", "-c", config, "-p", "test fallback"])?);

    fs::write(
        root.join("config/providers.yaml"),
        "providers:\n  fake:\n    enabled: true\n    type: cli\n    command: [./missing-executable]\n    models: [fake-model]\n",
    )?;
    let output = cli(root, &["doctor", "-c", config])?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("missing-primary: missing model or provider"));
    assert!(stdout.contains("test-model (fake): executable not found: ./missing-executable"));
    let output = cli(root, &["run", "-c", config, "-p", "test failure"])?;
    assert!(!output.status.success());
    let store = Store::open(root.join("data/test.db"))?;
    let job = store.job(2)?.context("missing failed job")?;
    assert!(
        job["report"]
            .as_str()
            .unwrap_or_default()
            .contains("test-model (fake): executable not found: ./missing-executable")
    );
    Ok(())
}

#[test]
fn cli_runs_persists_lists_and_resumes_jobs() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let root = dir.path();
    fixture(root, false)?;
    success(&cli(
        root,
        &[
            "run",
            "-c",
            "projects/test.yaml",
            "-p",
            "Implement a small change",
        ],
    )?);
    let store = Store::open(root.join("data/test.db"))?;
    let job = store.job(1)?.context("first job missing")?;
    assert_eq!(job["status"], "COMPLETED");
    assert_eq!(store.tasks(1)?[0]["status"], "COMPLETED");
    assert!(root.join("checkout/evidence.txt").is_file());
    assert!(root.join("data/artifacts/job-1-report.md").is_file());
    let status = cli(root, &["-c", "projects/test.yaml", "status", "--job", "1"])?;
    success(&status);
    assert!(String::from_utf8_lossy(&status.stdout).contains("COMPLETED"));
    let jobs = cli(root, &["-c", "projects/test.yaml", "jobs"])?;
    success(&jobs);
    assert!(String::from_utf8_lossy(&jobs.stdout).contains("Implement a small change"));
    success(&cli(
        root,
        &["resume", "-c", "projects/test.yaml", "--job", "1"],
    )?);
    let resumed = store.job(2)?.context("resumed job missing")?;
    assert_eq!(resumed["status"], "COMPLETED");
    assert!(resumed["prompt"].as_str().unwrap().contains("1"));
    assert_eq!(store.job(1)?.unwrap()["prompt"], "Implement a small change");
    Ok(())
}

#[test]
fn cli_records_provider_failure_and_returns_nonzero() -> Result<()> {
    let dir = tempfile::tempdir()?;
    fixture(dir.path(), true)?;
    let output = cli(
        dir.path(),
        &["run", "-c", "projects/test.yaml", "-p", "Fail deliberately"],
    )?;
    assert!(!output.status.success());
    let store = Store::open(dir.path().join("data/test.db"))?;
    assert_eq!(store.job(1)?.unwrap()["status"], "FAILED");
    assert!(
        store
            .events(1, 0)?
            .iter()
            .any(|event| event["event"] == "JOB_FAILED")
    );
    Ok(())
}

#[test]
fn cli_rejects_unknown_arguments_and_missing_jobs() -> Result<()> {
    let dir = tempfile::tempdir()?;
    fixture(dir.path(), false)?;
    assert!(
        !cli(dir.path(), &["run", "--not-a-real-option"])?
            .status
            .success()
    );
    assert!(
        !cli(
            dir.path(),
            &["resume", "-c", "projects/test.yaml", "--job", "999"]
        )?
        .status
        .success()
    );
    Ok(())
}
