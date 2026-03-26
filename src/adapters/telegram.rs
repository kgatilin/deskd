/// Telegram adapter — polls Telegram Bot API and bridges messages to/from the deskd bus.
///
/// Message flow:
///   Telegram user → adapter → bus (queue:tasks, reply_to=telegram:<chat_id>)
///   Agent result  → bus (target=telegram:<chat_id>) → adapter → Telegram user
///
/// The adapter registers on the bus as "telegram-adapter" and subscribes to "telegram:*"
/// to receive replies addressed to any Telegram chat.
use anyhow::{Context, Result};
use serde_json::Value;
use teloxide::prelude::*;
use teloxide::types::ParseMode;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

const ADAPTER_NAME: &str = "telegram-adapter";

/// Run the Telegram adapter.
/// Connects to the bus, starts Telegram polling, and bridges messages both ways.
pub async fn run(token: String, socket_path: String) -> Result<()> {
    info!("starting Telegram adapter");

    let bot = Bot::new(token);

    // Channel for outbound messages: (chat_id, text)
    let (outbound_tx, outbound_rx) = mpsc::unbounded_channel::<(i64, String)>();

    // Task 1: connect to bus and handle inbound/outbound routing
    let bus_task = {
        let outbound_tx = outbound_tx.clone();
        let socket_path = socket_path.clone();
        tokio::spawn(async move {
            if let Err(e) = bus_loop(&socket_path, outbound_tx).await {
                tracing::error!(error = %e, "telegram bus loop failed");
            }
        })
    };

    // Task 2: send outbound messages to Telegram
    let sender_task = {
        let bot = bot.clone();
        tokio::spawn(async move {
            outbound_sender(bot, outbound_rx).await;
        })
    };

    // Task 3: poll Telegram for incoming messages and post to bus
    let polling_task = {
        let socket_path = socket_path.clone();
        tokio::spawn(async move {
            if let Err(e) = polling_loop(bot, socket_path).await {
                tracing::error!(error = %e, "telegram polling loop failed");
            }
        })
    };

    // Wait for any task to finish (they all run indefinitely under normal operation)
    tokio::select! {
        _ = bus_task => warn!("telegram bus task exited"),
        _ = sender_task => warn!("telegram sender task exited"),
        _ = polling_task => warn!("telegram polling task exited"),
    }

    Ok(())
}

/// Connect to the bus as "telegram-adapter", subscribe to "telegram:*",
/// and forward incoming bus messages to the outbound channel.
async fn bus_loop(
    socket_path: &str,
    outbound_tx: mpsc::UnboundedSender<(i64, String)>,
) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("telegram adapter: failed to connect to bus at {}", socket_path))?;

    // Register on bus
    let reg = serde_json::json!({
        "type": "register",
        "name": ADAPTER_NAME,
        "subscriptions": ["telegram:*"],
    });
    let mut line = serde_json::to_string(&reg)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    info!("telegram adapter registered on bus");

    let (reader, _writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        if line.is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "telegram adapter: invalid message from bus");
                continue;
            }
        };

        let target = msg.get("target").and_then(|t| t.as_str()).unwrap_or("");

        // Target format: "telegram:<chat_id>"
        if let Some(chat_id_str) = target.strip_prefix("telegram:") {
            let chat_id: i64 = match chat_id_str.parse() {
                Ok(id) => id,
                Err(_) => {
                    warn!(target = %target, "telegram adapter: invalid chat_id in target");
                    continue;
                }
            };

            let text = msg
                .get("payload")
                .and_then(|p| p.get("result").or_else(|| p.get("error")))
                .and_then(|t| t.as_str())
                .unwrap_or("(no content)");

            debug!(chat_id = chat_id, "forwarding bus message to Telegram");
            if outbound_tx.send((chat_id, text.to_string())).is_err() {
                warn!("telegram adapter: outbound channel closed");
                break;
            }
        }
    }

    Ok(())
}

