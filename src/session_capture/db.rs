#![allow(dead_code)]
use rusqlite::Connection;
use std::path::PathBuf;

pub fn sessions_db_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".imi").join("sessions.db")
}

pub fn open_sessions_db() -> Result<Connection, String> {
    let path = sessions_db_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(&path).map_err(|e| e.to_string())?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| e.to_string())?;
    conn.pragma_update(None, "busy_timeout", "5000")
        .map_err(|e| e.to_string())?;
    run_sessions_schema(&conn)?;
    Ok(conn)
}

pub fn run_sessions_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS raw_events (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            source          TEXT NOT NULL,
            source_path     TEXT NOT NULL,
            source_line     INTEGER NOT NULL,
            session_id      TEXT NOT NULL,
            event_type      TEXT NOT NULL,
            timestamp_ms    INTEGER NOT NULL,
            cwd             TEXT,
            project         TEXT,
            tool_name       TEXT,
            call_id         TEXT,
            payload_json    TEXT NOT NULL,
            ingested_at_ms  INTEGER NOT NULL,
            UNIQUE(source, source_path, source_line)
        );

        CREATE INDEX IF NOT EXISTS idx_raw_events_session_time
            ON raw_events(session_id, timestamp_ms);
        CREATE INDEX IF NOT EXISTS idx_raw_events_project_time
            ON raw_events(project, timestamp_ms);
        CREATE INDEX IF NOT EXISTS idx_raw_events_type_time
            ON raw_events(event_type, timestamp_ms);

        CREATE TABLE IF NOT EXISTS sessions_summary (
            session_id           TEXT PRIMARY KEY,
            source               TEXT NOT NULL,
            project              TEXT NOT NULL,
            cwd                  TEXT,
            start_time_ms        INTEGER NOT NULL,
            end_time_ms          INTEGER NOT NULL,
            last_event_time_ms   INTEGER NOT NULL,
            duration_minutes     INTEGER NOT NULL,
            files_touched_json   TEXT NOT NULL DEFAULT '[]',
            files_touched_count  INTEGER NOT NULL DEFAULT 0,
            error_count          INTEGER NOT NULL DEFAULT 0,
            test_outcomes_json   TEXT NOT NULL DEFAULT '[]',
            tool_call_count      INTEGER NOT NULL DEFAULT 0,
            ended_by             TEXT NOT NULL DEFAULT 'inactivity',
            updated_at_ms        INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_sessions_summary_project_end
            ON sessions_summary(project, end_time_ms DESC);
        CREATE INDEX IF NOT EXISTS idx_sessions_summary_end
            ON sessions_summary(end_time_ms DESC);

        CREATE TABLE IF NOT EXISTS session_insights (
            session_id                TEXT PRIMARY KEY
                                      REFERENCES sessions_summary(session_id)
                                      ON DELETE CASCADE,
            project                   TEXT NOT NULL,
            generated_at_ms           INTEGER NOT NULL,
            decisions_observed_json   TEXT NOT NULL DEFAULT '[]',
            failures_observed_json    TEXT NOT NULL DEFAULT '[]',
            task_completed            INTEGER NOT NULL DEFAULT 0
                                      CHECK (task_completed IN (0,1)),
            files_at_risk_json        TEXT NOT NULL DEFAULT '[]',
            summary_text              TEXT NOT NULL,
            causal_tuples_json        TEXT NOT NULL DEFAULT '[]',
            truth_status              TEXT NOT NULL DEFAULT 'uncertain',
            contradiction_detected    INTEGER NOT NULL DEFAULT 0
                                      CHECK (contradiction_detected IN (0,1)),
            contradiction_policy      TEXT NOT NULL DEFAULT 'context_split',
            contradiction_note        TEXT NOT NULL DEFAULT '',
            retrieval_relevance       REAL NOT NULL DEFAULT 0.0,
            retrieval_recency         REAL NOT NULL DEFAULT 0.0,
            retrieval_evidence_strength REAL NOT NULL DEFAULT 0.0,
            retrieval_truth_component REAL NOT NULL DEFAULT 0.0,
            retrieval_total_score     REAL NOT NULL DEFAULT 0.0,
            intervention_signal       TEXT NOT NULL DEFAULT 'ask_for_evidence'
        );

        CREATE INDEX IF NOT EXISTS idx_session_insights_project_generated
            ON session_insights(project, generated_at_ms DESC);
    ",
    )
    .map_err(|e| e.to_string())?;

    ensure_column(
        conn,
        "session_insights",
        "causal_tuples_json",
        "TEXT NOT NULL DEFAULT '[]'",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "truth_status",
        "TEXT NOT NULL DEFAULT 'uncertain'",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "contradiction_detected",
        "INTEGER NOT NULL DEFAULT 0 CHECK (contradiction_detected IN (0,1))",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "contradiction_policy",
        "TEXT NOT NULL DEFAULT 'context_split'",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "contradiction_note",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "retrieval_relevance",
        "REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "retrieval_recency",
        "REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "retrieval_evidence_strength",
        "REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "retrieval_truth_component",
        "REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "retrieval_total_score",
        "REAL NOT NULL DEFAULT 0.0",
    )?;
    ensure_column(
        conn,
        "session_insights",
        "intervention_signal",
        "TEXT NOT NULL DEFAULT 'ask_for_evidence'",
    )?;

    // Create index on truth_status AFTER ensuring the column exists
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_session_insights_truth_status
            ON session_insights(truth_status, generated_at_ms DESC)",
        [],
    )
    .map_err(|e| format!("truth_status index: {e}"))?;

    ensure_memories_and_fts(conn)?;

    Ok(())
}

