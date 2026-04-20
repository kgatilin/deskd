//! Telegram MTProto client (issue #376).
//!
//! When the `mtproto` feature is enabled this module provides a thin
//! wrapper around grammers-client that can:
//!   - connect to Telegram using a persisted session file,
//!   - interactively log in (SMS code + optional 2FA),
//!   - fetch chat history via `messages.getHistory`.
//!
//! Without the `mtproto` feature the module only exports the shared
//! `ChatMessage` type (pure serde, no grammers dep).

use serde::{Deserialize, Serialize};

/// A single Telegram chat message as returned by `fetch_history`.
///
/// Deliberately narrow: text only, no media, no formatting entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub message_id: i32,
    pub date: chrono::DateTime<chrono::Utc>,
    pub sender_id: Option<i64>,
    pub sender_username: Option<String>,
    pub text: String,
    pub reply_to_msg_id: Option<i32>,
}

#[cfg(feature = "mtproto")]
mod imp {
    use super::ChatMessage;
    use crate::config::MtProtoConfig;
    use anyhow::{Context, Result, bail};
    use grammers_client::Client;
    use grammers_client::client::SignInError;
    use grammers_mtsender::{SenderPool, SenderPoolFatHandle};
    use grammers_session::storages::SqliteSession;
    use grammers_session::types::PeerId;
    use std::io::{self, BufRead, Write};
    use std::sync::Arc;
    use tracing::{debug, info};

    /// Wrapper around a grammers `Client` + its background I/O task.
    pub struct MtProtoClient {
        client: Client,
        handle: SenderPoolFatHandle,
        _pool_task: tokio::task::JoinHandle<()>,
    }

    impl MtProtoClient {
        /// Connect to Telegram using the session file from `cfg`.
        ///
        /// The session file must already exist (created by `deskd telegram-login`).
        pub async fn connect(cfg: &MtProtoConfig) -> Result<Self> {
            let session_path = cfg
                .session_path
                .to_str()
                .context("session_path is not valid UTF-8")?;

            let session = Arc::new(
                SqliteSession::open(session_path)
                    .await
                    .with_context(|| format!("failed to open session file: {}", session_path))?,
            );

            let SenderPool {
                runner,
                handle,
                updates: _,
            } = SenderPool::new(Arc::clone(&session), cfg.api_id);

            let pool_task = tokio::spawn(runner.run());
            let client = Client::new(handle.clone());

            if !client.is_authorized().await? {
                bail!(
                    "session file exists but is not authorized — run `deskd telegram-login` again"
                );
            }

            info!("MTProto client connected (api_id={})", cfg.api_id);
            Ok(Self {
                client,
                handle,
                _pool_task: pool_task,
            })
        }

        /// Fetch the most recent `limit` messages from `chat_id`.
        ///
        /// For supergroups/channels, `chat_id` is the negative ID as used
        /// by the Bot API (e.g. -1001234567890). We strip the -100 prefix
        /// to get the MTProto channel ID.
        pub async fn fetch_history(
            &self,
            chat_id: i64,
            limit: u32,
            offset_id: Option<i32>,
        ) -> Result<Vec<ChatMessage>> {
            let peer_ref = self
                .resolve_peer_ref(chat_id)
                .await
                .with_context(|| format!("failed to resolve chat {}", chat_id))?;

            let mut iter = self.client.iter_messages(peer_ref).limit(limit as usize);
            if let Some(oid) = offset_id {
                iter = iter.offset_id(oid);
            }

            let mut messages = Vec::new();
            while let Some(msg) = iter.next().await? {
                let sender_id = msg.sender().map(|p| peer_id_to_i64(&p.id()));
                let sender_username = msg.sender().and_then(|p| p.username().map(String::from));
                messages.push(ChatMessage {
                    message_id: msg.id(),
                    date: msg.date(),
                    sender_id,
                    sender_username,
                    text: msg.text().to_string(),
                    reply_to_msg_id: msg.reply_to_message_id(),
                });
            }

            debug!("fetched {} messages from chat {}", messages.len(), chat_id);
            Ok(messages)
        }

        /// Resolve a Bot-API-style chat_id to a grammers `PeerRef`.
        ///
        /// First tries the session cache (fast). If not cached, iterates
        /// dialogs to populate the cache and retries.
        async fn resolve_peer_ref(&self, chat_id: i64) -> Result<grammers_session::types::PeerRef> {
            let peer_id = bot_api_to_peer_id(chat_id)?;

            // Try session cache first.
            if let Some(pr) = self.handle.session.peer_ref(peer_id).await {
                return Ok(pr);
            }

            // Cache miss — iterate dialogs to populate it.
            info!(
                "peer {} not in session cache, iterating dialogs to populate",
                chat_id
            );
            let mut dialogs = self.client.iter_dialogs();
            while let Some(dialog) = dialogs.next().await? {
                let p = dialog.peer();
                if p.id() == peer_id {
                    // Found it — to_ref() should succeed now that it's cached.
                    return p.to_ref().await.with_context(|| {
                        format!(
                            "peer {} found in dialogs but to_ref() returned None",
                            chat_id
                        )
                    });
                }
            }

            bail!(
                "chat {} not found in account's dialogs — \
                 the user account must be a member of this chat",
                chat_id
            )
        }
    }

