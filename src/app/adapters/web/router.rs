//! Axum router builder for the web adapter (#443).

use axum::{
    Router, middleware,
    routing::{get, post},
};

use super::middleware::headers::security_headers;
use super::routes::{dashboard, health, login, logout};
use super::state::WebState;

/// Build the axum router. Public so the integration tests can drive it
/// in-process via `tower::ServiceExt::oneshot`.
pub fn build(state: WebState) -> Router {
    Router::new()
        .route("/healthz", get(health::healthz))
        .route("/", get(dashboard::dashboard))
        .route("/login", get(login::login_form))
        .route("/login/request", post(login::login_request))
        .route("/login/consume", get(login::login_consume))
        .route("/logout", post(logout::logout))
        .layer(middleware::from_fn(security_headers))
        .with_state(state)
}
