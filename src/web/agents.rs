//! Agent CLIs under `/api/agents`: whether each is signed in, and signing
//! it in from the page.

use super::{
    Web,
    api::{ApiError, ApiResult},
};
use crate::{
    install,
    login::{self, LoginStatus, LoginView},
    settings::AgentSettings,
};
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};

pub(super) fn routes() -> Router<Web> {
    Router::new()
        .route("/api/agents", get(agents))
        .route("/api/agents/{name}/status", get(status))
        .route("/api/agents/{name}/login", get(progress).post(start))
        .route("/api/agents/{name}/login/input", post(input))
        .route("/api/agents/{name}/login/cancel", post(cancel))
        .route(
            "/api/agents/{name}/install",
            get(install_progress).post(install),
        )
        .route("/api/agents/{name}/install/cancel", post(install_cancel))
}

fn agent<'a>(web: &'a Web, name: &str) -> Result<&'a AgentSettings, ApiError> {
    web.svc
        .settings
        .daemon
        .agents
        .get(name)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no agent {name}")))
}

async fn agents(State(web): State<Web>) -> Json<Vec<Value>> {
    let settings = &web.svc.settings;
    let list = settings
        .daemon
        .agents
        .iter()
        .map(|(name, a)| {
            let used_by: Vec<&str> = settings
                .projects
                .iter()
                .filter(|p| &p.agent == name)
                .map(|p| p.repo.as_str())
                .collect();
            card(&web, name, a, &used_by)
        })
        .collect();
    Json(list)
}

/// What the page shows of one agent.
fn card(web: &Web, name: &str, a: &AgentSettings, used_by: &[&str]) -> Value {
    let install_command = a.install_command();
    json!({
        "name": name,
                "driver": a.driver,
                "command": a.command,
                "login_command": command_line(a, &a.login_args()),
                "key_login_command": command_line(a, &a.key_login_args()),
                "status_command": command_line(a, &a.status_args()),
                "used_by": used_by,
                "login": web.logins.get(name),
                "installed": install::installed(a),
                "install_command": (!install_command.is_empty()).then(|| install_command.join(" ")),
                "install": web.installs.get(name),
        "install_notice": install::notice(a),
    })
}

/// The command as the operator would type it, or `None` when turned off.
fn command_line(agent: &AgentSettings, args: &[String]) -> Option<String> {
    (!args.is_empty()).then(|| format!("{} {}", agent.command, args.join(" ")))
}

async fn status(State(web): State<Web>, Path(name): Path<String>) -> ApiResult<LoginStatus> {
    Ok(Json(login::status(agent(&web, &name)?).await))
}

async fn progress(State(web): State<Web>, Path(name): Path<String>) -> ApiResult<LoginView> {
    agent(&web, &name)?;
    web.logins
        .get(&name)
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no sign-in for {name}")))
}

#[derive(Debug, Default, Deserialize)]
struct StartBody {
    /// An API key, for the agent's key sign-in. Never logged or returned.
    #[serde(default)]
    key: Option<String>,
}

async fn start(
    State(web): State<Web>,
    Path(name): Path<String>,
    body: Option<Json<StartBody>>,
) -> ApiResult<LoginView> {
    let settings = agent(&web, &name)?;
    let key = body
        .and_then(|Json(b)| b.key)
        .filter(|k| !k.trim().is_empty());
    let view = web
        .logins
        .start(&name, settings, key.as_deref())
        .await
        .map_err(|e| ApiError::rejected(&e))?;
    Ok(Json(view))
}

#[derive(Debug, Deserialize)]
struct InputBody {
    text: String,
}

async fn input(
    State(web): State<Web>,
    Path(name): Path<String>,
    Json(body): Json<InputBody>,
) -> ApiResult<LoginView> {
    agent(&web, &name)?;
    web.logins
        .input(&name, &body.text)
        .await
        .map_err(|e| ApiError::rejected(&e))?;
    progress(State(web), Path(name)).await
}

async fn cancel(State(web): State<Web>, Path(name): Path<String>) -> ApiResult<LoginView> {
    agent(&web, &name)?;
    web.logins
        .cancel(&name)
        .map_err(|e| ApiError::rejected(&e))?;
    progress(State(web), Path(name)).await
}

async fn install_progress(
    State(web): State<Web>,
    Path(name): Path<String>,
) -> ApiResult<LoginView> {
    agent(&web, &name)?;
    web.installs
        .get(&name)
        .map(Json)
        .ok_or_else(|| ApiError::new(StatusCode::NOT_FOUND, format!("no install for {name}")))
}

async fn install(State(web): State<Web>, Path(name): Path<String>) -> ApiResult<LoginView> {
    let settings = agent(&web, &name)?;
    let view = install::start(&web.installs, &web.svc.settings.base_dir, &name, settings)
        .map_err(|e| ApiError::rejected(&e))?;
    Ok(Json(view))
}

async fn install_cancel(State(web): State<Web>, Path(name): Path<String>) -> ApiResult<LoginView> {
    agent(&web, &name)?;
    web.installs
        .cancel(&name)
        .map_err(|e| ApiError::rejected(&e))?;
    install_progress(State(web), Path(name)).await
}
