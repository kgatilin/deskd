//! `deskd usage` — aggregate token usage and cost across all agents.

use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;

use crate::app::tasklog;

/// Per-agent aggregated stats.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentStats {
    pub agent: String,
    pub tasks: usize,
    pub cost_usd: f64,
    pub turns: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub duration_ms: u64,
    /// Number of Claude invocations.
    pub sessions: u32,
    /// Number of tool_use blocks emitted across all tasks.
    pub tool_calls: u32,
    /// Average input+output tokens per task.
    pub avg_tokens_per_task: u64,
    /// Average cost per task (USD).
    pub avg_cost_per_task: f64,
}

/// Aggregate stats across all agents.
#[derive(Debug, Clone, serde::Serialize)]
pub struct UsageStats {
    pub period: String,
    pub total_tasks: usize,
    pub total_cost_usd: f64,
    pub total_turns: u32,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cache_creation_input_tokens: u64,
    pub total_cache_read_input_tokens: u64,
    /// Cache hit rate (0.0–1.0): cache_read / (cache_read + cache_creation + non-cache input).
    pub cache_hit_rate: f64,
    pub total_duration_ms: u64,
    /// Total Claude invocations across all tasks.
    pub total_sessions: u32,
    /// Total tool_use blocks across all tasks.
    pub total_tool_calls: u32,
    pub by_agent: Vec<AgentStats>,
}

/// Parse a period string into a `since` cutoff timestamp.
fn parse_period(period: &str) -> Result<Option<DateTime<Utc>>> {
    let now = Utc::now();
    match period {
        "all" => Ok(None),
        "today" => {
            let start_of_day = now.date_naive().and_hms_opt(0, 0, 0).unwrap().and_utc();
            Ok(Some(start_of_day))
        }
        s if s.ends_with('d') => {
            let days: i64 = s.trim_end_matches('d').parse().map_err(|_| {
                anyhow::anyhow!("invalid period '{}' — expected e.g. 7d, 30d, today, all", s)
            })?;
            Ok(Some(now - Duration::days(days)))
        }
        s if s.ends_with('h') => {
            let hours: i64 = s.trim_end_matches('h').parse().map_err(|_| {
                anyhow::anyhow!("invalid period '{}' — expected e.g. 24h, 7d, today, all", s)
            })?;
            Ok(Some(now - Duration::hours(hours)))
        }
        _ => bail!(
            "unknown period '{}' — use today, 24h, 7d, 30d, or all",
            period
        ),
    }
}

/// Discover all agent names by scanning the tasklog directory.
fn discover_agents() -> Vec<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let logs_dir = std::path::PathBuf::from(home).join(".deskd").join("logs");

    let mut agents = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&logs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir()
                && let Some(name) = path.file_name().and_then(|n| n.to_str())
                && path.join("tasks.jsonl").exists()
            {
                agents.push(name.to_string());
            }
        }
    }
    agents.sort();
    agents
}

