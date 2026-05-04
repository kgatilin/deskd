//! Integration test for #426 call-site coverage: real call sites that publish
//! a `diagnostics.warn` event to the bus.
//!
//! Drives `infra::diag::warn_event` and `error_event` end-to-end through the
//! bus server, asserting that a subscriber receives a structured event with
//! the expected `kind`, `source`, and `details` fields.

use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

fn temp_socket(label: &str) -> String {
    format!(
        "/tmp/deskd-test-diag-{}-{}.sock",
        label,
        uuid::Uuid::new_v4()
    )
}

async fn connect_subscriber(
    socket: &str,
    name: &str,
    subscriptions: &[&str],
) -> (
    tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(socket).await.unwrap();
    let (reader, mut writer) = stream.into_split();

    let reg = serde_json::json!({
        "type": "register",
        "name": name,
        "subscriptions": subscriptions,
    });
    let mut line = serde_json::to_string(&reg).unwrap();
    line.push('\n');
    writer.write_all(line.as_bytes()).await.unwrap();

    (BufReader::new(reader).lines(), writer)
}

async fn read_one(
    lines: &mut tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    timeout_ms: u64,
) -> Option<serde_json::Value> {
    tokio::time::timeout(Duration::from_millis(timeout_ms), lines.next_line())
        .await
        .ok()?
        .ok()?
        .and_then(|l| serde_json::from_str(&l).ok())
}

#[tokio::test]
async fn warn_event_publishes_to_diagnostics_warn_topic() {
    let socket = temp_socket("warn-exact");

    let sock = socket.clone();
    tokio::spawn(async move {
        deskd::app::bus::serve(&sock).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (mut sub_rx, _sub_tx) =
        connect_subscriber(&socket, "test-subscriber", &["diagnostics.warn"]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    deskd::infra::diag::warn_event(
        Some(&socket),
        "telegram",
        "transport.send_failed",
        "telegram send failed: rate limited",
        serde_json::json!({"chat_id": 42}),
    );

    let received = read_one(&mut sub_rx, 2000).await;
    assert!(received.is_some(), "subscriber should receive diag event");
    let received = received.unwrap();
    assert_eq!(received["target"], "diagnostics.warn");
    let payload = &received["payload"];
    assert_eq!(payload["topic"], "diagnostics.warn");
    assert_eq!(payload["source"], "telegram");
    assert_eq!(payload["kind"], "transport.send_failed");
    assert_eq!(payload["details"]["chat_id"], 42);
    assert!(payload["timestamp"].is_string());

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn diagnostics_glob_receives_warn_and_error() {
    let socket = temp_socket("glob");

    let sock = socket.clone();
    tokio::spawn(async move {
        deskd::app::bus::serve(&sock).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (mut sub_rx, _sub_tx) =
        connect_subscriber(&socket, "diag-glob-sub", &["diagnostics.*"]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    deskd::infra::diag::warn_event(
        Some(&socket),
        "github_poll",
        "transport.poll_failed",
        "x",
        serde_json::Value::Null,
    );
    deskd::infra::diag::error_event(
        Some(&socket),
        "supervisor",
        "respawn.failed",
        "child died",
        serde_json::json!({"agent": "life"}),
    );

    let mut topics: Vec<String> = Vec::new();
    for _ in 0..2 {
        if let Some(msg) = read_one(&mut sub_rx, 2000).await
            && let Some(t) = msg["target"].as_str()
        {
            topics.push(t.to_string());
        }
    }
    topics.sort();
    assert_eq!(topics, vec!["diagnostics.error", "diagnostics.warn"]);

    let _ = std::fs::remove_file(&socket);
}

/// When a message has no matching subscriber on a non-`diagnostics.*` target,
/// the bus should publish a `bus.undeliverable` diagnostic so monitors see it.
#[tokio::test]
async fn bus_undeliverable_publishes_diag_event() {
    let socket = temp_socket("undeliverable");

    let sock = socket.clone();
    tokio::spawn(async move {
        deskd::app::bus::serve(&sock).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (mut sub_rx, _sub_tx) =
        connect_subscriber(&socket, "diag-watcher", &["diagnostics.warn"]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (_unused_rx, mut sender_tx) = connect_subscriber(&socket, "test-sender", &[]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let send = serde_json::json!({
        "type": "message",
        "id": uuid::Uuid::new_v4().to_string(),
        "source": "test-sender",
        "target": "unmatched.topic",
        "payload": {"hello": "world"},
    });
    let mut line = serde_json::to_string(&send).unwrap();
    line.push('\n');
    sender_tx.write_all(line.as_bytes()).await.unwrap();

    let received = read_one(&mut sub_rx, 2000).await;
    assert!(
        received.is_some(),
        "diag-watcher should receive bus.undeliverable event"
    );
    let received = received.unwrap();
    assert_eq!(received["target"], "diagnostics.warn");
    let payload = &received["payload"];
    assert_eq!(payload["topic"], "diagnostics.warn");
    assert_eq!(payload["source"], "bus");
    assert_eq!(payload["kind"], "bus.undeliverable");
    assert_eq!(payload["details"]["target"], "unmatched.topic");
    assert_eq!(payload["details"]["source"], "test-sender");

    let _ = std::fs::remove_file(&socket);
}

/// When the undeliverable message itself targets `diagnostics.*`, the bus
/// must NOT publish another `bus.undeliverable` event — that would loop on
/// every diag emission whose topic has no subscriber.
///
/// Subscribe a watcher to `diagnostics.warn`, then send a message to
/// `diagnostics.error` (a different diagnostic topic with no subscriber).
/// Without the guard the bus would publish a `bus.undeliverable` event to
/// `diagnostics.warn`; the watcher would see it. With the guard, the watcher
/// receives nothing.
#[tokio::test]
async fn bus_undeliverable_does_not_recurse_on_diagnostics_target() {
    let socket = temp_socket("no-recurse");

    let sock = socket.clone();
    tokio::spawn(async move {
        deskd::app::bus::serve(&sock).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (mut sub_rx, _sub_tx) =
        connect_subscriber(&socket, "diag-watcher", &["diagnostics.warn"]).await;
    let (_unused_rx, mut sender_tx) = connect_subscriber(&socket, "test-sender", &[]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let send = serde_json::json!({
        "type": "message",
        "id": uuid::Uuid::new_v4().to_string(),
        "source": "test-sender",
        "target": "diagnostics.error",
        "payload": {"x": 1},
    });
    let mut line = serde_json::to_string(&send).unwrap();
    line.push('\n');
    sender_tx.write_all(line.as_bytes()).await.unwrap();

    let received = read_one(&mut sub_rx, 500).await;
    assert!(
        received.is_none(),
        "bus must not publish bus.undeliverable when the undeliverable target is itself a diagnostics.* topic"
    );

    let _ = std::fs::remove_file(&socket);
}

#[tokio::test]
async fn warn_event_is_non_blocking_under_load() {
    // With no subscriber, each call still spawns a fire-and-forget publish task
    // that fails silently. The caller path itself must not block.
    let socket = temp_socket("no-sub");

    let sock = socket.clone();
    tokio::spawn(async move {
        deskd::app::bus::serve(&sock).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let start = std::time::Instant::now();
    for i in 0..10 {
        deskd::infra::diag::warn_event(
            Some(&socket),
            "test",
            "load",
            format!("event {}", i),
            serde_json::Value::Null,
        );
    }
    assert!(start.elapsed() < Duration::from_secs(1));

    let _ = std::fs::remove_file(&socket);
}
