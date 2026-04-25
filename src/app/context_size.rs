//! Live context-size aggregation across all active agents.
//!
//! Implements issue #393: a snapshot of how much of each session's context
//! window is currently consumed, derived from the latest task log entry for
//! every live agent.
//!
//! "Current live context size" is approximated by the latest task's
//! `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`
//! recorded in `~/.deskd/logs/<agent>/tasks.jsonl`. Each turn's input total
//! reflects everything Claude pinned in the window for that turn — the most
//! recent entry is therefore a good proxy for the live size.

use chrono::DateTime;

use crate::app::agent_registry::AgentState;
use crate::app::tasklog::{self, TaskLog};

/// Default context window for Claude 3.x models and unknown model ids.
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// Context window for Claude 4.x family (Opus, Sonnet, Haiku).
pub const CONTEXT_WINDOW_CLAUDE_4: u64 = 1_000_000;

/// Built-in default auto-compact threshold in tokens (300k).
/// Applied when neither a per-agent nor a global override is configured.
pub const DEFAULT_AUTO_COMPACT_THRESHOLD: u64 = 300_000;

/// Maximum percentage value accepted by `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`.
/// Values above this are silently clamped by Claude Code (anthropics/claude-code#31806).
pub const AUTO_COMPACT_PCT_MAX: u64 = 83;

/// Threshold (fraction of threshold) at which we surface a warning indicator.
/// 80% of the configured auto-compact threshold — not of the model window.
pub const WARN_THRESHOLD: f64 = 0.80;

/// One row in the `/context` snapshot — a single agent's current session.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub agent: String,
    pub model: String,
    pub session_id: String,
    /// Tokens currently pinned in the session window (input + cache).
    /// `None` when no task log data is available for the current session
    /// (e.g. ACP runtimes that don't emit per-turn token usage yet).
    pub context_tokens: Option<u64>,
    /// Context window for the model in use.
    pub context_limit: u64,
    /// Resolved auto-compact threshold for this agent (absolute tokens).
    pub auto_compact_threshold: u64,
}

impl SessionContext {
    /// First 8 chars of the session UUID — enough for at-a-glance distinction
    /// without overwhelming the Telegram line.
    pub fn session_short(&self) -> String {
        let trimmed = self.session_id.trim();
        let take = trimmed.chars().take(8).collect::<String>();
        if take.is_empty() {
            "(no session)".to_string()
        } else {
            take
        }
    }

    /// Fraction of the window consumed. Returns 0.0 when token data is
    /// unavailable so callers can branch cleanly without unwrapping.
    pub fn utilization(&self) -> f64 {
        match self.context_tokens {
            Some(t) if self.context_limit > 0 => t as f64 / self.context_limit as f64,
            _ => 0.0,
        }
    }

    /// Whether the auto-compact threshold warning should fire.
    /// Triggers at 80% of the configured threshold — not the model window.
    pub fn is_warning(&self) -> bool {
        match self.context_tokens {
            Some(t) if self.auto_compact_threshold > 0 => {
                t as f64 >= self.auto_compact_threshold as f64 * WARN_THRESHOLD
            }
            _ => false,
        }
    }

    /// Whether the threshold would clamp (i.e. threshold >= window * 0.83).
    /// Used to surface a warning marker in /context output.
    pub fn threshold_would_clamp(&self) -> bool {
        self.context_limit > 0
            && self.auto_compact_threshold as f64
                >= self.context_limit as f64 * (AUTO_COMPACT_PCT_MAX as f64 / 100.0)
    }
}

/// Pick the context window size for a given Claude model name.
///
/// - `claude-(opus|sonnet|haiku)-4-*` — 1M token window (Claude 4.x family).
/// - `claude-3-*` — 200k token window.
/// - Any unrecognised id — 200k (safe default).
///
/// Single source of truth; no caller should hardcode 200k or 1M literals.
pub fn context_window_for_model(model: &str) -> u64 {
    // Claude 4.x: claude-opus-4-*, claude-sonnet-4-*, claude-haiku-4-*
    let is_claude_4 = model.starts_with("claude-opus-4-")
        || model.starts_with("claude-sonnet-4-")
        || model.starts_with("claude-haiku-4-");
    if is_claude_4 {
        return CONTEXT_WINDOW_CLAUDE_4;
    }
    // Claude 3.x and all unrecognised ids.
    DEFAULT_CONTEXT_WINDOW
}

