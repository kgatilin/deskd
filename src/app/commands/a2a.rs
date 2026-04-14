//! CLI handler for `deskd a2a` subcommands.

use std::sync::Arc;

use anyhow::Result;

use crate::app::cli::A2aAction;
use crate::app::{a2a, a2a_server};
use crate::config::WorkspaceConfig;

pub async fn handle(action: A2aAction, config_path: &str) -> Result<()> {
    match action {
        A2aAction::AgentCard { .. } => {
            let workspace = WorkspaceConfig::load(config_path)?;
            let card = a2a::build_agent_card(&workspace)?;
            let json = serde_json::to_string_pretty(&card)?;
            println!("{json}");
            Ok(())
        }
        A2aAction::Serve { listen, .. } => {
            let workspace = WorkspaceConfig::load(config_path)?;
            let card = a2a::build_agent_card(&workspace)?;
            let a2a_cfg = workspace
                .a2a
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("workspace.yaml has no `a2a:` section"))?;

            let listen_addr = listen.as_deref().unwrap_or(&a2a_cfg.listen);

            // Find a bus socket from serve state or workspace agents.
            let bus_socket = crate::config::ServeState::load()
                .and_then(|s| s.any_bus_socket().map(String::from))
                .or_else(|| workspace.agents.first().map(|a| a.bus_socket()))
                .ok_or_else(|| anyhow::anyhow!("no bus socket found — is deskd serve running?"))?;

            let state = Arc::new(a2a_server::A2aState {
                agent_card: card,
                api_key: a2a_cfg.api_key.clone(),
                bus_socket,
            });

            a2a_server::serve(listen_addr, state).await
        }
    }
}
