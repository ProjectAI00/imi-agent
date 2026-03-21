#![allow(dead_code)]

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use std::collections::HashSet;
use std::path::PathBuf;

const TOTAL_BRIEF_BUDGET: usize = 2048;
const ACTIVE_GOAL_BUDGET: usize = 350;
const WHERE_LEFT_OFF_BUDGET: usize = 700;
const KNOWN_RISKS_BUDGET: usize = 350;
const DECISIONS_BUDGET: usize = 350;
const NEXT_TASK_BUDGET: usize = 250;

struct GoalBrief {
    id: String,
    name: String,
    description: String,
    priority: String,
    status: String,
    success_signal: String,
}

struct TaskBrief {
    title: String,
    why: String,
    acceptance_criteria: String,
    relevant_files: String,
}

struct SessionBrief {
    summary_text: String,
    duration_secs: i64,
    tool_calls: i64,
    error_count: i64,
    task_completed: bool,
    decisions: Vec<String>,
    failures: Vec<String>,
    files_at_risk: Vec<String>,
}

pub fn generate_brief(sessions_conn: &Connection, project: &str) -> Result<String, String> {
    let session_insights_cols = table_columns(sessions_conn, "session_insights")?;
    let sessions_summary_cols = table_columns(sessions_conn, "sessions_summary")?;

    let recent_sessions =
        load_recent_sessions(sessions_conn, project, &session_insights_cols, &sessions_summary_cols)?;

    let state_conn = open_state_db();
    let active_goal = state_conn.as_ref().and_then(load_active_goal);
    let next_task = match (&state_conn, &active_goal) {
        (Some(conn), Some(goal)) => load_next_task(conn, &goal.id),
        _ => None,
    };
    let state_decisions = state_conn
        .as_ref()
        .map(load_state_decisions)
        .unwrap_or_default();

    let active_goal_body = build_active_goal_body(state_conn.is_some(), active_goal.as_ref());
    let where_left_off_body = build_where_left_off_body(&recent_sessions);
    let known_risks_body = build_known_risks_body(&recent_sessions);
    let decisions_body = build_decisions_body(state_conn.is_some(), &state_decisions, &recent_sessions);
    let next_task_body = build_next_task_body(state_conn.is_some(), next_task.as_ref());

    let mut sections = vec![
        render_section("ACTIVE GOAL", &active_goal_body, ACTIVE_GOAL_BUDGET),
        render_section("WHERE WE LEFT OFF", &where_left_off_body, WHERE_LEFT_OFF_BUDGET),
        render_section("KNOWN RISKS", &known_risks_body, KNOWN_RISKS_BUDGET),
        render_section("DECISIONS IN EFFECT", &decisions_body, DECISIONS_BUDGET),
        render_section("NEXT TASK", &next_task_body, NEXT_TASK_BUDGET),
    ];

    shrink_sections_to_total(&mut sections, TOTAL_BRIEF_BUDGET);

    Ok(sections.join("\n").trim_end().to_string())
}

fn table_columns(conn: &Connection, table: &str) -> Result<HashSet<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;

    let mut cols = HashSet::new();
    for row in rows {
        cols.insert(row.map_err(|e| e.to_string())?);
    }
    Ok(cols)
}

fn open_state_db() -> Option<Connection> {
    let path = state_db_path();
    if !path.exists() {
        return None;
    }
    Connection::open(path).ok()
}

fn state_db_path() -> PathBuf {
    if let Ok(path) = std::env::var("IMI_DB") {
        return PathBuf::from(path);
    }

    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".imi").join("state.db");
    }

    PathBuf::from(".imi").join("state.db")
}

