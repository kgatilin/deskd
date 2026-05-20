//! `deskd reload` subcommand handler (#474).
//!
//! Sends a `reload_config` RPC to the running daemon's bus API and prints
//! the daemon's response. On parse failure (malformed YAML) the CLI exits
//! non-zero so scripted operators can surface the error.

use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::info;
use uuid::Uuid;

use crate::config;

const REQ_TIMEOUT_SECS: u64 = 5;
const CLIENT_NAME_PREFIX: &str = "deskd-reload-cli";

/// Entry point for `deskd reload`. Talks to the daemon over the bus.
pub async fn handle(
    config_override: Option<String>,
    agent_override: Option<String>,
    socket_override: Option<String>,
) -> Result<()> {
    // Resolve the bus socket. Priority:
    //   1. explicit --socket / $DESKD_BUS_SOCKET
    //   2. running serve state, scoped to --agent if provided
    let bus_socket = resolve_bus_socket(socket_override, agent_override.as_deref())?;

    if !std::path::Path::new(&bus_socket).exists() {
        anyhow::bail!(
            "bus socket not found at {} — is `deskd serve` running?",
            bus_socket
        );
    }

    let request_id = Uuid::new_v4().to_string();
    let client_name = format!("{}-{}", CLIENT_NAME_PREFIX, &request_id[..8]);

    let mut stream = UnixStream::connect(&bus_socket)
        .await
        .with_context(|| format!("connecting to bus at {}", bus_socket))?;

    // Register so the bus_api can route the response back to us.
    let reg = json!({
        "type": "register",
        "name": client_name,
        "subscriptions": [],
    });
    write_line(&mut stream, &reg).await?;

    // Build the RPC payload. `params.path` is only set when the operator
    // passed --config — otherwise the daemon uses its own launch path.
    let mut params = json!({});
    if let Some(ref path) = config_override {
        params["path"] = json!(path);
    }
    let req_payload = json!({
        "method": "reload_config",
        "params": params,
        "request_id": request_id,
    });
    let envelope = json!({
        "type": "message",
        "id": Uuid::new_v4().to_string(),
        "source": client_name,
        "target": "deskd:command",
        "payload": req_payload,
    });
    write_line(&mut stream, &envelope).await?;

    info!(socket = %bus_socket, "sent reload_config RPC, awaiting response");

    // Read until we see a response matching our request_id, or time out.
    // Keep the writer half alive while we read — dropping it would shutdown
    // the write end of the socket, and the bus server interprets that as a
    // client disconnect and removes us from its routing table before the
    // response arrives.
    let (reader, _writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    let response = tokio::time::timeout(std::time::Duration::from_secs(REQ_TIMEOUT_SECS), async {
        while let Some(line) = lines.next_line().await? {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(trimmed) {
                Ok(v) => v,
                Err(_) => continue,
            };
            let payload = v.get("payload").cloned().unwrap_or(v);
            if payload.get("request_id").and_then(|r| r.as_str()) == Some(&request_id) {
                return Ok::<_, anyhow::Error>(Some(payload));
            }
        }
        Ok(None)
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "timed out waiting for daemon response after {}s",
            REQ_TIMEOUT_SECS
        )
    })??;

    let payload =
        response.ok_or_else(|| anyhow::anyhow!("bus closed connection before daemon responded"))?;

    if let Some(err) = payload.get("error").and_then(|e| e.as_str()) {
        anyhow::bail!("daemon rejected reload: {}", err);
    }

    let result = payload
        .get("result")
        .ok_or_else(|| anyhow::anyhow!("daemon response missing 'result' field: {}", payload))?;

    let path = result
        .get("path")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");
    let agent = result
        .get("agent")
        .and_then(|v| v.as_str())
        .unwrap_or("(unknown)");

    println!(
        "reload accepted — daemon will re-apply config from {} (agent: {})",
        path, agent
    );
    Ok(())
}

/// Resolve the bus socket: explicit > $DESKD_BUS_SOCKET > running serve.
fn resolve_bus_socket(explicit: Option<String>, agent: Option<&str>) -> Result<String> {
    if let Some(s) = explicit {
        return Ok(s);
    }
    let state = config::ServeState::load().ok_or_else(|| {
        anyhow::anyhow!(
            "no --socket provided and no running serve detected (no ~/.deskd/serve.state.yaml)"
        )
    })?;
    if let Some(name) = agent {
        let agent_state = state.agent(name).ok_or_else(|| {
            anyhow::anyhow!("agent '{}' not present in running serve state", name)
        })?;
        return Ok(agent_state.bus_socket.clone());
    }
    state
        .find_agent_config()
        .map(|a| a.bus_socket.clone())
        .ok_or_else(|| anyhow::anyhow!("running serve has no agents configured"))
}

async fn write_line(stream: &mut UnixStream, value: &Value) -> Result<()> {
    let mut line = serde_json::to_string(value)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_bus_socket_uses_explicit_first() {
        // Explicit --socket wins over everything.
        let got = resolve_bus_socket(Some("/tmp/explicit.sock".into()), None).unwrap();
        assert_eq!(got, "/tmp/explicit.sock");
    }

    #[test]
    fn resolve_bus_socket_errors_without_state_or_socket() {
        // env-isolated; HOME points somewhere with no ~/.deskd/serve.state.yaml.
        let _env_guard = crate::test_support::env_lock().blocking_lock();
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var("HOME").ok();
        unsafe {
            std::env::set_var("HOME", tmp.path());
        }
        let err = resolve_bus_socket(None, None).unwrap_err();
        assert!(err.to_string().contains("no running serve"));
        unsafe {
            match prev_home {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
            }
        }
    }
}
