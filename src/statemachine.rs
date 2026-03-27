use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing::info;

use crate::config::{ModelDef, TransitionDef};

/// An instance of a state machine model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    pub id: String,
    pub model: String,
    pub title: String,
    #[serde(default)]
    pub body: String,
    pub state: String,
    pub assignee: String,
    #[serde(default)]
    pub result: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub history: Vec<Transition>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// A recorded transition in the instance history.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Transition {
    pub from: String,
    pub to: String,
    pub trigger: String,
    pub timestamp: String,
    #[serde(default)]
    pub note: Option<String>,
}

fn instances_dir() -> PathBuf {
    let dir = if let Ok(custom) = std::env::var("DESKD_INSTANCES_DIR") {
        PathBuf::from(custom)
    } else {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        PathBuf::from(home).join(".deskd").join("instances")
    };
    std::fs::create_dir_all(&dir).ok();
    dir
}

fn instance_path(id: &str) -> PathBuf {
    instances_dir().join(format!("{id}.json"))
}

pub fn save(inst: &Instance) -> Result<()> {
    let path = instance_path(&inst.id);
    let tmp = path.with_extension("tmp");
    let content = serde_json::to_string_pretty(inst)?;
    std::fs::write(&tmp, &content)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

pub fn load(id: &str) -> Result<Instance> {
    let path = instance_path(id);
    let content =
        std::fs::read_to_string(&path).with_context(|| format!("Instance '{id}' not found"))?;
    let inst: Instance = serde_json::from_str(&content)?;
    Ok(inst)
}

pub fn list_all() -> Result<Vec<Instance>> {
    let dir = instances_dir();
    let mut instances = Vec::new();
    if dir.exists() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false)
                && let Ok(content) = std::fs::read_to_string(&path)
                && let Ok(inst) = serde_json::from_str::<Instance>(&content)
            {
                instances.push(inst);
            }
        }
    }
    instances.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(instances)
}

#[allow(dead_code)]
pub fn delete(id: &str) -> Result<()> {
    let path = instance_path(id);
    std::fs::remove_file(&path).with_context(|| format!("Instance '{id}' not found"))?;
    Ok(())
}

/// Create a new instance from a model definition.
pub fn create(model: &ModelDef, title: &str, body: &str, created_by: &str) -> Result<Instance> {
    let id = format!("sm-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let now = Utc::now().to_rfc3339();

    // Find assignee for initial state (from first matching transition).
    let assignee = model
        .transitions
        .iter()
        .find(|t| t.from == model.initial || t.from == "*")
        .and_then(|t| t.assignee.clone())
        .unwrap_or_default();

    let inst = Instance {
        id,
        model: model.name.clone(),
        title: title.to_string(),
        body: body.to_string(),
        state: model.initial.clone(),
        assignee,
        result: None,
        error: None,
        created_by: created_by.to_string(),
        created_at: now.clone(),
        updated_at: now,
        history: Vec::new(),
        metadata: serde_json::Value::Null,
    };
    save(&inst)?;
    info!(id = %inst.id, model = %inst.model, state = %inst.state, "instance created");
    Ok(inst)
}

/// Find valid transitions from the current state.
pub fn valid_transitions<'a>(model: &'a ModelDef, current_state: &str) -> Vec<&'a TransitionDef> {
    model
        .transitions
        .iter()
        .filter(|t| t.from == current_state || t.from == "*")
        .collect()
}

/// Move an instance to a new state. Validates that the transition is allowed.
pub fn move_to(
    inst: &mut Instance,
    model: &ModelDef,
    target_state: &str,
    trigger: &str,
    note: Option<&str>,
) -> Result<()> {
    // Validate target state exists.
    if !model.states.contains(&target_state.to_string()) {
        bail!(
            "State '{}' not defined in model '{}'",
            target_state,
            model.name
        );
    }

    // Validate transition exists from current state.
    let from_state = inst.state.clone();
    let transition_def = model
        .transitions
        .iter()
        .find(|t| (t.from == from_state || t.from == "*") && t.to == target_state);

    if transition_def.is_none() {
        bail!(
            "No transition from '{}' to '{}' in model '{}'",
            from_state,
            target_state,
            model.name
        );
    }

    let now = Utc::now().to_rfc3339();
    let transition = Transition {
        from: from_state.clone(),
        to: target_state.to_string(),
        trigger: trigger.to_string(),
        timestamp: now.clone(),
        note: note.map(|s| s.to_string()),
    };

    inst.history.push(transition);
    inst.state = target_state.to_string();
    inst.updated_at = now;

    // Update assignee from the transition definition (looked up BEFORE state mutation).
    if let Some(td) = transition_def
        && let Some(ref a) = td.assignee
    {
        inst.assignee = a.clone();
    }

    save(inst)?;
    info!(id = %inst.id, from = %from_state, to = %target_state, "state transition");
    Ok(())
}

