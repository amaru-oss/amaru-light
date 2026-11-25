//! The page itself: HTML, CSS, script and logo, compiled into the binary.

use super::Web;
use axum::{
    Router,
    http::header,
    response::{IntoResponse, Response},
    routing::get,
};

const INDEX: &str = include_str!("../../web/index.html");
const STYLE: &str = include_str!("../../web/app.css");
const SCRIPT: &str = include_str!("../../web/app.js");
const LOGO: &str = include_str!("../../web/logo.svg");

/// Only the page's own files may load; nothing from other sites.
const POLICY: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; \
                      script-src 'self'; connect-src 'self'; frame-ancestors 'none'";

pub(super) fn routes() -> Router<Web> {
    Router::new()
        .route(
            "/",
            get(|| async { asset("text/html; charset=utf-8", INDEX) }),
        )
        .route(
            "/app.css",
            get(|| async { asset("text/css; charset=utf-8", STYLE) }),
        )
        .route(
            "/app.js",
            get(|| async { asset("text/javascript; charset=utf-8", SCRIPT) }),
        )
        .route("/logo.svg", get(|| async { asset("image/svg+xml", LOGO) }))
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONTENT_SECURITY_POLICY, POLICY),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}
