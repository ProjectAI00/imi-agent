#![allow(dead_code)]

use crate::session_capture::{
    db::open_sessions_db,
    memory::{self, Memory},
    memory_loop::{
        compute_retrieval_score, detect_and_resolve_contradiction, evolve_truth_status,
        extract_causal_tuples, map_intervention_signal, should_exclude_from_default_retrieval,
        to_json_string, AgentStep, TruthLifecycleSignal, TruthStatus,
    },
    types::{now_ms, EventMeta, SessionEvent, ToolCallEvent, ToolResultEvent, UserMessageEvent},
};
use regex::Regex;
use rusqlite::params;
use serde_json::Value;
use std::collections::HashSet;

struct Msg {
    speaker: String,
    text: String,
}

/// Run the compression function for a specific session_id.
/// Extracts decisions, failures, task_completed, files_at_risk, summary_text
/// and writes to session_insights.
/// Returns Ok(true) if insight was written, Ok(false) if session not found in summary.
pub fn compress_session(session_id: &str) -> Result<bool, String> {
    let conn = open_sessions_db()?;

    let summary_exists = conn
        .query_row(
            "SELECT count(*) FROM sessions_summary WHERE session_id=?1",
            params![session_id],
            |r| r.get::<_, i64>(0),
        )
        .unwrap_or(0)
        > 0;

    if !summary_exists {
        return Ok(false);
    }

    let (project, duration_minutes, files_touched_count, error_count, tool_call_count): (
        String,
        i64,
        i64,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT project, duration_minutes, files_touched_count, error_count, tool_call_count
             FROM sessions_summary WHERE session_id=?1",
            params![session_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|e| e.to_string())?;

    let mut msg_stmt = conn
        .prepare(
            "SELECT event_type, payload_json FROM raw_events
             WHERE session_id=?1 AND event_type IN ('user_message','assistant_message')
             ORDER BY timestamp_ms",
        )
        .map_err(|e| e.to_string())?;

    let messages: Vec<Msg> = msg_stmt
        .query_map(params![session_id], |r| {
            Ok(Msg {
                speaker: r.get(0)?,
                text: r.get(1)?,
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .map(|m| {
            let text = serde_json::from_str::<String>(&m.text).unwrap_or(m.text);
            Msg {
                speaker: m.speaker,
                text,
            }
        })
        .collect();

    let decisions = extract_decisions(&messages);
    let normalized_events = load_normalized_events(&conn, session_id)?;
    let causal_tuples = extract_causal_tuples(&normalized_events);

    let mut fail_stmt = conn
        .prepare(
            "SELECT r.payload_json, r.tool_name, r.call_id,
                    (SELECT c.payload_json FROM raw_events c
                     WHERE c.session_id=?1 AND c.event_type='tool_call' AND c.call_id=r.call_id LIMIT 1)
             FROM raw_events r
             WHERE r.session_id=?1 AND r.event_type='tool_result'
             AND json_extract(r.payload_json, '$.success') = 0
             LIMIT 20",
        )
        .map_err(|e| e.to_string())?;

    let failures: Vec<Value> = fail_stmt
        .query_map(params![session_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .map(|(result_payload, tool_name, _call_id, call_payload)| {
            let result: Value = serde_json::from_str(&result_payload).unwrap_or(Value::Null);
            let call: Value = call_payload
                .as_deref()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or(Value::Null);

            let error_excerpt = result["output"]
                .as_str()
                .unwrap_or("")
                .lines()
                .filter(|l| !l.trim().is_empty())
                .take(3)
                .collect::<Vec<_>>()
                .join(" | ");
            let error_excerpt = truncate(&error_excerpt, 200);

            let files = extract_path_from_value(&call);

            serde_json::json!({
                "tool_name": tool_name.unwrap_or_default(),
                "error_excerpt": error_excerpt,
                "files": files,
            })
        })
        .collect();

    let mut files_at_risk: Vec<String> = failures
        .iter()
        .flat_map(|f| {
            f["files"]
                .as_array()
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    files_at_risk.sort();

    let task_completed = infer_task_completed(&conn, session_id, files_touched_count)?;

    let proj_short = project
        .split('/')
        .rev()
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("/");
    let top_activity = if tool_call_count > 0 {
        format!("{tool_call_count} tool calls")
    } else {
        "minimal tool activity".to_string()
    };
    let outcome_str = if task_completed {
        "Session ended with successful verification."
    } else if error_count > 0 {
        "Session ended with unresolved errors."
    } else {
        "Session ended without explicit verification."
    };
    let summary_text = format!(
        "Worked in `{}` for {}m, touching {} file(s) across {}. {} error(s) observed. {}",
        proj_short, duration_minutes, files_touched_count, top_activity, error_count, outcome_str
    );

    let prior_truth_status = latest_truth_status_for_project(&conn, &project)?;
    let contradiction_signal = !failures.is_empty() && task_completed;
    let supersession_signal = task_completed && error_count == 0 && !causal_tuples.is_empty();
    let contradiction = detect_and_resolve_contradiction(
        prior_truth_status,
        0.8,
        &project,
        &project,
        contradiction_signal,
        supersession_signal && prior_truth_status == TruthStatus::Validated,
    );
    let truth_status = if contradiction.contradiction_detected {
        contradiction.status
    } else if task_completed && error_count == 0 {
        evolve_truth_status(prior_truth_status, TruthLifecycleSignal::ValidationEvidence)
    } else if error_count > 0 {
        evolve_truth_status(prior_truth_status, TruthLifecycleSignal::AmbiguousEvidence)
    } else {
        prior_truth_status
    };

    let importance = memory::compute_importance(
        files_touched_count,
        error_count,
        tool_call_count,
        duration_minutes,
        task_completed,
        decisions.len(),
        failures.len(),
    );
    let relevance = (importance as f32 / 10.0).clamp(0.0, 1.0);
    let recency = memory::compute_recency_decay(now_ms(), now_ms());
    let evidence_strength = ((causal_tuples.len() as f32 * 0.2)
        + if task_completed { 0.4 } else { 0.0 }
        + if failures.is_empty() { 0.2 } else { 0.0 })
    .clamp(0.0, 1.0);
    let score = compute_retrieval_score(relevance, recency, evidence_strength, truth_status);
    let total_score = if should_exclude_from_default_retrieval(truth_status) {
        0.0
    } else {
        score.total
    };

    let intervention_signal = map_intervention_signal(
        if task_completed {
            AgentStep::Complete
        } else if error_count > 0 {
            AgentStep::Verify
        } else {
            AgentStep::Execute
        },
        contradiction.confidence,
        contradiction.contradiction_detected,
    );

    conn.execute(
        "INSERT OR REPLACE INTO session_insights
         (session_id, project, generated_at_ms, decisions_observed_json,
          failures_observed_json, task_completed, files_at_risk_json, summary_text,
          causal_tuples_json, truth_status, contradiction_detected, contradiction_policy,
          contradiction_note, retrieval_relevance, retrieval_recency, retrieval_evidence_strength,
          retrieval_truth_component, retrieval_total_score, intervention_signal)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19)",
        params![
            session_id,
            project,
            now_ms(),
            to_json_string(&decisions),
            to_json_string(&failures),
            task_completed as i64,
            to_json_string(&files_at_risk),
            summary_text,
            to_json_string(&causal_tuples),
            truth_status.as_str(),
            contradiction.contradiction_detected as i64,
            contradiction.policy.as_str(),
            contradiction.note,
            score.relevance,
            score.recency,
            score.evidence_strength,
            score.truth_status,
            total_score,
            intervention_signal.as_str(),
        ],
    )
    .map_err(|e| e.to_string())?;

    // Parallel write to memories table for unified retrieval.
    let importance = memory::compute_importance(
        files_touched_count,
        error_count,
        tool_call_count,
        duration_minutes,
        task_completed,
        decisions.len(),
        failures.len(),
    );
    let mem = Memory {
        id: 0, // auto-generated
        env_id: project.clone(),
        domain: "coding".to_string(),
        level: 1, // session-level
        what: summary_text,
        result: if task_completed {
            "good".to_string()
        } else {
            "neutral".to_string()
        },
        context: session_id.to_string(),
        action_type: "session_summary".to_string(),
        outcome_detail: serde_json::json!({
            "session_id": session_id,
            "causal_tuples": causal_tuples,
            "decisions": decisions,
            "failures": failures,
        })
        .to_string(),
        importance: importance as i32,
        embedding: None,
        confidence: total_score as f64,
        truth_status: truth_status.as_str().to_string(),
        evidence_for: 0,
        evidence_against: 0,
        source_ids: serde_json::json!([format!("compress:{session_id}")]).to_string(),
        created_at: now_ms(),
        last_accessed: now_ms(),
    };
    memory::insert_memory(&conn, &mem).map_err(|e| e.to_string())?;

    Ok(true)
}

/// Run compress_session for ALL sessions in sessions_summary that don't have insights yet.
pub fn compress_all_pending(limit: usize) -> Result<usize, String> {
    let conn = open_sessions_db()?;

    let mut stmt = conn
        .prepare(
            "SELECT ss.session_id FROM sessions_summary ss
             LEFT JOIN session_insights si ON ss.session_id = si.session_id
             WHERE si.session_id IS NULL
             ORDER BY ss.end_time_ms DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;

    let ids: Vec<String> = stmt
        .query_map(params![limit as i64], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let mut count = 0;
    for id in &ids {
        match compress_session(id) {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(e) => eprintln!("[compress] Error on {id}: {e}"),
        }
    }

    Ok(count)
}

fn extract_decisions(messages: &[Msg]) -> Vec<Value> {
    let patterns = [
        r"(?i)\bremember(?:\s+that)?\b",
        r"(?i)\balways\b",
        r"(?i)\bnever\b",
        r"(?i)\b(?:we\s+)?decided\b",
        r"(?i)\bdon['’]?t\b",
        r"(?i)\bdo\s+not\b",
        r"(?i)\bmust\b",
        r"(?i)\bfrom\s+now\s+on\b",
        r"(?i)\bprefer\b",
    ];
    let regexes: Vec<Regex> = patterns.iter().filter_map(|p| Regex::new(p).ok()).collect();

    let mut seen: HashSet<String> = HashSet::new();
    let mut results = Vec::new();

    for msg in messages {
        for sentence in msg.text.split(['.', '\n', '!', '?']) {
            let sentence = sentence.trim();
            if sentence.len() < 15 {
                continue;
            }
            if regexes.iter().any(|re| re.is_match(sentence)) {
                let key = sentence.to_lowercase();
                if seen.insert(key) {
                    let speaker = if msg.speaker.contains("user") {
                        "user"
                    } else {
                        "assistant"
                    };
                    results.push(serde_json::json!({
                        "speaker": speaker,
                        "text": truncate(sentence, 220),
                    }));
                    if results.len() >= 10 {
                        break;
                    }
                }
            }
        }
        if results.len() >= 10 {
            break;
        }
    }

    results
}

fn infer_task_completed(
    conn: &rusqlite::Connection,
    session_id: &str,
    files_touched: i64,
) -> Result<bool, String> {
    if files_touched < 2 {
        return Ok(false);
    }

    let success_patterns = [
        "test result: ok",
        "tests passed",
        "all tests passed",
        "Finished dev",
        "build succeeded",
        "0 failed",
    ];

    let mut stmt = conn
        .prepare(
            "SELECT payload_json FROM raw_events
             WHERE session_id=?1 AND event_type='tool_result' AND tool_name='bash'
             ORDER BY timestamp_ms DESC LIMIT 20",
        )
        .map_err(|e| e.to_string())?;

    let outputs: Vec<String> = stmt
        .query_map(params![session_id], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    for payload in &outputs {
        if let Ok(v) = serde_json::from_str::<Value>(payload) {
            let output = v["output"].as_str().unwrap_or("");
            if success_patterns.iter().any(|p| output.contains(p)) {
                return Ok(true);
            }
        }
    }

    Ok(false)
}

fn extract_path_from_value(v: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    for key in ["path", "file_path", "filename"] {
        if let Some(s) = v[key].as_str() {
            if s.contains('/') || s.contains('.') {
                paths.push(s.to_string());
            }
        }
    }
    paths
}

fn truncate(s: &str, max_chars: usize) -> String {
    let char_count = s.chars().count();
    if char_count <= max_chars {
        return s.to_string();
    }

    let truncated: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{truncated}…")
}

fn latest_truth_status_for_project(
    conn: &rusqlite::Connection,
    project: &str,
) -> Result<TruthStatus, String> {
    let status = conn
        .query_row(
            "SELECT truth_status
             FROM session_insights
             WHERE project = ?1
             ORDER BY generated_at_ms DESC
             LIMIT 1",
            params![project],
            |r| r.get::<_, String>(0),
        )
        .ok();
    Ok(status
        .as_deref()
        .map(TruthStatus::parse)
        .unwrap_or(TruthStatus::Uncertain))
}

fn load_normalized_events(
    conn: &rusqlite::Connection,
    session_id: &str,
) -> Result<Vec<SessionEvent>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT source, session_id, timestamp_ms, cwd, project, event_type, payload_json, tool_name, call_id
             FROM raw_events
             WHERE session_id=?1
               AND event_type IN ('tool_call','tool_result','user_message')
             ORDER BY timestamp_ms, id",
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })
        .map_err(|e| e.to_string())?;

    let mut events = Vec::new();
    for row in rows {
        let (source, sid, timestamp_ms, cwd, project, event_type, payload_json, tool_name, call_id) =
            row.map_err(|e| e.to_string())?;
        let meta = EventMeta {
            source: if source == "claude" {
                crate::session_capture::types::SourceKind::Claude
            } else {
                crate::session_capture::types::SourceKind::Copilot
            },
            session_id: sid,
            timestamp_ms,
            cwd,
            project,
            raw_type: event_type.clone(),
        };
        match event_type.as_str() {
            "tool_call" => {
                let arguments: Value = serde_json::from_str(&payload_json).unwrap_or(Value::Null);
                events.push(SessionEvent::ToolCall(ToolCallEvent {
                    meta,
                    call_id,
                    tool_name: tool_name.unwrap_or_else(|| "unknown".to_string()),
                    arguments,
                }));
            }
            "tool_result" => {
                let payload: Value = serde_json::from_str(&payload_json).unwrap_or(Value::Null);
                events.push(SessionEvent::ToolResult(ToolResultEvent {
                    meta,
                    call_id,
                    tool_name: tool_name.unwrap_or_else(String::new),
                    success: payload
                        .get("success")
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    output: payload
                        .get("output")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                }));
            }
            "user_message" => {
                let text = serde_json::from_str::<String>(&payload_json).unwrap_or(payload_json);
                events.push(SessionEvent::UserMessage(UserMessageEvent { meta, text }));
            }
            _ => {}
        }
    }
    Ok(events)
}
