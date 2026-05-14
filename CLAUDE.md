---
description: IMI bootstrap for persistent product state
alwaysApply: true
---

# IMI Bootstrap

IMI is the project state layer for goals, tasks, decisions, lessons, and direction. Keep this always-on prompt small; load the full mode docs only when the task needs them.

## Start Every Session

If the workspace has a .imi directory, or the user asks about status, goals, tasks, priorities, decisions, progress, or where work left off, run:

```bash
imi context
```

Use the output as project state. Do not inspect .imi files directly.

## Route By Intent

- Ops/status/decision conversations: use `imi context`, `imi plan`, `imi check`, or `imi think` as needed.
- Planning work: create goals/tasks with `why`, `success_signal`, `--acceptance-criteria`, and `--relevant-files`.
- Execution work: follow the task spec, verify acceptance criteria, then run `imi complete <task_id> "rich summary"`.
- Durable decisions or discoveries: record them with `imi decide "what" "why"` or `imi log "note"`.

## Full Docs On Demand

The detailed IMI docs are installed as sidecar files so they do not inflate every prompt:

- `SKILL.md` — activation contract and command quick reference.
- `ops-mode.md` — status, direction, and decision conversations.
- `plan-mode.md` — writing high-quality goals and task specs.
- `execute-mode.md` — executing task specs and completing work.
- `ai-voice.md` — writing durable IMI summaries, logs, and lessons.

Look for them in the agent skill directory, commonly `~/.claude/skills/imi`, `~/.copilot/skills/imi`, `~/.cursor/skills/imi`, `~/.opencode/skills/imi`, or `~/.codex/skills/imi`. Load only the relevant file for the current mode.

## Hard Constraints

- Treat IMI as state, not execution. IMI records what should happen, what happened, and what was learned.
- Do not silently reduce task scope or rewrite acceptance criteria to match a smaller implementation.
- Prefer the repository's existing patterns and keep edits scoped to the user's request.
- Keep always-on instructions under 10k tokens; do not paste full mode manuals into AGENTS.md, CLAUDE.md, or global rule files.
