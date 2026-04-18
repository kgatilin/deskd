/// Unified config hot-reload — watches deskd.yaml and restarts all restartable
/// components when the config changes.
///
/// Restartable components (aborted and respawned on config change):
///   - Adapters (Telegram, Discord)
///   - Schedule watcher
///   - Reminder runner
///   - Sub-agent workers
///
/// Session-persistent components (NOT restarted):
///   - Bus server (transport layer)
///   - Main worker (Claude session)
///   - Bus API handler
///   - Workflow engine
///
/// System prompt changes are still injected via bus message (existing pattern
/// in config_watcher.rs) rather than restarting the worker.
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::app::{adapters, config_watcher, schedule, worker};
use crate::config;

/// Holds all restartable component handles for a single agent.
pub struct AgentComponents {
    pub adapter_handles: Vec<JoinHandle<()>>,
    pub schedule_watcher: Option<JoinHandle<()>>,
    pub config_watcher: Option<JoinHandle<()>>,
    pub reminder_runner: Option<JoinHandle<()>>,
    pub sub_agent_handles: Vec<JoinHandle<()>>,
}

impl AgentComponents {
    /// Abort all restartable components.
    pub fn abort_all(&mut self) {
        for h in self.adapter_handles.drain(..) {
            h.abort();
        }
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

/// Spawn all restartable components for an agent and return their handles.
pub async fn spawn_components(
    def: &config::AgentDef,
    user_cfg: Option<&config::UserConfig>,
    admin_telegram_ids: &[i64],
    bus_socket: &str,
    agent_name: &str,
    cfg_path: &str,
) -> anyhow::Result<AgentComponents> {
    let mut components = AgentComponents {
        adapter_handles: Vec::new(),
        schedule_watcher: None,
        config_watcher: None,
        reminder_runner: None,
        sub_agent_handles: Vec::new(),
    };

    // Adapters (Telegram, Discord, etc.)
    for adapter in adapters::build_adapters(def, user_cfg, admin_telegram_ids) {
        let bus = bus_socket.to_string();
        let name = agent_name.to_string();
        let adapter_name = adapter.name().to_string();
        let handle = tokio::spawn(async move {
            if let Err(e) = adapter.run(bus, name).await {
                tracing::error!(adapter = %adapter_name, error = %e, "adapter failed");
            }
        });
        components.adapter_handles.push(handle);
    }

    // Schedule watcher
    {
        let bus = bus_socket.to_string();
        let name = agent_name.to_string();
        let config = cfg_path.to_string();
        let home = def.work_dir.clone();
        let handle = tokio::spawn(async move {
            schedule::watch_and_reload(config, bus, name, home).await;
        });
        components.schedule_watcher = Some(handle);
    }

    // Config watcher (system_prompt injection)
    {
        let bus = bus_socket.to_string();
        let name = agent_name.to_string();
        let config = cfg_path.to_string();
        let handle = tokio::spawn(async move {
            config_watcher::watch_system_prompt(config, bus, name).await;
        });
        components.config_watcher = Some(handle);
    }

    // Reminder runner
    {
        let bus = bus_socket.to_string();
        let name = agent_name.to_string();
        let handle = tokio::spawn(async move {
            schedule::run_reminders(bus, name).await;
        });
        components.reminder_runner = Some(handle);
    }

    // Sub-agent workers
    if let Some(ucfg) = user_cfg {
        for sub in &ucfg.agents {
            let mcp_json = serde_json::json!({
                "mcpServers": {
                    "deskd": {
                        "command": "deskd",
                        "args": ["mcp", "--agent", &sub.name]
                    }
                }
            })
            .to_string();

            let context_cfg = sub.context.clone().or_else(|| ucfg.context.clone());

            let sub_cfg = crate::app::agent::AgentConfig {
                name: sub.name.clone(),
                model: sub.model.clone(),
                system_prompt: sub.system_prompt.clone(),
                work_dir: def.work_dir.clone(),
                max_turns: ucfg.max_turns,
                unix_user: def.unix_user.clone(),
                budget_usd: def.budget_usd,
                command: vec![
                    "claude".into(),
                    "--output-format".into(),
                    "stream-json".into(),
                    "--verbose".into(),
                    "--dangerously-skip-permissions".into(),
                    "--model".into(),
                    sub.model.clone(),
                    "--max-turns".into(),
                    ucfg.max_turns.to_string(),
                    "--mcp-config".into(),
                    mcp_json,
                ],
                config_path: Some(cfg_path.to_string()),
                container: def.container.clone(),
                session: sub.session.clone(),
                runtime: sub.runtime.clone(),
                context: context_cfg,
                compact_threshold: sub.compact_threshold,
            };
            crate::app::agent::create_or_update_from_config(&sub_cfg).await?;

            let sub_name = sub.name.clone();
            let bus = bus_socket.to_string();
            let subs = sub.subscribe.clone();
            let sub_task_store = crate::app::task::TaskStore::default_for_home();
            let handle = tokio::spawn(async move {
                if let Err(e) = worker::run(
                    &sub_name,
                    &bus,
                    Some(bus.clone()),
                    Some(subs),
                    &sub_task_store,
                )
                .await
                {
                    tracing::error!(agent = %sub_name, error = %e, "sub-agent worker exited");
                }
            });
            components.sub_agent_handles.push(handle);
        }
    }

    Ok(components)
}

/// Watch the agent's deskd.yaml for changes and hot-reload all restartable components.
///
/// Polls the file mtime every 30 seconds. On change, aborts all restartable
/// components, reloads config, and respawns them. Bus server, main worker,
/// bus API, and workflow engine are NOT affected.
pub async fn watch_and_reload(
    def: config::AgentDef,
    initial_components: AgentComponents,
    admin_telegram_ids: Vec<i64>,
    bus_socket: String,
    agent_name: String,
    cfg_path: String,
) {
    let mut components = initial_components;
    let mut last_modified = file_mtime(&cfg_path);

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;

        let current_mtime = file_mtime(&cfg_path);
        if current_mtime == last_modified {
            continue;
        }
        last_modified = current_mtime;

        info!(agent = %agent_name, "config file changed, reloading all restartable components");

        // Abort all running restartable components.
        let old_summary = components.summary();
        components.abort_all();
        info!(agent = %agent_name, old = %old_summary, "aborted old components");

        // Reload config.
        let user_cfg = match config::UserConfig::load(&cfg_path) {
            Ok(cfg) => Some(cfg),
            Err(e) => {
                warn!(
                    agent = %agent_name,
                    error = %e,
                    "failed to reload config, components stopped"
                );
                continue;
            }
        };

        // Respawn all restartable components.
        match spawn_components(
            &def,
            user_cfg.as_ref(),
            &admin_telegram_ids,
            &bus_socket,
            &agent_name,
            &cfg_path,
        )
        .await
        {
            Ok(new_components) => {
                let summary = new_components.summary();
                components = new_components;
                info!(
                    agent = %agent_name,
                    summary = %summary,
                    "config reloaded: {}", summary
                );
            }
            Err(e) => {
                warn!(
                    agent = %agent_name,
                    error = %e,
                    "failed to respawn components after config reload"
                );
            }
        }
    }
}

fn file_mtime(path: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_components_summary() {
        let components = AgentComponents {
            adapter_handles: Vec::new(),
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

    #[test]
    fn test_agent_components_abort_all_empty() {
        let mut components = AgentComponents {
            adapter_handles: Vec::new(),
            schedule_watcher: None,
            config_watcher: None,
            reminder_runner: None,
            sub_agent_handles: Vec::new(),
        };
        // Should not panic on empty components.
        components.abort_all();
        assert!(components.adapter_handles.is_empty());
        assert!(components.schedule_watcher.is_none());
        assert!(components.sub_agent_handles.is_empty());
    }

    #[test]
    fn test_file_mtime_missing() {
        assert!(file_mtime("/tmp/nonexistent-deskd-config-reload-test").is_none());
    }

    #[test]
    fn test_file_mtime_existing() {
        assert!(file_mtime("Cargo.toml").is_some());
    }
}
