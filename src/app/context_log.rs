//! Per-turn context measurements (#403, AC #2).
//!
//! After each task completion the worker appends a [`ContextLog`] record to
//! `~/.deskd/logs/<agent>/context.jsonl`. Each entry captures:
//!
//! * `tokens` — pinned context size for the turn
//!   (`input_tokens + cache_creation + cache_read`),
//! * `threshold` — the resolved auto-compact threshold for the agent,
//! * `context_limit` — the model's context window,
//! * `model` + `session_id` — for cross-session correlation.
//!
//! This is the data layer that future compaction strategies (drop-tool-results,
//! fork-with-synopsis) will consume to decide when to trigger. For now the
//! file is purely observational — no consumer reads it yet — but writing it
//! is the first acceptance criterion of #403 and unblocks `/context` history,
//! dashboard timeseries, and post-hoc analysis.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Cap on retained entries. When exceeded, the file is truncated to the last
/// `MAX_ENTRIES` lines on the next append (matches `tasklog` rotation).
const MAX_ENTRIES: usize = 10_000;

/// One per-turn context measurement.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextLog {
    /// RFC 3339 timestamp at which the turn completed.
    pub ts: String,
    /// Agent name (matches `AgentConfig.name`).
    pub agent: String,
    /// Claude session id at the time of measurement.
    pub session_id: String,
    /// Model id at the time of measurement.
    pub model: String,
    /// Cumulative tokens pinned in the session window:
    /// `input_tokens + cache_creation_input_tokens + cache_read_input_tokens`.
    pub tokens: u64,
    /// Resolved auto-compact threshold for this agent in tokens.
    pub threshold: u64,
    /// Model's full context window in tokens (e.g. 1_000_000 for Claude 4.x).
    pub context_limit: u64,
}

/// `~/.deskd/logs/<agent>/context.jsonl`.
pub fn log_path(agent_name: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let dir = PathBuf::from(home)
        .join(".deskd")
        .join("logs")
        .join(agent_name);
    std::fs::create_dir_all(&dir).ok();
    dir.join("context.jsonl")
}

/// Append a [`ContextLog`] entry to the agent's context log file.
pub fn append(entry: &ContextLog) -> Result<()> {
    append_to_path(&log_path(&entry.agent), entry)
}

/// Like [`append`] but writes to an explicit path. Used by tests.
pub fn append_to_path(path: &Path, entry: &ContextLog) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut line = serde_json::to_string(entry).context("serialize context log entry")?;
    line.push('\n');

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open context log: {}", path.display()))?;
    file.write_all(line.as_bytes())
        .with_context(|| format!("write context log: {}", path.display()))?;

    rotate_if_needed(path)?;
    Ok(())
}

fn rotate_if_needed(path: &Path) -> Result<()> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Ok(()),
    };
    let reader = std::io::BufReader::new(file);
    let lines: Vec<String> = reader.lines().collect::<std::io::Result<Vec<_>>>()?;

    if lines.len() > MAX_ENTRIES {
        let keep = &lines[lines.len() - MAX_ENTRIES..];
        let mut file = std::fs::File::create(path)?;
        for line in keep {
            writeln!(file, "{}", line)?;
        }
    }
    Ok(())
}

/// Read all entries from an agent's context log file. Empty / missing files
/// return `Ok(vec![])`. Malformed lines are skipped (matches `tasklog`).
pub fn read_logs(agent_name: &str) -> Result<Vec<ContextLog>> {
    read_logs_from_path(&log_path(agent_name))
}

/// Like [`read_logs`] but from an explicit path.
pub fn read_logs_from_path(path: &Path) -> Result<Vec<ContextLog>> {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let reader = std::io::BufReader::new(file);
    let mut out = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<ContextLog>(&line) {
            out.push(entry);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(ts: &str, tokens: u64) -> ContextLog {
        ContextLog {
            ts: ts.into(),
            agent: "kira".into(),
            session_id: "sess-abc".into(),
            model: "claude-sonnet-4-6".into(),
            tokens,
            threshold: 300_000,
            context_limit: 1_000_000,
        }
    }

    #[test]
    fn append_and_read_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.jsonl");
        append_to_path(&path, &sample("2026-04-30T00:00:00Z", 1234)).unwrap();
        append_to_path(&path, &sample("2026-04-30T00:01:00Z", 5678)).unwrap();

        let entries = read_logs_from_path(&path).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].tokens, 1234);
        assert_eq!(entries[1].tokens, 5678);
        assert_eq!(entries[0].threshold, 300_000);
        assert_eq!(entries[1].context_limit, 1_000_000);
    }

    #[test]
    fn append_creates_parent_dir() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("nested").join("deeper");
        let path = nested.join("context.jsonl");
        append_to_path(&path, &sample("2026-04-30T00:00:00Z", 42)).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn rotate_keeps_last_max_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.jsonl");

        // Write MAX_ENTRIES + 50 entries.
        for i in 0..(MAX_ENTRIES + 50) {
            let ts = format!("2026-04-30T00:00:{:02}Z", i % 60);
            append_to_path(&path, &sample(&ts, i as u64)).unwrap();
        }

        let entries = read_logs_from_path(&path).unwrap();
        assert_eq!(
            entries.len(),
            MAX_ENTRIES,
            "rotation should cap file at MAX_ENTRIES"
        );
        // The oldest 50 entries (tokens 0..49) should have been dropped.
        assert_eq!(entries[0].tokens, 50);
        assert_eq!(entries.last().unwrap().tokens, (MAX_ENTRIES + 49) as u64);
    }

    #[test]
    fn read_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.jsonl");
        let entries = read_logs_from_path(&path).unwrap();
        assert!(entries.is_empty());
    }

    #[test]
    fn read_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.jsonl");
        append_to_path(&path, &sample("2026-04-30T00:00:00Z", 1)).unwrap();
        // Manually inject a garbage line.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .unwrap();
            writeln!(f, "this is not json").unwrap();
        }
        append_to_path(&path, &sample("2026-04-30T00:00:01Z", 2)).unwrap();

        let entries = read_logs_from_path(&path).unwrap();
        assert_eq!(entries.len(), 2, "malformed line skipped, valid kept");
        assert_eq!(entries[0].tokens, 1);
        assert_eq!(entries[1].tokens, 2);
    }
}
