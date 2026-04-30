//! deskd library — agent orchestration runtime.
//!
//! Public API surface: `domain` (pure types), `ports` (trait interfaces),
//! `infra` (implementations), `config` (workspace/user config).
//! Application internals live in `app`.

pub mod app;
pub mod config;
pub mod domain;
pub mod infra;
pub mod ports;

pub mod test_support {
    //! Test-only utilities. `env_lock()` returns a process-wide mutex that
    //! serializes any test mutating environment variables — `setenv`/`unsetenv`
    //! are not thread-safe on POSIX, so concurrent mutations under cargo's
    //! parallel test runner cause UB that surfaces as flaky failures in
    //! unrelated tests doing file I/O.
    //!
    //! Backed by `tokio::sync::Mutex` so async tests can hold the guard across
    //! `.await` without tripping clippy's `await_holding_lock`. Sync tests use
    //! `blocking_lock()`; async tests use `lock().await`. Exposed unconditionally
    //! so integration tests (separate compilation units) can use it too.
    use std::sync::OnceLock;
    use tokio::sync::Mutex;

    pub fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
}
