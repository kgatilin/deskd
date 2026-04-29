//! Agent health diagnosis — pure heuristics over agent state + recent task logs.
//!
//! Surfaces a single `Verdict` per agent, derived from the same on-disk
//! signals that an operator would normally check by hand:
//!
//! - the agent state file (PID, status, parent)
//! - whether `/proc/<pid>` exists (process alive)
//! - the most recent N entries of the agent's `tasks.jsonl` (tokens, duration)
//! - the timestamp of the latest message in the agent's input inbox vs.
//!   the timestamp of the latest task log entry (queue-vs-completion gap)
//!
//! All inputs are parameters — no I/O happens in this module's pure
//! functions, which keeps the heuristic engine trivially testable. The CLI
//! handler in `commands::doctor` is responsible for collecting the inputs.
//!
//! See deskd issue #422 for the full motivation and verdict matrix.
//!
//! # Forward compatibility
//!
//! Issue #424 will introduce a `consecutive_empty_completions` counter on
//! the agent state. Once that counter exists, callers can pass it via
//! `DoctorInputs::recent_empty_completions` for a faster path, but the
//! current implementation falls back to scanning the last N tasklog
//! records, so this module works on `main` today without #424.

use chrono::{DateTime, Utc};

use crate::app::tasklog::TaskLog;

/// Default thresholds chosen to match the real-world incident in #422
/// (an agent silent for ~21h with 5 consecutive empty completions).
pub const DEFAULT_EMPTY_COMPLETION_THRESHOLD: usize = 3;
pub const DEFAULT_IDLE_MINUTES_THRESHOLD: i64 = 60;
pub const DEFAULT_STUCK_QUEUE_MINUTES: i64 = 5;
/// Empty completion = a task whose duration was below this and produced
/// zero output tokens. Matches the symptom described in #422 ("instantly
/// with output_tokens=0, duration_secs=0").
pub const EMPTY_DURATION_MS_CEILING: u64 = 2_000;

/// Tunable thresholds for the diagnose engine. Wired from workspace config
/// or CLI flags, with sensible defaults.
#[derive(Debug, Clone, Copy)]
pub struct DoctorThresholds {
    /// How many *consecutive* empty completions trigger a `Hung` verdict.
    pub empty_completion_threshold: usize,
    /// After this many minutes with no activity, a healthy agent flips to
    /// `Idle`.
    pub idle_minutes_threshold: i64,
    /// A queued message older than this with no subsequent completion
    /// triggers a `Stuck` verdict.
    pub stuck_queue_minutes: i64,
}

impl Default for DoctorThresholds {
    fn default() -> Self {
        Self {
            empty_completion_threshold: DEFAULT_EMPTY_COMPLETION_THRESHOLD,
            idle_minutes_threshold: DEFAULT_IDLE_MINUTES_THRESHOLD,
            stuck_queue_minutes: DEFAULT_STUCK_QUEUE_MINUTES,
        }
    }
}

/// Inputs to the diagnose engine. Pure data — collected by the CLI handler
/// from on-disk state, then handed to `diagnose` for the verdict.
#[derive(Debug, Clone)]
pub struct DoctorInputs<'a> {
    pub agent_name: &'a str,
    /// PID from the agent state file. `0` means "no PID known".
    pub state_pid: u32,
    /// Whether `/proc/<pid>` exists right now.
    pub process_alive: bool,
    /// Recent task log entries, oldest-first (i.e. natural file order).
    pub recent_tasks: &'a [TaskLog],
    /// Latest message in the agent's input inbox (`agent/<name>` inbox).
    /// `None` = nothing queued.
    pub latest_inbox_ts: Option<DateTime<Utc>>,
    /// "Now" — the diagnose timestamp. Pinned for deterministic tests.
    pub now: DateTime<Utc>,
}

