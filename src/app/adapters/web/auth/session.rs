//! HMAC-SHA256 signed session cookies for the web adapter (#443).
//!
//! The cookie value is a base64url-encoded `<payload>.<signature>` pair where
//! `payload` is itself base64url-encoded JSON of `{telegram_id, iat, exp,
//! csrf}` and `signature` is HMAC-SHA256(payload_bytes, secret).
//!
//! Verification requires:
//! 1. The two-segment shape parses cleanly.
//! 2. The HMAC tag verifies (constant-time via ring).
//! 3. `now <= exp`.
//!
//! On any failure the verifier returns `None`; the caller treats that as
//! unauthenticated.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::hmac;
use serde::{Deserialize, Serialize};

/// Decoded session cookie payload.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPayload {
    /// Telegram user ID this session belongs to.
    pub telegram_id: i64,
    /// Issued-at, unix epoch seconds.
    pub iat: i64,
    /// Expires-at, unix epoch seconds.
    pub exp: i64,
    /// Per-session CSRF token (base64url, ~22 chars). Embedded in the cookie
    /// so the same secret HMAC chain proves authenticity of both session
    /// identity and the CSRF anti-forgery value.
    pub csrf: String,
}

impl SessionPayload {
    /// Construct a fresh payload that expires `ttl_secs` from `now_unix`.
    /// Generates a new random CSRF token.
    pub fn new(telegram_id: i64, now_unix: i64, ttl_secs: i64, csrf: String) -> Self {
        Self {
            telegram_id,
            iat: now_unix,
            exp: now_unix + ttl_secs,
            csrf,
        }
    }
}

/// Sign a payload and return the cookie string `<payload_b64>.<sig_b64>`.
pub fn sign(payload: &SessionPayload, secret: &[u8]) -> String {
    let json = serde_json::to_vec(payload).expect("session payload serializes");
    let payload_b64 = URL_SAFE_NO_PAD.encode(&json);
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let tag = hmac::sign(&key, payload_b64.as_bytes());
    let sig_b64 = URL_SAFE_NO_PAD.encode(tag.as_ref());
    format!("{}.{}", payload_b64, sig_b64)
}

/// Verify a cookie string against `secret`. Returns the payload only if the
/// signature verifies AND `now_unix <= payload.exp`.
pub fn verify(cookie: &str, secret: &[u8], now_unix: i64) -> Option<SessionPayload> {
    let (payload_b64, sig_b64) = cookie.split_once('.')?;
    let sig = URL_SAFE_NO_PAD.decode(sig_b64).ok()?;
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hmac::verify(&key, payload_b64.as_bytes(), &sig).ok()?;

    let json = URL_SAFE_NO_PAD.decode(payload_b64).ok()?;
    let payload: SessionPayload = serde_json::from_slice(&json).ok()?;
    if now_unix > payload.exp {
        return None;
    }
    Some(payload)
}

/// Generate a fresh CSRF token (32 bytes from OS RNG, base64url-encoded).
pub fn generate_csrf_token() -> String {
    use ring::rand::SecureRandom;
    let mut buf = [0u8; 32];
    let rng = ring::rand::SystemRandom::new();
    rng.fill(&mut buf).expect("OS RNG failure for CSRF token");
    URL_SAFE_NO_PAD.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> [u8; 32] {
        let mut s = [0u8; 32];
        for (i, b) in s.iter_mut().enumerate() {
            *b = i as u8;
        }
        s
    }

    #[test]
    fn round_trip_signs_and_verifies() {
        let p = SessionPayload::new(123, 1000, 3600, "csrf-abc".into());
        let cookie = sign(&p, &secret());
        let v = verify(&cookie, &secret(), 1500).unwrap();
        assert_eq!(v, p);
    }

    #[test]
    fn rejects_expired_cookie() {
        let p = SessionPayload::new(7, 1000, 100, "x".into());
        let cookie = sign(&p, &secret());
        assert!(verify(&cookie, &secret(), 1101).is_none());
    }

    #[test]
    fn accepts_at_exact_expiry_boundary() {
        let p = SessionPayload::new(7, 1000, 100, "x".into());
        let cookie = sign(&p, &secret());
        // exp = 1100; verifier accepts now == exp.
        assert!(verify(&cookie, &secret(), 1100).is_some());
    }

    #[test]
    fn rejects_tampered_payload() {
        let p = SessionPayload::new(123, 1000, 3600, "csrf-abc".into());
        let cookie = sign(&p, &secret());
        // Flip a bit in the payload portion.
        let mut bad = cookie.clone();
        let mid = bad.find('.').unwrap();
        let ch = bad.as_bytes()[0];
        // Replace first char with a different valid base64url char
        let new_first: char = if ch == b'A' { 'B' } else { 'A' };
        unsafe { bad.as_bytes_mut()[0] = new_first as u8 };
        let _ = mid;
        assert!(verify(&bad, &secret(), 1500).is_none());
    }

    #[test]
    fn rejects_tampered_signature() {
        let p = SessionPayload::new(1, 1000, 3600, "x".into());
        let cookie = sign(&p, &secret());
        let mut bad = cookie.clone();
        // Change last char of signature portion.
        let last = bad.len() - 1;
        let ch = bad.as_bytes()[last];
        let new_last: u8 = if ch == b'A' { b'B' } else { b'A' };
        unsafe { bad.as_bytes_mut()[last] = new_last };
        assert!(verify(&bad, &secret(), 1500).is_none());
    }

    #[test]
    fn rejects_wrong_secret() {
        let p = SessionPayload::new(1, 1000, 3600, "x".into());
        let cookie = sign(&p, &secret());
        let mut other = secret();
        other[0] ^= 0xff;
        assert!(verify(&cookie, &other, 1500).is_none());
    }

    #[test]
    fn rejects_malformed_cookie() {
        assert!(verify("garbage", &secret(), 0).is_none());
        assert!(verify("only.one.two", &secret(), 0).is_none());
    }

    #[test]
    fn csrf_token_is_unique() {
        let a = generate_csrf_token();
        let b = generate_csrf_token();
        assert_ne!(a, b);
        assert!(a.len() >= 32);
    }
}
