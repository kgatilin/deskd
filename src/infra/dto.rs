//! Infrastructure DTOs — wire and storage formats.
//!
//! These types own their serde derives and decouple the infra layer
//! from domain types. Conversion happens at port boundaries via From/Into.

use serde::{Deserialize, Serialize};
use tracing::warn;

// ─── Bus wire format ─────────────────────────────────────────────────────────

/// Wire format for messages on the Unix socket bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusMessage {
    pub id: String,
    pub source: String,
    pub target: String,
    pub payload: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(default)]
    pub metadata: BusMetadata,
}

/// Wire format for message metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BusMetadata {
    #[serde(default = "default_priority")]
    pub priority: u8,
    #[serde(default)]
    pub fresh: bool,
}

fn default_priority() -> u8 {
    5
}

/// Wire format for the registration handshake.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BusRegister {
    pub name: String,
    #[serde(default)]
    pub subscriptions: Vec<String>,
}

/// Wire-level envelope: tagged union for all bus protocol messages.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum BusEnvelope {
    Register(BusRegister),
    Message(BusMessage),
    List,
}

// ─── Domain ↔ Bus conversions ────────────────────────────────────────────────

use crate::domain::message::{Envelope, Message, Metadata, Register};

impl From<BusMessage> for Message {
    fn from(dto: BusMessage) -> Self {
        Self {
            id: dto.id,
            source: dto.source,
            target: dto.target,
            payload: dto.payload,
            reply_to: dto.reply_to,
            metadata: Metadata {
                priority: dto.metadata.priority,
                fresh: dto.metadata.fresh,
            },
        }
    }
}

impl From<&Message> for BusMessage {
    fn from(msg: &Message) -> Self {
        Self {
            id: msg.id.clone(),
            source: msg.source.clone(),
            target: msg.target.clone(),
            payload: msg.payload.clone(),
            reply_to: msg.reply_to.clone(),
            metadata: BusMetadata {
                priority: msg.metadata.priority,
                fresh: msg.metadata.fresh,
            },
        }
    }
}

impl From<BusEnvelope> for Envelope {
    fn from(dto: BusEnvelope) -> Self {
        match dto {
            BusEnvelope::Register(r) => Envelope::Register(Register {
                name: r.name,
                subscriptions: r.subscriptions,
            }),
            BusEnvelope::Message(m) => Envelope::Message(m.into()),
            BusEnvelope::List => Envelope::List,
        }
    }
}

// ─── Store format: Task ──────────────────────────────────────────────────────

/// Storage format for tasks (JSON file on disk).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTask {
    pub id: String,
    pub description: String,
    pub status: String,
    pub assignee: Option<String>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub created_by: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub criteria: StoredTaskCriteria,
    #[serde(default)]
    pub sm_instance_id: Option<String>,
}

/// Storage format for task matching criteria.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StoredTaskCriteria {
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
}

use crate::domain::task::{Task, TaskCriteria, TaskStatus};

impl From<StoredTask> for Task {
    fn from(dto: StoredTask) -> Self {
        Self {
            id: dto.id,
            description: dto.description,
            status: match dto.status.as_str() {
                "pending" => TaskStatus::Pending,
                "active" => TaskStatus::Active,
                "done" => TaskStatus::Done,
                "failed" => TaskStatus::Failed,
                "cancelled" => TaskStatus::Cancelled,
                other => {
                    warn!(status = other, "unknown task status, defaulting to Pending");
                    TaskStatus::Pending
                }
            },
            assignee: dto.assignee,
            result: dto.result,
            error: dto.error,
            created_by: dto.created_by,
            created_at: dto.created_at,
            updated_at: dto.updated_at,
            criteria: TaskCriteria {
                model: dto.criteria.model,
                labels: dto.criteria.labels,
            },
            sm_instance_id: dto.sm_instance_id,
        }
    }
}

impl From<&Task> for StoredTask {
    fn from(task: &Task) -> Self {
        Self {
            id: task.id.clone(),
            description: task.description.clone(),
            status: match task.status {
                TaskStatus::Pending => "pending",
                TaskStatus::Active => "active",
                TaskStatus::Done => "done",
                TaskStatus::Failed => "failed",
                TaskStatus::Cancelled => "cancelled",
            }
            .to_string(),
            assignee: task.assignee.clone(),
            result: task.result.clone(),
            error: task.error.clone(),
            created_by: task.created_by.clone(),
            created_at: task.created_at.clone(),
            updated_at: task.updated_at.clone(),
            criteria: StoredTaskCriteria {
                model: task.criteria.model.clone(),
                labels: task.criteria.labels.clone(),
            },
            sm_instance_id: task.sm_instance_id.clone(),
        }
    }
}

// ─── Store format: StateMachine Instance ─────────────────────────────────────

/// Storage format for state machine instances.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredInstance {
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
    pub history: Vec<StoredTransition>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Storage format for a recorded state transition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredTransition {
    pub from: String,
    pub to: String,
    pub trigger: String,
    pub timestamp: String,
    #[serde(default)]
    pub note: Option<String>,
}

