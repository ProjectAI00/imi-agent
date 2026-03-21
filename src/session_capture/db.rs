#![allow(dead_code)]
use rusqlite::{Connection, Result as SqlResult};
use std::path::{Path, PathBuf};

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
    conn.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
    conn.pragma_update(None, "synchronous", "NORMAL").map_err(|e| e.to_string())?;
    conn.pragma_update(None, "foreign_keys", "ON").map_err(|e| e.to_string())?;
    conn.pragma_update(None, "busy_timeout", "5000").map_err(|e| e.to_string())?;
    run_sessions_schema(&conn)?;
    Ok(conn)
}

pub fn run_sessions_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch("
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
            summary_text              TEXT NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_session_insights_project_generated
            ON session_insights(project, generated_at_ms DESC);
    ").map_err(|e| e.to_string())
}
