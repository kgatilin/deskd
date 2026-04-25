//! `deskd context` — list live context-window usage across all active agent
//! sessions. CLI parity with the Telegram `/context` slash command (#393).

use anyhow::{Result, bail};
use serde_json::json;

use crate::app::context_size::{self, SessionContext};

pub async fn run(format: &str) -> Result<()> {
    let snapshot = context_size::gather().await?;
    match format {
        "table" => {
            print_table(&snapshot);
        }
        "json" => {
            print_json(&snapshot)?;
        }
        other => bail!("unknown --format '{}' (use 'table' or 'json')", other),
    }
    Ok(())
}

fn print_table(snapshot: &[SessionContext]) {
    if snapshot.is_empty() {
        println!("No active sessions.");
        return;
    }
    // SESSION column dropped (#406): session IDs add noise without aiding
    // humans. A trailing STATE column surfaces the stale-fallback case.
    println!(
        "{:<16} {:<28} {:<12} {:<10} {:<8} {:<8}",
        "AGENT", "MODEL", "TOKENS", "LIMIT", "%", "STATE"
    );
    println!("{}", "─".repeat(86));
    for s in snapshot {
        let tokens = match s.context_tokens {
            Some(t) if s.stale => format!("{} (stale)", t),
            Some(t) => t.to_string(),
            None => "n/a".into(),
        };
        let pct = match s.context_tokens {
            Some(_) => format!("{:>5.1}%", s.utilization() * 100.0),
            None => "    -".to_string(),
        };
        let state = if s.is_warning() { "⚠️" } else { "" };
        println!(
            "{:<16} {:<28} {:<12} {:<10} {:<8} {:<8}",
            s.agent, s.model, tokens, s.context_limit, pct, state,
        );
    }
}

fn print_json(snapshot: &[SessionContext]) -> Result<()> {
    let arr: Vec<serde_json::Value> = snapshot
        .iter()
        .map(|s| {
            json!({
                "agent": s.agent,
                "model": s.model,
                "session_id": s.session_id,
                "context_tokens": s.context_tokens,
                "context_limit": s.context_limit,
                "utilization": s.utilization(),
                "warning": s.is_warning(),
                "stale": s.stale,
            })
        })
        .collect();
    println!("{}", serde_json::to_string_pretty(&arr)?);
    Ok(())
}
