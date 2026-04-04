use anyhow::{Context, Result};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::app::message::Message;
use crate::app::statemachine;
use crate::app::worker;
use crate::domain::statemachine::ModelDef;

type Writer = std::sync::Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>;

/// Send a single message to a bus target.
///
/// Connects to the bus, registers as `<source>-mcp`, sends the message, and
/// disconnects. This is the canonical one-shot send helper used by MCP
/// handlers that need to emit bus messages without maintaining a persistent
/// connection.
pub async fn send_bus_message(
    bus_socket: &str,
    source: &str,
    target: &str,
    payload: Value,
    metadata: Value,
) -> Result<()> {
    let mut stream = UnixStream::connect(bus_socket)
        .await
        .with_context(|| format!("failed to connect to bus at {}", bus_socket))?;

    let reg = serde_json::json!({
        "type": "register",
        "name": format!("{}-mcp", source),
        "subscriptions": []
    });
    let mut line = serde_json::to_string(&reg)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let msg = serde_json::json!({
        "type": "message",
        "id": Uuid::new_v4().to_string(),
        "source": source,
        "target": target,
        "payload": payload,
        "metadata": metadata,
    });
    let mut msg_line = serde_json::to_string(&msg)?;
    msg_line.push('\n');
    stream.write_all(msg_line.as_bytes()).await?;
    stream.flush().await?;

    Ok(())
}

/// Dispatch the initial task for a newly created SM instance.
///
/// If the instance has no assignee (empty string), this is a no-op.
/// Otherwise, sends a task message to the assignee with instance context.
pub async fn dispatch_sm_task(
    bus_socket: &str,
    inst: &crate::domain::statemachine::Instance,
    source: &str,
) -> Result<()> {
    if inst.assignee.is_empty() {
        return Ok(());
    }

    let task_text = format!(
        "---\n## Task: {}\n\n{}\n\n---\n## Metadata\ninstance_id: {}\nmodel: {}\nstate: {}",
        inst.title, inst.body, inst.id, inst.model, inst.state
    );

    let mut stream = UnixStream::connect(bus_socket)
        .await
        .with_context(|| format!("failed to connect to bus at {}", bus_socket))?;

    let reg = serde_json::json!({
        "type": "register",
        "name": format!("{}-mcp-sm", source),
        "subscriptions": []
    });
    let mut line = serde_json::to_string(&reg)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let msg = serde_json::json!({
        "type": "message",
        "id": Uuid::new_v4().to_string(),
        "source": "workflow-engine",
        "target": &inst.assignee,
        "payload": {
            "task": task_text,
            "sm_instance_id": inst.id,
        },
        "reply_to": format!("sm:{}", inst.id),
        "metadata": {"priority": 5u8},
    });
    let mut msg_line = serde_json::to_string(&msg)?;
    msg_line.push('\n');
    stream.write_all(msg_line.as_bytes()).await?;
    stream.flush().await?;

    info!(instance = %inst.id, assignee = %inst.assignee, "dispatched initial task");
    Ok(())
}

/// Notify the workflow engine that an SM instance was moved.
///
/// Connects to the bus, sends an `action: "moved"` message to `sm:<id>`,
/// and disconnects. The workflow engine picks this up and dispatches if needed.
pub async fn notify_moved(bus_socket: &str, instance_id: &str, source: &str) -> Result<()> {
    let mut stream = UnixStream::connect(bus_socket).await?;

    let reg = serde_json::json!({
        "type": "register",
        "name": format!("{}-sm-notify", source),
        "subscriptions": []
    });
    let mut line = serde_json::to_string(&reg)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let msg = serde_json::json!({
        "type": "message",
        "id": Uuid::new_v4().to_string(),
        "source": source,
        "target": format!("sm:{}", instance_id),
        "payload": {"action": "moved"},
        "metadata": {"priority": 5u8},
    });
    let mut msg_line = serde_json::to_string(&msg)?;
    msg_line.push('\n');
    stream.write_all(msg_line.as_bytes()).await?;
    stream.flush().await?;

    Ok(())
}

