//! Read-side queries against the `~/.deskd/events/YYYY-MM-DD.jsonl`
//! event store (#495) for the Panel v2 dashboard (#497).
//!
//! Streams the daily-rotated event files line-by-line, parses each into a
//! [`TranscriptEvent`], and produces the aggregates the dashboard needs:
//!
//! - weekly token total + per-model breakdown
//! - 24h iteration sparkline (binned)
//! - per-agent rollup (24h / 7d / 30d each)
//! - KPI numerator + arrow comparison (today / week / month)
//! - sessions list for a given agent (Level 2)
//!
//! All scans are O(n) over the relevant set of `.jsonl` files (one file
//! per day) and run on every request. The event store is small (≤ a few
//! MB / week on a busy deployment) so caching is unnecessary for v1.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Duration, TimeZone, Utc};
use chrono_tz::Europe::Berlin;
use serde::Serialize;

use crate::app::transcript_extractor::{LimitKind, TranscriptEvent, default_event_root};

/// Aggregated weekly token usage broken out by model. The dashboard renders
/// `total` as the gauge numerator and emits one row per model in the
/// «tokens block».
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct WeeklyTokens {
    pub total_in: u64,
    pub total_out: u64,
    pub by_model: BTreeMap<String, ModelTokens>,
    /// Number of `limit.hit` events with `limit_kind == weekly_cap` over
    /// the same 7-day window.
    pub weekly_cap_hits: u64,
}

#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct ModelTokens {
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub iterations: u64,
}

impl WeeklyTokens {
    pub fn total(&self) -> u64 {
        self.total_in.saturating_add(self.total_out)
    }
}

/// One per-agent rollup row (#497 Level 1 «Agent rows»). Period is fixed
/// by the caller (24h / 7d / 30d for Level 2; 7d for Level 1).
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct AgentRollup {
    pub agent: String,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub iterations: u64,
    pub cost_usd: f64,
    pub limit_hits: u64,
    pub sessions: u64,
}

impl AgentRollup {
    pub fn total_tokens(&self) -> u64 {
        self.tokens_in.saturating_add(self.tokens_out)
    }
}

/// One session entry shown in the Level 2 list.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionInfo {
    pub session_id: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub iterations: u64,
}

impl SessionInfo {
    pub fn total_tokens(&self) -> u64 {
        self.tokens_in.saturating_add(self.tokens_out)
    }
}

/// Period selector for KPI numerator (#497 addendum). Cohort is consistent
/// across numerator and denominator — «week» means «this week's tokens vs
/// this week's issues closed».
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KpiPeriod {
    #[default]
    Today,
    Week,
    Month,
}

impl KpiPeriod {
    pub fn parse(s: &str) -> Self {
        match s {
            "week" | "this_week" | "7d" => Self::Week,
            "month" | "this_month" | "30d" => Self::Month,
            _ => Self::Today,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Today => "today",
            Self::Week => "week",
            Self::Month => "month",
        }
    }
}

/// KPI block payload (#497 addendum). Token breakdown lives on the
/// `tokens` field; the rest are scalar counts.
#[derive(Debug, Default, Clone, Serialize, PartialEq)]
pub struct Kpi {
    pub period: String,
    pub tokens_total: u64,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Cache-creation tokens — currently approximated as 0 since the event
    /// store doesn't split usage.cache_creation_input_tokens out yet.
    /// Surfaced for forward-compat; populated when #495 starts emitting.
    pub cache_creation: u64,
    /// Cache-read tokens — same caveat as `cache_creation`.
    pub cache_read: u64,
    pub iterations: u64,
    pub limit_hits: u64,
    pub sessions: u64,
}

