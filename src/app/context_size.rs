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

/// Default context window for Claude models (Opus 4.x / Sonnet / Haiku all
/// share a 200k token window today).
pub const DEFAULT_CONTEXT_WINDOW: u64 = 200_000;

/// Threshold (fraction of context window) at which we surface a warning
/// indicator. Matches the conventional 80% compaction trigger.
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

    pub fn is_warning(&self) -> bool {
        self.utilization() >= WARN_THRESHOLD
    }
}

/// Pick the context window size for a given Claude model name.
///
/// All current Claude families (Opus, Sonnet, Haiku, including the 4.x
/// generation) ship with a 200k window, so we just default to that. The
/// helper exists so future per-model overrides have one obvious place to live.
pub fn context_window_for_model(_model: &str) -> u64 {
    DEFAULT_CONTEXT_WINDOW
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
/// of `/context`. We require both:
///   * a non-empty `session_id` (otherwise there is no session to report on)
///   * the agent process to be running (PID alive)
fn is_live(state: &AgentState) -> bool {
    !state.session_id.is_empty() && is_pid_alive(state.pid)
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

    Some(SessionContext {
        agent: state.config.name.clone(),
        model: state.config.model.clone(),
        session_id: state.session_id.clone(),
        context_tokens,
        context_limit: context_window_for_model(&state.config.model),
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
        let line = match s.context_tokens {
            Some(tokens) => {
                let used = format_tokens_compact(tokens);
                let warn = if s.is_warning() { "  ⚠️" } else { "" };
                format!(
                    "{} (session {})  ~{} / {}{}",
                    s.agent,
                    s.session_short(),
                    used,
                    limit,
                    warn
                )
            }
            None => format!(
                "{} (session {})  n/a / {}",
                s.agent,
                s.session_short(),
                limit
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

    #[test]
    fn session_short_takes_first_eight_chars() {
        let s = SessionContext {
            agent: "kira".into(),
            model: "claude-opus-4".into(),
            session_id: "abcdef0123456789".into(),
            context_tokens: Some(100),
            context_limit: 200_000,
        };
        assert_eq!(s.session_short(), "abcdef01");
    }

    #[test]
    fn session_short_handles_short_or_blank_ids() {
        let mut s = SessionContext {
            agent: "kira".into(),
            model: "claude-opus-4".into(),
            session_id: "abc".into(),
            context_tokens: None,
            context_limit: 200_000,
        };
        assert_eq!(s.session_short(), "abc");
        s.session_id = "   ".into();
        assert_eq!(s.session_short(), "(no session)");
    }

    #[test]
    fn warning_triggers_above_eighty_percent() {
        let mut s = SessionContext {
            agent: "a".into(),
            model: "claude-opus-4".into(),
            session_id: "xxxxxxxx".into(),
            context_tokens: Some(160_000),
            context_limit: 200_000,
        };
        assert!(s.is_warning());
        s.context_tokens = Some(159_999);
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
        let snap = vec![
            SessionContext {
                agent: "agent-a".into(),
                model: "claude-opus-4".into(),
                session_id: "abc12345xx".into(),
                context_tokens: Some(45_000),
                context_limit: 200_000,
            },
            SessionContext {
                agent: "agent-a".into(),
                model: "claude-opus-4".into(),
                session_id: "def45678yy".into(),
                context_tokens: Some(180_000),
                context_limit: 200_000,
            },
            SessionContext {
                agent: "agent-b".into(),
                model: "claude-sonnet".into(),
                session_id: "ghi78901zz".into(),
                context_tokens: None,
                context_limit: 200_000,
            },
        ];
        let reply = format_reply(&snap);
        assert!(reply.starts_with("/context\n"));
        assert!(reply.contains("agent-a (session abc12345)  ~45k / 200k"));
        assert!(reply.contains("agent-a (session def45678)  ~180k / 200k  ⚠️"));
        assert!(reply.contains("agent-b (session ghi78901)  n/a / 200k"));
    }

    #[test]
    fn context_window_defaults_to_two_hundred_k() {
        assert_eq!(context_window_for_model("claude-opus-4"), 200_000);
        assert_eq!(context_window_for_model("claude-sonnet-4-6"), 200_000);
        assert_eq!(context_window_for_model("anything-else"), 200_000);
    }
}
