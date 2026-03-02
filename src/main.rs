use clap::{CommandFactory, Parser, Subcommand};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use std::env;
use std::fs::{self, File};
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
// PostHog EU public capture key — safe to commit (client-side key)
const POSTHOG_KEY: &str = "phc_exyd1ppU0ZS7McQ1ay1gxvCaUI2QfYPuMCTk4kawVKF";

fn get_or_create_install_id(conn: &Connection) -> String {
    // Try to read existing install_id
    if let Ok(id) = conn.query_row(
        "SELECT value FROM settings WHERE key='install_id' LIMIT 1",
        [],
        |r| r.get::<_, String>(0),
    ) {
        return id;
    }
    // Generate a new one and persist it
    let id = format!("iid_{}", gen_id());
    let _ = conn.execute(
        "INSERT OR IGNORE INTO settings (key, value) VALUES ('install_id', ?1)",
        params![id],
    );
    id
}

fn track(event: &str, install_id: &str, duration_ms: u128) {
    if env::var("IMI_NO_ANALYTICS").is_ok() {
        return;
    }
    if POSTHOG_KEY == "phc_REPLACE_ME" {
        return;
    }
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let body = format!(
        r#"{{"api_key":"{key}","event":"imi_{event}","distinct_id":"{id}","properties":{{"version":"{ver}","platform":"{plat}","duration_ms":{dur},"$lib":"imi-cli"}}}}"#,
        key = POSTHOG_KEY,
        event = event,
        id = install_id,
        ver = VERSION,
        plat = platform,
        dur = duration_ms,
    );
    let _ = Command::new("curl")
        .args([
            "-s", "-o", "/dev/null",
            "-X", "POST",
            "-H", "Content-Type: application/json",
            "-d", &body,
            "https://us.i.posthog.com/capture/",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn(); // fire-and-forget, never blocks
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputMode {
    Human,
    Toon,
    Json,
}

#[derive(Debug, Clone, Copy)]
struct OutputCtx {
    mode: OutputMode,
    color: bool,
}

impl OutputCtx {
    fn new(mode: OutputMode) -> Self {
        let color = matches!(mode, OutputMode::Human)
            && io::stdout().is_terminal()
            && env::var("TERM").unwrap_or_default() != "dumb";
        Self { mode, color }
    }

    fn is_toon(self) -> bool {
        matches!(self.mode, OutputMode::Toon)
    }

    fn is_json(self) -> bool {
        matches!(self.mode, OutputMode::Json)
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "imi",
    version = VERSION,
    about = "Persistent memory and planning layer for AI coding agents",
    long_about = "IMI — tracks goals, tasks, decisions, and direction across every session.\n\nStart here:\n  imi context                    → what we're building, decisions, active tasks\n  imi think                      → is what we're building still the right thing?\n  imi plan                       → full goal and task list\n\nCapture human thinking:\n  imi decide \"what\" \"why\"        → firm calls, what was ruled out, why\n  imi log \"note\"                 → direction, instincts, things to revisit",
    propagate_version = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    #[command(hide = true, about = "Initialize IMI in the current directory")]
    Init,
    #[command(alias = "s", hide = true, about = "Show all goals, tasks, and progress")]
    Status,
    #[command(alias = "p", about = "Show all goals and tasks with status. Use when: you need a full list of what exists before creating new goals/tasks (to avoid duplicates).")]
    Plan,
    #[command(about = "Archive a goal")]
    Archive {
        goal_id: String,
    },
    #[command(alias = "ctx", alias = "c", about = "Run this first, every session. Shows what's being built, active tasks, recent decisions, and direction. Use when: starting a session, picking up where we left off, or answering 'what should we work on today'.")]
    Context {
        goal_id: Option<String>,
    },
    #[command(
        alias = "n",
        hide = true,
        about = "Claim the highest-priority available task and get full context"
    )]
    Next {
        #[arg(long)]
        agent: Option<String>,
        goal_id: Option<String>,
    },
    #[command(alias = "st", hide = true, about = "Lock a specific task for this agent")]
    Start {
        #[arg(long)]
        agent: Option<String>,
        task_id: String,
    },
    #[command(
        alias = "done",
        about = "Use when: a task or piece of work is finished. Always call this after completing work — it marks the task done AND stores your summary as a persistent memory so the next session knows what was built and why. Never skip this. The summary is how context compounds across sessions."
    )]
    Complete {
        #[arg(long)]
        agent: Option<String>,
        task_id: String,
        summary: Vec<String>,
        /// How the agent interpreted the goal and what it thought success looked like
        #[arg(long)]
        interpretation: Option<String>,
        /// What the agent was uncertain about — where understanding may have drifted from intent
        #[arg(long)]
        uncertainty: Option<String>,
        /// Did it actually work? e.g. "deployed successfully, no issues" or "failed — auth bug introduced". Captures real-world outcome, not just what was built.
        #[arg(long)]
        outcome: Option<String>,
    },
    #[command(about = "Run hankweave for a task and auto-complete on success")]
    Run {
        task_id: String,
        model: Option<String>,
    },
    #[command(hide = true, about = "Run any command under IMI lifecycle automation")]
    Wrap {
        #[arg(long)]
        agent: Option<String>,
        task_id: String,
        #[arg(long, default_value_t = 300)]
        ping_secs: u64,
        #[arg(long, default_value_t = 900)]
        checkpoint_secs: u64,
        #[arg(last = true, num_args = 1.., allow_hyphen_values = true)]
        command: Vec<String>,
    },
    #[command(
        alias = "parallel",
        hide = true,
        about = "Orchestrate parallel task execution for a goal"
    )]
    Orchestrate {
        goal_id: Option<String>,
        #[arg(long, default_value_t = 4)]
        workers: usize,
        #[arg(long)]
        agent_prefix: Option<String>,
        #[arg(long, default_value_t = 300)]
        ping_secs: u64,
        #[arg(long, default_value_t = 900)]
        checkpoint_secs: u64,
        #[arg(long)]
        max_tasks: Option<usize>,
        /// Which CLI to use for workers: auto, claude, opencode, codex, or hankweave (default).
        /// 'auto' detects the current environment from env vars.
        #[arg(long)]
        cli: Option<String>,
        #[arg(last = true, num_args = 0.., allow_hyphen_values = true)]
        command: Vec<String>,
    },
    #[command(hide = true, about = "Release a task lock and record why it's blocked")]
    Fail {
        #[arg(long)]
        agent: Option<String>,
        task_id: String,
        reason: Vec<String>,
    },
    #[command(hide = true, about = "Heartbeat to keep a task locked (~every 10 min)")]
    Ping {
        task_id: String,
    },
    #[command(hide = true, about = "Save mid-task progress and refresh heartbeat")]
    Checkpoint {
        task_id: String,
        note: Vec<String>,
    },
    #[command(alias = "add-goal", alias = "ag", about = "Use when: a new initiative, area of work, or product direction is being committed to. Goals must trace back to a decision or direction note — if you can't point to one, use imi log first to capture the thinking, then create the goal once it's clear. A goal is a bet: we believe this is worth building. Name it like an outcome, fill in why it matters now, and set success_signal to something observable. Run imi plan first to check it doesn't already exist.")]
    Goal {
        name: String,
        desc: Option<String>,
        priority: Option<String>,
        why: Option<String>,
        #[arg(long = "why")]
        why_long: Option<String>,
        for_who: Option<String>,
        success_signal: Option<String>,
        #[arg(long, value_delimiter = ',')]
        relevant_files: Vec<String>,
        #[arg(long)]
        context: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
    },
    #[command(alias = "add-task", alias = "at", about = "Use when: something specific needs to be built or done and should be tracked ('add this to the backlog', 'we need to do X'). Always attach to a goal_id from imi context.")]
    Task {
        goal_id: String,
        title: String,
        desc: Option<String>,
        priority: Option<String>,
        why: Option<String>,
        #[arg(long = "why")]
        why_long: Option<String>,
        #[arg(long)]
        context: Option<String>,
        #[arg(long, value_delimiter = ',')]
        relevant_files: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        tools: Vec<String>,
        #[arg(long)]
        acceptance_criteria: Option<String>,
        #[arg(long)]
        workspace: Option<String>,
    },
    #[command(
        alias = "mem",
        hide = true,
        about = "View or add persistent memories"
    )]
    Memory {
        #[arg(long, help = "List human-verified lessons")]
        lessons: bool,
        #[command(subcommand)]
        action: Option<MemoryAction>,
    },
    #[command(about = "Use when: the agent made the same mistake more than once, or a human had to correct something that should have been obvious. Stores a verified lesson so every future agent session sees it before starting work. Example: agent keeps forgetting to check token expiry — store it here so it never happens again.")]
    Lesson {
        args: Vec<String>,
        #[arg(long)]
        correct_behavior: Option<String>,
        #[arg(long)]
        verified_by: Option<String>,
    },
    #[command(alias = "d", about = "Use when: a firm call was made that should be permanent and traceable. Captures the human reasoning behind a direction — not just what was decided but what was ruled out, what assumption it rests on, and what would change it. This is the highest-authority layer in IMI. Goals and tasks must trace back here. Write like a PM who needs this to still make sense in 3 months: be specific, name what was rejected, state the real reason. Bad: imi decide 'use postgres' 'better'. Good: imi decide 'use postgres over mysql' 'team knows it, simpler ops, mysql adds no value here — revisit if we need sharding'.")]
    Decide {
        /// What was decided AND what was ruled out. Name both sides. Example: 'use postgres over mysql' not just 'use postgres'
        what: String,
        /// The real reason — the assumption, the trade-off, what would change this decision. Not a one-word justification.
        why: String,
        /// What else in the codebase or product changes because of this. Example: 'auth design, session handling, all DB queries'
        affects: Option<String>,
    },
    #[command(alias = "l", about = "Use when: something important came up that isn't a firm decision yet — a direction, an instinct, a concern, something to revisit. Human thinking that should be preserved but isn't ready to be a decision. Captures the reasoning as it evolves. Write it the way you'd explain it to a colleague: what you noticed, why it matters, what you're uncertain about. If it becomes a firm call later, promote it to imi decide. Examples: 'the onboarding flow feels too long — users might drop off before seeing value', 'not sure if we should build this ourselves or use an existing library, leaning toward building'.")]
    Log {
        note: Vec<String>,
    },
    #[command(alias = "rm", hide = true, about = "Delete a goal or task by ID")]
    Delete {
        id: String,
    },
    #[command(hide = true, about = "Wipe all state (destructive — use with caution)")]
    Reset {
        #[arg(short, long)]
        force: bool,
    },
    #[command(alias = "stat", hide = true, about = "Show usage statistics")]
    Stats,
    #[command(hide = true, about = "Print agent instructions for a given target")]
    Instructions {
        target: Option<String>,
    },
    #[command(hide = true, about = "Verify whether a task's acceptance criteria is actually met")]
    Verify {
        task_id: String,
    },
    #[command(hide = true, about = "Audit done tasks — flags those with no acceptance criteria or no completion summary")]
    Audit,
    #[command(about = "Use when: you want the agent to reason over everything in the DB and ask whether we're still building the right things. Dumps full project state — goals, tasks, decisions, direction notes, memories — with a PM-style reasoning prompt. The agent reads this and surfaces what no longer aligns, what to challenge or kill, what's missing, and what the real next move is. Use when stuck, when things feel off, or when the human asks 'are we still on track?'")]
    Think,
    #[command(about = "Check verification state (all tasks or one task)")]
    Check {
        task_id: Option<String>,
    },
    #[command(about = "Update imi to the latest version")]
    Update,
    #[command(hide = true, about = "Show context or log a direction note")]
    Ops {
        args: Vec<String>,
    },
}

#[derive(Subcommand, Debug)]
enum MemoryAction {
    List,
    Add {
        goal_id: String,
        key: String,
        value: String,
    },
}

#[derive(Debug, Clone)]
struct GoalRow {
    id: String,
    name: String,
    description: String,
    why_: String,
    for_who: String,
    success_signal: String,
    status: String,
    priority: String,
    created_at: i64,
}

#[derive(Debug, Clone)]
struct TaskRow {
    id: String,
    title: String,
    description: String,
    why_: String,
    goal_id: Option<String>,
    status: String,
    priority: String,
    agent_id: Option<String>,
    created_at: i64,
}

#[derive(Debug, Clone)]
struct MemoryRow {
    id: String,
    goal_id: Option<String>,
    task_id: Option<String>,
    key: String,
    value: String,
    typ: String,
    source: String,
    created_at: i64,
}

#[derive(Debug, Clone)]
struct LessonRow {
    id: String,
    what_went_wrong: String,
    correct_behavior: String,
    verified_by: String,
    created_at: i64,
}

#[derive(Debug, Clone)]
struct TaskClaim {
    id: String,
    title: String,
    description: String,
    why_: String,
    context: String,
    goal_id: Option<String>,
    relevant_files: String,
    tools: String,
    acceptance_criteria: String,
    workspace_path: String,
}

enum ClaimResult {
    NoTasks,
    RaceLost,
    Claimed(TaskClaim),
}

fn main() {
    let original_args: Vec<String> = env::args().collect();
    let (mode, parsed_args) = extract_output_mode(original_args);
    let out = OutputCtx::new(mode);

    let cli = match Cli::try_parse_from(parsed_args.clone()) {
        Ok(c) => c,
        Err(e) => {
            let _ = e.print();
            return;
        }
    };

    let Some(command) = cli.command else {
        let mut cmd = Cli::command();
        let _ = cmd.print_help();
        println!();
        return;
    };

    let command_name = command_key(&command).to_string();
    let start = Instant::now();

    let mut db_path = match command {
        Commands::Init => {
            if let Ok(path) = env::var("IMI_DB") {
                if !path.trim().is_empty() {
                    PathBuf::from(path)
                } else {
                    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                    cwd.join(".imi").join("state.db")
                }
            } else {
                let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                cwd.join(".imi").join("state.db")
            }
        }
        _ => discover_db_path().unwrap_or_else(|| PathBuf::from(".imi/state.db")),
    };

    if let Ok(abs) = fs::canonicalize(&db_path) {
        db_path = abs;
    }

    let mut conn = match open_connection(&db_path) {
        Ok(c) => c,
        Err(e) => {
            emit_error(out, &e);
            std::process::exit(1);
        }
    };

    if let Err(e) = run_schema(&conn) {
        emit_error(out, &format!("schema error: {e}"));
        std::process::exit(1);
    }

    let result = dispatch(&mut conn, &db_path, out, command);

    let duration_ms = start.elapsed().as_millis();
    log_event(&conn, &command_name, None, None, None, duration_ms as i64);
    let install_id = get_or_create_install_id(&conn);
    track(&command_name, &install_id, duration_ms);

    if let Err(e) = result {
        emit_error(out, &e);
        std::process::exit(1);
    }

    maybe_auto_update(&conn, out);
}

fn dispatch(conn: &mut Connection, db_path: &Path, out: OutputCtx, command: Commands) -> Result<(), String> {
    match command {
        Commands::Init => cmd_init(conn, db_path, out),
        Commands::Status => cmd_status(conn, db_path, out),
        Commands::Plan => cmd_plan(conn, db_path, out),
        Commands::Archive { goal_id } => cmd_archive(conn, out, goal_id),
        Commands::Context { goal_id } => cmd_context(conn, out, goal_id),
        Commands::Next { agent, goal_id } => cmd_next(conn, out, agent, goal_id),
        Commands::Start { agent, task_id } => cmd_next(conn, out, agent, Some(task_id)),
        Commands::Complete {
            agent,
            task_id,
            summary,
            interpretation,
            uncertainty,
            outcome,
        } => cmd_complete(conn, out, agent, task_id, summary.join(" "), interpretation, uncertainty, outcome),
        Commands::Run { task_id, model } => cmd_run(conn, db_path, out, task_id, model),
        Commands::Wrap {
            agent,
            task_id,
            ping_secs,
            checkpoint_secs,
            command,
        } => cmd_wrap(
            conn,
            db_path,
            out,
            agent,
            task_id,
            ping_secs,
            checkpoint_secs,
            command,
        ),
        Commands::Orchestrate {
            goal_id,
            workers,
            agent_prefix,
            ping_secs,
            checkpoint_secs,
            max_tasks,
            cli,
            command,
        } => cmd_orchestrate(
            conn,
            db_path,
            out,
            goal_id,
            workers,
            agent_prefix,
            ping_secs,
            checkpoint_secs,
            max_tasks,
            cli,
            command,
        ),
        Commands::Fail {
            agent,
            task_id,
            reason,
        } => cmd_fail(conn, out, agent, task_id, reason.join(" ")),
        Commands::Ping { task_id } => cmd_ping(conn, out, task_id),
        Commands::Checkpoint { task_id, note } => cmd_checkpoint(conn, out, task_id, note.join(" ")),
        Commands::Goal {
            name,
            desc,
            priority,
            why,
            why_long,
            for_who,
            success_signal,
            relevant_files,
            context,
            workspace,
        } => cmd_add_goal(conn, out, name, desc, priority, why_long.or(why), for_who, success_signal, relevant_files, context, workspace),
        Commands::Task {
            goal_id,
            title,
            desc,
            priority,
            why,
            why_long,
            context,
            relevant_files,
            tools,
            acceptance_criteria,
            workspace,
        } => cmd_add_task(conn, out, goal_id, title, desc, priority, why_long.or(why), context, relevant_files, tools, acceptance_criteria, workspace),
        Commands::Memory { lessons, action } => {
            if lessons {
                if action.is_some() {
                    return Err("cannot combine memory subcommands with --lessons".to_string());
                }
                cmd_lessons(conn, out)
            } else {
                cmd_memory(conn, out, action)
            }
        }
        Commands::Lesson {
            args,
            correct_behavior,
            verified_by,
        } => cmd_lesson(conn, out, args, correct_behavior, verified_by),
        Commands::Decide { what, why, affects } => cmd_decide(conn, out, what, why, affects),
        Commands::Log { note } => cmd_log(conn, out, note.join(" ")),
        Commands::Delete { id } => cmd_delete(conn, out, id),
        Commands::Reset { force } => cmd_reset(conn, out, force),
        Commands::Stats => cmd_stats(conn, out),
        Commands::Instructions { target } => cmd_instructions(out, target),
        Commands::Verify { task_id } => cmd_verify(conn, out, task_id),
        Commands::Audit => cmd_audit(conn, out),
        Commands::Think => cmd_think(conn, out),
        Commands::Check { task_id } => cmd_check(conn, out, task_id),
        Commands::Update => cmd_update(out),
        Commands::Ops { args } => cmd_ops(conn, out, args),
    }
}

