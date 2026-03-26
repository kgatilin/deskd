/// PersistentSession — keeps a single claude CLI process alive across multiple turns.
///
/// deskd writes messages to claude's stdin one at a time; responses are read
/// from stdout as stream-json events.  On process death the session is restarted
/// automatically using `--resume <session_id>` to preserve conversation context.
///
/// Message protocol (stdin):
///   Each user turn is a single UTF-8 line (newline-terminated).
///   Multi-line content is collapsed to a single line to avoid ambiguity.
///
/// Message protocol (stdout):
///   Claude emits newline-delimited JSON events (stream-json format).
///   A `{"type":"result"}` event marks the end of one turn.
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tracing::{debug, info, warn};

use crate::agent::{AgentConfig, AgentState, load_state, save_state_pub};

/// Maximum consecutive restart attempts before giving up.
const MAX_RESTARTS: u32 = 3;

pub struct PersistentSession {
    name: String,
    child: Child,
    stdin: ChildStdin,
    stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    state: AgentState,
    restarts: u32,
}

impl PersistentSession {
    /// Spawn a persistent Claude process for the named agent.
    pub async fn spawn(name: &str) -> Result<Self> {
        let state = load_state(name)?;
        let (child, stdin, stdout_lines) = spawn_process(&state.config, &state.session_id).await?;
        info!(agent = %name, session = %state.session_id, "persistent session started");
        Ok(Self {
            name: name.to_string(),
            child,
            stdin,
            stdout_lines,
            state,
            restarts: 0,
        })
    }

    /// Send a message, wait for the result event, return response text.
    /// If the process has died, restarts it (up to MAX_RESTARTS times).
    pub async fn send(&mut self, message: &str) -> Result<String> {
        // Flatten to single line — claude reads one turn per newline.
        let line = format!("{}\n", message.replace('\n', " "));

        if let Err(e) = self.stdin.write_all(line.as_bytes()).await {
            warn!(agent = %self.name, error = %e, "stdin write failed, restarting session");
            self.restart().await?;
            self.stdin.write_all(line.as_bytes()).await
                .context("stdin write failed after restart")?;
        }
        self.stdin.flush().await.context("stdin flush")?;

        self.read_result().await
    }

    /// Read stream-json events until a `result` event, collecting the response text.
    async fn read_result(&mut self) -> Result<String> {
        let mut response_text = String::new();
        let mut new_session_id = String::new();
        let mut task_cost = 0.0_f64;
        let mut task_turns = 0_u32;

        loop {
            let line = match self.stdout_lines.next_line().await {
                Ok(Some(l)) => l,
                Ok(None) => {
                    // stdout closed — process exited unexpectedly
                    bail!("claude process exited unexpectedly");
                }
                Err(e) => bail!("stdout read error: {}", e),
            };

            if line.is_empty() {
                continue;
            }

            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            match v["type"].as_str() {
                Some("assistant") => {
                    if let Some(blocks) = v["message"]["content"].as_array() {
                        for block in blocks {
                            if block["type"] == "text" {
                                if let Some(t) = block["text"].as_str() {
                                    response_text.push_str(t);
                                }
                            }
                        }
                    }
                }
                Some("result") => {
                    if let Some(sid) = v["session_id"].as_str() {
                        new_session_id = sid.to_string();
                    }
                    if let Some(cost) = v["total_cost_usd"].as_f64() {
                        task_cost = cost;
                    }
                    if let Some(t) = v["num_turns"].as_u64() {
                        task_turns = t as u32;
                    }

                    // Check for error subtypes
                    if let Some(subtype) = v["subtype"].as_str() {
                        if subtype == "error_max_turns" {
                            warn!(agent = %self.name, "max turns reached");
                        }
                    }
                    break;
                }
                _ => {
                    debug!(agent = %self.name, event = %v["type"].as_str().unwrap_or("?"), "stream event");
                }
            }
        }

        // Persist updated state
        if !new_session_id.is_empty() {
            self.state.session_id = new_session_id;
        }
        self.state.total_cost += task_cost;
        self.state.total_turns += task_turns;
        save_state_pub(&self.state)?;

        if response_text.is_empty() {
            bail!("no response text from claude");
        }

        Ok(response_text)
    }

