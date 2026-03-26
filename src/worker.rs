use anyhow::{Context, Result, bail};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::agent;
use crate::message::Metadata;
use crate::session::PersistentSession;

/// Connect to the bus, register, and return the stream.
pub async fn bus_connect(
    socket_path: &str,
    name: &str,
    subscriptions: Vec<String>,
) -> Result<UnixStream> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("Failed to connect to bus at {}", socket_path))?;

    let envelope = serde_json::json!({
        "type": "register",
        "name": name,
        "subscriptions": subscriptions,
    });
    let mut line = serde_json::to_string(&envelope)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    info!(agent = %name, "registered on bus");
    Ok(stream)
}

/// Run the agent worker loop using a persistent Claude session.
///
/// A single claude process is kept alive for the lifetime of the worker.
/// Messages are written to its stdin one at a time; the worker waits for the
/// `result` event before accepting the next message from the bus.
///
/// On process death the session is restarted automatically via --resume.
pub async fn run(name: &str, socket_path: &str) -> Result<()> {
    let initial_state = agent::load_state(name)?;
    let budget_usd = initial_state.config.budget_usd;

    let subscriptions = vec![
        format!("agent:{}", name),
        "queue:tasks".to_string(),
    ];

    let stream = bus_connect(socket_path, name, subscriptions).await?;
    let (reader, writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));

    // Spawn persistent Claude session — one process for the lifetime of this worker.
    let mut session = PersistentSession::spawn(name).await?;

    info!(agent = %name, "waiting for tasks");

    while let Some(line) = lines.next_line().await? {
        if line.is_empty() {
            continue;
        }

        let msg: serde_json::Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                warn!(agent = %name, error = %e, "invalid message from bus");
                continue;
            }
        };

        // Budget check.
        let current_state = agent::load_state(name)?;
        if current_state.total_cost >= budget_usd {
            warn!(
                agent = %name,
                cost = current_state.total_cost,
                budget = budget_usd,
                "budget exceeded, rejecting task"
            );
            continue;
        }

        let task = msg["payload"]["task"].as_str().unwrap_or_default();
        if task.is_empty() {
            debug!(agent = %name, "message has no task payload, skipping");
            continue;
        }

        let msg_id = msg["id"].as_str().unwrap_or("").to_string();
        let source = msg["source"].as_str().unwrap_or("").to_string();
        let reply_to = msg["reply_to"].as_str().map(|s| s.to_string());

        info!(agent = %name, source = %source, task = %truncate(task, 80), "processing task");

        match session.send(task).await {
            Ok(text) => {
                info!(agent = %name, "task completed, posting result");

                let target = reply_to.as_deref().unwrap_or(&source);
                let reply = serde_json::json!({
                    "type": "message",
                    "id": Uuid::new_v4().to_string(),
                    "source": name,
                    "target": target,
                    "payload": {
                        "result": text,
                        "in_reply_to": msg_id,
                    },
                    "metadata": Metadata::default(),
                });

                let mut reply_line = serde_json::to_string(&reply)?;
                reply_line.push('\n');

                let mut w = writer.lock().await;
                if let Err(e) = w.write_all(reply_line.as_bytes()).await {
                    warn!(agent = %name, error = %e, target = %target, "failed to write reply to bus");
                } else {
                    debug!(agent = %name, target = %target, "reply sent to bus");
                }
            }
            Err(e) => {
                warn!(agent = %name, error = %e, "task failed");

                if let Some(rt) = &reply_to {
                    let error_msg = serde_json::json!({
                        "type": "message",
                        "id": Uuid::new_v4().to_string(),
                        "source": name,
                        "target": rt,
                        "payload": {"error": format!("{}", e), "in_reply_to": msg_id},
                        "metadata": {"priority": 5u8},
                    });
                    let mut err_line = serde_json::to_string(&error_msg)?;
                    err_line.push('\n');

                    let mut w = writer.lock().await;
                    if let Err(we) = w.write_all(err_line.as_bytes()).await {
                        warn!(agent = %name, error = %we, "failed to write error reply");
                    }
                }
            }
        }
    }

    session.shutdown().await;
    info!(agent = %name, "disconnected from bus");
    Ok(())
}

/// Send a message via the bus (connect, send, wait for one reply, disconnect).
pub async fn send_via_bus(
    socket_path: &str,
    source: &str,
    target: &str,
    task: &str,
    max_turns: Option<u32>,
) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("Failed to connect to bus at {}", socket_path))?;

    let reg = serde_json::json!({"type": "register", "name": source, "subscriptions": []});
    let mut line = serde_json::to_string(&reg)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let mut payload = serde_json::json!({"task": task});
    if let Some(turns) = max_turns {
        payload["max_turns"] = serde_json::json!(turns);
    }

    let msg = serde_json::json!({
        "type": "message",
        "id": Uuid::new_v4().to_string(),
        "source": source,
        "target": target,
        "payload": payload,
        "reply_to": format!("agent:{}", source),
        "metadata": {"priority": 5u8},
    });
    let mut msg_line = serde_json::to_string(&msg)?;
    msg_line.push('\n');
    stream.write_all(msg_line.as_bytes()).await?;

    debug!(source = %source, target = %target, "task posted to bus");

    let (reader, _) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    if let Some(response_line) = lines.next_line().await? {
        let resp: serde_json::Value = serde_json::from_str(&response_line)?;
        if let Some(result) = resp["payload"]["result"].as_str() {
            println!("{}", result);
        } else if let Some(err) = resp["payload"]["error"].as_str() {
            bail!("Agent error: {}", err);
        } else {
            println!("{}", serde_json::to_string_pretty(&resp)?);
        }
    }

    Ok(())
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}
