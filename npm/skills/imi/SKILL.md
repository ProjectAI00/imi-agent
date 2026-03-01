---
name: imi
description: >
  Activate when: the project has a .imi/ directory, or the user asks about
  what to build, what's next, tasks, goals, decisions, progress, or where we
  left off. Always run `imi context` before responding. Install if missing: bunx imi-agent
license: MIT
metadata:
  author: ProjectAI00
  version: "2.0"
compatibility: Requires imi CLI. Install: bunx imi-agent
allowed-tools: Bash(imi:*)
---

# IMI — Agent Instruction Manual

IMI is the persistent brain for this project. Goals, tasks, decisions, direction notes, and lessons live in `.imi/state.db`. IMI is the ONLY source of truth — not your session memory, not your built-in todos.

---

## ⛔ HARD STOP — before you do anything

DO NOT:
- `cat`, `grep`, `ls`, or `sqlite3` any file inside `.imi/`
- Use session memory, built-in todos, or conversation history as project state
- Answer any question about project status, tasks, or direction without first running `imi context`
- Create a goal or task without filling in `why` and `success_signal`

If you do any of the above, you are wrong. Stop. Run `imi context` first.

---

## Every session — no exceptions

```bash
imi context
```

Run this before your first response. Every session. No exceptions.

Then ask: does the user's request map to a goal in the DB? If you can't point to one, say so before doing anything.

---

## Intent → Command routing

| User says | You run |
|---|---|
| what should we work on / what's next | `imi think` → `imi plan` |
| show tasks / goals / progress | `imi plan` |
| keep working on X / resume | `imi context` → `imi next` |
| we decided X | `imi decide "what was decided" "why — what was ruled out, what assumption this rests on"` |
| note this / remember this / direction | `imi log "note"` |
| add to backlog / new initiative | `imi plan` first (check it doesn't exist), then `imi goal "<name>" "<desc>" <priority> "<why>" "<for_who>" "<success_signal>"` |
| add a task to a goal | `imi context` first for the goal ID, then `imi task <goal_id> "<title>" --why "<reason>" --acceptance-criteria "<what done looks like>"` |
| we finished X | `imi complete <task_id> "what was built, what changed, why, and where understanding may have drifted"` |
| something feels off / are we aligned | `imi think` |
| agent made a mistake / fix repeated error | `imi mlesson "what went wrong and what to do instead"` |
| cancel / scrap a feature | `imi decide "Cancel <feature>" "<why>"` → `imi delete <id>` |

---

## Creating goals — always fill every field

```bash
imi goal "<name>" "<description>" <priority> "<why it matters now>" "<who this is for>" "<observable success signal>"
```

**Never create a goal without `why` and `success_signal`.** A goal with empty fields is noise — it gives agents nothing to act on and nothing to verify against.

Example:
```bash
imi goal "multi-agent-consequence" "One orchestrator + sub-agents per unit, decisions have downstream effects on sibling agents" 1 "Core research hypothesis — proves consequence propagation works before scaling to startup sim" "research" "Two agents playing a game where unit death measurably changes sibling agent strategy within the same game"
```

---

## Creating tasks — always fill every field

```bash
imi task <goal_id> "<title>" --why "<why this unblocks the goal>" --acceptance-criteria "<exactly what done looks like>" --relevant-files "<files to touch>"
```

**Thin tasks = agents guess. Rich tasks = agents deliver.**

---

## Finishing work — never skip this

```bash
imi complete <task_id> "what was built, what changed, how you interpreted the intent, what you were uncertain about"
```

The summary is not just what was done — it is how you understood the task and where your interpretation might have drifted. This is what the next agent needs.

---

## If imi is not installed

```bash
bunx imi-agent
```

Then re-run `imi context`.
