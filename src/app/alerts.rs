//! Proactive degradation alerts (#425).
//!
//! When an agent's verdict transitions from healthy → degraded the
//! [`AlertManager`] fires an [`AlertRecord`] to every configured
//! [`AlertSink`]. Recovery (degraded → healthy) also fires once. The
//! manager dedupes within a single state — only transitions emit alerts.
//!
//! Sinks are best-effort and isolated: a failing sink is logged but does
//! not block the others or the runtime.
//!
//! The verdict source is intentionally pluggable. #422 will land the
//! canonical doctor heuristic; until then this module ships a heuristic
//! [`HeuristicVerdictSource`] that reads `{work_dir}/.deskd/usage.jsonl`.
//! Once #422 merges the source can be swapped without touching sinks or
//! the dedup machinery.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::config::{AlertSinkConfig, AlertsConfig};

/// Verdict kinds produced by the doctor heuristic. Mirrors the spec for #422
/// so that, when that ticket lands, the canonical doctor type can be swapped
/// in without touching sinks, the manager, or the dedup logic.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Healthy,
    Idle,
    Hung,
    Stuck,
    Dead,
    /// Other degraded state with a free-form label (e.g. "auth_expired").
    Degraded(String),
}

impl Verdict {
    /// True when the verdict is anything other than healthy/idle.
    /// Idle is treated as "fine" — the agent is just waiting for work.
    pub fn is_degraded(&self) -> bool {
        !matches!(self, Verdict::Healthy | Verdict::Idle)
    }

    /// Short human-readable label for the verdict ("hung", "stuck", ...).
    pub fn label(&self) -> &str {
        match self {
            Verdict::Healthy => "healthy",
            Verdict::Idle => "idle",
            Verdict::Hung => "hung",
            Verdict::Stuck => "stuck",
            Verdict::Dead => "dead",
            Verdict::Degraded(label) => label,
        }
    }
}

/// A single verdict observation for an agent at a point in time.
#[derive(Debug, Clone)]
pub struct VerdictReport {
    pub agent: String,
    pub verdict: Verdict,
    /// Human description of the signal that produced the verdict
    /// (e.g. "5/5 empty completions"). Used as `signal` field on the alert.
    pub signal: String,
    /// ISO 8601 timestamp of the most recent healthy observation, if known.
    pub last_good: Option<String>,
    /// Recommended remediation action, e.g.
    /// `"deskd agent restart life --fresh-session"`.
    pub recommended: Option<String>,
}

/// The structured alert record sent to every sink. Serialised as JSON for
/// the log sink and used to build the bus/telegram payloads.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlertRecord {
    pub agent: String,
    /// Verdict label (e.g. "hung", "healthy", "stuck"). When `kind` is
    /// `recovered` this will be `"healthy"`.
    pub verdict: String,
    pub signal: String,
    /// Whether this alert is for a transition into degradation or a recovery.
    pub kind: AlertKind,
    pub last_good: Option<String>,
    pub recommended: Option<String>,
    pub timestamp: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AlertKind {
    /// First time the verdict flipped into a degraded state.
    Degraded,
    /// Verdict returned to healthy after being degraded.
    Recovered,
}

impl AlertRecord {
    fn telegram_text(&self) -> String {
        let header = match self.kind {
            AlertKind::Degraded => format!("⚠ Agent `{}` degraded: {}", self.agent, self.verdict),
            AlertKind::Recovered => format!("✓ Agent `{}` recovered", self.agent),
        };
        let mut body = vec![header];
        body.push(format!("Signal: {}", self.signal));
        if let Some(lg) = &self.last_good {
            body.push(format!("Last good: {}", lg));
        }
        if let Some(rec) = &self.recommended {
            body.push(format!("Recommended: `{}`", rec));
        }
        body.join("\n")
    }

    fn bus_text(&self) -> String {
        format!(
            "[deskd alerts] {} agent={} verdict={} signal=\"{}\"",
            match self.kind {
                AlertKind::Degraded => "DEGRADED",
                AlertKind::Recovered => "RECOVERED",
            },
            self.agent,
            self.verdict,
            self.signal,
        )
    }
}

// ─── AlertSink trait + three implementations ────────────────────────────────

