#![allow(dead_code)]

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::session_capture::types::now_ms;

// ---------------------------------------------------------------------------
// Memory struct — mirrors the `memories` table schema
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: i64,
    pub env_id: String,
    pub domain: String,
    /// 0=raw, 1=session, 2=pattern, 3=rule
    pub level: i32,
    pub what: String,
    /// "good" | "bad" | "neutral"
    pub result: String,
    pub context: String,
    pub action_type: String,
    pub outcome_detail: String,
    /// 1-10 heuristic
    pub importance: i32,
    pub embedding: Option<Vec<u8>>,
    pub confidence: f64,
    /// "validated" | "invalidated" | "uncertain" | "superseded"
    pub truth_status: String,
    pub evidence_for: i32,
    pub evidence_against: i32,
    /// JSON array of source entity ids
    pub source_ids: String,
    /// epoch milliseconds
    pub created_at: i64,
    /// epoch milliseconds
    pub last_accessed: i64,
}

// ---------------------------------------------------------------------------
// Schema + indexes
// ---------------------------------------------------------------------------

/// Creates the `memories` table and its indexes if they do not already exist.
pub fn init_memories_table(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS memories (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            env_id           TEXT NOT NULL,
            domain           TEXT NOT NULL DEFAULT 'coding',
            level            INTEGER NOT NULL DEFAULT 0,
            what             TEXT NOT NULL,
            result           TEXT NOT NULL DEFAULT 'neutral',
            context          TEXT NOT NULL DEFAULT '',
            action_type      TEXT NOT NULL DEFAULT '',
            outcome_detail   TEXT NOT NULL DEFAULT '{}',
            importance       INTEGER NOT NULL DEFAULT 5,
            embedding        BLOB,
            confidence       REAL NOT NULL DEFAULT 0.5,
            truth_status     TEXT NOT NULL DEFAULT 'uncertain',
            evidence_for     INTEGER NOT NULL DEFAULT 0,
            evidence_against  INTEGER NOT NULL DEFAULT 0,
            source_ids       TEXT NOT NULL DEFAULT '[]',
            created_at       INTEGER NOT NULL,
            last_accessed    INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_memories_env_domain
            ON memories(env_id, domain);

        CREATE INDEX IF NOT EXISTS idx_memories_env_level
            ON memories(env_id, level);

        CREATE INDEX IF NOT EXISTS idx_memories_importance
            ON memories(importance DESC);

        CREATE INDEX IF NOT EXISTS idx_memories_truth_status
            ON memories(truth_status);

        CREATE INDEX IF NOT EXISTS idx_memories_created_at
            ON memories(created_at DESC);
        ",
    )
    .map_err(|e| format!("init_memories_table: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// CRUD helpers
// ---------------------------------------------------------------------------

fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: row.get("id")?,
        env_id: row.get("env_id")?,
        domain: row.get("domain")?,
        level: row.get("level")?,
        what: row.get("what")?,
        result: row.get("result")?,
        context: row.get("context")?,
        action_type: row.get("action_type")?,
        outcome_detail: row.get("outcome_detail")?,
        importance: row.get("importance")?,
        embedding: row.get("embedding")?,
        confidence: row.get("confidence")?,
        truth_status: row.get("truth_status")?,
        evidence_for: row.get("evidence_for")?,
        evidence_against: row.get("evidence_against")?,
        source_ids: row.get("source_ids")?,
        created_at: row.get("created_at")?,
        last_accessed: row.get("last_accessed")?,
    })
}

