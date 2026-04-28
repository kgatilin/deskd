//! Cross-instance A2A integration test (#350).
//!
//! Spins up a second deskd "instance B" (its own bus + A2A HTTP server bound
//! to an ephemeral port) and drives it from "instance A" — represented by a
//! plain reqwest client. Exercises the discovery + JSON-RPC surface end to
//! end:
//!   - GET /.well-known/agent-card.json
//!   - POST /a2a tasks/send
//!   - POST /a2a tasks/get
//!   - POST /a2a tasks/cancel
//!
//! Also verifies the bus side: instance B routes incoming tasks onto its own
//! bus so the registered drainer agent actually receives the task payload.
//! This is the missing piece a single-process unit test cannot prove.

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use deskd::app::a2a::{AgentAuthentication, AgentCapabilities, AgentCard, AgentSkill};
use deskd::app::a2a_server::{A2aState, A2aTaskRegistry, router};

fn temp_socket() -> String {
    format!(
        "/tmp/deskd-a2a-cross-{}.sock",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

/// Connect to a bus and register; return (lines reader, writer).
async fn connect_and_register(
    socket: &str,
    name: &str,
    subscriptions: &[&str],
) -> (
    tokio::io::Lines<BufReader<tokio::net::unix::OwnedReadHalf>>,
    tokio::net::unix::OwnedWriteHalf,
) {
    let stream = UnixStream::connect(socket)
        .await
        .unwrap_or_else(|e| panic!("connect {socket}: {e}"));
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

/// Build an A2aState for instance B advertising a single `dev/echo` skill.
fn make_state(bus_socket: String, listen_url: String) -> Arc<A2aState> {
    let card = AgentCard {
        name: "instance-b".into(),
        description: Some("cross-instance test peer".into()),
        url: listen_url,
        version: "0.0.1".into(),
        capabilities: AgentCapabilities {
            streaming: false,
            push_notifications: false,
        },
        skills: vec![AgentSkill {
            id: "dev/echo".into(),
            name: "echo".into(),
            description: String::new(),
            tags: vec![],
        }],
        needs: vec![],
        authentication: AgentAuthentication {
            schemes: vec!["none".into()],
            jwks: None,
        },
    };
    Arc::new(A2aState {
        agent_card: card,
        api_key: None,
        bus_socket,
        auth_mode: "none".into(),
        trusted_keys: vec![],
        tasks: A2aTaskRegistry::default(),
    })
}

/// Bind a TCP listener on an ephemeral port and start instance B's A2A
/// server on it. Returns the resolved `http://127.0.0.1:PORT` base URL.
async fn start_a2a_server(state: Arc<A2aState>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = router(state);
    tokio::spawn(async move {
        // Errors here just mean the test ended; ignore.
        let _ = axum::serve(listener, app).await;
    });
    // Give the listener a tick to start accepting.
    tokio::time::sleep(Duration::from_millis(50)).await;
    format!("http://{addr}")
}

#[tokio::test]
async fn test_a2a_cross_instance_send_get_cancel() {
    // ── Instance B: bus + drainer + A2A HTTP server ──
    let bus_sock = temp_socket();

    let serve_sock = bus_sock.clone();
    tokio::spawn(async move {
        let _ = deskd::app::bus::serve(&serve_sock).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The agent-side drainer that handle_tasks_send routes to (target
    // "agent:dev"). Without this the bus send would fail with no-route and
    // tasks/send would return -32000 instead of registering the task.
    let (mut dev_rx, _dev_tx) = connect_and_register(&bus_sock, "dev", &["agent:dev"]).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let state = make_state(bus_sock.clone(), "http://placeholder".into());
    let base_url = start_a2a_server(state).await;

    // ── Instance A: a plain reqwest client. ──
    let client = reqwest::Client::new();

    // 1) Discovery: agent card is reachable cross-instance.
    let card: serde_json::Value = client
        .get(format!("{base_url}/.well-known/agent-card.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(card["name"], "instance-b");
    assert_eq!(card["skills"][0]["id"], "dev/echo");

    // 2) tasks/send: A asks B to run dev/echo.
    let send_resp: serde_json::Value = client
        .post(format!("{base_url}/a2a"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tasks/send",
            "params": {"skill": "dev/echo", "message": "hello from A"},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        send_resp.get("error").map(|e| e.is_null()).unwrap_or(true),
        "tasks/send unexpected error: {send_resp}"
    );
    let task_id = send_resp["result"]["taskId"]
        .as_str()
        .expect("taskId in response")
        .to_string();
    assert_eq!(send_resp["result"]["status"], "working");
    assert_eq!(send_resp["result"]["agent"], "dev");
    assert_eq!(send_resp["result"]["skill"], "dev/echo");

    // 3) The drainer should actually receive the bus-routed task. This is
    //    what makes the test cross-cutting: the HTTP layer reached the bus,
    //    not just the in-memory registry.
    let bus_msg = read_one(&mut dev_rx, 2000)
        .await
        .expect("dev drainer should see bus message");
    assert_eq!(bus_msg["source"], "a2a");
    assert_eq!(bus_msg["target"], "agent:dev");
    assert_eq!(bus_msg["payload"]["task"], "hello from A");

    // 4) tasks/get returns the registered record.
    let get_resp: serde_json::Value = client
        .post(format!("{base_url}/a2a"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tasks/get",
            "params": {"taskId": task_id},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(get_resp["result"]["task_id"], task_id);
    assert_eq!(get_resp["result"]["status"], "working");
    assert_eq!(get_resp["result"]["skill"], "dev/echo");
    assert_eq!(get_resp["result"]["agent"], "dev");

    // 5) tasks/cancel flips the registered task to "cancelled".
    let cancel_resp: serde_json::Value = client
        .post(format!("{base_url}/a2a"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tasks/cancel",
            "params": {"taskId": task_id},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancel_resp["result"]["status"], "cancelled");

    // tasks/get reflects the cancellation.
    let get_after: serde_json::Value = client
        .post(format!("{base_url}/a2a"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tasks/get",
            "params": {"taskId": task_id},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(get_after["result"]["status"], "cancelled");

    let _ = std::fs::remove_file(&bus_sock);
}

#[tokio::test]
async fn test_a2a_cross_instance_unknown_task_returns_error() {
    // Instance B is reachable but the requested task id never existed.
    let bus_sock = temp_socket();
    let serve_sock = bus_sock.clone();
    tokio::spawn(async move {
        let _ = deskd::app::bus::serve(&serve_sock).await;
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    let state = make_state(bus_sock.clone(), "http://placeholder".into());
    let base_url = start_a2a_server(state).await;

    let client = reqwest::Client::new();
    let resp: serde_json::Value = client
        .post(format!("{base_url}/a2a"))
        .json(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "tasks/get",
            "params": {"taskId": "does-not-exist"},
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(resp["error"]["code"], -32002);
    assert!(
        resp["error"]["message"]
            .as_str()
            .unwrap()
            .contains("task not found")
    );

    let _ = std::fs::remove_file(&bus_sock);
}
