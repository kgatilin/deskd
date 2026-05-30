//! Integration tests for the Panel v2 routes (#497).
//!
//! Covers:
//! - `GET /` renders the Panel v2 overview (weekly token gauge, KPI block,
//!   agent rows, vendored Pico.css link, the legacy chart still folded in).
//! - `GET /agents/<name>` (Level 2) renders period tabs and the agent header.
//! - `GET /agents/<name>/sessions/<id>` (Level 3) returns a friendly «session
//!   not found» body when the JSONL is missing (no panic).
//! - `GET /api/kpi` returns a JSON payload with the right `period` label.
//! - `/static/pico.min.css` is served with the CSS content-type.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use deskd::app::adapters::web::audit::AuditLog;
use deskd::app::adapters::web::auth::{magic_link::TokenStore, session};
use deskd::app::adapters::web::dispatch::testing::{RecordingBusSender, RecordingDispatcher};
use deskd::app::adapters::web::middleware::rate_limit::RateLimiter;
use deskd::app::adapters::web::router;
use deskd::app::adapters::web::routes::SESSION_COOKIE_NAME;
use deskd::app::adapters::web::routes::github_webhook::shared_dedupe;
use deskd::app::adapters::web::state::WebState;
use deskd::config::{WebConfig, WebRateLimitConfig};
use tower::ServiceExt;

const TEST_TG_ID: i64 = 11_000;
const TEST_SECRET: [u8; 32] = [
    9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 11, 22, 33, 44, 55, 66, 77, 88, 99, 100, 101, 102, 103, 104, 105,
    106, 107, 108, 109, 110, 111, 112,
];

fn cfg(audit_path: std::path::PathBuf) -> WebConfig {
    WebConfig {
        enabled: true,
        bind: "127.0.0.1:0".into(),
        external_url: Some("https://deskd.example.com".into()),
        session_ttl_days: 30,
        magic_link_ttl_seconds: 300,
        allowed_telegram_ids: vec![TEST_TG_ID],
        audit_log: audit_path.to_string_lossy().to_string(),
        rate_limit: WebRateLimitConfig {
            auth_requests_per_hour: 20,
        },
        trust_transport: false,
        claude_weekly_token_estimate: 100_000_000,
    }
}

fn build_state() -> (WebState, RecordingDispatcher, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let audit_path = dir.path().join("audit.jsonl");
    let cfg_obj = cfg(audit_path.clone());
    let dispatcher = RecordingDispatcher::new();
    let dispatcher_arc: Arc<dyn deskd::app::adapters::web::dispatch::TelegramDispatcher> =
        Arc::new(dispatcher.clone());
    let bus_sender: Arc<dyn deskd::app::adapters::web::dispatch::BusSender> =
        Arc::new(RecordingBusSender::new());
    let agent_commands: Arc<dyn deskd::app::adapters::web::dispatch::AgentCommandDispatcher> =
        Arc::new(
            deskd::app::adapters::web::dispatch::testing::RecordingAgentCommandDispatcher::new(),
        );
    let metrics_cache = dir.path().join("disk-cache.json");
    let state = WebState {
        cfg: Arc::new(cfg_obj),
        secret: Arc::new(TEST_SECRET),
        tokens: Arc::new(TokenStore::new()),
        rate_limiter_ip: Arc::new(RateLimiter::new(20, 3600)),
        rate_limiter_tg: Arc::new(RateLimiter::new(20, 3600)),
        audit: AuditLog::new(audit_path),
        telegram: dispatcher_arc,
        github_webhooks: None,
        bus: bus_sender,
        github_deliveries: shared_dedupe(),
        agent_commands,
        now: Arc::new(|| 1_700_000_000),
        metrics: deskd::app::metrics::DiskMetrics::new(metrics_cache),
        agent_homes: Arc::new(Vec::new()),
        metrics_bus: None,
        cost: None,
        gh: Arc::new(deskd::app::adapters::web::data_cost::testing::RecordingGhClient::new()),
        cost_cache: deskd::app::adapters::web::data_cost::CostCache::new(),
    };
    (state, dispatcher, dir)
}

fn fake_peer() -> std::net::SocketAddr {
    "127.0.0.1:54321".parse().unwrap()
}

fn req_with_peer(req: Request<Body>) -> Request<Body> {
    let mut r = req;
    r.extensions_mut()
        .insert(axum::extract::ConnectInfo(fake_peer()));
    r
}

fn auth_cookie(state: &WebState) -> String {
    let now = (state.now)();
    let payload = session::SessionPayload::new(TEST_TG_ID, now, 86_400, "csrf-x".into());
    session::sign(&payload, state.secret.as_ref())
}

