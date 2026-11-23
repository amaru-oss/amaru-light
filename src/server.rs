//! Local JSON API. Agent work is serialized on a dedicated worker thread.
use crate::{config::Settings, delivery, joblog, runtime, storage::Store};
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::{
    io::Read,
    path::{Component, Path, PathBuf},
    sync::mpsc,
    thread,
};
use tiny_http::{Header, Request, Response, Server, StatusCode};

#[derive(Debug)]
enum Work {
    Job(i64, String),
    Loop(i64, String),
    Publish(i64),
}

pub fn actions(job: &Value) -> Value {
    let work = job["kind"] == "work";
    let status = job["status"].as_str().unwrap_or("");
    let has_pr = job["pr_number"].as_i64().is_some_and(|n| n > 0);
    let branch = job["branch"].as_str().is_some_and(|s| !s.is_empty());
    json!({
        "approve": work && has_pr && matches!(status, "WAITING_MERGE" | "BLOCKED"),
        "create_pr": work && branch && !has_pr && matches!(status, "WAITING_PR" | "BLOCKED"),
        "resume": job["kind"] == "loop" && matches!(status, "WAITING_MERGE" | "BLOCKED" | "STOPPED" | "INTERRUPTED" | "FAILED" | "CANCELLED"),
        "cancel": job["kind"] == "loop" && matches!(status, "RUNNING" | "QUEUED")
    })
}

fn error(status: u16, message: impl ToString) -> (u16, Value) {
    (status, json!({"error":message.to_string()}))
}

fn query_number(query: &str, key: &str) -> Result<i64> {
    let raw = query
        .split('&')
        .find_map(|part| {
            part.split_once('=')
                .filter(|(k, _)| *k == key)
                .map(|(_, v)| v)
        })
        .unwrap_or("0");
    raw.parse::<i64>()
        .ok()
        .filter(|n| *n >= 0)
        .ok_or_else(|| anyhow!("invalid '{key}' parameter"))
}

fn enqueue(
    settings: &Settings,
    store: &Store,
    tx: &mpsc::Sender<Work>,
    prompt: &str,
    mode: &str,
    milestone: Option<&str>,
) -> Result<i64> {
    let id = if mode == "loop" {
        delivery::create_loop_job(settings, store, prompt, milestone, "QUEUED")?
    } else {
        store.create_job(&settings.project_name(), prompt, "QUEUED", "job", None)?
    };
    store.event(
        id,
        "JOB_QUEUED",
        &json!({"prompt":prompt,"mode":mode,"milestone":milestone}),
    )?;
    let work = if mode == "loop" {
        Work::Loop(id, prompt.into())
    } else {
        Work::Job(id, prompt.into())
    };
    tx.send(work)?;
    Ok(id)
}

