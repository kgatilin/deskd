//! CLI handler for `deskd a2a` subcommands.

use anyhow::Result;

use crate::app::a2a;
use crate::app::cli::A2aAction;
use crate::config::WorkspaceConfig;

pub fn handle(action: A2aAction, config_path: &str) -> Result<()> {
    match action {
        A2aAction::AgentCard { .. } => {
            let workspace = WorkspaceConfig::load(config_path)?;
            let card = a2a::build_agent_card(&workspace)?;
            let json = serde_json::to_string_pretty(&card)?;
            println!("{json}");
            Ok(())
        }
    }
}
