#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Which AI tool produced this session
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Copilot,
    Claude,
}

impl SourceKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            SourceKind::Copilot => "copilot",
            SourceKind::Claude => "claude",
        }
    }
}

/// Normalized event type — source-agnostic
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventType {
    SessionStart,
    SessionEnd,
    UserMessage,
    AssistantMessage,
    ToolCall,
    ToolResult,
    MalformedJson,
    Unknown,
}

impl EventType {
    pub fn as_str(&self) -> &str {
        match self {
            EventType::SessionStart => "session_start",
            EventType::SessionEnd => "session_end",
            EventType::UserMessage => "user_message",
            EventType::AssistantMessage => "assistant_message",
            EventType::ToolCall => "tool_call",
            EventType::ToolResult => "tool_result",
            EventType::MalformedJson => "malformed_json",
            EventType::Unknown => "unknown",
        }
    }
}

/// Metadata common to every normalized event
#[derive(Debug, Clone)]
pub struct EventMeta {
    pub source: SourceKind,
    /// normalized: "copilot:{uuid}" or "claude:{path-derived-id}"
    pub session_id: String,
    /// unix epoch milliseconds
    pub timestamp_ms: i64,
    pub cwd: Option<String>,
    /// canonical project root (nearest .git or .imi ancestor, else cwd)
    pub project: Option<String>,
    /// raw event type string from the source file
    pub raw_type: String,
}

/// A raw event line that failed JSON parsing
#[derive(Debug, Clone)]
pub struct MalformedEvent {
    pub meta: EventMeta,
    pub raw_line: String,
    pub error: String,
}

/// Session started
#[derive(Debug, Clone)]
pub struct SessionStartEvent {
    pub meta: EventMeta,
    pub model: Option<String>,
}

/// Session ended (explicit)
#[derive(Debug, Clone)]
pub struct SessionEndEvent {
    pub meta: EventMeta,
    /// Files modified this session, from session.shutdown.codeChanges.filesModified
    pub files_modified: Vec<String>,
    pub lines_added: i64,
    pub lines_removed: i64,
}

/// Human typed a message
#[derive(Debug, Clone)]
pub struct UserMessageEvent {
    pub meta: EventMeta,
    pub text: String,
}

/// Assistant replied
#[derive(Debug, Clone)]
pub struct AssistantMessageEvent {
    pub meta: EventMeta,
    pub text: String,
    pub model: Option<String>,
}

/// A tool was called
#[derive(Debug, Clone)]
pub struct ToolCallEvent {
    pub meta: EventMeta,
    pub call_id: Option<String>,
    pub tool_name: String,
    pub arguments: Value,
}

/// A tool call completed
#[derive(Debug, Clone)]
pub struct ToolResultEvent {
    pub meta: EventMeta,
    pub call_id: Option<String>,
    pub tool_name: String,
    pub success: bool,
    pub output: String,
}

/// Unknown event type — preserved for forward compat
#[derive(Debug, Clone)]
pub struct UnknownEvent {
    pub meta: EventMeta,
    pub payload: Value,
}

/// All normalized events
#[derive(Debug, Clone)]
pub enum SessionEvent {
    SessionStart(SessionStartEvent),
    SessionEnd(SessionEndEvent),
    UserMessage(UserMessageEvent),
    AssistantMessage(AssistantMessageEvent),
    ToolCall(ToolCallEvent),
    ToolResult(ToolResultEvent),
    Malformed(MalformedEvent),
    Unknown(UnknownEvent),
}

impl SessionEvent {
    pub fn meta(&self) -> &EventMeta {
        match self {
            SessionEvent::SessionStart(e) => &e.meta,
            SessionEvent::SessionEnd(e) => &e.meta,
            SessionEvent::UserMessage(e) => &e.meta,
            SessionEvent::AssistantMessage(e) => &e.meta,
            SessionEvent::ToolCall(e) => &e.meta,
            SessionEvent::ToolResult(e) => &e.meta,
            SessionEvent::Malformed(e) => &e.meta,
            SessionEvent::Unknown(e) => &e.meta,
        }
    }

    pub fn event_type(&self) -> EventType {
        match self {
            SessionEvent::SessionStart(_) => EventType::SessionStart,
            SessionEvent::SessionEnd(_) => EventType::SessionEnd,
            SessionEvent::UserMessage(_) => EventType::UserMessage,
            SessionEvent::AssistantMessage(_) => EventType::AssistantMessage,
            SessionEvent::ToolCall(_) => EventType::ToolCall,
            SessionEvent::ToolResult(_) => EventType::ToolResult,
            SessionEvent::Malformed(_) => EventType::MalformedJson,
            SessionEvent::Unknown(_) => EventType::Unknown,
        }
    }
}

/// Helper: parse ISO8601 timestamp string to epoch ms
pub fn parse_timestamp_ms(s: &str) -> i64 {
    // Try chrono parsing first
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return dt.timestamp_millis();
    }
    // Fallback: try without timezone
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f") {
        return dt.and_utc().timestamp_millis();
    }
    // Last resort: current time
    chrono::Utc::now().timestamp_millis()
}

/// Helper: derive canonical project root from a cwd path
/// Walks up from cwd looking for .git or .imi directory.
/// Falls back to cwd itself.
pub fn canonical_project(cwd: &str) -> String {
    let path = std::path::Path::new(cwd);
    let mut current = path;
    loop {
        if current.join(".git").exists() || current.join(".imi").exists() {
            return current.to_string_lossy().to_string();
        }
        match current.parent() {
            Some(p) if p != current => current = p,
            _ => break,
        }
    }
    cwd.to_string()
}

/// Helper: current epoch ms
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
