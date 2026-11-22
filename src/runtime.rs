use crate::{config::Settings, engine::Engine, storage::Store};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs;

fn cancelled(store: &Store, id: i64) -> Result<bool> {
    let Some(job) = store.job(id)? else {
        return Ok(false);
    };
    if matches!(job["status"].as_str(), Some("CANCELLING" | "CANCELLED")) {
        return Ok(true);
    }
    if let Some(parent) = job["parent_job_id"].as_i64() {
        return cancelled(store, parent);
    }
    Ok(false)
}

pub fn run_pending_tasks(
    settings: &Settings,
    store: &Store,
    engine: &Engine<'_>,
    job_id: i64,
    task_context: &str,
) -> Result<Vec<String>> {
    let retries = settings.runtime["execution"]["max_retries"]
        .as_u64()
        .unwrap_or(2);
    let keep_going = settings.runtime["execution"]["continue_on_task_failure"]
        .as_bool()
        .unwrap_or(false);
    let mut failures = Vec::new();
    for mut task in store.tasks(job_id)? {
        if task["status"] != "PENDING" {
            continue;
        }
        let id = task["id"].as_i64().context("Task id missing")?;
        let mut success = false;
        for attempt in 0..=retries {
            if cancelled(store, job_id)? {
                bail!("cancelled");
            }
            store.update_task(id, "RUNNING", None, None, true)?;
            task["attempts"] = json!(task["attempts"].as_u64().unwrap_or(0) + 1);
            match engine.execute_task(job_id, &task, task_context) {
                Ok(output) => {
                    store.update_task(id, "COMPLETED", Some(&output), None, false)?;
                    store.event(job_id, "TASK_COMPLETED", &json!({"task_id":id}))?;
                    success = true;
                    break;
                }
                Err(e) => {
                    let message = e.to_string();
                    store.update_task(id, "FAILED", None, Some(&message), false)?;
                    store.event(
                        job_id,
                        "TASK_FAILED",
                        &json!({"task_id":id,"error":message}),
                    )?;
                    if attempt == retries {
                        failures.push(format!(
                            "- {}: {message}",
                            task["title"].as_str().unwrap_or("")
                        ));
                    }
                }
            }
        }
        if !success && !keep_going {
            break;
        }
    }
    Ok(failures)
}

#[allow(clippy::too_many_arguments)]
pub fn execute_plan(
    settings: &Settings,
    store: &Store,
    engine: &Engine<'_>,
    job_id: i64,
    prompt: &str,
    github: bool,
    max_tasks: Option<usize>,
    context: &str,
    task_context: &str,
) -> Result<(Value, Vec<String>)> {
    let plan = engine.plan(job_id, prompt, github, max_tasks, context)?;
    Ok((
        plan,
        run_pending_tasks(settings, store, engine, job_id, task_context)?,
    ))
}

pub fn run_job(settings: &Settings, prompt: &str, job_id: Option<i64>) -> Result<i64> {
    let store = Store::open(settings.db_path())?;
    let id = match job_id {
        Some(id) => {
            store.set_job(id, "RUNNING", None, None)?;
            id
        }
        None => store.create_job(&settings.project_name(), prompt, "RUNNING", "job", None)?,
    };
    let result = (|| -> Result<()> {
        crate::workspace::ensure_project(settings)?;
        let engine = Engine::new(settings, &store, None, "job")?;
        let repo = engine.repo_info();
        store.event(id,"JOB_STARTED",&json!({"prompt":prompt,"cwd":repo["cwd"],"branch":repo["branch"],"agents_md":repo["agents_md"],"claude_md":repo["claude_md"]}))?;
        if repo["exists"] != true {
            bail!(
                "Project directory does not exist: {}",
                engine.project_root.display()
            );
        }
        let (plan, failures) =
            execute_plan(settings, &store, &engine, id, prompt, true, None, "", "")?;
        let tasks = store.tasks(id)?;
        let completed = tasks.iter().filter(|t| t["status"] == "COMPLETED").count();
        let mut status = if completed == tasks.len() {
            "COMPLETED"
        } else {
            "BLOCKED"
        };
        let mut report = format!(
            "# Mini-Amaru execution report — Job {id}\n\n**Project:** {}\n**Request:** {prompt}\n**Status:** {status}\n\n## Plan\n{}\n\n## Results\n- Tasks: {}\n- Completed: {completed}\n- Not completed: {}\n\n## Task details\n",
            settings.project_name(),
            plan["summary"].as_str().unwrap_or(""),
            tasks.len(),
            tasks.len() - completed
        );
        for task in &tasks {
            report.push_str(&format!(
                "- [{}] {}{}\n",
                task["status"].as_str().unwrap_or(""),
                task["title"].as_str().unwrap_or(""),
                task["error"]
                    .as_str()
                    .map(|s| format!(": {s}"))
                    .unwrap_or_default()
            ));
        }
        if !failures.is_empty() {
            report.push_str(&format!("\n## Failures\n{}", failures.join("\n")));
        }
        let path = engine.artifacts.join(format!("job-{id}-report.md"));
        match engine.publish_report(id, &report) {
            Ok(note) => println!("{note}"),
            Err(e) => {
                status = "BLOCKED";
                report.push_str(&format!("\n\n**Publication failed:** {e}"));
            }
        }
        fs::write(&path, &report)?;
        store.artifact(id, "final_report", &path.to_string_lossy())?;
        store.set_job(id, status, None, Some(&report))?;
        store.event(id, "JOB_FINISHED", &json!({"status":status,"report":path}))?;
        println!("Job {id}: {status}\nReport: {}", path.display());
        Ok(())
    })();
    if let Err(e) = result {
        let status = if cancelled(&store, id)? {
            "CANCELLED"
        } else {
            "FAILED"
        };
        store.set_job(id, status, None, Some(&e.to_string()))?;
        store.event(id, "JOB_FAILED", &json!({"error":e.to_string()}))?;
        return Err(e.context(format!("Job {id} {status}")));
    }
    Ok(id)
}

