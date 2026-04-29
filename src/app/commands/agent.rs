//! `deskd agent` subcommand handlers.

use anyhow::Result;
use tracing::info;

use crate::app::cli::AgentAction;
use crate::app::{agent, tasklog, unified_inbox, worker};
use crate::config;

use super::{format_relative_time, parse_duration_secs, truncate};

pub async fn handle(action: AgentAction) -> Result<()> {
    match action {
        AgentAction::Create {
            name,
            prompt,
            model,
            workdir,
            max_turns,
            unix_user,
            budget_usd,
            command,
        } => {
            let cfg = agent::AgentConfig {
                name,
                model,
                system_prompt: prompt.unwrap_or_default(),
                work_dir: workdir.unwrap_or_else(|| ".".into()),
                max_turns,
                unix_user,
                budget_usd,
                command: if command.is_empty() {
                    vec!["claude".to_string()]
                } else {
                    command
                },
                config_path: None,
                container: None,
                session: crate::domain::config_types::ConfigSessionMode::default(),
                runtime: crate::domain::config_types::ConfigAgentRuntime::default(),
                kind: crate::domain::config_types::ConfigAgentKind::default(),
                context: None,
                compact_threshold: None,
                auto_compact_threshold_tokens: None,
            };
            let state = agent::create(&cfg).await?;
            println!("Agent {} created", state.config.name);
        }
        AgentAction::Send {
            name,
            message,
            max_turns,
            socket,
        } => {
            let effective_socket = if let Some(ref s) = socket {
                if std::path::Path::new(s).exists() {
                    socket
                } else {
                    None
                }
            } else {
                // Try agent state file first, then serve state.
                agent::load_state(&name)
                    .ok()
                    .and_then(|s| {
                        let bus = config::agent_bus_socket(&s.config.work_dir);
                        if std::path::Path::new(&bus).exists() {
                            Some(bus)
                        } else {
                            None
                        }
                    })
                    .or_else(|| {
                        config::ServeState::load().and_then(|state| {
                            state.agent(&name).and_then(|a| {
                                if std::path::Path::new(&a.bus_socket).exists() {
                                    Some(a.bus_socket.clone())
                                } else {
                                    None
                                }
                            })
                        })
                    })
            };

            if let Some(sock) = effective_socket {
                let target = format!("agent:{}", name);
                worker::send_via_bus(&sock, "cli", &target, &message, max_turns).await?;
            } else {
                let response = agent::send(&name, &message, max_turns, None).await?;
                println!("{}", response);
            }
        }
        AgentAction::Run {
            name,
            socket,
            subscribe,
        } => {
            agent::load_state(&name)?;
            let subs = if subscribe.is_empty() {
                None
            } else {
                Some(subscribe)
            };
            let task_store = crate::app::task::TaskStore::default_for_home();
            info!(agent = %name, "starting worker");
            tokio::select! {
                result = worker::run(&name, &socket, Some(socket.clone()), subs, &task_store) => { result?; }
                _ = tokio::signal::ctrl_c() => {
                    info!(agent = %name, "shutting down");
                }
            }
        }
        AgentAction::List { socket } => {
            let agents = agent::list().await?;
            // Query all known bus sockets for live agents.
            let mut live = crate::app::serve::query_live_agents(&socket)
                .await
                .unwrap_or_default();
            // Also check sockets from serve state (per-agent buses).
            if let Some(state) = config::ServeState::load() {
                for agent_state in state.agents.values() {
                    if agent_state.bus_socket != socket
                        && let Ok(more) =
                            crate::app::serve::query_live_agents(&agent_state.bus_socket).await
                    {
                        live.extend(more);
                    }
                }
            }
            // Also check internal buses for sub-agents: each parent agent may
            // have an internal bus at /tmp/deskd-{parent}-internal.sock.
            let parent_names: std::collections::HashSet<String> =
                agents.iter().filter_map(|a| a.parent.clone()).collect();
            for parent in &parent_names {
                let internal_sock = format!("/tmp/deskd-{}-internal.sock", parent);
                if let Ok(more) = crate::app::serve::query_live_agents(&internal_sock).await {
                    live.extend(more);
                }
            }

            if agents.is_empty() {
                println!("No agents registered");
            } else {
                println!(
                    "{:<15} {:<14} {:<8} {:<10} {:<12} MODEL",
                    "NAME", "STATUS", "TURNS", "COST", "USER"
                );
                let thresholds = crate::app::doctor::DoctorThresholds::default();
                for a in &agents {
                    let domain = agent::to_domain_agent(a, &live);
                    // Run the same heuristic the `doctor` command uses so the
                    // STATUS column is honest (no more `ready` for hung agents
                    // — see #422).
                    let verdict = super::doctor::diagnose_one(
                        &a.config.name,
                        thresholds.empty_completion_threshold.max(3) * 3,
                        &thresholds,
                    )
                    .map(|(v, _, _)| v)
                    .ok();
                    let status_str = format_list_status(verdict.as_ref(), &domain, a);
                    println!(
                        "{:<15} {:<14} {:<8} ${:<9.2} {:<12} {}",
                        domain.name,
                        status_str,
                        a.total_turns,
                        a.total_cost,
                        a.config.unix_user.as_deref().unwrap_or("-"),
                        domain.capabilities.model,
                    );
                }
            }
        }
        AgentAction::Stats { name } => {
            let s = agent::load_state(&name)?;
            println!("Agent:      {}", s.config.name);
            println!("Model:      {}", s.config.model);
            println!(
                "Unix user:  {}",
                s.config.unix_user.as_deref().unwrap_or("-")
            );
            println!("Work dir:   {}", s.config.work_dir);
            println!(
                "Bus:        {}",
                config::agent_bus_socket(&s.config.work_dir)
            );
            println!(
                "Config:     {}",
                s.config.config_path.as_deref().unwrap_or("-")
            );
            println!("Total turns:{}", s.total_turns);
            println!("Total cost: ${:.4}", s.total_cost);
            if s.config.budget_usd > 0.0 {
                println!("Budget:     ${:.2}", s.config.budget_usd);
            } else {
                println!("Budget:     unlimited");
            }
            println!(
                "Session:    {}",
                if s.session_id.is_empty() {
                    "-"
                } else {
                    &s.session_id
                }
            );
            println!("Created:    {}", s.created_at);
        }
        AgentAction::Read {
            name,
            clear: _,
            follow,
        } => {
            let inbox_name = format!("replies/{}", name);
            // If invoked from inside an agent's context (DESKD_AGENT_NAME set),
            // enforce inbox_acl from that agent's deskd.yaml. When invoked from
            // a human shell (no env), the operator is trusted (file permissions
            // remain the OS-level guard).
            if let Ok(caller) = std::env::var("DESKD_AGENT_NAME") {
                let cfg_path = std::env::var("DESKD_AGENT_CONFIG").ok();
                let cfg = cfg_path
                    .as_deref()
                    .and_then(|p| crate::config::UserConfig::load(p).ok());
                if !crate::app::mcp_tools::inbox_access_allowed(&caller, &inbox_name, cfg.as_ref())
                    && !crate::app::mcp_tools::inbox_access_allowed(&caller, &name, cfg.as_ref())
                {
                    anyhow::bail!(
                        "inbox access denied: agent \"{}\" is not in allow-list for inbox \"{}\"",
                        caller,
                        name
                    );
                }
            }
            let entries = unified_inbox::read_messages(&inbox_name, 100, None)?;
            if entries.is_empty() && !follow {
                println!("No messages for {}", name);
            } else {
                for entry in &entries {
                    print_inbox_message(entry);
                }
            }
            if follow {
                let mut last_ts = entries.last().map(|m| m.ts);
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    let newer = unified_inbox::read_messages(&inbox_name, 100, last_ts)?;
                    for entry in &newer {
                        print_inbox_message(entry);
                        last_ts = Some(entry.ts);
                    }
                }
            }
        }
        AgentAction::Tasks { name, limit } => {
            let show_all = name == "all";

            let mut filtered: Vec<unified_inbox::InboxMessage> = if show_all {
                let inboxes = unified_inbox::list_inboxes()?;
                let mut all = Vec::new();
                for (inbox_name, _) in &inboxes {
                    if inbox_name.starts_with("replies/") {
                        let msgs = unified_inbox::read_messages(inbox_name, 1000, None)?;
                        all.extend(msgs.into_iter().filter(|m| {
                            m.metadata.get("type").and_then(|v| v.as_str()) == Some("task_result")
                        }));
                    }
                }
                all
            } else {
                let inbox_name = format!("replies/{}", name);
                let msgs = unified_inbox::read_messages(&inbox_name, 1000, None)?;
                msgs.into_iter()
                    .filter(|m| {
                        m.metadata.get("type").and_then(|v| v.as_str()) == Some("task_result")
                    })
                    .collect()
            };

            filtered.sort_by_key(|b| std::cmp::Reverse(b.ts));
            filtered.truncate(limit);

            if filtered.is_empty() {
                if show_all {
                    println!("No completed tasks found");
                } else {
                    println!("No completed tasks for {}", name);
                }
            } else {
                println!("COMPLETED ({}):", filtered.len());
                let now = chrono::Utc::now();
                for entry in &filtered {
                    let dur = now.signed_duration_since(entry.ts);
                    let age = format_relative_time(dur);
                    let agent = entry
                        .metadata
                        .get("agent")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let task_text = entry
                        .metadata
                        .get("task")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let task_excerpt = truncate(task_text, 36);
                    let has_error = entry
                        .metadata
                        .get("has_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let status = if has_error { "err" } else { "done" };
                    let msg_id = entry
                        .metadata
                        .get("message_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let id_short = if msg_id.len() > 6 {
                        &msg_id[..6]
                    } else {
                        msg_id
                    };
                    if show_all {
                        println!(
                            "  {:<8} {:<12} from:{:<6} {:38} {} {} ago",
                            id_short,
                            agent,
                            entry.source,
                            format!("\"{}\"", task_excerpt),
                            status,
                            age,
                        );
                    } else {
                        println!(
                            "  {:<8} from:{:<6} {:38} {} {} ago",
                            id_short,
                            entry.source,
                            format!("\"{}\"", task_excerpt),
                            status,
                            age,
                        );
                    }
                }
            }
        }
        AgentAction::Logs {
            name,
            limit,
            source,
            since,
            json,
            cost,
            by_pr,
        } => {
            let since_dt = if let Some(ref dur) = since {
                let secs = parse_duration_secs(dur)?;
                Some(chrono::Utc::now() - chrono::Duration::seconds(secs as i64))
            } else {
                None
            };

            let entries = tasklog::read_logs(
                &name,
                if cost || by_pr { usize::MAX } else { limit },
                source.as_deref(),
                since_dt,
            )?;

            if entries.is_empty() {
                println!("No task logs for {}", name);
            } else if by_pr {
                tasklog::print_pr_summary(&name, &entries, since.as_deref());
            } else if cost {
                tasklog::print_cost_summary(&name, &entries, since.as_deref());
            } else if json {
                tasklog::print_json(&entries);
            } else {
                tasklog::print_table(&entries);
            }
        }
        AgentAction::Status { name } => match name {
            Some(name) => {
                let s = agent::load_state(&name)?;
                let pid_alive =
                    s.pid > 0 && std::path::Path::new(&format!("/proc/{}", s.pid)).exists();
                // For sub-agents on an internal bus, also check the internal bus socket.
                let on_internal_bus = if let Some(ref parent) = s.parent {
                    let internal_sock = format!("/tmp/deskd-{}-internal.sock", parent);
                    crate::app::serve::query_live_agents(&internal_sock)
                        .await
                        .map(|live| live.contains(&s.config.name))
                        .unwrap_or(false)
                } else {
                    false
                };
                let status_str = if pid_alive || on_internal_bus {
                    s.status.as_str()
                } else {
                    "offline"
                };
                let is_sub_agent = s.parent.is_some();
                println!("Agent:       {}", s.config.name);
                if is_sub_agent {
                    println!("Type:        sub-agent");
                }
                println!("Status:      {}", status_str);
                println!(
                    "PID:         {}",
                    if pid_alive {
                        s.pid.to_string()
                    } else {
                        "-".into()
                    }
                );
                println!("Model:       {}", s.config.model);
                println!("Turns:       {}", s.total_turns);
                println!("Cost:        ${:.4}", s.total_cost);
                if s.config.budget_usd > 0.0 {
                    println!("Budget:      ${:.2}", s.config.budget_usd);
                } else {
                    println!("Budget:      unlimited");
                }
                println!("Created:     {}", s.created_at);
                println!("Work dir:    {}", s.config.work_dir);
                if !s.current_task.is_empty() {
                    println!("Current:     {}", truncate(&s.current_task, 60));
                }
                if let Some(ref parent) = s.parent {
                    println!("Parent:      {}", parent);
                }
                // Show stderr tail if available.
                let stderr_path = agent::stderr_log_path(&name);
                if stderr_path.exists() {
                    let content = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                    let lines: Vec<&str> = content.lines().collect();
                    if !lines.is_empty() {
                        let start = lines.len().saturating_sub(5);
                        println!("Stderr (last {}):", lines.len().min(5));
                        for line in &lines[start..] {
                            println!("  {}", line);
                        }
                    }
                }
            }
            None => {
                let agents = agent::list().await?;
                if agents.is_empty() {
                    println!("No agents registered");
                } else {
                    println!(
                        "{:<15} {:<12} {:<6} {:<10} {:<20} MODEL",
                        "NAME", "STATUS", "TURNS", "COST", "CREATED"
                    );
                    println!("{}", "─".repeat(78));
                    for a in &agents {
                        let pid_alive =
                            a.pid > 0 && std::path::Path::new(&format!("/proc/{}", a.pid)).exists();
                        // Sub-agents: treat as alive if their PID is alive even if
                        // we can't reach the internal bus from the CLI.
                        let status = if pid_alive {
                            if a.parent.is_some() {
                                format!("{}[sub]", a.status)
                            } else {
                                a.status.clone()
                            }
                        } else if a.parent.is_some() {
                            "offline[sub]".to_string()
                        } else {
                            "offline".to_string()
                        };
                        let created = if a.created_at.len() > 19 {
                            &a.created_at[..19]
                        } else {
                            &a.created_at
                        };
                        println!(
                            "{:<15} {:<12} {:<6} ${:<9.4} {:<20} {}",
                            a.config.name,
                            status,
                            a.total_turns,
                            a.total_cost,
                            created,
                            a.config.model,
                        );
                    }
                }
            }
        },
        AgentAction::Stderr { name, tail, follow } => {
            let path = agent::stderr_log_path(&name);
            if !path.exists() {
                println!("No stderr logs for {}", name);
                return Ok(());
            }
            let content = std::fs::read_to_string(&path)?;
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(tail);
            for line in &lines[start..] {
                println!("{}", line);
            }
            if follow {
                let mut pos = std::fs::metadata(&path)?.len();
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(meta) = std::fs::metadata(&path) {
                        let new_len = meta.len();
                        if new_len > pos {
                            let mut f = std::fs::File::open(&path)?;
                            use std::io::{Read, Seek, SeekFrom};
                            f.seek(SeekFrom::Start(pos))?;
                            let mut buf = String::new();
                            f.read_to_string(&mut buf)?;
                            print!("{}", buf);
                            pos = new_len;
                        }
                    }
                }
            }
        }
        AgentAction::Stream {
            name,
            tail,
            follow,
            raw,
        } => {
            let path = agent::stream_log_path(&name);
            if !path.exists() {
                println!("No stream log for {}", name);
                return Ok(());
            }
            let content = std::fs::read_to_string(&path)?;
            let lines: Vec<&str> = content.lines().collect();
            let start = lines.len().saturating_sub(tail);
            for line in &lines[start..] {
                if raw {
                    println!("{}", line);
                } else {
                    print_stream_event(line);
                }
            }
            if follow {
                let mut pos = std::fs::metadata(&path)?.len();
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if let Ok(meta) = std::fs::metadata(&path) {
                        let new_len = meta.len();
                        if new_len > pos {
                            let mut f = std::fs::File::open(&path)?;
                            use std::io::{Read, Seek, SeekFrom};
                            f.seek(SeekFrom::Start(pos))?;
                            let mut buf = String::new();
                            f.read_to_string(&mut buf)?;
                            for line in buf.lines() {
                                if raw {
                                    println!("{}", line);
                                } else {
                                    print_stream_event(line);
                                }
                            }
                            pos = new_len;
                        }
                    }
                }
            }
        }
        AgentAction::Rm { name } => {
            agent::remove(&name).await?;
            println!("Agent {} removed", name);
        }
        AgentAction::Set {
            name,
            container,
            config: config_opt,
        } => {
            let workspace_path = resolve_workspace_path(config_opt)?;
            let Some(profile) = container else {
                anyhow::bail!(
                    "`deskd agent set` requires at least one field to update (e.g. --container)"
                );
            };
            set_agent_container(&workspace_path, &name, &profile)?;
            println!(
                "Updated agent '{}' container profile to '{}' in {}",
                name, profile, workspace_path
            );
            println!(
                "Run `deskd restart` (or restart the agent's process) for the change to take effect."
            );
        }
        AgentAction::Doctor {
            name,
            last,
            empty_threshold,
            idle_minutes,
            stuck_minutes,
        } => {
            super::doctor::handle(name, last, empty_threshold, idle_minutes, stuck_minutes).await?;
        }
        AgentAction::Restart {
            name,
            all,
            fresh_session,
            timeout,
        } => {
            return restart_agents(name, all, fresh_session, timeout).await;
        }
        AgentAction::Spawn {
            name,
            task,
            socket,
            work_dir,
            model,
            max_turns,
        } => {
            let bus_socket = socket
                .or_else(|| std::env::var("DESKD_BUS_SOCKET").ok())
                .ok_or_else(|| {
                    anyhow::anyhow!("No bus socket: pass --socket or set DESKD_BUS_SOCKET")
                })?;

            let parent = std::env::var("DESKD_AGENT_NAME").unwrap_or_else(|_| "unknown".into());
            let resolved_work_dir = work_dir.unwrap_or_else(|| ".".into());

            let response = agent::spawn_ephemeral(
                &name,
                &task,
                &model,
                &resolved_work_dir,
                max_turns,
                &bus_socket,
                &parent,
            )
            .await?;

            println!("{}", response);
        }
    }
    Ok(())
}