    /// Kill current process and spawn a fresh one, resuming the existing session.
    async fn restart(&mut self) -> Result<()> {
        self.restarts += 1;
        if self.restarts > MAX_RESTARTS {
            bail!("persistent session for '{}' exceeded max restarts ({})", self.name, MAX_RESTARTS);
        }

        warn!(agent = %self.name, attempt = self.restarts, "restarting persistent session");

        let _ = self.child.kill().await;

        // Reload state in case it was updated externally
        self.state = load_state(&self.name)?;

        let (child, stdin, stdout_lines) =
            spawn_process(&self.state.config, &self.state.session_id).await?;

        self.child = child;
        self.stdin = stdin;
        self.stdout_lines = stdout_lines;

        info!(agent = %self.name, session = %self.state.session_id, "session restarted");
        Ok(())
    }

    /// Gracefully shut down the session (kill the child process).
    pub async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
        info!(agent = %self.name, "session shut down");
    }
}

async fn spawn_process(
    cfg: &AgentConfig,
    session_id: &str,
) -> Result<(
    Child,
    ChildStdin,
    tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
)> {
    let mut args = vec![
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--model".to_string(),
        cfg.model.clone(),
    ];

    if !session_id.is_empty() {
        args.push("--resume".to_string());
        args.push(session_id.to_string());
    }

    if !cfg.system_prompt.is_empty() && session_id.is_empty() {
        args.push("--system-prompt".to_string());
        args.push(cfg.system_prompt.clone());
    }

    let mut cmd = build_spawn_command(cfg, &args);
    let mut child = cmd.spawn().context("failed to spawn claude")?;

    let stdin = child.stdin.take().context("claude has no stdin")?;
    let stdout = child.stdout.take().context("claude has no stdout")?;
    let stdout_lines = BufReader::new(stdout).lines();

    Ok((child, stdin, stdout_lines))
}

fn build_spawn_command(cfg: &AgentConfig, args: &[String]) -> Command {
    let mut cmd = match &cfg.unix_user {
        Some(user) => {
            let mut c = Command::new("sudo");
            c.args(["-u", user, "-H", "--", "claude"]);
            c.args(args);
            c.env_remove("SSH_AUTH_SOCK");
            c.env_remove("SSH_AGENT_PID");
            c
        }
        None => {
            let mut c = Command::new("claude");
            c.args(args);
            c
        }
    };
    cmd.current_dir(&cfg.work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()); // suppress TUI noise
    cmd
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_message_flatten() {
        // Multi-line messages are collapsed to a single line for stdin protocol
        let msg = "line one\nline two\nline three";
        let flattened = format!("{}\n", msg.replace('\n', " "));
        assert_eq!(flattened, "line one line two line three\n");
        assert_eq!(flattened.lines().count(), 1);
    }

    #[test]
    fn test_stream_json_result_parse() {
        let event = serde_json::json!({
            "type": "result",
            "session_id": "sess-abc123",
            "total_cost_usd": 0.042,
            "num_turns": 3,
            "subtype": "success",
        });
        assert_eq!(event["type"], "result");
        assert_eq!(event["session_id"], "sess-abc123");
        assert_eq!(event["total_cost_usd"].as_f64().unwrap(), 0.042);
        assert_eq!(event["num_turns"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_stream_json_assistant_text_extraction() {
        let event = serde_json::json!({
            "type": "assistant",
            "message": {
                "content": [
                    {"type": "text", "text": "Hello, "},
                    {"type": "text", "text": "world!"},
                    {"type": "tool_use", "name": "Read"}, // non-text block, skip
                ]
            }
        });
        let mut text = String::new();
        if let Some(blocks) = event["message"]["content"].as_array() {
            for block in blocks {
                if block["type"] == "text" {
                    if let Some(t) = block["text"].as_str() {
                        text.push_str(t);
                    }
                }
            }
        }
        assert_eq!(text, "Hello, world!");
    }
}