use crate::domain::statemachine::{Instance, Transition};

impl From<StoredInstance> for Instance {
    fn from(dto: StoredInstance) -> Self {
        Self {
            id: dto.id,
            model: dto.model,
            title: dto.title,
            body: dto.body,
            state: dto.state,
            assignee: dto.assignee,
            result: dto.result,
            error: dto.error,
            created_by: dto.created_by,
            created_at: dto.created_at,
            updated_at: dto.updated_at,
            history: dto.history.into_iter().map(Into::into).collect(),
            metadata: dto.metadata,
        }
    }
}

impl From<&Instance> for StoredInstance {
    fn from(inst: &Instance) -> Self {
        Self {
            id: inst.id.clone(),
            model: inst.model.clone(),
            title: inst.title.clone(),
            body: inst.body.clone(),
            state: inst.state.clone(),
            assignee: inst.assignee.clone(),
            result: inst.result.clone(),
            error: inst.error.clone(),
            created_by: inst.created_by.clone(),
            created_at: inst.created_at.clone(),
            updated_at: inst.updated_at.clone(),
            history: inst.history.iter().map(Into::into).collect(),
            metadata: inst.metadata.clone(),
        }
    }
}

impl From<StoredTransition> for Transition {
    fn from(dto: StoredTransition) -> Self {
        Self {
            from: dto.from,
            to: dto.to,
            trigger: dto.trigger,
            timestamp: dto.timestamp,
            note: dto.note,
        }
    }
}

impl From<&Transition> for StoredTransition {
    fn from(t: &Transition) -> Self {
        Self {
            from: t.from.clone(),
            to: t.to.clone(),
            trigger: t.trigger.clone(),
            timestamp: t.timestamp.clone(),
            note: t.note.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bus_message_to_domain_roundtrip() {
        let wire = BusMessage {
            id: "msg-1".into(),
            source: "cli".into(),
            target: "agent:dev".into(),
            payload: serde_json::json!({"task": "hello"}),
            reply_to: Some("cli".into()),
            metadata: BusMetadata {
                priority: 3,
                fresh: true,
            },
        };
        let domain: Message = wire.clone().into();
        assert_eq!(domain.id, "msg-1");
        assert_eq!(domain.source, "cli");
        assert_eq!(domain.metadata.priority, 3);
        assert!(domain.metadata.fresh);

        let back: BusMessage = (&domain).into();
        assert_eq!(back.id, wire.id);
        assert_eq!(back.reply_to, wire.reply_to);
    }

    #[test]
    fn test_bus_envelope_deserialize_message() {
        let json = r#"{"type":"message","id":"1","source":"a","target":"b","payload":{}}"#;
        let env: BusEnvelope = serde_json::from_str(json).unwrap();
        let domain: Envelope = env.into();
        match domain {
            Envelope::Message(m) => assert_eq!(m.source, "a"),
            _ => panic!("expected Message"),
        }
    }

    #[test]
    fn test_bus_envelope_deserialize_register() {
        let json = r#"{"type":"register","name":"cli","subscriptions":["agent:cli"]}"#;
        let env: BusEnvelope = serde_json::from_str(json).unwrap();
        let domain: Envelope = env.into();
        match domain {
            Envelope::Register(r) => assert_eq!(r.name, "cli"),
            _ => panic!("expected Register"),
        }
    }

    #[test]
    fn test_stored_task_roundtrip() {
        let task = Task {
            id: "task-1".into(),
            description: "test task".into(),
            status: TaskStatus::Active,
            assignee: Some("dev".into()),
            result: None,
            error: None,
            created_by: "cli".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            criteria: TaskCriteria {
                model: Some("claude-sonnet-4-6".into()),
                labels: vec!["rust".into()],
            },
            sm_instance_id: None,
        };
        let stored: StoredTask = (&task).into();
        assert_eq!(stored.status, "active");
        let restored: Task = stored.into();
        assert_eq!(restored.status, TaskStatus::Active);
        assert_eq!(restored.id, "task-1");
        assert_eq!(
            restored.criteria.model.as_deref(),
            Some("claude-sonnet-4-6")
        );
    }

    #[test]
    fn test_stored_instance_roundtrip() {
        let inst = Instance {
            id: "sm-1".into(),
            model: "review".into(),
            title: "PR review".into(),
            body: String::new(),
            state: "open".into(),
            assignee: "dev".into(),
            result: None,
            error: None,
            created_by: "cli".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            updated_at: "2026-01-01T00:00:00Z".into(),
            history: vec![Transition {
                from: "new".into(),
                to: "open".into(),
                trigger: "create".into(),
                timestamp: "2026-01-01T00:00:00Z".into(),
                note: None,
            }],
            metadata: serde_json::json!({}),
        };
        let stored: StoredInstance = (&inst).into();
        let restored: Instance = stored.into();
        assert_eq!(restored.id, "sm-1");
        assert_eq!(restored.state, "open");
        assert_eq!(restored.history.len(), 1);
        assert_eq!(restored.history[0].from, "new");
    }
}