/// Format the STATUS column for `agent list`, honoring the doctor verdict
/// when it surfaces a real problem.
///
/// Falls back to the original `ready / busy / unhealthy` labels when the
/// verdict is `Healthy` so existing consumers (TUI, scripts) keep working.
fn format_list_status(
    verdict: Option<&crate::app::doctor::Verdict>,
    domain: &crate::domain::agent::Agent,
    state: &agent::AgentState,
) -> String {
    use crate::app::doctor::Verdict;
    let suffix = if state.parent.is_some() { "[sub]" } else { "" };
    if let Some(v) = verdict {
        match v {
            Verdict::Hung { .. } => return format!("🔴 hung{}", suffix),
            Verdict::Stuck { .. } => return format!("🔴 stuck{}", suffix),
            Verdict::Dead { .. } => return format!("🔴 dead{}", suffix),
            Verdict::Idle { .. } => return format!("🟡 idle{}", suffix),
            Verdict::Healthy { .. } => {} // fall through to base label
        }
    }
    match &domain.status {
        crate::domain::agent::AgentStatus::Ready => format!("ready{}", suffix),
        crate::domain::agent::AgentStatus::Busy { .. } => format!("busy{}", suffix),
        crate::domain::agent::AgentStatus::Unhealthy { .. } => "unhealthy".to_string(),
    }
}

