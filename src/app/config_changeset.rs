//! Config diff classifier — figures out which restartable components need to
//! be restarted when `deskd.yaml` changes.
//!
//! Used by `config_reload::watch_and_reload` to drive selective restarts so
//! that, e.g., a `system_prompt`-only edit does not disrupt the Telegram
//! adapter.

use crate::config;

/// What changed between two `UserConfig` snapshots.
#[derive(Debug, Default, PartialEq)]
pub struct ConfigChangeset {
    /// Telegram routes, token, or admin_ids changed → adapter restart needed.
    pub adapters_changed: bool,
    /// Schedules changed → schedule_watcher restart needed.
    pub schedules_changed: bool,
    /// Sub-agent definitions changed → sub-agent worker restart needed.
    pub sub_agents_changed: bool,
    /// Only system_prompt changed (and nothing else requiring a restart).
    pub system_prompt_only: bool,
}

/// Compare two `UserConfig` snapshots and classify what changed.
/// Returns a `ConfigChangeset` that callers use to decide which components
/// need to be restarted.
pub fn classify_config_change(
    old: &config::UserConfig,
    new: &config::UserConfig,
) -> ConfigChangeset {
    let adapters_changed = old.telegram != new.telegram || old.discord != new.discord;
    let schedules_changed = old.schedules != new.schedules;
    let sub_agents_changed = old.agents != new.agents;
    let system_prompt_changed = old.system_prompt != new.system_prompt;

    // Classify as system-prompt-only when ONLY the system prompt differs and
    // nothing that requires restarting any component has changed.
    let system_prompt_only =
        system_prompt_changed && !adapters_changed && !schedules_changed && !sub_agents_changed;

    ConfigChangeset {
        adapters_changed,
        schedules_changed,
        sub_agents_changed,
        system_prompt_only,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ScheduleAction, ScheduleDef, TelegramRoute, TelegramRoutesConfig};

    fn base_config() -> config::UserConfig {
        config::UserConfig {
            system_prompt: "You are a helpful assistant.".into(),
            ..Default::default()
        }
    }

    #[test]
    fn test_classify_no_change() {
        let cfg = base_config();
        let cs = classify_config_change(&cfg, &cfg);
        assert_eq!(cs, ConfigChangeset::default());
        assert!(!cs.system_prompt_only);
    }

    #[test]
    fn test_classify_system_prompt_only() {
        let old = base_config();
        let mut new = old.clone();
        new.system_prompt = "New prompt.".into();
        let cs = classify_config_change(&old, &new);
        assert!(cs.system_prompt_only);
        assert!(!cs.adapters_changed);
        assert!(!cs.schedules_changed);
        assert!(!cs.sub_agents_changed);
    }

    #[test]
    fn test_classify_telegram_routes_changed() {
        let old = base_config();
        let mut new = old.clone();
        new.telegram = Some(TelegramRoutesConfig {
            routes: vec![TelegramRoute {
                chat_id: 12345,
                mention_only: false,
                name: None,
                route_to: None,
            }],
        });
        let cs = classify_config_change(&old, &new);
        assert!(cs.adapters_changed);
        assert!(!cs.system_prompt_only);
        assert!(!cs.schedules_changed);
    }

    #[test]
    fn test_classify_schedules_changed() {
        let old = base_config();
        let mut new = old.clone();
        new.schedules = vec![ScheduleDef {
            cron: "0 9 * * *".into(),
            target: "agent:dev".into(),
            action: ScheduleAction::Raw,
            config: None,
            timezone: None,
        }];
        let cs = classify_config_change(&old, &new);
        assert!(cs.schedules_changed);
        assert!(!cs.adapters_changed);
        assert!(!cs.system_prompt_only);
    }

    #[test]
    fn test_classify_system_prompt_and_adapters_not_prompt_only() {
        let old = base_config();
        let mut new = old.clone();
        new.system_prompt = "Updated.".into();
        new.telegram = Some(TelegramRoutesConfig { routes: vec![] });
        let cs = classify_config_change(&old, &new);
        assert!(
            !cs.system_prompt_only,
            "both changed — should not be prompt-only"
        );
        assert!(cs.adapters_changed);
    }
}