/// The diagnosis result for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Process alive, but the last N tasks all completed instantly with
    /// zero output tokens — symptom of a wedged Claude session.
    Hung {
        empty_count: usize,
        last_good_age_secs: Option<i64>,
    },
    /// A message has been queued for `queued_minutes` with no completion
    /// since.
    Stuck { queued_minutes: i64 },
    /// State file claims the agent is alive but the process is gone.
    Dead { pid: u32 },
    /// Last completion was healthy (cost > 0) but no recent activity.
    Idle { last_good_age_secs: i64 },
    /// Recent completion within thresholds had real tokens and cost.
    Healthy { last_good_age_secs: Option<i64> },
}

impl Verdict {
    /// Single-character glyph for compact tables.
    pub fn glyph(&self) -> &'static str {
        match self {
            Verdict::Hung { .. } => "🔴",
            Verdict::Stuck { .. } => "🔴",
            Verdict::Dead { .. } => "🔴",
            Verdict::Idle { .. } => "🟡",
            Verdict::Healthy { .. } => "🟢",
        }
    }

    /// Short label for the STATUS column of `agent list`.
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Hung { .. } => "hung",
            Verdict::Stuck { .. } => "stuck",
            Verdict::Dead { .. } => "dead",
            Verdict::Idle { .. } => "idle",
            Verdict::Healthy { .. } => "healthy",
        }
    }

    /// Whether this verdict indicates a problem the operator should act on.
    pub fn is_problem(&self) -> bool {
        matches!(
            self,
            Verdict::Hung { .. } | Verdict::Stuck { .. } | Verdict::Dead { .. }
        )
    }

    /// Recommended remediation command, if any.
    pub fn recommended_action(&self, agent_name: &str) -> Option<String> {
        match self {
            Verdict::Hung { .. } => Some(format!("deskd agent restart {agent_name}")),
            Verdict::Dead { .. } => Some(format!("deskd agent restart {agent_name}")),
            Verdict::Stuck { .. } => Some(format!(
                "deskd agent stderr {agent_name} && deskd agent restart {agent_name}"
            )),
            Verdict::Idle { .. } => None,
            Verdict::Healthy { .. } => None,
        }
    }

    /// Human-readable signal for the right-hand "SIGNAL" column.
    pub fn signal(&self, threshold: usize) -> String {
        match self {
            Verdict::Hung { empty_count, .. } => {
                format!("{empty_count}/{threshold} recent tasks 0 tokens, <2s duration")
            }
            Verdict::Stuck { queued_minutes } => {
                format!("queued {queued_minutes}m ago, no completion")
            }
            Verdict::Dead { pid } => format!("state says alive but pid {pid} not found"),
            Verdict::Idle { last_good_age_secs } => {
                format!(
                    "no recent traffic ({} since last good)",
                    fmt_secs(*last_good_age_secs)
                )
            }
            Verdict::Healthy { .. } => "—".to_string(),
        }
    }
}

/// Format an age in seconds as a compact human-readable string.
pub fn fmt_secs(secs: i64) -> String {
    if secs < 0 {
        return "now".into();
    }
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86400)
    }
}

/// Whether a single task log entry counts as an "empty completion" — the
/// signature of a wedged Claude session per #422.
pub fn is_empty_completion(t: &TaskLog) -> bool {
    let zero_output = t.output_tokens.unwrap_or(0) == 0;
    let fast = t.duration_ms < EMPTY_DURATION_MS_CEILING;
    zero_output && fast
}

/// Whether a task log entry is a "good" completion — real work happened.
pub fn is_good_completion(t: &TaskLog) -> bool {
    t.cost > 0.0 || t.output_tokens.unwrap_or(0) > 0
}

/// Count consecutive empty completions at the *tail* of the log
/// (most recent first). Returns 0 if the most recent entry is "good".
pub fn trailing_empty_count(tasks: &[TaskLog]) -> usize {
    tasks
        .iter()
        .rev()
        .take_while(|t| is_empty_completion(t))
        .count()
}

