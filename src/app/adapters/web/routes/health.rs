//! `GET /healthz` — unauthenticated liveness probe (#443).

use axum::http::StatusCode;

pub async fn healthz() -> (StatusCode, &'static str) {
    (StatusCode::OK, "ok")
}