/// A sink that consumes [`AlertRecord`]s. Implementations must be cheap to
/// construct; failures inside `fire` are isolated by the manager so each
/// sink may surface its own errors via `Err(_)`.
#[async_trait]
pub trait AlertSink: Send + Sync {
    /// Short label used in tracing output ("telegram", "bus", "log").
    fn name(&self) -> &str;
    async fn fire(&self, alert: &AlertRecord) -> Result<()>;
}

/// Sink that publishes the alert as a bus message to `agent:<target>`.
pub struct BusMessageSink {
    socket_path: String,
    target_agent: String,
    source: String,
}

impl BusMessageSink {
    pub fn new(socket_path: String, target_agent: String, source: String) -> Self {
        Self {
            socket_path,
            target_agent,
            source,
        }
    }
}

#[async_trait]
impl AlertSink for BusMessageSink {
    fn name(&self) -> &str {
        "bus_message"
    }

    async fn fire(&self, alert: &AlertRecord) -> Result<()> {
        let target = format!("agent:{}", self.target_agent);
        crate::app::bus::send_message(&self.socket_path, &self.source, &target, &alert.bus_text())
            .await
    }
}

/// Sink that publishes the alert as a Telegram message via the existing
/// telegram adapter — we just send to `telegram.out:<chat_id>` and the
/// adapter handles the actual API call.
pub struct TelegramSink {
    socket_path: String,
    chat_id: i64,
    source: String,
}

impl TelegramSink {
    pub fn new(socket_path: String, chat_id: i64, source: String) -> Self {
        Self {
            socket_path,
            chat_id,
            source,
        }
    }
}

#[async_trait]
impl AlertSink for TelegramSink {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn fire(&self, alert: &AlertRecord) -> Result<()> {
        let target = format!("telegram.out:{}", self.chat_id);
        crate::app::bus::send_message(
            &self.socket_path,
            &self.source,
            &target,
            &alert.telegram_text(),
        )
        .await
    }
}

/// Sink that appends the alert (one JSON object per line) to a JSONL file.
pub struct LogSink {
    path: PathBuf,
}

impl LogSink {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

#[async_trait]
impl AlertSink for LogSink {
    fn name(&self) -> &str {
        "log"
    }

    async fn fire(&self, alert: &AlertRecord) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        let mut line =
            serde_json::to_string(alert).context("serialize alert record for log sink")?;
        line.push('\n');
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await
            .with_context(|| format!("open alert log: {}", self.path.display()))?;
        file.write_all(line.as_bytes())
            .await
            .with_context(|| format!("write alert log: {}", self.path.display()))?;
        // tokio::fs::File buffers writes internally; without an explicit flush
        // here, drop happens before the buffer is committed to the kernel and
        // a subsequent `read_to_string` (or another append) misses the data.
        // This was the pre-existing flake in `log_sink_writes_jsonl` (#428 CI).
        file.flush()
            .await
            .with_context(|| format!("flush alert log: {}", self.path.display()))?;
        Ok(())
    }
}

// ─── AlertManager ────────────────────────────────────────────────────────────

/// Tracks per-agent verdict state and emits transition alerts to all sinks.
pub struct AlertManager {
    sinks: Vec<Arc<dyn AlertSink>>,
    /// Last verdict observed per agent. Used to dedup: an alert only fires
    /// when the verdict transitions across the healthy/degraded boundary.
    last_verdict: Mutex<HashMap<String, Verdict>>,
}

impl AlertManager {
    pub fn new(sinks: Vec<Arc<dyn AlertSink>>) -> Self {
        Self {
            sinks,
            last_verdict: Mutex::new(HashMap::new()),
        }
    }