/// Restart one or all agents and wait for them to return to ready.
///
/// Implementation:
/// 1. Resolve target agent list (single named, or all registered).
/// 2. For each: clear session_id (when --fresh-session) directly in the state
///    file BEFORE killing, then SIGTERM the claude worker child PID. The
///    supervisor (worker loop in `deskd serve`) respawns claude on the next
///    task — so for the agent to fully return to `idle` we just wait for the
///    state file's `status` field.
/// 3. Poll each agent's state until `idle` (a.k.a. ready) within --timeout.
/// 4. Exit code is non-zero if any agent fails to return to ready in time.
async fn restart_agents(
    name: Option<String>,
    all: bool,
    fresh_session: bool,
    timeout_secs: u64,
) -> Result<()> {
    if all && name.is_some() {
        anyhow::bail!("pass either <name> or --all, not both");
    }
    if !all && name.is_none() {
        anyhow::bail!("missing agent name (or pass --all)");
    }

    let targets: Vec<String> = if all {
        let agents = agent::list().await?;
        if agents.is_empty() {
            println!("No agents registered");
            return Ok(());
        }
        agents.into_iter().map(|s| s.config.name).collect()
    } else {
        vec![name.unwrap()]
    };

    let mut had_failure = false;
    for target in &targets {
        match restart_one_agent(target, fresh_session, timeout_secs).await {
            Ok(()) => {}
            Err(e) => {
                eprintln!("agent restart {}: {}", target, e);
                had_failure = true;
            }
        }
    }

    if had_failure {
        anyhow::bail!("one or more agents failed to return to ready");
    }
    Ok(())
}

