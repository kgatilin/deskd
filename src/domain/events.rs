//! Domain events — notifications emitted by state machine and task operations.
//!
//! Pure data types — no I/O, no bus logic. Published to the bus by the
//! application layer (workflow engine, worker).

/// Domain event emitted by orchestration operations.
#[derive(Debug, Clone)]
pub enum DomainEvent {
    /// A new SM instance was created.
    InstanceCreated {
        instance_id: String,
        model: String,
        title: String,
        created_by: String,
    },
    /// A state machine transition was applied.
    TransitionApplied {
        instance_id: String,
        from: String,
        to: String,
        trigger: String,
    },
    /// A task was dispatched (queued or sent to an agent).
    TaskDispatched {
        task_id: String,
        instance_id: Option<String>,
        assignee: String,
    },
    /// A task was completed successfully.
    TaskCompleted {
        task_id: String,
        instance_id: Option<String>,
        result_summary: String,
    },
    /// A task failed.
    TaskFailed {
        task_id: String,
        instance_id: Option<String>,
        error: String,
    },
    /// A task timed out.
    TaskTimedOut {
        task_id: String,
        instance_id: Option<String>,
    },
    /// An instance reached a terminal state.
    InstanceCompleted {
        instance_id: String,
        model: String,
        final_state: String,
    },
}

impl DomainEvent {
    /// Event type as a string, used for bus target routing.
    pub fn event_type(&self) -> &'static str {
        match self {
            Self::InstanceCreated { .. } => "instance_created",
            Self::TransitionApplied { .. } => "transition_applied",
            Self::TaskDispatched { .. } => "task_dispatched",
            Self::TaskCompleted { .. } => "task_completed",
            Self::TaskFailed { .. } => "task_failed",
            Self::TaskTimedOut { .. } => "task_timed_out",
            Self::InstanceCompleted { .. } => "instance_completed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_type_names() {
        let event = DomainEvent::InstanceCreated {
            instance_id: "sm-123".into(),
            model: "pipeline".into(),
            title: "Test".into(),
            created_by: "kira".into(),
        };
        assert_eq!(event.event_type(), "instance_created");

        let event = DomainEvent::TransitionApplied {
            instance_id: "sm-123".into(),
            from: "draft".into(),
            to: "review".into(),
            trigger: "auto".into(),
        };
        assert_eq!(event.event_type(), "transition_applied");
    }
}
