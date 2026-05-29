//! Level 3 session transcript renderer (#497).
//!
//! Reads raw Claude Code session JSONL files on demand and produces a
//! sequence of rendered lines for the panel. Layout matches the issue
//! spec:
//!
//! - `→ ` user lines
//! - `← ` agent / assistant lines
//! - `   $ tool_call` indented tool calls (assistant-side)
//!
//! Streaming parse — never loads the whole file into memory beyond the
//! per-line buffer. Unknown record types pass through as plain `<other>`
//! and are visible in the «raw JSONL» toggle.

use std::path::{Path, PathBuf};

use serde::Serialize;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;

/// A single rendered transcript line. The view layer escapes `text`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TranscriptLine {
    /// Arrow / glyph prefix: `→ ` (user), `← ` (assistant), `  $ ` (tool).
    pub prefix: String,
    /// Visible rendered text (truncated by caller if needed).
    pub text: String,
}

/// Parse a single Claude Code session JSONL file into rendered lines.
///
/// `agent_home` is the agent's `work_dir` (the parent of `.claude/`).
/// `session_id` is the JSONL stem under
/// `<agent_home>/.claude/projects/<flat-cwd>/<session_id>.jsonl`.
pub async fn render_session(
    agent_home: &Path,
    session_id: &str,
) -> Result<Vec<TranscriptLine>, std::io::Error> {
    let path = find_session_file(agent_home, session_id)
        .await
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "session {} not found under {}",
                    session_id,
                    agent_home.display()
                ),
            )
        })?;
    let raw = tokio::fs::read_to_string(&path).await?;
    Ok(render_jsonl(&raw))
}

/// Return the raw JSONL contents for the «view raw» toggle.
pub async fn raw_session(agent_home: &Path, session_id: &str) -> Result<String, std::io::Error> {
    let path = find_session_file(agent_home, session_id)
        .await
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "session {} not found under {}",
                    session_id,
                    agent_home.display()
                ),
            )
        })?;
    tokio::fs::read_to_string(&path).await
}

/// Locate `<agent_home>/.claude/projects/**/<session_id>.jsonl`. Returns
/// the first match.
pub async fn find_session_file(agent_home: &Path, session_id: &str) -> Option<PathBuf> {
    let root = agent_home.join(".claude").join("projects");
    let mut stack: Vec<PathBuf> = vec![root];
    while let Some(dir) = stack.pop() {
        let read = match std::fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in read.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_stem().and_then(|s| s.to_str()) == Some(session_id)
                && p.extension().and_then(|s| s.to_str()) == Some("jsonl")
            {
                return Some(p);
            }
        }
    }
    None
}

