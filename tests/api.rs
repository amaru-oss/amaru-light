#![allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction,
    reason = "legacy runtime, replaced by the v2 daemon"
)]
#![cfg(unix)]

use anyhow::{Context, Result};
use reqwest::blocking::{Client, Response};
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Api {
    child: Child,
    base: String,
    client: Client,
    _dir: tempfile::TempDir,
}

impl Drop for Api {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn configure(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("projects"))?;
    fs::create_dir_all(root.join("config"))?;
    fs::create_dir_all(root.join("checkout"))?;
    let output = Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(root.join("checkout"))
        .output()?;
    anyhow::ensure!(output.status.success(), "git init failed");
    fs::write(
        root.join("agent.sh"),
        r#"#!/bin/sh
prompt=$(cat)
case "$prompt" in
  *"planning brain"*) printf '%s\n' '{"summary":"API test plan","tasks":[{"title":"Complete task","description":"Produce test evidence","type":"simple_implementation"}]}' ;;
  *) printf '%s\n' 'COMPLETED: API fake agent output' ;;
esac
"#,
    )?;
    let files: Vec<(&str, Value)> = vec![
        (
            "projects/test.yaml",
            json!({"project":{"name":"api-test","root":"../checkout"},"github":{"publish_report":false},"execution":{"require_tests":false}}),
        ),
        (
            "config/runtime.yaml",
            json!({"storage":{"database":"data/test.db","artifacts":"data/artifacts"},"execution":{"max_tasks":5,"max_retries":0}}),
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

impl Api {
    fn start() -> Result<Self> {
        Self::start_with_frontend(false)
    }

    fn start_with_frontend(frontend: bool) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        configure(dir.path())?;
        if frontend {
            fs::create_dir_all(dir.path().join("web/assets"))?;
            fs::write(
                dir.path().join("web/index.html"),
                "<!doctype html><title>Packaged app</title>",
            )?;
            fs::write(
                dir.path().join("web/assets/app.js"),
                "console.log('packaged app')",
            )?;
            fs::write(
                dir.path().join("web/assets/app.css"),
                "body { color: black }",
            )?;
            fs::write(dir.path().join("web/assets/font.woff2"), [0, 1, 2, 255])?;
        }
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        drop(listener);
        let mut command = Command::new(env!("CARGO_BIN_EXE_mini-amaru"));
        command.current_dir(dir.path()).args([
            "-c",
            "projects/test.yaml",
            "serve",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--cors-origin",
            "http://localhost:5173",
        ]);
        if frontend {
            command.args(["--web-dir", "web"]);
        }
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let mut api = Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
            client: Client::builder().timeout(Duration::from_secs(3)).build()?,
            _dir: dir,
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if let Some(status) = api.child.try_wait()? {
                anyhow::bail!("API process exited during startup: {status}");
            }
            if api
                .client
                .get(format!("{}/api/health", api.base))
                .send()
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(api);
            }
            thread::sleep(Duration::from_millis(25));
        }
        anyhow::bail!("API did not start within ten seconds")
    }

    fn get(&self, path: &str) -> Result<Response> {
        Ok(self.client.get(format!("{}{path}", self.base)).send()?)
    }
    fn post(&self, path: &str, body: &Value) -> Result<Response> {
        Ok(self
            .client
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()?)
    }
    fn wait_completed(&self, id: i64) -> Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let job: Value = self.get(&format!("/api/jobs/{id}"))?.json()?;
            if job["status"] == "COMPLETED" {
                return Ok(job);
            }
            anyhow::ensure!(
                job["status"] != "FAILED" && job["status"] != "BLOCKED",
                "Job failed: {job}"
            );
            thread::sleep(Duration::from_millis(25));
        }
        anyhow::bail!("job {id} did not complete within ten seconds")
    }
}

#[test]
fn packaged_frontend_serves_assets_spa_routes_and_same_origin_api() -> Result<()> {
    let api = Api::start_with_frontend(true)?;
    for route in ["/", "/new", "/runs", "/jobs/12"] {
        let response = api.get(route)?;
        assert_eq!(response.status(), 200, "{route}");
        assert_eq!(
            response.headers()["content-type"],
            "text/html; charset=utf-8"
        );
        assert!(response.text()?.contains("Packaged app"));
    }
    for (path, mime) in [
        ("/assets/app.js", "text/javascript; charset=utf-8"),
        ("/assets/app.css", "text/css; charset=utf-8"),
        ("/assets/font.woff2", "font/woff2"),
    ] {
        let response = api.get(path)?;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], mime);
    }
    assert_eq!(
        api.get("/assets/font.woff2")?.bytes()?.as_ref(),
        &[0, 1, 2, 255]
    );
    for path in ["/assets/missing.js", "/missing", "/api/missing"] {
        assert_eq!(api.get(path)?.status(), 404, "{path}");
    }
    let health: Value = api.get("/api/health")?.json()?;
    assert_eq!(health["ok"], true);
    let response = api
        .client
        .post(format!("{}/api/jobs", api.base))
        .header("Origin", &api.base)
        .json(&json!({"prompt":"Packaged frontend test"}))
        .send()?;
    assert_eq!(response.status(), 201);
    let response = api
        .client
        .get(format!("{}/", api.base))
        .header("Origin", "https://untrusted.example")
        .send()?;
    assert_eq!(response.status(), 403);
    Ok(())
}

