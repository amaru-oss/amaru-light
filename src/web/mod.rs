//! The web interface: a small page and the JSON API it uses.
//!
//! `amaru serve` starts it next to the scheduler. Both share the services,
//! so an action taken on the page is the same as the one on the command
//! line. Set `AMARU_WEB_TOKEN` to require a token for the API.

mod agents;
mod api;
mod assets;

use crate::{lifecycle::Services, login::Logins, scheduler::log};
use anyhow::{Context, Result};
use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::watch;

/// The environment variable that holds the API token, if any.
pub const TOKEN_ENV: &str = "AMARU_WEB_TOKEN";

/// The header every request that changes something must carry. A page on
/// another site cannot send it without a CORS preflight, which is refused.
const ACTION_HEADER: &str = "x-amaru-action";

#[derive(Clone, Debug)]
struct Web {
    svc: Arc<Services>,
    token: Option<String>,
    logins: Arc<Logins>,
}

/// Build the routes, with `token` required by the API when set.
pub fn router(svc: Arc<Services>, token: Option<String>) -> Router {
    let web = Web {
        svc,
        token,
        logins: Arc::default(),
    };
    let api = api::routes()
        .merge(agents::routes())
        .layer(middleware::from_fn_with_state(web.clone(), guard));
    assets::routes().merge(api).with_state(web)
}

/// Listen on `web.listen` and serve the page in the background until `stop`
/// is set. Fails at once when the address cannot be used.
pub async fn start(
    svc: Arc<Services>,
    mut stop: watch::Receiver<bool>,
) -> Result<tokio::task::JoinHandle<()>> {
    let listen = svc.settings.daemon.web.listen.clone();
    let addr: SocketAddr = listen.parse().context("invalid web.listen")?;
    let token = std::env::var(TOKEN_ENV).ok().filter(|t| !t.is_empty());
    if token.is_none() && !addr.ip().is_loopback() {
        log(&format!(
            "web: listening on {addr} without {TOKEN_ENV}; anyone who reaches this port can act on tickets"
        ));
    }
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("cannot listen on {addr} (web.listen)"))?;
    log(&format!("web: http://{addr}"));
    let app = router(svc, token);
    Ok(tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            // A dropped sender also ends the server.
            let _ = stop.wait_for(|s| *s).await;
        });
        if let Err(e) = server.await {
            log(&format!("web: stopped: {e}"));
        }
    }))
}

/// Check the token and, for changes, the action header.
async fn guard(State(web): State<Web>, request: Request, next: Next) -> Response {
    if let Some(token) = &web.token
        && request.uri().path() != "/api/health"
        && !authorized(request.headers(), token)
    {
        return error(StatusCode::UNAUTHORIZED, "a valid token is required");
    }
    if request.method() != Method::GET && !request.headers().contains_key(ACTION_HEADER) {
        return error(StatusCode::FORBIDDEN, "missing the x-amaru-action header");
    }
    next.run(request).await
}

fn authorized(headers: &HeaderMap, token: &str) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|given| same(given.as_bytes(), token.as_bytes()))
}

/// Compare without stopping at the first difference.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "error": message });
    (status, axum::Json(body)).into_response()
}

#[cfg(test)]
mod tests;