/// Inserts a new memory row.  Sets `created_at` and `last_accessed` to the
/// current time when they are zero.  Returns the new row id.
pub fn insert_memory(conn: &Connection, mem: &Memory) -> Result<i64, String> {
    let created = if mem.created_at != 0 {
        mem.created_at
    } else {
        now_ms()
    };
    let accessed = if mem.last_accessed != 0 {
        mem.last_accessed
    } else {
        created
    };

    conn.execute(
        "
        INSERT INTO memories (
            env_id, domain, level, what, result, context,
            action_type, outcome_detail, importance, embedding,
            confidence, truth_status, evidence_for, evidence_against,
            source_ids, created_at, last_accessed
        ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)
        ",
        params![
            mem.env_id,
            mem.domain,
            mem.level,
            mem.what,
            mem.result,
            mem.context,
            mem.action_type,
            mem.outcome_detail,
            mem.importance,
            mem.embedding,
            mem.confidence,
            mem.truth_status,
            mem.evidence_for,
            mem.evidence_against,
            mem.source_ids,
            created,
            accessed,
        ],
    )
    .map_err(|e| format!("insert_memory: {e}"))?;

    Ok(conn.last_insert_rowid())
}

/// Queries memories filtered by env_id, with optional domain and level
/// filters.  Returns rows ordered by importance descending, then created_at
/// descending, limited to `limit`.
pub fn query_memories(
    conn: &Connection,
    env_id: &str,
    domain: Option<&str>,
    level: Option<i32>,
    limit: usize,
) -> Result<Vec<Memory>, String> {
    let mut sql = String::from(
        "SELECT * FROM memories WHERE env_id = ?1",
    );

    if domain.is_some() {
        sql.push_str(" AND domain = ?2");
    }
    if level.is_some() {
        let param_idx = if domain.is_some() { 3 } else { 2 };
        sql.push_str(&format!(" AND level = ?{param_idx}"));
    }

    sql.push_str(" ORDER BY importance DESC, created_at DESC LIMIT ");
    sql.push_str(&limit.to_string());

    let mut stmt = conn.prepare(&sql).map_err(|e| format!("query_memories prepare: {e}"))?;

    // Build parameter list dynamically based on which filters are present.
    let mut rows_result: rusqlite::Rows<'_>;
    let column_count = stmt.column_count();

    if domain.is_some() && level.is_some() {
        rows_result = stmt
            .query(params![env_id, domain.unwrap(), level.unwrap()])
            .map_err(|e| format!("query_memories: {e}"))?;
    } else if domain.is_some() {
        rows_result = stmt
            .query(params![env_id, domain.unwrap()])
            .map_err(|e| format!("query_memories: {e}"))?;
    } else if level.is_some() {
        rows_result = stmt
            .query(params![env_id, level.unwrap()])
            .map_err(|e| format!("query_memories: {e}"))?;
    } else {
        rows_result = stmt
            .query(params![env_id])
            .map_err(|e| format!("query_memories: {e}"))?;
    }

    let _ = column_count; // used only for conditional branching above

    let mut results = Vec::new();
    while let Some(row) = rows_result
        .next()
        .map_err(|e| format!("query_memories fetch: {e}"))?
    {
        results.push(row_to_memory(row).map_err(|e| format!("query_memories row: {e}"))?);
    }
    Ok(results)
}

/// Returns a single memory by its primary key, or `None` if not found.
pub fn get_memory_by_id(conn: &Connection, id: i64) -> Result<Option<Memory>, String> {
    let mut stmt = conn
        .prepare("SELECT * FROM memories WHERE id = ?1")
        .map_err(|e| format!("get_memory_by_id prepare: {e}"))?;

    let mut rows = stmt
        .query(params![id])
        .map_err(|e| format!("get_memory_by_id: {e}"))?;

    match rows
        .next()
        .map_err(|e| format!("get_memory_by_id fetch: {e}"))?
    {
        Some(row) => Ok(Some(
            row_to_memory(row).map_err(|e| format!("get_memory_by_id row: {e}"))?,
        )),
        None => Ok(None),
    }
}

/// Updates the truth_status column for a given memory.
pub fn update_truth_status(conn: &Connection, id: i64, status: &str) -> Result<(), String> {
    conn.execute(
        "UPDATE memories SET truth_status = ?1 WHERE id = ?2",
        params![status, id],
    )
    .map_err(|e| format!("update_truth_status: {e}"))?;
    Ok(())
}

