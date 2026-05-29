//! `GET /` — dashboard (#443/#444/#484). Redirects unauthenticated visitors
//! to /login. Authenticated users get the live agent overview built from
//! the existing registry/context_size/tasklog data sources (#444) plus the
//! top-of-page summary chart (#484).

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
};
use chrono::Utc;
use serde::Deserialize;

use crate::app::adapters::web::data;
use crate::app::adapters::web::data_chart;
use crate::app::adapters::web::event_query::{
    self, KpiPeriod, aggregate_agents, aggregate_kpi, aggregate_weekly_tokens,
    iteration_sparkline_24h, read_events_between, week_start_berlin,
};
use crate::app::adapters::web::panel;
use crate::app::adapters::web::routes::{SESSION_COOKIE_NAME, authenticate};
use crate::app::adapters::web::state::WebState;
use crate::app::adapters::web::templates;
use crate::app::adapters::web::view;

/// Query params for the dashboard. All keys are optional and fall back to
/// safe defaults — the AC says invalid input must not 5xx and must not lose
/// the rest of the page, so we accept any string.
#[derive(Debug, Default, Deserialize)]
pub struct DashboardQuery {
    pub metric: Option<String>,
    pub period: Option<String>,
}

pub async fn dashboard(
    State(state): State<WebState>,
    Query(q): Query<DashboardQuery>,
    headers: HeaderMap,
) -> Response {
    let session_payload = authenticate(&state, &headers);

    match session_payload {
        Some(p) => {
            let disk = state.metrics.snapshot().await;
            let summaries = data::collect_agent_summaries(Some(&disk)).await;
            let overview = data::collect_vps_overview(Some(&disk));
            let strip_html = view::vps_strip(&overview);
            let agents_html = view::agents_section_with_disk(&summaries, disk.updated_at);
            // Keep the «Refresh now» button + the (legacy) chart available so
            // existing tooling that relied on them still works. Both are
            // folded into the legacy details block at the bottom of Panel v2.
            let refresh_html = templates::metrics_refresh_form(&p.csrf);
            let metric = q
                .metric
                .as_deref()
                .map(data_chart::Metric::parse)
                .unwrap_or_default();
            let period_chart = q
                .period
                .as_deref()
                .map(data_chart::Period::parse)
                .unwrap_or_default();
            let series = data_chart::collect_chart_data(metric, period_chart).await;
            let chart_html = view::chart::chart_block(&series, metric, period_chart);
            // Legacy markup is split into two parts by a sentinel comment:
            //   • the chart block (#484) must render ABOVE the legacy
            //     `vps-strip-wrap` so the existing chart tests still see
            //     `chart_idx < strip_idx`.
            //   • the rest (refresh form + legacy agents section) is folded
            //     under a `<details>` element at the bottom of Panel v2.
            let legacy_block = format!(
                "{chart}\n<!--LEGACY_SPLIT-->\n{refresh}\n{agents}",
                chart = chart_html,
                refresh = refresh_html,
                agents = agents_html
            );

            // ─── Panel v2 (#497) ───────────────────────────────────────
            let now = Utc::now();
            let week_start = week_start_berlin(now);
            let kpi_period = q
                .period
                .as_deref()
                .map(KpiPeriod::parse)
                .unwrap_or_default();
            let kpi_start = match kpi_period {
                KpiPeriod::Today => event_query::day_start_berlin(now),
                KpiPeriod::Week => week_start,
                KpiPeriod::Month => event_query::month_start_berlin(now),
            };
            let root = event_query::default_root();
            let weekly_events = read_events_between(&root, week_start, now).await;
            let weekly = aggregate_weekly_tokens(&weekly_events);
            let agent_rollups = aggregate_agents(&weekly_events);
            let kpi_events = if kpi_period == KpiPeriod::Week {
                weekly_events.clone()
            } else {
                read_events_between(&root, kpi_start, now).await
            };
            let kpi = aggregate_kpi(&kpi_events, kpi_period);
            let spark = iteration_sparkline_24h(&weekly_events, now, 24);
            let estimate = state.cfg.claude_weekly_token_estimate;

            let body = panel::level1_body(
                &kpi,
                &weekly,
                estimate,
                &spark,
                &agent_rollups,
                &strip_html,
                &legacy_block,
            );
            let uptime_human = overview
                .uptime
                .map(|d| {
                    let s = d.as_secs();
                    if s >= 86_400 {
                        format!("{}d", s / 86_400)
                    } else if s >= 3_600 {
                        format!("{}h", s / 3_600)
                    } else {
                        format!("{}m", s / 60)
                    }
                })
                .unwrap_or_else(|| "—".into());
            let html = panel::shell_page("dashboard", p.telegram_id, &p.csrf, &body, &uptime_human);
            html_response(html)
        }
        None => {
            // Tampered/expired/missing — clear the cookie and redirect.
            let mut resp = Redirect::to("/login").into_response();
            // Defensive: drop any stale session cookie.
            if let Ok(v) = expire_cookie_value().parse() {
                resp.headers_mut().insert(header::SET_COOKIE, v);
            }
            resp
        }
    }
}

fn html_response(body: String) -> Response {
    let mut resp = (StatusCode::OK, body).into_response();
    if let Ok(v) = "text/html; charset=utf-8".parse() {
        resp.headers_mut().insert(header::CONTENT_TYPE, v);
    }
    resp
}

/// Cookie value that immediately expires the session cookie (used for
/// logout and on tampered-cookie redirects).
pub fn expire_cookie_value() -> String {
    format!(
        "{}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0",
        SESSION_COOKIE_NAME
    )
}