/// Find the most recent good completion's timestamp.
pub fn last_good_ts(tasks: &[TaskLog]) -> Option<DateTime<Utc>> {
    tasks
        .iter()
        .rev()
        .find(|t| is_good_completion(t))
        .and_then(|t| {
            DateTime::parse_from_rfc3339(&t.ts)
                .ok()
                .map(|d| d.with_timezone(&Utc))
        })
}

/// Run the heuristics. Pure, deterministic, no I/O.
///
/// Verdict precedence (most severe first):
///   1. `Dead`    — state says alive but no PID found
///   2. `Hung`    — process alive but last N tasks all empty completions
///   3. `Stuck`   — queue grew but no completion since
///   4. `Idle`    — last completion healthy, but no recent activity
///   5. `Healthy` — recent good completion (or no history at all)
pub fn diagnose(input: &DoctorInputs<'_>, thresholds: &DoctorThresholds) -> Verdict {
    let last_good = last_good_ts(input.recent_tasks);
    let last_good_age_secs = last_good.map(|ts| (input.now - ts).num_seconds());

    // 1. Dead — state file claims alive but process is gone.
    //    A "claims alive" state is signaled by a non-zero PID. (Status
    //    column is unreliable per #422; the PID field is the source of
    //    truth set when the worker started.)
    if input.state_pid > 0 && !input.process_alive {
        return Verdict::Dead {
            pid: input.state_pid,
        };
    }

    // 2. Hung — process alive AND last N tasks all empty completions.
    if input.process_alive {
        let empty = trailing_empty_count(input.recent_tasks);
        if empty >= thresholds.empty_completion_threshold {
            return Verdict::Hung {
                empty_count: empty,
                last_good_age_secs,
            };
        }
    }

    // 3. Stuck — a message arrived in the inbox more than M minutes ago
    //    AND no completion has been logged since that message arrived.
    if let Some(inbox_ts) = input.latest_inbox_ts {
        let age_minutes = (input.now - inbox_ts).num_minutes();
        if age_minutes >= thresholds.stuck_queue_minutes {
            // Did *any* task log entry land after the queued message?
            let completed_after = input
                .recent_tasks
                .iter()
                .filter_map(|t| DateTime::parse_from_rfc3339(&t.ts).ok())
                .any(|ts| ts.with_timezone(&Utc) >= inbox_ts);
            if !completed_after {
                return Verdict::Stuck {
                    queued_minutes: age_minutes,
                };
            }
        }
    }

    // 4. Idle — last good completion exists but is older than the idle
    //    threshold AND there's no obvious problem.
    if let Some(age_secs) = last_good_age_secs {
        let idle_secs = thresholds.idle_minutes_threshold * 60;
        if age_secs >= idle_secs {
            return Verdict::Idle {
                last_good_age_secs: age_secs,
            };
        }
    }

    // 5. Healthy — recent good completion or quiet but no problem signals.
    Verdict::Healthy { last_good_age_secs }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ts: &str, output_tokens: u64, duration_ms: u64, cost: f64) -> TaskLog {
        TaskLog {
            ts: ts.to_string(),
            source: "test".into(),
            turns: 1,
            cost,
            duration_ms,
            status: "ok".into(),
            task: "t".into(),
            error: None,
            msg_id: "m".into(),
            github_repo: None,
            github_pr: None,
            input_tokens: Some(100),
            output_tokens: Some(output_tokens),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: None,
            session_count: None,
            tool_use_count: None,
            parent_agent: None,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-04-28T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn empty_completion_detection() {
        // 0 output tokens AND <2s duration → empty
        assert!(is_empty_completion(&entry(
            "2026-04-28T11:00:00Z",
            0,
            500,
            0.0
        )));
        // Real output → not empty
        assert!(!is_empty_completion(&entry(
            "2026-04-28T11:00:00Z",
            500,
            500,
            0.10
        )));
        // Zero tokens but >2s — counts as an attempt, not empty
        assert!(!is_empty_completion(&entry(
            "2026-04-28T11:00:00Z",
            0,
            5_000,
            0.0
        )));
    }

    #[test]
    fn trailing_count_only_tail() {
        // [good, empty, empty, empty] → trailing 3
        let tasks = vec![
            entry("2026-04-28T08:00:00Z", 500, 5000, 0.10),
            entry("2026-04-28T09:00:00Z", 0, 500, 0.0),
            entry("2026-04-28T10:00:00Z", 0, 500, 0.0),
            entry("2026-04-28T11:00:00Z", 0, 500, 0.0),
        ];
        assert_eq!(trailing_empty_count(&tasks), 3);
    }

    #[test]
    fn trailing_count_resets_on_good() {
        let tasks = vec![
            entry("2026-04-28T08:00:00Z", 0, 500, 0.0),
            entry("2026-04-28T09:00:00Z", 0, 500, 0.0),
            entry("2026-04-28T10:00:00Z", 500, 5000, 0.10),
        ];
        assert_eq!(trailing_empty_count(&tasks), 0);
    }

    #[test]
    fn verdict_hung_when_pid_alive_and_n_empty() {
        let tasks = vec![
            entry("2026-04-28T08:00:00Z", 500, 5000, 0.10),
            entry("2026-04-28T09:00:00Z", 0, 500, 0.0),
            entry("2026-04-28T10:00:00Z", 0, 500, 0.0),
            entry("2026-04-28T11:00:00Z", 0, 500, 0.0),
        ];
        let input = DoctorInputs {
            agent_name: "life",
            state_pid: 4242,
            process_alive: true,
            recent_tasks: &tasks,
            latest_inbox_ts: None,
            now: now(),
        };
        let v = diagnose(&input, &DoctorThresholds::default());
        assert!(matches!(v, Verdict::Hung { empty_count: 3, .. }));
        assert_eq!(v.label(), "hung");
        assert_eq!(
            v.recommended_action("life").unwrap(),
            "deskd agent restart life"
        );
    }

    #[test]
    fn verdict_dead_when_pid_set_but_process_gone() {
        let input = DoctorInputs {
            agent_name: "life",
            state_pid: 9999,
            process_alive: false,
            recent_tasks: &[],
            latest_inbox_ts: None,
            now: now(),
        };
        let v = diagnose(&input, &DoctorThresholds::default());
        assert_eq!(v, Verdict::Dead { pid: 9999 });
        assert!(v.recommended_action("life").is_some());
    }

    #[test]
    fn verdict_stuck_when_inbox_grew_without_completion() {
        // Inbox message arrived 10m ago; no task log entry since.
        let inbox_ts = now() - chrono::Duration::minutes(10);
        let tasks = vec![entry("2026-04-28T10:00:00Z", 500, 5000, 0.10)];
        let input = DoctorInputs {
            agent_name: "papers",
            state_pid: 4242,
            // Process alive: must NOT trip Hung (only one tasklog entry, and
            // it's good) so Stuck wins.
            process_alive: true,
            recent_tasks: &tasks,
            latest_inbox_ts: Some(inbox_ts),
            now: now(),
        };
        let v = diagnose(&input, &DoctorThresholds::default());
        assert!(matches!(v, Verdict::Stuck { queued_minutes } if queued_minutes >= 10));
    }

    #[test]
    fn verdict_idle_when_last_good_old_but_no_problem() {
        // Last good completion 4 hours ago, no queued messages, process up.
        let tasks = vec![entry("2026-04-28T08:00:00Z", 500, 5000, 0.10)];
        let input = DoctorInputs {
            agent_name: "papers",
            state_pid: 4242,
            process_alive: true,
            recent_tasks: &tasks,
            latest_inbox_ts: None,
            now: now(),
        };
        let v = diagnose(&input, &DoctorThresholds::default());
        assert!(matches!(v, Verdict::Idle { .. }));
    }

    #[test]
    fn verdict_healthy_when_recent_good_completion() {
        // Last good completion 5 minutes ago.
        let recent_ts = (now() - chrono::Duration::minutes(5)).to_rfc3339();
        let tasks = vec![entry(&recent_ts, 500, 5000, 0.10)];
        let input = DoctorInputs {
            agent_name: "dev",
            state_pid: 4242,
            process_alive: true,
            recent_tasks: &tasks,
            latest_inbox_ts: None,
            now: now(),
        };
        let v = diagnose(&input, &DoctorThresholds::default());
        assert!(matches!(v, Verdict::Healthy { .. }));
        assert!(v.recommended_action("dev").is_none());
    }

    #[test]
    fn verdict_healthy_when_no_history_and_no_signals() {
        let input = DoctorInputs {
            agent_name: "fresh",
            state_pid: 0,
            process_alive: false,
            recent_tasks: &[],
            latest_inbox_ts: None,
            now: now(),
        };
        let v = diagnose(&input, &DoctorThresholds::default());
        // No PID and no signals — treat as Healthy (nothing wrong yet).
        assert!(matches!(v, Verdict::Healthy { .. }));
    }

    #[test]
    fn dead_takes_precedence_over_hung() {
        // Even with empty completions, a missing process is the bigger
        // problem to surface.
        let tasks = vec![
            entry("2026-04-28T09:00:00Z", 0, 500, 0.0),
            entry("2026-04-28T10:00:00Z", 0, 500, 0.0),
            entry("2026-04-28T11:00:00Z", 0, 500, 0.0),
        ];
        let input = DoctorInputs {
            agent_name: "ghost",
            state_pid: 4242,
            process_alive: false,
            recent_tasks: &tasks,
            latest_inbox_ts: None,
            now: now(),
        };
        let v = diagnose(&input, &DoctorThresholds::default());
        assert!(matches!(v, Verdict::Dead { .. }));
    }

    #[test]
    fn stuck_only_when_completion_didnt_land_after_queue() {
        // Inbox grew 10m ago, AND a completion landed since — not stuck.
        let inbox_ts = now() - chrono::Duration::minutes(10);
        let recent_completion = (now() - chrono::Duration::minutes(2)).to_rfc3339();
        let tasks = vec![entry(&recent_completion, 500, 5000, 0.10)];
        let input = DoctorInputs {
            agent_name: "dev",
            state_pid: 4242,
            process_alive: true,
            recent_tasks: &tasks,
            latest_inbox_ts: Some(inbox_ts),
            now: now(),
        };
        let v = diagnose(&input, &DoctorThresholds::default());
        assert!(matches!(v, Verdict::Healthy { .. }));
    }

    #[test]
    fn fmt_secs_buckets() {
        assert_eq!(fmt_secs(-1), "now");
        assert_eq!(fmt_secs(45), "45s");
        assert_eq!(fmt_secs(120), "2m");
        assert_eq!(fmt_secs(3 * 3600 + 120), "3h");
        assert_eq!(fmt_secs(2 * 86400), "2d");
    }

    #[test]
    fn glyph_and_label_match_problem_flag() {
        assert!(
            Verdict::Hung {
                empty_count: 3,
                last_good_age_secs: None
            }
            .is_problem()
        );
        assert!(Verdict::Dead { pid: 1 }.is_problem());
        assert!(Verdict::Stuck { queued_minutes: 6 }.is_problem());
        assert!(
            !Verdict::Idle {
                last_good_age_secs: 9000
            }
            .is_problem()
        );
        assert!(
            !Verdict::Healthy {
                last_good_age_secs: Some(60)
            }
            .is_problem()
        );
        assert_eq!(Verdict::Dead { pid: 1 }.glyph(), "🔴");
        assert_eq!(
            Verdict::Idle {
                last_good_age_secs: 9000
            }
            .glyph(),
            "🟡"
        );
    }
}