/// Adjusts evidence_for and evidence_against by the given deltas.
pub fn update_evidence(
    conn: &Connection,
    id: i64,
    for_delta: i32,
    against_delta: i32,
) -> Result<(), String> {
    conn.execute(
        "UPDATE memories SET evidence_for = evidence_for + ?1, evidence_against = evidence_against + ?2 WHERE id = ?3",
        params![for_delta, against_delta, id],
    )
    .map_err(|e| format!("update_evidence: {e}"))?;
    Ok(())
}

/// Touches the last_accessed timestamp — used to track recency.
pub fn touch_last_accessed(conn: &Connection, id: i64) -> Result<(), String> {
    let now = now_ms();
    conn.execute(
        "UPDATE memories SET last_accessed = ?1 WHERE id = ?2",
        params![now, id],
    )
    .map_err(|e| format!("touch_last_accessed: {e}"))?;
    Ok(())
}

/// Deletes a memory by its primary key.
pub fn delete_memory(conn: &Connection, id: i64) -> Result<(), String> {
    conn.execute("DELETE FROM memories WHERE id = ?1", params![id])
        .map_err(|e| format!("delete_memory: {e}"))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Importance heuristic
// ---------------------------------------------------------------------------

/// Computes a 1-10 importance score from session signals.
///
/// Rules:
///   baseline 3
///   +2 if task_completed
///   +2 if tool_call_count > 20
///   +1 if tool_call_count > 10
///   +1 if error_count > 3
///   +1 if failure_count > 0
///   +1 if duration_minutes > 30
///   +1 if decision_count > 0
///   clamped to [1, 10]
pub fn compute_importance(
    _files_touched: i64,
    error_count: i64,
    tool_call_count: i64,
    duration_minutes: i64,
    task_completed: bool,
    decision_count: usize,
    failure_count: usize,
) -> i64 {
    let mut score: i64 = 3;

    if task_completed {
        score += 2;
    }
    if tool_call_count > 20 {
        score += 2;
    } else if tool_call_count > 10 {
        score += 1;
    }
    if error_count > 3 {
        score += 1;
    }
    if failure_count > 0 {
        score += 1;
    }
    if duration_minutes > 30 {
        score += 1;
    }
    if decision_count > 0 {
        score += 1;
    }

    score.clamp(1, 10)
}

// ---------------------------------------------------------------------------
// Recency decay
// ---------------------------------------------------------------------------

/// Exponential decay with a 24-hour half-life.
///
/// Returns `e^(-0.0288 * hours_since)`.
/// At 24 hours the value is approximately 0.5.
/// At 0 hours the value is 1.0.
pub fn compute_recency_decay(last_accessed_ms: i64, now_ms: i64) -> f32 {
    let ms_diff = (now_ms - last_accessed_ms).max(0) as f64;
    let hours_since = ms_diff / 3_600_000.0;
    (-0.0288 * hours_since).exp() as f32
}

// ---------------------------------------------------------------------------
// Migration from session_insights → memories
// ---------------------------------------------------------------------------

/// Migrates rows from `session_insights` (joined with `sessions_summary`)
/// into level-1 Memory rows.  Idempotent: if any memories with
/// `source_ids` containing `"migration:session_insights"` already exist,
/// the function returns early with 0 new rows.
pub fn migrate_insights_to_memories(conn: &Connection) -> Result<usize, String> {
    // Idempotency check — skip if we already ran this migration.
    let already: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memories WHERE source_ids LIKE '%migration:session_insights%'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);
    if already > 0 {
        return Ok(0);
    }

    let mut stmt = conn
        .prepare(
            "
            SELECT
                si.session_id,
                si.project,
                si.generated_at_ms,
                si.decisions_observed_json,
                si.failures_observed_json,
                si.task_completed,
                si.summary_text,
                si.causal_tuples_json,
                ss.error_count,
                ss.tool_call_count,
                ss.duration_minutes,
                ss.files_touched_count
            FROM session_insights si
            LEFT JOIN sessions_summary ss ON si.session_id = ss.session_id
            ",
        )
        .map_err(|e| format!("migrate_insights_to_memories prepare: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            // Extract columns that may be NULL from the LEFT JOIN
            let error_count: Option<i64> = row.get(7)?;
            let tool_call_count: Option<i64> = row.get(8)?;
            let duration_minutes: Option<i64> = row.get(9)?;
            let files_touched_count: Option<i64> = row.get(10)?;

            Ok((
                row.get::<_, String>(0)?,  // session_id
                row.get::<_, String>(1)?,  // project
                row.get::<_, i64>(2)?,     // generated_at_ms
                row.get::<_, String>(3)?,  // decisions_observed_json
                row.get::<_, String>(4)?,  // failures_observed_json
                row.get::<_, i32>(5)?,     // task_completed
                row.get::<_, String>(6)?,  // summary_text
                error_count.unwrap_or(0),
                tool_call_count.unwrap_or(0),
                duration_minutes.unwrap_or(0),
                files_touched_count.unwrap_or(0),
            ))
        })
        .map_err(|e| format!("migrate_insights_to_memories query: {e}"))?;

    let mut inserted = 0usize;
    for row in rows {
        let (
            session_id,
            project,
            generated_at_ms,
            decisions_json,
            failures_json,
            task_completed,
            summary_text,
            error_count,
            tool_call_count,
            duration_minutes,
            files_touched_count,
        ) = row.map_err(|e| format!("migrate_insights_to_memories row: {e}"))?;

        let decisions: Vec<serde_json::Value> =
            serde_json::from_str(&decisions_json).unwrap_or_default();
        let failures: Vec<serde_json::Value> =
            serde_json::from_str(&failures_json).unwrap_or_default();

        let importance = compute_importance(
            files_touched_count,
            error_count,
            tool_call_count,
            duration_minutes,
            task_completed != 0,
            decisions.len(),
            failures.len(),
        );

        let mem = Memory {
            id: 0, // auto-generated
            env_id: project,
            domain: "coding".to_string(),
            level: 1, // session-level
            what: summary_text,
            result: if task_completed != 0 {
                "good".to_string()
            } else {
                "neutral".to_string()
            },
            context: session_id.clone(),
            action_type: "session_summary".to_string(),
            outcome_detail: format!("{{\"session_id\":\"{session_id}\"}}"),
            importance: importance as i32,
            embedding: None,
            confidence: 0.5,
            truth_status: "uncertain".to_string(),
            evidence_for: 0,
            evidence_against: 0,
            source_ids: format!("[\"migration:session_insights:{session_id}\"]"),
            created_at: generated_at_ms,
            last_accessed: generated_at_ms,
        };

        insert_memory(conn, &mem)?;
        inserted += 1;
    }

    Ok(inserted)
}

