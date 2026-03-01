# IMI — AI Voice Guide

Use this guide when writing `imi complete`, `imi log`, and `imi lesson` content. It defines what to say and how to say it so future agents can trust and reuse stored context.

The content an agent writes back to IMI is the only thing that survives a session. When a new agent picks up this goal in a future session, everything they know about what's been done, what was decided, and what to watch out for comes from what previous agents wrote. If you write vague summaries, you erase your own work from shared memory. Write like someone who cares that the next agent doesn't have to start over.

---

## 1) Completion Summary Structure (`imi complete`)

Every completion summary must include four parts:

1. **What was built**
   - Name concrete outputs: files, functions, commands, tests.
   - State the change in past tense.

2. **How the intent was interpreted**
   - State how you read the task and what scope you executed.
   - Call out if you narrowed or expanded scope and why.

3. **Uncertainty or drift**
   - State anything ambiguous, unverifiable, or mismatched vs spec.
   - Be explicit about what was verified vs not verified.

4. **Future agent note**
   - One practical handoff line: what to check first before touching this area again.

**Template:**

```text
Built: <concrete changes with files/functions/tests>.
Interpretation: <how task intent was read and applied>.
Drift/uncertainty: <spec mismatch, risk, or unverifiable point>.
Future-agent note: <first thing to know/check next time>.
```

**With flags:**

```bash
imi complete <task_id> "Built: <summary>. Interpretation: <intent>. Drift: <gaps>. Future-agent: <note>." \
  --interpretation "<how you read the scope and what you executed>" \
  --uncertainty "<anything ambiguous, unverifiable, or that deviated from spec>" \
  --outcome "<real-world result: tests passed/failed, build status, deployed, not tested>"
```

Use the flags when they add signal beyond the summary text — typically for tasks where interpretation of scope was ambiguous, or where verification was incomplete.

---

## 2) Voice Principles

- Write in **direct, past tense**: `Implemented`, `Added`, `Removed`, `Verified`.
- Use **no hedging**: avoid `tried`, `attempted to`, `maybe`, `seems`, `probably`.
- Avoid fluff words like **"successfully"** and **"properly"**.
- Prefer **specific references** over abstractions:
  - Good: `prompts/ai-voice.md`, `src/main.rs:L47`, `fire_analytics()`
  - Bad: `the file`, `some logic`, `the system`
- Keep statements falsifiable: each line should be checkable in code or command output.

---

## 3) Observation Entries (`imi log`)

A log entry should record one durable, reusable fact — something that will matter the next time an agent touches this area.

- Write it as a present-tense observation or constraint: `"execute-mode.md needs command signature updates before the next agent session"`, `"BLOCKED: task-7 — serde version pinned at 1.0.188, upgrade requires touching three crates"`
- Include concrete location when relevant (file, function, line, command).
- Use `BLOCKED: <task_id>` prefix when logging a genuine blocker — this makes the note easy to scan later.
- Store durable facts, constraints, patterns, or locations.
- **Never** use log entries for status updates that duplicate what `imi complete` already captures.

**Patterns:**

```text
Where is <thing>? → <file:function:line or command>
Why does <behavior> happen? → <constraint/decision>
What must stay true in this area? → <rule>
BLOCKED: <task_id> — <exact blocker, what was tried, what needs to happen>
```

---

## 4) Blocker Entries (when you can't complete)

When you're genuinely stuck and cannot finish the task, do not silently abandon it. Log the blocker and preserve everything you found:

```bash
imi log "BLOCKED: <task_id> — <found> | <tried> | <impact> | <next>"
imi decide "could not complete <task_id>" "<reason, including specific commands or searches that confirmed the block>"
```

Every blocker description must include:
1. **Found** — what the codebase actually has (file exists or doesn't, column name, type mismatch, etc.)
2. **Tried** — search terms, paths, commands you ran to confirm the block
3. **Impact** — why this specific thing blocks the task
4. **Next** — what has to change before retry, as precisely as possible

**Template:**

```text
BLOCKED: <task_id> — Found: <what's actually there>. Tried: <search/commands to confirm>. Impact: <why this blocks completion>. Next: <prerequisite before retry>.
```

---

## 5) Lesson Entries (`imi lesson`)

Lessons are for verified corrections — moments where a human had to correct something that should have been obvious, or where the same mistake happened more than once.

Every future session sees these lessons before starting work. Write them as a correction that a new agent can act on immediately.

```bash
imi lesson "Do not rewrite run.ts installSkills() without first confirming all target paths exist — the ~/.opencode/ directory may not exist on the user's machine and the script will silently fail" \
  --correct-behavior "Check that each target directory exists before writing; create it or skip with a warning" \
  --verified-by "user confirmed the .opencode directory was missing and no file was written"
```

---

## 6) Bad vs Good Examples

### A) Completion summaries (`imi complete`)

**Bad:** `Updated prompt files.`

**Good:** `Built: Added npm/skills/imi/ai-voice.md with sections for summary structure, voice rules, log format, blocker format, and examples. Interpretation: Treated the task as a companion to execute-mode.md focused on writing quality, not command usage. The scope was expanded slightly to include the imi log blocker pattern because it maps directly to the imi fail concept from the old prompts. Drift/uncertainty: No spec mismatch found; no runtime verification needed because this was documentation-only. Future-agent note: Keep examples concrete with file/function references when expanding this guide. If execute-mode tone changes, update wording here to stay aligned.`

**Bad:** `Done. Everything looks good.`

**Good:** `Built: Documented mandatory four-part completion summary format and blocker log format in npm/skills/imi/ai-voice.md. Updated imi complete examples throughout execute-mode.md to include --interpretation and --uncertainty flags. Interpretation: Implemented concise policy text with actionable templates. Drift/uncertainty: Did not validate against live task outputs because acceptance was file-content based. Future-agent note: execute-mode.md references this file for voice guidance — if either file changes significantly, review them together.`

---

### B) Log entries (`imi log`)

**Bad:** `note → changed some docs`

**Good:** `ops-mode.md also needs the same conversational rewrite — it currently reads like a checklist rather than guidance, which is the same problem execute-mode.md had before this task`

**Bad:** `update → task complete`

**Good:** `BLOCKED: task-12 — Found: the acceptance_criteria column doesn't exist; schema uses acceptanceCriteria (camelCase). Tried: sqlite3 .schema tasks — column name confirmed. Impact: all task creation in execute-mode.md references the wrong field name. Next: update task spec to use camelCase field name, or fix schema to use snake_case before retry.`

---

### C) Lesson entries (`imi lesson`)

**Bad:** `Don't make mistakes with commands.`

**Good:**
```bash
imi lesson "imi complete --interpretation and --outcome flags existed since v0.3.x — stop writing summaries without them when scope or verification is ambiguous" \
  --correct-behavior "Always include --interpretation when scope was adjusted, --uncertainty when criteria couldn't be verified, --outcome when there's a real-world result (build, test, deploy)" \
  --verified-by "human pointed out that two completed tasks had no flags despite ambiguous scope — flags are required not optional"
```
