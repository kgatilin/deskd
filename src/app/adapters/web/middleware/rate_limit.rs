//! In-memory rolling-window rate limiter for the web adapter (#443).
//!
//! Two independent buckets — one keyed by IP, one keyed by telegram_id. Each
//! bucket tracks `(count, window_start_unix)` over a one-hour window; the
//! count resets on the first request that arrives once the window has rolled
//! past `window_start + window_secs`.

use std::collections::HashMap;
use std::sync::Mutex;

/// Sliding-window-ish rate limiter (technically fixed window per key).
pub struct RateLimiter {
    inner: Mutex<HashMap<String, Bucket>>,
    limit: u32,
    window_secs: i64,
}

#[derive(Debug, Clone, Copy)]
struct Bucket {
    count: u32,
    window_start: i64,
}

impl RateLimiter {
    pub fn new(limit: u32, window_secs: i64) -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
            limit,
            window_secs: window_secs.max(1),
        }
    }

    /// Atomically check the limit and, if allowed, increment. Returns
    /// `true` when the caller may proceed and `false` when the limit has
    /// been hit for the current window.
    pub fn check_and_record(&self, key: &str, now_unix: i64) -> bool {
        let mut g = self.inner.lock().expect("rate limiter mutex");
        let bucket = g.entry(key.to_string()).or_insert(Bucket {
            count: 0,
            window_start: now_unix,
        });

        if now_unix - bucket.window_start >= self.window_secs {
            bucket.count = 0;
            bucket.window_start = now_unix;
        }

        if bucket.count >= self.limit {
            return false;
        }

        bucket.count += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_under_limit() {
        let rl = RateLimiter::new(3, 60);
        assert!(rl.check_and_record("ip", 0));
        assert!(rl.check_and_record("ip", 1));
        assert!(rl.check_and_record("ip", 2));
    }

    #[test]
    fn rejects_over_limit() {
        let rl = RateLimiter::new(2, 60);
        assert!(rl.check_and_record("ip", 0));
        assert!(rl.check_and_record("ip", 1));
        assert!(!rl.check_and_record("ip", 2));
        assert!(!rl.check_and_record("ip", 30));
    }

    #[test]
    fn resets_after_window() {
        let rl = RateLimiter::new(2, 60);
        assert!(rl.check_and_record("ip", 0));
        assert!(rl.check_and_record("ip", 1));
        assert!(!rl.check_and_record("ip", 2));
        // Past the window — limit resets.
        assert!(rl.check_and_record("ip", 60));
        assert!(rl.check_and_record("ip", 90));
        assert!(!rl.check_and_record("ip", 100));
    }

    #[test]
    fn keys_are_independent() {
        let rl = RateLimiter::new(1, 60);
        assert!(rl.check_and_record("ip-a", 0));
        // ip-a is now full, but ip-b is fresh.
        assert!(!rl.check_and_record("ip-a", 0));
        assert!(rl.check_and_record("ip-b", 0));
    }
}