/// Restart a single agent and poll for `idle` within `timeout_secs`.
async fn restart_one_agent(name: &str, fresh_session: bool, timeout_secs: u64) -> Result<()> {
    use std::time::Duration;

    let state = agent::load_state(name)
        .map_err(|e| anyhow::anyhow!("failed to load agent state for '{}': {}", name, e))?;
    let pid = state.pid;

    // --fresh-session: clear session_id BEFORE respawn so claude does not
    // pass --resume. Done first so a race where the worker respawns claude
    // before we get here can't pick up the stale session_id.
    if fresh_session {
        let mut s = state.clone();
        s.session_id.clear();
        s.session_cost = 0.0;
        s.session_turns = 0;
        s.session_start = None;
        agent::save_state_pub(&s)?;
        info!(agent = %name, "cleared session_id (--fresh-session)");
    }

    if pid > 0 && std::path::Path::new(&format!("/proc/{}", pid)).exists() {
        crate::app::mcp_service::stop_agent_process(name, pid).await;
        println!("agent {}: sent SIGTERM to pid {}", name, pid);
    } else {
        // Already stopped — restart still succeeds (no-op kill, supervisor
        // will respawn on next task).
        println!("agent {}: already stopped (no live pid)", name);
    }

    // Mark restarting so external observers see a transition.
    if let Ok(mut s) = agent::load_state(name) {
        s.status = "restarting".to_string();
        let _ = agent::save_state_pub(&s);
    }

    // Poll the state file for status=idle (ready). The worker loop sets
    // status to "idle" via set_idle() once it has finished its current task
    // and is waiting on the bus.
    let deadline = std::time::Instant::now() + Duration::from_secs(timeout_secs);
    loop {
        if let Ok(s) = agent::load_state(name) {
            // "idle" or "ready" both count as ready. "working" / "restarting"
            // do not.
            if s.status == "idle" || s.status == "ready" {
                println!("agent {}: ready", name);
                return Ok(());
            }
        }
        if std::time::Instant::now() >= deadline {
            anyhow::bail!(
                "agent '{}' did not return to ready within {}s",
                name,
                timeout_secs
            );
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Parse a stream-json line and print a human-readable summary.
fn print_stream_event(line: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return;
    };
    match v.get("type").and_then(|t| t.as_str()) {
        Some("assistant") => {
            if let Some(blocks) = v
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array())
            {
                for block in blocks {
                    match block.get("type").and_then(|t| t.as_str()) {
                        Some("text") => {
                            if let Some(text) = block.get("text").and_then(|t| t.as_str())
                                && !text.is_empty()
                            {
                                println!("[assistant] {}", truncate(text, 200));
                            }
                        }
                        Some("tool_use") => {
                            let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                            let input = block
                                .get("input")
                                .map(|i| {
                                    let s = i.to_string();
                                    truncate(&s, 120).to_string()
                                })
                                .unwrap_or_default();
                            println!("[tool_use] {} {}", name, input);
                        }
                        _ => {}
                    }
                }
            }
        }
        Some("tool") | Some("tool_result") => {
            let tool_id = v
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|b| b.get("text"))
                .and_then(|t| t.as_str())
                .unwrap_or("");
            let is_error = v
                .get("content")
                .and_then(|c| c.as_array())
                .and_then(|a| a.first())
                .and_then(|b| b.get("is_error"))
                .and_then(|e| e.as_bool())
                .unwrap_or(false);
            if is_error {
                println!("[tool_error] {}", truncate(tool_id, 200));
            } else if !tool_id.is_empty() {
                println!("[tool_result] {}", truncate(tool_id, 200));
            }
        }
        Some("result") => {
            let cost = v
                .get("total_cost_usd")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.0);
            let turns = v.get("num_turns").and_then(|t| t.as_u64()).unwrap_or(0);
            println!("[result] turns={} cost=${:.4}", turns, cost);
        }
        Some(other) => {
            println!("[{}]", other);
        }
        None => {}
    }
}

