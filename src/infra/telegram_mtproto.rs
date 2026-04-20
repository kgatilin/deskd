//! Telegram MTProto client (issue #376) — skeleton.
//!
//! This module provides the API surface that the rest of deskd calls
//! into when the `mtproto` feature is enabled. The actual grammers
//! integration (connect, auth flow, `messages.getHistory`) lands in
//! phase 2; for now every method is a `todo!()` stub so callers can
//! be wired against a stable contract.
//!
//! Without the `mtproto` feature, the module compiles to an empty
//! stub — no grammers types leak into the default build.

use serde::{Deserialize, Serialize};

/// A single Telegram chat message as returned by `fetch_history`.
///
/// Deliberately narrow: text only, no media, no formatting entities.
/// Media support is out of scope for phase 1 (see issue #376).
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

    // Keep the grammers import live under the feature flag so the
    // dependency actually gets compiled and linked — otherwise cargo
    // warns about the unused optional dep. The concrete Client wiring
    // lands in phase 2.
    #[allow(unused_imports)]
    use grammers_client as _;

    /// Wrapper around a grammers `Client`. Phase 2 will hold the real
    /// client handle; for now it is a unit struct so the API surface
    /// is stable.
    pub struct MtProtoClient {
        _private: (),
    }

    impl MtProtoClient {
        /// Connect to Telegram's MTProto servers using the session
        /// file referenced by `cfg`. Phase 2: actual grammers wiring.
        pub async fn connect(_cfg: &MtProtoConfig) -> anyhow::Result<Self> {
            todo!("Phase 2: integrate grammers-client (issue #376)")
        }

        /// Fetch the most recent `limit` messages from `chat_id`.
        /// If `offset_id` is set, only messages older than that id are
        /// returned (pagination). Phase 2: real MTProto call.
        pub async fn fetch_history(
            &self,
            _chat_id: i64,
            _limit: u32,
            _offset_id: Option<i32>,
        ) -> anyhow::Result<Vec<ChatMessage>> {
            todo!("Phase 2: actual MTProto messages.getHistory (issue #376)")
        }
    }
}

#[cfg(feature = "mtproto")]
pub use imp::MtProtoClient;

// When the `mtproto` feature is disabled, the module still exports
// `ChatMessage` (pure serde types, no grammers dep) so callers can
// reference the shape in error messages and tests. No MtProtoClient
// is exported — any code that needs it must be gated on the feature.

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