/// Compute the `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE` value from a resolved
/// threshold and model window.
///
/// Formula: `(threshold / window * 100).round().clamp(1, 83)`.
/// Returns `(pct, clamped)` where `clamped` is true if the raw value exceeded
/// `AUTO_COMPACT_PCT_MAX` (signals that the user override is ineffective).
pub fn autocompact_pct(threshold_tokens: u64, window_tokens: u64) -> (u64, bool) {
    if window_tokens == 0 {
        return (AUTO_COMPACT_PCT_MAX, false);
    }
    let raw = (threshold_tokens as f64 / window_tokens as f64 * 100.0).round() as u64;
    let clamped = raw > AUTO_COMPACT_PCT_MAX;
    let pct = raw.clamp(1, AUTO_COMPACT_PCT_MAX);
    (pct, clamped)
}

/// Determine whether an agent's process is currently running.
///
/// Mirrors the check used by `deskd agent status` (`/proc/<pid>` existence).
/// We deliberately avoid talking to per-agent buses here — gathering context
/// sizes must work even when the caller doesn't have access to every socket.
fn is_pid_alive(pid: u32) -> bool {
    pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).exists()
}

/// Whether a given agent state should be considered "live" for the purposes
/// of `/context`. We require a non-empty `session_id` (otherwise there is no
/// session to report on). Process liveness is opportunistic:
///   * `state.pid == 0` — top-level agents started by `deskd serve` don't
///     record a PID (only sub-agents spawned via MCP do). Treat as live.
///   * `state.pid > 0` — sub-agent path; verify the worker process exists.
///
/// This trades a small risk of reporting stale data for a top-level agent
/// that crashed mid-session against the much worse failure mode of showing
/// "No active sessions" when sessions clearly exist.
fn is_live(state: &AgentState) -> bool {
    if state.session_id.is_empty() {
        return false;
    }
    state.pid == 0 || is_pid_alive(state.pid)
}

/// Find the most recent task log entry that belongs to the agent's current
/// session, falling back to the latest entry overall when `session_start`
/// is unavailable.
fn latest_session_entry(state: &AgentState) -> Option<TaskLog> {
    let since = state
        .session_start
        .as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&chrono::Utc));

    // Read with a generous limit so the latest entry is included even when
    // the file is large.
    let entries = tasklog::read_logs(&state.config.name, usize::MAX, None, since).ok()?;
    entries.into_iter().last()
}

/// Compute live context tokens from a task log entry: the input total plus
/// cache reads/creations represents what's currently pinned in the window.
fn entry_context_tokens(entry: &TaskLog) -> Option<u64> {
    let input = entry.input_tokens?;
    let cache_creation = entry.cache_creation_input_tokens.unwrap_or(0);
    let cache_read = entry.cache_read_input_tokens.unwrap_or(0);
    Some(input + cache_creation + cache_read)
}

/// Build a `SessionContext` for a live agent. Returns `None` when the agent
/// is not live (no session / dead PID).
fn snapshot_for(state: &AgentState) -> Option<SessionContext> {
    if !is_live(state) {
        return None;
    }

    let context_tokens = latest_session_entry(state)
        .as_ref()
        .and_then(entry_context_tokens);

    let context_limit = context_window_for_model(&state.config.model);
    let auto_compact_threshold = state
        .config
        .auto_compact_threshold_tokens
        .unwrap_or(DEFAULT_AUTO_COMPACT_THRESHOLD);

    Some(SessionContext {
        agent: state.config.name.clone(),
        model: state.config.model.clone(),
        session_id: state.session_id.clone(),
        context_tokens,
        context_limit,
        auto_compact_threshold,
    })
}

/// Gather a context-size snapshot across every agent registered on the host.
///
/// Only live agents (running PID with an active session) are included.
pub async fn gather() -> anyhow::Result<Vec<SessionContext>> {
    let agents = crate::app::agent::list().await?;
    let mut out: Vec<SessionContext> = agents.iter().filter_map(snapshot_for).collect();
    out.sort_by(|a, b| a.agent.cmp(&b.agent).then(a.session_id.cmp(&b.session_id)));
    Ok(out)
}

