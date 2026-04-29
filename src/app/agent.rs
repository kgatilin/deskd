//! Agent module — re-exports from submodules for backward compatibility.
//!
//! Split into three submodules (#280):
//! - `agent_registry` — CRUD, state persistence, one-shot execution
//! - `process_builder` — command construction (pure functions)
//! - `agent_process` — persistent process lifecycle (`AgentProcess` + `Executor`)

// Re-export everything so `crate::app::agent::*` continues to work.
pub use super::agent_process::AgentProcess;
pub use super::agent_registry::{
    AgentConfig, AgentState, create, create_or_recover, create_or_update_from_config,
    default_agent_command, list, load_state, load_state_in, remove, save_state_in, save_state_pub,
    send, spawn_ephemeral, stderr_log_path, stream_log_path, to_domain_agent,
};
pub use super::process_builder::build_command;

// Re-export executor port types — canonical definitions live in ports::executor.
pub use crate::ports::executor::{Executor, ProgressSink, TaskLimits, TokenUsage, TurnResult};

// Tests for TokenUsage live in ports::executor. Only keep re-export-level tests here.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_token_usage_default() {
        let usage = TokenUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn test_token_usage_from_json() {
        let json1 = serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 50,
            "cache_creation_input_tokens": 10,
            "cache_read_input_tokens": 20
        });
        let usage = TokenUsage::from(&json1);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_creation_input_tokens, 10);
        assert_eq!(usage.cache_read_input_tokens, 20);

        // Merge a second parsed usage
        let json2 = serde_json::json!({
            "input_tokens": 200,
            "output_tokens": 100,
            "cache_creation_input_tokens": 5,
            "cache_read_input_tokens": 30
        });
        let mut accumulated = usage;
        accumulated.merge(&TokenUsage::from(&json2));
        assert_eq!(accumulated.input_tokens, 300);
        assert_eq!(accumulated.output_tokens, 150);
        assert_eq!(accumulated.cache_creation_input_tokens, 15);
        assert_eq!(accumulated.cache_read_input_tokens, 50);
    }

    #[test]
    fn test_token_usage_from_json_partial() {
        let json = serde_json::json!({
            "input_tokens": 100,
            "output_tokens": 50
        });
        let usage = TokenUsage::from(&json);
        assert_eq!(usage.input_tokens, 100);
        assert_eq!(usage.output_tokens, 50);
        assert_eq!(usage.cache_creation_input_tokens, 0);
        assert_eq!(usage.cache_read_input_tokens, 0);
    }

    #[test]
    fn test_token_usage_merge() {
        let mut a = TokenUsage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 10,
            cache_read_input_tokens: 20,
        };
        let b = TokenUsage {
            input_tokens: 200,
            output_tokens: 75,
            cache_creation_input_tokens: 5,
            cache_read_input_tokens: 15,
        };
        a.merge(&b);
        assert_eq!(a.input_tokens, 300);
        assert_eq!(a.output_tokens, 125);
        assert_eq!(a.cache_creation_input_tokens, 15);
        assert_eq!(a.cache_read_input_tokens, 35);
    }
}
