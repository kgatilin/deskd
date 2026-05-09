//! In-memory magic-link token store (#443).
//!
//! Tokens are 32 random bytes generated with the OS RNG and base64url-encoded.
//! The store keeps `SHA-256(token)` (hex) as the key — never the plaintext —
//! so a memory dump or core file does not directly expose live tokens.
//!
//! Single use is enforced by removing the entry on lookup.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::digest;
use ring::rand::SecureRandom;
use std::collections::HashMap;
use std::sync::Mutex;

/// One pending magic-link token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingToken {
    pub telegram_id: i64,
    /// Expiry, unix epoch seconds.
    pub expires_at: i64,
}

/// Thread-safe in-memory token store. Created once at adapter startup.
#[derive(Default)]
pub struct TokenStore {
    inner: Mutex<HashMap<String, PendingToken>>,
}

impl TokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh token for `telegram_id`. Returns the raw token (which
    /// must be sent to Telegram via the link) and stores `SHA-256(token)`
    /// internally with the supplied expiry.
    pub fn issue(&self, telegram_id: i64, expires_at: i64) -> String {
        let token = generate_token();
        let hash = hash_token(&token);
        let mut g = self.inner.lock().expect("token store mutex");
        g.insert(
            hash,
            PendingToken {
                telegram_id,
                expires_at,
            },
        );
        token
    }

    /// Consume a token. Returns the matching pending entry only if it
    /// existed and `now_unix <= expires_at`. The entry is removed regardless
    /// (single-use even if expired, so an expired token can't be replayed).
    pub fn consume(&self, token: &str, now_unix: i64) -> Option<PendingToken> {
        let hash = hash_token(token);
        let mut g = self.inner.lock().expect("token store mutex");
        let entry = g.remove(&hash)?;
        if now_unix > entry.expires_at {
            return None;
        }
        Some(entry)
    }

    /// Drop expired entries — called opportunistically. Cheap (linear scan
    /// over the map), and the store is small (one entry per pending login).
    pub fn sweep_expired(&self, now_unix: i64) {
        let mut g = self.inner.lock().expect("token store mutex");
        g.retain(|_, t| t.expires_at >= now_unix);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.inner.lock().unwrap().len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.inner.lock().unwrap().is_empty()
    }
}

/// Generate a fresh random token (32 bytes, base64url-encoded).
pub fn generate_token() -> String {
    let rng = ring::rand::SystemRandom::new();
    let mut buf = [0u8; 32];
    rng.fill(&mut buf).expect("OS RNG failure for magic link");
    URL_SAFE_NO_PAD.encode(buf)
}

/// SHA-256 of the token, hex-encoded — the storage key.
fn hash_token(token: &str) -> String {
    let d = digest::digest(&digest::SHA256, token.as_bytes());
    hex_encode(d.as_ref())
}

fn hex_encode(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{:02x}", byte));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_returns_token_consumable_within_ttl() {
        let store = TokenStore::new();
        let token = store.issue(42, 1_000_000);
        let entry = store.consume(&token, 999_000).expect("token valid");
        assert_eq!(entry.telegram_id, 42);
    }

    #[test]
    fn second_consume_returns_none() {
        let store = TokenStore::new();
        let token = store.issue(42, 1_000_000);
        assert!(store.consume(&token, 999_000).is_some());
        assert!(store.consume(&token, 999_000).is_none());
    }

    #[test]
    fn expired_token_consume_returns_none_and_evicts() {
        let store = TokenStore::new();
        let token = store.issue(42, 1000);
        assert_eq!(store.len(), 1);
        assert!(store.consume(&token, 2000).is_none());
        // Even though expired, the entry was removed (defense-in-depth).
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn unknown_token_returns_none() {
        let store = TokenStore::new();
        assert!(store.consume("nonsense", 0).is_none());
    }

    #[test]
    fn store_does_not_keep_plaintext_tokens() {
        let store = TokenStore::new();
        let token = store.issue(7, 1_000_000);
        let g = store.inner.lock().unwrap();
        // The map key must not be the raw token.
        assert!(!g.contains_key(&token));
        // Exactly one entry, keyed by hex SHA-256 (64 hex chars).
        assert_eq!(g.len(), 1);
        let key = g.keys().next().unwrap();
        assert_eq!(key.len(), 64);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn sweep_drops_only_expired() {
        let store = TokenStore::new();
        let _t1 = store.issue(1, 1000); // expired at 1500
        let _t2 = store.issue(2, 5000); // still valid at 1500
        assert_eq!(store.len(), 2);
        store.sweep_expired(1500);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn issued_tokens_are_unique() {
        let store = TokenStore::new();
        let a = store.issue(1, 1_000_000);
        let b = store.issue(2, 1_000_000);
        assert_ne!(a, b);
    }
}
