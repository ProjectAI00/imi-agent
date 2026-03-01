---
name: imi
description: >
  Activate when: the project has a .imi/ directory, or the user asks about
  what to build, what's next, tasks, goals, decisions, progress, or where we
  left off. Always run `imi context` before responding. Install if missing: bunx imi-agent
license: MIT
metadata:
  author: ProjectAI00
  version: "3.0"
compatibility: Requires imi CLI. Install: bunx imi-agent
allowed-tools: Bash(imi:*)
---

# IMI — Agent Instruction Manual

IMI is the PM thinking layer between humans and agents. It keeps human intent, direction, and decisions persistent so every session stays aligned without re-briefing. Goals, tasks, decisions, and direction notes live in `.imi/state.db`.

**The 3 questions IMI must always answer:**
1. What are we building?
2. How is it going?
3. Are we still aligned with intent?

**Authority order:** Direction notes + decisions → goals → tasks → execution. If work cannot trace to a direction or decision, flag it before doing anything.

---

## ⛔ HARD STOP — before you do anything

DO NOT:
- `cat`, `grep`, `ls`, or `sqlite3` any file inside `.imi/`
- Use session memory, built-in todos, or conversation history as project state
- Answer any question about project status without first running `imi context`
- Create a goal or task without filling in `why` and `success_signal`
- Execute work that cannot be traced to a goal in the DB

---

## Every session — no exceptions

```bash
imi context
```

Run this before your first response. Every session. No exceptions. Then ask: does the user's request map to a goal in the DB? If you can't point to one, say so before doing anything.

---

## Mode routing

Detect which mode applies and load the full instructions:

| Situation | Mode | Load |
|---|---|---|
| User is checking status, making decisions, thinking out loud | **Ops** | `cat ~/.copilot/skills/imi/ops-mode.md` |
| User wants to plan goals, decompose work, write task specs | **Plan** | `cat ~/.copilot/skills/imi/plan-mode.md` |
| User wants to execute a task, do the work | **Execute** | `cat ~/.copilot/skills/imi/execute-mode.md` |
| Writing `imi complete`, `imi fail`, or memory entries | **Voice** | `cat ~/.copilot/skills/imi/ai-voice.md` |

Load the relevant mode file before proceeding. It contains the full behavioral contract for that mode.

---

## Quick command reference

```bash
imi context                        # start every session here
imi think                          # is this still the right thing to build?
imi plan                           # full goal + task list
imi decide "what" "why"            # log a firm decision
imi log "note"                     # log a direction, instinct, or observation
imi goal "<name>" "<desc>" <pri> "<why>" "<for_who>" "<success_signal>"
imi task <goal_id> "<title>" --why "<reason>" --acceptance-criteria "<done looks like>" --relevant-files "<files>"
imi complete <task_id> "summary"   # mark done — never skip this
imi mlesson "what went wrong"      # store a lesson after a corrected mistake
imi check                          # verification state
```

---

## If imi is not installed

```bash
bunx imi-agent
```

Then re-run `imi context`.
