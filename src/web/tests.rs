use super::*;
use crate::testing::Fake;
use axum::{
    body::{Body, to_bytes},
    http::Request,
};
use serde_json::Value;
use tower::ServiceExt;

async fn call(app: &Router, request: Request<Body>) -> (StatusCode, Value) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

fn get(path: &str) -> Request<Body> {
    Request::get(path).body(Body::empty()).unwrap()
}

fn post(path: &str, body: &str) -> Request<Body> {
    Request::post(path)
        .header(header::CONTENT_TYPE, "application/json")
        .header(ACTION_HEADER, "1")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn claims_lists_and_acts_on_tickets() {
    let fake = Fake::new("").await;
    let app = router(Arc::clone(&fake.svc), None);
    let (status, body) = call(&app, post("/api/tickets", r#"{"issue":"me/app#4"}"#)).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(body["outcome"], "queued");
    assert_eq!(
        body["ticket"]["issue_url"],
        "https://github.com/me/app/issues/4"
    );
    let id = body["ticket"]["id"].as_i64().unwrap();
    let (_, body) = call(&app, post("/api/tickets", r#"{"issue":"me/app#4"}"#)).await;
    assert_eq!(body["outcome"], "exists");
    let (status, list) = call(&app, get("/api/tickets")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(list[0]["state"], "QUEUED");
    let (_, events) = call(&app, get(&format!("/api/tickets/{id}/events"))).await;
    assert_eq!(events[0]["kind"], "CLAIMED");
    let (status, body) = call(&app, post(&format!("/api/tickets/{id}/unblock"), "")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(body["error"].as_str().unwrap().contains("not blocked"));
    let (status, body) = call(&app, post(&format!("/api/tickets/{id}/cancel"), "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "CANCELLED");
    let (status, _) = call(&app, get("/api/tickets/99")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn keeps_guidance_for_the_agent() {
    let fake = Fake::new("").await;
    let app = router(Arc::clone(&fake.svc), None);
    let id = fake.claim(4).await.id;
    let guide = format!("/api/tickets/{id}/guide");
    let (status, _) = call(&app, post(&guide, r#"{"text":"  "}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let (status, body) = call(&app, post(&guide, r#"{"text":"use ring"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"], "QUEUED");
    let pending = fake.svc.store.pending_guidance(id).unwrap();
    assert_eq!(
        (pending[0].by.as_str(), pending[0].text.as_str()),
        ("web", "use ring")
    );
    let (status, _) = call(&app, post("/api/tickets/99/guide", r#"{"text":"x"}"#)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn reports_settings_and_serves_the_page() {
    let fake = Fake::new("").await;
    let app = router(Arc::clone(&fake.svc), None);
    let (status, info) = call(&app, get("/api/info")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(info["projects"][0]["repo"], "me/app");
    let response = app.clone().oneshot(get("/")).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let policy = response.headers().get(header::CONTENT_SECURITY_POLICY);
    assert!(policy.is_some());
}

#[tokio::test]
async fn refuses_changes_without_the_action_header() {
    let fake = Fake::new("").await;
    let app = router(Arc::clone(&fake.svc), None);
    let request = Request::post("/api/tickets")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"issue":"me/app#4"}"#))
        .unwrap();
    let (status, _) = call(&app, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(fake.svc.store.recent(10).unwrap().is_empty());
}

#[tokio::test]
async fn requires_the_token_when_one_is_set() {
    let fake = Fake::new("").await;
    let app = router(Arc::clone(&fake.svc), Some("s3cret".into()));
    let (status, _) = call(&app, get("/api/tickets")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = call(&app, get("/api/health")).await;
    assert_eq!(status, StatusCode::OK);
    let wrong = Request::get("/api/tickets")
        .header(header::AUTHORIZATION, "Bearer s3cres")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&app, wrong).await.0, StatusCode::UNAUTHORIZED);
    let right = Request::get("/api/tickets")
        .header(header::AUTHORIZATION, "Bearer s3cret")
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(&app, right).await.0, StatusCode::OK);
    assert_eq!(call(&app, get("/")).await.0, StatusCode::OK);
}

#[test]
fn compares_tokens_fully() {
    assert!(same(b"abc", b"abc"));
    assert!(!same(b"abc", b"abd"));
    assert!(!same(b"abc", b"abcd"));
}

#[tokio::test]
async fn lists_agents_and_their_sign_in() {
    let fake = Fake::new("").await;
    let app = router(Arc::clone(&fake.svc), None);
    let (status, list) = call(&app, get("/api/agents")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(list[0]["name"], "claude");
    assert_eq!(list[0]["driver"], "claude");
    assert!(
        list[0]["login_command"]
            .as_str()
            .unwrap()
            .ends_with(" auth login")
    );
    assert_eq!(list[0]["used_by"][0], "me/app");
    assert!(list[0]["login"].is_null());
    let (status, _) = call(&app, get("/api/agents/claude/login")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, get("/api/agents/nope/status")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, post("/api/agents/claude/login/cancel", "")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn refuses_a_key_sign_in_the_agent_lacks() {
    let fake = Fake::new("").await;
    let app = router(Arc::clone(&fake.svc), None);
    let (_, list) = call(&app, get("/api/agents")).await;
    assert!(list[0]["key_login_command"].is_null());
    let (status, body) = call(&app, post("/api/agents/claude/login", r#"{"key":"sk-1"}"#)).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(!body.to_string().contains("sk-1"));
}
