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

/// Send a one-shot message with a fresh-session flag.
///
/// Like `send_message`, but sets `metadata.fresh = true` so the worker
/// restarts the executor without `--resume`.
pub async fn send_message_fresh(
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
        metadata: Metadata {
            fresh: true,
            ..Default::default()
        },
    };
    bus.send(&msg).await?;
    Ok(())
}
