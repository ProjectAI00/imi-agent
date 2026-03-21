#![allow(dead_code)]
use crate::session_capture::types::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Discovers and parses Copilot session event files.
pub struct CopilotSource;

impl CopilotSource {
    pub fn new() -> Self {
        CopilotSource
    }

    /// Return all active events.jsonl paths under ~/.copilot/session-state/
    pub fn discover_session_files() -> Vec<PathBuf> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let base = PathBuf::from(home).join(".copilot").join("session-state");
        let mut files = Vec::new();
        let dir = match std::fs::read_dir(&base) {
            Ok(dir) => dir,
            Err(_) => return files,
        };

        for entry in dir.flatten() {
            let session_dir = entry.path();
            if !session_dir.is_dir() || !has_active_lock(&session_dir) {
                continue;
            }

            let events_file = session_dir.join("events.jsonl");
            if events_file.is_file() {
                files.push(events_file);
            }
        }

        files.sort();
        files
    }

    /// Parse one NDJSON line from a Copilot events.jsonl file into a SessionEvent.
    /// source_path: absolute path to the events.jsonl file
    /// line_no: 1-based line number
    /// line: the raw JSON string
    pub fn parse_line(source_path: &Path, line_no: u64, line: &str) -> SessionEvent {
        let session_uuid = source_path
            .parent()
            .and_then(|parent| parent.file_name())
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let session_id = format!("copilot:{session_uuid}");

        let raw_json: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(err) => {
                let meta = EventMeta {
                    source: SourceKind::Copilot,
                    session_id,
                    timestamp_ms: now_ms(),
                    cwd: None,
                    project: None,
                    raw_type: "malformed_json".to_string(),
                };
                return SessionEvent::Malformed(MalformedEvent {
                    meta,
                    raw_line: line.to_string(),
                    error: format!("line {line_no}: {err}"),
                });
            }
        };

        let event_type_str = raw_json
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let timestamp_ms = raw_json
            .get("timestamp")
            .and_then(Value::as_str)
            .map(parse_timestamp_ms)
            .unwrap_or_else(now_ms);

        let data = raw_json.get("data").unwrap_or(&Value::Null);
        let cwd = extract_cwd(data);
        let project = cwd.as_deref().map(canonical_project);

        let meta = EventMeta {
            source: SourceKind::Copilot,
            session_id: session_id.clone(),
            timestamp_ms,
            cwd: cwd.clone(),
            project,
            raw_type: event_type_str.clone(),
        };

        match event_type_str.as_str() {
            "session.start" => SessionEvent::SessionStart(SessionStartEvent {
                meta,
                model: data
                    .get("model")
                    .and_then(Value::as_str)
                    .map(|model| model.to_string()),
            }),
            "session.shutdown" => {
                let code_changes = data.get("codeChanges").unwrap_or(&Value::Null);
                let files_modified = code_changes
                    .get("filesModified")
                    .and_then(Value::as_array)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(|item| item.as_str().map(|value| value.to_string()))
                            .collect()
                    })
                    .unwrap_or_default();

                SessionEvent::SessionEnd(SessionEndEvent {
                    meta,
                    files_modified,
                    lines_added: code_changes
                        .get("linesAdded")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                    lines_removed: code_changes
                        .get("linesRemoved")
                        .and_then(Value::as_i64)
                        .unwrap_or(0),
                })
            }
            "user.message" => SessionEvent::UserMessage(UserMessageEvent {
                meta,
                text: extract_text(data.get("content").unwrap_or(&Value::Null)),
            }),
            "assistant.message" => SessionEvent::AssistantMessage(AssistantMessageEvent {
                meta,
                text: extract_text(data.get("content").unwrap_or(&Value::Null)),
                model: data
                    .get("model")
                    .and_then(Value::as_str)
                    .map(|model| model.to_string()),
            }),
            "tool.execution_start" => SessionEvent::ToolCall(ToolCallEvent {
                meta,
                call_id: data
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(|value| value.to_string()),
                tool_name: data
                    .get("toolName")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string(),
                arguments: data.get("arguments").cloned().unwrap_or(Value::Null),
            }),
            "tool.execution_complete" => SessionEvent::ToolResult(ToolResultEvent {
                meta,
                call_id: data
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .map(|value| value.to_string()),
                tool_name: String::new(),
                success: data
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                output: extract_text(
                    data.get("result")
                        .and_then(|result| result.get("content"))
                        .unwrap_or(&Value::Null),
                ),
            }),
            _ => SessionEvent::Unknown(UnknownEvent {
                meta,
                payload: raw_json,
            }),
        }
    }
}

fn has_active_lock(session_dir: &Path) -> bool {
    std::fs::read_dir(session_dir)
        .ok()
        .map(|entries| {
            entries.flatten().any(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name.starts_with("inuse.") && name.ends_with(".lock")
            })
        })
        .unwrap_or(false)
}

fn extract_cwd(data: &Value) -> Option<String> {
    data.get("context")
        .and_then(|context| context.get("cwd"))
        .and_then(Value::as_str)
        .map(|cwd| cwd.to_string())
        .or_else(|| {
            data.get("cwd")
                .and_then(Value::as_str)
                .map(|cwd| cwd.to_string())
        })
        .or_else(|| {
            data.get("workingDirectory")
                .and_then(Value::as_str)
                .map(|cwd| cwd.to_string())
        })
}

fn extract_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(extract_text)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Object(map) => map
            .get("text")
            .or_else(|| map.get("content"))
            .map(extract_text)
            .unwrap_or_else(|| value.to_string()),
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::CopilotSource;
    use crate::session_capture::types::SessionEvent;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_session_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("imi-copilot-{name}-{unique}"));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn discover_session_files_only_returns_locked_sessions() {
        let home = temp_session_dir("home");
        let base = home.join(".copilot").join("session-state");
        let active = base.join("active-session");
        let inactive = base.join("inactive-session");
        fs::create_dir_all(&active).unwrap();
        fs::create_dir_all(&inactive).unwrap();
        fs::write(active.join("events.jsonl"), b"{}\n").unwrap();
        fs::write(active.join("inuse.123.lock"), b"").unwrap();
        fs::write(inactive.join("events.jsonl"), b"{}\n").unwrap();

        let old_home = std::env::var_os("HOME");
        unsafe { std::env::set_var("HOME", &home) };
        let files = CopilotSource::discover_session_files();
        match old_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }

        assert_eq!(files, vec![active.join("events.jsonl")]);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn parses_tool_completion_output() {
        let source = PathBuf::from("/tmp/session-123/events.jsonl");
        let line = json!({
            "type": "tool.execution_complete",
            "timestamp": "2026-03-21T10:38:36.094Z",
            "data": {
                "toolCallId": "call-1",
                "success": true,
                "result": { "content": "done" }
            }
        })
        .to_string();

        match CopilotSource::parse_line(&source, 1, &line) {
            SessionEvent::ToolResult(event) => {
                assert_eq!(event.meta.session_id, "copilot:session-123");
                assert_eq!(event.call_id.as_deref(), Some("call-1"));
                assert!(event.success);
                assert_eq!(event.output, "done");
            }
            other => panic!("expected tool result, got {other:?}"),
        }
    }
}
