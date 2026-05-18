//! `GET /agent/<name>` — per-agent detail page (#446 disk surface).
//!
//! Renders the agent's home-directory size and a top-5 subdirectory
//! breakdown via `du -k --max-depth=1 | sort -h | tail -5` (the issue
//! spec). When #445 (full agent detail view) lands, this handler can be
//! merged with it; the breakdown block is structured so it slots in as
//! a dedicated section without conflicting renames.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};

use crate::app::adapters::web::auth::session;
use crate::app::adapters::web::routes::read_session_cookie;
use crate::app::adapters::web::state::WebState;
use crate::app::adapters::web::templates;
use crate::app::adapters::web::view::{agent_disk_detail_html, format_bytes};
use crate::app::metrics::{AgentBreakdownEntry, sample_agent_breakdown};

/// Top-N subdirectories shown on the detail page. The issue spec calls
/// out «top 5».
pub const BREAKDOWN_LIMIT: usize = 5;

pub async fn agent_detail(
    State(state): State<WebState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> Response {
    let now = (state.now)();
    let cookie = read_session_cookie(&headers);
    let payload = cookie.and_then(|c| session::verify(&c, state.secret.as_ref(), now));
    let payload = match payload {
        Some(p) => p,
        None => return Redirect::to("/login").into_response(),
    };

    // Resolve the home_dir for this agent — fall back to None when the
    // name is unknown (e.g. user typed it in by hand).
    let home_dir = state
        .agent_homes
        .iter()
        .find(|(n, _)| n == &name)
        .map(|(_, h)| h.clone());

    let snap = state.metrics.snapshot().await;
    let agent_sample = snap.lookup_agent(&name).cloned();

    let breakdown: Vec<AgentBreakdownEntry> = if let Some(home) = home_dir.as_deref() {
        sample_agent_breakdown(home, BREAKDOWN_LIMIT).await
    } else {
        Vec::new()
    };

    let total_bytes = agent_sample.as_ref().and_then(|a| a.home_dir_bytes);
    let updated_at = snap.updated_at;
    let detail_html = agent_disk_detail_html(
        home_dir.as_deref(),
        total_bytes,
        updated_at,
        &breakdown,
        format_bytes,
    );

    let html =
        templates::agent_detail_page(payload.telegram_id, &payload.csrf, &name, &detail_html);

    let mut resp = (StatusCode::OK, html).into_response();
    if let Ok(v) = "text/html; charset=utf-8".parse() {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    resp
}