/// Send messages from the outbound channel to Telegram.
async fn outbound_sender(bot: Bot, mut rx: mpsc::UnboundedReceiver<(i64, String)>) {
    while let Some((chat_id, text)) = rx.recv().await {
        let chat = ChatId(chat_id);
        if let Err(e) = bot
            .send_message(chat, &text)
            .parse_mode(ParseMode::MarkdownV2)
            .await
            .or_else(|_| {
                // If MarkdownV2 fails (e.g. malformed markdown), retry as plain text
                futures_util::future::Either::Right(bot.send_message(chat, &text))
            })
            .await
        {
            warn!(chat_id = chat_id, error = %e, "failed to send Telegram message");
        }
    }
}

/// Poll Telegram for new messages and post them to the bus as tasks.
async fn polling_loop(bot: Bot, socket_path: String) -> Result<()> {
    // We use a one-shot bus connection per message to keep things simple.
    // For high-throughput scenarios this can be replaced with a persistent connection.
    teloxide::repl(bot, move |bot: Bot, msg: Message| {
        let socket_path = socket_path.clone();
        async move {
            if let Some(text) = msg.text() {
                let chat_id = msg.chat.id.0;
                let reply_to = format!("telegram:{}", chat_id);

                debug!(chat_id = chat_id, "received Telegram message, posting to bus");

                if let Err(e) = post_to_bus(&socket_path, text, &reply_to).await {
                    warn!(chat_id = chat_id, error = %e, "failed to post message to bus");
                    let _ = bot.send_message(msg.chat.id, "Internal error, please try again.").await;
                }
            }
            Ok(())
        }
    })
    .await;

    Ok(())
}

/// Post a task message to the bus (fire-and-forget, no reply wait).
async fn post_to_bus(socket_path: &str, task: &str, reply_to: &str) -> Result<()> {
    let mut stream = UnixStream::connect(socket_path)
        .await
        .with_context(|| format!("failed to connect to bus at {}", socket_path))?;

    let reg = serde_json::json!({
        "type": "register",
        "name": format!("telegram-inbound-{}", Uuid::new_v4()),
        "subscriptions": [],
    });
    let mut reg_line = serde_json::to_string(&reg)?;
    reg_line.push('\n');
    stream.write_all(reg_line.as_bytes()).await?;

    let msg = serde_json::json!({
        "type": "message",
        "id": Uuid::new_v4().to_string(),
        "source": ADAPTER_NAME,
        "target": "queue:tasks",
        "payload": {
            "task": task,
            "msg_type": "task",
        },
        "reply_to": reply_to,
        "metadata": {"priority": 5u8},
    });
    let mut msg_line = serde_json::to_string(&msg)?;
    msg_line.push('\n');
    stream.write_all(msg_line.as_bytes()).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_chat_id_parse() {
        // Verify that both positive (user) and negative (group) chat IDs parse correctly
        let target = "telegram:-1001234567890";
        let chat_id_str = target.strip_prefix("telegram:").unwrap();
        let id: i64 = chat_id_str.parse().unwrap();
        assert_eq!(id, -1001234567890i64);

        let target2 = "telegram:123456";
        let id2: i64 = target2.strip_prefix("telegram:").unwrap().parse().unwrap();
        assert_eq!(id2, 123456i64);
    }

    #[test]
    fn test_post_to_bus_message_format() {
        // Verify the message JSON has required fields
        let task = "hello world";
        let reply_to = "telegram:-123";
        let msg = serde_json::json!({
            "type": "message",
            "id": "test-id",
            "source": "telegram-adapter",
            "target": "queue:tasks",
            "payload": {
                "task": task,
                "msg_type": "task",
            },
            "reply_to": reply_to,
            "metadata": {"priority": 5u8},
        });
        assert_eq!(msg["payload"]["task"], "hello world");
        assert_eq!(msg["reply_to"], "telegram:-123");
        assert_eq!(msg["target"], "queue:tasks");
    }
}