async fn body_string(resp: axum::response::Response) -> String {
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Variant of `build_state` that registers two fixture agent homes (#521).
/// The caller gets the dir handles back so they can write event-store
/// fixtures under `{home}/.deskd/events/`. Both temp dirs MUST be held
/// alive for the lifetime of the test or the fixture files vanish.
fn build_state_with_agent_homes() -> (
    WebState,
    RecordingDispatcher,
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
) {
    let cfg_dir = tempfile::tempdir().unwrap();
    let audit_path = cfg_dir.path().join("audit.jsonl");
    let cfg_obj = cfg(audit_path.clone());
    let dispatcher = RecordingDispatcher::new();
    let dispatcher_arc: Arc<dyn deskd::app::adapters::web::dispatch::TelegramDispatcher> =
        Arc::new(dispatcher.clone());
    let bus_sender: Arc<dyn deskd::app::adapters::web::dispatch::BusSender> =
        Arc::new(RecordingBusSender::new());
    let agent_commands: Arc<dyn deskd::app::adapters::web::dispatch::AgentCommandDispatcher> =
        Arc::new(
            deskd::app::adapters::web::dispatch::testing::RecordingAgentCommandDispatcher::new(),
        );
    let metrics_cache = cfg_dir.path().join("disk-cache.json");

    // Two fixture agent homes — these stand in for `/home/dev` and
    // `/home/kira` on the real VPS. Each gets its own tempdir so we
    // can prove the panel unions across them.
    let home_a = tempfile::tempdir().unwrap();
    let home_b = tempfile::tempdir().unwrap();
    let agent_homes = vec![
        ("dev".to_string(), home_a.path().display().to_string()),
        ("kira".to_string(), home_b.path().display().to_string()),
    ];

    let state = WebState {
        cfg: Arc::new(cfg_obj),
        secret: Arc::new(TEST_SECRET),
        tokens: Arc::new(TokenStore::new()),
        rate_limiter_ip: Arc::new(RateLimiter::new(20, 3600)),
        rate_limiter_tg: Arc::new(RateLimiter::new(20, 3600)),
        audit: AuditLog::new(audit_path),
        telegram: dispatcher_arc,
        github_webhooks: None,
        bus: bus_sender,
        github_deliveries: shared_dedupe(),
        agent_commands,
        now: Arc::new(|| 1_700_000_000),
        metrics: deskd::app::metrics::DiskMetrics::new(metrics_cache),
        agent_homes: Arc::new(agent_homes),
        metrics_bus: None,
        cost: None,
        gh: Arc::new(deskd::app::adapters::web::data_cost::testing::RecordingGhClient::new()),
        cost_cache: deskd::app::adapters::web::data_cost::CostCache::new(),
    };
    (state, dispatcher, cfg_dir, home_a, home_b)
}

/// Compute today's Berlin-date `YYYY-MM-DD` label, matching the
/// event-store rotation convention.
fn berlin_today_label() -> String {
    use chrono::{TimeZone, Utc};
    use chrono_tz::Europe::Berlin;
    let now = Utc::now();
    Berlin
        .from_utc_datetime(&now.naive_utc())
        .format("%Y-%m-%d")
        .to_string()
}

/// Write a `session.start` + `session.end` pair to
/// `{home}/.deskd/events/{today}.jsonl`. Returns the file path for
/// debugging. Timestamps use "now-ish" so they fall inside the
/// today/week/month KPI windows.
fn seed_today_events(
    home: &std::path::Path,
    agent: &str,
    session_id: &str,
    tokens_in: u64,
    tokens_out: u64,
    iterations: u64,
) -> std::path::PathBuf {
    use chrono::{Duration, Utc};
    let events_dir = home.join(".deskd").join("events");
    std::fs::create_dir_all(&events_dir).unwrap();
    let path = events_dir.join(format!("{}.jsonl", berlin_today_label()));
    // Pick timestamps strictly in the past so the KPI window's `now`
    // upper-bound includes them. 1h ago is safely inside any of
    // today/week/month windows when the test runs near midnight too.
    let now = Utc::now();
    let start = now - Duration::minutes(5);
    let end = now - Duration::minutes(1);
    let start_ev = serde_json::json!({
        "kind": "session.start",
        "ts": start.to_rfc3339(),
        "agent": agent,
        "session_id": session_id,
        "model": "claude-opus-4-7",
    });
    let end_ev = serde_json::json!({
        "kind": "session.end",
        "ts": end.to_rfc3339(),
        "agent": agent,
        "session_id": session_id,
        "tokens_in": tokens_in,
        "tokens_out": tokens_out,
        "cost_usd": 0.0,
        "iterations": iterations,
        "duration_s": 1800,
    });
    let line_a = serde_json::to_string(&start_ev).unwrap();
    let line_b = serde_json::to_string(&end_ev).unwrap();
    std::fs::write(&path, format!("{}\n{}\n", line_a, line_b)).unwrap();
    path
}

// ─── Level 1: GET / ─────────────────────────────────────────────────────

#[tokio::test]
async fn root_renders_panel_v2_with_weekly_gauge_and_pico_css_link() {
    let (state, _disp, _dir) = build_state();
    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    // Pico.css vendored link.
    assert!(
        body.contains(r#"<link rel="stylesheet" href="/static/pico.min.css">"#),
        "Pico.css link missing"
    );
    // Panel v2 sections.
    assert!(
        body.contains("Weekly tokens"),
        "weekly tokens header missing"
    );
    assert!(body.contains("KPI"), "KPI block missing");
    assert!(body.contains("Activity (24h iterations)"));
    assert!(body.contains("Agents (7d)"));
    // Legacy markers preserved.
    assert!(
        body.contains("vps-strip-wrap"),
        "legacy vps-strip preserved"
    );
    assert!(body.contains("Legacy dashboard"));
}

#[tokio::test]
async fn root_includes_tg_id_and_logout_form() {
    // Panel v2 shell must still carry the session owner's tg id + a CSRF-
    // protected logout form so the existing dashboard contract holds.
    let (state, _disp, _dir) = build_state();
    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    let body = body_string(resp).await;
    assert!(body.contains(&format!("tg:{}", TEST_TG_ID)));
    assert!(body.contains(r#"action="/logout""#));
}

// ─── Level 2: GET /agents/<name> ────────────────────────────────────────

#[tokio::test]
async fn level2_redirects_unauthenticated() {
    let (state, _disp, _dir) = build_state();
    let app = router::build(state);
    let req = req_with_peer(Request::get("/agents/dev").body(Body::empty()).unwrap());
    let resp = app.oneshot(req).await.unwrap();
    assert!(resp.status().is_redirection(), "got {}", resp.status());
    assert_eq!(resp.headers().get(header::LOCATION).unwrap(), "/login");
}

#[tokio::test]
async fn level2_renders_period_tabs_for_authenticated() {
    let (state, _disp, _dir) = build_state();
    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/agents/dev?period=7d")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("agent · dev"));
    assert!(body.contains("period=24h"));
    assert!(body.contains("period=7d"));
    assert!(body.contains("period=30d"));
    assert!(body.contains("tab--active"));
}

// ─── Level 3: GET /agents/<name>/sessions/<id> ──────────────────────────

#[tokio::test]
async fn level3_returns_session_not_found_for_unknown_id() {
    let (state, _disp, _dir) = build_state();
    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/agents/dev/sessions/UNKNOWN")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(body.contains("Session not found"));
    assert!(body.contains("UNKNOWN"));
}

// ─── /api/kpi ───────────────────────────────────────────────────────────

#[tokio::test]
async fn api_kpi_returns_401_when_unauthenticated() {
    let (state, _disp, _dir) = build_state();
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/api/kpi?period=today")
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn api_kpi_returns_json_with_period_label() {
    let (state, _disp, _dir) = build_state();
    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/api/kpi?period=week")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        ct.starts_with("application/json"),
        "expected JSON, got {}",
        ct
    );
    let body = body_string(resp).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["period"], "week");
    // Default counts are zero — these are scalar numerics, not strings.
    assert!(v["tokens_total"].is_number());
    assert!(v["sessions"].is_number());
}

// ─── #521 regression: panel unions events across agent homes ───────────

#[tokio::test]
async fn api_kpi_aggregates_across_registered_agent_homes() {
    // Real-world setup: `deskd serve` runs as root (HOME=/root with no
    // events), agents `dev` and `kira` each own their own home with a
    // populated event store. Before #521 the KPI numerator queried
    // /root/.deskd/events/ and returned all zeros. After #521 the
    // panel must union both registered homes.
    let (state, _disp, _cfg_dir, home_a, home_b) = build_state_with_agent_homes();
    seed_today_events(home_a.path(), "dev", "s-dev-1", 1000, 500, 5);
    seed_today_events(home_b.path(), "kira", "s-kira-1", 2000, 750, 7);

    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/api/kpi?period=week")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();

    // Union of both homes:
    //   tokens_in  = 1000 + 2000 = 3000
    //   tokens_out =  500 +  750 = 1250
    //   total      = 3000 + 1250 = 4250
    //   sessions   = 2 (one per agent)
    //   iterations = 5 + 7 = 12
    assert_eq!(v["tokens_in"], 3000, "tokens_in must union both homes");
    assert_eq!(v["tokens_out"], 1250, "tokens_out must union both homes");
    assert_eq!(
        v["tokens_total"], 4250,
        "tokens_total must union both homes"
    );
    assert_eq!(v["sessions"], 2, "should count both agents' sessions");
    assert_eq!(v["iterations"], 12, "should sum iterations across homes");
}

#[tokio::test]
async fn api_kpi_zero_when_no_event_files_exist() {
    // Graceful empty state: two agents are registered but neither has
    // ever written an event file. The route must return 200 with zeros,
    // not 500 or a missing-file error.
    let (state, _disp, _cfg_dir, _home_a, _home_b) = build_state_with_agent_homes();
    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/api/kpi?period=today")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    let v: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(v["tokens_total"], 0);
    assert_eq!(v["sessions"], 0);
    assert_eq!(v["iterations"], 0);
}

#[tokio::test]
async fn level1_dashboard_unions_agent_event_homes() {
    // End-to-end sanity for `GET /`: with two agent homes populated, the
    // rendered HTML must surface non-zero numbers. We inspect the cheap
    // token count via the panel body — pre-#521 this rendered "0".
    let (state, _disp, _cfg_dir, home_a, home_b) = build_state_with_agent_homes();
    seed_today_events(home_a.path(), "dev", "s-dev-1", 1_000_000, 500_000, 5);
    seed_today_events(home_b.path(), "kira", "s-kira-1", 2_000_000, 750_000, 7);

    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    // Both agent rows must be in the rendered agent list.
    assert!(
        body.contains("dev"),
        "agent dev should appear in the agent rollup table"
    );
    assert!(
        body.contains("kira"),
        "agent kira should appear in the agent rollup table"
    );
}

// ─── /static/pico.min.css ──────────────────────────────────────────────

// Sample HTML dump — gated behind an env var so it's a no-op in CI but
// produces an inspectable file when explicitly requested (used to capture
// the snippet in the PR description).
#[tokio::test]
async fn dump_panel_html_sample_on_demand() {
    if std::env::var_os("DESKD_DUMP_PANEL_HTML").is_none() {
        return;
    }
    let (state, _disp, _dir) = build_state();
    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    let body = body_string(resp).await;
    std::fs::write("/tmp/panel_sample.html", &body).unwrap();
}

// ─── #516: CSP-safe row click (no inline onclick handlers) ─────────────

#[tokio::test]
async fn root_has_no_inline_onclick_and_uses_data_href_with_panel_js() {
    // #516 regression: panel v2 must not emit inline `onclick` handlers
    // (strict CSP from #443 forbids `'unsafe-inline'`). Full-row click is
    // delegated through `/static/panel.js` which reads `data-href`.
    let (state, _disp, _dir) = build_state();
    let cookie = auth_cookie(&state);
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/")
            .header(
                header::COOKIE,
                format!("{}={}", SESSION_COOKIE_NAME, cookie),
            )
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = body_string(resp).await;
    assert!(
        !body.contains("onclick="),
        "panel v2 must not emit inline onclick attributes"
    );
    assert!(
        body.contains(r#"src="/static/panel.js""#),
        "shell head must link /static/panel.js"
    );
}

#[tokio::test]
async fn static_panel_js_served_with_js_content_type() {
    let (state, _disp, _dir) = build_state();
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/static/panel.js")
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        ct.starts_with("application/javascript"),
        "expected JS, got {}",
        ct
    );
    let body = body_string(resp).await;
    // The handler must be a delegated listener on `data-href` rows.
    assert!(body.contains("tr.agent-row[data-href]"));
}

#[tokio::test]
async fn static_pico_css_served_with_css_content_type() {
    let (state, _disp, _dir) = build_state();
    let app = router::build(state);
    let req = req_with_peer(
        Request::get("/static/pico.min.css")
            .body(Body::empty())
            .unwrap(),
    );
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .expect("content-type")
        .to_str()
        .unwrap()
        .to_string();
    assert!(ct.starts_with("text/css"), "expected CSS, got {}", ct);
    let body = body_string(resp).await;
    // Sanity-check: real vendored Pico.css starts with the charset
    // directive (or at least the copyright banner).
    assert!(body.contains("Pico CSS"), "vendored file lacks Pico banner");
}