pub fn resume_prompt(prompt: &str, id: i64) -> String {
    format!(
        "{prompt}\nContinue the interrupted work from Job {id}. Reuse the repository state and finish what remains."
    )
}
pub fn resume_job(settings: &Settings, id: i64) -> Result<i64> {
    let store = Store::open(settings.db_path())?;
    let old = store.job(id)?.context("Job not found")?;
    run_job(
        settings,
        &resume_prompt(old["prompt"].as_str().unwrap_or(""), id),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(script: &str) -> Result<(tempfile::TempDir, Settings)> {
        let dir = tempfile::tempdir()?;
        let settings = Settings {
            root_dir: dir.path().to_path_buf(),
            project_path: dir.path().join("projects/demo.yaml"),
            project: json!({"project":{"name":"demo","root":dir.path()},"github":{"publish_report":false}}),
            runtime: json!({"storage":{"database":"state.db","artifacts":"artifacts"},"execution":{"max_retries":1}}),
            providers: json!({"fake":{"enabled":true,"type":"cli","command":["sh","-c",script],"models":["test"],"timeout_seconds":2}}),
            models: json!({"test":{"provider":"fake","model":"test"}}),
            routing: json!({"planning":"test","simple_implementation":"test"}),
        };
        Ok((dir, settings))
    }
    #[test]
    fn full_job_persists_plan_task_run_and_log() -> Result<()> {
        let script = r#"input=$(cat); case "$input" in *'planning brain'*) printf '%s' '{"summary":"test","tasks":[{"title":"task","description":"write a file","type":"simple_implementation"}]}' ;; *) echo done; echo artifact > generated.txt ;; esac"#;
        let (_dir, settings) = fixture(script)?;
        let id = run_job(&settings, "test goal", None)?;
        let store = Store::open(settings.db_path())?;
        assert_eq!(store.job(id)?.unwrap()["status"], "COMPLETED");
        assert_eq!(store.tasks(id)?[0]["status"], "COMPLETED");
        assert!(settings.project_root().join("generated.txt").is_file());
        assert!(
            fs::read_to_string(crate::joblog::log_path(&settings.artifacts_dir(), id))?
                .contains("done")
        );
        let count: i64 = store.conn.query_row(
            "SELECT COUNT(*) FROM runs WHERE status='SUCCEEDED'",
            [],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1);
        Ok(())
    }
    #[test]
    fn missing_scratch_project_directory_is_created_before_execution() -> Result<()> {
        let script = r#"input=$(cat); case "$input" in *'planning brain'*) printf '%s' '{"tasks":[{"title":"task","description":"write a file"}]}' ;; *) echo done; echo artifact > generated.txt ;; esac"#;
        let (_dir, mut settings) = fixture(script)?;
        let checkout = settings.root_dir.join("repos/nested/new-project");
        settings.project["project"]["root"] = json!(checkout);
        assert!(!checkout.exists());
        let id = run_job(&settings, "test goal", None)?;
        assert!(checkout.join("generated.txt").is_file());
        assert_eq!(
            Store::open(settings.db_path())?.job(id)?.unwrap()["status"],
            "COMPLETED"
        );
        Ok(())
    }
    #[test]
    fn failed_task_retries_and_records_failed_runs() -> Result<()> {
        let script = r#"input=$(cat); case "$input" in *'planning brain'*) echo '{"tasks":[{"title":"bad","description":"fails"}]}' ;; *) echo rejected >&2; exit 2 ;; esac"#;
        let (_dir, settings) = fixture(script)?;
        let id = run_job(&settings, "test goal", None)?;
        let store = Store::open(settings.db_path())?;
        assert_eq!(store.job(id)?.unwrap()["status"], "BLOCKED");
        assert_eq!(store.tasks(id)?[0]["attempts"], 2);
        let count: i64 =
            store
                .conn
                .query_row("SELECT COUNT(*) FROM runs WHERE status='FAILED'", [], |r| {
                    r.get(0)
                })?;
        assert_eq!(count, 2);
        Ok(())
    }
}