/// Read all event store files whose label falls between `start` and `end`
/// (inclusive, Berlin date). Streaming JSONL parse: lines that fail to
/// parse are skipped silently — the store is treated as forward-compatible.
///
/// Single-root reader. Production web handlers should use
/// [`read_events_between_from_roots`] which unions across every registered
/// agent home — `deskd serve` may run as a different unix user than the
/// agents that write events, so a single `$HOME`-rooted read is
/// systematically wrong (see #521).
pub async fn read_events_between(
    root: &Path,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<TranscriptEvent> {
    let mut out = Vec::new();
    let dates = berlin_date_labels_between(start, end);
    for label in dates {
        let path = root.join(format!("{}.jsonl", label));
        let bytes = match tokio::fs::read(&path).await {
            Ok(b) => b,
            Err(_) => continue, // Missing day file is fine — no events.
        };
        let text = match std::str::from_utf8(&bytes) {
            Ok(t) => t,
            Err(_) => continue,
        };
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(ev) = serde_json::from_str::<TranscriptEvent>(line)
                && event_in_window(&ev, start, end)
            {
                out.push(ev);
            }
        }
    }
    out
}

/// Multi-root variant of [`read_events_between`] (#521).
///
/// Unions events from every supplied root, sorts the result by event
/// timestamp ascending, and silently skips roots that don't exist or
/// can't be read. This is what the web adapter MUST use: `deskd serve`
/// typically runs as `root` (or a supervisor user) while agents run as
/// their own unix users (`dev`, `kira`, `family`, …). Each agent owns a
/// `~/.deskd/events/` under its own `$HOME`, so a single-root read
/// rooted in the supervisor's `$HOME` always misses the real data.
///
/// Roots are typically `{agent_work_dir}/.deskd/events/` — derive them
/// with [`crate::app::adapters::web::state::WebState::event_roots`].
///
/// Empty input → empty output (no error). Duplicate roots are *not*
/// deduplicated — call sites are expected to pass distinct paths.
pub async fn read_events_between_from_roots(
    roots: &[PathBuf],
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> Vec<TranscriptEvent> {
    let mut out = Vec::new();
    for root in roots {
        let part = read_events_between(root, start, end).await;
        out.extend(part);
    }
    // Sort ascending by parsed RFC-3339 ts; events with unparseable
    // timestamps sink to the end so aggregations stay deterministic.
    out.sort_by_key(|ev| {
        DateTime::parse_from_rfc3339(ev.ts())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|_| DateTime::<Utc>::MAX_UTC)
    });
    out
}

/// Aggregate every `session.end` event in the window into one
/// [`WeeklyTokens`]. Also counts `limit_kind=weekly_cap` from `limit.hit`.
pub fn aggregate_weekly_tokens(events: &[TranscriptEvent]) -> WeeklyTokens {
    let mut acc = WeeklyTokens::default();
    let mut model_per_session: BTreeMap<String, String> = BTreeMap::new();

    // Pass 1: collect (session, model) from session.start so we can attribute
    // session.end tokens to the right model.
    for ev in events {
        if let TranscriptEvent::SessionStart {
            session_id, model, ..
        } = ev
        {
            model_per_session.insert(session_id.clone(), model.clone());
        }
    }
    // Pass 2: roll up tokens / iterations.
    for ev in events {
        match ev {
            TranscriptEvent::SessionEnd {
                session_id,
                tokens_in,
                tokens_out,
                iterations,
                ..
            } => {
                acc.total_in = acc.total_in.saturating_add(*tokens_in);
                acc.total_out = acc.total_out.saturating_add(*tokens_out);
                let model = model_per_session
                    .get(session_id)
                    .cloned()
                    .unwrap_or_else(|| "unknown".into());
                let entry = acc.by_model.entry(model_bucket(&model)).or_default();
                entry.tokens_in = entry.tokens_in.saturating_add(*tokens_in);
                entry.tokens_out = entry.tokens_out.saturating_add(*tokens_out);
                entry.iterations = entry.iterations.saturating_add(*iterations);
            }
            TranscriptEvent::LimitHit { limit_kind, .. } => {
                if matches!(limit_kind, LimitKind::WeeklyCap) {
                    acc.weekly_cap_hits = acc.weekly_cap_hits.saturating_add(1);
                }
            }
            _ => {}
        }
    }
    acc
}

/// Group model id into a coarse bucket ("opus" / "sonnet" / "haiku" /
/// other) so the per-model rows on the dashboard stay readable across
/// minor model rev numbers.
pub fn model_bucket(model: &str) -> String {
    let lower = model.to_ascii_lowercase();
    for tier in ["opus", "sonnet", "haiku"] {
        if lower.contains(tier) {
            return tier.to_string();
        }
    }
    if lower.is_empty() {
        return "unknown".into();
    }
    lower
}

