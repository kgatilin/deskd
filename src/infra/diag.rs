//! Bus-native diagnostics topic (issue #426).
//!
//! Provides helper functions and macros that emit operationally-meaningful
//! warnings and errors as structured events on two reserved bus topics:
//!
//!   - `diagnostics.warn`
//!   - `diagnostics.error`
//!
//! Every emission ALSO writes a `tracing::warn!` / `tracing::error!` line so
//! existing log scrapers continue to work. Publishing failures are best-effort
//! and never block the underlying operation — they are logged at debug level
//! and dropped.
//!
//! # Event shape
//!
//! ```json
//! {
//!   "topic":     "diagnostics.error",
//!   "timestamp": "2026-04-28T12:34:56Z",
//!   "source":    "telegram-life",
//!   "kind":      "transport.send_failed",
//!   "message":   "telegram send failed: 429 Too Many Requests",
//!   "details":   { "chat_id": "...", "retry_after": 30 },
//!   "trace_id":  null
//! }
//! ```
//!
//! # Usage
//!
//! ```ignore
//! use deskd::infra::diag;
//!
//! // Best-effort: publish a warning to the bus AND log it.
//! diag::warn(
//!     Some(&bus_socket),
//!     &agent_name,
//!     "transport.send_failed",
//!     format!("telegram send failed: {}", err),
//!     serde_json::json!({ "chat_id": chat_id }),
//! );
//! ```
//!
//! Or via the macros:
//!
//! ```ignore
//! diag_warn!(bus = bus_socket, source = agent_name,
//!            kind = "transport.send_failed",
//!            message = format!("telegram send failed: {}", err),
//!            details = serde_json::json!({ "chat_id": chat_id }));
//! ```

use serde_json::{Value, json};
use std::fmt::Display;

/// Reserved bus topic for warnings.
pub const TOPIC_WARN: &str = "diagnostics.warn";
/// Reserved bus topic for errors.
pub const TOPIC_ERROR: &str = "diagnostics.error";

/// Diagnostic severity level. Maps to topic + tracing call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Warn,
    Error,
}

impl Level {
    pub fn topic(self) -> &'static str {
        match self {
            Level::Warn => TOPIC_WARN,
            Level::Error => TOPIC_ERROR,
        }
    }
}

/// Build a structured diagnostic event payload.
///
/// Returns the JSON object that is sent as the bus message payload.
pub fn build_event(
    level: Level,
    source: &str,
    kind: &str,
    message: &str,
    details: Value,
    trace_id: Option<&str>,
) -> Value {
    json!({
        "topic": level.topic(),
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "source": source,
        "kind": kind,
        "message": message,
        "details": details,
        "trace_id": trace_id,
    })
}

/// Publish a diagnostic event to the bus, best-effort.
///
/// If `bus_socket` is `None`, only the local tracing log is written. If the
/// bus connection or send fails, the error is logged at debug level and
/// dropped — the caller is never blocked.
///
/// This function spawns a fire-and-forget tokio task. It assumes a tokio
/// runtime is running; if not (e.g. in tests outside `#[tokio::test]`), the
/// publish step is silently skipped.
pub fn publish(
    level: Level,
    bus_socket: Option<&str>,
    source: &str,
    kind: &str,
    message: impl Display,
    details: Value,
) {
    let message = message.to_string();

    // Always emit a tracing line. The macros also do this so callers see the
    // message at the right level even if they bypass the macros.
    match level {
        Level::Warn => tracing::warn!(source = %source, kind = %kind, "{}", message),
        Level::Error => tracing::error!(source = %source, kind = %kind, "{}", message),
    }

    let socket = match bus_socket {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return,
    };

    let payload = build_event(level, source, kind, &message, details, None);
    let topic = level.topic().to_string();
    let source = source.to_string();

    // Fire-and-forget. If we are not inside a tokio runtime, `try_spawn` would
    // panic — guard with a runtime handle check.
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(async move {
            if let Err(e) = publish_once(&socket, &topic, &source, payload).await {
                tracing::debug!(error = %e, topic = %topic, "diag publish failed (best-effort)");
            }
        });
    }
}