fn route(
    settings: &Settings,
    tx: &mpsc::Sender<Work>,
    method: &str,
    url: &str,
    body: Option<&Value>,
) -> Result<(u16, Value)> {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    let parts: Vec<_> = path.split('/').filter(|p| !p.is_empty()).collect();
    let store = Store::open(settings.db_path())?;
    match (method, parts.as_slice()) {
        ("GET", ["api", "health"]) => {
            return Ok((200, json!({"ok":true,"project":settings.project_name()})));
        }
        ("GET", ["api", "jobs"]) => return Ok((200, json!({"jobs":store.jobs()?}))),
        ("POST", ["api", "jobs"]) => {
            let body = body.unwrap_or(&Value::Null);
            let Some(prompt) = body["prompt"]
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            else {
                return Ok(error(400, "'prompt' is required"));
            };
            if prompt.chars().count() > 20_000 {
                return Ok(error(400, "prompt exceeds 20000 characters"));
            }
            let mode = body.get("mode").and_then(Value::as_str).unwrap_or("job");
            if !matches!(mode, "job" | "loop") || body.get("mode").is_some_and(|v| !v.is_string()) {
                return Ok(error(400, "mode must be job or loop"));
            }
            let milestone = match body.get("milestone") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) if s.chars().count() <= 120 => Some(s.clone()),
                Some(Value::Number(n)) if n.is_i64() || n.is_u64() => Some(n.to_string()),
                _ => return Ok(error(400, "invalid milestone")),
            };
            let id = enqueue(
                settings,
                &store,
                tx,
                prompt,
                mode,
                milestone.as_deref().filter(|s| !s.is_empty()),
            )?;
            return Ok((201, json!({"id":id,"status":"QUEUED","kind":mode})));
        }
        _ => {}
    }
    if parts.len() < 3 || parts[0] != "api" || parts[1] != "jobs" || parts.len() > 4 {
        return Ok(error(404, "route not found"));
    }
    let Some(id) = parts[2].parse::<i64>().ok().filter(|n| *n > 0) else {
        return Ok(error(404, "invalid job"));
    };
    let Some(mut job) = store.job(id)? else {
        return Ok(error(404, "job not found"));
    };
    let action = parts.get(3).copied().unwrap_or("");
    match (method, action) {
        ("GET", "") => {
            let mut available = actions(&job);
            if delivery::requires_upstream_merge(settings, &job) {
                available["approve"] = json!(false);
            }
            job["actions"] = available;
            job["tasks"] = json!(store.tasks(id)?);
            job["children"] = json!(store.children(id)?);
            Ok((200, job))
        }
        ("GET", "events") => match query_number(query, "after") {
            Ok(after) => Ok((200, json!({"events":store.events(id,after)?}))),
            Err(e) => Ok(error(400, e)),
        },
        ("GET", "log") => match query_number(query, "offset") {
            Ok(offset) => Ok((
                200,
                joblog::read_log(
                    &joblog::log_path(&settings.artifacts_dir(), id),
                    offset as u64,
                )?,
            )),
            Err(e) => Ok(error(400, e)),
        },
        ("POST", "resume") => {
            if job["kind"] == "work" {
                return Ok(error(409, "resume the parent loop"));
            }
            if job["kind"] == "loop" {
                if matches!(
                    job["status"].as_str(),
                    Some("RUNNING" | "QUEUED" | "CANCELLING")
                ) {
                    return Ok(error(409, "loop is already active"));
                }
                store.set_job(id, "QUEUED", None, None)?;
                store.event(id, "JOB_QUEUED", &json!({"resume":true}))?;
                tx.send(Work::Loop(id, job["prompt"].as_str().unwrap_or("").into()))?;
                Ok((202, json!({"id":id,"status":"QUEUED","resumed":true})))
            } else {
                let prompt = runtime::resume_prompt(job["prompt"].as_str().unwrap_or(""), id);
                let new_id = enqueue(settings, &store, tx, &prompt, "job", None)?;
                Ok((
                    202,
                    json!({"id":new_id,"status":"QUEUED","resumed_from":id}),
                ))
            }
        }
        ("POST", "cancel") => {
            if actions(&job)["cancel"] != true {
                return Ok(error(409, "only active loops can be cancelled"));
            }
            store.set_job(id, "CANCELLING", None, None)?;
            store.event(id, "CANCEL_REQUESTED", &json!({}))?;
            Ok((202, json!({"id":id,"status":"CANCELLING"})))
        }
        ("POST", "pr") => {
            if actions(&job)["create_pr"] != true {
                return Ok(error(409, "item is not ready for a PR"));
            }
            store.event(id, "CREATE_PR_REQUESTED", &json!({}))?;
            // Reserve this action before queueing it, preventing duplicate publication requests.
            store.set_job(id, "QUEUED_PR", None, None)?;
            tx.send(Work::Publish(id))?;
            Ok((202, json!({"id":id,"status":"QUEUED_PR"})))
        }
        ("POST", "approve") => {
            if actions(&job)["approve"] != true {
                return Ok(error(409, "item has no PR awaiting approval"));
            }
            match delivery::approve_pr(settings, id) {
                Ok(()) => {
                    let job = store.job(id)?.ok_or_else(|| anyhow!("job disappeared"))?;
                    Ok((
                        200,
                        json!({"id":id,"status":job["status"],"merge_state":job["merge_state"],"pr_url":job["pr_url"]}),
                    ))
                }
                Err(e) => Ok(error(409, e)),
            }
        }
        _ => Ok(error(404, "route not found")),
    }
}