fn ensure_memories_and_fts(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS memories (
            id              INTEGER PRIMARY KEY AUTOINCREMENT,
            env_id          TEXT NOT NULL,
            domain          TEXT NOT NULL DEFAULT 'coding',
            level           INTEGER NOT NULL DEFAULT 0,
            what            TEXT NOT NULL,
            result          TEXT NOT NULL DEFAULT 'neutral',
            context         TEXT NOT NULL DEFAULT '',
            action_type     TEXT NOT NULL DEFAULT '',
            outcome_detail  TEXT NOT NULL DEFAULT '{}',
            importance      INTEGER NOT NULL DEFAULT 5,
            embedding       BLOB,
            confidence      REAL NOT NULL DEFAULT 0.5,
            truth_status    TEXT NOT NULL DEFAULT 'uncertain',
            evidence_for    INTEGER NOT NULL DEFAULT 0,
            evidence_against INTEGER NOT NULL DEFAULT 0,
            source_ids      TEXT NOT NULL DEFAULT '[]',
            created_at      INTEGER NOT NULL,
            last_accessed   INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_memories_env_domain
            ON memories(env_id, domain);
        CREATE INDEX IF NOT EXISTS idx_memories_level
            ON memories(level);
        CREATE INDEX IF NOT EXISTS idx_memories_importance
            ON memories(importance DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_created_at
            ON memories(created_at DESC);
        CREATE INDEX IF NOT EXISTS idx_memories_truth_status
            ON memories(truth_status);
        ",
    )
    .map_err(|e| format!("memories table/indexes: {e}"))?;

    // FTS5 virtual table — create separately so a missing FTS5 compile flag
    // produces a clear error rather than failing the whole batch.
    conn.execute_batch(
        "
        CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts
            USING fts5(what, context, action_type, outcome_detail,
                       content='memories', content_rowid='id');
        ",
    )
    .map_err(|e| format!("memories_fts vtable: {e}"))?;

    // Triggers to keep FTS in sync on INSERT and DELETE.
    conn.execute_batch(
        "
        CREATE TRIGGER IF NOT EXISTS memories_ai AFTER INSERT ON memories BEGIN
            INSERT INTO memories_fts(rowid, what, context, action_type, outcome_detail)
                VALUES (new.id, new.what, new.context, new.action_type, new.outcome_detail);
        END;

        CREATE TRIGGER IF NOT EXISTS memories_ad AFTER DELETE ON memories BEGIN
            INSERT INTO memories_fts(memories_fts, rowid, what, context, action_type, outcome_detail)
                VALUES ('delete', old.id, old.what, old.context, old.action_type, old.outcome_detail);
        END;
        ",
    )
    .map_err(|e| format!("memories FTS triggers: {e}"))?;

    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition_sql: &str,
) -> Result<(), String> {
    if column_exists(conn, table, column)? {
        return Ok(());
    }
    conn.execute(
        &format!("ALTER TABLE {table} ADD COLUMN {column} {definition_sql}"),
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    for row in rows {
        if row.map_err(|e| e.to_string())? == column {
            return Ok(true);
        }
    }
    Ok(false)
}