/// Run the workflow engine on the given bus.
/// Registers as `workflow-engine`, subscribes to `sm:*`.
/// On startup, dispatches pending instances. Then listens for completion messages.
pub async fn run(socket_path: &str, models: Vec<ModelDef>) -> Result<()> {
    let store = statemachine::StateMachineStore::default_for_home();

    let stream =
        worker::bus_connect(socket_path, "workflow-engine", vec!["sm:*".to_string()]).await?;

    let (reader, writer_half) = stream.into_split();

    let writer: Writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer_half));
    let mut lines = BufReader::new(reader).lines();

    info!("workflow engine started, subscribed to sm:*");

    // On startup: check for pending instances and dispatch them.
    dispatch_pending(&writer, &models, &store).await;

    // Main loop: listen for completion messages.
    while let Some(line) = lines.next_line().await? {
        if line.is_empty() {
            continue;
        }

        let msg: Message = match serde_json::from_str::<crate::infra::dto::BusMessage>(&line) {
            Ok(dto) => dto.into(),
            Err(_) => continue,
        };

        // Extract instance ID from target: "sm:sm-a1b2c3d4"
        let instance_id = match msg.target.strip_prefix("sm:") {
            Some(id) => id.to_string(),
            None => continue,
        };

        // Check if this is a move notification (from CLI/MCP sm_move).
        if msg.payload.get("action").and_then(|a| a.as_str()) == Some("moved") {
            info!(instance = %instance_id, "received move notification");
            if let Err(e) = handle_move_notification(&writer, &models, &store, &instance_id).await {
                warn!(instance = %instance_id, error = %e, "failed to handle move notification");
            }
            continue;
        }

        // Extract result from payload.
        let result = msg
            .payload
            .get("result")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .to_string();

        let error = msg
            .payload
            .get("error")
            .and_then(|e| e.as_str())
            .map(|s| s.to_string());

        info!(instance = %instance_id, "received completion");

        if let Err(e) = handle_completion(
            &writer,
            &models,
            &store,
            &instance_id,
            &result,
            error.as_deref(),
        )
        .await
        {
            warn!(instance = %instance_id, error = %e, "failed to process completion");
        }
    }

    Ok(())
}

/// Handle a task completion: apply transitions and dispatch next step.
async fn handle_completion(
    writer: &Writer,
    models: &[ModelDef],
    store: &statemachine::StateMachineStore,
    instance_id: &str,
    result: &str,
    error: Option<&str>,
) -> Result<()> {
    let mut inst = store.load(instance_id)?;
    let model = models
        .iter()
        .find(|m| m.name == inst.model)
        .ok_or_else(|| anyhow::anyhow!("model '{}' not found", inst.model))?;

    // Store result.
    inst.result = Some(result.to_string());
    if let Some(err) = error {
        inst.error = Some(err.to_string());
    }
    inst.updated_at = chrono::Utc::now().to_rfc3339();
    store.save(&inst)?;

    // Find matching transition.
    let current_state = inst.state.clone();
    let target_state = find_next_state(model, &current_state, result);

    if let Some(target) = target_state {
        store.move_to(&mut inst, model, &target, "auto", Some(result), None, None)?;

        info!(
            instance = %instance_id,
            from = %current_state,
            to = %target,
            "transition applied"
        );

        // If not terminal and has assignee, dispatch.
        if !statemachine::is_terminal(model, &inst) {
            dispatch_instance(writer, model, &inst).await?;
        }
    } else {
        info!(instance = %instance_id, state = %inst.state, "no matching transition, awaiting manual move");
    }

    Ok(())
}

/// Handle a move notification: reload instance and dispatch if it has an assignee.
async fn handle_move_notification(
    writer: &Writer,
    models: &[ModelDef],
    store: &statemachine::StateMachineStore,
    instance_id: &str,
) -> Result<()> {
    let inst = store.load(instance_id)?;
    let model = models
        .iter()
        .find(|m| m.name == inst.model)
        .ok_or_else(|| anyhow::anyhow!("model '{}' not found", inst.model))?;

    if statemachine::is_terminal(model, &inst) {
        info!(instance = %instance_id, state = %inst.state, "moved to terminal state, no dispatch");
        return Ok(());
    }

    if !inst.assignee.is_empty() {
        dispatch_instance(writer, model, &inst).await?;
        info!(instance = %instance_id, assignee = %inst.assignee, "dispatched after move");
    }

    Ok(())
}