fn extract_output_mode(args: Vec<String>) -> (OutputMode, Vec<String>) {
    let mut mode = OutputMode::Human;
    let mut keep = Vec::with_capacity(args.len());
    if let Some(first) = args.first() {
        keep.push(first.clone());
    }
    for arg in args.into_iter().skip(1) {
        match arg.as_str() {
            "--toon" => mode = OutputMode::Toon,
            "--json" => mode = OutputMode::Json,
            _ => keep.push(arg),
        }
    }
    (mode, keep)
}

fn command_key(command: &Commands) -> &'static str {
    match command {
        Commands::Init => "init",
        Commands::Status => "status",
        Commands::Plan => "plan",
        Commands::Archive { .. } => "archive",
        Commands::Context { .. } => "context",
        Commands::Next { .. } => "next",
        Commands::Start { .. } => "start",
        Commands::Complete { .. } => "complete",
        Commands::Run { .. } => "run",
        Commands::Wrap { .. } => "wrap",
        Commands::Orchestrate { .. } => "orchestrate",
        Commands::Fail { .. } => "fail",
        Commands::Ping { .. } => "ping",
        Commands::Checkpoint { .. } => "checkpoint",
        Commands::Goal { .. } => "goal",
        Commands::Task { .. } => "task",
        Commands::Memory { .. } => "memory",
        Commands::Lesson { .. } => "lesson",
        Commands::Decide { .. } => "decide",
        Commands::Log { .. } => "log",
        Commands::Delete { .. } => "delete",
        Commands::Reset { .. } => "reset",
        Commands::Stats => "stats",
        Commands::Instructions { .. } => "instructions",
        Commands::Verify { .. } => "verify",
        Commands::Audit => "audit",
        Commands::Think => "think",
        Commands::Check { .. } => "check",
        Commands::Update => "update",
        Commands::Ops { .. } => "ops",
    }
}

fn get_platform_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

fn fetch_latest_version() -> Option<String> {
    let out = Command::new("curl")
        .args([
            "-s", "--max-time", "5",
            "-H", "Accept: application/vnd.github.v3+json",
            "-H", "User-Agent: imi-cli",
            "https://api.github.com/repos/ProjectAI00/ai-db-imi/releases/latest",
        ])
        .output()
        .ok()?;
    let body = String::from_utf8(out.stdout).ok()?;
    let json: Value = serde_json::from_str(&body).ok()?;
    let tag = json["tag_name"].as_str()?;
    Some(tag.trim_start_matches('v').to_string())
}

fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> (u32, u32, u32) {
        let parts: Vec<u32> = s.split('.').filter_map(|p| p.parse().ok()).collect();
        (
            parts.first().copied().unwrap_or(0),
            parts.get(1).copied().unwrap_or(0),
            parts.get(2).copied().unwrap_or(0),
        )
    };
    parse(latest) > parse(current)
}

fn install_version(version: &str) -> Result<(), String> {
    let target = get_platform_target().ok_or("Unsupported platform")?;
    let url = format!(
        "https://github.com/ProjectAI00/ai-db-imi/releases/download/v{version}/imi-{target}.tar.gz"
    );
    let current_bin = std::env::current_exe().map_err(|e| format!("cannot find binary: {e}"))?;
    let bin_dir = current_bin.parent().ok_or("cannot find bin dir")?;
    let tmp = format!(
        "/tmp/imi-update-{}.tar.gz",
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
    );
    let dl = Command::new("curl")
        .args(["-fsSL", "--max-time", "60", "-o", &tmp, &url])
        .status()
        .map_err(|e| format!("curl failed: {e}"))?;
    if !dl.success() {
        return Err(format!("download failed for {url}"));
    }
    Command::new("tar")
        .args(["-xzf", &tmp, "-C", bin_dir.to_str().unwrap_or("/tmp")])
        .status()
        .map_err(|e| format!("tar failed: {e}"))?;
    let _ = fs::remove_file(&tmp);
    Ok(())
}

fn maybe_auto_update(conn: &Connection, out: OutputCtx) {
    if !matches!(out.mode, OutputMode::Human) {
        return;
    }
    let last_check: i64 = conn
        .query_row(
            "SELECT value FROM settings WHERE key='last_update_check' LIMIT 1",
            [],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    if now - last_check < 86_400 {
        return;
    }
    let _ = conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('last_update_check', ?1)",
        params![now.to_string()],
    );
    let Some(latest) = fetch_latest_version() else { return };
    if !is_newer(&latest, VERSION) {
        return;
    }
    if out.color {
        print!("\x1b[2m→ updating imi to v{latest}... \x1b[0m");
    } else {
        print!("→ updating imi to v{latest}... ");
    }
    let _ = io::stdout().flush();
    match install_version(&latest) {
        Ok(()) => {
            if out.color {
                println!("\x1b[32mdone\x1b[0m");
            } else {
                println!("done");
            }
        }
        Err(e) => {
            if out.color {
                println!("\x1b[2mskipped: {e}\x1b[0m");
            } else {
                println!("skipped: {e}");
            }
        }
    }
}

fn cmd_update(out: OutputCtx) -> Result<(), String> {
    if out.color {
        print!("Checking for updates... ");
    } else {
        print!("Checking for updates... ");
    }
    let _ = io::stdout().flush();
    let latest = fetch_latest_version()
        .ok_or("Could not reach GitHub — check your connection.")?;
    if !is_newer(&latest, VERSION) {
        println!("already on latest (v{VERSION})");
        return Ok(());
    }
    if out.color {
        println!("v{latest} available");
        print!("Installing... ");
    } else {
        println!("v{latest} available");
        print!("Installing... ");
    }
    let _ = io::stdout().flush();
    install_version(&latest)?;
    if out.color {
        println!("\x1b[32mdone\x1b[0m — updated v{VERSION} → v{latest}. Restart to use the new version.");
    } else {
        println!("done — updated v{VERSION} → v{latest}. Restart to use the new version.");
    }
    Ok(())
}

fn cmd_init(conn: &Connection, db_path: &Path, out: OutputCtx) -> Result<(), String> {
    let cwd = env::current_dir().map_err(|e| e.to_string())?;
    let imi_dir = cwd.join(".imi");
    fs::create_dir_all(&imi_dir).map_err(|e| format!("failed to create .imi dir: {e}"))?;
    run_schema(conn)?;
    register_workspace(conn, &cwd)?;

    if out.is_json() {
        println!(
            "{}",
            json!({"ok": true, "db_path": db_path.display().to_string()})
        );
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "init",
            &["db_path"],
            vec![vec![db_path.display().to_string()]],
        );
        print!("{}", t.finish());
    } else {
        let bold  = |s: &str| if out.color { format!("\x1b[1m{s}\x1b[0m") } else { s.to_string() };
        let dim   = |s: &str| if out.color { format!("\x1b[2m{s}\x1b[0m") } else { s.to_string() };
        let green = |s: &str| if out.color { format!("\x1b[32m{s}\x1b[0m") } else { s.to_string() };

        println!();
        println!("  {}  {}", bold("IMI"), dim(&format!("v{VERSION}")));
        println!("  {}", dim("The AI product manager for your AI agents."));
        println!();
        println!("  Stores your goals, decisions, and context in a local DB per project");
        println!("  so AI agents remember what you're building across every session.");
        println!("  Works natively with Cursor, Claude Code, Codex, and GitHub Copilot.");
        println!();
        println!("  {} Start any prompt with {} — e.g. {}",
            dim("→"),
            bold("imi"),
            dim("\"imi save this goal for later\""));
        println!();
        println!("  {} {}", green("✓"), format!("Initialized → {}", db_path.display()));
        println!();
    }
    Ok(())
}

fn cmd_status(conn: &Connection, db_path: &Path, out: OutputCtx) -> Result<(), String> {
    let goals_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM goals WHERE status!='archived'", [], |r| r.get(0))
        .unwrap_or(0);
    let (tasks_count, done_count, wip_count, review_count, todo_count): (i64, i64, i64, i64, i64) = conn
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN status='done' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN status='in_progress' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN status='review' THEN 1 ELSE 0 END),0),
                    COALESCE(SUM(CASE WHEN status='todo' THEN 1 ELSE 0 END),0)
             FROM tasks",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap_or((0, 0, 0, 0, 0));
    let memories_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM memories", [], |r| r.get(0))
        .unwrap_or(0);

    let goals = get_goals(conn)?;

    if out.is_json() {
        let mut goal_json = Vec::new();
        for g in goals.iter().filter(|g| g.status != "archived") {
            let tasks = get_tasks_for_goal(conn, &g.id)?;
            let total = tasks.len() as i64;
            let done = tasks.iter().filter(|t| t.status == "done").count() as i64;
            goal_json.push(json!({
                "id": g.id,
                "name": g.name,
                "status": g.status,
                "priority": g.priority,
                "done_tasks": done,
                "total_tasks": total,
                "tasks": tasks.iter().map(|t| json!({
                    "id": t.id,
                    "title": t.title,
                    "status": t.status,
                    "priority": t.priority,
                    "agent_id": t.agent_id
                })).collect::<Vec<_>>()
            }));
        }

        println!(
            "{}",
            json!({
                "version": VERSION,
                "db_path": db_path.display().to_string(),
                "counts": {
                    "goals": goals_count,
                    "tasks": tasks_count,
                    "done": done_count,
                    "wip": wip_count,
                    "review": review_count,
                    "todo": todo_count,
                    "memories": memories_count
                },
                "goals": goal_json
            })
        );
        return Ok(());
    }

    if out.is_toon() {
        let done_goals_count = goals.iter().filter(|g| g.status == "done").count();
        let active_goals_count = goals
            .iter()
            .filter(|g| g.status != "done" && g.status != "archived")
            .count();
        let mut t = ToonBuilder::new();
        t.section(
            "counts",
            &["goals", "active_goals", "done_goals", "tasks", "done", "wip", "review", "todo", "memories"],
            vec![vec![
                goals_count.to_string(),
                active_goals_count.to_string(),
                done_goals_count.to_string(),
                tasks_count.to_string(),
                done_count.to_string(),
                wip_count.to_string(),
                review_count.to_string(),
                todo_count.to_string(),
                memories_count.to_string(),
            ]],
        );

        let mut goal_rows = Vec::new();
        let mut task_rows = Vec::new();
        for g in goals
            .into_iter()
            .filter(|g| g.status != "done" && g.status != "archived")
        {
            let tasks = get_tasks_for_goal(conn, &g.id)?;
            let total = tasks.len();
            let done = tasks.iter().filter(|t| t.status == "done").count();
            goal_rows.push(vec![
                g.id.clone(),
                g.name.clone(),
                g.status.clone(),
                done.to_string(),
                total.to_string(),
            ]);
            for task in tasks
                .into_iter()
                .filter(|t| t.status == "todo" || t.status == "in_progress")
            {
                task_rows.push(vec![
                    g.id.clone(),
                    task.id,
                    task.title,
                    task.status,
                    task.priority,
                    task.agent_id.unwrap_or_default(),
                ]);
            }
        }
        t.section("goals", &["id", "name", "status", "done", "total"], goal_rows);
        t.section(
            "tasks",
            &["goal_id", "id", "title", "status", "priority", "agent"],
            task_rows,
        );
        print!("{}", t.finish());
        return Ok(());
    }

    println!(
        "{}",
        paint(out, "1", &format!("IMI State Engine  v{}", VERSION))
    );
    println!("## IMI State");
    println!("DB: {}", db_path.display());
    println!();
    println!("## Summary");
    println!("  Goals       {}", goals_count);
    println!(
        "  Tasks       {}  {}{} done  {}{} in progress  {}{} review  {}{} todo",
        tasks_count,
        status_icon(out, "done"),
        done_count,
        status_icon(out, "in_progress"),
        wip_count,
        status_icon(out, "review"),
        review_count,
        status_icon(out, "todo"),
        todo_count
    );
    println!("  Memories    {}", memories_count);
    println!();
    println!("## Active goals");

    let all_goals = get_goals(conn)?;
    let archived_goals_ct = all_goals.iter().filter(|g| g.status == "archived").count();
    let done_goals_ct = all_goals.iter().filter(|g| g.status == "done").count();
    let completed_ct = done_goals_ct + archived_goals_ct;
    for g in all_goals
        .into_iter()
        .filter(|g| g.status != "done" && g.status != "archived")
    {
        let tasks = get_tasks_for_goal(conn, &g.id)?;
        let total = tasks.len();
        let done = tasks.iter().filter(|t| t.status == "done").count();
        println!(
            "  {} {} {}  ({}/{})  {}",
            status_icon(out, &g.status),
            priority_icon(out, &g.priority),
            g.name,
            done,
            total,
            g.id
        );
        if done > 0 {
            println!("    {} {} done", status_icon(out, "done"), done);
        }
        for task in tasks
            .into_iter()
            .filter(|t| t.status == "todo" || t.status == "in_progress")
        {
            let agent = task.agent_id.unwrap_or_default();
            if agent.is_empty() {
                println!(
                    "    {} {} {}  {}",
                    status_icon(out, &task.status),
                    priority_icon(out, &task.priority),
                    task.title,
                    task.id
                );
            } else {
                println!(
                    "    {} {} {}  {}  @{}",
                    status_icon(out, &task.status),
                    priority_icon(out, &task.priority),
                    task.title,
                    task.id,
                    agent
                );
            }
        }
        println!();
    }
    if completed_ct > 0 {
        println!(
            "## Completed goals\n  ✓ {} completed goal{}  (run `imi goals --archived` to list)",
            completed_ct,
            if completed_ct == 1 { "" } else { "s" }
        );
        println!();
    }

    Ok(())
}

fn cmd_goals(conn: &Connection, out: OutputCtx, archived: bool) -> Result<(), String> {
    let goals: Vec<GoalRow> = get_goals(conn)?
        .into_iter()
        .filter(|g| (archived && g.status == "archived") || (!archived && g.status != "archived"))
        .collect();

    if out.is_json() {
        println!(
            "{}",
            json!(goals
                .iter()
                .map(|g| json!({
                    "id": g.id,
                    "name": g.name,
                    "description": g.description,
                    "status": g.status,
                    "priority": g.priority,
                    "created_at": g.created_at,
                    "created_ago": ago(g.created_at)
                }))
                .collect::<Vec<_>>())
        );
        return Ok(());
    }

    if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "goals",
            &["id", "name", "status", "priority", "created_ago"],
            goals
                .iter()
                .map(|g| {
                    vec![
                        g.id.clone(),
                        g.name.clone(),
                        g.status.clone(),
                        g.priority.clone(),
                        ago(g.created_at),
                    ]
                })
                .collect(),
        );
        print!("{}", t.finish());
        return Ok(());
    }

    for g in goals {
        println!(
            "{} {} {}  {}  {}",
            status_icon(out, &g.status),
            priority_icon(out, &g.priority),
            g.id,
            g.name,
            ago(g.created_at)
        );
    }

    Ok(())
}

fn cmd_archive(conn: &Connection, out: OutputCtx, goal_prefix: String) -> Result<(), String> {
    let goal_id = resolve_id_prefix(conn, "goals", &goal_prefix)?
        .ok_or_else(|| format!("goal not found: {goal_prefix}"))?;
    let now = now_ts();
    conn.execute(
        "UPDATE goals SET status='archived', updated_at=?1 WHERE id=?2",
        params![now, goal_id],
    )
    .map_err(|e| e.to_string())?;
    emit_simple_ok(out, "Archived goal")?;
    Ok(())
}

fn cmd_tasks(conn: &Connection, out: OutputCtx, filter: Option<String>) -> Result<(), String> {
    let filter = filter.unwrap_or_else(|| "all".to_string());
    let mut query = "SELECT t.id, t.title, COALESCE(t.description,''), COALESCE(t.why,''), t.goal_id, COALESCE(t.status,'todo'), COALESCE(t.priority,'medium'), t.agent_id, COALESCE(t.created_at,0), COALESCE(g.name,'') FROM tasks t LEFT JOIN goals g ON t.goal_id=g.id".to_string();
    let mut params_vec: Vec<String> = Vec::new();

    match filter.as_str() {
        "all" => {}
        "todo" | "done" | "review" => {
            query.push_str(" WHERE t.status=?1");
            params_vec.push(filter.clone());
        }
        "wip" | "in_progress" => {
            query.push_str(" WHERE t.status='in_progress'");
        }
        prefix => {
            let goal_id = resolve_id_prefix(conn, "goals", prefix)?
                .ok_or_else(|| format!("goal not found for prefix: {prefix}"))?;
            query.push_str(" WHERE t.goal_id=?1");
            params_vec.push(goal_id);
        }
    }
    query.push_str(" ORDER BY COALESCE(t.updated_at,t.created_at,0) DESC");

    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let rows = if params_vec.is_empty() {
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(9)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
    } else {
        stmt.query_map(params![params_vec[0].clone()], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(9)?,
            ))
        })
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?
    };

    if out.is_json() {
        println!(
            "{}",
            json!(rows
                .iter()
                .map(|r| json!({"id": r.0, "title": r.1, "status": r.2, "priority": r.3, "goal": r.4}))
                .collect::<Vec<_>>())
        );
        return Ok(());
    }

    if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "tasks",
            &["id", "title", "status", "priority", "goal"],
            rows.iter()
                .map(|r| vec![r.0.clone(), r.1.clone(), r.2.clone(), r.3.clone(), r.4.clone()])
                .collect(),
        );
        print!("{}", t.finish());
        return Ok(());
    }

    for (id, title, status, priority, goal_name) in rows {
        if goal_name.is_empty() {
            println!(
                "{} {} {}  {}",
                status_icon(out, &status),
                priority_icon(out, &priority),
                id,
                title
            );
        } else {
            println!(
                "{} {} {}  {}  — {}",
                status_icon(out, &status),
                priority_icon(out, &priority),
                id,
                title,
                goal_name
            );
        }
    }
    Ok(())
}

fn cmd_plan(conn: &Connection, db_path: &Path, out: OutputCtx) -> Result<(), String> {
    if out.is_json() || out.is_toon() {
        return cmd_status(conn, db_path, out);
    }
    cmd_context(conn, out, None)?;
    println!();
    cmd_status(conn, db_path, out)
}