fn header(request: &Request, name: &str) -> Option<String> {
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str().to_owned())
}

fn handle(
    settings: &Settings,
    tx: &mpsc::Sender<Work>,
    origins: &[String],
    request: &mut Request,
) -> (u16, Value) {
    let method = request.method().as_str();
    // Validate Origin on the server as well as emitting CORS response headers.
    if header(request, "Origin").is_some_and(|origin| !origins.contains(&origin)) {
        return error(403, "origin is not allowed");
    }
    if method == "OPTIONS" {
        return (204, Value::Null);
    }
    let method = method.to_owned();
    let url = request.url().to_owned();
    let mut body = None;
    if method == "POST" && url.split('?').next() == Some("/api/jobs") {
        if header(request, "Content-Type")
            .as_deref()
            .and_then(|s| s.split(';').next())
            .map(str::trim)
            != Some("application/json")
        {
            return error(415, "Content-Type must be application/json");
        }
        if request.body_length().is_some_and(|n| n > 65_536) {
            return error(413, "request body too large");
        }
        let mut bytes = Vec::new();
        if let Err(e) = request.as_reader().take(65_537).read_to_end(&mut bytes) {
            return error(400, e);
        }
        if bytes.len() > 65_536 {
            return error(413, "request body too large");
        }
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) if value.is_object() => body = Some(value),
            _ => return error(400, "body must be a JSON object"),
        }
    }
    match route(settings, tx, &method, &url, body.as_ref()) {
        Ok(response) => response,
        Err(e) => error(500, e),
    }
}

fn web_file(root: &Path, url: &str) -> Result<Option<(Vec<u8>, &'static str)>> {
    let path = url.split('?').next().unwrap_or(url);
    // Build output has no percent-encoded names. Reject encoded traversal and platform separators.
    if path.contains(['%', '\\', '\0']) {
        return Ok(None);
    }
    let relative = Path::new(path.trim_start_matches('/'));
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Ok(None);
    }
    let requested = root.join(relative);
    let file = if requested.is_file() {
        requested
    } else if matches!(path, "/" | "/new" | "/runs")
        || path
            .strip_prefix("/jobs/")
            .is_some_and(|id| !id.is_empty() && id.bytes().all(|c| c.is_ascii_digit()))
    {
        root.join("index.html")
    } else {
        return Ok(None);
    };
    let canonical = file.canonicalize()?;
    if !canonical.starts_with(root) {
        return Ok(None);
    }
    let mime = match canonical
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
    {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    };
    Ok(Some((std::fs::read(canonical)?, mime)))
}