    /// Convert a Bot-API-style chat_id to a grammers `PeerId`.
    ///
    /// `PeerId` uses the same Bot-API Dialog ID encoding internally,
    /// so we construct via the appropriate typed constructor using the
    /// bare (positive) id extracted from the Bot-API convention:
    /// - Positive → user
    /// - -100XXXXXXXXXX → channel/supergroup (bare = strip -100 prefix)
    /// - Other negative → small group (bare = abs)
    fn bot_api_to_peer_id(chat_id: i64) -> Result<PeerId> {
        if chat_id > 0 {
            PeerId::user(chat_id).context("invalid user id")
        } else if chat_id < -1_000_000_000_000 {
            let bare = -(chat_id + 1_000_000_000_000);
            PeerId::channel(bare).context("invalid channel id")
        } else {
            let bare = -chat_id;
            PeerId::chat(bare).context("invalid group id")
        }
    }

    /// Extract the bare (positive) user/chat/channel id from a `PeerId`.
    fn peer_id_to_i64(pid: &PeerId) -> i64 {
        pid.bare_id()
    }

    // ─── Interactive login flow ─────────────────────────────────────────────

    /// Run the interactive `deskd telegram-login` flow.
    ///
    /// Prompts for SMS code and optional 2FA password on stdin, then
    /// persists the session to `session_path`.
    pub async fn telegram_login(
        api_id: i32,
        api_hash: &str,
        phone: &str,
        session_path: &str,
    ) -> Result<()> {
        eprintln!("Opening session at: {}", session_path);

        let session = Arc::new(SqliteSession::open(session_path).await?);
        let SenderPool {
            runner,
            handle,
            updates: _,
        } = SenderPool::new(Arc::clone(&session), api_id);

        let pool_task = tokio::spawn(runner.run());
        let client = Client::new(handle.clone());

        if client.is_authorized().await? {
            eprintln!("Already authorized! Session file is valid.");
            handle.quit();
            let _ = pool_task.await;
            return Ok(());
        }

        eprintln!("Requesting login code for {}...", phone);
        let token = client
            .request_login_code(phone, api_hash)
            .await
            .context("failed to request login code")?;

        eprint!("Enter the code you received: ");
        io::stderr().flush()?;
        let code = {
            let stdin = io::stdin();
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            line.trim().to_string()
        };

        match client.sign_in(&token, &code).await {
            Ok(user) => {
                eprintln!(
                    "Signed in as: {} (id {:?})",
                    user.first_name().unwrap_or("?"),
                    user.id()
                );
            }
            Err(SignInError::PasswordRequired(pw_token)) => {
                let hint = pw_token
                    .hint()
                    .map(|h| format!(" (hint: {})", h))
                    .unwrap_or_default();
                eprint!("2FA password required{}: ", hint);
                io::stderr().flush()?;
                let password = {
                    let stdin = io::stdin();
                    let mut line = String::new();
                    stdin.lock().read_line(&mut line)?;
                    line.trim().to_string()
                };
                let user = client
                    .check_password(pw_token, password.as_bytes())
                    .await
                    .map_err(|e| match e {
                        SignInError::InvalidPassword(_) => {
                            anyhow::anyhow!("invalid 2FA password")
                        }
                        other => anyhow::anyhow!("sign-in error: {:?}", other),
                    })?;
                eprintln!(
                    "Signed in as: {} (id {:?})",
                    user.first_name().unwrap_or("?"),
                    user.id()
                );
            }
            Err(SignInError::InvalidCode) => {
                bail!("invalid login code — please try again");
            }
            Err(SignInError::SignUpRequired) => {
                bail!("this phone number is not registered with Telegram");
            }
            Err(other) => {
                bail!("sign-in failed: {:?}", other);
            }
        }

        eprintln!("Session saved to: {}", session_path);

        handle.quit();
        let _ = pool_task.await;
        Ok(())
    }
}

#[cfg(feature = "mtproto")]
pub use imp::{MtProtoClient, telegram_login};

#[cfg(test)]
mod tests {
    use super::ChatMessage;
    use chrono::Utc;

    #[test]
    fn chat_message_roundtrips_through_json() {
        let msg = ChatMessage {
            message_id: 42,
            date: Utc::now(),
            sender_id: Some(111),
            sender_username: Some("kira".into()),
            text: "hello".into(),
            reply_to_msg_id: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: ChatMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(back.message_id, 42);
        assert_eq!(back.text, "hello");
        assert_eq!(back.sender_username.as_deref(), Some("kira"));
    }
}
