//! State machine domain types.
//!
//! Pure data types — no I/O, no persistence logic.
//! Serde lives on infra DTOs (infra::dto), not here.

/// A state machine model definition.
#[derive(Debug, Clone)]
pub struct ModelDef {
    pub name: String,
    pub description: String,
    pub states: Vec<String>,
    pub initial: String,
    pub terminal: Vec<String>,
    pub transitions: Vec<TransitionDef>,
}

/// The type of execution for a state machine transition step.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum StepType {
    /// Full agent execution (default).
    #[default]
    Agent,
    /// Deterministic shell command — exit 0 = pass, non-zero = fail.
    Check,
    /// Lightweight LLM review with structured output.
    Validate,
    /// Wait for human input (manual transition via `sm move`).
    Human,
}

impl StepType {
    /// Parse from a string, returning an error for unknown values.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "agent" => Ok(Self::Agent),
            "check" => Ok(Self::Check),
            "validate" => Ok(Self::Validate),
            "human" => Ok(Self::Human),
            other => Err(format!("unknown step_type: {other:?}")),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Check => "check",
            Self::Validate => "validate",
            Self::Human => "human",
        }
    }
}

impl std::fmt::Display for StepType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A transition between states in a model.
#[derive(Debug, Clone)]
pub struct TransitionDef {
    pub from: String,
    pub to: String,
    pub trigger: Option<String>,
    pub on: Option<String>,
    pub assignee: Option<String>,
    pub prompt: Option<String>,
    pub step_type: StepType,
    /// Shell command to execute for `Check` steps.
    pub command: Option<String>,
    pub notify: Option<String>,
    pub timeout: Option<String>,
    pub timeout_goto: Option<String>,
    /// Task queue criteria for this transition (model, labels).
    /// When set, dispatch creates a task in the queue instead of direct bus message.
    pub criteria: Option<crate::domain::task::TaskCriteria>,
    /// Maximum number of retries for tasks dispatched by this transition (default 0).
    pub max_retries: u32,
}

/// An instance of a state machine model.
#[derive(Debug, Clone)]
pub struct Instance {
    pub id: String,
    pub model: String,
    pub title: String,
    pub body: String,
    pub state: String,
    pub assignee: String,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    pub history: Vec<Transition>,
    pub metadata: serde_json::Value,
    /// Cumulative cost across all transitions.
    pub total_cost: f64,
    /// Cumulative turns across all transitions.
    pub total_turns: u32,
    /// Task IDs owned by this instance (current and historical).
    pub task_ids: Vec<String>,
}

impl Instance {
    /// Record a task as owned by this instance.
    pub fn record_task(&mut self, task_id: &str) {
        if !self.task_ids.contains(&task_id.to_string()) {
            self.task_ids.push(task_id.to_string());
        }
    }

    /// Get the current (most recently dispatched) task ID, if any.
    pub fn current_task_id(&self) -> Option<&str> {
        self.history.last().and_then(|h| h.task_id.as_deref())
    }
}

/// A recorded transition in the instance history.
#[derive(Debug, Clone)]
pub struct Transition {
    pub from: String,
    pub to: String,
    pub trigger: String,
    pub timestamp: String,
    pub note: Option<String>,
    /// Cost in USD for the step that triggered this transition.
    pub cost_usd: Option<f64>,
    /// Number of turns for the step.
    pub turns: Option<u32>,
    /// Task ID created for this transition's dispatched step.
    pub task_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_step_type_parse_valid() {
        assert_eq!(StepType::parse("agent").unwrap(), StepType::Agent);
        assert_eq!(StepType::parse("check").unwrap(), StepType::Check);
        assert_eq!(StepType::parse("validate").unwrap(), StepType::Validate);
        assert_eq!(StepType::parse("human").unwrap(), StepType::Human);
    }

    #[test]
    fn test_step_type_parse_invalid() {
        assert!(StepType::parse("chekc").is_err());
        assert!(StepType::parse("").is_err());
        assert!(StepType::parse("Agent").is_err());
    }

    #[test]
    fn test_step_type_default_is_agent() {
        assert_eq!(StepType::default(), StepType::Agent);
    }

    #[test]
    fn test_step_type_display() {
        assert_eq!(StepType::Agent.to_string(), "agent");
        assert_eq!(StepType::Check.to_string(), "check");
        assert_eq!(StepType::Validate.to_string(), "validate");
        assert_eq!(StepType::Human.to_string(), "human");
    }

    fn make_test_instance() -> Instance {
        Instance {
            id: "sm-test".into(),
            model: "test".into(),
            title: "Test".into(),
            body: String::new(),
            state: "open".into(),
            assignee: "dev".into(),
            result: None,
            error: None,
            created_by: "test".into(),
            created_at: String::new(),
            updated_at: String::new(),
            history: vec![Transition {
                from: "new".into(),
                to: "open".into(),
                trigger: "auto".into(),
                timestamp: String::new(),
                note: None,
                cost_usd: None,
                turns: None,
                task_id: Some("task-abc".into()),
            }],
            metadata: serde_json::Value::Null,
            total_cost: 0.0,
            total_turns: 0,
            task_ids: Vec::new(),
        }
    }

    #[test]
    fn test_record_task() {
        let mut inst = make_test_instance();
        assert!(inst.task_ids.is_empty());

        inst.record_task("task-001");
        assert_eq!(inst.task_ids, vec!["task-001"]);

        // Duplicate is not added.
        inst.record_task("task-001");
        assert_eq!(inst.task_ids.len(), 1);

        inst.record_task("task-002");
        assert_eq!(inst.task_ids, vec!["task-001", "task-002"]);
    }

    #[test]
    fn test_current_task_id() {
        let inst = make_test_instance();
        assert_eq!(inst.current_task_id(), Some("task-abc"));

        let mut inst2 = inst;
        inst2.history.clear();
        assert_eq!(inst2.current_task_id(), None);
    }
}
