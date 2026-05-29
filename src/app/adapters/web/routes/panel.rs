//! Panel v2 routes (#497).
//!
//! Three server-rendered HTML routes + one JSON endpoint:
//!
//! - `GET /agents/<name>` — Level 2 per-agent detail.
//! - `GET /agents/<name>/sessions/<id>` — Level 3 transcript view
//!   (`?raw=1` switches to unparsed JSONL).
//! - `GET /api/kpi?period=today|week|month` — JSON KPI payload for the
//!   digest cron.
//!
//! The Level 1 route is mounted at `/` via `dashboard.rs` (which renders
//! the panel template instead of the legacy chrome).

use std::path::PathBuf;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde::Deserialize;

use crate::app::adapters::web::event_query::{
    self, KpiPeriod, aggregate_agents, aggregate_kpi, aggregate_sessions, iteration_sparkline_24h,
    read_events_between,
};
use crate::app::adapters::web::panel;
use crate::app::adapters::web::routes::authenticate;
use crate::app::adapters::web::state::WebState;
use crate::app::adapters::web::transcript_view;

/// Period query param for Level 2.
#[derive(Debug, Default, Deserialize)]
pub struct PeriodQuery {
    pub period: Option<String>,
}

/// `?raw=1` query param for Level 3.
#[derive(Debug, Default, Deserialize)]
pub struct RawQuery {
    pub raw: Option<String>,
}

/// `?period=today|week|month` query param for the JSON KPI endpoint.
#[derive(Debug, Default, Deserialize)]
pub struct KpiQuery {
    pub period: Option<String>,
}

/// `GET /agents/<name>` — Level 2.
pub async fn level2(
    State(state): State<WebState>,
    Path(name): Path<String>,
    Query(q): Query<PeriodQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(p) = authenticate(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    let now = Utc::now();
    let (window_start, label) = match q.period.as_deref().unwrap_or("7d") {
        "24h" => (now - chrono::Duration::hours(24), "24h"),
        "30d" => (now - chrono::Duration::days(30), "30d"),
        _ => (now - chrono::Duration::days(7), "7d"),
    };
    let root = event_query::default_root();
    let events = read_events_between(&root, window_start, now).await;
    let rows = aggregate_agents(&events);
    let rollup = rows
        .into_iter()
        .find(|r| r.agent == name)
        .unwrap_or_else(|| crate::app::adapters::web::event_query::AgentRollup {
            agent: name.clone(),
            ..Default::default()
        });
    let sessions = aggregate_sessions(&events, &name);
    let spark = iteration_sparkline_24h(&events, now, 24);
    let body = panel::level2_body(&name, label, &rollup, &sessions, &spark);
    let html = panel::shell_page(
        &format!("agent · {}", name),
        p.telegram_id,
        &p.csrf,
        &body,
        "—",
    );
    html_response(html)
}

/// `GET /agents/<name>/sessions/<id>` — Level 3.
pub async fn level3(
    State(state): State<WebState>,
    Path((name, session_id)): Path<(String, String)>,
    Query(q): Query<RawQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(p) = authenticate(&state, &headers) else {
        return Redirect::to("/login").into_response();
    };
    // Look up the agent's `work_dir`. Fall back to `~/{name}` when the
    // registry doesn't know about the agent — keeps the route usable
    // without a workspace.yaml entry.
    let home = resolve_agent_home(&state, &name);

    let raw_mode = q.raw.as_deref().is_some_and(|v| !v.is_empty() && v != "0");
    let body = if raw_mode {
        match transcript_view::raw_session(&home, &session_id).await {
            Ok(raw) => panel::level3_raw_body(&name, &session_id, &raw),
            Err(_) => not_found_body(&name, &session_id),
        }
    } else {
        match transcript_view::render_session(&home, &session_id).await {
            Ok(lines) => panel::level3_body(&name, &session_id, &lines),
            Err(_) => not_found_body(&name, &session_id),
        }
    };
    let html = panel::shell_page(
        &format!("session · {}", session_id),
        p.telegram_id,
        &p.csrf,
        &body,
        "—",
    );
    html_response(html)
}

/// `GET /api/kpi` — JSON KPI for the digest cron.
pub async fn api_kpi(
    State(state): State<WebState>,
    Query(q): Query<KpiQuery>,
    headers: HeaderMap,
) -> Response {
    if authenticate(&state, &headers).is_none() {
        // Don't 302 a JSON client — 401 is the right shape.
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let period = q
        .period
        .as_deref()
        .map(KpiPeriod::parse)
        .unwrap_or_default();
    let now = Utc::now();
    let start = match period {
        KpiPeriod::Today => event_query::day_start_berlin(now),
        KpiPeriod::Week => event_query::week_start_berlin(now),
        KpiPeriod::Month => event_query::month_start_berlin(now),
    };
    let root = event_query::default_root();
    let events = read_events_between(&root, start, now).await;
    let kpi = aggregate_kpi(&events, period);
    Json(kpi).into_response()
}

fn resolve_agent_home(state: &WebState, name: &str) -> PathBuf {
    for (n, p) in state.agent_homes.iter() {
        if n == name {
            return PathBuf::from(p);
        }
    }
    // Final fallback: a conventional path. Won't find sessions, but the
    // 404 page below is friendlier than an unwrap.
    PathBuf::from(format!("/home/{}", name))
}

fn not_found_body(name: &str, session_id: &str) -> String {
    format!(
        r#"<main class="panel container">
<nav class="breadcrumb"><a href="/">overview</a> · <a href="/agents/{a}">{a}</a></nav>
<header><h1>Session not found</h1></header>
<p>No JSONL file matches <code>{sid}</code> under the agent's <code>.claude/projects/</code>.</p>
</main>"#,
        a = html_escape(name),
        sid = html_escape(session_id),
    )
}

fn html_escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '&' => "&amp;".into(),
            '<' => "&lt;".into(),
            '>' => "&gt;".into(),
            '"' => "&quot;".into(),
            '\'' => "&#39;".into(),
            other => other.to_string(),
        })
        .collect()
}

fn html_response(body: String) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    if let Ok(v) = "text/html; charset=utf-8".parse() {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    resp
}