/// Format a token count compactly: 45_000 → "45k", 1_500 → "1.5k", small
/// values stay as-is. Designed to fit in narrow Telegram lines.
fn format_tokens_compact(n: u64) -> String {
    if n < 1_000 {
        return n.to_string();
    }
    let k = n as f64 / 1_000.0;
    if k >= 10.0 {
        format!("{}k", k.round() as u64)
    } else {
        // Small enough to want one decimal — e.g. "1.5k".
        let rounded = (k * 10.0).round() / 10.0;
        if (rounded - rounded.round()).abs() < f64::EPSILON {
            format!("{}k", rounded as u64)
        } else {
            format!("{:.1}k", rounded)
        }
    }
}

/// Render a snapshot as a Telegram-friendly plain-text reply.
pub fn format_reply(snapshot: &[SessionContext]) -> String {
    if snapshot.is_empty() {
        return "No active sessions.".to_string();
    }

    let mut lines = Vec::with_capacity(snapshot.len() + 1);
    lines.push("/context".to_string());
    lines.push(String::new()); // blank line under the header

    for s in snapshot {
        let limit = format_tokens_compact(s.context_limit);
        let threshold = format_tokens_compact(s.auto_compact_threshold);
        let (pct, would_clamp) = autocompact_pct(s.auto_compact_threshold, s.context_limit);
        let clamp_marker = if would_clamp { " ⚠️" } else { "" };
        let line = match s.context_tokens {
            Some(tokens) => {
                let used = format_tokens_compact(tokens);
                let warn = if s.is_warning() { "  ⚠️" } else { "" };
                format!(
                    "{} (session {})  ~{} / {}  (auto-compact at {} = {}%{}){}",
                    s.agent,
                    s.session_short(),
                    used,
                    limit,
                    threshold,
                    pct,
                    clamp_marker,
                    warn
                )
            }
            None => format!(
                "{} (session {})  n/a / {}  (auto-compact at {} = {}%{})",
                s.agent,
                s.session_short(),
                limit,
                threshold,
                pct,
                clamp_marker
            ),
        };
        lines.push(line);
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_entry(input: u64, cache_creation: u64, cache_read: u64) -> TaskLog {
        TaskLog {
            ts: chrono::Utc::now().to_rfc3339(),
            source: "test".into(),
            turns: 1,
            cost: 0.0,
            duration_ms: 0,
            status: "ok".into(),
            task: "t".into(),
            error: None,
            msg_id: "m".into(),
            github_repo: None,
            github_pr: None,
            input_tokens: Some(input),
            output_tokens: None,
            cache_creation_input_tokens: Some(cache_creation),
            cache_read_input_tokens: Some(cache_read),
            session_count: None,
            tool_use_count: None,
            parent_agent: None,
        }
    }

    #[test]
    fn entry_context_tokens_sums_input_and_cache() {
        let e = mk_entry(1_000, 200, 50_000);
        assert_eq!(entry_context_tokens(&e), Some(51_200));
    }

    #[test]
    fn entry_context_tokens_returns_none_without_input() {
        let mut e = mk_entry(0, 0, 0);
        e.input_tokens = None;
        assert_eq!(entry_context_tokens(&e), None);
    }

    fn mk_session(
        agent: &str,
        model: &str,
        context_tokens: Option<u64>,
        auto_compact_threshold: u64,
    ) -> SessionContext {
        let context_limit = context_window_for_model(model);
        SessionContext {
            agent: agent.into(),
            model: model.into(),
            session_id: "abcdef0123456789".into(),
            context_tokens,
            context_limit,
            auto_compact_threshold,
        }
    }

    #[test]
    fn session_short_takes_first_eight_chars() {
        let s = mk_session("kira", "claude-opus-4", Some(100), DEFAULT_AUTO_COMPACT_THRESHOLD);
        assert_eq!(s.session_short(), "abcdef01");
    }

    #[test]
    fn session_short_handles_short_or_blank_ids() {
        let mut s = SessionContext {
            agent: "kira".into(),
            model: "claude-opus-4".into(),
            session_id: "abc".into(),
            context_tokens: None,
            context_limit: context_window_for_model("claude-opus-4"),
            auto_compact_threshold: DEFAULT_AUTO_COMPACT_THRESHOLD,
        };
        assert_eq!(s.session_short(), "abc");
        s.session_id = "   ".into();
        assert_eq!(s.session_short(), "(no session)");
    }

    #[test]
    fn warning_triggers_at_eighty_percent_of_threshold() {
        // threshold=300k, 80% = 240k
        let threshold = 300_000u64;
        let warn_at = (threshold as f64 * 0.80) as u64; // 240000
        let mut s = SessionContext {
            agent: "a".into(),
            model: "claude-opus-4-7".into(),
            session_id: "xxxxxxxx".into(),
            context_tokens: Some(warn_at),
            context_limit: context_window_for_model("claude-opus-4-7"),
            auto_compact_threshold: threshold,
        };
        assert!(s.is_warning());
        s.context_tokens = Some(warn_at - 1);
        assert!(!s.is_warning());
    }

    #[test]
    fn format_tokens_compact_buckets() {
        assert_eq!(format_tokens_compact(0), "0");
        assert_eq!(format_tokens_compact(999), "999");
        assert_eq!(format_tokens_compact(1_000), "1k");
        assert_eq!(format_tokens_compact(1_500), "1.5k");
        assert_eq!(format_tokens_compact(45_000), "45k");
        assert_eq!(format_tokens_compact(180_000), "180k");
    }

    #[test]
    fn format_reply_empty_snapshot_says_no_sessions() {
        assert_eq!(format_reply(&[]), "No active sessions.");
    }

    #[test]
    fn format_reply_renders_lines_with_warning() {
        // agent-a/session1: 45k used, threshold=300k on 200k window → clamps → ⚠️ marker on threshold
        // agent-a/session2: 250k used, threshold=300k → 250k >= 300k*0.8=240k → warn ⚠️
        // agent-b: no tokens, threshold=300k on 200k window (clamp)
        let threshold = DEFAULT_AUTO_COMPACT_THRESHOLD;
        let snap = vec![
            SessionContext {
                agent: "agent-a".into(),
                model: "claude-opus-4".into(),
                session_id: "abc12345xx".into(),
                context_tokens: Some(45_000),
                context_limit: context_window_for_model("claude-opus-4"),
                auto_compact_threshold: threshold,
            },
            SessionContext {
                agent: "agent-a".into(),
                model: "claude-opus-4-7".into(),
                session_id: "def45678yy".into(),
                context_tokens: Some(250_000),
                context_limit: context_window_for_model("claude-opus-4-7"),
                auto_compact_threshold: threshold,
            },
            SessionContext {
                agent: "agent-b".into(),
                model: "claude-sonnet".into(),
                session_id: "ghi78901zz".into(),
                context_tokens: None,
                context_limit: context_window_for_model("claude-sonnet"),
                auto_compact_threshold: threshold,
            },
        ];
        let reply = format_reply(&snap);
        assert!(reply.starts_with("/context\n"));
        // session1: 200k window, 300k threshold → clamp marker ⚠️ on threshold portion
        assert!(reply.contains("agent-a (session abc12345)  ~45k / 200k"));
        assert!(reply.contains("auto-compact at 300k = 83%"));
        // session2: 1M window, 300k threshold = 30%, 250k >= 240k → warn ⚠️
        assert!(reply.contains("agent-a (session def45678)  ~250k / 1000k"));
        assert!(reply.contains("auto-compact at 300k = 30%"));
        assert!(reply.contains("⚠️")); // warning on session2
        // agent-b: n/a
        assert!(reply.contains("agent-b (session ghi78901)  n/a / 200k"));
    }

    #[test]
    fn context_window_for_model_returns_correct_windows() {
        // Claude 4.x family → 1M
        assert_eq!(context_window_for_model("claude-opus-4-7"), CONTEXT_WINDOW_CLAUDE_4);
        assert_eq!(context_window_for_model("claude-opus-4-6"), CONTEXT_WINDOW_CLAUDE_4);
        assert_eq!(context_window_for_model("claude-sonnet-4-6"), CONTEXT_WINDOW_CLAUDE_4);
        assert_eq!(context_window_for_model("claude-haiku-4-5"), CONTEXT_WINDOW_CLAUDE_4);
        // Claude 3.x → 200k
        assert_eq!(context_window_for_model("claude-opus-3"), DEFAULT_CONTEXT_WINDOW);
        assert_eq!(context_window_for_model("claude-sonnet-3-5"), DEFAULT_CONTEXT_WINDOW);
        // Unknown → 200k safe default
        assert_eq!(context_window_for_model("anything-else"), DEFAULT_CONTEXT_WINDOW);
        assert_eq!(context_window_for_model(""), DEFAULT_CONTEXT_WINDOW);
        // claude-opus-4 (no version suffix) → NOT matched by claude-opus-4- prefix → 200k
        assert_eq!(context_window_for_model("claude-opus-4"), DEFAULT_CONTEXT_WINDOW);
    }

    #[test]
    fn autocompact_pct_normal_cases() {
        // 300k on 1M window = 30%
        let (pct, clamped) = autocompact_pct(300_000, 1_000_000);
        assert_eq!(pct, 30);
        assert!(!clamped);

        // 300k on 200k window → 150% → clamps to 83
        let (pct, clamped) = autocompact_pct(300_000, 200_000);
        assert_eq!(pct, AUTO_COMPACT_PCT_MAX);
        assert!(clamped);

        // 800k on 1M window = 80% < 83 → no clamp
        let (pct, clamped) = autocompact_pct(800_000, 1_000_000);
        assert_eq!(pct, 80);
        assert!(!clamped);

        // 1 token on 1M → rounds to 0 → clamped to 1
        let (pct, clamped) = autocompact_pct(1, 1_000_000);
        assert_eq!(pct, 1);
        assert!(!clamped);
    }

    #[test]
    fn threshold_would_clamp_reflects_clamping() {
        // 300k threshold on 200k window → threshold >= 200k * 0.83 = 166k → clamps
        let s = SessionContext {
            agent: "x".into(),
            model: "claude-opus-4".into(),
            session_id: "s".into(),
            context_tokens: None,
            context_limit: DEFAULT_CONTEXT_WINDOW,
            auto_compact_threshold: DEFAULT_AUTO_COMPACT_THRESHOLD,
        };
        assert!(s.threshold_would_clamp());

        // 300k threshold on 1M window → threshold < 1M * 0.83 = 830k → no clamp
        let s2 = SessionContext {
            context_limit: CONTEXT_WINDOW_CLAUDE_4,
            ..s
        };
        assert!(!s2.threshold_would_clamp());
    }

    fn mk_state(pid: u32, session_id: &str) -> AgentState {
        use crate::app::agent_registry::AgentConfig;
        AgentState {
            config: AgentConfig {
                name: "kira".into(),
                model: "claude-opus-4".into(),
                system_prompt: String::new(),
                work_dir: "/tmp".into(),
                max_turns: 100,
                unix_user: None,
                budget_usd: 50.0,
                command: vec!["claude".into()],
                config_path: None,
                container: None,
                session: Default::default(),
                runtime: Default::default(),
                context: None,
                compact_threshold: None,
                auto_compact_threshold_tokens: None,
            },
            pid,
            session_id: session_id.into(),
            total_turns: 0,
            total_cost: 0.0,
            created_at: String::new(),
            status: "idle".into(),
            current_task: String::new(),
            parent: None,
            scope: None,
            can_message: None,
            env_keys: None,
            session_start: None,
            session_cost: 0.0,
            session_turns: 0,
        }
    }

    #[test]
    fn is_live_treats_top_level_agent_with_zero_pid_as_live() {
        // Regression: top-level agents started via `deskd serve` never have
        // their state.pid updated from the default 0 (only MCP-spawned
        // sub-agents do). They must still appear in `/context`.
        let state = mk_state(0, "abcdef0123456789");
        assert!(is_live(&state));
    }

    #[test]
    fn is_live_rejects_empty_session_id() {
        // No session means nothing to report on, regardless of pid.
        let state = mk_state(0, "");
        assert!(!is_live(&state));
        let state = mk_state(std::process::id(), "");
        assert!(!is_live(&state));
    }

    #[test]
    fn is_live_accepts_subagent_with_running_pid() {
        // Sub-agent path: pid > 0 and process exists.
        let state = mk_state(std::process::id(), "abcdef0123456789");
        assert!(is_live(&state));
    }

    #[test]
    fn is_live_rejects_subagent_with_dead_pid() {
        // Sub-agent path: pid > 0 but process is gone.
        // PID 1 is the init/systemd process and always alive on Linux, so
        // pick a deliberately implausible pid that won't exist.
        let state = mk_state(u32::MAX - 1, "abcdef0123456789");
        assert!(!is_live(&state));
    }
}