/// Find the next state based on transition rules.
/// Priority: 1) keyword match (`on:`), 2) auto trigger, 3) none.
fn find_next_state(model: &ModelDef, current_state: &str, result: &str) -> Option<String> {
    let transitions = statemachine::valid_transitions(model, current_state);
    let result_upper = result.trim().to_uppercase();

    // 1. Check keyword matches first.
    for t in &transitions {
        if let Some(ref keyword) = t.on
            && result_upper.starts_with(&keyword.to_uppercase())
        {
            return Some(t.to.clone());
        }
    }

    // 2. Check auto triggers.
    for t in &transitions {
        if t.trigger.as_deref() == Some("auto") {
            return Some(t.to.clone());
        }
    }

    None
}

/// Dispatch a task to the instance's current assignee.
async fn dispatch_instance(
    writer: &Writer,
    model: &ModelDef,
    inst: &statemachine::Instance,
) -> Result<()> {
    if inst.assignee.is_empty() {
        debug!(instance = %inst.id, state = %inst.state, "no assignee, skipping dispatch");
        return Ok(());
    }

    // Find the prompt for the transition that leads to the current state.
    let prompt = inst
        .history
        .last()
        .and_then(|h| {
            model
                .transitions
                .iter()
                .find(|t| t.to == inst.state && (t.from == h.from || t.from == "*"))
        })
        .and_then(|t| t.prompt.as_ref())
        .cloned()
        .unwrap_or_default();

    // Check if this is a human step.
    let transition_def = model.transitions.iter().find(|t| t.to == inst.state);
    let is_human = transition_def
        .and_then(|t| t.step_type.as_deref())
        .map(|s| s == "human")
        == Some(true);

    if is_human {
        // Send notification instead of dispatching to agent.
        if let Some(notify_target) = transition_def.and_then(|t| t.notify.as_ref()) {
            let task_text = build_task_text(&prompt, inst);
            let msg = serde_json::json!({
                "type": "message",
                "id": Uuid::new_v4().to_string(),
                "source": "workflow-engine",
                "target": notify_target,
                "payload": {
                    "task": task_text,
                    "sm_instance_id": inst.id,
                },
                "reply_to": format!("sm:{}", inst.id),
                "metadata": {"priority": 5u8},
            });
            let mut line = serde_json::to_string(&msg)?;
            line.push('\n');
            let mut w = writer.lock().await;
            w.write_all(line.as_bytes()).await?;
            info!(instance = %inst.id, target = %notify_target, "human notification sent");
        }
        return Ok(());
    }

    // Build task text with prompt injection.
    let task_text = build_task_text(&prompt, inst);

    // Check if the transition has task queue criteria — dispatch via queue if so.
    let transition_criteria = transition_def.and_then(|t| t.criteria.clone());

    if let Some(criteria) = transition_criteria {
        // Dispatch via task queue (pull-based).
        let store = crate::app::task::TaskStore::default_for_home();
        let task = store.create_for_sm(&task_text, criteria, "workflow-engine", &inst.id)?;
        info!(
            instance = %inst.id,
            task_id = %task.id,
            "task created in queue for SM dispatch"
        );
        return Ok(());
    }

    // Fallback: direct bus message dispatch (no criteria = legacy behavior).
    let msg = serde_json::json!({
        "type": "message",
        "id": Uuid::new_v4().to_string(),
        "source": "workflow-engine",
        "target": &inst.assignee,
        "payload": {
            "task": task_text,
            "sm_instance_id": inst.id,
        },
        "reply_to": format!("sm:{}", inst.id),
        "metadata": {"priority": 5u8},
    });

    let mut line = serde_json::to_string(&msg)?;
    line.push('\n');
    let mut w = writer.lock().await;
    w.write_all(line.as_bytes()).await?;

    info!(instance = %inst.id, assignee = %inst.assignee, "task dispatched via bus");
    Ok(())
}

