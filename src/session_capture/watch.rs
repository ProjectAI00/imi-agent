#![allow(dead_code)]
use crate::session_capture::{
    db::{open_sessions_db, sessions_db_path},
    sources::{claude::ClaudeSource, copilot::CopilotSource},
    tail::FileCursor,
    types::{now_ms, EventType, SessionEvent},
};
use notify::{Config, Event, RecommendedWatcher, RecursiveMode, Watcher};
use rusqlite::params;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, Sender},
        Arc,
    },
    thread,
    time::Duration,
};

enum DbMsg {
    Event(SessionEvent, String, u64),
    Shutdown,
}

/// Run the watch daemon. Blocks until Ctrl-C or error.
/// scan_interval_secs: how often to rescan for new files (default 30)
pub fn run_watch(scan_interval_secs: u64) -> Result<(), String> {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx): (Sender<DbMsg>, Receiver<DbMsg>) = mpsc::channel();

    let writer_stop = Arc::clone(&stop);
    let writer_handle = thread::spawn(move || {
        let conn = match open_sessions_db() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[imi-watch] Failed to open sessions.db: {e}");
                return;
            }
        };
        eprintln!(
            "[imi-watch] sessions.db opened at {}",
            sessions_db_path().display()
        );

        loop {
            match rx.recv_timeout(Duration::from_millis(200)) {
                Ok(DbMsg::Shutdown) => break,
                Ok(DbMsg::Event(event, source_path, line_no)) => {
                    if let Err(e) = write_event(&conn, &event, &source_path, line_no) {
                        eprintln!("[imi-watch] DB write error: {e}");
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if writer_stop.load(Ordering::Relaxed) {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        eprintln!("[imi-watch] Writer thread exiting.");
    });

    let mut cursors: HashMap<PathBuf, FileCursor> = HashMap::new();
    let (notify_sender, notify_receiver) = mpsc::channel::<notify::Result<Event>>();

    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = notify_sender.send(res);
        },
        Config::default(),
    )
    .map_err(|e| e.to_string())?;

    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let copilot_base = PathBuf::from(&home).join(".copilot").join("session-state");
    let claude_base = PathBuf::from(&home).join(".claude").join("projects");

    if copilot_base.exists() {
        watcher
            .watch(&copilot_base, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
        eprintln!("[imi-watch] Watching {}", copilot_base.display());
    }
    if claude_base.exists() {
        watcher
            .watch(&claude_base, RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
        eprintln!("[imi-watch] Watching {}", claude_base.display());
    }

    let ctrlc_stop = Arc::clone(&stop);
    let ctrlc_tx = tx.clone();
    ctrlc::set_handler(move || {
        eprintln!("\n[imi-watch] Shutting down...");
        ctrlc_stop.store(true, Ordering::Relaxed);
        let _ = ctrlc_tx.send(DbMsg::Shutdown);
    })
    .map_err(|e| e.to_string())?;

    eprintln!("[imi-watch] Running. Press Ctrl-C to stop.");

    let all_files = discover_all_files();
    for path in &all_files {
        cursors
            .entry(path.clone())
            .or_insert_with(|| FileCursor::new(path.clone()));
    }
    drain_cursors(&mut cursors, &tx);

    let mut last_rescan = std::time::Instant::now();

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        loop {
            match notify_receiver.try_recv() {
                Ok(Ok(event)) => {
                    for path in event.paths {
                        if is_session_file(&path) {
                            let cursor = cursors
                                .entry(path.clone())
                                .or_insert_with(|| FileCursor::new(path.clone()));
                            drain_one_cursor(cursor, &tx);
                        }
                    }
                }
                Ok(Err(e)) => eprintln!("[imi-watch] Notify error: {e}"),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => break,
            }
        }

        if last_rescan.elapsed().as_secs() >= scan_interval_secs {
            let files = discover_all_files();
            for path in &files {
                let cursor = cursors
                    .entry(path.clone())
                    .or_insert_with(|| FileCursor::new(path.clone()));
                drain_one_cursor(cursor, &tx);
            }
            last_rescan = std::time::Instant::now();
        }

        thread::sleep(Duration::from_millis(100));
    }

    let _ = tx.send(DbMsg::Shutdown);
    let _ = writer_handle.join();
    eprintln!("[imi-watch] Done.");
    Ok(())
}

fn discover_all_files() -> Vec<PathBuf> {
    let mut files = CopilotSource::discover_session_files();
    files.extend(ClaudeSource::discover_session_files());
    files
}

fn is_session_file(path: &Path) -> bool {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let parent = path.parent().and_then(|p| p.to_str()).unwrap_or("");
    (name == "events.jsonl" && parent.contains(".copilot"))
        || (name.ends_with(".jsonl") && parent.contains(".claude"))
}

fn drain_cursors(cursors: &mut HashMap<PathBuf, FileCursor>, tx: &Sender<DbMsg>) {
    for cursor in cursors.values_mut() {
        drain_one_cursor(cursor, tx);
    }
}

fn drain_one_cursor(cursor: &mut FileCursor, tx: &Sender<DbMsg>) {
    let path = cursor.path().to_path_buf();
    let is_copilot = path
        .to_str()
        .map(|path| path.contains(".copilot"))
        .unwrap_or(false);

    for (line_no, line) in cursor.read_new_lines() {
        let event = if is_copilot {
            CopilotSource::parse_line(&path, line_no, &line)
        } else {
            ClaudeSource::parse_line(&path, line_no, &line)
        };
        let source_path = path.to_string_lossy().to_string();
        let _ = tx.send(DbMsg::Event(event, source_path, line_no));
    }
}

fn write_event(
    conn: &rusqlite::Connection,
    event: &SessionEvent,
    source_path: &str,
    line_no: u64,
) -> Result<(), String> {
    let meta = event.meta();
    let event_type = event.event_type();

    if event_type == EventType::Unknown && meta.raw_type == "queue-operation" {
        return Ok(());
    }

    let payload_json = match event {
        SessionEvent::ToolCall(e) => {
            serde_json::to_string(&e.arguments).unwrap_or_else(|_| "{}".to_string())
        }
        SessionEvent::ToolResult(e) => format!(
            "{{\"success\":{},\"output\":{}}}",
            e.success,
            serde_json::to_string(&e.output).unwrap_or_else(|_| "\"\"".to_string())
        ),
        SessionEvent::SessionEnd(e) => format!(
            "{{\"files_modified\":{},\"lines_added\":{},\"lines_removed\":{}}}",
            serde_json::to_string(&e.files_modified).unwrap_or_else(|_| "[]".to_string()),
            e.lines_added,
            e.lines_removed
        ),
        SessionEvent::UserMessage(e) => {
            serde_json::to_string(&e.text).unwrap_or_else(|_| "\"\"".to_string())
        }
        SessionEvent::AssistantMessage(e) => {
            serde_json::to_string(&e.text).unwrap_or_else(|_| "\"\"".to_string())
        }
        SessionEvent::Malformed(e) => format!(
            "{{\"error\":{},\"raw\":{}}}",
            serde_json::to_string(&e.error).unwrap_or_else(|_| "\"\"".to_string()),
            serde_json::to_string(&e.raw_line).unwrap_or_else(|_| "\"\"".to_string())
        ),
        _ => "{}".to_string(),
    };

    let tool_name = match event {
        SessionEvent::ToolCall(e) => Some(e.tool_name.clone()),
        SessionEvent::ToolResult(e) if !e.tool_name.is_empty() => Some(e.tool_name.clone()),
        _ => None,
    };

    let call_id = match event {
        SessionEvent::ToolCall(e) => e.call_id.clone(),
        SessionEvent::ToolResult(e) => e.call_id.clone(),
        _ => None,
    };

    conn.execute(
        "INSERT OR IGNORE INTO raw_events
         (source, source_path, source_line, session_id, event_type, timestamp_ms,
          cwd, project, tool_name, call_id, payload_json, ingested_at_ms)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            meta.source.as_str(),
            source_path,
            line_no as i64,
            meta.session_id.as_str(),
            event_type.as_str(),
            meta.timestamp_ms,
            meta.cwd.as_deref(),
            meta.project.as_deref(),
            tool_name,
            call_id,
            payload_json,
            now_ms(),
        ],
    )
    .map_err(|e| e.to_string())?;

    Ok(())
}