/// Sparkline of `bins` unicode block characters representing iteration
/// counts over the last 24h. `bins` defaults to 24 (one slot per hour).
pub fn iteration_sparkline_24h(
    events: &[TranscriptEvent],
    now: DateTime<Utc>,
    bins: usize,
) -> String {
    if bins == 0 {
        return String::new();
    }
    let mut counts = vec![0u64; bins];
    let start = now - Duration::hours(24);
    let bin_secs = (24 * 3600) / bins as i64;
    for ev in events {
        if let TranscriptEvent::SessionEnd { ts, iterations, .. } = ev
            && let Ok(t) = DateTime::parse_from_rfc3339(ts)
        {
            let t_utc = t.with_timezone(&Utc);
            if t_utc < start || t_utc > now {
                continue;
            }
            let off = (t_utc - start).num_seconds();
            let idx = (off / bin_secs).clamp(0, (bins as i64) - 1) as usize;
            counts[idx] = counts[idx].saturating_add(*iterations);
        }
    }
    sparkline_from(&counts)
}

/// Convert a counts slice to a unicode-block sparkline (e.g. `▁▂▃▄▅▆▇█`).
pub fn sparkline_from(counts: &[u64]) -> String {
    const BLOCKS: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = counts.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return "·".repeat(counts.len());
    }
    let mut out = String::with_capacity(counts.len());
    for &c in counts {
        if c == 0 {
            out.push('·');
            continue;
        }
        let idx = ((c as f64 / max as f64) * (BLOCKS.len() as f64 - 1.0)).round() as usize;
        out.push(BLOCKS[idx.min(BLOCKS.len() - 1)]);
    }
    out
}

/// Render a unicode bar gauge for a `used / total` ratio. Width is the
/// total number of cells, including filled + empty. Used for the Level 1
/// weekly-tokens gauge.
pub fn unicode_bar(used: u64, total: u64, width: usize) -> String {
    if width == 0 || total == 0 {
        return String::new();
    }
    let ratio = (used as f64 / total as f64).clamp(0.0, 1.0);
    let filled = (ratio * width as f64).round() as usize;
    let mut s = String::with_capacity(width);
    for _ in 0..filled {
        s.push('█');
    }
    for _ in filled..width {
        s.push('░');
    }
    s
}

/// Color tier for the weekly-tokens gauge — `green < 70% <= yellow < 90% <= red`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum GaugeTier {
    Green,
    Yellow,
    Red,
}

impl GaugeTier {
    pub fn from_ratio(ratio: f64) -> Self {
        if ratio >= 0.9 {
            Self::Red
        } else if ratio >= 0.7 {
            Self::Yellow
        } else {
            Self::Green
        }
    }
    pub fn css_class(self) -> &'static str {
        match self {
            Self::Green => "gauge--green",
            Self::Yellow => "gauge--yellow",
            Self::Red => "gauge--red",
        }
    }
}

/// Per-agent rollup over a fixed window. `events` must be pre-filtered to
/// the window of interest.
pub fn aggregate_agents(events: &[TranscriptEvent]) -> Vec<AgentRollup> {
    let mut by: BTreeMap<String, AgentRollup> = BTreeMap::new();
    let mut session_models: BTreeMap<(String, String), String> = BTreeMap::new();
    for ev in events {
        if let TranscriptEvent::SessionStart {
            agent,
            session_id,
            model,
            ..
        } = ev
        {
            session_models.insert((agent.clone(), session_id.clone()), model.clone());
        }
    }
    for ev in events {
        let agent_name = ev.agent().to_string();
        let entry = by.entry(agent_name.clone()).or_insert_with(|| AgentRollup {
            agent: agent_name.clone(),
            ..Default::default()
        });
        match ev {
            TranscriptEvent::SessionStart { model, .. } => {
                entry.sessions = entry.sessions.saturating_add(1);
                if entry.model.is_empty() {
                    entry.model = model.clone();
                }
            }
            TranscriptEvent::SessionEnd {
                tokens_in,
                tokens_out,
                cost_usd,
                iterations,
                session_id,
                ..
            } => {
                entry.tokens_in = entry.tokens_in.saturating_add(*tokens_in);
                entry.tokens_out = entry.tokens_out.saturating_add(*tokens_out);
                entry.iterations = entry.iterations.saturating_add(*iterations);
                entry.cost_usd += cost_usd;
                if entry.model.is_empty()
                    && let Some(m) = session_models.get(&(agent_name.clone(), session_id.clone()))
                {
                    entry.model = m.clone();
                }
            }
            TranscriptEvent::LimitHit { .. } => {
                entry.limit_hits = entry.limit_hits.saturating_add(1);
            }
            TranscriptEvent::ModelSwitch { to, .. } => {
                if entry.model.is_empty() {
                    entry.model = to.clone();
                }
            }
        }
    }
    let mut rows: Vec<AgentRollup> = by.into_values().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.total_tokens()));
    rows
}