    pub fn from_config(cfg: &AlertsConfig, bus_socket: &str, agent_name: &str) -> Self {
        let mut sinks: Vec<Arc<dyn AlertSink>> = Vec::with_capacity(cfg.sinks.len());
        let source = format!("alerts-{}", agent_name);
        for s in &cfg.sinks {
            match s {
                AlertSinkConfig::BusMessage { target_agent } => {
                    sinks.push(Arc::new(BusMessageSink::new(
                        bus_socket.to_string(),
                        target_agent.clone(),
                        source.clone(),
                    )));
                }
                AlertSinkConfig::Telegram { chat_id } => {
                    let parsed: i64 = match chat_id.parse() {
                        Ok(v) => v,
                        Err(e) => {
                            warn!(chat_id = %chat_id, error = %e, "invalid telegram chat_id in alerts config — skipping sink");
                            continue;
                        }
                    };
                    sinks.push(Arc::new(TelegramSink::new(
                        bus_socket.to_string(),
                        parsed,
                        source.clone(),
                    )));
                }
                AlertSinkConfig::Log { path } => {
                    sinks.push(Arc::new(LogSink::new(path.clone())));
                }
            }
        }
        Self::new(sinks)
    }

    /// True when no sinks are configured — the alert pipeline can be skipped.
    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }

    /// Process a batch of verdict reports. For each report, if the verdict
    /// transitioned across the healthy/degraded boundary, build an alert and
    /// dispatch it to every sink. Sink failures are logged but do not block
    /// other sinks or other reports.
    pub async fn observe(&self, reports: Vec<VerdictReport>) {
        for report in reports {
            if let Some(alert) = self.transition_alert(&report).await {
                self.dispatch(&alert).await;
            }
        }
    }

    /// Decide whether `report` represents a verdict transition. If so, return
    /// the [`AlertRecord`] to emit and update the per-agent last-verdict map.
    async fn transition_alert(&self, report: &VerdictReport) -> Option<AlertRecord> {
        let mut state = self.last_verdict.lock().await;
        let prev = state.get(&report.agent).cloned();
        let new_is_degraded = report.verdict.is_degraded();
        let prev_is_degraded = prev.as_ref().map(|v| v.is_degraded()).unwrap_or(false);

        // Always update the state map so subsequent calls see the latest.
        state.insert(report.agent.clone(), report.verdict.clone());

        let kind = match (prev_is_degraded, new_is_degraded) {
            (false, true) => AlertKind::Degraded,
            (true, false) => AlertKind::Recovered,
            // No transition across the boundary → dedup, no alert.
            _ => return None,
        };

        Some(AlertRecord {
            agent: report.agent.clone(),
            verdict: report.verdict.label().to_string(),
            signal: report.signal.clone(),
            kind,
            last_good: report.last_good.clone(),
            recommended: report.recommended.clone(),
            timestamp: chrono::Utc::now().to_rfc3339(),
        })
    }

    /// Dispatch `alert` to every sink. Each sink runs in its own try/await so
    /// a failing sink does not block the others.
    async fn dispatch(&self, alert: &AlertRecord) {
        for sink in &self.sinks {
            match sink.fire(alert).await {
                Ok(()) => {
                    info!(
                        sink = sink.name(),
                        agent = %alert.agent,
                        verdict = %alert.verdict,
                        kind = ?alert.kind,
                        "alert delivered"
                    );
                }
                Err(e) => {
                    warn!(
                        sink = sink.name(),
                        agent = %alert.agent,
                        error = %e,
                        "alert sink failed (other sinks unaffected)"
                    );
                }
            }
        }
    }
}

// ─── Verdict source (placeholder until #422 lands) ───────────────────────────

/// Source of verdict reports. The doctor heuristic from #422 will implement
/// this trait; in the meantime [`HeuristicVerdictSource`] provides a minimal
/// stand-in that reads usage.jsonl per agent.
#[async_trait]
pub trait VerdictSource: Send + Sync {
    async fn poll(&self) -> Result<Vec<VerdictReport>>;
}

/// A minimal usage-jsonl-based verdict source. Reports `Idle` for every
/// known agent. This is an intentionally conservative placeholder so the
/// alert plumbing can be exercised end-to-end before #422 lands. Once #422
/// merges, swap this for the canonical doctor module.
pub struct HeuristicVerdictSource {
    agents: Vec<(String, PathBuf)>,
}

impl HeuristicVerdictSource {
    pub fn new(agents: Vec<(String, PathBuf)>) -> Self {
        Self { agents }
    }
}

