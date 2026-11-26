//! The JSON API under `/api`.

use super::Web;
use crate::{
    actions::{self, Claimed},
    db::{Event, Listed},
    lifecycle::Services,
    settings::ProjectSettings,
    ticket::State as TicketState,
};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Tickets returned at most by one list request.
const MAX_TICKETS: i64 = 500;
/// Events returned at most by one request; ask again with `after`.
const MAX_EVENTS: usize = 1_000;

pub(super) fn routes() -> Router<Web> {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/info", get(info))
        .route("/api/tickets", get(tickets).post(claim))
        .route("/api/tickets/{id}", get(ticket))
        .route("/api/tickets/{id}/events", get(events))
        .route("/api/tickets/{id}/unblock", post(unblock))
        .route("/api/tickets/{id}/guide", post(guide))
        .route("/api/tickets/{id}/cancel", post(cancel))
}

/// An error with the HTTP status it maps to.
#[derive(Debug)]
pub(super) struct ApiError(StatusCode, String);

impl ApiError {
    pub(super) const fn new(status: StatusCode, message: String) -> Self {
        Self(status, message)
    }

    pub(super) fn rejected(error: &anyhow::Error) -> Self {
        Self(StatusCode::UNPROCESSABLE_ENTITY, format!("{error:#}"))
    }

    fn not_found(id: i64) -> Self {
        Self(StatusCode::NOT_FOUND, format!("no ticket {id}"))
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        Self(StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

pub(super) type ApiResult<T> = Result<Json<T>, ApiError>;

/// A ticket with links for the page.
#[derive(Debug, Serialize)]
struct TicketView {
    #[serde(flatten)]
    listed: Listed,
    /// `owner/name#42` or the Jira key.
    reference: String,
    issue_url: String,
    pr_url: Option<String>,
    branch_url: Option<String>,
}

fn view(svc: &Services, listed: Listed) -> TicketView {
    let t = &listed.ticket;
    let issue_url = svc.settings.project(&t.repo).map_or_else(
        || format!("https://github.com/{}/issues/{}", t.repo, t.number),
        |p| p.issue_url(t.number),
    );
    let pr_url = t
        .pr_number
        .map(|n| format!("https://github.com/{}/pull/{n}", t.repo));
    let push_repo = svc
        .settings
        .project(&t.repo)
        .and_then(|p| p.fork.clone())
        .unwrap_or_else(|| t.repo.clone());
    let branch_url = t
        .branch
        .as_ref()
        .map(|b| format!("https://github.com/{push_repo}/tree/{b}"));
    TicketView {
        reference: listed.ticket.reference(),
        listed,
        issue_url,
        pr_url,
        branch_url,
    }
}

async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

fn project(p: &ProjectSettings) -> Value {
    json!({
        "repo": p.repo,
        "fork": p.fork,
        "jira": p.jira.as_ref().map(|j| format!("{} ({})", j.project, j.url)),
        "base_branch": p.base_branch,
        "agent": p.agent,
        "labels": p.intake.labels,
        "assignee": p.intake.assignee,
        "checks": p.checks.iter().map(|c| &c.name).collect::<Vec<_>>(),
        "self_review": format!("{:?}", p.review.self_review),
        "reviewers": p.review.reviewers,
    })
}

async fn info(State(web): State<Web>) -> Json<Value> {
    let settings = &web.svc.settings;
    let projects: Vec<Value> = settings.projects.iter().map(project).collect();
    let states: Vec<Value> = TicketState::ALL
        .iter()
        .map(|s| json!({ "name": s, "active": s.needs_slot(), "terminal": s.is_terminal() }))
        .collect();
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "poll_interval_seconds": settings.daemon.poll_interval.0.as_secs(),
        "max_parallel": settings.daemon.max_parallel,
        "bot_login": settings.daemon.bot_login,
        "projects": projects,
        "states": states,
    }))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    limit: Option<i64>,
}

async fn tickets(State(web): State<Web>, Query(q): Query<ListQuery>) -> ApiResult<Vec<TicketView>> {
    let limit = q.limit.unwrap_or(200).clamp(1, MAX_TICKETS);
    let all = web.svc.store.listed(limit)?;
    Ok(Json(all.into_iter().map(|l| view(&web.svc, l)).collect()))
}

fn load(web: &Web, id: i64) -> Result<TicketView, ApiError> {
    let listed = web.svc.store.get_listed(id)?;
    Ok(view(
        &web.svc,
        listed.ok_or_else(|| ApiError::not_found(id))?,
    ))
}

async fn ticket(State(web): State<Web>, Path(id): Path<i64>) -> ApiResult<TicketView> {
    Ok(Json(load(&web, id)?))
}

#[derive(Debug, Deserialize)]
struct EventQuery {
    after: Option<i64>,
}

async fn events(
    State(web): State<Web>,
    Path(id): Path<i64>,
    Query(q): Query<EventQuery>,
) -> ApiResult<Vec<Event>> {
    load(&web, id)?;
    let mut all = web.svc.store.events(id, q.after.unwrap_or(0))?;
    all.truncate(MAX_EVENTS);
    Ok(Json(all))
}

#[derive(Debug, Deserialize)]
struct ClaimBody {
    issue: String,
}

async fn claim(State(web): State<Web>, Json(body): Json<ClaimBody>) -> Response {
    let (status, outcome, ticket) = match actions::claim(&web.svc, &body.issue).await {
        Ok(Claimed::Queued(t)) => (StatusCode::CREATED, "queued", t),
        Ok(Claimed::Exists(t)) => (StatusCode::OK, "exists", t),
        Err(e) => return ApiError::rejected(&e).into_response(),
    };
    match load(&web, ticket.id) {
        Ok(view) => (status, Json(json!({ "outcome": outcome, "ticket": view }))).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn unblock(State(web): State<Web>, Path(id): Path<i64>) -> ApiResult<TicketView> {
    load(&web, id)?;
    actions::resume(&web.svc, id)
        .await
        .map_err(|e| ApiError::rejected(&e))?;
    Ok(Json(load(&web, id)?))
}

async fn cancel(State(web): State<Web>, Path(id): Path<i64>) -> ApiResult<TicketView> {
    load(&web, id)?;
    actions::cancel(&web.svc, id).map_err(|e| ApiError::rejected(&e))?;
    Ok(Json(load(&web, id)?))
}

#[derive(Debug, Deserialize)]
struct GuideBody {
    text: String,
}

async fn guide(
    State(web): State<Web>,
    Path(id): Path<i64>,
    Json(body): Json<GuideBody>,
) -> ApiResult<TicketView> {
    load(&web, id)?;
    actions::guide(&web.svc, id, &body.text, "web")
        .await
        .map_err(|e| ApiError::rejected(&e))?;
    Ok(Json(load(&web, id)?))
}