/// Connect to the bus, send the diagnostic message, then disconnect.
async fn publish_once(
    bus_socket: &str,
    topic: &str,
    source: &str,
    payload: Value,
) -> anyhow::Result<()> {
    use crate::ports::bus::MessageBus;

    let bus = crate::app::bus::connect_bus(bus_socket).await?;
    let client_name = format!("diag:{}-{}", source, uuid::Uuid::new_v4());
    bus.register(&client_name, &[]).await?;

    let msg = crate::domain::message::Message {
        id: uuid::Uuid::new_v4().to_string(),
        source: client_name,
        target: topic.to_string(),
        payload,
        reply_to: None,
        metadata: crate::domain::message::Metadata::default(),
    };
    bus.send(&msg).await?;
    Ok(())
}

// ─── Convenience wrappers ───────────────────────────────────────────────────

/// Convenience wrapper: emit a `diagnostics.warn` event.
pub fn warn_event(
    bus_socket: Option<&str>,
    source: &str,
    kind: &str,
    message: impl Display,
    details: Value,
) {
    publish(Level::Warn, bus_socket, source, kind, message, details);
}

/// Convenience wrapper: emit a `diagnostics.error` event.
pub fn error_event(
    bus_socket: Option<&str>,
    source: &str,
    kind: &str,
    message: impl Display,
    details: Value,
) {
    publish(Level::Error, bus_socket, source, kind, message, details);
}

// ─── Macros ─────────────────────────────────────────────────────────────────

/// Emit a `diagnostics.warn` event: log via tracing and best-effort publish to
/// the bus.
///
/// Required arguments: `source`, `kind`, `message`. Optional: `bus`, `details`.
///
/// # Examples
///
/// ```ignore
/// diag_warn!(
///     source = agent_name,
///     bus = bus_socket,
///     kind = "transport.send_failed",
///     message = format!("telegram send failed: {}", e),
///     details = serde_json::json!({ "chat_id": chat_id }),
/// );
/// ```
#[macro_export]
macro_rules! diag_warn {
    (
        source = $source:expr,
        bus = $bus:expr,
        kind = $kind:expr,
        message = $msg:expr
        $(, details = $details:expr )?
        $(,)?
    ) => {{
        let bus_opt: Option<&str> = $crate::infra::diag::__bus_opt(&$bus);
        let details = $crate::infra::diag::__details!( $($details)? );
        $crate::infra::diag::warn_event(bus_opt, &$source, $kind, $msg, details);
    }};
    (
        source = $source:expr,
        kind = $kind:expr,
        message = $msg:expr
        $(, details = $details:expr )?
        $(,)?
    ) => {{
        let details = $crate::infra::diag::__details!( $($details)? );
        $crate::infra::diag::warn_event(None, &$source, $kind, $msg, details);
    }};
}

/// Emit a `diagnostics.error` event: log via tracing and best-effort publish
/// to the bus.
#[macro_export]
macro_rules! diag_error {
    (
        source = $source:expr,
        bus = $bus:expr,
        kind = $kind:expr,
        message = $msg:expr
        $(, details = $details:expr )?
        $(,)?
    ) => {{
        let bus_opt: Option<&str> = $crate::infra::diag::__bus_opt(&$bus);
        let details = $crate::infra::diag::__details!( $($details)? );
        $crate::infra::diag::error_event(bus_opt, &$source, $kind, $msg, details);
    }};
    (
        source = $source:expr,
        kind = $kind:expr,
        message = $msg:expr
        $(, details = $details:expr )?
        $(,)?
    ) => {{
        let details = $crate::infra::diag::__details!( $($details)? );
        $crate::infra::diag::error_event(None, &$source, $kind, $msg, details);
    }};
}

/// Internal helper: lift any `&str`/`String`/`Option<&str>` style argument
/// into an `Option<&str>` for `publish()`.
#[doc(hidden)]
pub trait DiagBusArg {
    fn as_diag_bus(&self) -> Option<&str>;
}

impl DiagBusArg for &str {
    fn as_diag_bus(&self) -> Option<&str> {
        if self.is_empty() { None } else { Some(self) }
    }
}

impl DiagBusArg for String {
    fn as_diag_bus(&self) -> Option<&str> {
        if self.is_empty() {
            None
        } else {
            Some(self.as_str())
        }
    }
}

impl DiagBusArg for &String {
    fn as_diag_bus(&self) -> Option<&str> {
        if self.is_empty() {
            None
        } else {
            Some(self.as_str())
        }
    }
}

impl DiagBusArg for Option<&str> {
    fn as_diag_bus(&self) -> Option<&str> {
        self.and_then(|s| if s.is_empty() { None } else { Some(s) })
    }
}