#[async_trait]
impl VerdictSource for HeuristicVerdictSource {
    async fn poll(&self) -> Result<Vec<VerdictReport>> {
        let mut out = Vec::with_capacity(self.agents.len());
        for (name, _usage_path) in &self.agents {
            // Placeholder until #422: report Idle, which is treated as
            // healthy-equivalent and never produces an alert. The dedup
            // and sink machinery is exercised by tests with a mock source.
            out.push(VerdictReport {
                agent: name.clone(),
                verdict: Verdict::Idle,
                signal: "placeholder verdict source — awaiting #422".to_string(),
                last_good: None,
                recommended: None,
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Counting sink — increments a counter on every successful fire.
    struct CountingSink {
        name: &'static str,
        count: Arc<AtomicUsize>,
        captured: Arc<Mutex<Vec<AlertRecord>>>,
    }

    impl CountingSink {
        fn new(name: &'static str) -> (Self, Arc<AtomicUsize>, Arc<Mutex<Vec<AlertRecord>>>) {
            let count = Arc::new(AtomicUsize::new(0));
            let captured = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    name,
                    count: count.clone(),
                    captured: captured.clone(),
                },
                count,
                captured,
            )
        }
    }

    #[async_trait]
    impl AlertSink for CountingSink {
        fn name(&self) -> &str {
            self.name
        }
        async fn fire(&self, alert: &AlertRecord) -> Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            self.captured.lock().await.push(alert.clone());
            Ok(())
        }
    }

    /// Always-failing sink — used to prove sink isolation.
    struct FailingSink {
        count: Arc<AtomicUsize>,
    }

    impl FailingSink {
        fn new() -> (Self, Arc<AtomicUsize>) {
            let count = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    count: count.clone(),
                },
                count,
            )
        }
    }

    #[async_trait]
    impl AlertSink for FailingSink {
        fn name(&self) -> &str {
            "failing"
        }
        async fn fire(&self, _alert: &AlertRecord) -> Result<()> {
            self.count.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("simulated sink failure")
        }
    }

    fn report(agent: &str, verdict: Verdict) -> VerdictReport {
        VerdictReport {
            agent: agent.to_string(),
            verdict,
            signal: "test signal".to_string(),
            last_good: Some("2026-04-27T09:55:57Z".to_string()),
            recommended: Some("deskd agent restart life --fresh-session".to_string()),
        }
    }

    #[tokio::test]
    async fn transition_into_degraded_fires_alert() {
        let (sink, count, captured) = CountingSink::new("count");
        let mgr = AlertManager::new(vec![Arc::new(sink)]);

        // First observation: healthy → no alert.
        mgr.observe(vec![report("life", Verdict::Healthy)]).await;
        assert_eq!(count.load(Ordering::SeqCst), 0);

        // Transition healthy → hung → fires.
        mgr.observe(vec![report("life", Verdict::Hung)]).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let captured = captured.lock().await;
        assert_eq!(captured[0].kind, AlertKind::Degraded);
        assert_eq!(captured[0].verdict, "hung");
        assert_eq!(captured[0].agent, "life");
    }

    #[tokio::test]
    async fn dedup_holds_across_multiple_ticks() {
        let (sink, count, _) = CountingSink::new("count");
        let mgr = AlertManager::new(vec![Arc::new(sink)]);

        mgr.observe(vec![report("life", Verdict::Healthy)]).await;
        // Five degraded ticks in a row → only one alert.
        for _ in 0..5 {
            mgr.observe(vec![report("life", Verdict::Hung)]).await;
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            1,
            "expected exactly one alert across five degraded ticks"
        );
    }

    #[tokio::test]
    async fn recovery_fires_when_verdict_returns_to_healthy() {
        let (sink, count, captured) = CountingSink::new("count");
        let mgr = AlertManager::new(vec![Arc::new(sink)]);

        mgr.observe(vec![report("life", Verdict::Healthy)]).await;
        mgr.observe(vec![report("life", Verdict::Stuck)]).await;
        // Many stuck ticks dedup.
        mgr.observe(vec![report("life", Verdict::Stuck)]).await;
        // Recovery fires once.
        mgr.observe(vec![report("life", Verdict::Healthy)]).await;
        // Subsequent healthy ticks dedup.
        mgr.observe(vec![report("life", Verdict::Healthy)]).await;

        assert_eq!(count.load(Ordering::SeqCst), 2);
        let captured = captured.lock().await;
        assert_eq!(captured[0].kind, AlertKind::Degraded);
        assert_eq!(captured[1].kind, AlertKind::Recovered);
        assert_eq!(captured[1].verdict, "healthy");
    }

    #[tokio::test]
    async fn flapping_into_degraded_again_fires_again() {
        let (sink, count, _) = CountingSink::new("count");
        let mgr = AlertManager::new(vec![Arc::new(sink)]);

        mgr.observe(vec![report("life", Verdict::Healthy)]).await;
        mgr.observe(vec![report("life", Verdict::Hung)]).await; // alert 1
        mgr.observe(vec![report("life", Verdict::Healthy)]).await; // alert 2 (recovery)
        mgr.observe(vec![report("life", Verdict::Hung)]).await; // alert 3
        mgr.observe(vec![report("life", Verdict::Healthy)]).await; // alert 4 (recovery)

        assert_eq!(count.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn sink_failure_does_not_block_other_sinks() {
        let (good, good_count, _) = CountingSink::new("good");
        let (bad, bad_count) = FailingSink::new();
        let mgr = AlertManager::new(vec![Arc::new(bad), Arc::new(good)]);

        mgr.observe(vec![report("life", Verdict::Healthy)]).await;
        mgr.observe(vec![report("life", Verdict::Hung)]).await;

        assert_eq!(
            bad_count.load(Ordering::SeqCst),
            1,
            "failing sink was attempted"
        );
        assert_eq!(
            good_count.load(Ordering::SeqCst),
            1,
            "good sink fired despite earlier sink failure"
        );
    }

    #[tokio::test]
    async fn idle_does_not_count_as_degraded() {
        let (sink, count, _) = CountingSink::new("count");
        let mgr = AlertManager::new(vec![Arc::new(sink)]);

        mgr.observe(vec![report("life", Verdict::Healthy)]).await;
        mgr.observe(vec![report("life", Verdict::Idle)]).await;
        mgr.observe(vec![report("life", Verdict::Idle)]).await;
        mgr.observe(vec![report("life", Verdict::Healthy)]).await;
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn first_observation_degraded_fires_alert() {
        // Cold-start: no prior verdict known. Treat the first observation
        // of a degraded verdict as a transition (prev "unknown" is healthy-ish).
        let (sink, count, _) = CountingSink::new("count");
        let mgr = AlertManager::new(vec![Arc::new(sink)]);
        mgr.observe(vec![report("life", Verdict::Hung)]).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn per_agent_state_is_independent() {
        let (sink, count, captured) = CountingSink::new("count");
        let mgr = AlertManager::new(vec![Arc::new(sink)]);

        mgr.observe(vec![
            report("life", Verdict::Healthy),
            report("dev", Verdict::Healthy),
        ])
        .await;
        // Only `life` flips.
        mgr.observe(vec![
            report("life", Verdict::Hung),
            report("dev", Verdict::Healthy),
        ])
        .await;
        // dev flips next.
        mgr.observe(vec![
            report("life", Verdict::Hung),
            report("dev", Verdict::Stuck),
        ])
        .await;

        assert_eq!(count.load(Ordering::SeqCst), 2);
        let captured = captured.lock().await;
        assert_eq!(captured[0].agent, "life");
        assert_eq!(captured[1].agent, "dev");
    }

    #[tokio::test]
    async fn log_sink_writes_jsonl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("alerts.jsonl");
        let sink = LogSink::new(&path);
        let mgr = AlertManager::new(vec![Arc::new(sink)]);
        mgr.observe(vec![report("life", Verdict::Hung)]).await;
        mgr.observe(vec![report("life", Verdict::Healthy)]).await;

        let contents = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = contents.lines().collect();
        assert_eq!(
            lines.len(),
            2,
            "expected 2 alert lines (Degraded + Recovered), got {}: {:?}",
            lines.len(),
            contents,
        );
        let first: AlertRecord = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(first.kind, AlertKind::Degraded);
        let second: AlertRecord = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(second.kind, AlertKind::Recovered);
    }

    #[tokio::test]
    async fn empty_manager_is_noop() {
        let mgr = AlertManager::new(vec![]);
        assert!(mgr.is_empty());
        // Should not panic.
        mgr.observe(vec![report("life", Verdict::Hung)]).await;
    }
}