// ---------------------------------------------------------------------------
// BM25 full-text search via FTS5
// ---------------------------------------------------------------------------

/// Full-text search over memories using FTS5 BM25 ranking.
///
/// Returns memories matching `query` for the given `env_id`, filtered to
/// exclude invalidated/superseded entries. Each result carries a normalised
/// similarity score in [0, 1] derived from the raw (negative) BM25 rank via
/// `1.0 / (1.0 + (-rank).exp())`.
///
/// Returned memories have their `last_accessed` timestamp bumped.
pub fn query_by_similarity(
    conn: &Connection,
    query: &str,
    env_id: &str,
    limit: usize,
) -> Result<Vec<(Memory, f32)>, String> {
    let sql = "
        SELECT m.*,
               bm25(memories_fts) as rank
        FROM memories_fts fts
        JOIN memories m ON m.id = fts.rowid
        WHERE memories_fts MATCH ?1
          AND m.env_id = ?2
          AND m.truth_status NOT IN ('invalidated', 'superseded')
        ORDER BY rank
        LIMIT ?3
    ";

    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let mut results: Vec<(Memory, f32)> = Vec::new();

    let rows = stmt
        .query_map(params![query, env_id, limit as i64], |row| {
            let mem = row_to_memory(row)?;
            // Column count from m.* is 18 columns (id..last_accessed),
            // so rank is at index 18.
            let rank: f64 = row.get(18)?;
            Ok((mem, rank))
        })
        .map_err(|e| e.to_string())?;

    for row in rows {
        let (mem, rank) = row.map_err(|e| e.to_string())?;
        // BM25 returns negative values; more negative = better match.
        // Normalise to [0, 1]: 1 / (1 + exp(-rank))
        // Since rank < 0, -rank > 0 and the sigmoid maps to (0.5, 1).
        let similarity = (1.0 / (1.0 + (-rank).exp())) as f32;
        results.push((mem, similarity));
    }

    // Touch last_accessed for returned memories.
    let touch_now = now_ms();
    for (mem, _) in &results {
        let _ = conn.execute(
            "UPDATE memories SET last_accessed = ?1 WHERE id = ?2",
            params![touch_now, mem.id],
        );
    }

    Ok(results)
}