/// Build [`SessionInfo`] entries for one agent across the supplied window.
pub fn aggregate_sessions(events: &[TranscriptEvent], for_agent: &str) -> Vec<SessionInfo> {
    let mut by: BTreeMap<String, SessionInfo> = BTreeMap::new();
    for ev in events {
        if ev.agent() != for_agent {
            continue;
        }
        let sid = ev.session_id().to_string();
        let entry = by.entry(sid.clone()).or_insert_with(|| SessionInfo {
            session_id: sid.clone(),
            started_at: None,
            ended_at: None,
            model: String::new(),
            tokens_in: 0,
            tokens_out: 0,
            iterations: 0,
        });
        match ev {
            TranscriptEvent::SessionStart { ts, model, .. } => {
                entry.started_at = parse_ts(ts);
                entry.model = model.clone();
            }
            TranscriptEvent::SessionEnd {
                ts,
                tokens_in,
                tokens_out,
                iterations,
                ..
            } => {
                entry.ended_at = parse_ts(ts);
                entry.tokens_in = entry.tokens_in.saturating_add(*tokens_in);
                entry.tokens_out = entry.tokens_out.saturating_add(*tokens_out);
                entry.iterations = entry.iterations.saturating_add(*iterations);
            }
            _ => {}
        }
    }
    let mut sessions: Vec<SessionInfo> = by.into_values().collect();
    sessions.sort_by_key(|s| std::cmp::Reverse(s.started_at));
    sessions
}

/// Compute [`Kpi`] block for the given period.
pub fn aggregate_kpi(events: &[TranscriptEvent], period: KpiPeriod) -> Kpi {
    let mut k = Kpi {
        period: period.label().to_string(),
        ..Default::default()
    };
    for ev in events {
        match ev {
            TranscriptEvent::SessionStart { .. } => {
                k.sessions = k.sessions.saturating_add(1);
            }
            TranscriptEvent::SessionEnd {
                tokens_in,
                tokens_out,
                iterations,
                ..
            } => {
                k.tokens_in = k.tokens_in.saturating_add(*tokens_in);
                k.tokens_out = k.tokens_out.saturating_add(*tokens_out);
                k.tokens_total = k.tokens_total.saturating_add(*tokens_in);
                k.tokens_total = k.tokens_total.saturating_add(*tokens_out);
                k.iterations = k.iterations.saturating_add(*iterations);
            }
            TranscriptEvent::LimitHit { .. } => {
                k.limit_hits = k.limit_hits.saturating_add(1);
            }
            _ => {}
        }
    }
    k
}

/// Arrow logic per #497 spec: `✓` at-or-better-than p50, `⬇` worse than
/// p50, `⬆` better-than-p50 AND at-or-above p90. Higher values are
/// «better» for tokens, sessions, iterations; «lower» is better for
/// limit_hits. Caller picks orientation by setting `higher_is_better`.
pub fn arrow_vs_baseline(value: u64, p50: u64, p90: u64, higher_is_better: bool) -> char {
    let better = if higher_is_better {
        value >= p50
    } else {
        value <= p50
    };
    let great = if higher_is_better {
        value >= p90
    } else {
        // For metrics where lower is better, "great" means even less than
        // p10 — caller passes that as `p90` for symmetry.
        value <= p90
    };
    if !better {
        '⬇'
    } else if great {
        '⬆'
    } else {
        '✓'
    }
}

