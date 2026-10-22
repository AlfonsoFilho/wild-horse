//! What a prompt turn reports back, in order.

use std::path::PathBuf;

use crate::acp;
use crate::policy::{PermissionDecision, PermissionRequest};

#[derive(Debug, Clone, PartialEq)]
pub enum SessionEvent {
    /// A chunk of the agent's reply text.
    AgentText(String),
    /// A chunk of the agent's visible reasoning.
    AgentThought(String),
    ToolCallStarted {
        id: String,
        title: String,
        kind: acp::ToolKind,
        status: acp::ToolCallStatus,
    },
    ToolCallUpdated {
        id: String,
        title: Option<String>,
        status: Option<acp::ToolCallStatus>,
    },
    /// The agent's whole current plan (each update replaces the previous one).
    Plan(Vec<acp::PlanEntry>),
    /// The agent asked to read a file; `result` is what the policy answered.
    FileRead {
        path: PathBuf,
        result: Result<(), String>,
    },
    /// The agent asked to write a file; `result` is what the policy answered.
    FileWrite {
        path: PathBuf,
        result: Result<(), String>,
    },
    /// The policy answered a permission request. `selected` is the option id
    /// sent back, or `None` when the outcome was `cancelled`.
    PermissionDecided {
        request: PermissionRequest,
        decision: PermissionDecision,
        selected: Option<String>,
    },
    /// Any other `session/update` (usage, modes, commands, …).
    Other(Box<acp::SessionUpdate>),
    /// The turn ended. Always the last event of a successful turn.
    Finished { stop_reason: acp::StopReason },
    /// The turn failed (protocol error, agent crashed). Always the last event.
    Error(String),
}

impl SessionEvent {
    /// `Finished` or `Error`: nothing follows it in this turn.
    pub fn is_terminal(&self) -> bool {
        matches!(self, SessionEvent::Finished { .. } | SessionEvent::Error(_))
    }

    pub(crate) fn from_update(update: acp::SessionUpdate) -> Self {
        use acp::SessionUpdate as U;
        match update {
            U::AgentMessageChunk(acp::ContentChunk {
                content: acp::ContentBlock::Text(text),
                ..
            }) => SessionEvent::AgentText(text.text),
            U::AgentThoughtChunk(acp::ContentChunk {
                content: acp::ContentBlock::Text(text),
                ..
            }) => SessionEvent::AgentThought(text.text),
            U::ToolCall(call) => SessionEvent::ToolCallStarted {
                id: call.tool_call_id.0.to_string(),
                title: call.title,
                kind: call.kind,
                status: call.status,
            },
            U::ToolCallUpdate(update) => SessionEvent::ToolCallUpdated {
                id: update.tool_call_id.0.to_string(),
                title: update.fields.title,
                status: update.fields.status,
            },
            U::Plan(plan) => SessionEvent::Plan(plan.entries),
            other => SessionEvent::Other(Box::new(other)),
        }
    }
}
