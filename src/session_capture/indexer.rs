#![allow(dead_code)]
use crate::session_capture::db::open_sessions_db;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

/// Materialize sessions_summary for all sessions that have ended
/// (either explicit session_end event or inactivity > inactivity_secs).
/// Safe to call multiple times — uses INSERT OR REPLACE (idempotent).
pub fn run_indexer(inactivity_secs: i64) -> Result<usize, String> {
    let conn = open_sessions_db()?;
    let now_ms = crate::session_capture::types::now_ms();
    let inactivity_threshold_ms = inactivity_secs * 1000;

    // Get all distinct session_ids that have events
    let mut stmt = conn
        .prepare("SELECT DISTINCT session_id FROM raw_events ORDER BY session_id")
        .map_err(|e| e.to_string())?;

    let session_ids: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let mut materialized = 0;
    for session_id in &session_ids {
        // Get session bounds
        let bounds: Option<(i64, i64, i64, String, Option<String>, Option<String>)> = conn
            .query_row(
                "SELECT min(timestamp_ms), max(timestamp_ms), count(*),
                    source,
                    (SELECT cwd FROM raw_events WHERE session_id=?1 AND cwd IS NOT NULL LIMIT 1),
                    (SELECT project FROM raw_events WHERE session_id=?1 AND project IS NOT NULL LIMIT 1)
             FROM raw_events WHERE session_id=?1",
                params![session_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .map_err(|e| e.to_string())?;

        let (start_ms, last_event_ms, tool_call_count, source, cwd, project) = match bounds {
            Some(b) => b,
            None => continue,
        };

        // Determine if session has ended
        let has_explicit_end: bool = conn
            .query_row(
                "SELECT count(*) FROM raw_events WHERE session_id=?1 AND event_type='session_end'",
                params![session_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(0)
            > 0;

        let is_inactive = (now_ms - last_event_ms) > inactivity_threshold_ms;

        if !has_explicit_end && !is_inactive {
            continue; // Session still active, skip
        }

        let ended_by = if has_explicit_end {
            "explicit"
        } else {
            "inactivity"
        };

        // Compute end_time_ms
        let end_ms = if has_explicit_end {
            conn.query_row(
                "SELECT timestamp_ms FROM raw_events WHERE session_id=?1 AND event_type='session_end' LIMIT 1",
                params![session_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap_or(last_event_ms)
        } else {
            last_event_ms
        };

        let duration_minutes = ((end_ms - start_ms) / 1000 / 60).max(0);

        // Files touched — from session_end payload (filesModified) first, then tool_call args
        let files_touched_json = compute_files_touched(&conn, session_id, &project)?;
        let files_touched: Vec<String> =
            serde_json::from_str(&files_touched_json).unwrap_or_default();
        let files_touched_count = files_touched.len() as i64;

        // Error count — tool_result events with success=false
        let error_count: i64 = conn
            .query_row(
                "SELECT count(*) FROM raw_events
             WHERE session_id=?1 AND event_type='tool_result'
             AND json_extract(payload_json, '$.success') = 0",
                params![session_id],
                |r| r.get(0),
            )
            .unwrap_or(0);

        // Test outcomes — bash tool results matching test patterns
        let test_outcomes_json = compute_test_outcomes(&conn, session_id)?;

        let project_val = project.as_deref().unwrap_or(cwd.as_deref().unwrap_or("unknown"));

        conn.execute(
            "INSERT OR REPLACE INTO sessions_summary
             (session_id, source, project, cwd, start_time_ms, end_time_ms,
              last_event_time_ms, duration_minutes, files_touched_json,
              files_touched_count, error_count, test_outcomes_json,
              tool_call_count, ended_by, updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                session_id,
                source,
                project_val,
                cwd,
                start_ms,
                end_ms,
                last_event_ms,
                duration_minutes,
                files_touched_json,
                files_touched_count,
                error_count,
                test_outcomes_json,
                tool_call_count,
                ended_by,
                crate::session_capture::types::now_ms(),
            ],
        )
        .map_err(|e| e.to_string())?;

        materialized += 1;
    }

    Ok(materialized)
}

fn compute_files_touched(
    conn: &Connection,
    session_id: &str,
    project: &Option<String>,
) -> Result<String, String> {
    let mut files: std::collections::HashSet<String> = std::collections::HashSet::new();

    // 1. Best source: session_end event filesModified
    let end_payload: Option<String> = conn
        .query_row(
            "SELECT payload_json FROM raw_events WHERE session_id=?1 AND event_type='session_end' LIMIT 1",
            params![session_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    if let Some(payload) = end_payload {
        if let Ok(v) = serde_json::from_str::<Value>(&payload) {
            if let Some(arr) = v["files_modified"].as_array() {
                for f in arr {
                    if let Some(s) = f.as_str() {
                        files.insert(normalize_path(s, project.as_deref()));
                    }
                }
            }
        }
    }

    // 2. Fallback: extract paths from tool_call payload_json arguments
    // Look for known path-bearing fields: path, file_path, filename
    if files.is_empty() {
        let mut stmt = conn
            .prepare(
                "SELECT payload_json FROM raw_events
             WHERE session_id=?1 AND event_type='tool_call'
             AND (tool_name IN ('edit','create','view') OR payload_json LIKE '%\"path\"%')",
            )
            .map_err(|e| e.to_string())?;

        let payloads: Vec<String> = stmt
            .query_map(params![session_id], |r| r.get(0))
            .map_err(|e| e.to_string())?
            .filter_map(|r| r.ok())
            .collect();

        for payload in payloads {
            if let Ok(v) = serde_json::from_str::<Value>(&payload) {
                for key in &["path", "file_path", "filename"] {
                    if let Some(s) = v[*key].as_str() {
                        if looks_like_file_path(s) {
                            files.insert(normalize_path(s, project.as_deref()));
                        }
                    }
                }
            }
        }
    }

    let mut sorted: Vec<String> = files.into_iter().collect();
    sorted.sort();
    Ok(serde_json::to_string(&sorted).unwrap_or_else(|_| "[]".to_string()))
}

fn compute_test_outcomes(conn: &Connection, session_id: &str) -> Result<String, String> {
    let mut stmt = conn
        .prepare(
            "SELECT payload_json FROM raw_events
         WHERE session_id=?1 AND event_type='tool_result'
         AND tool_name='bash'",
        )
        .map_err(|e| e.to_string())?;

    let mut outcomes = Vec::new();
    let payloads: Vec<String> = stmt
        .query_map(params![session_id], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    for payload in payloads {
        if let Ok(v) = serde_json::from_str::<Value>(&payload) {
            let output = v["output"].as_str().unwrap_or("");
            let success = v["success"].as_bool().unwrap_or(true);

            // Detect test runs
            if output.contains("test result:")
                || output.contains("tests passed")
                || output.contains("cargo test")
            {
                let status = if output.contains("FAILED") || output.contains("test result: FAILED")
                {
                    "fail"
                } else if output.contains("test result: ok") || output.contains("tests passed") {
                    "pass"
                } else {
                    continue;
                };
                outcomes.push(serde_json::json!({"kind":"test","status":status}));
            }
            // Detect build
            else if output.contains("cargo build") || output.contains("Compiling") {
                let status = if !success || output.contains("error[") {
                    "fail"
                } else {
                    "pass"
                };
                outcomes.push(serde_json::json!({"kind":"build","status":status}));
            }
        }
    }

    outcomes.dedup_by_key(|o| o["kind"].as_str().unwrap_or("").to_string());
    Ok(serde_json::to_string(&outcomes).unwrap_or_else(|_| "[]".to_string()))
}

fn normalize_path(path: &str, project: Option<&str>) -> String {
    // If path starts with project root, make it relative
    if let Some(proj) = project {
        if let Some(rel) = path.strip_prefix(proj) {
            return rel.trim_start_matches('/').to_string();
        }
    }
    path.to_string()
}

fn looks_like_file_path(s: &str) -> bool {
    s.starts_with('/') || s.starts_with("./") || s.contains('.')
}
