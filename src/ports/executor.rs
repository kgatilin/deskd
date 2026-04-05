//! Executor port — abstraction over LLM execution backends.
//!
//! Claude CLI, ACP, Gemini, Ollama — all implement this trait.
//! The executor is stateless infrastructure: it receives a task message
//! and returns a result. It does not own context or state.

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;

/// Accumulated token usage across all messages in a task.
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

impl TokenUsage {
    /// Merge another `TokenUsage` into this one (struct-to-struct).
    pub fn merge(&mut self, other: &TokenUsage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
    }

    /// Accumulate usage from a parsed JSON value.
    /// Expects the `usage` object from a Claude assistant message.
    pub fn accumulate(&mut self, usage: &serde_json::Value) {
        if let Some(v) = usage.get("input_tokens").and_then(|v| v.as_u64()) {
            self.input_tokens += v;
        }
        if let Some(v) = usage.get("output_tokens").and_then(|v| v.as_u64()) {
            self.output_tokens += v;
        }
        if let Some(v) = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
        {
            self.cache_creation_input_tokens += v;
        }
        if let Some(v) = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
        {
            self.cache_read_input_tokens += v;
        }
    }
}

/// Resource limits for a single task execution.
pub struct TaskLimits {
    /// Max assistant turns (tool-use loops) before killing the process.
    pub max_turns: Option<u32>,
    /// Max cumulative cost (USD) for this agent before killing.
    pub budget_usd: Option<f64>,
}

/// Result of a single executor turn (task completion).
pub struct TurnResult {
    pub response_text: String,
    pub session_id: String,
    pub cost_usd: f64,
    pub num_turns: u32,
    pub token_usage: TokenUsage,
}

/// Abstraction over LLM execution backends.
///
/// Implementations manage a long-lived subprocess (Claude CLI, ACP server, etc.)
/// and accept tasks via `send_task`. The executor handles streaming, progress
/// reporting, and session management internally.
///
/// Object-safe: all async methods return `Pin<Box<dyn Future>>` so the worker
/// can hold `Box<dyn Executor>` without knowing the concrete backend type.
pub trait Executor: Send {
    /// Send a task to the executor and wait for completion.
    ///
    /// `progress_tx` receives streaming text chunks for real-time progress.
    /// `image` is an optional (base64_data, media_type) pair for image attachments.
    fn send_task<'a>(
        &'a self,
        message: &'a str,
        progress_tx: Option<&'a tokio::sync::mpsc::UnboundedSender<String>>,
        image: Option<(&'a str, &'a str)>,
        limits: &'a TaskLimits,
    ) -> Pin<Box<dyn Future<Output = Result<TurnResult>> + Send + 'a>>;

    /// Inject a message into an in-progress task (mid-task message).
    /// Returns Ok(()) if supported, or a warning if not.
    fn inject_message(&self, _message: &str) -> Result<()> {
        Ok(())
    }

    /// Gracefully stop the executor.
    fn stop(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;
}