/// Compute aggregate usage stats.
///
/// Sub-agent attribution: when a tasklog entry has `parent_agent` set, its
/// usage is added BOTH to the sub-agent's own bucket and to the parent's
/// "(via sub-agents)" attribution bucket. The parent's totals therefore
/// reflect the full cost incurred under its delegation tree.
///
/// Top-level totals (`total_*`) are summed from raw entries only — they are
/// NOT double-counted across sub-agent / parent buckets.
pub fn compute_stats(period: &str, agent_filter: Option<&str>) -> Result<UsageStats> {
    let since = parse_period(period)?;

    // Always scan all agents — even when `agent_filter` is set — because
    // sub-agent entries contribute to their parent's bucket. The display
    // buckets are post-filtered below using `agent_filter`.
    let scan_agents = discover_agents();

    let mut by_agent: HashMap<String, AgentStats> = HashMap::new();

    // Top-level totals (sum once per entry, no double-counting).
    let mut total_tasks: usize = 0;
    let mut total_cost_usd: f64 = 0.0;
    let mut total_turns: u32 = 0;
    let mut total_input_tokens: u64 = 0;
    let mut total_output_tokens: u64 = 0;
    let mut total_cache_creation: u64 = 0;
    let mut total_cache_read: u64 = 0;
    let mut total_duration_ms: u64 = 0;
    let mut total_sessions: u32 = 0;
    let mut total_tool_calls: u32 = 0;

    for agent_name in &scan_agents {
        let entries = tasklog::read_logs(agent_name, usize::MAX, None, since)?;
        if entries.is_empty() {
            continue;
        }

        for e in &entries {
            // Update top-level totals once per raw entry.
            total_tasks += 1;
            total_cost_usd += e.cost;
            total_turns += e.turns;
            total_input_tokens += e.input_tokens.unwrap_or(0);
            total_output_tokens += e.output_tokens.unwrap_or(0);
            total_cache_creation += e.cache_creation_input_tokens.unwrap_or(0);
            total_cache_read += e.cache_read_input_tokens.unwrap_or(0);
            total_duration_ms += e.duration_ms;
            total_sessions += e.session_count.unwrap_or(0);
            total_tool_calls += e.tool_use_count.unwrap_or(0);

            // Attribute to the agent that ran the task.
            attribute_entry(&mut by_agent, agent_name, e);

            // Also attribute to the parent agent (if any).
            if let Some(parent) = e.parent_agent.as_deref()
                && parent != agent_name
            {
                attribute_entry(&mut by_agent, parent, e);
            }
        }
    }

    // Compute averages.
    for stats in by_agent.values_mut() {
        if stats.tasks > 0 {
            stats.avg_tokens_per_task =
                (stats.input_tokens + stats.output_tokens) / stats.tasks as u64;
            stats.avg_cost_per_task = stats.cost_usd / stats.tasks as f64;
        }
    }

    // Apply post-filter on displayed buckets.
    let mut agent_list: Vec<AgentStats> = if let Some(filter) = agent_filter {
        by_agent
            .into_values()
            .filter(|a| a.agent == filter)
            .collect()
    } else {
        by_agent.into_values().collect()
    };

    // Sort by cost descending.
    agent_list.sort_by(|a, b| {
        b.cost_usd
            .partial_cmp(&a.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Cache hit rate: proportion of input tokens served from cache.
    let total_all_input = total_input_tokens + total_cache_creation + total_cache_read;
    let cache_hit_rate = if total_all_input > 0 {
        total_cache_read as f64 / total_all_input as f64
    } else {
        0.0
    };

    Ok(UsageStats {
        period: period.to_string(),
        total_tasks,
        total_cost_usd,
        total_turns,
        total_input_tokens,
        total_output_tokens,
        total_cache_creation_input_tokens: total_cache_creation,
        total_cache_read_input_tokens: total_cache_read,
        cache_hit_rate,
        total_duration_ms,
        total_sessions,
        total_tool_calls,
        by_agent: agent_list,
    })
}

/// Add a tasklog entry's usage into the named agent's bucket.
fn attribute_entry(
    by_agent: &mut HashMap<String, AgentStats>,
    agent_name: &str,
    e: &tasklog::TaskLog,
) {
    let stats = by_agent
        .entry(agent_name.to_string())
        .or_insert_with(|| AgentStats {
            agent: agent_name.to_string(),
            tasks: 0,
            cost_usd: 0.0,
            turns: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            duration_ms: 0,
            sessions: 0,
            tool_calls: 0,
            avg_tokens_per_task: 0,
            avg_cost_per_task: 0.0,
        });

    stats.tasks += 1;
    stats.cost_usd += e.cost;
    stats.turns += e.turns;
    stats.input_tokens += e.input_tokens.unwrap_or(0);
    stats.output_tokens += e.output_tokens.unwrap_or(0);
    stats.cache_creation_input_tokens += e.cache_creation_input_tokens.unwrap_or(0);
    stats.cache_read_input_tokens += e.cache_read_input_tokens.unwrap_or(0);
    stats.duration_ms += e.duration_ms;
    stats.sessions += e.session_count.unwrap_or(0);
    stats.tool_calls += e.tool_use_count.unwrap_or(0);
}

/// Format token count as human-readable.
fn format_tokens(n: u64) -> String {
    if n == 0 {
        "-".to_string()
    } else if n < 1_000 {
        format!("{}", n)
    } else if n < 1_000_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
}

/// Print usage stats as a table.
pub fn print_table(stats: &UsageStats) {
    println!("Token Usage ({})", stats.period);
    println!("{}", "─".repeat(95));
    println!(
        "{:<20} {:>5}  {:>4}  {:>5}  {:>7}  {:>8}  {:>8}  {:>9}",
        "Agent", "Tasks", "Sess", "Tools", "Cost", "Input", "Output", "Duration"
    );
    println!("{}", "─".repeat(95));

    for a in &stats.by_agent {
        println!(
            "{:<20} {:>5}  {:>4}  {:>5}  ${:>6.2}  {:>8}  {:>8}  {:>9}",
            a.agent,
            a.tasks,
            a.sessions,
            a.tool_calls,
            a.cost_usd,
            format_tokens(a.input_tokens),
            format_tokens(a.output_tokens),
            tasklog::format_duration(a.duration_ms),
        );
    }

    println!("{}", "─".repeat(95));
    println!(
        "{:<20} {:>5}  {:>4}  {:>5}  ${:>6.2}  {:>8}  {:>8}  {:>9}",
        "TOTAL",
        stats.total_tasks,
        stats.total_sessions,
        stats.total_tool_calls,
        stats.total_cost_usd,
        format_tokens(stats.total_input_tokens),
        format_tokens(stats.total_output_tokens),
        tasklog::format_duration(stats.total_duration_ms),
    );

    if stats.total_tasks > 0 {
        println!();
        println!(
            "Avg per task: {} input, {} output, ${:.2}",
            format_tokens(stats.total_input_tokens / stats.total_tasks as u64),
            format_tokens(stats.total_output_tokens / stats.total_tasks as u64),
            stats.total_cost_usd / stats.total_tasks as f64,
        );
        println!("Cache hit rate: {:.1}%", stats.cache_hit_rate * 100.0);
    }
}

/// Print usage stats as JSON.
pub fn print_json(stats: &UsageStats) {
    if let Ok(json) = serde_json::to_string_pretty(stats) {
        println!("{}", json);
    }
}

/// Entry point for `deskd usage`.
pub async fn run(period: &str, agent: Option<&str>, format: &str) -> Result<()> {
    let stats = compute_stats(period, agent)?;

    match format {
        "json" => print_json(&stats),
        _ => print_table(&stats),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_period_7d() {
        let since = parse_period("7d").unwrap();
        assert!(since.is_some());
        let cutoff = since.unwrap();
        let now = Utc::now();
        let diff = now - cutoff;
        // Should be approximately 7 days (within a second tolerance).
        assert!((diff.num_days() - 7).abs() <= 1);
    }

    #[test]
    fn test_parse_period_today() {
        let since = parse_period("today").unwrap();
        assert!(since.is_some());
        let cutoff = since.unwrap();
        assert_eq!(cutoff.date_naive(), Utc::now().date_naive());
    }

    #[test]
    fn test_parse_period_24h() {
        let since = parse_period("24h").unwrap();
        assert!(since.is_some());
    }

    #[test]
    fn test_parse_period_all() {
        let since = parse_period("all").unwrap();
        assert!(since.is_none());
    }

    #[test]
    fn test_parse_period_invalid() {
        assert!(parse_period("xyz").is_err());
    }

    #[test]
    fn test_format_tokens() {
        assert_eq!(format_tokens(0), "-");
        assert_eq!(format_tokens(500), "500");
        assert_eq!(format_tokens(1500), "1.5K");
        assert_eq!(format_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn test_compute_stats_empty() {
        // Use a non-existent agent to get empty results.
        let stats = compute_stats("7d", Some("nonexistent-agent-xyz")).unwrap();
        assert_eq!(stats.total_tasks, 0);
        assert_eq!(stats.total_cost_usd, 0.0);
        assert!(stats.by_agent.is_empty());
    }

    #[test]
    fn test_usage_stats_serializable() {
        let stats = UsageStats {
            period: "7d".to_string(),
            total_tasks: 10,
            total_cost_usd: 5.50,
            total_turns: 42,
            total_input_tokens: 100_000,
            total_output_tokens: 25_000,
            total_cache_creation_input_tokens: 5_000,
            total_cache_read_input_tokens: 80_000,
            cache_hit_rate: 80_000.0 / (100_000.0 + 5_000.0 + 80_000.0),
            total_duration_ms: 300_000,
            total_sessions: 10,
            total_tool_calls: 25,
            by_agent: vec![AgentStats {
                agent: "test".to_string(),
                tasks: 10,
                cost_usd: 5.50,
                turns: 42,
                input_tokens: 100_000,
                output_tokens: 25_000,
                cache_creation_input_tokens: 5_000,
                cache_read_input_tokens: 80_000,
                duration_ms: 300_000,
                sessions: 10,
                tool_calls: 25,
                avg_tokens_per_task: 12_500,
                avg_cost_per_task: 0.55,
            }],
        };
        let json = serde_json::to_string(&stats).unwrap();
        assert!(json.contains("\"total_tasks\":10"));
        assert!(json.contains("\"total_cost_usd\":5.5"));
        assert!(json.contains("\"total_cache_creation_input_tokens\":5000"));
        assert!(json.contains("\"total_cache_read_input_tokens\":80000"));
        assert!(json.contains("\"cache_hit_rate\":"));
        assert!(json.contains("\"total_sessions\":10"));
        assert!(json.contains("\"total_tool_calls\":25"));
        assert!(json.contains("\"sessions\":10"));
        assert!(json.contains("\"tool_calls\":25"));
        assert!(json.contains("\"avg_tokens_per_task\":12500"));
    }

    /// Verify aggregation uses session_count, tool_use_count, and parent_agent
    /// from raw entries, attributing usage to both the sub-agent and its parent.
    #[test]
    fn test_subagent_attribution_logic() {
        use super::tasklog::TaskLog;

        let mut by_agent: HashMap<String, AgentStats> = HashMap::new();
        let entry = TaskLog {
            ts: "2026-04-24T10:00:00Z".to_string(),
            source: "telegram".to_string(),
            turns: 3,
            cost: 0.10,
            duration_ms: 5_000,
            status: "ok".to_string(),
            task: "test".to_string(),
            error: None,
            msg_id: "m1".to_string(),
            github_repo: None,
            github_pr: None,
            input_tokens: Some(1_000),
            output_tokens: Some(200),
            cache_creation_input_tokens: Some(50),
            cache_read_input_tokens: Some(800),
            session_count: Some(1),
            tool_use_count: Some(4),
            parent_agent: Some("kira".to_string()),
        };

        // Attribute to sub-agent and parent (matches compute_stats inner loop).
        attribute_entry(&mut by_agent, "kira-helper", &entry);
        attribute_entry(&mut by_agent, "kira", &entry);

        let helper = by_agent.get("kira-helper").unwrap();
        let parent = by_agent.get("kira").unwrap();

        assert_eq!(helper.tasks, 1);
        assert_eq!(parent.tasks, 1);
        assert_eq!(helper.cost_usd, 0.10);
        assert_eq!(parent.cost_usd, 0.10);
        assert_eq!(helper.tool_calls, 4);
        assert_eq!(parent.tool_calls, 4);
        assert_eq!(helper.sessions, 1);
        assert_eq!(parent.sessions, 1);
        assert_eq!(helper.input_tokens, 1_000);
        assert_eq!(parent.input_tokens, 1_000);
    }

    #[test]
    fn test_compute_stats_attributes_subagent_to_parent() {
        // Set up isolated HOME with two agents: parent "p1" and sub-agent "p1-sub".
        let tmp = std::path::PathBuf::from(format!(
            "/tmp/deskd-test-usage-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let logs = tmp.join(".deskd").join("logs");
        std::fs::create_dir_all(logs.join("p1")).unwrap();
        std::fs::create_dir_all(logs.join("p1-sub")).unwrap();

        // Helper to write one entry.
        let write_entry =
            |dir: &str, parent: Option<&str>, cost: f64, in_tok: u64, out_tok: u64, tool: u32| {
                let entry = tasklog::TaskLog {
                    ts: chrono::Utc::now().to_rfc3339(),
                    source: "test".to_string(),
                    turns: 1,
                    cost,
                    duration_ms: 1_000,
                    status: "ok".to_string(),
                    task: "t".to_string(),
                    error: None,
                    msg_id: "m".to_string(),
                    github_repo: None,
                    github_pr: None,
                    input_tokens: Some(in_tok),
                    output_tokens: Some(out_tok),
                    cache_creation_input_tokens: None,
                    cache_read_input_tokens: None,
                    session_count: Some(1),
                    tool_use_count: Some(tool),
                    parent_agent: parent.map(str::to_string),
                };
                let line = serde_json::to_string(&entry).unwrap();
                let path = logs.join(dir).join("tasks.jsonl");
                let mut f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&path)
                    .unwrap();
                use std::io::Write;
                writeln!(f, "{}", line).unwrap();
            };

        write_entry("p1", None, 1.00, 1_000, 100, 2);
        write_entry("p1-sub", Some("p1"), 0.50, 500, 50, 3);

        // Override HOME for this test.
        // SAFETY: tests run sequentially; restore HOME on exit.
        // (single-threaded test runner not guaranteed, so we do best-effort cleanup).
        let prev_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", &tmp);
        }

        let stats = compute_stats("all", None).unwrap();

        // Restore HOME first to keep test environment hygienic.
        unsafe {
            if let Some(h) = prev_home {
                std::env::set_var("HOME", h);
            } else {
                std::env::remove_var("HOME");
            }
        }

        // Top-level totals: only the two raw entries.
        assert_eq!(stats.total_tasks, 2, "raw entries summed once");
        assert!((stats.total_cost_usd - 1.50).abs() < 1e-9);
        assert_eq!(stats.total_tool_calls, 5);
        assert_eq!(stats.total_sessions, 2);

        // Sub-agent bucket: 1 task, 0.50 cost.
        let sub = stats
            .by_agent
            .iter()
            .find(|a| a.agent == "p1-sub")
            .expect("sub-agent bucket present");
        assert_eq!(sub.tasks, 1);
        assert!((sub.cost_usd - 0.50).abs() < 1e-9);
        assert_eq!(sub.tool_calls, 3);

        // Parent bucket: 2 tasks (own + sub-agent's), 1.50 cost.
        let parent = stats
            .by_agent
            .iter()
            .find(|a| a.agent == "p1")
            .expect("parent bucket present");
        assert_eq!(parent.tasks, 2);
        assert!((parent.cost_usd - 1.50).abs() < 1e-9);
        assert_eq!(parent.tool_calls, 5);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_tasklog_backward_compat() {
        // Old-style JSONL line without session_count, tool_use_count, parent_agent
        // must still parse via serde(default).
        let old_line = r#"{"ts":"2026-04-01T00:00:00Z","source":"telegram","turns":3,"cost":0.42,"duration_ms":1000,"status":"ok","task":"old task","error":null,"msg_id":"m1"}"#;
        let entry: tasklog::TaskLog = serde_json::from_str(old_line).unwrap();
        assert_eq!(entry.turns, 3);
        assert_eq!(entry.session_count, None);
        assert_eq!(entry.tool_use_count, None);
        assert_eq!(entry.parent_agent, None);
    }
}
