use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::tool::ToolName;

/// Role of a message participant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    System,
}

/// A message in a session. This is the persistence/UI type.
/// For the wire format (API requests), we use async-openai types directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    /// Unique message ID (UUID v4).
    pub id: String,
    /// Session this message belongs to.
    pub session_id: String,
    /// Who sent this message.
    pub role: Role,
    /// The parts that make up this message.
    pub parts: Vec<MessagePart>,
    /// When the message was created.
    pub created_at: DateTime<Utc>,
}

/// A single part of a message. Messages can have multiple parts
/// (e.g., text + tool calls in an assistant response).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MessagePart {
    /// Plain text content.
    #[serde(rename = "text")]
    Text { text: String },

    /// Reasoning/thinking content (from models that support it).
    #[serde(rename = "reasoning")]
    Reasoning { text: String },

    /// A tool call made by the assistant.
    #[serde(rename = "tool_call")]
    ToolCall {
        call_id: String,
        tool_name: ToolName,
        input: serde_json::Value,
        state: ToolCallState,
    },

    /// The result of executing a tool call.
    #[serde(rename = "tool_result")]
    ToolResult {
        call_id: String,
        tool_name: ToolName,
        output: String,
        title: String,
        is_error: bool,
    },
}

/// State tracking for a tool call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolCallState {
    Pending,
    Running,
    Completed,
    Error { message: String },
    Denied,
}

impl Message {
    /// Create a new user message with a single text part.
    pub fn user(session_id: &str, text: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            role: Role::User,
            parts: vec![MessagePart::Text {
                text: text.to_string(),
            }],
            created_at: Utc::now(),
        }
    }

    /// Create a new assistant message with a single text part.
    pub fn assistant(session_id: &str, text: &str) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_string(),
            role: Role::Assistant,
            parts: vec![MessagePart::Text {
                text: text.to_string(),
            }],
            created_at: Utc::now(),
        }
    }

    /// Get the text content of this message (concatenating all text parts).
    pub fn text_content(&self) -> String {
        self.parts
            .iter()
            .filter_map(|p| match p {
                MessagePart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Update the text of the first text part (for streaming accumulation).
    pub fn set_text(&mut self, new_text: &str) {
        if let Some(MessagePart::Text { text }) = self.parts.first_mut() {
            *text = new_text.to_string();
        }
    }

    /// Append text to the first text part (for streaming deltas).
    pub fn append_text(&mut self, delta: &str) {
        if let Some(MessagePart::Text { text }) = self.parts.first_mut() {
            text.push_str(delta);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_has_correct_role() {
        let msg = Message::user("sess-1", "hello");
        assert_eq!(msg.role, Role::User);
        assert_eq!(msg.session_id, "sess-1");
    }

    #[test]
    fn assistant_message_has_correct_role() {
        let msg = Message::assistant("sess-1", "hi");
        assert_eq!(msg.role, Role::Assistant);
    }

    #[test]
    fn text_content_returns_text() {
        let msg = Message::user("s", "hello world");
        assert_eq!(msg.text_content(), "hello world");
    }

    #[test]
    fn text_content_concatenates_multiple_text_parts() {
        let mut msg = Message::assistant("s", "hello ");
        msg.parts.push(MessagePart::Text {
            text: "world".to_string(),
        });
        assert_eq!(msg.text_content(), "hello world");
    }

    #[test]
    fn text_content_skips_non_text_parts() {
        let mut msg = Message::assistant("s", "before");
        msg.parts.push(MessagePart::ToolCall {
            call_id: "c1".into(),
            tool_name: ToolName::Read,
            input: serde_json::json!({}),
            state: ToolCallState::Completed,
        });
        msg.parts.push(MessagePart::Text {
            text: "after".to_string(),
        });
        assert_eq!(msg.text_content(), "beforeafter");
    }

    #[test]
    fn set_text_replaces_first_part() {
        let mut msg = Message::assistant("s", "original");
        msg.set_text("replaced");
        assert_eq!(msg.text_content(), "replaced");
    }

    #[test]
    fn append_text_accumulates() {
        let mut msg = Message::assistant("s", "");
        msg.append_text("hello");
        msg.append_text(" world");
        assert_eq!(msg.text_content(), "hello world");
    }

    #[test]
    fn message_serialization_roundtrip() {
        let msg = Message::user("sess-1", "test message");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.text_content(), "test message");
        assert_eq!(deserialized.role, Role::User);
        assert_eq!(deserialized.session_id, "sess-1");
    }
}
