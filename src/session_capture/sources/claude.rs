#![allow(dead_code)]

use crate::session_capture::types::*;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct ClaudeSource;

impl ClaudeSource {
    pub fn new() -> Self {
        ClaudeSource
    }

    /// Return all Claude Code session JSONL files under ~/.claude/projects/
    pub fn discover_session_files() -> Vec<PathBuf> {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        let base = PathBuf::from(home).join(".claude").join("projects");
        let mut files = Vec::new();

        let project_dirs = match std::fs::read_dir(&base) {
            Ok(entries) => entries,
            Err(_) => return files,
        };

        for project_entry in project_dirs.flatten() {
            let project_path = project_entry.path();
            if !project_path.is_dir() {
                continue;
            }

            let session_entries = match std::fs::read_dir(&project_path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for session_entry in session_entries.flatten() {
                let path = session_entry.path();
                if path.is_file() && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl") {
                    files.push(path);
                }
            }
        }

        files.sort();
        files
    }

    /// Parse one NDJSON line from a Claude Code session file.
    /// source_path: path to the .jsonl file
    /// line_no: 1-based
    /// line: raw JSON string
    pub fn parse_line(source_path: &Path, line_no: u64, line: &str) -> SessionEvent {
        Self::parse_line_multi(source_path, line_no, line)
            .into_iter()
            .next()
            .unwrap_or_else(|| {
                let meta = Self::build_meta(
                    source_path,
                    "empty",
                    now_ms(),
                    None,
                    None,
                    line_no,
                );
                SessionEvent::Unknown(UnknownEvent {
                    meta,
                    payload: Value::Null,
                })
            })
    }

    /// Parse one NDJSON line into all normalized events represented by that line.
    pub fn parse_line_multi(source_path: &Path, line_no: u64, line: &str) -> Vec<SessionEvent> {
        let raw_json: Value = match serde_json::from_str(line) {
            Ok(value) => value,
            Err(error) => {
                let meta = Self::build_meta(
                    source_path,
                    "malformed_json",
                    now_ms(),
                    None,
                    None,
                    line_no,
                );
                return vec![SessionEvent::Malformed(MalformedEvent {
                    meta,
                    raw_line: line.to_string(),
                    error: error.to_string(),
                })];
            }
        };

        let raw_type = raw_json
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let timestamp_ms = raw_json
            .get("timestamp")
            .and_then(Value::as_str)
            .map(parse_timestamp_ms)
            .unwrap_or_else(now_ms);
        let cwd = raw_json
            .get("cwd")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let project = cwd.as_deref().map(canonical_project);
        let meta = Self::build_meta(
            source_path,
            raw_type,
            timestamp_ms,
            cwd.clone(),
            project.clone(),
            line_no,
        );

        match raw_type {
            "queue-operation" => vec![SessionEvent::Unknown(UnknownEvent {
                meta,
                payload: raw_json,
            })],
            "user" => Self::parse_user(meta, raw_json),
            "assistant" => Self::parse_assistant(meta, raw_json),
            _ => vec![SessionEvent::Unknown(UnknownEvent {
                meta,
                payload: raw_json,
            })],
        }
    }

    fn parse_user(meta: EventMeta, raw_json: Value) -> Vec<SessionEvent> {
        let message = raw_json.get("message").unwrap_or(&Value::Null);
        let content = message.get("content").unwrap_or(&Value::Null);

        match content {
            Value::String(text) => vec![SessionEvent::UserMessage(UserMessageEvent {
                meta,
                text: text.clone(),
            })],
            Value::Array(blocks) => {
                let tool_results = blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
                    .map(|block| {
                        let output = match block.get("content") {
                            Some(Value::String(text)) => text.clone(),
                            Some(Value::Array(parts)) => Self::extract_text_blocks(parts),
                            Some(other) => other.to_string(),
                            None => String::new(),
                        };

                        SessionEvent::ToolResult(ToolResultEvent {
                            meta: meta.clone(),
                            call_id: block
                                .get("tool_use_id")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            tool_name: String::new(),
                            success: true,
                            output,
                        })
                    })
                    .collect::<Vec<_>>();

                if !tool_results.is_empty() {
                    return tool_results;
                }

                vec![SessionEvent::UserMessage(UserMessageEvent {
                    meta,
                    text: Self::extract_text_blocks(blocks),
                })]
            }
            _ => vec![SessionEvent::UserMessage(UserMessageEvent {
                meta,
                text: String::new(),
            })],
        }
    }

    fn parse_assistant(meta: EventMeta, raw_json: Value) -> Vec<SessionEvent> {
        let message = raw_json.get("message").unwrap_or(&Value::Null);
        let model = message
            .get("model")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let content = message.get("content").unwrap_or(&Value::Null);

        match content {
            Value::Array(blocks) => {
                let tool_calls = blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
                    .map(|block| {
                        SessionEvent::ToolCall(ToolCallEvent {
                            meta: meta.clone(),
                            call_id: block.get("id").and_then(Value::as_str).map(ToOwned::to_owned),
                            tool_name: block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                                .to_string(),
                            arguments: block.get("input").cloned().unwrap_or(Value::Null),
                        })
                    })
                    .collect::<Vec<_>>();

                if !tool_calls.is_empty() {
                    return tool_calls;
                }

                vec![SessionEvent::AssistantMessage(AssistantMessageEvent {
                    meta,
                    text: Self::extract_text_blocks(blocks),
                    model,
                })]
            }
            Value::String(text) => vec![SessionEvent::AssistantMessage(AssistantMessageEvent {
                meta,
                text: text.clone(),
                model,
            })],
            _ => vec![SessionEvent::Unknown(UnknownEvent {
                meta,
                payload: raw_json,
            })],
        }
    }

    fn extract_text_blocks(blocks: &[Value]) -> String {
        blocks
            .iter()
            .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                Some("text") => block
                    .get("text")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn build_meta(
        source_path: &Path,
        raw_type: &str,
        timestamp_ms: i64,
        cwd: Option<String>,
        project: Option<String>,
        line_no: u64,
    ) -> EventMeta {
        EventMeta {
            source: SourceKind::Claude,
            session_id: Self::session_id_from_path(source_path, line_no),
            timestamp_ms,
            cwd,
            project,
            raw_type: raw_type.to_string(),
        }
    }

    fn session_id_from_path(source_path: &Path, line_no: u64) -> String {
        let stem = source_path
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| format!("line-{line_no}"));
        format!("claude:{stem}")
    }
}