/// Check if an instance is in a terminal state.
pub fn is_terminal(model: &ModelDef, inst: &Instance) -> bool {
    model.terminal.contains(&inst.state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ModelDef, TransitionDef};

    /// Set up a unique temp directory for instance storage during a test.
    /// Returns a guard that cleans up on drop.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join("deskd-test").join(format!(
                "{}-{}",
                name,
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            unsafe {
                std::env::set_var("DESKD_INSTANCES_DIR", &dir);
            }
            TestDir(dir)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).ok();
            unsafe {
                std::env::remove_var("DESKD_INSTANCES_DIR");
            }
        }
    }

    fn test_model() -> ModelDef {
        ModelDef {
            name: "review".into(),
            description: "Code review workflow".into(),
            states: vec![
                "open".into(),
                "in_review".into(),
                "approved".into(),
                "rejected".into(),
            ],
            initial: "open".into(),
            terminal: vec!["approved".into(), "rejected".into()],
            transitions: vec![
                TransitionDef {
                    from: "open".into(),
                    to: "in_review".into(),
                    trigger: None,
                    on: None,
                    assignee: Some("agent:reviewer".into()),
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                },
                TransitionDef {
                    from: "in_review".into(),
                    to: "approved".into(),
                    trigger: None,
                    on: Some("approve".into()),
                    assignee: None,
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                },
                TransitionDef {
                    from: "in_review".into(),
                    to: "rejected".into(),
                    trigger: None,
                    on: Some("reject".into()),
                    assignee: None,
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                },
                TransitionDef {
                    from: "*".into(),
                    to: "rejected".into(),
                    trigger: Some("cancel".into()),
                    on: None,
                    assignee: None,
                    prompt: None,
                    step_type: None,
                    notify: None,
                    timeout: None,
                    timeout_goto: None,
                },
            ],
        }
    }

    #[test]
    fn test_create_instance() {
        let _dir = TestDir::new("create");
        let model = test_model();
        let inst = create(&model, "Fix bug #42", "Details here", "kira").unwrap();
        assert!(inst.id.starts_with("sm-"));
        assert_eq!(inst.model, "review");
        assert_eq!(inst.state, "open");
        assert_eq!(inst.assignee, "agent:reviewer");
        assert!(inst.history.is_empty());
        // Clean up.
        delete(&inst.id).ok();
    }

    #[test]
    fn test_move_to_valid() {
        let _dir = TestDir::new("move_valid");
        let model = test_model();
        let mut inst = create(&model, "Test move", "", "kira").unwrap();
        assert_eq!(inst.state, "open");

        move_to(&mut inst, &model, "in_review", "manual", None).unwrap();
        assert_eq!(inst.state, "in_review");
        assert_eq!(inst.history.len(), 1);
        assert_eq!(inst.history[0].from, "open");
        assert_eq!(inst.history[0].to, "in_review");

        move_to(
            &mut inst,
            &model,
            "approved",
            "keyword:approve",
            Some("LGTM"),
        )
        .unwrap();
        assert_eq!(inst.state, "approved");
        assert_eq!(inst.history.len(), 2);
        assert!(is_terminal(&model, &inst));
        // Clean up.
        delete(&inst.id).ok();
    }

    #[test]
    fn test_move_to_invalid_transition() {
        let _dir = TestDir::new("move_invalid");
        let model = test_model();
        let mut inst = create(&model, "Test invalid", "", "kira").unwrap();
        // Cannot go directly from open to approved.
        let result = move_to(&mut inst, &model, "approved", "manual", None);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No transition from")
        );
        delete(&inst.id).ok();
    }

    #[test]
    fn test_move_to_invalid_state() {
        let _dir = TestDir::new("move_bad_state");
        let model = test_model();
        let mut inst = create(&model, "Test bad state", "", "kira").unwrap();
        let result = move_to(&mut inst, &model, "nonexistent", "manual", None);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not defined"));
        delete(&inst.id).ok();
    }

    #[test]
    fn test_valid_transitions() {
        let model = test_model();
        // From "open": direct transition to in_review + wildcard to rejected.
        let transitions = valid_transitions(&model, "open");
        let targets: Vec<&str> = transitions.iter().map(|t| t.to.as_str()).collect();
        assert!(targets.contains(&"in_review"));
        assert!(targets.contains(&"rejected")); // from "*"
    }

    #[test]
    fn test_wildcard_cancel() {
        let _dir = TestDir::new("wildcard_cancel");
        let model = test_model();
        let mut inst = create(&model, "Test cancel", "", "kira").unwrap();
        // Wildcard transition allows cancel from any state.
        move_to(&mut inst, &model, "rejected", "cancel", Some("Cancelled")).unwrap();
        assert_eq!(inst.state, "rejected");
        assert!(is_terminal(&model, &inst));
        delete(&inst.id).ok();
    }

    #[test]
    fn test_save_load_roundtrip() {
        let _dir = TestDir::new("roundtrip");
        let model = test_model();
        let inst = create(&model, "Roundtrip test", "body text", "kira").unwrap();
        let loaded = load(&inst.id).unwrap();
        assert_eq!(loaded.id, inst.id);
        assert_eq!(loaded.title, "Roundtrip test");
        assert_eq!(loaded.body, "body text");
        delete(&inst.id).ok();
    }

    #[test]
    fn test_is_terminal() {
        let model = test_model();
        let inst_open = Instance {
            id: "test".into(),
            model: "review".into(),
            title: "t".into(),
            body: String::new(),
            state: "open".into(),
            assignee: String::new(),
            result: None,
            error: None,
            created_by: "test".into(),
            created_at: String::new(),
            updated_at: String::new(),
            history: vec![],
            metadata: serde_json::Value::Null,
        };
        assert!(!is_terminal(&model, &inst_open));

        let inst_approved = Instance {
            state: "approved".into(),
            ..inst_open
        };
        assert!(is_terminal(&model, &inst_approved));
    }
}