/// Compute the Monday-anchored start (Berlin time) of the week containing
/// `now`. Per the issue: «Berlin-week rollover».
pub fn week_start_berlin(now: DateTime<Utc>) -> DateTime<Utc> {
    let berlin = Berlin.from_utc_datetime(&now.naive_utc());
    let weekday_index = berlin.weekday().num_days_from_monday() as i64;
    let monday = berlin
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight always valid")
        - chrono::Duration::days(weekday_index);
    let monday_berlin = Berlin
        .from_local_datetime(&monday)
        .single()
        .unwrap_or_else(|| Berlin.from_utc_datetime(&monday));
    monday_berlin.with_timezone(&Utc)
}

/// Day-aligned start (Berlin) for the day containing `now`.
pub fn day_start_berlin(now: DateTime<Utc>) -> DateTime<Utc> {
    let berlin = Berlin.from_utc_datetime(&now.naive_utc());
    let midnight = berlin
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight valid");
    Berlin
        .from_local_datetime(&midnight)
        .single()
        .unwrap_or_else(|| Berlin.from_utc_datetime(&midnight))
        .with_timezone(&Utc)
}

/// Month-aligned start (Berlin).
pub fn month_start_berlin(now: DateTime<Utc>) -> DateTime<Utc> {
    let berlin = Berlin.from_utc_datetime(&now.naive_utc());
    let nd = berlin.date_naive();
    let first = chrono::NaiveDate::from_ymd_opt(nd.year(), nd.month(), 1)
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .expect("month start valid");
    Berlin
        .from_local_datetime(&first)
        .single()
        .unwrap_or_else(|| Berlin.from_utc_datetime(&first))
        .with_timezone(&Utc)
}

/// Convert `[start, end]` into the list of Berlin `YYYY-MM-DD` labels we
/// need to open. Inclusive.
fn berlin_date_labels_between(start: DateTime<Utc>, end: DateTime<Utc>) -> Vec<String> {
    let berlin_start = Berlin.from_utc_datetime(&start.naive_utc()).date_naive();
    let berlin_end = Berlin.from_utc_datetime(&end.naive_utc()).date_naive();
    if berlin_end < berlin_start {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = berlin_start;
    while cur <= berlin_end {
        out.push(cur.format("%Y-%m-%d").to_string());
        cur = cur.succ_opt().unwrap_or(cur);
        if cur == berlin_end && cur.format("%Y-%m-%d").to_string() == *out.last().unwrap() {
            break; // Guard against pathological infinite loop on overflow.
        }
    }
    out
}

fn event_in_window(ev: &TranscriptEvent, start: DateTime<Utc>, end: DateTime<Utc>) -> bool {
    match DateTime::parse_from_rfc3339(ev.ts()) {
        Ok(t) => {
            let t = t.with_timezone(&Utc);
            t >= start && t <= end
        }
        Err(_) => false,
    }
}

fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .ok()
}