pub fn serve(
    settings: Settings,
    host: &str,
    port: u16,
    mut origins: Vec<String>,
    web_dir: Option<PathBuf>,
) -> Result<()> {
    crate::workspace::ensure_runtime_dirs(&settings)?;
    let web_dir = web_dir
        .map(|dir| -> Result<PathBuf> {
            let dir = dir.canonicalize()?;
            anyhow::ensure!(
                dir.join("index.html").is_file(),
                "web directory must contain index.html"
            );
            Ok(dir)
        })
        .transpose()?;
    let server = Server::http((host, port)).map_err(|e| anyhow!(e.to_string()))?;
    if web_dir.is_some() {
        origins.push(format!("http://{}", server.server_addr()));
        if let Some(address) = server
            .server_addr()
            .to_ip()
            .filter(|address| address.ip().is_loopback())
        {
            origins.push(format!("http://localhost:{}", address.port()));
        }
    }
    Store::open(settings.db_path())?.mark_interrupted()?;
    let (tx, rx) = mpsc::channel::<Work>();
    let worker_settings = settings.clone();
    thread::Builder::new()
        .name("mini-amaru-worker".into())
        .spawn(move || {
            while let Ok(work) = rx.recv() {
                let id = match &work {
                    Work::Job(id, _) | Work::Loop(id, _) | Work::Publish(id) => *id,
                };
                let result = match work {
                    Work::Job(id, prompt) => {
                        runtime::run_job(&worker_settings, &prompt, Some(id)).map(|_| ())
                    }
                    Work::Loop(id, goal) => {
                        delivery::run_loop(&worker_settings, &goal, Some(id), None).map(|_| ())
                    }
                    Work::Publish(id) => delivery::create_pr_now(&worker_settings, id),
                };
                if let Err(e) = result {
                    eprintln!("Job {id} failed: {e:#}");
                    if let Ok(store) = Store::open(worker_settings.db_path()) {
                        let _ = store.set_job(id, "FAILED", None, Some(&e.to_string()));
                    }
                }
            }
        })?;
    println!(
        "Mini-Amaru {} at http://{} (project: {})",
        if web_dir.is_some() {
            "web and API"
        } else {
            "API"
        },
        server.server_addr(),
        settings.project_name()
    );
    for mut request in server.incoming_requests() {
        let origin = header(&request, "Origin").filter(|o| origins.contains(o));
        let path = request.url().split('?').next().unwrap_or(request.url());
        if request.method().as_str() == "GET"
            && !path.starts_with("/api/")
            && path != "/api"
            && let Some(root) = &web_dir
            && !header(&request, "Origin").is_some_and(|o| !origins.contains(&o))
        {
            let response = match web_file(root, request.url()) {
                Ok(Some((data, mime))) => Response::from_data(data).with_header(
                    Header::from_bytes("Content-Type", mime).expect("static MIME type"),
                ),
                Ok(None) => {
                    Response::from_data(b"Not found".to_vec()).with_status_code(StatusCode(404))
                }
                Err(e) => {
                    eprintln!("Frontend file error: {e}");
                    Response::from_data(b"Cannot read frontend file".to_vec())
                        .with_status_code(StatusCode(500))
                }
            };
            if let Err(e) = request.respond(response) {
                eprintln!("HTTP response error: {e}");
            }
            continue;
        }
        let (status, body) = handle(&settings, &tx, &origins, &mut request);
        let mut response = Response::from_string(if status == 204 {
            String::new()
        } else {
            body.to_string()
        })
        .with_status_code(StatusCode(status));
        response.add_header(
            Header::from_bytes("Content-Type", "application/json").expect("static header"),
        );
        if let Some(origin) = origin {
            for (key, value) in [
                ("Access-Control-Allow-Origin", origin.as_str()),
                ("Access-Control-Allow-Methods", "GET, POST, OPTIONS"),
                ("Access-Control-Allow-Headers", "Content-Type"),
                ("Vary", "Origin"),
            ] {
                if let Ok(h) = Header::from_bytes(key, value) {
                    response.add_header(h);
                }
            }
        }
        if let Err(e) = request.respond(response) {
            eprintln!("HTTP response error: {e}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frontend_rejects_traversal_and_symlink_escape() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("web");
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("index.html"), "app")?;
        std::fs::write(directory.path().join("secret.txt"), "secret")?;
        let root = root.canonicalize()?;
        for path in [
            "/../secret.txt",
            "/%2e%2e/secret.txt",
            "/..\\secret.txt",
            "/assets/../../secret.txt",
        ] {
            assert!(web_file(&root, path)?.is_none(), "{path}");
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                directory.path().join("secret.txt"),
                root.join("linked.txt"),
            )?;
            assert!(web_file(&root, "/linked.txt")?.is_none());
        }
        Ok(())
    }
    #[test]
    fn actions_match_workflow_state() {
        let work = json!({"kind":"work","status":"WAITING_PR","branch":"feat/1","pr_number":null});
        assert_eq!(actions(&work)["create_pr"], true);
        assert_eq!(actions(&work)["approve"], false);
        let pr = json!({"kind":"work","status":"WAITING_MERGE","pr_number":12});
        assert_eq!(actions(&pr)["approve"], true);
        assert_eq!(actions(&pr)["cancel"], false);
        assert_eq!(
            actions(&json!({"kind":"loop","status":"RUNNING"}))["cancel"],
            true
        );
    }
    #[test]
    fn offsets_reject_negative_and_invalid_input() {
        assert_eq!(query_number("after=12", "after").unwrap(), 12);
        assert_eq!(query_number("", "after").unwrap(), 0);
        assert!(query_number("after=-1", "after").is_err());
        assert!(query_number("after=abc", "after").is_err());
    }
}