fn print_inbox_message(msg: &unified_inbox::InboxMessage) {
    let ts = msg.ts.format("%Y-%m-%dT%H:%M:%S").to_string();
    let from = msg.from.as_deref().unwrap_or(&msg.source);
    println!("─── {} [{}] ───", from, ts);
    println!("{}", msg.text);
    println!();
}

/// Resolve the workspace.yaml path for `deskd agent set`:
/// explicit flag > running serve state > error.
fn resolve_workspace_path(explicit: Option<String>) -> Result<String> {
    if let Some(path) = explicit {
        return Ok(path);
    }
    if let Some(state) = config::ServeState::load() {
        return Ok(state.workspace_config);
    }
    anyhow::bail!("no --config provided and deskd serve is not running (no serve state found)")
}

/// Edit `workspace.yaml` in place: set the named agent's `container:` field
/// to the string `profile_name`. Validates that the profile is defined under
/// the top-level `containers:` map and that the agent exists.
pub(crate) fn set_agent_container(
    workspace_path: &str,
    agent_name: &str,
    profile_name: &str,
) -> Result<()> {
    let raw = std::fs::read_to_string(workspace_path).map_err(|e| {
        anyhow::anyhow!("failed to read workspace config {}: {}", workspace_path, e)
    })?;
    let mut doc: serde_yaml::Value = serde_yaml::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("failed to parse workspace yaml: {}", e))?;

    // Validate profile exists in top-level containers map.
    let profile_keys: Vec<String> = doc
        .get("containers")
        .and_then(|v| v.as_mapping())
        .map(|m| {
            m.keys()
                .filter_map(|k| k.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    if !profile_keys.iter().any(|k| k == profile_name) {
        let available = if profile_keys.is_empty() {
            "<none defined>".to_string()
        } else {
            profile_keys.join(", ")
        };
        anyhow::bail!(
            "container profile '{}' not defined (available: {})",
            profile_name,
            available
        );
    }

    // Find the named agent in agents[] and overwrite its `container:` field
    // with the profile name string.
    let agents = doc
        .get_mut("agents")
        .and_then(|v| v.as_sequence_mut())
        .ok_or_else(|| anyhow::anyhow!("workspace yaml has no `agents:` list"))?;

    let mut updated = false;
    for agent in agents.iter_mut() {
        let name_matches = agent
            .get("name")
            .and_then(|v| v.as_str())
            .map(|n| n == agent_name)
            .unwrap_or(false);
        if !name_matches {
            continue;
        }
        let mapping = agent
            .as_mapping_mut()
            .ok_or_else(|| anyhow::anyhow!("agent entry is not a mapping"))?;
        mapping.insert(
            serde_yaml::Value::String("container".to_string()),
            serde_yaml::Value::String(profile_name.to_string()),
        );
        updated = true;
        break;
    }
    if !updated {
        anyhow::bail!(
            "agent '{}' not found in {} (check workspace.yaml `agents:` list)",
            agent_name,
            workspace_path
        );
    }

    let serialized = serde_yaml::to_string(&doc)
        .map_err(|e| anyhow::anyhow!("failed to re-serialize workspace yaml: {}", e))?;
    std::fs::write(workspace_path, serialized).map_err(|e| {
        anyhow::anyhow!("failed to write workspace config {}: {}", workspace_path, e)
    })?;
    info!(
        agent = agent_name,
        profile = profile_name,
        path = workspace_path,
        "updated container profile"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a unique temp dir under std::env::temp_dir() (no tempfile crate).
    /// Caller is responsible for cleanup; tests below remove it via `drop_dir`.
    struct TmpDir(std::path::PathBuf);
    impl TmpDir {
        fn new(tag: &str) -> Self {
            let id = uuid::Uuid::new_v4().to_string();
            let p = std::env::temp_dir().join(format!("deskd-set-{}-{}", tag, id));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write_tmp(path: &std::path::Path, content: &str) {
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn set_agent_container_updates_string_ref() {
        let tmp = TmpDir::new("string-ref");
        let path = tmp.path().join("workspace.yaml");
        let yaml = r#"containers:
  personal:
    image: claude-personal
  work:
    image: claude-work
agents:
  - name: uagent
    work_dir: /home/uagent
    container: personal
"#;
        write_tmp(&path, yaml);
        set_agent_container(path.to_str().unwrap(), "uagent", "work").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&after).unwrap();
        let container = doc["agents"][0]["container"].as_str().unwrap();
        assert_eq!(container, "work");
    }

    #[test]
    fn set_agent_container_replaces_inline_with_named_ref() {
        let tmp = TmpDir::new("inline-replace");
        let path = tmp.path().join("workspace.yaml");
        let yaml = r#"containers:
  work:
    image: claude-work
agents:
  - name: uagent
    work_dir: /home/uagent
    container:
      image: inline-image
      env:
        FOO: bar
"#;
        write_tmp(&path, yaml);
        set_agent_container(path.to_str().unwrap(), "uagent", "work").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        let doc: serde_yaml::Value = serde_yaml::from_str(&after).unwrap();
        // After update, container is a string reference.
        let container = doc["agents"][0]["container"].as_str().unwrap();
        assert_eq!(container, "work");
    }

    #[test]
    fn set_agent_container_unknown_profile_errors() {
        let tmp = TmpDir::new("unknown-profile");
        let path = tmp.path().join("workspace.yaml");
        let yaml = r#"containers:
  personal:
    image: claude-personal
agents:
  - name: uagent
    work_dir: /home/uagent
    container: personal
"#;
        write_tmp(&path, yaml);
        let err = set_agent_container(path.to_str().unwrap(), "uagent", "ghost").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ghost"));
        assert!(msg.contains("personal"));
    }

    #[test]
    fn set_agent_container_unknown_agent_errors() {
        let tmp = TmpDir::new("unknown-agent");
        let path = tmp.path().join("workspace.yaml");
        let yaml = r#"containers:
  work:
    image: claude-work
agents:
  - name: uagent
    work_dir: /home/uagent
    container: work
"#;
        write_tmp(&path, yaml);
        let err = set_agent_container(path.to_str().unwrap(), "missing", "work").unwrap_err();
        assert!(err.to_string().contains("missing"));
    }

    #[test]
    fn set_agent_container_no_profiles_section_errors() {
        let tmp = TmpDir::new("no-profiles");
        let path = tmp.path().join("workspace.yaml");
        let yaml = r#"agents:
  - name: uagent
    work_dir: /home/uagent
    container:
      image: foo
"#;
        write_tmp(&path, yaml);
        let err = set_agent_container(path.to_str().unwrap(), "uagent", "work").unwrap_err();
        assert!(err.to_string().contains("not defined"));
    }
}