fn load_active_goal(conn: &Connection) -> Option<GoalBrief> {
    conn.query_row(
        "SELECT id,
                COALESCE(name, ''),
                COALESCE(description, ''),
                COALESCE(priority, ''),
                COALESCE(status, ''),
                COALESCE(success_signal, '')
         FROM goals
         WHERE COALESCE(status, '') != 'done'
         ORDER BY CASE LOWER(COALESCE(priority, ''))
                    WHEN '1' THEN 1
                    WHEN 'critical' THEN 1
                    WHEN '2' THEN 2
                    WHEN 'high' THEN 2
                    WHEN '3' THEN 3
                    WHEN 'medium' THEN 3
                    WHEN '4' THEN 4
                    WHEN 'low' THEN 4
                    ELSE 99
                  END ASC,
                  id ASC
         LIMIT 1",
        [],
        |row| {
            Ok(GoalBrief {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                priority: normalize_priority(&row.get::<_, String>(3)?),
                status: normalize_status(&row.get::<_, String>(4)?),
                success_signal: row.get(5)?,
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

fn load_next_task(conn: &Connection, goal_id: &str) -> Option<TaskBrief> {
    conn.query_row(
        "SELECT COALESCE(title, ''),
                COALESCE(why, ''),
                COALESCE(acceptance_criteria, ''),
                COALESCE(relevant_files, '[]')
         FROM tasks
         WHERE goal_id = ?1
           AND LOWER(COALESCE(status, '')) IN ('pending', 'in_progress', 'todo', 'active')
         ORDER BY CASE LOWER(COALESCE(status, ''))
                    WHEN 'in_progress' THEN 0
                    WHEN 'active' THEN 0
                    WHEN 'pending' THEN 1
                    WHEN 'todo' THEN 1
                    ELSE 2
                  END ASC,
                  CASE LOWER(COALESCE(priority, ''))
                    WHEN '1' THEN 1
                    WHEN 'critical' THEN 1
                    WHEN '2' THEN 2
                    WHEN 'high' THEN 2
                    WHEN '3' THEN 3
                    WHEN 'medium' THEN 3
                    WHEN '4' THEN 4
                    WHEN 'low' THEN 4
                    ELSE 99
                  END ASC,
                  id ASC
         LIMIT 1",
        params![goal_id],
        |row| {
            let relevant_files: String = row.get(3)?;
            Ok(TaskBrief {
                title: row.get(0)?,
                why: row.get(1)?,
                acceptance_criteria: row.get(2)?,
                relevant_files: decode_string_list(&relevant_files).join(", "),
            })
        },
    )
    .optional()
    .ok()
    .flatten()
}

fn load_state_decisions(conn: &Connection) -> Vec<String> {
    let mut stmt = match conn.prepare(
        "SELECT COALESCE(what, '')
         FROM decisions
         ORDER BY COALESCE(created_at, 0) DESC, id DESC
         LIMIT 5",
    ) {
        Ok(stmt) => stmt,
        Err(_) => return Vec::new(),
    };

    let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
        Ok(rows) => rows,
        Err(_) => return Vec::new(),
    };

    rows.filter_map(Result::ok)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn load_recent_sessions(
    conn: &Connection,
    project: &str,
    insight_cols: &HashSet<String>,
    summary_cols: &HashSet<String>,
) -> Result<Vec<SessionBrief>, String> {
    let decisions_col = pick_col(insight_cols, &["decisions", "decisions_observed_json"]);
    let failures_col = pick_col(insight_cols, &["failures", "failures_observed_json"]);
    let risks_col = pick_col(insight_cols, &["files_at_risk", "files_at_risk_json"]);
    let sort_expr = if insight_cols.contains("compressed_at_ms") {
        "COALESCE(si.compressed_at_ms, 0)"
    } else if insight_cols.contains("generated_at_ms") {
        "COALESCE(si.generated_at_ms, 0)"
    } else if summary_cols.contains("ended_at_ms") {
        "COALESCE(ss.ended_at_ms, 0)"
    } else {
        "COALESCE(ss.end_time_ms, 0)"
    };
    let duration_expr = if summary_cols.contains("duration_secs") {
        "COALESCE(ss.duration_secs, 0)"
    } else if summary_cols.contains("duration_minutes") {
        "COALESCE(ss.duration_minutes, 0) * 60"
    } else {
        "0"
    };
    let tool_expr = if summary_cols.contains("tool_calls") {
        "COALESCE(ss.tool_calls, 0)"
    } else {
        "COALESCE(ss.tool_call_count, 0)"
    };
    let error_expr = "COALESCE(ss.error_count, 0)";
    let task_expr = "COALESCE(si.task_completed, 0)";

    let sql = format!(
        "SELECT COALESCE(si.summary_text, ''),
                {duration_expr},
                {tool_expr},
                {error_expr},
                {task_expr},
                COALESCE(si.{decisions_col}, '[]'),
                COALESCE(si.{failures_col}, '[]'),
                COALESCE(si.{risks_col}, '[]')
         FROM session_insights si
         LEFT JOIN sessions_summary ss ON ss.session_id = si.session_id
         WHERE si.project = ?1
         ORDER BY {sort_expr} DESC
         LIMIT 2"
    );

    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params![project], |row| {
            let decisions_json: String = row.get(5)?;
            let failures_json: String = row.get(6)?;
            let risks_json: String = row.get(7)?;
            Ok(SessionBrief {
                summary_text: row.get(0)?,
                duration_secs: row.get(1)?,
                tool_calls: row.get(2)?,
                error_count: row.get(3)?,
                task_completed: row.get::<_, i64>(4)? != 0,
                decisions: decode_json_array(&decisions_json, JsonKind::Decision),
                failures: decode_json_array(&failures_json, JsonKind::Failure),
                files_at_risk: decode_json_array(&risks_json, JsonKind::FilePath),
            })
        })
        .map_err(|e| e.to_string())?;

    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row.map_err(|e| e.to_string())?);
    }
    Ok(sessions)
}

fn pick_col<'a>(cols: &'a HashSet<String>, candidates: &[&'a str]) -> &'a str {
    candidates
        .iter()
        .copied()
        .find(|name| cols.contains(*name))
        .unwrap_or(candidates[0])
}

fn build_active_goal_body(state_available: bool, goal: Option<&GoalBrief>) -> String {
    if !state_available {
        return "unavailable".to_string();
    }

    match goal {
        Some(goal) => {
            let mut lines = vec![format!(
                "{} — {}",
                clean_inline(&goal.name),
                clean_inline(&goal.description)
            )];
            lines.push(format!(
                "Priority: {}  Status: {}",
                goal.priority, goal.status
            ));
            lines.push(format!(
                "Success: {}",
                empty_fallback(&goal.success_signal, "unavailable")
            ));
            lines.join("\n")
        }
        None => "No active goal found.".to_string(),
    }
}

fn build_where_left_off_body(sessions: &[SessionBrief]) -> String {
    if sessions.is_empty() {
        return "No sessions recorded yet.".to_string();
    }

    sessions
        .iter()
        .map(|session| {
            format!(
                "- {}\n  Duration: {}  Tool calls: {}  Errors: {}  Task completed: {}",
                clean_inline(&session.summary_text),
                duration_minutes_label(session.duration_secs),
                session.tool_calls,
                session.error_count,
                yes_no(session.task_completed),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_known_risks_body(sessions: &[SessionBrief]) -> String {
    if sessions.is_empty() {
        return "None noted.".to_string();
    }

    let mut files_seen = HashSet::new();
    let mut files = Vec::new();
    for session in sessions.iter().take(2) {
        for file in &session.files_at_risk {
            let cleaned = clean_inline(file);
            if !cleaned.is_empty() && files_seen.insert(cleaned.clone()) {
                files.push(cleaned);
            }
        }
    }

    let mut lines = Vec::new();
    if files.is_empty() {
        lines.push("Files at risk: none".to_string());
    } else {
        lines.push("Files at risk:".to_string());
        lines.extend(files.into_iter().map(|file| format!("- {file}")));
    }

    let failures: Vec<String> = sessions
        .iter()
        .take(2)
        .flat_map(|session| session.failures.iter().take(3))
        .map(|failure| format!("- {}", clean_inline(failure)))
        .collect();

    if failures.is_empty() {
        lines.push("Failures: none".to_string());
    } else {
        lines.push("Failures:".to_string());
        lines.extend(failures);
    }

    lines.join("\n")
}

fn build_decisions_body(
    state_available: bool,
    state_decisions: &[String],
    sessions: &[SessionBrief],
) -> String {
    if !state_available {
        return "unavailable".to_string();
    }

    let mut lines = Vec::new();
    let mut seen = HashSet::new();

    for decision in state_decisions {
        let cleaned = clean_inline(decision);
        if !cleaned.is_empty() && seen.insert(cleaned.clone()) {
            lines.push(format!("- {cleaned}"));
        }
    }

    if let Some(session) = sessions.first() {
        for decision in &session.decisions {
            let cleaned = clean_inline(decision);
            if !cleaned.is_empty() && seen.insert(cleaned.clone()) {
                lines.push(format!("- {cleaned}"));
            }
        }
    }

    if lines.is_empty() {
        "No decisions recorded.".to_string()
    } else {
        lines.join("\n")
    }
}

fn build_next_task_body(state_available: bool, task: Option<&TaskBrief>) -> String {
    if !state_available {
        return "unavailable".to_string();
    }

    match task {
        Some(task) => vec![
            clean_inline(&task.title),
            format!("Why: {}", empty_fallback(&task.why, "unavailable")),
            format!(
                "Acceptance: {}",
                empty_fallback(&task.acceptance_criteria, "unavailable")
            ),
            format!("Files: {}", empty_fallback(&task.relevant_files, "unavailable")),
        ]
        .join("\n"),
        None => "No pending task found.".to_string(),
    }
}

fn render_section(header: &str, body: &str, max_bytes: usize) -> String {
    let prefix = format!("## {header}\n");
    let suffix = "\n";
    let body_budget = max_bytes.saturating_sub(prefix.len() + suffix.len());
    format!("{prefix}{}{suffix}", trunc(body.trim(), body_budget))
}

fn shrink_sections_to_total(sections: &mut [String], max_total: usize) {
    loop {
        let total = sections.iter().map(|s| s.len()).sum::<usize>() + sections.len().saturating_sub(1);
        if total < max_total {
            break;
        }

        let mut shrunk = false;
        for idx in (0..sections.len()).rev() {
            if sections[idx].ends_with("…\n") || sections[idx].ends_with("…") {
                continue;
            }
            if let Some((header, body)) = sections[idx].split_once('\n') {
                let body = body.trim_end();
                if body.len() > 8 {
                    let new_body = trunc(body, body.len().saturating_sub(8));
                    sections[idx] = format!("{header}\n{new_body}\n");
                    shrunk = true;
                    break;
                }
            }
        }

        if !shrunk {
            break;
        }
    }
}

fn trunc(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }

    if max <= "…".len() {
        return "…".to_string();
    }

    let mut out = String::new();
    let budget = max - "…".len();
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > budget {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

fn duration_minutes_label(duration_secs: i64) -> String {
    let minutes = if duration_secs <= 0 {
        0
    } else {
        ((duration_secs + 59) / 60) as i64
    };
    format!("{minutes}m")
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn normalize_priority(priority: &str) -> String {
    match priority.trim().to_lowercase().as_str() {
        "critical" | "1" => "1".to_string(),
        "high" | "2" => "2".to_string(),
        "medium" | "3" => "3".to_string(),
        "low" | "4" => "4".to_string(),
        other if !other.is_empty() => priority.trim().to_string(),
        _ => "unavailable".to_string(),
    }
}

fn normalize_status(status: &str) -> String {
    match status.trim().to_lowercase().as_str() {
        "in_progress" | "active" => "active".to_string(),
        "pending" | "todo" => "pending".to_string(),
        "done" | "completed" => "done".to_string(),
        other if !other.is_empty() => other.to_string(),
        _ => "unavailable".to_string(),
    }
}

fn empty_fallback(value: &str, fallback: &str) -> String {
    let trimmed = clean_inline(value);
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

fn clean_inline(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_string_list(raw: &str) -> Vec<String> {
    let parsed = decode_json_array(raw, JsonKind::FilePath);
    if !parsed.is_empty() {
        return parsed;
    }

    raw.split(',')
        .map(|s| s.trim().trim_matches('"').to_string())
        .filter(|s| !s.is_empty() && s != "[]")
        .collect()
}

enum JsonKind {
    Decision,
    Failure,
    FilePath,
}

fn decode_json_array(raw: &str, kind: JsonKind) -> Vec<String> {
    let value: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };

    let arr = match value {
        Value::Array(arr) => arr,
        _ => return Vec::new(),
    };

    arr.into_iter()
        .filter_map(|item| match item {
            Value::String(s) => Some(clean_inline(&s)),
            Value::Object(map) => match kind {
                JsonKind::Decision => map
                    .get("text")
                    .and_then(|v| v.as_str())
                    .or_else(|| map.get("what").and_then(|v| v.as_str()))
                    .map(clean_inline),
                JsonKind::Failure => {
                    let tool = map.get("tool_name").and_then(|v| v.as_str()).unwrap_or("");
                    let excerpt = map
                        .get("error_excerpt")
                        .and_then(|v| v.as_str())
                        .or_else(|| map.get("text").and_then(|v| v.as_str()))
                        .unwrap_or("");
                    let combined = match (tool.is_empty(), excerpt.is_empty()) {
                        (_, true) => String::new(),
                        (true, false) => excerpt.to_string(),
                        (false, false) => format!("{tool}: {excerpt}"),
                    };
                    if combined.is_empty() {
                        None
                    } else {
                        Some(clean_inline(&combined))
                    }
                }
                JsonKind::FilePath => map
                    .get("path")
                    .and_then(|v| v.as_str())
                    .map(clean_inline),
            },
            _ => None,
        })
        .filter(|s| !s.is_empty())
        .collect()
}