/// Default event-store root for the *current process's* `$HOME`. Exposed
/// for the per-agent transcript-extractor and tests.
///
/// **Do not call this from web-adapter route handlers** — the web adapter
/// runs in `deskd serve`'s process whose `$HOME` is the supervisor's
/// (often `/root`), not the agent home. Use
/// [`read_events_between_from_roots`] with the homes resolved from
/// [`crate::app::adapters::web::state::WebState`] instead (#521).
pub fn default_root() -> PathBuf {
    default_event_root()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Timelike};
    use tempfile::tempdir;

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn ev_start(t: &str, agent: &str, session: &str, model: &str) -> TranscriptEvent {
        TranscriptEvent::SessionStart {
            ts: t.into(),
            agent: agent.into(),
            session_id: session.into(),
            model: model.into(),
        }
    }
    fn ev_end(t: &str, agent: &str, session: &str, ti: u64, to: u64, it: u64) -> TranscriptEvent {
        TranscriptEvent::SessionEnd {
            ts: t.into(),
            agent: agent.into(),
            session_id: session.into(),
            tokens_in: ti,
            tokens_out: to,
            cost_usd: 0.0,
            iterations: it,
            duration_s: 1,
        }
    }
    fn ev_limit(t: &str, agent: &str, session: &str, kind: LimitKind) -> TranscriptEvent {
        TranscriptEvent::LimitHit {
            ts: t.into(),
            agent: agent.into(),
            session_id: session.into(),
            limit_kind: kind,
            detail: "x".into(),
        }
    }

    #[test]
    fn sparkline_zero_returns_dots() {
        let s = sparkline_from(&[0, 0, 0, 0]);
        assert_eq!(s, "····");
    }

    #[test]
    fn sparkline_picks_blocks_for_nonzero() {
        let s = sparkline_from(&[1, 4, 8]);
        // Just assert it's 3 non-dot chars + ASCII-safe length isn't 3
        // (unicode multi-byte) — char count is what matters.
        assert_eq!(s.chars().count(), 3);
        for c in s.chars() {
            assert_ne!(c, '·');
        }
    }

    #[test]
    fn unicode_bar_clamps_and_fills() {
        assert_eq!(unicode_bar(50, 100, 10).chars().count(), 10);
        assert!(unicode_bar(150, 100, 10).chars().all(|c| c == '█'));
        assert!(unicode_bar(0, 100, 10).chars().all(|c| c == '░'));
    }

    #[test]
    fn gauge_tier_thresholds() {
        assert_eq!(GaugeTier::from_ratio(0.0), GaugeTier::Green);
        assert_eq!(GaugeTier::from_ratio(0.69), GaugeTier::Green);
        assert_eq!(GaugeTier::from_ratio(0.70), GaugeTier::Yellow);
        assert_eq!(GaugeTier::from_ratio(0.89), GaugeTier::Yellow);
        assert_eq!(GaugeTier::from_ratio(0.90), GaugeTier::Red);
        assert_eq!(GaugeTier::from_ratio(2.0), GaugeTier::Red);
    }

    #[test]
    fn model_bucket_groups_tiers() {
        assert_eq!(model_bucket("claude-opus-4-7"), "opus");
        assert_eq!(model_bucket("claude-sonnet-4-6"), "sonnet");
        assert_eq!(model_bucket("claude-haiku-3"), "haiku");
        assert_eq!(model_bucket("custom-model"), "custom-model");
        assert_eq!(model_bucket(""), "unknown");
    }

    #[test]
    fn aggregate_weekly_tokens_sums_per_model() {
        let evs = vec![
            ev_start("2026-05-22T10:00:00Z", "a", "s1", "claude-opus-4-7"),
            ev_end("2026-05-22T11:00:00Z", "a", "s1", 100, 50, 3),
            ev_start("2026-05-22T12:00:00Z", "b", "s2", "claude-sonnet-4-6"),
            ev_end("2026-05-22T13:00:00Z", "b", "s2", 200, 100, 5),
            ev_limit("2026-05-22T14:00:00Z", "b", "s2", LimitKind::WeeklyCap),
            ev_limit("2026-05-22T15:00:00Z", "b", "s2", LimitKind::Rate429),
        ];
        let w = aggregate_weekly_tokens(&evs);
        assert_eq!(w.total_in, 300);
        assert_eq!(w.total_out, 150);
        assert_eq!(w.total(), 450);
        assert_eq!(w.weekly_cap_hits, 1);
        let opus = w.by_model.get("opus").unwrap();
        assert_eq!(opus.tokens_in, 100);
        assert_eq!(opus.tokens_out, 50);
        let sonnet = w.by_model.get("sonnet").unwrap();
        assert_eq!(sonnet.tokens_in, 200);
        assert_eq!(sonnet.tokens_out, 100);
    }

    #[test]
    fn aggregate_agents_sorted_by_tokens_desc() {
        let evs = vec![
            ev_start("2026-05-22T10:00:00Z", "a", "s1", "claude-opus-4-7"),
            ev_end("2026-05-22T11:00:00Z", "a", "s1", 100, 100, 5),
            ev_start("2026-05-22T10:00:00Z", "b", "s2", "claude-sonnet-4-6"),
            ev_end("2026-05-22T11:00:00Z", "b", "s2", 500, 500, 20),
        ];
        let rows = aggregate_agents(&evs);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].agent, "b"); // bigger first
        assert_eq!(rows[1].agent, "a");
        assert_eq!(rows[0].iterations, 20);
        assert_eq!(rows[0].sessions, 1);
        assert_eq!(rows[0].model, "claude-sonnet-4-6");
    }

    #[test]
    fn aggregate_sessions_filters_by_agent() {
        let evs = vec![
            ev_start("2026-05-22T10:00:00Z", "a", "s1", "claude-opus-4-7"),
            ev_end("2026-05-22T11:00:00Z", "a", "s1", 100, 100, 5),
            ev_start("2026-05-22T12:00:00Z", "b", "s2", "claude-sonnet-4-6"),
            ev_end("2026-05-22T13:00:00Z", "b", "s2", 500, 500, 20),
        ];
        let s = aggregate_sessions(&evs, "a");
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].session_id, "s1");
        assert_eq!(s[0].total_tokens(), 200);
    }

    #[test]
    fn arrow_logic_three_tiers_higher_is_better() {
        // value=5, p50=10, p90=20 → 5<10 ⬇
        assert_eq!(arrow_vs_baseline(5, 10, 20, true), '⬇');
        // value=15, p50=10, p90=20 → between → ✓
        assert_eq!(arrow_vs_baseline(15, 10, 20, true), '✓');
        // value=25, p50=10, p90=20 → ≥p90 → ⬆
        assert_eq!(arrow_vs_baseline(25, 10, 20, true), '⬆');
    }

    #[test]
    fn arrow_logic_lower_is_better() {
        // value=2, p50=5, p10=1 (passed as "p90") → ≤p50 ✓ but not ≤1
        assert_eq!(arrow_vs_baseline(2, 5, 1, false), '✓');
        // value=10, p50=5 → 10>5 ⬇
        assert_eq!(arrow_vs_baseline(10, 5, 1, false), '⬇');
        // value=0, p10=1 → great ⬆
        assert_eq!(arrow_vs_baseline(0, 5, 1, false), '⬆');
    }

    #[test]
    fn week_start_falls_on_monday_berlin() {
        // 2026-05-22 is a Friday. Monday of that week is 2026-05-18.
        let now = ts("2026-05-22T12:00:00Z");
        let monday = week_start_berlin(now);
        let berlin = Berlin.from_utc_datetime(&monday.naive_utc());
        assert_eq!(berlin.weekday().num_days_from_monday(), 0);
    }

    #[test]
    fn day_start_yields_midnight_berlin() {
        let now = ts("2026-05-22T22:00:00Z");
        let day = day_start_berlin(now);
        let berlin = Berlin.from_utc_datetime(&day.naive_utc());
        assert_eq!(berlin.hour(), 0);
        assert_eq!(berlin.minute(), 0);
    }

    #[test]
    fn month_start_yields_first_of_month_berlin() {
        let now = ts("2026-05-22T22:00:00Z");
        let m = month_start_berlin(now);
        let berlin = Berlin.from_utc_datetime(&m.naive_utc());
        assert_eq!(berlin.day(), 1);
    }

    #[tokio::test]
    async fn read_events_between_from_roots_unions_and_sorts() {
        // Two fixture roots, each owning one day file. The aggregator
        // must union events and return them sorted by timestamp asc.
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let root_a = dir_a.path();
        let root_b = dir_b.path();

        // Root A: one early event + one late event on 2026-05-22.
        let day_a = root_a.join("2026-05-22.jsonl");
        let a_early = ev_start("2026-05-22T08:00:00Z", "dev", "sA", "claude-opus-4-7");
        let a_late = ev_end("2026-05-22T18:00:00Z", "dev", "sA", 100, 50, 3);
        std::fs::write(
            &day_a,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&a_early).unwrap(),
                serde_json::to_string(&a_late).unwrap()
            ),
        )
        .unwrap();
        // Root B: one midday event on the same day.
        let day_b = root_b.join("2026-05-22.jsonl");
        let b_mid = ev_start("2026-05-22T12:00:00Z", "kira", "sB", "claude-sonnet-4-6");
        std::fs::write(
            &day_b,
            format!("{}\n", serde_json::to_string(&b_mid).unwrap()),
        )
        .unwrap();

        let roots = vec![root_a.to_path_buf(), root_b.to_path_buf()];
        let start = ts("2026-05-22T00:00:00Z");
        let end = ts("2026-05-22T23:59:59Z");
        let events = read_events_between_from_roots(&roots, start, end).await;

        assert_eq!(events.len(), 3, "should union events from both roots");
        // Verify sort: 08:00 (dev), 12:00 (kira), 18:00 (dev).
        let timestamps: Vec<&str> = events.iter().map(|e| e.ts()).collect();
        assert_eq!(
            timestamps,
            vec![
                "2026-05-22T08:00:00Z",
                "2026-05-22T12:00:00Z",
                "2026-05-22T18:00:00Z",
            ],
            "events must be sorted by timestamp ascending"
        );
        // Verify both agents are represented.
        let agents: std::collections::HashSet<&str> = events.iter().map(|e| e.agent()).collect();
        assert!(agents.contains("dev"));
        assert!(agents.contains("kira"));
    }

    #[tokio::test]
    async fn read_events_between_from_roots_skips_missing_roots() {
        // A non-existent root must not panic and must not pollute the
        // output of other roots that do exist. Empty input → empty output.
        let dir = tempdir().unwrap();
        let day = dir.path().join("2026-05-22.jsonl");
        let e = ev_start("2026-05-22T10:00:00Z", "dev", "s1", "claude-opus-4-7");
        std::fs::write(&day, format!("{}\n", serde_json::to_string(&e).unwrap())).unwrap();

        let missing = std::path::PathBuf::from("/nonexistent/deskd-521/events");
        let roots = vec![missing, dir.path().to_path_buf()];
        let start = ts("2026-05-22T00:00:00Z");
        let end = ts("2026-05-22T23:59:59Z");
        let events = read_events_between_from_roots(&roots, start, end).await;
        assert_eq!(events.len(), 1);

        // Empty roots list → empty result.
        let empty = read_events_between_from_roots(&[], start, end).await;
        assert!(empty.is_empty());
    }

    #[tokio::test]
    async fn read_events_between_streams_lines_in_window() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // Two day files, three events total. Window covers only the first day.
        let day1 = root.join("2026-05-22.jsonl");
        let day2 = root.join("2026-05-23.jsonl");
        let e1 = ev_start("2026-05-22T10:00:00Z", "a", "s1", "claude-opus-4-7");
        let e2 = ev_end("2026-05-22T11:00:00Z", "a", "s1", 100, 100, 1);
        let e3 = ev_start("2026-05-23T10:00:00Z", "a", "s2", "claude-opus-4-7");
        std::fs::write(
            &day1,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&e1).unwrap(),
                serde_json::to_string(&e2).unwrap()
            ),
        )
        .unwrap();
        std::fs::write(&day2, format!("{}\n", serde_json::to_string(&e3).unwrap())).unwrap();

        let start = ts("2026-05-22T00:00:00Z");
        let end = ts("2026-05-22T23:59:59Z");
        let events = read_events_between(root, start, end).await;
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn iteration_sparkline_24h_distributes_iterations() {
        let now = ts("2026-05-22T12:00:00Z");
        // One session 6h ago with 5 iterations.
        let evs = vec![ev_end("2026-05-22T06:00:00Z", "a", "s1", 10, 10, 5)];
        let s = iteration_sparkline_24h(&evs, now, 24);
        assert_eq!(s.chars().count(), 24);
        // The bin at offset (24-6=18) should be non-empty.
        let chars: Vec<char> = s.chars().collect();
        let non_dot = chars.iter().filter(|&&c| c != '·').count();
        assert_eq!(non_dot, 1, "exactly one bin should be populated");
    }

    #[test]
    fn kpi_aggregates_token_breakdown_and_sessions() {
        let evs = vec![
            ev_start("2026-05-22T10:00:00Z", "a", "s1", "claude-opus-4-7"),
            ev_end("2026-05-22T11:00:00Z", "a", "s1", 100, 50, 3),
            ev_start("2026-05-22T12:00:00Z", "b", "s2", "claude-sonnet-4-6"),
            ev_limit("2026-05-22T13:00:00Z", "b", "s2", LimitKind::WeeklyCap),
        ];
        let k = aggregate_kpi(&evs, KpiPeriod::Today);
        assert_eq!(k.tokens_in, 100);
        assert_eq!(k.tokens_out, 50);
        assert_eq!(k.tokens_total, 150);
        assert_eq!(k.sessions, 2);
        assert_eq!(k.limit_hits, 1);
        assert_eq!(k.period, "today");
    }
}
