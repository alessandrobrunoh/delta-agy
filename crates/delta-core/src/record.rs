//! Schema-1 records that `delta thread append` reads from stdin, one JSON object per line.
//!
//! Each record wraps an ACP-shaped `session/update` payload ([`Update`]) in an [`Envelope`] that
//! carries the producer's sequence and, for turn-scoped records, the sequence of the turn's start.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One line of `delta thread append` input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    /// Always 1.
    pub schema_version: u32,
    /// The producer's own monotonically increasing sequence, starting at 1.
    pub sequence: u64,
    /// The sequence of the `state_update: running` record that started the turn this record belongs
    /// to. A turn's own start names itself.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_sequence: Option<u64>,
    pub update: Update,
}

impl Envelope {
    pub fn new(sequence: u64, turn_sequence: Option<u64>, update: Update) -> Self {
        Self { schema_version: 1, sequence, turn_sequence, update }
    }

    /// The exact line sent to `delta thread append`, newline included.
    pub fn to_line(&self) -> String {
        let mut line = serde_json::to_string(self).expect("envelopes always serialize");
        line.push('\n');
        line
    }
}

/// An ACP `session/update` payload, tagged by `sessionUpdate`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "sessionUpdate", rename_all = "snake_case")]
pub enum Update {
    #[serde(rename_all = "camelCase")]
    UserMessage { message_id: String, content: Vec<ContentBlock> },
    #[serde(rename_all = "camelCase")]
    AgentMessage { message_id: String, content: Vec<ContentBlock> },
    #[serde(rename_all = "camelCase")]
    AgentThought { message_id: String, content: Vec<ContentBlock> },
    ToolCallUpdate(ToolCallUpdate),
    StateUpdate {
        state: TurnState,
        #[serde(rename = "_meta", skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },
    SessionInfoUpdate { title: String },
}

impl Update {
    pub fn user_text(message_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::UserMessage { message_id: message_id.into(), content: vec![ContentBlock::text(text)] }
    }

    pub fn agent_text(message_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::AgentMessage { message_id: message_id.into(), content: vec![ContentBlock::text(text)] }
    }

    pub fn thought_text(message_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::AgentThought { message_id: message_id.into(), content: vec![ContentBlock::text(text)] }
    }

    /// The `running` record that starts a turn, attributed to `producer_version`.
    pub fn running(producer_version: &str) -> Self {
        Self::StateUpdate {
            state: TurnState::Running,
            meta: Some(serde_json::json!({ "delta": { "producer_version": producer_version } })),
        }
    }

    pub fn idle() -> Self {
        Self::StateUpdate { state: TurnState::Idle, meta: None }
    }

    pub fn is_running(&self) -> bool {
        matches!(self, Self::StateUpdate { state: TurnState::Running, .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnState {
    Running,
    Idle,
}

/// A tool call's start, progress or completion. Repeating the same `tool_call_id` updates the call;
/// only the fields present change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallUpdate {
    pub tool_call_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<ToolKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<ToolStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<Value>,
    /// ACP tool call content: `{"type":"content","content":<ContentBlock>}` or
    /// `{"type":"diff","path":..,"oldText":..,"newText":..}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<Value>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Execute,
    Think,
    Fetch,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// An ACP content block. Only text is modelled; other block kinds pass through as raw JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentBlock {
    Text {
        #[serde(rename = "type")]
        kind: TextTag,
        text: String,
    },
    Other(Value),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextTag {
    Text,
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { kind: TextTag::Text, text: text.into() }
    }

    /// Wrap a block as ACP tool call content.
    pub fn as_tool_content(&self) -> Value {
        serde_json::json!({ "type": "content", "content": self })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_acp_shape() {
        let line = Envelope::new(3, Some(2), Update::agent_text("m1", "hi")).to_line();
        assert_eq!(
            line,
            r#"{"schemaVersion":1,"sequence":3,"turnSequence":2,"update":{"sessionUpdate":"agent_message","messageId":"m1","content":[{"type":"text","text":"hi"}]}}"#.to_owned() + "\n"
        );
    }

    #[test]
    fn tool_call_omits_absent_fields() {
        let update = Update::ToolCallUpdate(ToolCallUpdate {
            tool_call_id: "t1".into(),
            status: Some(ToolStatus::Completed),
            ..Default::default()
        });
        assert_eq!(
            serde_json::to_string(&update).unwrap(),
            r#"{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}"#
        );
    }

    #[test]
    fn running_round_trips() {
        let update = Update::running("0.1.0");
        let back: Update = serde_json::from_str(&serde_json::to_string(&update).unwrap()).unwrap();
        assert!(back.is_running());
    }
}