fn cmd_context(conn: &Connection, out: OutputCtx, goal_id: Option<String>) -> Result<(), String> {
    if let Some(goal_prefix) = goal_id {
        return cmd_context_goal(conn, out, goal_prefix);
    }

    let now = now_ts();
    let week_ago = now - 7 * 24 * 3600;

    let direction = query_direction(conn, Some(week_ago), 10)?;
    let decisions = query_decisions(conn, 15)?;
    let founding_intent: Vec<(String, String, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT content, COALESCE(author,''), COALESCE(created_at,0)
                 FROM direction_notes
                 ORDER BY COALESCE(created_at,0) ASC
                 LIMIT 2",
            )
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    let killed_decisions: Vec<(String, String, String, i64)> = {
        let mut stmt = conn
            .prepare(
                "SELECT what, why, COALESCE(affects,''), COALESCE(created_at,0)
                 FROM decisions
                 WHERE LOWER(COALESCE(what,'')) LIKE '%killed%'
                    OR LOWER(COALESCE(what,'')) LIKE '%not%'
                    OR LOWER(COALESCE(why,'')) LIKE '%instead%'
                    OR LOWER(COALESCE(why,'')) LIKE '%rejected%'
                 ORDER BY COALESCE(created_at,0) DESC
                 LIMIT 5",
            )
            .map_err(|e| e.to_string())?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| e.to_string())?;
        mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?
    };
    let active_goals = query_active_goals(conn, 10)?;
    let wip = query_wip_tasks(conn, 10)?;
    let lessons = query_lessons(conn, 15)?;
    let memories = query_active_memories(conn, 15)?;

    if out.is_json() {
        let founding_intent_json: Vec<Value> = founding_intent
            .iter()
            .map(|d| json!({"content": d.0, "author": d.1, "created_at": d.2}))
            .collect();
        let killed_decisions_json: Vec<Value> = killed_decisions
            .iter()
            .map(|d| json!({"what": d.0, "why": d.1, "affects": d.2, "created_at": d.3}))
            .collect();
        let direction_json: Vec<Value> = direction
            .iter()
            .map(|d| json!({"content": d.0, "author": d.1, "created_at": d.2}))
            .collect();
        let decisions_json: Vec<Value> = decisions
            .iter()
            .map(|d| json!({"what": d.0, "why": d.1, "affects": d.2, "created_at": d.3}))
            .collect();
        let goals_json: Vec<Value> = active_goals.iter().map(goal_to_value).collect();
        let wip_json: Vec<Value> = wip.iter().map(wip_task_to_value).collect();
        let lessons_json: Vec<Value> = lessons.iter().map(lesson_to_value).collect();
        let memories_json: Vec<Value> = memories.iter().map(memory_to_value).collect();
        println!(
            "{}",
            json!({
                "product_vision": founding_intent_json,
                "killed_decisions": killed_decisions_json,
                "direction": direction_json,
                "decisions": decisions_json,
                "goals": goals_json,
                "wip": wip_json,
                "verified_lessons": lessons_json,
                "memories": memories_json
            })
        );
        return Ok(());
    }

    if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "product_vision",
            &["founding_intent", "author", "created_at"],
            founding_intent
                .iter()
                .map(|d| vec![d.0.clone(), d.1.clone(), d.2.to_string()])
                .collect(),
        );
        t.section(
            "what_was_killed_and_why",
            &["what", "why", "affects", "created_at"],
            killed_decisions
                .iter()
                .map(|d| vec![d.0.clone(), d.1.clone(), d.2.clone(), d.3.to_string()])
                .collect(),
        );
        t.section(
            "direction",
            &["content", "author", "created_at"],
            direction
                .iter()
                .map(|d| vec![d.0.clone(), d.1.clone(), d.2.to_string()])
                .collect(),
        );
        t.section(
            "decisions",
            &["what", "why", "affects", "created_at"],
            decisions
                .iter()
                .map(|d| vec![d.0.clone(), d.1.clone(), d.2.clone(), d.3.to_string()])
                .collect(),
        );
        t.section(
            "goals",
            &["id", "name", "status", "priority"],
            active_goals
                .iter()
                .map(|g| vec![g.id.clone(), g.name.clone(), g.status.clone(), g.priority.clone()])
                .collect(),
        );
        t.section(
            "wip",
            &["id", "title", "goal", "agent"],
            wip.iter()
                .map(|w| {
                    vec![
                        w.id.clone(),
                        w.title.clone(),
                        w.goal_name.clone().unwrap_or_default(),
                        w.agent_id.clone().unwrap_or_default(),
                    ]
                })
                .collect(),
        );
        t.section(
            "verified_lessons",
            &["what_went_wrong", "correct_behavior", "verified_by", "created_at"],
            lessons
                .iter()
                .map(|l| {
                    vec![
                        l.what_went_wrong.clone(),
                        l.correct_behavior.clone(),
                        l.verified_by.clone(),
                        l.created_at.to_string(),
                    ]
                })
                .collect(),
        );
        t.section(
            "memories",
            &["key", "value", "type", "source"],
            memories
                .iter()
                .map(|m| vec![m.key.clone(), m.value.clone(), m.typ.clone(), m.source.clone()])
                .collect(),
        );
        print!("{}", t.finish());
        return Ok(());
    }

    println!("## IMI Context");
    println!("What matters right now:\n");

    println!("## Product Vision");
    if founding_intent.is_empty() {
        println!("  Founding intent: (none)");
    } else {
        for d in &founding_intent {
            let author = if d.1.is_empty() { "unknown" } else { &d.1 };
            println!("  Founding intent: {}\n    {} ago  @{}", d.0, ago(d.2), author);
        }
    }

    println!("\n## What was killed and why");
    if killed_decisions.is_empty() {
        println!("  (none)");
    } else {
        for d in &killed_decisions {
            println!("  {}\n    why: {}\n    affects: {}\n    {} ago", d.0, d.1, d.2, ago(d.3));
        }
    }

    println!("\n## Direction notes (last 7 days)");
    if direction.is_empty() {
        println!("  (none)");
    } else {
        for d in &direction {
            let author = if d.1.is_empty() { "unknown" } else { &d.1 };
            println!("  ▸ {}\n    {} ago  @{}", d.0, ago(d.2), author);
        }
    }

    println!("\n## Decisions");
    if decisions.is_empty() {
        println!("  (none)");
    } else {
        for d in &decisions {
            println!("  {}\n    why: {}\n    affects: {}\n    {} ago", d.0, d.1, d.2, ago(d.3));
        }
    }

    if !lessons.is_empty() {
        println!("\n## Verified Lessons");
        for l in &lessons {
            println!(
                "  - {}\n    correct behavior: {}\n    verified by: {} ({} ago)",
                l.what_went_wrong,
                l.correct_behavior,
                l.verified_by,
                ago(l.created_at)
            );
        }
    }

    println!("\n## Active goals");
    if active_goals.is_empty() {
        println!("  (none)");
    } else {
        for g in &active_goals {
            let tasks = get_tasks_for_goal(conn, &g.id)?;
            println!(
                "  {} {} {}  {} ago",
                status_icon(out, &g.status),
                priority_icon(out, &g.priority),
                g.name,
                ago(g.created_at)
            );
            if !g.why_.is_empty() {
                println!("    why: {}", g.why_);
            }
            for task in tasks
                .into_iter()
                .filter(|t| t.status == "todo" || t.status == "in_progress")
            {
                println!(
                    "    {} {} {}  {}",
                    status_icon(out, &task.status),
                    priority_icon(out, &task.priority),
                    task.title,
                    task.id
                );
            }
        }
    }

    println!("\n## In progress");
    if wip.is_empty() {
        println!("  (nothing in progress)");
    } else {
        for t in &wip {
            println!(
                "  {} {} {}  {}",
                status_icon(out, &t.status),
                priority_icon(out, &t.priority),
                t.title,
                t.id
            );
        }
    }

    Ok(())
}

fn cmd_context_goal(conn: &Connection, out: OutputCtx, goal_prefix: String) -> Result<(), String> {
    let goal_id = resolve_id_prefix(conn, "goals", &goal_prefix)?
        .ok_or_else(|| format!("goal not found: {goal_prefix}"))?;
    let goal = get_goal(conn, &goal_id)?.ok_or_else(|| "goal not found".to_string())?;
    let tasks = get_tasks_for_goal(conn, &goal_id)?;
    let memories = query_memories(conn, Some(&goal_id), 30)?;

    if out.is_json() {
        let tasks_json: Vec<Value> = tasks.iter().map(task_to_value).collect();
        let memories_json: Vec<Value> = memories.iter().map(memory_to_value).collect();
        println!(
            "{}",
            json!({
                "goal": goal_to_value(&goal),
                "tasks": tasks_json,
                "memories": memories_json
            })
        );
        return Ok(());
    }

    if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "goal",
            &["id", "name", "status", "why", "for_who", "success"],
            vec![vec![
                goal.id.clone(),
                goal.name.clone(),
                goal.status.clone(),
                goal.why_.clone(),
                goal.for_who.clone(),
                goal.success_signal.clone(),
            ]],
        );
        t.section(
            "tasks",
            &["id", "title", "status", "priority", "agent"],
            tasks
                .iter()
                .map(|x| {
                    vec![
                        x.id.clone(),
                        x.title.clone(),
                        x.status.clone(),
                        x.priority.clone(),
                        x.agent_id.clone().unwrap_or_default(),
                    ]
                })
                .collect(),
        );
        t.section(
            "memories",
            &["key", "value", "type", "source"],
            memories
                .iter()
                .map(|m| vec![m.key.clone(), m.value.clone(), m.typ.clone(), m.source.clone()])
                .collect(),
        );
        print!("{}", t.finish());
        return Ok(());
    }

    println!("## Goal");
    println!("{} {}  {}", status_icon(out, &goal.status), goal.name, goal.id);
    if !goal.why_.is_empty() {
        println!("why: {}", goal.why_);
    }
    if !goal.for_who.is_empty() {
        println!("for who: {}", goal.for_who);
    }
    if !goal.success_signal.is_empty() {
        println!("success: {}", goal.success_signal);
    }

    println!("\n## Tasks");
    if tasks.is_empty() {
        println!("  (none)");
    } else {
        for t in tasks {
            println!(
                "  {} {} {}  {}",
                status_icon(out, &t.status),
                priority_icon(out, &t.priority),
                t.title,
                t.id
            );
        }
    }

    println!("\n## Memories");
    if memories.is_empty() {
        println!("  (none)");
    } else {
        for m in memories {
            println!("  [{}] {} = {}", m.typ, m.key, m.value);
        }
    }

    Ok(())
}