/// Build the full task text with prompt and context.
fn build_task_text(prompt: &str, inst: &statemachine::Instance) -> String {
    let mut parts = Vec::new();

    if !prompt.is_empty() {
        parts.push(prompt.to_string());
    }

    parts.push(format!("---\n## Task: {}\n\n{}", inst.title, inst.body));

    if let Some(ref result) = inst.result
        && !result.is_empty()
    {
        parts.push(format!("---\n## Previous step result\n\n{}", result));
    }

    let mut meta_lines = format!(
        "---\n## Metadata\ninstance_id: {}\nmodel: {}\nstate: {}",
        inst.id, inst.model, inst.state
    );
    if !inst.metadata.is_null()
        && let Ok(pretty) = serde_json::to_string_pretty(&inst.metadata)
    {
        meta_lines.push_str(&format!("\nmetadata: {}", pretty));
    }
    parts.push(meta_lines);

    parts.join("\n\n")
}

/// On startup, find non-terminal instances and dispatch them.
///
/// Crash recovery: if an instance has a result but hasn't transitioned yet
/// (deskd crashed after agent returned result but before SM transition),
/// apply the transition now instead of re-dispatching.
async fn dispatch_pending(
    writer: &Writer,
    models: &[ModelDef],
    store: &statemachine::StateMachineStore,
) {
    let instances = match store.list_all() {
        Ok(list) => list,
        Err(_) => return,
    };

    // Collect SM instance IDs that already have an active task-queue entry,
    // so we don't re-dispatch if a task is already in flight.
    let active_sm_ids: std::collections::HashSet<String> = {
        let task_store = crate::app::task::TaskStore::default_for_home();
        task_store
            .list(Some(crate::app::task::TaskStatus::Active))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|t| t.sm_instance_id)
            .collect()
    };

    for inst in instances {
        let model = match models.iter().find(|m| m.name == inst.model) {
            Some(m) => m,
            None => continue,
        };

        // Skip terminal instances.
        if statemachine::is_terminal(model, &inst) {
            continue;
        }

        // Crash recovery: result present but SM not yet transitioned.
        // This happens when deskd crashes after the agent returns a result
        // but before handle_completion runs the state transition.
        if let Some(ref result) = inst.result
            && !result.is_empty()
        {
            info!(
                instance = %inst.id,
                state = %inst.state,
                "recovery: result present but not transitioned, applying transition"
            );
            let mut inst_mut = inst.clone();
            if let Err(e) =
                handle_completion_sync(writer, models, store, &mut inst_mut, result).await
            {
                warn!(instance = %inst.id, error = %e, "recovery: failed to apply transition from existing result");
            }
            continue;
        }

        // Idempotency: skip dispatch if a task-queue task is already active
        // for this SM instance (re-dispatch would cause duplicate work).
        if active_sm_ids.contains(&inst.id) {
            info!(
                instance = %inst.id,
                "idempotency: active task already exists for instance, skipping dispatch"
            );
            continue;
        }

        // Dispatch if has assignee (pending work).
        if !inst.assignee.is_empty()
            && let Err(e) = dispatch_instance(writer, model, &inst).await
        {
            warn!(instance = %inst.id, error = %e, "failed to dispatch pending instance");
        }
    }
}