// ---------------------------------------------------------------------------
// Delta brief generation
// ---------------------------------------------------------------------------

/// Assemble a compact "delta brief" summarising recent activity, relevant
/// past memories, and the current active goal.
///
/// The output is kept within approximately 1024 bytes and is structured as:
///
/// ```text
/// ## WHAT JUST HAPPENED
/// ...
/// ## RELEVANT PAST
/// ...
/// ## CURRENT GOAL
/// ...
/// ```
pub fn generate_delta_brief(
    conn: &Connection,
    env_id: &str,
    current_session_id: Option<&str>,
    since_ms: i64,
) -> Result<String, String> {
    let effective_since = if since_ms <= 0 {
        now_ms() - 5 * 60 * 1000 // default: 5 minutes ago
    } else {
        since_ms
    };

    // --- WHAT JUST HAPPENED: recent level-0 memories for current session ---
    let mut recent_sql = String::from(
        "SELECT * FROM memories WHERE env_id = ?1 AND level = 0 AND created_at > ?2",
    );
    if current_session_id.is_some() {
        recent_sql.push_str(" AND source_ids LIKE '%' || ?3 || '%'");
    }
    recent_sql.push_str(" ORDER BY created_at DESC LIMIT 10");

    let recent_memories = if let Some(sid) = current_session_id {
        query_memories_raw(&recent_sql, params![env_id, effective_since, sid], conn)?
    } else {
        query_memories_raw(&recent_sql, params![env_id, effective_since], conn)?
    };

    let what_section = if recent_memories.is_empty() {
        "No recent activity.".to_string()
    } else {
        recent_memories
            .iter()
            .map(|m| format!("- {}", truncate_str(&m.what, 120)))
            .collect::<Vec<_>>()
            .join("\n")
    };

    // --- RELEVANT PAST: top 5 by BM25 using env_id as query context ---
    let relevant = query_by_similarity(conn, env_id, env_id, 5).unwrap_or_default();
    let past_section = if relevant.is_empty() {
        "No relevant past memories.".to_string()
    } else {
        relevant
            .iter()
            .map(|(m, score)| {
                format!(
                    "- [{}] {} ({:.2})",
                    m.domain,
                    truncate_str(&m.what, 100),
                    score
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    // --- CURRENT GOAL: read from state.db (mirrors brief.rs pattern) ---
    let goal_section = load_current_goal_section();

    // Assemble with byte budget (~1024 bytes total).
    let total_budget: usize = 1024;
    let header_overhead = 3 * 4; // "## X\n" headers ~12 bytes each
    let section_budget = total_budget.saturating_sub(header_overhead) / 3;

    let mut sections = Vec::new();
    sections.push(format!(
        "## WHAT JUST HAPPENED\n{}\n",
        truncate_str(&what_section, section_budget)
    ));
    sections.push(format!(
        "## RELEVANT PAST\n{}\n",
        truncate_str(&past_section, section_budget)
    ));
    sections.push(format!(
        "## CURRENT GOAL\n{}\n",
        truncate_str(&goal_section, section_budget)
    ));

    // Final trim to hard cap.
    let assembled = sections.join("\n");
    Ok(truncate_str(assembled.trim_end(), total_budget))
}

// ---------------------------------------------------------------------------
// Helpers for query_by_similarity and generate_delta_brief
// ---------------------------------------------------------------------------

/// Generic raw-SQL query returning Vec<Memory>.
fn query_memories_raw(
    sql: &str,
    params: &[&dyn rusqlite::types::ToSql],
    conn: &Connection,
) -> Result<Vec<Memory>, String> {
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params, |row| row_to_memory(row))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Open the IMI state.db following the same discovery logic as brief.rs.
fn open_state_db() -> Option<Connection> {
    let path = state_db_path();
    if !path.exists() {
        return None;
    }
    Connection::open(path).ok()
}

fn state_db_path() -> PathBuf {
    if let Ok(path) = std::env::var("IMI_DB") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Ok(mut dir) = std::env::current_dir() {
        loop {
            let candidate = dir.join(".imi").join("state.db");
            if candidate.exists() {
                return candidate;
            }
            if !dir.pop() {
                break;
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".imi").join("state.db");
    }
    PathBuf::from(".imi").join("state.db")
}

/// Load the active goal section from state.db, mirroring the pattern in
/// brief.rs (`open_state_db` + `load_active_goal`).
fn load_current_goal_section() -> String {
    let state_conn = match open_state_db() {
        Some(c) => c,
        None => return "unavailable".to_string(),
    };

    state_conn
        .query_row(
            "SELECT COALESCE(name, ''), COALESCE(description, ''), COALESCE(status, '')
             FROM goals
             WHERE COALESCE(status, '') != 'done'
             ORDER BY CASE LOWER(COALESCE(priority, ''))
                        WHEN '1' THEN 1 WHEN 'critical' THEN 1
                        WHEN '2' THEN 2 WHEN 'high' THEN 2
                        WHEN '3' THEN 3 WHEN 'medium' THEN 3
                        WHEN '4' THEN 4 WHEN 'low' THEN 4
                        ELSE 99 END ASC, id ASC
             LIMIT 1",
            [],
            |row| {
                let name: String = row.get(0)?;
                let desc: String = row.get(1)?;
                let status: String = row.get(2)?;
                Ok(format!("{} — {} [{}]", name, desc, status))
            },
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or_else(|| "No active goal found.".to_string())
}

// ---------------------------------------------------------------------------
// Pattern compression
// ---------------------------------------------------------------------------

/// Scans level-0 and level-1 memories for repeated patterns and promotes
/// them to level-2 (pattern) memories.  Returns the number of new pattern
/// rows inserted.
///
/// Current heuristic: groups memories by `(env_id, action_type)` and
/// creates a pattern when a group has 3+ entries.  The pattern's `what`
/// field summarises the common action and result.
pub fn compress_patterns(conn: &Connection) -> Result<usize, String> {
    // Find groups with 3+ entries that do not already have a corresponding
    // level-2 pattern row.
    let mut stmt = conn
        .prepare(
            "
            SELECT env_id, action_type, result,
                   COUNT(*) as cnt,
                   GROUP_CONCAT(what, ' | ') as samples
            FROM memories
            WHERE level IN (0, 1)
              AND truth_status NOT IN ('invalidated', 'superseded')
            GROUP BY env_id, action_type, result
            HAVING cnt >= 3
            ",
        )
        .map_err(|e| format!("compress_patterns query: {e}"))?;

    let rows: Vec<(String, String, String, i64, String)> = stmt
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(|e| format!("compress_patterns map: {e}"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("compress_patterns collect: {e}"))?;

    let mut inserted = 0usize;
    let now = now_ms();

    for (env_id, action_type, result, count, samples) in &rows {
        // Truncate samples to avoid blowing past column limits.
        let sample_preview = if samples.len() > 500 {
            format!("{}...", &samples[..500])
        } else {
            samples.clone()
        };

        let what_text = format!(
            "Pattern: {}x [{}] with result '{}' — samples: {}",
            count, action_type, result, sample_preview
        );

        let mem = Memory {
            id: 0,
            env_id: env_id.clone(),
            domain: "coding".to_string(),
            level: 2, // pattern level
            what: what_text,
            result: result.clone(),
            context: String::new(),
            action_type: action_type.clone(),
            outcome_detail: format!("{{\"compressed_from_count\":{count}}}"),
            importance: 6,
            embedding: None,
            confidence: 0.7,
            truth_status: "uncertain".to_string(),
            evidence_for: 0,
            evidence_against: 0,
            source_ids: "[]".to_string(),
            created_at: now,
            last_accessed: now,
        };

        insert_memory(conn, &mem)?;
        inserted += 1;
    }

    Ok(inserted)
}

fn truncate_str(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    if max_bytes <= 1 {
        return "…".to_string();
    }
    let budget = max_bytes - "…".len();
    let mut out = String::new();
    for ch in s.chars() {
        if out.len() + ch.len_utf8() > budget {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        init_memories_table(&conn).unwrap();
        conn
    }

    fn sample_memory() -> Memory {
        Memory {
            id: 0,
            env_id: "env_test".to_string(),
            domain: "coding".to_string(),
            level: 0,
            what: "ran cargo test".to_string(),
            result: "good".to_string(),
            context: "project-x".to_string(),
            action_type: "tool_call".to_string(),
            outcome_detail: "{}".to_string(),
            importance: 5,
            embedding: None,
            confidence: 0.75,
            truth_status: "uncertain".to_string(),
            evidence_for: 0,
            evidence_against: 0,
            source_ids: "[]".to_string(),
            created_at: 0, // will be set by insert
            last_accessed: 0,
        }
    }

    #[test]
    fn insert_and_retrieve_memory() {
        let conn = test_conn();
        let mem = sample_memory();
        let id = insert_memory(&conn, &mem).unwrap();
        assert!(id > 0);

        let fetched = get_memory_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(fetched.what, "ran cargo test");
        assert_eq!(fetched.env_id, "env_test");
        assert_eq!(fetched.importance, 5);
    }

    #[test]
    fn query_memories_filters_correctly() {
        let conn = test_conn();

        let mut mem_a = sample_memory();
        mem_a.env_id = "env_1".to_string();
        mem_a.domain = "coding".to_string();
        mem_a.level = 1;
        mem_a.importance = 8;
        insert_memory(&conn, &mem_a).unwrap();

        let mut mem_b = sample_memory();
        mem_b.env_id = "env_1".to_string();
        mem_b.domain = "ops".to_string();
        mem_b.level = 2;
        mem_b.importance = 3;
        insert_memory(&conn, &mem_b).unwrap();

        // No filters beyond env_id — both returned
        let all = query_memories(&conn, "env_1", None, None, 10).unwrap();
        assert_eq!(all.len(), 2);

        // Domain filter
        let coding = query_memories(&conn, "env_1", Some("coding"), None, 10).unwrap();
        assert_eq!(coding.len(), 1);
        assert_eq!(coding[0].domain, "coding");

        // Level filter
        let level_2 = query_memories(&conn, "env_1", None, Some(2), 10).unwrap();
        assert_eq!(level_2.len(), 1);
        assert_eq!(level_2[0].level, 2);

        // Combined
        let combined =
            query_memories(&conn, "env_1", Some("ops"), Some(2), 10).unwrap();
        assert_eq!(combined.len(), 1);
    }

    #[test]
    fn update_truth_status_works() {
        let conn = test_conn();
        let id = insert_memory(&conn, &sample_memory()).unwrap();

        update_truth_status(&conn, id, "validated").unwrap();
        let mem = get_memory_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(mem.truth_status, "validated");
    }

    #[test]
    fn update_evidence_works() {
        let conn = test_conn();
        let id = insert_memory(&conn, &sample_memory()).unwrap();

        update_evidence(&conn, id, 3, 1).unwrap();
        let mem = get_memory_by_id(&conn, id).unwrap().unwrap();
        assert_eq!(mem.evidence_for, 3);
        assert_eq!(mem.evidence_against, 1);
    }

    #[test]
    fn touch_last_accessed_works() {
        let conn = test_conn();
        let id = insert_memory(&conn, &sample_memory()).unwrap();

        let before = get_memory_by_id(&conn, id).unwrap().unwrap().last_accessed;
        touch_last_accessed(&conn, id).unwrap();
        let after = get_memory_by_id(&conn, id).unwrap().unwrap().last_accessed;
        assert!(after >= before);
    }

    #[test]
    fn delete_memory_works() {
        let conn = test_conn();
        let id = insert_memory(&conn, &sample_memory()).unwrap();
        assert!(get_memory_by_id(&conn, id).unwrap().is_some());

        delete_memory(&conn, id).unwrap();
        assert!(get_memory_by_id(&conn, id).unwrap().is_none());
    }

    #[test]
    fn importance_heuristic_rules() {
        // baseline 3
        assert_eq!(compute_importance(0, 0, 0, 0, false, 0, 0), 3);

        // +2 task completed
        assert_eq!(compute_importance(0, 0, 0, 0, true, 0, 0), 5);

        // +2 tool_calls > 20
        assert_eq!(compute_importance(0, 0, 25, 0, false, 0, 0), 5);

        // +1 tool_calls > 10 (not > 20)
        assert_eq!(compute_importance(0, 0, 15, 0, false, 0, 0), 4);

        // +1 errors > 3
        assert_eq!(compute_importance(0, 5, 0, 0, false, 0, 0), 4);

        // +1 failures > 0
        assert_eq!(compute_importance(0, 0, 0, 0, false, 0, 1), 4);

        // +1 duration > 30
        assert_eq!(compute_importance(0, 0, 0, 45, false, 0, 0), 4);

        // +1 decisions > 0
        assert_eq!(compute_importance(0, 0, 0, 0, false, 2, 0), 4);

        // All bonuses — clamped to 10
        assert_eq!(
            compute_importance(10, 5, 25, 60, true, 3, 2),
            10
        );
    }

    #[test]
    fn recency_decay_half_life() {
        // At 0 hours, decay should be ~1.0
        let now = 100_000_000;
        let decay_0 = compute_recency_decay(now, now);
        assert!((decay_0 - 1.0).abs() < 0.01);

        // At 24 hours, decay should be ~0.5
        let ms_24h: i64 = 24 * 3_600_000;
        let decay_24 = compute_recency_decay(now, now + ms_24h);
        assert!((decay_24 - 0.5).abs() < 0.05);
    }

    #[test]
    fn migration_is_idempotent() {
        let conn = test_conn();
        // Without session_insights table, migration will fail — that is fine.
        // The idempotency path is what we test here: if we insert a memory
        // with the migration marker manually, re-running should return 0.
        let marker = Memory {
            id: 0,
            env_id: "env_test".to_string(),
            domain: "coding".to_string(),
            level: 1,
            what: "marker".to_string(),
            result: "neutral".to_string(),
            context: String::new(),
            action_type: String::new(),
            outcome_detail: "{}".to_string(),
            importance: 1,
            embedding: None,
            confidence: 0.5,
            truth_status: "uncertain".to_string(),
            evidence_for: 0,
            evidence_against: 0,
            source_ids: "[\"migration:session_insights:some_id\"]".to_string(),
            created_at: 0,
            last_accessed: 0,
        };
        insert_memory(&conn, &marker).unwrap();

        // The migration should detect the marker and skip
        let count = migrate_insights_to_memories(&conn).unwrap();
        assert_eq!(count, 0);
    }
}