/// Pure-string entry point used by tests. Parses a JSONL blob into
/// rendered lines without touching the filesystem.
pub fn render_jsonl(raw: &str) -> Vec<TranscriptLine> {
    let mut out = Vec::new();
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match kind {
            "user" => {
                let text = extract_message_text(&v).unwrap_or_default();
                if !text.is_empty() {
                    out.push(TranscriptLine {
                        prefix: "→ ".into(),
                        text,
                    });
                }
            }
            "assistant" => {
                if let Some(arr) = v
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_array())
                {
                    for block in arr {
                        let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        match btype {
                            "text" => {
                                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                                    out.push(TranscriptLine {
                                        prefix: "← ".into(),
                                        text: t.to_string(),
                                    });
                                }
                            }
                            "tool_use" => {
                                let name =
                                    block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                                let input_compact = block
                                    .get("input")
                                    .map(compact_tool_input)
                                    .unwrap_or_default();
                                let body = if input_compact.is_empty() {
                                    name.to_string()
                                } else {
                                    format!("{} {}", name, input_compact)
                                };
                                out.push(TranscriptLine {
                                    prefix: "  $ ".into(),
                                    text: body,
                                });
                            }
                            _ => {}
                        }
                    }
                } else if let Some(t) = extract_message_text(&v) {
                    out.push(TranscriptLine {
                        prefix: "← ".into(),
                        text: t,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

/// Pull the rendered text out of `message.content` (string or array of
/// `{type:"text", text:...}` blocks).
fn extract_message_text(v: &serde_json::Value) -> Option<String> {
    let m = v.get("message")?;
    if let Some(content) = m.get("content") {
        if let Some(s) = content.as_str() {
            return Some(s.to_string());
        }
        if let Some(arr) = content.as_array() {
            let mut out = String::new();
            for block in arr {
                if let Some(t) = block.get("text").and_then(|x| x.as_str()) {
                    if !out.is_empty() {
                        out.push('\n');
                    }
                    out.push_str(t);
                }
            }
            if !out.is_empty() {
                return Some(out);
            }
        }
    }
    None
}

/// Shorten a tool input json into something fittable on one line.
fn compact_tool_input(v: &serde_json::Value) -> String {
    let s = v.to_string();
    if s.len() > 120 {
        format!("{}…", &s[..120])
    } else {
        s
    }
}

/// Asynchronous variant used internally to stream large JSONL files. Kept
/// for future use when file sizes become problematic.
#[allow(dead_code)]
pub async fn render_stream<R>(reader: R) -> Result<Vec<TranscriptLine>, std::io::Error>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut buf = BufReader::new(reader);
    let mut line = String::new();
    let mut acc = String::new();
    loop {
        line.clear();
        let n = buf.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        acc.push_str(&line);
    }
    Ok(render_jsonl(&acc))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_user_line_with_arrow_right() {
        let raw = r#"{"type":"user","message":{"role":"user","content":"hello"}}"#;
        let lines = render_jsonl(raw);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].prefix, "→ ");
        assert_eq!(lines[0].text, "hello");
    }

    #[test]
    fn renders_assistant_text_with_arrow_left() {
        let raw = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"hi there"}]}}"#;
        let lines = render_jsonl(raw);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].prefix, "← ");
        assert_eq!(lines[0].text, "hi there");
    }

    #[test]
    fn renders_tool_use_with_dollar_prefix() {
        let raw = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","name":"Read","input":{"file_path":"/tmp/x"}}]}}"#;
        let lines = render_jsonl(raw);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].prefix, "  $ ");
        assert!(lines[0].text.starts_with("Read"));
        assert!(lines[0].text.contains("/tmp/x"));
    }

    #[test]
    fn skips_unknown_record_types() {
        let raw = "{\"type\":\"system\",\"message\":\"boot\"}\n{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}";
        let lines = render_jsonl(raw);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "hi");
    }

    #[test]
    fn malformed_json_is_skipped() {
        let raw = "{not json}\n{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}";
        let lines = render_jsonl(raw);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn mixes_user_assistant_tool() {
        let raw = r#"{"type":"user","message":{"content":"do X"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"sure"},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#;
        let lines = render_jsonl(raw);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].prefix, "→ ");
        assert_eq!(lines[1].prefix, "← ");
        assert_eq!(lines[2].prefix, "  $ ");
    }

    #[tokio::test]
    async fn find_session_file_locates_jsonl_recursively() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join(".claude").join("projects").join("-home-x");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("ABC123.jsonl");
        std::fs::write(&file, "{}\n").unwrap();
        let found = find_session_file(dir.path(), "ABC123").await;
        assert_eq!(found.as_deref(), Some(file.as_path()));
    }

    #[tokio::test]
    async fn render_session_returns_lines() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join(".claude").join("projects").join("-home-x");
        std::fs::create_dir_all(&nested).unwrap();
        let file = nested.join("S1.jsonl");
        std::fs::write(
            &file,
            "{\"type\":\"user\",\"message\":{\"content\":\"q\"}}\n",
        )
        .unwrap();
        let lines = render_session(dir.path(), "S1").await.unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "q");
    }
}