/// Apply the transition for an instance that already has a result stored
/// (used during crash recovery in dispatch_pending).
async fn handle_completion_sync(
    writer: &Writer,
    models: &[ModelDef],
    store: &statemachine::StateMachineStore,
    inst: &mut statemachine::Instance,
    result: &str,
) -> anyhow::Result<()> {
    let model = models
        .iter()
        .find(|m| m.name == inst.model)
        .ok_or_else(|| anyhow::anyhow!("model '{}' not found", inst.model))?;

    let current_state = inst.state.clone();
    let target_state = find_next_state(model, &current_state, result);

    if let Some(target) = target_state {
        store.move_to(inst, model, &target, "recovery", Some(result), None, None)?;
        info!(
            instance = %inst.id,
            from = %current_state,
            to = %target,
            "recovery: transition applied from stored result"
        );
        if !statemachine::is_terminal(model, inst) {
            dispatch_instance(writer, model, inst).await?;
        }
    } else {
        info!(
            instance = %inst.id,
            state = %current_state,
            "recovery: no matching transition for stored result, awaiting manual move"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::statemachine::TransitionDef;

    fn test_model() -> ModelDef {
        ModelDef {
            name: "pipeline".into(),
            description: "Test pipeline".into(),
            states: vec![
                "draft".into(),
                "review".into(),
                "approved".into(),
                "rejected".into(),
            ],
            initial: "draft".into(),
            terminal: vec!["approved".into(), "rejected".into()],
            transitions: vec![
                TransitionDef {
                    from: "draft".into(),
                    to: "review".into(),
                    trigger: Some("auto".into()),
                    on: None,
                    assignee: Some("agent:reviewer".into()),
                    prompt: Some("Review this.".into()),
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                    criteria: None,
                },
                TransitionDef {
                    from: "review".into(),
                    to: "approved".into(),
                    trigger: None,
                    on: Some("LGTM".into()),
                    assignee: None,
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                    criteria: None,
                },
                TransitionDef {
                    from: "review".into(),
                    to: "rejected".into(),
                    trigger: None,
                    on: Some("REJECT".into()),
                    assignee: None,
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                    criteria: None,
                },
            ],
        }
    }

    #[test]
    fn test_find_next_state_keyword_match() {
        let model = test_model();
        // "LGTM looks good" starts with "LGTM" -> approved
        assert_eq!(
            find_next_state(&model, "review", "LGTM looks good"),
            Some("approved".into())
        );
        // Case-insensitive
        assert_eq!(
            find_next_state(&model, "review", "lgtm"),
            Some("approved".into())
        );
    }

    #[test]
    fn test_find_next_state_reject_keyword() {
        let model = test_model();
        assert_eq!(
            find_next_state(&model, "review", "REJECT: needs work"),
            Some("rejected".into())
        );
    }

    #[test]
    fn test_find_next_state_auto_trigger() {
        let model = test_model();
        // From "draft" there's an auto trigger to "review"
        assert_eq!(
            find_next_state(&model, "draft", "anything"),
            Some("review".into())
        );
    }

    #[test]
    fn test_find_next_state_no_match() {
        let model = test_model();
        // From "review" with result that doesn't match any keyword and no auto trigger
        assert_eq!(find_next_state(&model, "review", "not sure"), None);
    }

    #[test]
    fn test_find_next_state_keyword_priority_over_auto() {
        // Keywords should be checked before auto triggers
        let model = ModelDef {
            name: "test".into(),
            description: String::new(),
            states: vec!["a".into(), "b".into(), "c".into()],
            initial: "a".into(),
            terminal: vec!["c".into()],
            transitions: vec![
                TransitionDef {
                    from: "a".into(),
                    to: "b".into(),
                    trigger: Some("auto".into()),
                    on: None,
                    assignee: None,
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                    criteria: None,
                },
                TransitionDef {
                    from: "a".into(),
                    to: "c".into(),
                    trigger: None,
                    on: Some("DONE".into()),
                    assignee: None,
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                    criteria: None,
                },
            ],
        };
        // Keyword match should take priority over auto
        assert_eq!(find_next_state(&model, "a", "DONE"), Some("c".into()));
        // No keyword match -> auto
        assert_eq!(
            find_next_state(&model, "a", "something else"),
            Some("b".into())
        );
    }

    #[test]
    fn test_build_task_text_basic() {
        let inst = statemachine::Instance {
            id: "sm-test123".into(),
            model: "pipeline".into(),
            title: "Fix bug".into(),
            body: "Details here".into(),
            state: "review".into(),
            assignee: "agent:reviewer".into(),
            result: None,
            error: None,
            created_by: "kira".into(),
            created_at: String::new(),
            updated_at: String::new(),
            history: vec![],
            metadata: serde_json::Value::Null,
            total_cost: 0.0,
            total_turns: 0,
        };
        let text = build_task_text("Review this code.", &inst);
        assert!(text.contains("Review this code."));
        assert!(text.contains("Fix bug"));
        assert!(text.contains("Details here"));
        assert!(text.contains("sm-test123"));
    }

    #[test]
    fn test_build_task_text_with_previous_result() {
        let inst = statemachine::Instance {
            id: "sm-test456".into(),
            model: "pipeline".into(),
            title: "Task".into(),
            body: String::new(),
            state: "review".into(),
            assignee: "agent:reviewer".into(),
            result: Some("Previous output here".into()),
            error: None,
            created_by: "kira".into(),
            created_at: String::new(),
            updated_at: String::new(),
            history: vec![],
            metadata: serde_json::Value::Null,
            total_cost: 0.0,
            total_turns: 0,
        };
        let text = build_task_text("", &inst);
        assert!(text.contains("Previous output here"));
    }

    fn queue_dispatch_model() -> ModelDef {
        ModelDef {
            name: "feature".into(),
            description: "Feature pipeline with queue dispatch".into(),
            states: vec![
                "backlog".into(),
                "planning".into(),
                "implementing".into(),
                "done".into(),
            ],
            initial: "backlog".into(),
            terminal: vec!["done".into()],
            transitions: vec![
                TransitionDef {
                    from: "backlog".into(),
                    to: "planning".into(),
                    trigger: Some("start".into()),
                    on: None,
                    assignee: Some("agent:planner".into()),
                    prompt: Some("Create a plan.".into()),
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                    criteria: Some(crate::app::task::TaskCriteria {
                        model: Some("claude-sonnet-4-6".into()),
                        labels: vec!["planning".into()],
                    }),
                },
                TransitionDef {
                    from: "planning".into(),
                    to: "implementing".into(),
                    trigger: Some("auto".into()),
                    on: None,
                    assignee: Some("agent:dev".into()),
                    prompt: Some("Implement the plan.".into()),
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                    criteria: Some(crate::app::task::TaskCriteria {
                        model: Some("claude-sonnet-4-6".into()),
                        labels: vec![],
                    }),
                },
                TransitionDef {
                    from: "implementing".into(),
                    to: "done".into(),
                    trigger: Some("auto".into()),
                    on: None,
                    assignee: None,
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                    criteria: None,
                },
            ],
        }
    }

    #[tokio::test]
    async fn test_sm_dispatch_creates_task_in_queue() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sm_dir = std::path::PathBuf::from(format!("/tmp/deskd_sm_queue_test_{}", ts));
        let task_dir = std::path::PathBuf::from(format!("/tmp/deskd_task_queue_test_{}", ts));

        let sm_store = statemachine::StateMachineStore::new(sm_dir.clone());
        let task_store = crate::app::task::TaskStore::new(task_dir.clone());
        let model = queue_dispatch_model();

        // Create SM instance.
        let inst = sm_store
            .create(&model, "FEAT-100", "Build widget", "kira")
            .unwrap();
        assert_eq!(inst.state, "backlog");

        // Move to planning — this transition has criteria.
        let mut inst = inst;
        sm_store
            .move_to(&mut inst, &model, "planning", "start", None, None, None)
            .unwrap();
        assert_eq!(inst.state, "planning");
        assert_eq!(inst.assignee, "agent:planner");

        // Simulate dispatch — create task via TaskStore (same as dispatch_instance with criteria).
        let transition = model
            .transitions
            .iter()
            .find(|t| t.to == "planning")
            .unwrap();
        let criteria = transition.criteria.clone().unwrap();
        let task_text = build_task_text(transition.prompt.as_deref().unwrap_or(""), &inst);
        let task = task_store
            .create_for_sm(&task_text, criteria, "workflow-engine", &inst.id)
            .unwrap();

        // Verify task was created with correct sm_instance_id.
        assert_eq!(task.sm_instance_id.as_deref(), Some(inst.id.as_str()));
        assert_eq!(task.status, crate::app::task::TaskStatus::Pending);
        assert!(task.description.contains("Create a plan."));
        assert!(task.description.contains("FEAT-100"));

        // Verify task list shows the SM-linked task.
        let all = task_store.list(None).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].sm_instance_id.as_deref(), Some(inst.id.as_str()));

        // Simulate completion — handle_completion applies auto transition.
        inst.result = Some("Plan: implement X, Y, Z".into());
        inst.updated_at = chrono::Utc::now().to_rfc3339();
        sm_store.save(&inst).unwrap();

        let target = find_next_state(&model, "planning", "Plan: implement X, Y, Z");
        assert_eq!(target, Some("implementing".into()));

        // Apply the transition.
        sm_store
            .move_to(
                &mut inst,
                &model,
                "implementing",
                "auto",
                Some("Plan: implement X, Y, Z"),
                None,
                None,
            )
            .unwrap();
        assert_eq!(inst.state, "implementing");

        // The implementing transition also has criteria — would create another task.
        let impl_transition = model
            .transitions
            .iter()
            .find(|t| t.to == "implementing")
            .unwrap();
        assert!(impl_transition.criteria.is_some());

        let task2 = task_store
            .create_for_sm(
                &build_task_text(impl_transition.prompt.as_deref().unwrap_or(""), &inst),
                impl_transition.criteria.clone().unwrap(),
                "workflow-engine",
                &inst.id,
            )
            .unwrap();
        assert_eq!(task2.sm_instance_id.as_deref(), Some(inst.id.as_str()));
        assert!(task2.description.contains("Implement the plan."));

        // After implementing, auto goes to done (terminal).
        let final_target = find_next_state(&model, "implementing", "Done: all implemented");
        assert_eq!(final_target, Some("done".into()));
        sm_store
            .move_to(&mut inst, &model, "done", "auto", None, None, None)
            .unwrap();
        assert!(statemachine::is_terminal(&model, &inst));

        // Cleanup.
        let _ = std::fs::remove_dir_all(&sm_dir);
        let _ = std::fs::remove_dir_all(&task_dir);
    }

    #[test]
    fn test_dispatch_without_criteria_uses_bus() {
        // Transitions without criteria should NOT use task queue.
        let model = test_model();
        let transition = model.transitions.iter().find(|t| t.to == "review").unwrap();
        assert!(transition.criteria.is_none());
    }

    // ─── Crash recovery tests ────────────────────────────────────────────────

    /// handle_completion_sync applies the correct transition from a stored result.
    #[tokio::test]
    async fn test_handle_completion_sync_applies_transition() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sm_dir = std::path::PathBuf::from(format!("/tmp/deskd_recovery_test_{}", ts));
        let store = statemachine::StateMachineStore::new(sm_dir.clone());
        let models = vec![test_model()];

        // Create instance in "draft" state with a result already stored (simulating the crash gap).
        let mut inst = store
            .create(&models[0], "Recovery Test", "body", "kira")
            .unwrap();
        inst.result = Some("anything".into());
        inst.updated_at = chrono::Utc::now().to_rfc3339();
        store.save(&inst).unwrap();
        assert_eq!(inst.state, "draft");

        // Connect a minimal writer (we won't be writing to it in this test but need the type).
        // Use a socketpair to satisfy the Writer type without a real bus.
        let (a, _b) = tokio::net::UnixStream::pair().unwrap();
        let (_reader, writer_half) = a.into_split();
        let writer: Writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer_half));

        handle_completion_sync(&writer, &models, &store, &mut inst, "anything")
            .await
            .unwrap();

        // "draft" + any result -> auto trigger -> "review"
        let reloaded = store.load(&inst.id).unwrap();
        assert_eq!(reloaded.state, "review");
        assert_eq!(reloaded.history.last().unwrap().trigger, "recovery");

        let _ = std::fs::remove_dir_all(&sm_dir);
    }

    /// handle_completion_sync does nothing when no matching transition exists.
    #[tokio::test]
    async fn test_handle_completion_sync_no_matching_transition() {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let sm_dir = std::path::PathBuf::from(format!("/tmp/deskd_recovery_no_trans_test_{}", ts));
        let store = statemachine::StateMachineStore::new(sm_dir.clone());
        let models = vec![test_model()];

        // Put instance in "review" with a non-matching result (neither LGTM nor REJECT).
        let mut inst = store
            .create(&models[0], "No Transition Test", "body", "kira")
            .unwrap();
        store
            .move_to(&mut inst, &models[0], "review", "manual", None, None, None)
            .unwrap();
        inst.result = Some("not sure yet".into());
        store.save(&inst).unwrap();

        let (a, _b) = tokio::net::UnixStream::pair().unwrap();
        let (_reader, writer_half) = a.into_split();
        let writer: Writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer_half));

        handle_completion_sync(&writer, &models, &store, &mut inst, "not sure yet")
            .await
            .unwrap();

        // State should remain "review" — no valid transition for "not sure yet".
        let reloaded = store.load(&inst.id).unwrap();
        assert_eq!(reloaded.state, "review");

        let _ = std::fs::remove_dir_all(&sm_dir);
    }
}
