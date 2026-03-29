#![allow(dead_code)]

use crate::session_capture::memory::{self, generate_delta_brief, init_memories_table};
use crate::session_capture::types::canonical_project;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Budget constants
// ---------------------------------------------------------------------------

const SESSION_START_BUDGET: usize = 2048;
const MID_SESSION_BUDGET: usize = 512;
const MAX_MID_SESSION_INJECTIONS: u32 = 3;

// ---------------------------------------------------------------------------
// Hook event types (from Claude Code hooks spec)
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum HookEvent {
    SessionStart,
    SessionEnd,
    UserPromptSubmit,
    PreToolUse,
    PostToolUse,
    Stop,
    Unknown(String),
}

impl HookEvent {
    fn from_str(s: &str) -> Self {
        match s {
            "session_start" => HookEvent::SessionStart,
            "SessionStart" => HookEvent::SessionStart,
            "session_end" => HookEvent::SessionEnd,
            "SessionEnd" => HookEvent::SessionEnd,
            "user_prompt_submit" => HookEvent::UserPromptSubmit,
            "UserPromptSubmit" => HookEvent::UserPromptSubmit,
            "pre_tool_use" => HookEvent::PreToolUse,
            "PreToolUse" => HookEvent::PreToolUse,
            "post_tool_use" => HookEvent::PostToolUse,
            "PostToolUse" => HookEvent::PostToolUse,
            "stop" => HookEvent::Stop,
            "Stop" => HookEvent::Stop,
            other => HookEvent::Unknown(other.to_string()),
        }
    }
}

// ---------------------------------------------------------------------------
// Injection tracking (temp file)
// ---------------------------------------------------------------------------

fn injection_tracker_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".imi")
        .join("hook_injection_count")
}

fn read_injection_count() -> u32 {
    fs::read_to_string(injection_tracker_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn write_injection_count(count: u32) {
    let path = injection_tracker_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, count.to_string());
}

fn reset_injection_count() {
    let _ = fs::remove_file(injection_tracker_path());
}

// ---------------------------------------------------------------------------
// Session start time tracking
// ---------------------------------------------------------------------------

fn session_start_marker_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".imi").join("session_start_ms")
}

fn record_session_start() {
    let path = session_start_marker_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let now = crate::session_capture::types::now_ms();
    let _ = fs::write(path, now.to_string());
    reset_injection_count();
}

