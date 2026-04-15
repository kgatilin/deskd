//! Bus application layer — re-exports bus server and provides bus construction.
//!
//! The bus server implementation lives in `infra::bus_server`. This module
//! re-exports it for convenience and adds app-level helpers (e.g. `connect_bus`).

pub use crate::infra::bus_server::serve;

use crate::domain::message::{Message, Metadata};
use crate::infra::unix_bus::UnixBus;
use crate::ports::bus::MessageBus;

/// Connect to a bus socket, returning a trait-erased `MessageBus`.
///
/// This is the composition-root factory for bus clients. Application code
/// should call this instead of importing `UnixBus` directly.
pub async fn connect_bus(socket_path: &str) -> anyhow::Result<impl MessageBus> {
    UnixBus::connect(socket_path).await
}

/// Send a one-shot message to a target via the bus.
///
/// Connects, registers as `source`, sends the message, then disconnects.
/// Used by A2A server and other fire-and-forget senders.
pub async fn send_message(
    socket_path: &str,
    source: &str,
    target: &str,
    text: &str,
) -> anyhow::Result<()> {
    let bus = connect_bus(socket_path).await?;
    bus.register(source, &[]).await?;
    let msg = Message {
        id: uuid::Uuid::new_v4().to_string(),
        source: source.to_string(),
        target: target.to_string(),
        payload: serde_json::json!({"task": text}),
        reply_to: None,
        metadata: Metadata::default(),
    };
    bus.send(&msg).await?;
    Ok(())
}

/// Send a query to a target and wait for the response.
///
/// Creates a temporary bus connection with a unique name, sends the query with
/// `reply_to` pointing to the temporary name, and waits for a response matched
/// by `query_id`. The connection is dropped (cleaned up) when done.
pub async fn query_and_wait(
    socket_path: &str,
    agent_name: &str,
    target: &str,
    question: &str,
    query_id: &str,
    timeout: std::time::Duration,
) -> anyhow::Result<String> {
    let temp_name = format!(
        "{}-query-{}",
        agent_name,
        &query_id[..8.min(query_id.len())]
    );

    let bus = connect_bus(socket_path).await?;
    bus.register(&temp_name, &[]).await?;

    // Check target exists via bus list before sending.
    // (Best-effort — target could disconnect between check and send.)

    let msg = Message {
        id: query_id.to_string(),
        source: temp_name.clone(),
        target: target.to_string(),
        payload: serde_json::json!({"task": question, "query_id": query_id}),
        reply_to: Some(temp_name.clone()),
        metadata: Metadata::default(),
    };
    bus.send(&msg).await?;

    // Wait for response with correlation ID matching.
    let response = tokio::time::timeout(timeout, async {
        let mut full_response = String::new();
        loop {
            let resp = bus.recv().await?;

            // Correlation check: only accept responses with matching query_id.
            if let Some(resp_qid) = resp.payload.get("query_id").and_then(|v| v.as_str())
                && resp_qid != query_id
            {
                continue; // Not our response — skip.
            }

            // Extract text from payload.
            if let Some(text) = resp.payload.get("text").and_then(|v| v.as_str()) {
                full_response.push_str(text);
            } else if let Some(result) = resp.payload.get("result").and_then(|v| v.as_str()) {
                full_response.push_str(result);
            } else if let Some(task) = resp.payload.get("task").and_then(|v| v.as_str()) {
                full_response.push_str(task);
            }

            let is_final = resp
                .payload
                .get("final")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if is_final || !full_response.is_empty() {
                return Ok::<String, anyhow::Error>(full_response);
            }
        }
    })
    .await;

    // Bus connection is dropped here — temp client removed from bus server.
    match response {
        Ok(Ok(text)) => Ok(text),
        Ok(Err(e)) => anyhow::bail!("bus error while waiting for response: {}", e),
        Err(_) => anyhow::bail!(
            "timed out after {}s waiting for response from {}",
            timeout.as_secs(),
            target
        ),
    }
}