#[test]
fn api_executes_and_exposes_jobs_events_logs_and_resume() -> Result<()> {
    let api = Api::start()?;
    for directory in ["data", "data/artifacts", "data/worktrees"] {
        assert!(api._dir.path().join(directory).is_dir(), "{directory}");
    }
    let health: Value = api.get("/api/health")?.json()?;
    assert_eq!(health, json!({"ok":true,"project":"api-test"}));
    assert_eq!(api.get("/api/jobs")?.json::<Value>()?["jobs"], json!([]));
    let response = api.post("/api/jobs", &json!({"prompt":"Implement an API test task"}))?;
    assert_eq!(response.status(), 201);
    let created: Value = response.json()?;
    assert_eq!(created["status"], "QUEUED");
    let id = created["id"].as_i64().context("missing id")?;
    let job = api.wait_completed(id)?;
    assert_eq!(job["kind"], "job");
    assert_eq!(job["tasks"][0]["status"], "COMPLETED");
    assert!(job["report"].as_str().is_some_and(|text| !text.is_empty()));
    assert_eq!(job["children"], json!([]));
    assert_eq!(job["actions"]["approve"], false);
    assert_eq!(
        api.get(&format!("/api/jobs/{id}/events?after=-1"))?
            .status(),
        400
    );
    assert_eq!(
        api.get(&format!("/api/jobs/{id}/log?offset=invalid"))?
            .status(),
        400
    );
    for action in ["approve", "pr", "cancel"] {
        assert_eq!(
            api.client
                .post(format!("{}/api/jobs/{id}/{action}", api.base))
                .send()?
                .status(),
            409
        );
    }
    let events: Value = api.get(&format!("/api/jobs/{id}/events?after=0"))?.json()?;
    let rows = events["events"].as_array().context("events missing")?;
    assert!(rows.iter().any(|event| event["event"] == "JOB_FINISHED"));
    let last = rows.last().context("no events")?["id"].as_i64().unwrap();
    assert_eq!(
        api.get(&format!("/api/jobs/{id}/events?after={last}"))?
            .json::<Value>()?["events"],
        json!([])
    );
    let log: Value = api.get(&format!("/api/jobs/{id}/log?offset=0"))?.json()?;
    assert!(
        log["text"]
            .as_str()
            .unwrap()
            .contains("API fake agent output")
    );
    let offset = log["offset"].as_u64().unwrap();
    let tail: Value = api
        .get(&format!("/api/jobs/{id}/log?offset={offset}"))?
        .json()?;
    assert_eq!(tail["text"], "");
    let resumed = api
        .client
        .post(format!("{}/api/jobs/{id}/resume", api.base))
        .send()?;
    assert_eq!(resumed.status(), 202);
    let resumed: Value = resumed.json()?;
    assert_eq!(resumed["resumed_from"], id);
    let new_id = resumed["id"].as_i64().unwrap();
    assert_ne!(new_id, id);
    api.wait_completed(new_id)?;
    assert_eq!(
        api.get("/api/jobs")?.json::<Value>()?["jobs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    Ok(())
}

#[test]
fn api_validates_requests_and_rejects_foreign_origins() -> Result<()> {
    let api = Api::start()?;
    assert_eq!(api.post("/api/jobs", &json!({}))?.status(), 400);
    assert_eq!(
        api.post("/api/jobs", &json!({"prompt":"   "}))?.status(),
        400
    );
    assert_eq!(
        api.post("/api/jobs", &json!({"prompt":"test","mode":"unknown"}))?
            .status(),
        400
    );
    assert_eq!(
        api.post("/api/jobs", &json!({"prompt":"test","milestone":{}}))?
            .status(),
        400
    );
    assert_eq!(
        api.post("/api/jobs", &json!({"prompt":"x".repeat(20_001)}))?
            .status(),
        400
    );
    assert_eq!(api.post("/api/jobs", &json!([]))?.status(), 400);
    assert_eq!(
        api.client
            .post(format!("{}/api/jobs", api.base))
            .header("Content-Type", "application/json")
            .body("{")
            .send()?
            .status(),
        400
    );
    assert_eq!(
        api.client
            .post(format!("{}/api/jobs", api.base))
            .body("prompt=test")
            .send()?
            .status(),
        415
    );
    assert_eq!(
        api.post("/api/jobs", &json!({"prompt":"x".repeat(65_537)}))?
            .status(),
        413
    );
    assert_eq!(api.get("/api/jobs/999")?.status(), 404);
    assert_eq!(api.get("/api/jobs/nope")?.status(), 404);
    assert_eq!(api.get("/missing")?.status(), 404);
    let allowed = api
        .client
        .get(format!("{}/api/health", api.base))
        .header("Origin", "http://localhost:5173")
        .send()?;
    assert_eq!(allowed.status(), 200);
    assert_eq!(
        allowed.headers()["Access-Control-Allow-Origin"],
        "http://localhost:5173"
    );
    let rejected = api
        .client
        .post(format!("{}/api/jobs/1/approve", api.base))
        .header("Origin", "https://untrusted.example")
        .send()?;
    assert_eq!(rejected.status(), 403);
    assert!(
        rejected
            .headers()
            .get("Access-Control-Allow-Origin")
            .is_none()
    );
    let preflight = api
        .client
        .request(reqwest::Method::OPTIONS, format!("{}/api/jobs", api.base))
        .header("Origin", "http://localhost:5173")
        .send()?;
    assert_eq!(preflight.status(), 204);
    assert_eq!(
        preflight.headers()["Access-Control-Allow-Methods"],
        "GET, POST, OPTIONS"
    );
    Ok(())
}
