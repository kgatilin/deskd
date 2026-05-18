//! Shared state for the web adapter (#443).
//!
//! `WebState` is a single struct shared across every axum handler via
//! `axum::extract::State`. It bundles config, the secret, the magic-link
//! token store, the rate limiter, the audit log writer, and the Telegram
//! dispatcher (a trait object so tests can stub it out).

use std::sync::Arc;

use crate::config::WebConfig;

use super::audit::AuditLog;
use super::auth::magic_link::TokenStore;
use super::dispatch::{AgentCommandDispatcher, TelegramDispatcher};
use super::middleware::rate_limit::RateLimiter;

/// Aggregate state passed through axum's `State` extractor.
#[derive(Clone)]
pub struct WebState {
    pub cfg: Arc<WebConfig>,
    pub secret: Arc<[u8; 32]>,
    pub tokens: Arc<TokenStore>,
    pub rate_limiter_ip: Arc<RateLimiter>,
    pub rate_limiter_tg: Arc<RateLimiter>,
    pub audit: AuditLog,
    pub telegram: Arc<dyn TelegramDispatcher>,
    /// Per-agent command dispatcher (#445). Published as `{command: "…"}`
    /// envelopes to `agent:<name>` on the bus by the production
    /// implementation; tests inject a recording double.
    pub agent_commands: Arc<dyn AgentCommandDispatcher>,
    /// Cached "now" provider — defaults to system time. Tests substitute a
    /// closure that returns a fixed timestamp so cookie/expiry semantics are
    /// deterministic.
    pub now: NowFn,
}

/// Returns a unix timestamp in seconds. Boxed so we can stub it out in tests.
pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Default "now" implementation backed by `chrono::Utc::now()`.
pub fn system_now() -> NowFn {
    Arc::new(|| chrono::Utc::now().timestamp())
}
