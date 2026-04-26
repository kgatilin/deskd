//! Per-agent restartable component bundle used by `config_reload`.
//!
//! Holds the join handles + cancellation tokens for adapters, schedule
//! watcher, config watcher, reminder runner, and sub-agent workers, plus
//! the abort routines that drive cooperative shutdown when `deskd.yaml`
//! changes.

use std::time::Duration;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Holds all restartable component handles and cancellation tokens for a single agent.
pub struct AgentComponents {
    pub adapter_handles: Vec<JoinHandle<()>>,
    /// Cancellation tokens — one per adapter, in the same order as `adapter_handles`.
    pub adapter_cancel_tokens: Vec<CancellationToken>,
    pub schedule_watcher: Option<JoinHandle<()>>,
    pub config_watcher: Option<JoinHandle<()>>,
    pub reminder_runner: Option<JoinHandle<()>>,
    pub sub_agent_handles: Vec<JoinHandle<()>>,
}

impl AgentComponents {
    /// Gracefully cancel then abort all restartable components.
    ///
    /// 1. Cancel all adapter tokens (cooperative cancellation).
    /// 2. Wait up to 1 s for adapter tasks to finish.
    /// 3. Abort any adapters that are still running after the grace period.
    /// 4. Abort all remaining non-adapter components immediately.
    pub async fn abort_all(&mut self) {
        // Step 1: signal cooperative cancellation for all adapters.
        for token in &self.adapter_cancel_tokens {
            token.cancel();
        }

        // Step 2: wait up to 1 s for adapters to drain.
        // We can't await the JoinHandles directly without consuming them, so we
        // use a short sleep as the grace window — the tokens already signalled
        // cancellation; the tasks will exit their select! loops promptly.
        if !self.adapter_handles.is_empty() {
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }

        // Step 3: abort adapter handles (no-op if already finished).
        for h in self.adapter_handles.drain(..) {
            h.abort();
        }
        self.adapter_cancel_tokens.clear();

        // Step 4: abort remaining components immediately.
        if let Some(h) = self.schedule_watcher.take() {
            h.abort();
        }
        if let Some(h) = self.config_watcher.take() {
            h.abort();
        }
        if let Some(h) = self.reminder_runner.take() {
            h.abort();
        }
        for h in self.sub_agent_handles.drain(..) {
            h.abort();
        }
    }

    /// Abort only adapter components (used for adapter-only config changes).
    pub async fn abort_adapters(&mut self) {
        for token in &self.adapter_cancel_tokens {
            token.cancel();
        }
        if !self.adapter_handles.is_empty() {
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
        for h in self.adapter_handles.drain(..) {
            h.abort();
        }
        self.adapter_cancel_tokens.clear();
    }

    /// Abort only schedule/reminder components (used for schedule-only config changes).
    pub fn abort_schedules(&mut self) {
        if let Some(h) = self.schedule_watcher.take() {
            h.abort();
        }
        if let Some(h) = self.reminder_runner.take() {
            h.abort();
        }
    }

    /// Abort only sub-agent worker components.
    pub fn abort_sub_agents(&mut self) {
        for h in self.sub_agent_handles.drain(..) {
            h.abort();
        }
    }

    /// Return a summary string of component counts.
    pub fn summary(&self) -> String {
        format!(
            "adapters={}, schedules={}, sub_agents={}",
            self.adapter_handles.len(),
            if self.schedule_watcher.is_some() {
                1
            } else {
                0
            },
            self.sub_agent_handles.len(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_components_summary() {
        let components = AgentComponents {
            adapter_handles: Vec::new(),
            adapter_cancel_tokens: Vec::new(),
            schedule_watcher: None,
            config_watcher: None,
            reminder_runner: None,
            sub_agent_handles: Vec::new(),
        };
        assert_eq!(
            components.summary(),
            "adapters=0, schedules=0, sub_agents=0"
        );
    }

    #[tokio::test]
    async fn test_agent_components_abort_all_empty() {
        let mut components = AgentComponents {
            adapter_handles: Vec::new(),
            adapter_cancel_tokens: Vec::new(),
            schedule_watcher: None,
            config_watcher: None,
            reminder_runner: None,
            sub_agent_handles: Vec::new(),
        };
        // Should not panic on empty components.
        components.abort_all().await;
        assert!(components.adapter_handles.is_empty());
        assert!(components.adapter_cancel_tokens.is_empty());
        assert!(components.schedule_watcher.is_none());
        assert!(components.sub_agent_handles.is_empty());
    }

    #[tokio::test]
    async fn test_adapter_cancel_token_fires_on_abort_adapters() {
        let token = CancellationToken::new();
        let child = token.clone();

        // Spawn a task that waits for cancellation.
        let handle = tokio::spawn(async move {
            child.cancelled().await;
        });

        let mut components = AgentComponents {
            adapter_handles: vec![handle],
            adapter_cancel_tokens: vec![token],
            schedule_watcher: None,
            config_watcher: None,
            reminder_runner: None,
            sub_agent_handles: Vec::new(),
        };

        components.abort_adapters().await;
        assert!(components.adapter_handles.is_empty());
        assert!(components.adapter_cancel_tokens.is_empty());
    }
}
