#![allow(dead_code)]

use crate::session_capture::{db::open_sessions_db, types::now_ms};
use rusqlite::params;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Default)]
struct SessionAggregate {
    source: String,
    project: String,
    cwd: Option<String>,
    start_time_ms: i64,
    end_time_ms: i64,
    files_touched: BTreeSet<String>,
    error_count: i64,
    tool_call_count: i64,
    ended_by: String,
}

pub fn run_indexer(inactivity_secs: u64) -> Result<usize, String> {
    let mut conn = open_sessions_db()?;
    let mut sessions: BTreeMap<String, SessionAggregate> = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "SELECT session_id, source, project, cwd, event_type, timestamp_ms, payload_json
                 FROM raw_events
                 ORDER BY session_id, timestamp_ms, id",
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(|e| e.to_string())?;

        for row in rows {
            let (session_id, source, project, cwd, event_type, timestamp_ms, payload_json) =
                row.map_err(|e| e.to_string())?;
            let agg = sessions.entry(session_id).or_default();

            if agg.source.is_empty() {
                agg.source = source;
            }
            if agg.project.is_empty() {
                agg.project = project
                    .clone()
                    .filter(|p| !p.trim().is_empty())
                    .or_else(|| cwd.clone())
                    .unwrap_or_else(|| "unknown".to_string());
            }
            if agg.cwd.is_none() {
                agg.cwd = cwd.clone();
            }
            if agg.start_time_ms == 0 || timestamp_ms < agg.start_time_ms {
                agg.start_time_ms = timestamp_ms;
            }
            if timestamp_ms > agg.end_time_ms {
                agg.end_time_ms = timestamp_ms;
            }

            match event_type.as_str() {
                "tool_call" => agg.tool_call_count += 1,
                "malformed_json" => agg.error_count += 1,
                "tool_result" if payload_success_is_false(&payload_json) => agg.error_count += 1,
                "session_end" => {
                    agg.ended_by = "session_end".to_string();
                    for file in session_end_files(&payload_json) {
                        agg.files_touched.insert(file);
                    }
                }
                _ => {}
            }
        }
    }

    let now = now_ms();
    let inactivity_ms = (inactivity_secs as i64) * 1000;
    let tx = conn.transaction().map_err(|e| e.to_string())?;

    for (session_id, mut agg) in sessions {
        if agg.project.is_empty() {
            agg.project = agg.cwd.clone().unwrap_or_else(|| "unknown".to_string());
        }
        if agg.ended_by.is_empty() {
            agg.ended_by = if now - agg.end_time_ms >= inactivity_ms {
                "inactivity".to_string()
            } else {
                "active".to_string()
            };
        }

        let duration_minutes = if agg.end_time_ms <= agg.start_time_ms {
            0
        } else {
            ((agg.end_time_ms - agg.start_time_ms) + 59_999) / 60_000
        };
        let files_touched = agg.files_touched.into_iter().collect::<Vec<_>>();
        let files_touched_json =
            serde_json::to_string(&files_touched).unwrap_or_else(|_| "[]".to_string());

        tx.execute(
            "INSERT INTO sessions_summary
                (session_id, source, project, cwd, start_time_ms, end_time_ms, last_event_time_ms,
                 duration_minutes, files_touched_json, files_touched_count, error_count,
                 test_outcomes_json, tool_call_count, ended_by, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, '[]', ?12, ?13, ?14)
             ON CONFLICT(session_id) DO UPDATE SET
                 source = excluded.source,
                 project = excluded.project,
                 cwd = excluded.cwd,
                 start_time_ms = excluded.start_time_ms,
                 end_time_ms = excluded.end_time_ms,
                 last_event_time_ms = excluded.last_event_time_ms,
                 duration_minutes = excluded.duration_minutes,
                 files_touched_json = excluded.files_touched_json,
                 files_touched_count = excluded.files_touched_count,
                 error_count = excluded.error_count,
                 test_outcomes_json = excluded.test_outcomes_json,
                 tool_call_count = excluded.tool_call_count,
                 ended_by = excluded.ended_by,
                 updated_at_ms = excluded.updated_at_ms",
            params![
                session_id,
                agg.source,
                agg.project,
                agg.cwd,
                agg.start_time_ms,
                agg.end_time_ms,
                agg.end_time_ms,
                duration_minutes,
                files_touched_json,
                files_touched.len() as i64,
                agg.error_count,
                agg.tool_call_count,
                agg.ended_by,
                now,
            ],
        )
        .map_err(|e| e.to_string())?;
    }

    tx.commit().map_err(|e| e.to_string())?;
    Ok(sessions_len(&conn)?)
}

fn payload_success_is_false(payload_json: &str) -> bool {
    serde_json::from_str::<Value>(payload_json)
        .ok()
        .and_then(|value| value.get("success").and_then(Value::as_bool))
        == Some(false)
}

fn session_end_files(payload_json: &str) -> Vec<String> {
    serde_json::from_str::<Value>(payload_json)
        .ok()
        .and_then(|value| value.get("files_modified").cloned())
        .and_then(|value| value.as_array().cloned())
        .map(|files| {
            files
                .into_iter()
                .filter_map(|file| file.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

fn sessions_len(conn: &rusqlite::Connection) -> Result<usize, String> {
    conn.query_row("SELECT COUNT(*) FROM sessions_summary", [], |row| {
        row.get::<_, i64>(0)
    })
    .map(|count| count as usize)
    .map_err(|e| e.to_string())
}