fn session_duration_minutes() -> f64 {
    let start_ms: i64 = fs::read_to_string(session_start_marker_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    if start_ms == 0 {
        return 0.0;
    }
    let now = crate::session_capture::types::now_ms();
    ((now - start_ms).max(0) as f64) / 60_000.0
}

// ---------------------------------------------------------------------------
// Project detection from hook input
// ---------------------------------------------------------------------------

fn extract_project(input: &Value) -> Option<String> {
    // Try cwd from hook input first
    if let Some(cwd) = input
        .get("cwd")
        .or_else(|| input.get("workingDirectory"))
        .and_then(|v| v.as_str())
    {
        if !cwd.is_empty() {
            return Some(canonical_project(cwd));
        }
    }
    // Fall back to env var (Claude Code sets this)
    if let Ok(p) = env::var("CLAUDE_PROJECT_DIR") {
        if !p.trim().is_empty() {
            return Some(canonical_project(&p));
        }
    }
    // Fall back to current directory
    if let Ok(cwd) = env::current_dir() {
        let cwd_str = cwd.to_string_lossy().to_string();
        return Some(canonical_project(&cwd_str));
    }
    None
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

fn handle_session_start(input: &Value) -> Value {
    record_session_start();

    let project = match extract_project(input) {
        Some(p) => p,
        None => return empty_response(),
    };

    let sconn = match crate::session_capture::db::open_sessions_db() {
        Ok(c) => c,
        Err(_) => return empty_response(),
    };

    let _ = init_memories_table(&sconn);

    // Run indexer + compress to ensure we have fresh data
    let _ = crate::session_capture::indexer::run_indexer(300);
    let _ = crate::session_capture::compress::compress_all_pending(20);

    let brief = crate::session_capture::brief::generate_brief(&sconn, &project)
        .unwrap_or_else(|_| "No session history available.".to_string());

    let brief = truncate_bytes(&brief, SESSION_START_BUDGET);

    json!({
        "additionalContext": brief
    })
}

fn handle_session_end(_input: &Value) -> Value {
    let _ = crate::session_capture::indexer::run_indexer(300);
    let _ = crate::session_capture::compress::compress_all_pending(50);
    let _ = crate::session_capture::memory::compress_patterns(
        &crate::session_capture::db::open_sessions_db().unwrap()
    );
    reset_injection_count();
    let _ = fs::remove_file(session_start_marker_path());
    empty_response()
}

fn handle_post_tool_use(input: &Value) -> Value {
    let count = read_injection_count();
    if count >= MAX_MID_SESSION_INJECTIONS {
        return empty_response();
    }

    // Only trigger on errors/failures
    let is_error = input
        .get("tool_result")
        .and_then(|r| r.get("success"))
        .and_then(|v| v.as_bool())
        .map(|b| !b)
        .unwrap_or(false);

    // Also check for error in stderr or exit code patterns
    let has_error_output = input
        .get("tool_result")
        .and_then(|r| r.get("output"))
        .and_then(|v| v.as_str())
        .map(|s| {
            let lower = s.to_lowercase();
            lower.contains("error:") || lower.contains("failed") || lower.contains("panic")
        })
        .unwrap_or(false);

    if !is_error && !has_error_output {
        return empty_response();
    }

    let project = match extract_project(input) {
        Some(p) => p,
        None => return empty_response(),
    };

    // Extract tool name and file for more targeted retrieval
    let tool_name = input
        .get("tool_name")
        .or_else(|| input.get("tool_result").and_then(|r| r.get("tool_name")))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let file_hint = extract_file_from_input(input);

    let sconn = match crate::session_capture::db::open_sessions_db() {
        Ok(c) => c,
        Err(_) => return empty_response(),
    };

    let _ = init_memories_table(&sconn);

    // Build a targeted query from the error context
    let query = if !file_hint.is_empty() {
        format!("{} error {}", tool_name, file_hint)
    } else {
        format!("{} error failure", tool_name)
    };

    let relevant = memory::query_by_similarity(&sconn, &query, &project, 3).unwrap_or_default();

    if relevant.is_empty() {
        return empty_response();
    }

    let context_parts: Vec<String> = relevant
        .iter()
        .map(|(m, score)| {
            format!(
                "- [{}] {} (relevance: {:.2})",
                m.action_type,
                truncate_bytes(&m.what, 100),
                score
            )
        })
        .collect();

    let mut context_text = format!("IMI context (tool error detected):\n");
    context_text.push_str(&context_parts.join("\n"));
    context_text = truncate_bytes(&context_text, MID_SESSION_BUDGET);

    write_injection_count(count + 1);

    json!({
        "systemMessage": context_text
    })
}

fn handle_user_prompt_submit(input: &Value) -> Value {
    let count = read_injection_count();
    let duration = session_duration_minutes();

    // Only inject if session has been running >30 min and we haven't injected too much
    if duration < 30.0 || count >= MAX_MID_SESSION_INJECTIONS {
        return empty_response();
    }

    let project = match extract_project(input) {
        Some(p) => p,
        None => return empty_response(),
    };

    let sconn = match crate::session_capture::db::open_sessions_db() {
        Ok(c) => c,
        Err(_) => return empty_response(),
    };

    let _ = init_memories_table(&sconn);

    // Lightweight context refresh — just the delta brief
    let brief = generate_delta_brief(&sconn, &project, None, 0).unwrap_or_default();

    if brief.is_empty() || brief == "No recent activity." {
        return empty_response();
    }

    let context_text = format!("IMI mid-session refresh ({}m elapsed):\n{}", duration as i32, truncate_bytes(&brief, MID_SESSION_BUDGET));

    write_injection_count(count + 1);

    json!({
        "systemMessage": context_text
    })
}

fn handle_stop(input: &Value) -> Value {
    let project = match extract_project(input) {
        Some(p) => p,
        None => return empty_response(),
    };

    let sconn = match crate::session_capture::db::open_sessions_db() {
        Ok(c) => c,
        Err(_) => return empty_response(),
    };

    // Check for unresolved risks
    let has_risks: bool = sconn
        .query_row(
            "SELECT COUNT(*) FROM session_insights
             WHERE project = ?1
               AND json_array_length(files_at_risk_json) > 0
               AND generated_at_ms > ?2
               AND task_completed = 0",
            rusqlite::params![
                project,
                crate::session_capture::types::now_ms() - 30 * 60 * 1000 // last 30 min
            ],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    let has_recent_errors: bool = sconn
        .query_row(
            "SELECT COUNT(*) FROM session_insights
             WHERE project = ?1
               AND json_array_length(failures_observed_json) > 0
               AND generated_at_ms > ?2",
            rusqlite::params![
                project,
                crate::session_capture::types::now_ms() - 30 * 60 * 1000
            ],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    if has_risks || has_recent_errors {
        let reason = if has_risks {
            "IMI: There are files at risk or unresolved failures in this session. Consider running tests or verifying changes before stopping."
        } else {
            "IMI: There are unresolved errors from this session. Consider addressing them before stopping."
        };
        json!({
            "decision": "block",
            "reason": reason
        })
    } else {
        empty_response()
    }
}

fn handle_pre_tool_use(input: &Value) -> Value {
    // Currently no-op — could add file switch detection here
    let _ = input;
    empty_response()
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn extract_file_from_input(input: &Value) -> String {
    // Try tool_input first (PreToolUse), then tool_result
    for key in &["tool_input", "tool_result"] {
        if let Some(obj) = input.get(*key) {
            for path_key in &["file_path", "path", "filename"] {
                if let Some(s) = obj.get(*path_key).and_then(|v| v.as_str()) {
                    if !s.is_empty() && (s.contains('/') || s.contains('.')) {
                        return s.to_string();
                    }
                }
            }
        }
    }
    String::new()
}

fn empty_response() -> Value {
    json!({})
}

fn truncate_bytes(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    if max_bytes <= 3 {
        return "...".to_string();
    }
    let budget = max_bytes - 3;
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > budget {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Read a Claude Code hook event from stdin and return the appropriate
/// response JSON on stdout.
pub fn run_hook_handler() -> Result<(), String> {
    let mut input_str = String::new();
    io::stdin()
        .read_to_string(&mut input_str)
        .map_err(|e| format!("hook-handler stdin read: {e}"))?;

    let input: Value = if input_str.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&input_str).unwrap_or(json!({}))
    };

    // Detect event type from input fields
    let event_type = input
        .get("event_type")
        .or_else(|| input.get("type"))
        .or_else(|| input.get("hookEvent"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let event = HookEvent::from_str(event_type);

    let response = match event {
        HookEvent::SessionStart => handle_session_start(&input),
        HookEvent::SessionEnd => handle_session_end(&input),
        HookEvent::PostToolUse => handle_post_tool_use(&input),
        HookEvent::UserPromptSubmit => handle_user_prompt_submit(&input),
        HookEvent::Stop => handle_stop(&input),
        HookEvent::PreToolUse => handle_pre_tool_use(&input),
        HookEvent::Unknown(_) => empty_response(),
    };

    // Only output non-empty responses — silent no-ops produce no stdout
    if response != json!({}) {
        println!("{}", serde_json::to_string(&response).unwrap_or_default());
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_event_parsing() {
        assert!(matches!(
            HookEvent::from_str("session_start"),
            HookEvent::SessionStart
        ));
        assert!(matches!(
            HookEvent::from_str("SessionStart"),
            HookEvent::SessionStart
        ));
        assert!(matches!(
            HookEvent::from_str("post_tool_use"),
            HookEvent::PostToolUse
        ));
        assert!(matches!(
            HookEvent::from_str("unknown_event"),
            HookEvent::Unknown(_)
        ));
    }

    #[test]
    fn truncate_bytes_works() {
        assert_eq!(truncate_bytes("hello", 10), "hello");
        assert_eq!(truncate_bytes("hello world", 8), "hello...");
        assert_eq!(truncate_bytes("hi", 3), "hi");
    }

    #[test]
    fn test_extract_file_from_input() {
        let input = json!({
            "tool_input": {
                "file_path": "/src/main.rs"
            }
        });
        assert_eq!(super::extract_file_from_input(&input), "/src/main.rs");

        let input_no_file = json!({
            "tool_input": {
                "command": "ls"
            }
        });
        assert_eq!(super::extract_file_from_input(&input_no_file), "");
    }

    #[test]
    fn injection_count_round_trip() {
        // These tests use the real FS, so use a unique marker
        let path = injection_tracker_path();
        let _ = fs::remove_file(&path);

        assert_eq!(read_injection_count(), 0);
        write_injection_count(2);
        assert_eq!(read_injection_count(), 2);
        reset_injection_count();
        assert_eq!(read_injection_count(), 0);
    }

    #[test]
    fn session_start_records_and_resets() {
        record_session_start();
        // Should have written a timestamp
        let ms: i64 = fs::read_to_string(session_start_marker_path())
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0);
        assert!(ms > 0);
        // Injection count should be reset
        assert_eq!(read_injection_count(), 0);
        // Clean up
        let _ = fs::remove_file(session_start_marker_path());
    }

    #[test]
    fn empty_response_is_empty_json() {
        let resp = empty_response();
        assert_eq!(resp, json!({}));
    }

    #[test]
    fn post_tool_use_no_error_is_noop() {
        // Reset state so count is predictable
        reset_injection_count();

        let input = json!({
            "event_type": "post_tool_use",
            "tool_result": {
                "success": true,
                "output": "all good"
            }
        });
        // With success=true, should return empty (no injection)
        let resp = handle_post_tool_use(&input);
        assert_eq!(resp, json!({}));
        // Count should not have changed
        assert_eq!(read_injection_count(), 0);
    }
}