fn cmd_next(
    conn: &mut Connection,
    out: OutputCtx,
    agent: Option<String>,
    goal_prefix: Option<String>,
) -> Result<(), String> {
    let released = release_stale_locks(conn)?;
    let goal_filter = if let Some(prefix) = goal_prefix {
        if let Some(goal_id) = resolve_id_prefix(conn, "goals", &prefix)? {
            Some(goal_id)
        } else if let Some(task_id) = resolve_id_prefix(conn, "tasks", &prefix)? {
            return cmd_start(conn, out, agent, task_id);
        } else {
            return Err(format!("goal or task not found: {prefix}"));
        }
    } else {
        None
    };
    let agent_id = current_agent(agent.as_deref());

    match claim_next_task(conn, goal_filter.as_deref(), &agent_id)? {
        ClaimResult::NoTasks => {
            if out.is_json() {
                println!("{}", json!({"ok": true, "no_tasks": true, "released_stale": released}));
            } else if out.is_toon() {
                let mut t = ToonBuilder::new();
                t.section("no_tasks", &["note"], vec![vec!["all_done_or_claimed".to_string()]]);
                print!("{}", t.finish());
            } else {
                if released > 0 {
                    println!("⚠ Released {released} stale in-progress task(s)");
                }
                println!("No available tasks to claim (all tasks are done or already locked).");
            }
            Ok(())
        }
        ClaimResult::RaceLost => {
            if out.is_json() {
                println!("{}", json!({"ok": true, "race_lost": true}));
            } else if out.is_toon() {
                let mut t = ToonBuilder::new();
                t.section("race_lost", &["note"], vec![vec!["try_again".to_string()]]);
                print!("{}", t.finish());
            } else {
                if released > 0 {
                    println!("⚠ Released {released} stale in-progress task(s)");
                }
                println!("Another agent claimed the task first; retry `imi next`.");
            }
            Ok(())
        }
        ClaimResult::Claimed(task) => {
            let goal = task
                .goal_id
                .as_ref()
                .and_then(|gid| get_goal(conn, gid).ok().flatten());
            let goal_reasoning_decisions: Vec<(String, String, String, i64)> = if let Some(g) = goal.as_ref()
            {
                let mut stmt = conn
                    .prepare(
                        "SELECT what, why, COALESCE(affects,''), COALESCE(created_at,0)
                         FROM decisions
                         WHERE LOWER(COALESCE(affects,'')) LIKE LOWER(?1)
                         ORDER BY COALESCE(created_at,0) DESC
                         LIMIT 2",
                    )
                    .map_err(|e| e.to_string())?;
                let mapped = stmt
                    .query_map(params![format!("%{}%", g.name)], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })
                    .map_err(|e| e.to_string())?;
                mapped
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|e| e.to_string())?
            } else {
                vec![]
            };
            let decisions = query_decisions(conn, 10)?;
            let direction = query_direction(conn, Some(now_ts() - 7 * 24 * 3600), 8)?;
            let lessons = query_lessons(conn, 15)?;
            let memories = if let Some(gid) = &task.goal_id {
                query_memories(conn, Some(gid), 15)?
            } else {
                query_active_memories(conn, 15)?
            };
            let last_failure: Option<String> = if let Some(gid) = &task.goal_id {
                conn.query_row(
                    "SELECT value FROM memories WHERE type='failure' AND (goal_id=?1 OR task_id IN (SELECT id FROM tasks WHERE goal_id=?1)) ORDER BY created_at DESC LIMIT 1",
                    params![gid],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
            } else {
                conn.query_row(
                    "SELECT value FROM memories WHERE type='failure' ORDER BY created_at DESC LIMIT 1",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(|e| e.to_string())?
            };

            if out.is_json() {
                let relevant_files: Vec<String> = serde_json::from_str(&task.relevant_files).unwrap_or_default();
                let tools: Vec<String> = serde_json::from_str(&task.tools).unwrap_or_default();
                let goal_name = goal.as_ref().map(|g| g.name.clone()).unwrap_or_default();
                let prior_work_on_goal: Vec<Value> = if let Some(gid) = &task.goal_id {
                    let mut stmt = conn
                        .prepare(
                            "SELECT COALESCE(m.task_id,''), COALESCE(t.title,''), COALESCE(m.value,''), COALESCE(m.created_at,0)
                             FROM memories m
                             JOIN tasks t ON t.id = m.task_id
                             WHERE t.goal_id=?1 AND m.key='completion_summary' AND COALESCE(m.task_id,'') != ?2
                             ORDER BY COALESCE(m.created_at,0) DESC
                             LIMIT 3",
                        )
                        .map_err(|e| e.to_string())?;
                    let rows: Vec<(String, String, String, i64)> = stmt
                        .query_map(params![gid, task.id.clone()], |r| {
                            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                        })
                        .map_err(|e| e.to_string())?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?;
                    rows.into_iter()
                        .map(|(task_id, title, summary, created_at)| {
                            json!({"task_id": task_id, "title": title, "completion_summary": summary, "created_at": created_at})
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let decisions_affecting_goal: Vec<Value> = if goal_name.is_empty() {
                    Vec::new()
                } else {
                    let mut stmt = conn
                        .prepare(
                            "SELECT COALESCE(what,''), COALESCE(why,''), COALESCE(affects,''), COALESCE(created_at,0)
                             FROM decisions
                             WHERE COALESCE(affects,'') LIKE ?1
                             ORDER BY COALESCE(created_at,0) DESC
                             LIMIT 3",
                        )
                        .map_err(|e| e.to_string())?;
                    let pattern = format!("%{}%", goal_name);
                    let rows: Vec<(String, String, String, i64)> = stmt
                        .query_map(params![pattern], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                        .map_err(|e| e.to_string())?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|e| e.to_string())?;
                    rows.into_iter()
                        .map(|(what, why, affects, created_at)| {
                            json!({"what": what, "why": why, "affects": affects, "created_at": created_at})
                        })
                        .collect()
                };
                let goal_json = goal.as_ref().map(goal_to_value);
                let decisions_json: Vec<Value> = decisions
                    .iter()
                    .map(|d| json!({"what": d.0, "why": d.1, "affects": d.2, "created_at": d.3}))
                    .collect();
                let direction_json: Vec<Value> = direction
                    .iter()
                    .map(|d| json!({"content": d.0, "author": d.1, "created_at": d.2}))
                    .collect();
                let lessons_json: Vec<Value> = lessons.iter().map(lesson_to_value).collect();
                let memories_json: Vec<Value> = memories.iter().map(memory_to_value).collect();
                println!(
                    "{}",
                    json!({
                        "ok": true,
                        "released_stale": released,
                        "verified_lessons": lessons_json,
                        "task": {
                            "id": task.id,
                            "title": task.title,
                            "why": task.why_,
                            "description": task.description,
                            "context": task.context,
                            "relevant_files": relevant_files,
                            "tools": tools,
                            "acceptance_criteria": task.acceptance_criteria,
                            "workspace_path": task.workspace_path
                        },
                        "goal": goal_json,
                        "goal_description": goal.as_ref().map(|g| g.description.clone()).unwrap_or_default(),
                        "goal_why": goal.as_ref().map(|g| g.why_.clone()).unwrap_or_default(),
                        "prior_work_on_goal": prior_work_on_goal,
                        "decisions_affecting_goal": decisions_affecting_goal,
                        "decisions": decisions_json,
                        "direction": direction_json,
                        "last_failure": last_failure,
                        "memories": memories_json
                    })
                );
                return Ok(());
            }

            if out.is_toon() {
                let mut t = ToonBuilder::new();
                t.section(
                    "verified_lessons",
                    &["what_went_wrong", "correct_behavior", "verified_by", "created_at"],
                    lessons
                        .iter()
                        .map(|l| {
                            vec![
                                l.what_went_wrong.clone(),
                                l.correct_behavior.clone(),
                                l.verified_by.clone(),
                                l.created_at.to_string(),
                            ]
                        })
                        .collect(),
                );
                t.section(
                    "task",
                    &["id", "title", "why"],
                    vec![vec![task.id.clone(), task.title.clone(), task.why_.clone()]],
                );
                t.section("desc", &["text"], vec![vec![task.description.clone()]]);
                if !task.context.is_empty() {
                    t.section("context", &["text"], vec![vec![task.context.clone()]]);
                }
                if task.relevant_files != "[]" && !task.relevant_files.is_empty() {
                    t.section("relevant_files", &["files"], vec![vec![task.relevant_files.clone()]]);
                }
                if !task.acceptance_criteria.is_empty() {
                    t.section("acceptance_criteria", &["criteria"], vec![vec![task.acceptance_criteria.clone()]]);
                }
                if task.tools != "[]" && !task.tools.is_empty() {
                    t.section("tools", &["tools"], vec![vec![task.tools.clone()]]);
                }
                if !task.workspace_path.is_empty() {
                    t.section("workspace", &["path"], vec![vec![task.workspace_path.clone()]]);
                }
                let mut why_this_matters = String::from("## Why this matters\n");
                if let Some(g) = goal.as_ref() {
                    if !g.why_.is_empty() {
                        why_this_matters.push_str(&format!("- Goal why: {}\n", g.why_));
                    }
                    if !g.description.is_empty() {
                        why_this_matters.push_str(&format!("- Goal description: {}\n", g.description));
                    }
                }
                if goal_reasoning_decisions.is_empty() {
                    why_this_matters.push_str("- Related decisions: none\n");
                } else {
                    why_this_matters.push_str("- Related decisions:\n");
                    for d in &goal_reasoning_decisions {
                        why_this_matters.push_str(&format!("  - {} (why: {})\n", d.0, d.1));
                    }
                }
                t.section("why_this_matters", &["text"], vec![vec![why_this_matters]]);
                if let Some(g) = goal {
                    t.section(
                        "goal",
                        &["name", "why", "for_who", "success"],
                        vec![vec![g.name, g.why_, g.for_who, g.success_signal]],
                    );
                }
                t.section(
                    "decisions",
                    &["what", "why", "affects"],
                    decisions
                        .iter()
                        .map(|d| vec![d.0.clone(), d.1.clone(), d.2.clone()])
                        .collect(),
                );
                t.section(
                    "direction",
                    &["content", "author"],
                    direction
                        .iter()
                        .map(|d| vec![d.0.clone(), d.1.clone()])
                        .collect(),
                );
                if let Some(failure) = last_failure {
                    t.section("last_failure", &["note"], vec![vec![failure]]);
                }
                t.section(
                    "memories",
                    &["key", "value", "type"],
                    memories
                        .iter()
                        .map(|m| vec![m.key.clone(), m.value.clone(), m.typ.clone()])
                        .collect(),
                );
                print!("{}", t.finish());
                return Ok(());
            }

            if released > 0 {
                println!("⚠ Released {released} stale in-progress task(s)");
            }
            if !lessons.is_empty() {
                println!("## Verified Lessons");
                for l in &lessons {
                    println!(
                        "  - {}\n    correct behavior: {}\n    verified by: {} ({} ago)",
                        l.what_went_wrong,
                        l.correct_behavior,
                        l.verified_by,
                        ago(l.created_at)
                    );
                }
                println!();
            }
            println!("🔒 Task claimed and locked to this agent");
            println!("ID: {}  {}", task.id, task.title);
            if !task.why_.is_empty() {
                println!("Why: {}", task.why_);
            }
            if !task.description.is_empty() {
                println!("\n{}", task.description);
            }
            if !task.context.is_empty() {
                println!("\nContext: {}", task.context);
            }
            if let Some(g) = goal {
                println!("\nGoal: {}", g.name);
                if !g.why_.is_empty() {
                    println!("  Why: {}", g.why_);
                }
                if !g.for_who.is_empty() {
                    println!("  For who: {}", g.for_who);
                }
                if !g.success_signal.is_empty() {
                    println!("  Success: {}", g.success_signal);
                }
            }
            if !task.acceptance_criteria.is_empty() {
                println!("\nAcceptance criteria (verify against these — do not rewrite them to match your implementation):");
                println!("{}", task.acceptance_criteria);
            }
            println!("\n⚠ Before you begin: find the direction note or decision in the DB that authorizes this task.");
            println!("  If you can't trace this work to something a human wrote, stop and surface it.");
            println!("  Do not reduce scope without recording why. Do not write your own acceptance criteria.");
            if !direction.is_empty() {
                println!("\nHuman direction:");
                for d in &direction {
                    println!("  - {}", d.0);
                }
            }
            if !decisions.is_empty() {
                println!("\nDecisions:");
                for d in decisions.iter().take(5) {
                    println!("  - {} — {}", d.0, d.1);
                }
            }
            if let Some(failure) = last_failure {
                println!("\nLast failure: {}", failure);
            }
            Ok(())
        }
    }
}

fn cmd_start(conn: &Connection, out: OutputCtx, agent: Option<String>, task_id: String) -> Result<(), String> {
    let agent_id = current_agent(agent.as_deref());
    let task = ensure_task_in_progress(conn, &task_id, &agent_id)?;

    emit_simple_ok(out, &format!("Task {id} is now in progress and locked to this agent", id = task.id))?;
    Ok(())
}

struct LifecycleWatchdog {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl LifecycleWatchdog {
    fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn spawn_lifecycle_watchdog(
    db_path: PathBuf,
    task_id: String,
    goal_id: Option<String>,
    agent_id: String,
    ping_secs: u64,
    checkpoint_secs: u64,
) -> Option<LifecycleWatchdog> {
    if ping_secs == 0 && checkpoint_secs == 0 {
        return None;
    }

    let stop = Arc::new(AtomicBool::new(false));
    let stop_ref = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        let started = Instant::now();
        let mut last_ping = Instant::now();
        let mut last_checkpoint = Instant::now();

        loop {
            if stop_ref.load(Ordering::Relaxed) {
                break;
            }

            thread::sleep(Duration::from_secs(1));
            if stop_ref.load(Ordering::Relaxed) {
                break;
            }

            let now = now_ts();

            if ping_secs > 0 && last_ping.elapsed().as_secs() >= ping_secs {
                if let Ok(conn) = open_connection(&db_path) {
                    let updated = conn
                        .execute(
                            "UPDATE tasks SET updated_at=?1, last_ping_at=?1 WHERE id=?2 AND status='in_progress'",
                            params![now, task_id.clone()],
                        )
                        .unwrap_or(0);
                    if updated == 0 {
                        break;
                    }
                }
                last_ping = Instant::now();
            }

            if checkpoint_secs > 0 && last_checkpoint.elapsed().as_secs() >= checkpoint_secs {
                if let Ok(conn) = open_connection(&db_path) {
                    let elapsed_minutes = (started.elapsed().as_secs() / 60).max(1);
                    let note = format!(
                        "Auto-checkpoint: still running via IMI wrapper ({}m elapsed)",
                        elapsed_minutes
                    );
                    let _ = conn.execute(
                        "INSERT INTO memories (id, goal_id, task_id, key, value, type, reasoning, source, created_at)
                         VALUES (?1, ?2, ?3, 'checkpoint', ?4, 'checkpoint', ?4, ?5, ?6)",
                        params![gen_id(), goal_id.clone(), task_id.clone(), note, agent_id.clone(), now],
                    );
                    let updated = conn
                        .execute(
                            "UPDATE tasks SET updated_at=?1, last_ping_at=?1 WHERE id=?2 AND status='in_progress'",
                            params![now, task_id.clone()],
                        )
                        .unwrap_or(0);
                    if updated == 0 {
                        break;
                    }
                }
                last_checkpoint = Instant::now();
                last_ping = Instant::now();
            }
        }
    });

    Some(LifecycleWatchdog {
        stop,
        handle: Some(handle),
    })
}

fn ensure_task_in_progress(conn: &Connection, task_id: &str, agent_id: &str) -> Result<TaskRow, String> {
    let mut task = resolve_task(conn, task_id)?;
    if task.status == "done" {
        return Err("task is already done".to_string());
    }
    if task.status == "in_progress" {
        if let Some(owner) = &task.agent_id {
            if !owner.is_empty() && owner != agent_id {
                return Err(format!("task already in progress by {owner}"));
            }
        }
    }

    let now = now_ts();
    conn.execute(
        "UPDATE tasks SET status='in_progress', agent_id=?1, updated_at=?2, last_ping_at=?2 WHERE id=?3",
        params![agent_id, now, task.id],
    )
    .map_err(|e| e.to_string())?;
    if let Some(goal_id) = &task.goal_id {
        sync_goal(conn, goal_id)?;
    }

    task.status = "in_progress".to_string();
    task.agent_id = Some(agent_id.to_string());
    Ok(task)
}

fn task_status(conn: &Connection, task_id: &str) -> Result<String, String> {
    conn.query_row("SELECT status FROM tasks WHERE id=?1", params![task_id], |r| r.get(0))
        .map_err(|e| e.to_string())
}

fn cmd_complete(
    conn: &Connection,
    out: OutputCtx,
    agent: Option<String>,
    task_id: String,
    summary: String,
    interpretation: Option<String>,
    uncertainty: Option<String>,
    outcome: Option<String>,
) -> Result<(), String> {
    let agent_id = current_agent(agent.as_deref());
    let task = resolve_task(conn, &task_id)?;
    let now = now_ts();
    let summary_text = if summary.trim().is_empty() {
        "completed".to_string()
    } else {
        summary
    };

    // Surface original acceptance criteria so the agent verifies against what was asked, not what was built
    if !out.is_json() {
        let criteria: Option<String> = conn.query_row(
            "SELECT COALESCE(acceptance_criteria,'') FROM tasks WHERE id=?1",
            params![task.id],
            |r| r.get(0),
        ).optional().map_err(|e| e.to_string())?.flatten();
        if let Some(c) = criteria.filter(|s| !s.is_empty()) {
            println!("Acceptance criteria set for this task:");
            println!("{}", c);
            println!("Confirm your implementation satisfies these as written — not a reduced version of them.");
            println!();
        }
    }

    conn.execute(
        "UPDATE tasks SET status='done', summary=?1, agent_id=?2, updated_at=?3, completed_at=?3 WHERE id=?4",
        params![summary_text, agent_id, now, task.id],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "INSERT INTO memories (id, goal_id, task_id, key, value, type, reasoning, source, created_at)
         VALUES (?1, ?2, ?3, 'completion_summary', ?4, 'completion', ?4, ?5, ?6)",
        params![
            gen_id(),
            task.goal_id,
            task.id,
            summary_text,
            agent_id,
            now
        ],
    )
    .map_err(|e| e.to_string())?;

    if let Some(interp) = interpretation {
        if !interp.trim().is_empty() {
            conn.execute(
                "INSERT INTO memories (id, goal_id, task_id, key, value, type, reasoning, source, created_at)
                 VALUES (?1, ?2, ?3, 'interpretation', ?4, 'completion', ?4, ?5, ?6)",
                params![gen_id(), task.goal_id, task.id, interp, agent_id, now],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    if let Some(unc) = uncertainty {
        if !unc.trim().is_empty() {
            conn.execute(
                "INSERT INTO memories (id, goal_id, task_id, key, value, type, reasoning, source, created_at)
                 VALUES (?1, ?2, ?3, 'uncertainty', ?4, 'completion', ?4, ?5, ?6)",
                params![gen_id(), task.goal_id, task.id, unc, agent_id, now],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    if let Some(out_note) = outcome {
        if !out_note.trim().is_empty() {
            conn.execute(
                "INSERT INTO memories (id, goal_id, task_id, key, value, type, reasoning, source, created_at)
                 VALUES (?1, ?2, ?3, 'outcome', ?4, 'outcome', ?4, ?5, ?6)",
                params![gen_id(), task.goal_id, task.id, out_note, agent_id, now],
            )
            .map_err(|e| e.to_string())?;
        }
    }

    if let Some(ref goal_id) = task.goal_id {
        sync_goal(conn, goal_id)?;
        // auto-archive when all tasks under the goal are done
        let goal_status: String = conn
            .query_row("SELECT status FROM goals WHERE id=?1", params![goal_id], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if goal_status == "done" {
            conn.execute(
                "UPDATE goals SET status='archived', updated_at=?1 WHERE id=?2",
                params![now, goal_id],
            )
            .map_err(|e| e.to_string())?;
            if !out.is_json() {
                println!("🗂  Goal complete — auto-archived. Run `imi goals --archived` to see it.");
            }
        }
    }

    emit_simple_ok(out, "✅ Task marked done and completion summary saved")?;
    Ok(())
}

fn build_task_context(conn: &Connection, db_path: &Path, task_id: &str) -> Result<PathBuf, String> {
    let task: (String, String, String, String, String, String, String, String) = conn
        .query_row(
            "SELECT id, title, COALESCE(description,''), COALESCE(acceptance_criteria,''), COALESCE(relevant_files,'[]'), COALESCE(tools,'[]'), COALESCE(workspace_path,''), COALESCE(goal_id,'')
             FROM tasks WHERE id=?1",
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)),
        )
        .map_err(|e| e.to_string())?;

    let relevant_files: Vec<String> = serde_json::from_str(&task.4).unwrap_or_default();
    let tools: Vec<String> = serde_json::from_str(&task.5).unwrap_or_default();
    let relevant_files_text = if relevant_files.is_empty() { "- (none)".to_string() } else { relevant_files.iter().map(|f| format!("- {f}")).collect::<Vec<_>>().join("\n") };
    let tools_text = if tools.is_empty() { "- (none)".to_string() } else { tools.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n") };

    let goal = if task.7.is_empty() { None } else {
        conn.query_row(
            "SELECT COALESCE(name,''), COALESCE(description,''), COALESCE(why,'') FROM goals WHERE id=?1",
            params![task.7.clone()],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
        ).optional().map_err(|e| e.to_string())?
    };
    let (goal_name, goal_description, goal_why) = goal.unwrap_or_else(|| ("".to_string(), "".to_string(), "".to_string()));

    let prior_work_rows: Vec<(String, String, String, i64)> = if task.7.is_empty() { Vec::new() } else {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(m.task_id,''), COALESCE(t.title,''), COALESCE(m.value,''), COALESCE(m.created_at,0)
             FROM memories m JOIN tasks t ON t.id = m.task_id
             WHERE t.goal_id=?1 AND m.key='completion_summary' AND COALESCE(m.task_id,'') != ?2
             ORDER BY COALESCE(m.created_at,0) DESC LIMIT 3",
        ).map_err(|e| e.to_string())?;
        let rows: Vec<(String, String, String, i64)> = stmt.query_map(params![task.7.clone(), task.0.clone()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        rows
    };
    let prior_work_text = if prior_work_rows.is_empty() { "- (none)".to_string() } else {
        prior_work_rows.iter().map(|(tid, title, summary, _)| format!("- **{title}** ({tid}): {summary}")).collect::<Vec<_>>().join("\n")
    };

    let goal_decisions_rows: Vec<(String, String, String)> = if goal_name.is_empty() { Vec::new() } else {
        let mut stmt = conn.prepare(
            "SELECT COALESCE(what,''), COALESCE(why,''), COALESCE(affects,'') FROM decisions WHERE COALESCE(affects,'') LIKE ?1 ORDER BY COALESCE(created_at,0) DESC LIMIT 3",
        ).map_err(|e| e.to_string())?;
        let pat = format!("%{}%", goal_name);
        let rows: Vec<(String, String, String)> = stmt.query_map(params![pat], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        rows
    };
    let goal_decisions_text = if goal_decisions_rows.is_empty() { "- (none)".to_string() } else {
        goal_decisions_rows.iter().map(|(what, why, affects)| format!("- **{what}** — {why} (affects: {affects})")).collect::<Vec<_>>().join("\n")
    };

    let imi_dir = db_path.parent().ok_or_else(|| "invalid db path".to_string())?;
    let run_dir = imi_dir.join("runs").join(&task.0);
    fs::create_dir_all(&run_dir).map_err(|e| format!("failed to create run dir: {e}"))?;

    let context_md = format!(
        "# Task: {title}\n\n## Description\n{description}\n\n## Acceptance Criteria\n{acceptance}\n\n## Relevant Files\n{relevant}\n\n## Tools\n{tools}\n\n## Goal description\n{goal_description}\n\n## Goal why\n{goal_why}\n\n## Prior work on this goal\n{prior_work}\n\n## Decisions affecting this goal\n{goal_decisions}\n\n## Workspace Path\n{workspace}\n",
        title = task.1,
        description = if task.2.is_empty() { "(none)" } else { &task.2 },
        acceptance = if task.3.is_empty() { "(none)" } else { &task.3 },
        relevant = relevant_files_text,
        tools = tools_text,
        goal_description = if goal_description.is_empty() { "(none)" } else { &goal_description },
        goal_why = if goal_why.is_empty() { "(none)" } else { &goal_why },
        prior_work = prior_work_text,
        goal_decisions = goal_decisions_text,
        workspace = if task.6.is_empty() { "(none)" } else { &task.6 },
    );
    fs::write(run_dir.join("context.md"), context_md).map_err(|e| format!("failed to write context.md: {e}"))?;
    Ok(run_dir)
}

fn cmd_run(
    conn: &Connection,
    db_path: &Path,
    out: OutputCtx,
    task_id: String,
    model: Option<String>,
) -> Result<(), String> {
    let id = resolve_id_prefix(conn, "tasks", &task_id)?
        .ok_or_else(|| format!("No task with ID '{task_id}' — run `imi tasks` to list available tasks"))?;
    let agent_id = current_agent(None);
    let claimed = ensure_task_in_progress(conn, &id, &agent_id)?;

    let run_dir = build_task_context(conn, db_path, &id)?;

    let selected_model = model.unwrap_or_else(|| "claude-sonnet-4-5".to_string());
    let hank_json = json!({
        "globalSystemPromptFile": "../../prompts/execute-mode.md",
        "context": fs::read_to_string(run_dir.join("context.md")).map_err(|e| format!("failed to read context.md: {e}"))?,
        "codons": [
            {
                "model": selected_model,
                "promptFile": "context.md",
                "continuationMode": "fresh"
            }
        ]
    });
    fs::write(
        run_dir.join("hank.json"),
        serde_json::to_string_pretty(&hank_json).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("failed to write hank.json: {e}"))?;

    let watchdog = spawn_lifecycle_watchdog(
        db_path.to_path_buf(),
        claimed.id.clone(),
        claimed.goal_id.clone(),
        agent_id.clone(),
        300,
        900,
    );
    let status = Command::new("bunx")
        .arg("hankweave")
        .current_dir(&run_dir)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status();
    if let Some(watchdog) = watchdog {
        watchdog.stop();
    }
    let status = match status {
        Ok(status) => status,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let reason = "hankweave not found. Install with: npm install -g hankweave".to_string();
            if task_status(conn, &claimed.id).unwrap_or_default() == "in_progress" {
                let _ = cmd_fail(conn, out, Some(agent_id.clone()), claimed.id.clone(), reason.clone());
            }
            return Err(reason);
        }
        Err(e) => {
            let reason = format!("failed to run hankweave: {e}");
            if task_status(conn, &claimed.id).unwrap_or_default() == "in_progress" {
                let _ = cmd_fail(conn, out, Some(agent_id.clone()), claimed.id.clone(), reason.clone());
            }
            return Err(reason);
        }
    };
    if !status.success() {
        let reason = format!("hankweave exited with status: {status}");
        if task_status(conn, &claimed.id).unwrap_or_default() == "in_progress" {
            let _ = cmd_fail(conn, out, Some(agent_id.clone()), claimed.id.clone(), reason.clone());
        }
        return Err(reason);
    }

    if task_status(conn, &claimed.id).unwrap_or_default() == "done" {
        emit_simple_ok(out, "Task already completed")?;
        return Ok(());
    }

    let summary = fs::read_to_string(run_dir.join("summary.md"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Completed via imi run (no summary.md)".to_string());
    cmd_complete(conn, out, Some(agent_id), claimed.id, summary, None, None, None)
}

fn cmd_wrap(
    conn: &Connection,
    db_path: &Path,
    out: OutputCtx,
    agent: Option<String>,
    task_id: String,
    ping_secs: u64,
    checkpoint_secs: u64,
    command: Vec<String>,
) -> Result<(), String> {
    let agent_id = current_agent(agent.as_deref());
    let task = ensure_task_in_progress(conn, &task_id, &agent_id)?;
    let workspace_path: String = conn
        .query_row(
            "SELECT COALESCE(workspace_path,'') FROM tasks WHERE id=?1",
            params![task.id.clone()],
            |r| r.get(0),
        )
        .unwrap_or_default();

    // Build context.md so the wrapped command (or hankweave) can read task details
    let run_dir = build_task_context(conn, db_path, &task.id).ok();
    let context_file = run_dir.as_ref().map(|d| d.join("context.md"));

    // No command given — use hankweave (same execution path as imi run)
    if command.is_empty() {
        let run_dir = run_dir.ok_or_else(|| "failed to build task context".to_string())?;
        let hank_json = json!({
            "globalSystemPromptFile": "../../prompts/execute-mode.md",
            "context": fs::read_to_string(run_dir.join("context.md")).map_err(|e| format!("failed to read context.md: {e}"))?,
            "codons": [{ "model": "claude-sonnet-4-5", "promptFile": "context.md", "continuationMode": "fresh" }]
        });
        fs::write(
            run_dir.join("hank.json"),
            serde_json::to_string_pretty(&hank_json).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("failed to write hank.json: {e}"))?;
        let watchdog = spawn_lifecycle_watchdog(
            db_path.to_path_buf(), task.id.clone(), task.goal_id.clone(), agent_id.clone(), ping_secs, checkpoint_secs,
        );
        let status = Command::new("bunx")
            .arg("hankweave")
            .current_dir(&run_dir)
            .stdin(std::process::Stdio::inherit())
            .stdout(std::process::Stdio::inherit())
            .stderr(std::process::Stdio::inherit())
            .status();
        if let Some(watchdog) = watchdog { watchdog.stop(); }
        let status = match status {
            Ok(s) => s,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let reason = "hankweave not found. Install with: npm install -g hankweave".to_string();
                if task_status(conn, &task.id).unwrap_or_default() == "in_progress" {
                    let _ = cmd_fail(conn, out, Some(agent_id.clone()), task.id.clone(), reason.clone());
                }
                return Err(reason);
            }
            Err(e) => {
                let reason = format!("failed to run hankweave: {e}");
                if task_status(conn, &task.id).unwrap_or_default() == "in_progress" {
                    let _ = cmd_fail(conn, out, Some(agent_id.clone()), task.id.clone(), reason.clone());
                }
                return Err(reason);
            }
        };
        if !status.success() {
            let reason = format!("hankweave exited with status: {status}");
            if task_status(conn, &task.id).unwrap_or_default() == "in_progress" {
                let _ = cmd_fail(conn, out, Some(agent_id.clone()), task.id.clone(), reason.clone());
            }
            return Err(reason);
        }
        if task_status(conn, &task.id).unwrap_or_default() == "done" {
            emit_simple_ok(out, "Task already completed")?;
            return Ok(());
        }
        let summary = fs::read_to_string(run_dir.join("summary.md"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "Completed via imi wrap (no summary.md)".to_string());
        return cmd_complete(conn, out, Some(agent_id), task.id, summary, None, None, None);
    }

    // Custom command path
    let first = command.first().unwrap();
    let watchdog = spawn_lifecycle_watchdog(
        db_path.to_path_buf(),
        task.id.clone(),
        task.goal_id.clone(),
        agent_id.clone(),
        ping_secs,
        checkpoint_secs,
    );

    let mut child = Command::new(first);
    child
        .args(command.iter().skip(1))
        .env("IMI_TASK_ID", &task.id)
        .env("IMI_TASK_TITLE", &task.title)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    if let Some(ref cf) = context_file {
        child.env("IMI_TASK_CONTEXT_FILE", cf.display().to_string());
    }

    if !workspace_path.trim().is_empty() {
        let workspace = PathBuf::from(workspace_path.trim());
        if workspace.exists() {
            child.current_dir(workspace);
        }
    }

    let status = child.status();
    if let Some(watchdog) = watchdog {
        watchdog.stop();
    }

    let status = match status {
        Ok(status) => status,
        Err(e) => {
            let reason = format!("wrapped command failed to start: {e}");
            if task_status(conn, &task.id).unwrap_or_default() == "in_progress" {
                let _ = cmd_fail(conn, out, Some(agent_id.clone()), task.id.clone(), reason.clone());
            }
            return Err(reason);
        }
    };

    let current_status = task_status(conn, &task.id).unwrap_or_default();
    if status.success() {
        if current_status == "done" {
            emit_simple_ok(out, "Wrapped command succeeded (task already completed)")?;
            return Ok(());
        }
        let summary = format!("Wrapped command succeeded: {}", command.join(" "));
        return cmd_complete(conn, out, Some(agent_id), task.id, summary, None, None, None);
    }

    let reason = format!("wrapped command exited with status {status}: {}", command.join(" "));
    if current_status == "in_progress" {
        let _ = cmd_fail(conn, out, Some(agent_id), task.id, reason.clone());
    }
    Err(reason)
}

struct OrchestrateWorker {
    task_id: String,
    agent_id: String,
    child: Child,
}

/// Resolve which CLI command workers should use when no explicit command is given.
/// - None / "hankweave" → use `imi run` (hankweave, default)
/// - "auto"             → detect from env vars: Claude Code → claude, OpenCode → opencode, Copilot → copilot, else hankweave
/// - "claude"           → `sh -c 'claude -p "$(cat "$IMI_TASK_CONTEXT_FILE")" --dangerously-skip-permissions'`
/// - "opencode"         → `opencode`
/// - "codex"            → `sh -c 'codex exec "$(cat "$IMI_TASK_CONTEXT_FILE")"'`
/// - "copilot"          → `sh -c 'gh agent-task create -F "$IMI_TASK_CONTEXT_FILE"'`
/// Returns None to signal "use imi run / hankweave", or Some(command_vec) to use imi wrap.
fn resolve_worker_cli(cli: Option<&str>) -> Option<Vec<String>> {
    let cli = match cli {
        None | Some("hankweave") => return None,
        Some("auto") => {
            // Detect from environment
            if env::var("CLAUDE_CODE_SSE_PORT").is_ok() || env::var("CLAUDE_CODE_ENTRYPOINT").is_ok() {
                "claude"
            } else if env::var("OPENCODE_SESSION").is_ok() {
                "opencode"
            } else if env::var("GH_COPILOT_SESSION_ID").is_ok() || env::var("COPILOT_AGENT_SESSION").is_ok() {
                "copilot"
            } else {
                return None; // fall back to hankweave
            }
        }
        Some(v) => v,
    };
    match cli {
        "claude" => Some(vec![
            "sh".into(), "-c".into(),
            r#"claude -p "$(cat "$IMI_TASK_CONTEXT_FILE")" --dangerously-skip-permissions"#.into(),
        ]),
        "opencode" => Some(vec!["opencode".into()]),
        "codex" => Some(vec![
            "sh".into(), "-c".into(),
            r#"codex exec "$(cat "$IMI_TASK_CONTEXT_FILE")""#.into(),
        ]),
        "copilot" => Some(vec![
            "sh".into(), "-c".into(),
            r#"gh agent-task create -F "$IMI_TASK_CONTEXT_FILE""#.into(),
        ]),
        _ => None,
    }
}

fn spawn_orchestrate_worker(
    db_path: &Path,
    task_id: &str,
    agent_id: &str,
    ping_secs: u64,
    checkpoint_secs: u64,
    command: &[String],
) -> Result<Child, String> {
    let exe = env::current_exe().map_err(|e| format!("failed to locate current executable: {e}"))?;
    let mut child_cmd = Command::new(exe);
    child_cmd
        .env("IMI_DB", db_path.display().to_string())
        .env("IMI_AGENT_ID", agent_id)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());

    if command.is_empty() {
        child_cmd.args(["run", task_id]);
    } else {
        child_cmd
            .args([
                "wrap",
                task_id,
                "--agent",
                agent_id,
                "--ping-secs",
                &ping_secs.to_string(),
                "--checkpoint-secs",
                &checkpoint_secs.to_string(),
                "--",
            ])
            .args(command);
    }

    child_cmd
        .spawn()
        .map_err(|e| format!("failed to spawn worker process: {e}"))
}

fn cmd_orchestrate(
    conn: &mut Connection,
    db_path: &Path,
    out: OutputCtx,
    goal_id: Option<String>,
    workers: usize,
    agent_prefix: Option<String>,
    ping_secs: u64,
    checkpoint_secs: u64,
    max_tasks: Option<usize>,
    cli: Option<String>,
    command: Vec<String>,
) -> Result<(), String> {
    if workers == 0 {
        return Err("workers must be >= 1".to_string());
    }

    // Resolve which CLI to use for workers (only applies when no explicit command given)
    let resolved_command: Vec<String> = if command.is_empty() {
        resolve_worker_cli(cli.as_deref()).unwrap_or_default()
    } else {
        command
    };

    let goal = if let Some(goal_id) = goal_id {
        Some(
            resolve_id_prefix(conn, "goals", &goal_id)?
                .ok_or_else(|| format!("goal not found: {goal_id}"))?,
        )
    } else {
        None
    };

    let prefix = agent_prefix.unwrap_or_else(|| "imi-worker".to_string());
    let limit = max_tasks.unwrap_or(usize::MAX);
    let mut active: Vec<OrchestrateWorker> = Vec::new();
    let mut launched = 0usize;
    let mut done = 0usize;
    let mut failed = 0usize;
    let mut no_more_tasks = false;
    let mut race_guard = 0usize;

    loop {
        while !no_more_tasks && active.len() < workers && launched < limit {
            let worker_agent = format!("{prefix}-{}", launched + 1);
            let claim = claim_next_task(conn, goal.as_deref(), &worker_agent)?;
            match claim {
                ClaimResult::NoTasks => {
                    no_more_tasks = true;
                    break;
                }
                ClaimResult::RaceLost => {
                    race_guard += 1;
                    if race_guard > workers * 8 {
                        no_more_tasks = true;
                        break;
                    }
                    continue;
                }
                ClaimResult::Claimed(task) => {
                    race_guard = 0;
                    launched += 1;
                    match spawn_orchestrate_worker(
                        db_path,
                        &task.id,
                        &worker_agent,
                        ping_secs,
                        checkpoint_secs,
                        &resolved_command,
                    ) {
                        Ok(child) => active.push(OrchestrateWorker {
                            task_id: task.id,
                            agent_id: worker_agent,
                            child,
                        }),
                        Err(e) => {
                            failed += 1;
                            let _ = cmd_fail(conn, out, Some(worker_agent), task.id, e.clone());
                        }
                    }
                }
            }
        }

        let mut idx = 0usize;
        while idx < active.len() {
            let status = active[idx].child.try_wait().map_err(|e| e.to_string())?;
            match status {
                None => idx += 1,
                Some(status) => {
                    if status.success() {
                        done += 1;
                    } else {
                        failed += 1;
                        let reason = format!("worker {} exited with status {status}", active[idx].agent_id);
                        let _ = cmd_fail(
                            conn,
                            out,
                            Some(active[idx].agent_id.clone()),
                            active[idx].task_id.clone(),
                            reason,
                        );
                    }
                    active.swap_remove(idx);
                }
            }
        }

        if (no_more_tasks || launched >= limit) && active.is_empty() {
            break;
        }

        if active.is_empty() && no_more_tasks {
            break;
        }

        thread::sleep(Duration::from_millis(400));
    }

    if out.is_json() {
        println!(
            "{}",
            json!({
                "ok": failed == 0,
                "goal_id": goal,
                "workers": workers,
                "launched": launched,
                "completed": done,
                "failed": failed
            })
        );
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "orchestrate",
            &["workers", "launched", "completed", "failed"],
            vec![vec![
                workers.to_string(),
                launched.to_string(),
                done.to_string(),
                failed.to_string(),
            ]],
        );
        print!("{}", t.finish());
    } else {
        println!(
            "Orchestrate finished: launched={} completed={} failed={}",
            launched, done, failed
        );
    }

    if failed > 0 {
        return Err(format!("orchestrate finished with {failed} failed worker(s)"));
    }

    Ok(())
}

fn cmd_fail(
    conn: &Connection,
    out: OutputCtx,
    agent: Option<String>,
    task_id: String,
    reason: String,
) -> Result<(), String> {
    if reason.trim().is_empty() {
        return Err("reason is required".to_string());
    }

    let task = resolve_task(conn, &task_id)?;
    let agent_id = current_agent(agent.as_deref());
    let now = now_ts();

    conn.execute(
        "UPDATE tasks SET status='todo', agent_id=NULL, updated_at=?1 WHERE id=?2",
        params![now, task.id],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "INSERT INTO memories (id, goal_id, task_id, key, value, type, reasoning, source, created_at)
         VALUES (?1, ?2, ?3, 'failure_reason', ?4, 'failure', ?4, ?5, ?6)",
        params![gen_id(), task.goal_id, task.id, reason, agent_id, now],
    )
    .map_err(|e| e.to_string())?;

    if let Some(goal_id) = task.goal_id {
        sync_goal(conn, &goal_id)?;
    }

    if out.is_json() {
        println!(
            "{}",
            json!({"ok": true, "status": "todo", "id": task.id, "title": task.title})
        );
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "task",
            &["id", "title", "status"],
            vec![vec![task.id, task.title, "todo".to_string()]],
        );
        print!("{}", t.finish());
    } else {
        println!("🚫 Task {} marked blocked for this attempt and moved back to 📋 todo", task.id);
    }

    Ok(())
}

fn cmd_ping(conn: &Connection, out: OutputCtx, task_id: String) -> Result<(), String> {
    let id = resolve_id_prefix(conn, "tasks", &task_id)?
        .ok_or_else(|| format!("No task with ID '{task_id}' — run `imi tasks` to list available tasks"))?;
    let now = now_ts();
    let n = conn
        .execute(
            "UPDATE tasks SET updated_at=?1, last_ping_at=?1 WHERE id=?2 AND status='in_progress'",
            params![now, id],
        )
        .map_err(|e| e.to_string())?;
    if n == 0 {
        return Err("task is not in progress".to_string());
    }

    emit_simple_ok(out, "pong")?;
    Ok(())
}

fn cmd_checkpoint(conn: &Connection, out: OutputCtx, task_id: String, note: String) -> Result<(), String> {
    if note.trim().is_empty() {
        return Err("progress note is required".to_string());
    }

    let task = resolve_task(conn, &task_id)?;
    if task.status != "in_progress" {
        return Err("task is not in progress".to_string());
    }

    let now = now_ts();
    let agent_id = current_agent(None);
    conn.execute(
        "INSERT INTO memories (id, goal_id, task_id, key, value, type, reasoning, source, created_at)
         VALUES (?1, ?2, ?3, 'checkpoint', ?4, 'checkpoint', ?4, ?5, ?6)",
        params![gen_id(), task.goal_id, task.id, note, agent_id, now],
    )
    .map_err(|e| e.to_string())?;

    conn.execute(
        "UPDATE tasks SET updated_at=?1, last_ping_at=?1 WHERE id=?2 AND status='in_progress'",
        params![now, task.id],
    )
    .map_err(|e| e.to_string())?;

    if out.is_json() {
        println!(
            "{}",
            json!({"ok": true, "task_id": task.id, "message": format!("Checkpoint saved — {}", note)})
        );
    } else {
        println!("Checkpoint saved — {}", note);
    }
    Ok(())
}

fn cmd_add_goal(
    conn: &Connection,
    out: OutputCtx,
    name: String,
    desc: Option<String>,
    priority: Option<String>,
    why: Option<String>,
    for_who: Option<String>,
    success_signal: Option<String>,
    relevant_files: Vec<String>,
    context: Option<String>,
    workspace: Option<String>,
) -> Result<(), String> {
    let id = gen_id();
    let now = now_ts();
    let cwd = workspace.unwrap_or_else(|| {
        env::current_dir()
            .ok()
            .map(|x| x.display().to_string())
            .unwrap_or_default()
    });
    let rf_json = if relevant_files.is_empty() {
        "[]".to_string()
    } else {
        serde_json::to_string(&relevant_files).unwrap_or_else(|_| "[]".to_string())
    };

    conn.execute(
        "INSERT INTO goals (id, name, description, why, for_who, success_signal, status, priority, context, tags, workspace_path, relevant_files, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'todo', ?7, ?8, '[]', ?9, ?10, ?11, ?11)",
        params![
            id,
            name,
            desc.unwrap_or_else(|| "".to_string()),
            why.unwrap_or_default(),
            for_who.unwrap_or_default(),
            success_signal.unwrap_or_default(),
            priority.unwrap_or_else(|| "medium".to_string()),
            context.unwrap_or_default(),
            cwd,
            rf_json,
            now
        ],
    )
    .map_err(|e| e.to_string())?;

    if out.is_json() {
        println!("{}", json!({"ok": true, "id": id}));
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section("goal", &["id", "name"], vec![vec![id, name]]);
        print!("{}", t.finish());
    } else {
        println!("Added goal: {}", id);
    }
    Ok(())
}

fn cmd_add_task(
    conn: &Connection,
    out: OutputCtx,
    goal_prefix: String,
    title: String,
    desc: Option<String>,
    priority: Option<String>,
    why: Option<String>,
    context: Option<String>,
    relevant_files: Vec<String>,
    tools: Vec<String>,
    acceptance_criteria: Option<String>,
    workspace: Option<String>,
) -> Result<(), String> {
    let goal_id = resolve_id_prefix(conn, "goals", &goal_prefix)?
        .ok_or_else(|| format!("goal not found: {goal_prefix}"))?;
    let id = gen_id();
    let now = now_ts();
    let cwd = workspace.unwrap_or_else(|| {
        env::current_dir()
            .ok()
            .map(|x| x.display().to_string())
            .unwrap_or_default()
    });
    let rf_json = if relevant_files.is_empty() {
        "[]".to_string()
    } else {
        serde_json::to_string(&relevant_files).unwrap_or_else(|_| "[]".to_string())
    };
    let tools_json = if tools.is_empty() {
        "[]".to_string()
    } else {
        serde_json::to_string(&tools).unwrap_or_else(|_| "[]".to_string())
    };

    conn.execute(
        "INSERT INTO tasks (id, title, description, why, context, linked_files, tags, time_frame, priority, status, goal_id, execution_format, workspace_path, relevant_files, tools, acceptance_criteria, created_at, updated_at, created_by)
         VALUES (?1, ?2, ?3, ?4, ?5, '[]', '[]', 'this_week', ?6, 'todo', ?7, 'json', ?8, ?9, ?10, ?11, ?12, ?12, 'user')",
        params![
            id,
            title,
            desc.unwrap_or_default(),
            why.unwrap_or_default(),
            context.unwrap_or_default(),
            priority.unwrap_or_else(|| "medium".to_string()),
            goal_id,
            cwd,
            rf_json,
            tools_json,
            acceptance_criteria,
            now
        ],
    )
    .map_err(|e| e.to_string())?;

    sync_goal(conn, &goal_id)?;

    if out.is_json() {
        println!("{}", json!({"ok": true, "id": id, "goal_id": goal_id}));
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section("task", &["id", "goal_id", "title"], vec![vec![id, goal_id, title]]);
        print!("{}", t.finish());
    } else {
        println!("Added task: {}", id);
    }

    Ok(())
}

fn cmd_memory(conn: &Connection, out: OutputCtx, action: Option<MemoryAction>) -> Result<(), String> {
    match action {
        Some(MemoryAction::Add {
            goal_id,
            key,
            value,
        }) => {
            let gid = resolve_id_prefix(conn, "goals", &goal_id)?
                .ok_or_else(|| format!("goal not found: {goal_id}"))?;
            let now = now_ts();
            conn.execute(
                "INSERT INTO memories (id, goal_id, key, value, type, source, created_at) VALUES (?1, ?2, ?3, ?4, 'learning', 'agent', ?5)",
                params![gen_id(), gid, key, value, now],
            )
            .map_err(|e| e.to_string())?;
            emit_simple_ok(out, "Memory added")
        }
        _ => {
            let memories = query_memories(conn, None, 50)?;
            if out.is_json() {
                println!(
                    "{}",
                    json!(memories
                        .iter()
                        .map(memory_to_value)
                        .collect::<Vec<_>>())
                );
            } else if out.is_toon() {
                let mut t = ToonBuilder::new();
                t.section(
                    "memories",
                    &["id", "goal_id", "task_id", "key", "value", "type"],
                    memories
                        .iter()
                        .map(|m| {
                            vec![
                                m.id.clone(),
                                m.goal_id.clone().unwrap_or_default(),
                                m.task_id.clone().unwrap_or_default(),
                                m.key.clone(),
                                m.value.clone(),
                                m.typ.clone(),
                            ]
                        })
                        .collect(),
                );
                print!("{}", t.finish());
            } else if memories.is_empty() {
                println!("No memories.");
            } else {
                for m in memories {
                    println!("[{}] {} = {}", m.typ, m.key, m.value);
                }
            }
            Ok(())
        }
    }
}

fn cmd_lesson(
    conn: &Connection,
    out: OutputCtx,
    mut args: Vec<String>,
    correct_behavior: Option<String>,
    verified_by: Option<String>,
) -> Result<(), String> {
    if args.first().map(|s| s.eq_ignore_ascii_case("add")).unwrap_or(false) {
        let _ = args.remove(0);
    }
    let what_went_wrong = args.join(" ").trim().to_string();
    if what_went_wrong.is_empty() {
        return Err("lesson text is required".to_string());
    }

    let correct_behavior = match correct_behavior {
        Some(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => {
            let v = ops_read_line("Correct behavior: ")?;
            if v.trim().is_empty() {
                return Err("correct behavior is required".to_string());
            }
            v
        }
    };
    let verified_by = verified_by
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "human".to_string());

    conn.execute(
        "INSERT INTO lessons (id, what_went_wrong, correct_behavior, verified_by, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![gen_id(), what_went_wrong, correct_behavior, verified_by, now_ts()],
    )
    .map_err(|e| e.to_string())?;

    emit_simple_ok(out, "Lesson added")
}

fn cmd_lessons(conn: &Connection, out: OutputCtx) -> Result<(), String> {
    let lessons = query_lessons(conn, 200)?;
    if out.is_json() {
        println!(
            "{}",
            json!(lessons
                .iter()
                .map(lesson_to_value)
                .collect::<Vec<_>>())
        );
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "verified_lessons",
            &["id", "what_went_wrong", "correct_behavior", "verified_by", "created_at"],
            lessons
                .iter()
                .map(|l| {
                    vec![
                        l.id.clone(),
                        l.what_went_wrong.clone(),
                        l.correct_behavior.clone(),
                        l.verified_by.clone(),
                        l.created_at.to_string(),
                    ]
                })
                .collect(),
        );
        print!("{}", t.finish());
    } else if lessons.is_empty() {
        println!("No lessons.");
    } else {
        for l in lessons {
            println!(
                "- {}\n  correct behavior: {}\n  verified by: {} ({} ago)\n",
                l.what_went_wrong,
                l.correct_behavior,
                l.verified_by,
                ago(l.created_at)
            );
        }
    }
    Ok(())
}

fn cmd_decide(
    conn: &Connection,
    out: OutputCtx,
    what: String,
    why: String,
    affects: Option<String>,
) -> Result<(), String> {
    let now = now_ts();
    let id = gen_id();
    conn.execute(
        "INSERT INTO decisions (id, what, why, affects, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![id, what, why, affects.unwrap_or_default(), now],
    )
    .map_err(|e| e.to_string())?;

    emit_simple_ok(out, "Decision recorded")
}

fn cmd_log(conn: &Connection, out: OutputCtx, note: String) -> Result<(), String> {
    if note.trim().is_empty() {
        return Err("note is required".to_string());
    }
    let now = now_ts();
    let id = gen_id();
    let author = current_agent(None);
    conn.execute(
        "INSERT INTO direction_notes (id, content, author, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, note, author, now],
    )
    .map_err(|e| e.to_string())?;
    emit_simple_ok(out, "Direction note added")
}

fn ops_read_line(prompt: &str) -> Result<String, String> {
    print!("{}", prompt);
    io::stdout().flush().map_err(|e| e.to_string())?;
    let mut line = String::new();
    io::stdin().read_line(&mut line).map_err(|e| e.to_string())?;
    Ok(line.trim().to_string())
}

fn cmd_ops(conn: &Connection, out: OutputCtx, args: Vec<String>) -> Result<(), String> {
    if args.is_empty() {
        return cmd_context(conn, out, None);
    }
    conn.execute(
        "INSERT INTO direction_notes (id, content, author, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![gen_id(), args.join(" "), current_agent(None), now_ts()],
    )
    .map_err(|e| e.to_string())?;
    println!("Direction noted.");
    Ok(())
}

fn cmd_delete(conn: &Connection, out: OutputCtx, id: String) -> Result<(), String> {
    if let Some(goal_id) = resolve_id_prefix(conn, "goals", &id)? {
        conn.execute(
            "DELETE FROM memories WHERE goal_id=?1 OR task_id IN (SELECT id FROM tasks WHERE goal_id=?1)",
            params![goal_id],
        )
        .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM tasks WHERE goal_id=?1", params![goal_id])
            .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM goals WHERE id=?1", params![goal_id])
            .map_err(|e| e.to_string())?;
        emit_simple_ok(out, "Goal deleted")?;
        return Ok(());
    }

    if let Some(task_id) = resolve_id_prefix(conn, "tasks", &id)? {
        let goal_id: Option<String> = conn
            .query_row("SELECT goal_id FROM tasks WHERE id=?1", params![task_id.clone()], |r| {
                r.get(0)
            })
            .optional()
            .map_err(|e| e.to_string())?
            .flatten();
        conn.execute("DELETE FROM memories WHERE task_id=?1", params![task_id.clone()])
            .map_err(|e| e.to_string())?;
        conn.execute("DELETE FROM tasks WHERE id=?1", params![task_id])
            .map_err(|e| e.to_string())?;
        if let Some(gid) = goal_id {
            let _ = sync_goal(conn, &gid);
        }
        emit_simple_ok(out, "Task deleted")?;
        return Ok(());
    }

    Err("id not found in goals or tasks".to_string())
}

fn cmd_reset(conn: &Connection, out: OutputCtx, force: bool) -> Result<(), String> {
    if !force {
        if !io::stdin().is_terminal() {
            return Err("reset requires --force in non-interactive mode".to_string());
        }
        print!("This will delete goals/tasks/memories. Type 'yes' to continue: ");
        let _ = io::stdout().flush();
        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .map_err(|e| e.to_string())?;
        if line.trim() != "yes" {
            emit_simple_ok(out, "Reset cancelled")?;
            return Ok(());
        }
    }

    conn.execute("DELETE FROM memories", []).map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM tasks", []).map_err(|e| e.to_string())?;
    conn.execute("DELETE FROM goals", []).map_err(|e| e.to_string())?;

    emit_simple_ok(out, "Reset complete")
}

fn cmd_stats(conn: &Connection, out: OutputCtx) -> Result<(), String> {
    let now = now_ts();
    let week_ago = now - 7 * 24 * 3600;

    let (total, done): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(CASE WHEN status='done' THEN 1 ELSE 0 END),0) FROM tasks",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((0, 0));
    let completion_rate = if total > 0 {
        (done as f64 / total as f64) * 100.0
    } else {
        0.0
    };

    let avg_cycle_seconds: Option<f64> = conn
        .query_row(
            "SELECT AVG(completed_at - created_at) FROM tasks WHERE status='done' AND completed_at IS NOT NULL AND created_at IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?
        .flatten();

    let activity_7d: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM events WHERE COALESCE(created_at,0) >= ?1",
            params![week_ago],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let mut top_stmt = conn
        .prepare(
            "SELECT command, COUNT(*) as c FROM events GROUP BY command ORDER BY c DESC LIMIT 5",
        )
        .map_err(|e| e.to_string())?;
    let top_commands: Vec<(String, i64)> = top_stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;

    let wip_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE status='in_progress'", [], |r| r.get(0))
        .unwrap_or(0);
    let stale_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM tasks WHERE status='in_progress' AND COALESCE(last_ping_at,updated_at,created_at,0) < ?1",
            params![now - 1800],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if out.is_json() {
        println!(
            "{}",
            json!({
                "completion_rate": completion_rate,
                "avg_cycle_seconds": avg_cycle_seconds,
                "activity_7d": activity_7d,
                "top_commands": top_commands,
                "health": {
                    "wip": wip_count,
                    "stale_locks": stale_count
                }
            })
        );
        return Ok(());
    }

    if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "metrics",
            &["completion_rate", "avg_cycle_seconds", "activity_7d"],
            vec![vec![
                format!("{completion_rate:.2}"),
                avg_cycle_seconds
                    .map(|x| format!("{x:.2}"))
                    .unwrap_or_default(),
                activity_7d.to_string(),
            ]],
        );
        t.section(
            "top_commands",
            &["command", "count"],
            top_commands
                .iter()
                .map(|x| vec![x.0.clone(), x.1.to_string()])
                .collect(),
        );
        t.section(
            "health",
            &["wip", "stale_locks"],
            vec![vec![wip_count.to_string(), stale_count.to_string()]],
        );
        print!("{}", t.finish());
        return Ok(());
    }

    println!("IMI Stats");
    println!("  completion rate: {:.1}% ({}/{})", completion_rate, done, total);
    if let Some(avg) = avg_cycle_seconds {
        println!("  avg cycle time: {:.1}h", avg / 3600.0);
    } else {
        println!("  avg cycle time: n/a");
    }
    println!("  activity (7d): {} event(s)", activity_7d);

    println!("\nTop commands");
    if top_commands.is_empty() {
        println!("  (none)");
    } else {
        for (cmd, c) in top_commands {
            println!("  {}  {}", c, cmd);
        }
    }

    println!("\nHealth signals");
    println!("  in progress: {}", wip_count);
    println!("  stale locks (>30m): {}", stale_count);

    Ok(())
}

fn cmd_instructions(out: OutputCtx, target: Option<String>) -> Result<(), String> {
    let tool = target.unwrap_or_else(|| "cursor".to_string()).to_lowercase();
    let snippet = match tool.as_str() {
        "cursor" => instructions_cursor(),
        "copilot" => instructions_copilot(),
        "windsurf" => instructions_windsurf(),
        _ => return Err("tool must be one of: cursor, copilot, windsurf".to_string()),
    };

    if out.is_json() {
        println!("{}", json!({"tool": tool, "instructions": snippet}));
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section(
            "instructions",
            &["tool", "text"],
            vec![vec![tool, snippet.to_string()]],
        );
        print!("{}", t.finish());
    } else {
        println!("{}", snippet);
    }
    Ok(())
}

fn emit_simple_ok(out: OutputCtx, message: &str) -> Result<(), String> {
    if out.is_json() {
        println!("{}", json!({"ok": true, "message": message}));
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section("ok", &["message"], vec![vec![message.to_string()]]);
        print!("{}", t.finish());
    } else {
        println!("{message}");
    }
    Ok(())
}

fn emit_error(out: OutputCtx, msg: &str) {
    if out.is_json() {
        println!("{}", json!({"ok": false, "error": msg}));
    } else if out.is_toon() {
        let mut t = ToonBuilder::new();
        t.section("error", &["message"], vec![vec![msg.to_string()]]);
        print!("{}", t.finish());
    } else {
        eprintln!("{}", paint(out, "31", &format!("Error: {msg}")));
    }
}

fn discover_db_path() -> Option<PathBuf> {
    if let Ok(path) = env::var("IMI_DB") {
        if !path.trim().is_empty() {
            return Some(PathBuf::from(path));
        }
    }

    if let Ok(mut dir) = env::current_dir() {
        loop {
            let candidate = dir.join(".imi").join("state.db");
            if candidate.exists() {
                return Some(candidate);
            }
            if !dir.pop() {
                break;
            }
        }
    }

    let home = env::var("HOME").ok()?;
    if cfg!(target_os = "macos") {
        Some(
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
                .join("Agents Dev")
                .join("data")
                .join("agents.db"),
        )
    } else {
        Some(PathBuf::from(home).join(".local").join("share").join("imi").join("state.db"))
    }
}

fn open_connection(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    let _ = conn.pragma_update(None, "foreign_keys", "ON");
    Ok(conn)
}

fn run_schema(conn: &Connection) -> Result<(), String> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS goals (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, description TEXT NOT NULL,
  why TEXT, for_who TEXT, success_signal TEXT, out_of_scope TEXT,
  workspace_id TEXT, status TEXT NOT NULL DEFAULT 'todo',
  priority TEXT NOT NULL DEFAULT 'medium', context TEXT, tags TEXT DEFAULT '[]',
  workspace_path TEXT, relevant_files TEXT DEFAULT '[]',
  created_at INTEGER, updated_at INTEGER, completed_at INTEGER
);
CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT NOT NULL,
  why TEXT, context TEXT, linked_files TEXT DEFAULT '[]',
  project_id TEXT, workspace_id TEXT, assignee_type TEXT NOT NULL DEFAULT 'ai',
  agent_id TEXT, team_id TEXT, tags TEXT DEFAULT '[]',
  time_frame TEXT NOT NULL DEFAULT 'this_week', due_date INTEGER,
  priority TEXT NOT NULL DEFAULT 'medium', status TEXT NOT NULL DEFAULT 'todo',
  chat_id TEXT, summary TEXT, goal_id TEXT REFERENCES goals(id) ON DELETE SET NULL,
  plan_id TEXT, execution_format TEXT DEFAULT 'json', execution_payload TEXT,
  workspace_path TEXT, relevant_files TEXT DEFAULT '[]', tools TEXT DEFAULT '[]',
  acceptance_criteria TEXT, created_at INTEGER, updated_at INTEGER,
  completed_at INTEGER, last_ping_at INTEGER, created_by TEXT NOT NULL DEFAULT 'user'
);
CREATE TABLE IF NOT EXISTS memories (
  id TEXT PRIMARY KEY, goal_id TEXT REFERENCES goals(id) ON DELETE SET NULL,
  task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
  key TEXT NOT NULL, value TEXT NOT NULL,
  type TEXT NOT NULL DEFAULT 'learning', reasoning TEXT,
  source TEXT NOT NULL DEFAULT 'agent', created_at INTEGER
);
CREATE TABLE IF NOT EXISTS lessons (
  id TEXT PRIMARY KEY,
  what_went_wrong TEXT NOT NULL,
  correct_behavior TEXT NOT NULL,
  verified_by TEXT NOT NULL DEFAULT 'human',
  created_at DATETIME
);
CREATE TABLE IF NOT EXISTS decisions (
  id TEXT PRIMARY KEY, what TEXT NOT NULL, why TEXT NOT NULL,
  affects TEXT, created_at INTEGER
);
CREATE TABLE IF NOT EXISTS direction_notes (
  id TEXT PRIMARY KEY, content TEXT NOT NULL, author TEXT, created_at INTEGER
);
CREATE TABLE IF NOT EXISTS workspaces (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, path TEXT NOT NULL,
  git_remote TEXT, created_at INTEGER, updated_at INTEGER
);
CREATE TABLE IF NOT EXISTS events (
  id TEXT PRIMARY KEY, command TEXT NOT NULL, task_id TEXT,
  goal_id TEXT, agent_id TEXT, duration_ms INTEGER DEFAULT 0, created_at INTEGER
);
CREATE TABLE IF NOT EXISTS settings (
  key TEXT PRIMARY KEY, value TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tasks_status_idx ON tasks(status);
CREATE INDEX IF NOT EXISTS tasks_goal_id_idx ON tasks(goal_id);
CREATE INDEX IF NOT EXISTS goals_status_idx ON goals(status);
CREATE INDEX IF NOT EXISTS memories_goal_id_idx ON memories(goal_id);
CREATE INDEX IF NOT EXISTS memories_created_at_idx ON memories(created_at);
CREATE INDEX IF NOT EXISTS lessons_created_at_idx ON lessons(created_at);
CREATE INDEX IF NOT EXISTS decisions_created_at_idx ON decisions(created_at);",
    )
    .map_err(|e| e.to_string())?;

    let has_last_ping: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM pragma_table_info('tasks') WHERE name='last_ping_at' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if has_last_ping.is_none() {
        conn.execute("ALTER TABLE tasks ADD COLUMN last_ping_at INTEGER", [])
            .map_err(|e| e.to_string())?;
    }

    Ok(())
}

fn register_workspace(conn: &Connection, cwd: &Path) -> Result<(), String> {
    let now = now_ts();
    let path = cwd.display().to_string();
    let name = cwd
        .file_name()
        .and_then(|x| x.to_str())
        .unwrap_or("workspace")
        .to_string();
    let git_remote = git_remote(cwd);

    let existing: Option<String> = conn
        .query_row(
            "SELECT id FROM workspaces WHERE path=?1 LIMIT 1",
            params![path.clone()],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    if let Some(id) = existing {
        conn.execute(
            "UPDATE workspaces SET name=?1, git_remote=?2, updated_at=?3 WHERE id=?4",
            params![name, git_remote, now, id],
        )
        .map_err(|e| e.to_string())?;
    } else {
        conn.execute(
            "INSERT INTO workspaces (id, name, path, git_remote, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
            params![gen_id(), name, path, git_remote, now],
        )
        .map_err(|e| e.to_string())?;
    }

    Ok(())
}

fn git_remote(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["config", "--get", "remote.origin.url"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

fn get_goals(conn: &Connection) -> Result<Vec<GoalRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, COALESCE(description,''), COALESCE(why,''), COALESCE(for_who,''), COALESCE(success_signal,''), COALESCE(status,'todo'), COALESCE(priority,'medium'), COALESCE(created_at,0)
             FROM goals
             ORDER BY COALESCE(created_at,0) DESC",
        )
        .map_err(|e| e.to_string())?;
    let mapped = stmt.query_map([], |r| {
        Ok(GoalRow {
            id: r.get(0)?,
            name: r.get(1)?,
            description: r.get(2)?,
            why_: r.get(3)?,
            for_who: r.get(4)?,
            success_signal: r.get(5)?,
            status: r.get(6)?,
            priority: r.get(7)?,
            created_at: r.get(8)?,
        })
    })
    .map_err(|e| e.to_string())?;
    let rows = mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn get_goal(conn: &Connection, id: &str) -> Result<Option<GoalRow>, String> {
    conn.query_row(
        "SELECT id, name, COALESCE(description,''), COALESCE(why,''), COALESCE(for_who,''), COALESCE(success_signal,''), COALESCE(status,'todo'), COALESCE(priority,'medium'), COALESCE(created_at,0)
         FROM goals WHERE id=?1",
        params![id],
        |r| {
            Ok(GoalRow {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                why_: r.get(3)?,
                for_who: r.get(4)?,
                success_signal: r.get(5)?,
                status: r.get(6)?,
                priority: r.get(7)?,
                created_at: r.get(8)?,
            })
        },
    )
    .optional()
    .map_err(|e| e.to_string())
}

fn get_tasks_for_goal(conn: &Connection, goal_id: &str) -> Result<Vec<TaskRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, title, COALESCE(description,''), COALESCE(why,''), goal_id, COALESCE(status,'todo'), COALESCE(priority,'medium'), agent_id, COALESCE(created_at,0)
             FROM tasks WHERE goal_id=?1
             ORDER BY COALESCE(updated_at, created_at, 0) DESC",
        )
        .map_err(|e| e.to_string())?;
    let mapped = stmt.query_map(params![goal_id], |r| {
        Ok(TaskRow {
            id: r.get(0)?,
            title: r.get(1)?,
            description: r.get(2)?,
            why_: r.get(3)?,
            goal_id: r.get(4)?,
            status: r.get(5)?,
            priority: r.get(6)?,
            agent_id: r.get(7)?,
            created_at: r.get(8)?,
        })
    })
    .map_err(|e| e.to_string())?;
    let rows = mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn query_direction(
    conn: &Connection,
    since: Option<i64>,
    limit: i64,
) -> Result<Vec<(String, String, i64)>, String> {
    let sql = if since.is_some() {
        "SELECT content, COALESCE(author,''), COALESCE(created_at,0) FROM direction_notes WHERE COALESCE(created_at,0) >= ?1 ORDER BY COALESCE(created_at,0) DESC LIMIT ?2"
    } else {
        "SELECT content, COALESCE(author,''), COALESCE(created_at,0) FROM direction_notes ORDER BY COALESCE(created_at,0) DESC LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    if let Some(s) = since {
        let mapped = stmt
            .query_map(params![s, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        let rows = mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    } else {
        let mapped = stmt
            .query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .map_err(|e| e.to_string())?;
        let rows = mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
}

fn query_decisions(conn: &Connection, limit: i64) -> Result<Vec<(String, String, String, i64)>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT what, why, COALESCE(affects,''), COALESCE(created_at,0) FROM decisions ORDER BY COALESCE(created_at,0) DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let mapped = stmt
        .query_map(params![limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(|e| e.to_string())?;
    let rows = mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn query_active_goals(conn: &Connection, limit: i64) -> Result<Vec<GoalRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, COALESCE(description,''), COALESCE(why,''), COALESCE(for_who,''), COALESCE(success_signal,''), COALESCE(status,'todo'), COALESCE(priority,'medium'), COALESCE(created_at,0)
             FROM goals
             WHERE status != 'done' AND status != 'archived'
             ORDER BY CASE priority
                WHEN 'critical' THEN 4
                WHEN 'high' THEN 3
                WHEN 'medium' THEN 2
                WHEN 'low' THEN 1
                ELSE 0 END DESC,
                COALESCE(updated_at, created_at, 0) DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let mapped = stmt.query_map(params![limit], |r| {
        Ok(GoalRow {
            id: r.get(0)?,
            name: r.get(1)?,
            description: r.get(2)?,
            why_: r.get(3)?,
            for_who: r.get(4)?,
            success_signal: r.get(5)?,
            status: r.get(6)?,
            priority: r.get(7)?,
            created_at: r.get(8)?,
        })
    })
    .map_err(|e| e.to_string())?;
    let rows = mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn query_wip_tasks(conn: &Connection, limit: i64) -> Result<Vec<TaskRowWithGoal>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT t.id, t.title, COALESCE(t.description,''), COALESCE(t.why,''), t.goal_id, COALESCE(t.status,'todo'), COALESCE(t.priority,'medium'), t.agent_id, COALESCE(t.created_at,0), COALESCE(g.name,'')
             FROM tasks t
             LEFT JOIN goals g ON t.goal_id=g.id
             WHERE t.status='in_progress'
               AND (t.goal_id IS NULL OR COALESCE(g.status,'') != 'archived')
             ORDER BY COALESCE(t.updated_at,t.created_at,0) DESC LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let mapped = stmt.query_map(params![limit], |r| {
        Ok(TaskRowWithGoal {
            id: r.get(0)?,
            title: r.get(1)?,
            description: r.get(2)?,
            why_: r.get(3)?,
            goal_id: r.get(4)?,
            status: r.get(5)?,
            priority: r.get(6)?,
            agent_id: r.get(7)?,
            created_at: r.get(8)?,
            goal_name: Some(r.get(9)?),
        })
    })
    .map_err(|e| e.to_string())?;
    let rows = mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

#[derive(Debug, Clone)]
struct TaskRowWithGoal {
    id: String,
    title: String,
    description: String,
    why_: String,
    goal_id: Option<String>,
    status: String,
    priority: String,
    agent_id: Option<String>,
    created_at: i64,
    goal_name: Option<String>,
}

fn query_memories(conn: &Connection, goal_id: Option<&str>, limit: i64) -> Result<Vec<MemoryRow>, String> {
    let sql = if goal_id.is_some() {
        "SELECT id, goal_id, task_id, key, value, COALESCE(type,'learning'), COALESCE(source,'agent'), COALESCE(created_at,0)
         FROM memories
         WHERE goal_id=?1 OR task_id IN (SELECT id FROM tasks WHERE goal_id=?1)
         ORDER BY COALESCE(created_at,0) DESC LIMIT ?2"
    } else {
        "SELECT id, goal_id, task_id, key, value, COALESCE(type,'learning'), COALESCE(source,'agent'), COALESCE(created_at,0)
         FROM memories
         ORDER BY COALESCE(created_at,0) DESC LIMIT ?1"
    };
    let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
    if let Some(gid) = goal_id {
        let mapped = stmt.query_map(params![gid, limit], |r| {
            Ok(MemoryRow {
                id: r.get(0)?,
                goal_id: r.get(1)?,
                task_id: r.get(2)?,
                key: r.get(3)?,
                value: r.get(4)?,
                typ: r.get(5)?,
                source: r.get(6)?,
                created_at: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
        let rows = mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    } else {
        let mapped = stmt.query_map(params![limit], |r| {
            Ok(MemoryRow {
                id: r.get(0)?,
                goal_id: r.get(1)?,
                task_id: r.get(2)?,
                key: r.get(3)?,
                value: r.get(4)?,
                typ: r.get(5)?,
                source: r.get(6)?,
                created_at: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
        let rows = mapped
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(rows)
    }
}

fn query_lessons(conn: &Connection, limit: i64) -> Result<Vec<LessonRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT id, COALESCE(what_went_wrong,''), COALESCE(correct_behavior,''), COALESCE(verified_by,'human'), COALESCE(created_at,0)
             FROM lessons
             ORDER BY COALESCE(created_at,0) DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let mapped = stmt
        .query_map(params![limit], |r| {
            Ok(LessonRow {
                id: r.get(0)?,
                what_went_wrong: r.get(1)?,
                correct_behavior: r.get(2)?,
                verified_by: r.get(3)?,
                created_at: r.get(4)?,
            })
        })
        .map_err(|e| e.to_string())?;
    mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

// Memories scoped to non-done goals, with WIP-goal memories surfaced first.
// At scale this keeps context relevant: finished-goal learnings don't pollute
// the agent's view of what's actively being worked on.
fn query_active_memories(conn: &Connection, limit: i64) -> Result<Vec<MemoryRow>, String> {
    let mut stmt = conn
        .prepare(
            "SELECT m.id, m.goal_id, m.task_id, m.key, m.value,
                    COALESCE(m.type,'learning'), COALESCE(m.source,'agent'), COALESCE(m.created_at,0)
             FROM memories m
             WHERE m.goal_id IS NULL
                OR m.goal_id IN (SELECT id FROM goals WHERE status != 'done' AND status != 'archived')
             ORDER BY
                CASE WHEN m.goal_id IN (
                    SELECT DISTINCT goal_id FROM tasks WHERE status='in_progress' AND goal_id IS NOT NULL
                ) THEN 1 ELSE 0 END DESC,
                COALESCE(m.created_at,0) DESC
             LIMIT ?1",
        )
        .map_err(|e| e.to_string())?;
    let mapped = stmt
        .query_map(params![limit], |r| {
            Ok(MemoryRow {
                id: r.get(0)?,
                goal_id: r.get(1)?,
                task_id: r.get(2)?,
                key: r.get(3)?,
                value: r.get(4)?,
                typ: r.get(5)?,
                source: r.get(6)?,
                created_at: r.get(7)?,
            })
        })
        .map_err(|e| e.to_string())?;
    let rows = mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
    Ok(rows)
}

fn resolve_task(conn: &Connection, prefix: &str) -> Result<TaskRow, String> {
    let id = resolve_id_prefix(conn, "tasks", prefix)?
        .ok_or_else(|| format!("No task with ID '{prefix}' — run `imi tasks` to list available tasks"))?;
    conn.query_row(
        "SELECT id, title, COALESCE(description,''), COALESCE(why,''), goal_id, COALESCE(status,'todo'), COALESCE(priority,'medium'), agent_id, COALESCE(created_at,0)
         FROM tasks WHERE id=?1",
        params![id],
        |r| {
            Ok(TaskRow {
                id: r.get(0)?,
                title: r.get(1)?,
                description: r.get(2)?,
                why_: r.get(3)?,
                goal_id: r.get(4)?,
                status: r.get(5)?,
                priority: r.get(6)?,
                agent_id: r.get(7)?,
                created_at: r.get(8)?,
            })
        },
    )
    .map_err(|e| e.to_string())
}

fn resolve_id_prefix(conn: &Connection, table: &str, prefix: &str) -> Result<Option<String>, String> {
    let sql = format!(
        "SELECT id FROM {table} WHERE id = ?1 OR id LIKE ?2 ORDER BY CASE WHEN id=?1 THEN 0 ELSE 1 END LIMIT 1"
    );
    let pattern = format!("{prefix}%");
    conn.query_row(&sql, params![prefix, pattern], |r| r.get(0))
        .optional()
        .map_err(|e| e.to_string())
}

fn release_stale_locks(conn: &Connection) -> Result<usize, String> {
    let now = now_ts();
    conn.execute(
        "UPDATE tasks
         SET status='todo', agent_id=NULL, updated_at=?1
         WHERE status='in_progress' AND COALESCE(last_ping_at, updated_at, created_at, 0) < ?2",
        params![now, now - 1800],
    )
    .map_err(|e| e.to_string())
}

fn claim_next_task(conn: &mut Connection, goal_id: Option<&str>, agent: &str) -> Result<ClaimResult, String> {
    let now = now_ts();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|e| e.to_string())?;

    let candidate: Option<TaskClaim> = if let Some(goal) = goal_id {
        tx.query_row(
            "SELECT id, title, COALESCE(description,''), COALESCE(why,''), COALESCE(context,''), goal_id,
                    COALESCE(relevant_files,'[]'), COALESCE(tools,'[]'), COALESCE(acceptance_criteria,''), COALESCE(workspace_path,'')
             FROM tasks
             WHERE status='todo'
               AND goal_id=?1
               AND goal_id IN (SELECT id FROM goals WHERE status!='archived')
             ORDER BY CASE priority
                WHEN 'critical' THEN 4
                WHEN 'high' THEN 3
                WHEN 'medium' THEN 2
                WHEN 'low' THEN 1
                ELSE 0 END DESC,
                COALESCE(updated_at,created_at,0) ASC
             LIMIT 1",
            params![goal],
            |r| {
                Ok(TaskClaim {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    description: r.get(2)?,
                    why_: r.get(3)?,
                    context: r.get(4)?,
                    goal_id: r.get(5)?,
                    relevant_files: r.get(6)?,
                    tools: r.get(7)?,
                    acceptance_criteria: r.get(8)?,
                    workspace_path: r.get(9)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
    } else {
        tx.query_row(
            "SELECT id, title, COALESCE(description,''), COALESCE(why,''), COALESCE(context,''), goal_id,
                    COALESCE(relevant_files,'[]'), COALESCE(tools,'[]'), COALESCE(acceptance_criteria,''), COALESCE(workspace_path,'')
             FROM tasks
             WHERE status='todo'
               AND (goal_id IS NULL OR goal_id IN (SELECT id FROM goals WHERE status!='archived'))
             ORDER BY CASE priority
                WHEN 'critical' THEN 4
                WHEN 'high' THEN 3
                WHEN 'medium' THEN 2
                WHEN 'low' THEN 1
                ELSE 0 END DESC,
                COALESCE(updated_at,created_at,0) ASC
             LIMIT 1",
            [],
            |r| {
                Ok(TaskClaim {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    description: r.get(2)?,
                    why_: r.get(3)?,
                    context: r.get(4)?,
                    goal_id: r.get(5)?,
                    relevant_files: r.get(6)?,
                    tools: r.get(7)?,
                    acceptance_criteria: r.get(8)?,
                    workspace_path: r.get(9)?,
                })
            },
        )
        .optional()
        .map_err(|e| e.to_string())?
    };

    let Some(candidate) = candidate else {
        tx.commit().map_err(|e| e.to_string())?;
        return Ok(ClaimResult::NoTasks);
    };

    let updated = tx
        .execute(
            "UPDATE tasks SET status='in_progress', agent_id=?1, updated_at=?2, last_ping_at=?2 WHERE id=?3 AND status='todo'",
            params![agent, now, candidate.id],
        )
        .map_err(|e| e.to_string())?;

    tx.commit().map_err(|e| e.to_string())?;

    if updated == 0 {
        return Ok(ClaimResult::RaceLost);
    }

    let verify: Option<(String, Option<String>)> = conn
        .query_row(
            "SELECT status, agent_id FROM tasks WHERE id=?1",
            params![candidate.id.clone()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    if let Some((status, owner)) = verify {
        if status == "in_progress" && owner.unwrap_or_default() == agent {
            if let Some(goal) = &candidate.goal_id {
                let _ = sync_goal(conn, goal);
            }
            return Ok(ClaimResult::Claimed(candidate));
        }
    }

    Ok(ClaimResult::RaceLost)
}

fn sync_goal(conn: &Connection, goal_id: &str) -> Result<(), String> {
    let now = now_ts();
    conn.execute(
        "UPDATE goals SET status = CASE
  WHEN NOT EXISTS(SELECT 1 FROM tasks WHERE goal_id=?1) THEN 'todo'
  WHEN EXISTS(SELECT 1 FROM tasks WHERE goal_id=?2 AND status='in_progress') THEN 'ongoing'
  WHEN EXISTS(SELECT 1 FROM tasks WHERE goal_id=?3 AND status='review') THEN 'review'
  WHEN NOT EXISTS(SELECT 1 FROM tasks WHERE goal_id=?4 AND status!='done') THEN 'done'
  WHEN EXISTS(SELECT 1 FROM tasks WHERE goal_id=?5 AND status='done') THEN 'ongoing'
  ELSE 'todo'
END, updated_at=?6 WHERE id=?7",
        params![goal_id, goal_id, goal_id, goal_id, goal_id, now, goal_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn log_event(
    conn: &Connection,
    command: &str,
    task_id: Option<&str>,
    goal_id: Option<&str>,
    agent_id: Option<&str>,
    duration_ms: i64,
) {
    let _ = conn.execute(
        "INSERT INTO events (id, command, task_id, goal_id, agent_id, duration_ms, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            gen_id(),
            command,
            task_id,
            goal_id,
            agent_id,
            duration_ms,
            now_ts()
        ],
    );
}

fn current_agent(explicit: Option<&str>) -> String {
    if let Ok(v) = env::var("IMI_AGENT_ID") {
        if !v.trim().is_empty() {
            return v;
        }
    }
    if let Some(v) = explicit {
        if !v.trim().is_empty() {
            return v.to_string();
        }
    }
    env::var("USER")
        .or_else(|_| env::var("USERNAME"))
        .unwrap_or_else(|_| "agent".to_string())
}

fn now_ts() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn ago(ts: i64) -> String {
    let diff = (now_ts() - ts).max(0);
    if diff < 60 {
        format!("{diff}s")
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86400 {
        format!("{}h", diff / 3600)
    } else {
        format!("{}d", diff / 86400)
    }
}

fn status_icon(out: OutputCtx, status: &str) -> String {
    match status {
        "done" => paint(out, "32", "✅"),
        "in_progress" | "ongoing" => paint(out, "33", "🔄"),
        "review" => paint(out, "35", "🔎"),
        "blocked" | "failed" | "cancelled" => paint(out, "31", "🚫"),
        _ => paint(out, "90", "📋"),
    }
}

fn priority_icon(out: OutputCtx, priority: &str) -> String {
    match priority {
        "critical" | "high" => paint(out, "31", "▲"),
        "low" => paint(out, "36", "▽"),
        _ => paint(out, "37", "■"),
    }
}

fn paint(out: OutputCtx, code: &str, text: &str) -> String {
    if out.color {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn base36(mut n: u128) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let chars: Vec<char> = "0123456789abcdefghijklmnopqrstuvwxyz".chars().collect();
    let mut out = String::new();
    while n > 0 {
        out.insert(0, chars[(n % 36) as usize]);
        n /= 36;
    }
    out
}

fn rand_u8() -> u8 {
    let mut b = [0u8; 1];
    if let Ok(mut f) = File::open("/dev/urandom") {
        if f.read_exact(&mut b).is_ok() {
            return b[0];
        }
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    (nanos & 0xff) as u8
}

fn gen_id() -> String {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let chars: Vec<char> = "0123456789abcdefghijklmnopqrstuvwxyz".chars().collect();
    let rand: String = (0..8)
        .map(|_| chars[(rand_u8() as usize) % 36])
        .collect();
    format!("{}{}", base36(ts_ms), rand)
}

fn goal_to_value(g: &GoalRow) -> Value {
    json!({
        "id": g.id,
        "name": g.name,
        "description": g.description,
        "why": g.why_,
        "for_who": g.for_who,
        "success_signal": g.success_signal,
        "status": g.status,
        "priority": g.priority,
        "created_at": g.created_at
    })
}

fn task_to_value(t: &TaskRow) -> Value {
    json!({
        "id": t.id,
        "title": t.title,
        "description": t.description,
        "why": t.why_,
        "goal_id": t.goal_id,
        "status": t.status,
        "priority": t.priority,
        "agent_id": t.agent_id,
        "created_at": t.created_at
    })
}

fn memory_to_value(m: &MemoryRow) -> Value {
    json!({
        "id": m.id,
        "goal_id": m.goal_id,
        "task_id": m.task_id,
        "key": m.key,
        "value": m.value,
        "type": m.typ,
        "source": m.source,
        "created_at": m.created_at
    })
}

fn lesson_to_value(l: &LessonRow) -> Value {
    json!({
        "id": l.id,
        "what_went_wrong": l.what_went_wrong,
        "correct_behavior": l.correct_behavior,
        "verified_by": l.verified_by,
        "created_at": l.created_at
    })
}

fn wip_task_to_value(t: &TaskRowWithGoal) -> Value {
    json!({
        "id": t.id,
        "title": t.title,
        "description": t.description,
        "why": t.why_,
        "goal_id": t.goal_id,
        "goal_name": t.goal_name,
        "status": t.status,
        "priority": t.priority,
        "agent_id": t.agent_id,
        "created_at": t.created_at
    })
}

struct ToonBuilder {
    buf: String,
}

impl ToonBuilder {
    fn new() -> Self {
        Self { buf: String::new() }
    }

    fn section(&mut self, name: &str, fields: &[&str], rows: Vec<Vec<String>>) {
        if rows.is_empty() {
            return;
        }
        if !self.buf.is_empty() {
            self.buf.push('\n');
        }
        self.buf
            .push_str(&format!("{}[{}]{{{}}}:\n", name, rows.len(), fields.join(",")));
        for row in rows {
            let escaped: Vec<String> = row.into_iter().map(|v| escape_toon(&v)).collect();
            self.buf.push_str("  ");
            self.buf.push_str(&escaped.join(","));
            self.buf.push('\n');
        }
    }

    fn finish(self) -> String {
        self.buf
    }
}

fn escape_toon(v: &str) -> String {
    v.replace('\\', "\\\\")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

fn instructions_cursor() -> &'static str {
    "# IMI Ops\n\nEvery session:\nimi status\nimi context\n\nWhen working:\nimi start <task_id>\nimi complete <task_id> \"summary\"\nimi memory add <goal_id> <key> \"insight\""
}

fn instructions_copilot() -> &'static str {
    "# IMI Ops for Copilot\n\nAt session start run:\nimi status\nimi context\n\nWhen you take work:\nimi start <task_id>\n\nWhen done:\nimi complete <task_id> \"summary\"\nimi memory add <goal_id> <key> \"what you learned\""
}

fn instructions_windsurf() -> &'static str {
    "# IMI Ops for Windsurf\n\nBoot:\nimi status\nimi context\n\nExecution loop:\nimi next\nimi start <task_id>\nimi complete <task_id> \"summary\"\nimi memory add <goal_id> <key> \"insight\""
}

fn cmd_check(conn: &Connection, out: OutputCtx, task_id: Option<String>) -> Result<(), String> {
    if let Some(task_id) = task_id {
        return cmd_verify(conn, out, task_id);
    }
    cmd_audit(conn, out)
}

fn cmd_verify(conn: &Connection, out: OutputCtx, task_prefix: String) -> Result<(), String> {
    let task_id = resolve_id_prefix(conn, "tasks", &task_prefix)?
        .ok_or_else(|| format!("No task with ID '{task_prefix}' — run `imi tasks` to list available tasks"))?;

    let (title, status, description, acceptance_criteria, relevant_files, why): (String, String, String, Option<String>, String, String) = conn
        .query_row(
            "SELECT title, status, description, acceptance_criteria, relevant_files, why FROM tasks WHERE id=?1",
            params![task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .map_err(|e| e.to_string())?;

    // Fetch completion memories
    let completion_summary: Option<String> = conn
        .query_row(
            "SELECT value FROM memories WHERE task_id=?1 AND key='completion_summary' ORDER BY created_at DESC LIMIT 1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let interpretation: Option<String> = conn
        .query_row(
            "SELECT value FROM memories WHERE task_id=?1 AND key='interpretation' ORDER BY created_at DESC LIMIT 1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    let uncertainty: Option<String> = conn
        .query_row(
            "SELECT value FROM memories WHERE task_id=?1 AND key='uncertainty' ORDER BY created_at DESC LIMIT 1",
            params![task_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| e.to_string())?;

    // Determine verification flags
    let has_criteria = acceptance_criteria.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let has_summary = completion_summary.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false);
    let is_done = status == "done";

    let unverified = !has_criteria || !has_summary;

    if out.is_json() {
        println!("{}", json!({
            "id": task_id,
            "title": title,
            "status": status,
            "has_acceptance_criteria": has_criteria,
            "has_completion_summary": has_summary,
            "unverified": unverified,
            "acceptance_criteria": acceptance_criteria,
            "completion_summary": completion_summary,
            "interpretation": interpretation,
            "uncertainty": uncertainty,
            "description": description,
            "relevant_files": relevant_files,
            "why": why,
        }));
        return Ok(());
    }

    println!("Verify: {} [{}]", title, task_id);
    println!("Status: {}", status);
    println!();

    if has_criteria {
        println!("Acceptance criteria:");
        println!("  {}", acceptance_criteria.as_deref().unwrap_or(""));
    } else {
        println!("⚠  No acceptance criteria — nothing to verify against");
    }
    println!();

    if has_summary {
        println!("Completion summary:");
        println!("  {}", completion_summary.as_deref().unwrap_or(""));
    } else if is_done {
        println!("⚠  Marked done but no completion summary written");
    } else {
        println!("No completion summary (task not done yet)");
    }

    if let Some(ref interp) = interpretation {
        println!();
        println!("Agent interpretation:");
        println!("  {}", interp);
    }

    if let Some(ref unc) = uncertainty {
        println!();
        println!("Agent uncertainty:");
        println!("  {}", unc);
    }

    if !description.is_empty() {
        println!();
        println!("Description: {}", description);
    }
    if !why.is_empty() {
        println!("Why: {}", why);
    }

    let rf: Vec<String> = serde_json::from_str(&relevant_files).unwrap_or_default();
    if !rf.is_empty() {
        println!();
        println!("Relevant files:");
        for f in &rf {
            let exists = Path::new(f).exists();
            println!("  {} {}", if exists { "✓" } else { "✗" }, f);
        }
    }

    println!();
    if unverified {
        println!("UNVERIFIED — agent should check if this work actually exists in the codebase.");
    } else {
        println!("Verifiable — criteria and summary present. Agent should confirm criteria is met.");
    }

    Ok(())
}

fn cmd_audit(conn: &Connection, out: OutputCtx) -> Result<(), String> {
    // Find tasks that are done but missing acceptance_criteria or completion_summary
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.status, t.goal_id,
                t.acceptance_criteria,
                (SELECT value FROM memories WHERE task_id=t.id AND key='completion_summary' ORDER BY created_at DESC LIMIT 1) as summary
         FROM tasks t
         WHERE t.status='done'
         ORDER BY t.updated_at DESC"
    ).map_err(|e| e.to_string())?;

    struct AuditRow {
        id: String,
        title: String,
        goal_id: String,
        has_criteria: bool,
        has_summary: bool,
    }

    let rows: Vec<AuditRow> = stmt
        .query_map([], |r| {
            let ac: Option<String> = r.get(4)?;
            let sum: Option<String> = r.get(5)?;
            Ok(AuditRow {
                id: r.get(0)?,
                title: r.get(1)?,
                goal_id: r.get(3)?,
                has_criteria: ac.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false),
                has_summary: sum.as_deref().map(|s| !s.trim().is_empty()).unwrap_or(false),
            })
        })
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();

    let unverified: Vec<&AuditRow> = rows.iter().filter(|r| !r.has_criteria || !r.has_summary).collect();
    let verified: Vec<&AuditRow> = rows.iter().filter(|r| r.has_criteria && r.has_summary).collect();

    if out.is_json() {
        let unverified_json: Vec<Value> = unverified.iter().map(|r| json!({
            "id": r.id,
            "title": r.title,
            "goal_id": r.goal_id,
            "has_acceptance_criteria": r.has_criteria,
            "has_completion_summary": r.has_summary,
        })).collect();
        println!("{}", json!({
            "total_done": rows.len(),
            "verified": verified.len(),
            "unverified": unverified.len(),
            "unverified_tasks": unverified_json,
        }));
        return Ok(());
    }

    println!("## Audit");
    println!(
        "Done tasks: {} total, {} verified, {} needing review",
        rows.len(),
        verified.len(),
        unverified.len()
    );
    println!();

    if unverified.is_empty() {
        println!("All done tasks have acceptance criteria and completion summaries.");
    } else {
        println!("## Needs verification ({})", unverified.len());
        for r in &unverified {
            let flags = format!("{}{}",
                if !r.has_criteria { " missing acceptance criteria" } else { "" },
                if !r.has_summary { " no completion summary" } else { "" },
            );
            println!("  ⚠  {} [{}]{}", r.title, r.id, flags);
        }
    }

    if !verified.is_empty() {
        println!();
        println!("## Verified ({})", verified.len());
        for r in &verified {
            println!("  ✓  {} [{}]", r.title, r.id);
        }
    }

    Ok(())
}

fn cmd_think(conn: &Connection, _out: OutputCtx) -> Result<(), String> {
    let context = build_think_context(conn)?;

    println!("You are a sharp product manager reviewing the state of a product ops database (IMI).");
    println!("IMI is the translation layer between human product thinking and AI agent execution.");
    println!("Your job is not to audit task completion — it is to reason about strategic alignment.");
    println!();
    println!("Ask: given what was decided and why, are we still working on the right thing?");
    println!("Read the direction notes as the human thinking process. Read the decisions as bets that were made.");
    println!("Read the goals as what the team believes is worth building right now.");
    println!();
    println!("Surface: what no longer aligns with stated intent. What bets are stale or based on outdated assumptions.");
    println!("What a sharp PM would challenge or kill. What is missing that the team has not articulated yet.");
    println!("What the real next move is given everything you know.");
    println!();
    println!("Do not summarize what exists. Do not list completed tasks. Reason about whether the work still serves the intent behind it.");
    println!("Be direct. Be ruthless about misalignment. This is not a status report — it is a strategic alignment check.");
    println!();
    println!("---");
    println!();
    println!("{}", context);
    println!("---");
    println!();
    println!("Given what was decided, why it was decided, and how direction has evolved —");
    println!("are we working on the right things? What no longer aligns with intent?");
    println!("What would you challenge or kill? What is the team not seeing?");
    println!("What is the single most important thing to clarify or act on right now?");
    Ok(())
}

fn build_think_context(conn: &Connection) -> Result<String, String> {
    let mut out = String::new();

    // Goals
    out.push_str("## Goals\n");
    let mut stmt = conn.prepare(
        "SELECT id, name, status, priority, why, description FROM goals ORDER BY priority DESC, created_at"
    ).map_err(|e| e.to_string())?;
    let goals: Vec<(String, String, String, String, String, String)> = stmt
        .query_map([], |r| Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            r.get::<_, Option<String>>(5)?.unwrap_or_default(),
        )))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    for (id, name, status, priority, why, desc) in &goals {
        out.push_str(&format!("- [{}] {} ({}, {}) — why: {} | {}\n", status, name, id, priority, why, desc));
    }

    // Tasks grouped by status
    out.push_str("\n## Tasks\n");
    let mut stmt = conn.prepare(
        "SELECT t.id, t.title, t.status, t.why, t.description, t.acceptance_criteria,
                (SELECT value FROM memories WHERE task_id=t.id AND key='completion_summary' ORDER BY created_at DESC LIMIT 1) as summary,
                g.name as goal_name
         FROM tasks t LEFT JOIN goals g ON t.goal_id=g.id
         ORDER BY t.status, t.priority DESC"
    ).map_err(|e| e.to_string())?;
    let tasks: Vec<(String, String, String, String, String, String, String, String)> = stmt
        .query_map([], |r| Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            r.get::<_, Option<String>>(7)?.unwrap_or_default(),
        )))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    for (id, title, status, why, _desc, criteria, summary, goal_name) in &tasks {
        out.push_str(&format!("- [{}] {} [{}] (goal: {})\n", status, title, id, goal_name));
        if !why.is_empty() { out.push_str(&format!("  why: {}\n", why)); }
        if !criteria.is_empty() { out.push_str(&format!("  criteria: {}\n", criteria)); }
        if !summary.is_empty() { out.push_str(&format!("  done summary: {}\n", &summary[..summary.len().min(300)])); }
    }

    // Decisions
    out.push_str("\n## Decisions\n");
    let mut stmt = conn.prepare(
        "SELECT what, why, affects FROM decisions ORDER BY created_at DESC LIMIT 10"
    ).map_err(|e| e.to_string())?;
    let decisions: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((
            r.get(0)?,
            r.get(1)?,
            r.get::<_, Option<String>>(2)?.unwrap_or_default(),
        )))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    for (what, why, affects) in &decisions {
        out.push_str(&format!("- {} — {}", what, why));
        if !affects.is_empty() { out.push_str(&format!(" (affects: {})", affects)); }
        out.push('\n');
    }

    // Direction notes
    out.push_str("\n## Direction Notes\n");
    let mut stmt = conn.prepare(
        "SELECT content FROM direction_notes ORDER BY created_at DESC LIMIT 5"
    ).map_err(|e| e.to_string())?;
    let notes: Vec<String> = stmt
        .query_map([], |r| r.get(0))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    for note in &notes {
        out.push_str(&format!("- {}\n", note));
    }

    // Recent memories
    out.push_str("\n## Recent Memories (last 10)\n");
    let mut stmt = conn.prepare(
        "SELECT key, value, type FROM memories ORDER BY created_at DESC LIMIT 10"
    ).map_err(|e| e.to_string())?;
    let mems: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .map_err(|e| e.to_string())?
        .filter_map(|r| r.ok())
        .collect();
    for (key, value, kind) in &mems {
        out.push_str(&format!("[{}] {}: {}\n", kind, key, &value[..value.len().min(300)]));
    }

    Ok(out)
}