impl DiagBusArg for Option<String> {
    fn as_diag_bus(&self) -> Option<&str> {
        self.as_deref()
            .and_then(|s| if s.is_empty() { None } else { Some(s) })
    }
}

#[doc(hidden)]
pub fn __bus_opt<T: DiagBusArg>(value: &T) -> Option<&str> {
    value.as_diag_bus()
}

/// Internal helper: lift the optional `details = ...` macro arg into a
/// `serde_json::Value`, defaulting to `Value::Null`.
#[doc(hidden)]
#[macro_export]
macro_rules! __diag_details {
    () => {
        ::serde_json::Value::Null
    };
    ($e:expr) => {
        $e
    };
}

#[doc(hidden)]
pub use crate::__diag_details as __details;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_topic_for_level() {
        assert_eq!(Level::Warn.topic(), "diagnostics.warn");
        assert_eq!(Level::Error.topic(), "diagnostics.error");
    }

    #[test]
    fn test_build_event_shape() {
        let event = build_event(
            Level::Warn,
            "telegram-kira",
            "transport.send_failed",
            "boom",
            json!({"chat_id": "-1234"}),
            None,
        );
        assert_eq!(event["topic"], "diagnostics.warn");
        assert_eq!(event["source"], "telegram-kira");
        assert_eq!(event["kind"], "transport.send_failed");
        assert_eq!(event["message"], "boom");
        assert_eq!(event["details"]["chat_id"], "-1234");
        assert!(event["timestamp"].is_string());
        assert!(event["trace_id"].is_null());
    }

    #[test]
    fn test_build_event_with_trace_id() {
        let event = build_event(
            Level::Error,
            "supervisor",
            "agent.respawn_failed",
            "could not respawn",
            json!({}),
            Some("trace-abc"),
        );
        assert_eq!(event["topic"], "diagnostics.error");
        assert_eq!(event["trace_id"], "trace-abc");
    }

    #[test]
    fn test_diag_bus_arg_empty_string_yields_none() {
        let s: &str = "";
        assert!(s.as_diag_bus().is_none());

        let owned = String::new();
        assert!(owned.as_diag_bus().is_none());

        let none_opt: Option<&str> = None;
        assert!(none_opt.as_diag_bus().is_none());

        let some_empty: Option<&str> = Some("");
        assert!(some_empty.as_diag_bus().is_none());
    }

    #[test]
    fn test_diag_bus_arg_non_empty() {
        let s: &str = "/tmp/bus.sock";
        assert_eq!(s.as_diag_bus(), Some("/tmp/bus.sock"));

        let owned = String::from("/tmp/bus.sock");
        assert_eq!(owned.as_diag_bus(), Some("/tmp/bus.sock"));

        let some_str: Option<&str> = Some("/tmp/bus.sock");
        assert_eq!(some_str.as_diag_bus(), Some("/tmp/bus.sock"));
    }

    /// Sanity check: calling `publish` outside a tokio runtime is a no-op for
    /// the bus side (no panic) and still emits a tracing line.
    #[test]
    fn test_publish_without_runtime_does_not_panic() {
        publish(
            Level::Warn,
            Some("/nonexistent/socket.sock"),
            "test-source",
            "test.kind",
            "test message",
            json!({"foo": "bar"}),
        );
        // No assertion — we just want to confirm no panic.
    }

    /// Ensures the `diag_warn!` and `diag_error!` macros expand correctly
    /// without a `bus =` argument (which makes them safe to call from
    /// non-async contexts where no bus is available).
    #[test]
    fn test_diag_macros_compile_without_bus() {
        let source = "test-source";
        crate::diag_warn!(
            source = source,
            kind = "test.kind",
            message = "macro warn (no bus)",
            details = json!({"x": 1}),
        );
        crate::diag_error!(
            source = source,
            kind = "test.kind",
            message = "macro error (no bus)",
        );
    }

    #[test]
    fn test_diag_macros_compile_with_bus() {
        let source = "test-source";
        let bus = "";
        crate::diag_warn!(
            source = source,
            bus = bus,
            kind = "test.kind",
            message = "macro warn (empty bus)",
        );
        crate::diag_error!(
            source = source,
            bus = bus,
            kind = "test.kind",
            message = "macro error (empty bus)",
            details = json!({"a": 2}),
        );
    }
}
