# IMI

Like a lot of engineers, I spend a lot of time working with AI coding agents — Cursor, Claude Code — to ship products and features.

The better the models got, the harder it became to track what was being shipped, what still needed to be built, and what was actually worth building. Our team ships so much that we pretty much gave up on keeping up with GitHub PRs or task boards. It got genuinely overwhelming.

So I built IMI.

IMI is an AI agent that just solves it. It tracks all decisions, notes, goals, and tasks inside your codebase. Every time you start a new session, you can recall previous context and have your agent know exactly where to pick up from.

Just start with something like:

```
imi what do we need to do — check logs and previous decisions
```

Agents like Claude Code instantly know where you left off. Every time an agent ships something, it updates the board in the background. And the best part: you don't have to do much. Just a simple prompt to update this or that.

---

## Install

### Option 1 — CLI binary (standalone)

```bash
bunx @imi-ai/imi@latest
```

That's it. Downloads the binary, runs `imi init` in your project folder.

Or via curl:

```bash
curl -fsSL https://aibyimi.com/install | bash
```

Make sure `~/.local/bin` is in your `$PATH`:
```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc && source ~/.zshrc
```

### Option 2 — Plugin (Claude Code + GitHub Copilot CLI)

Install the IMI plugin directly into your agent CLI. The plugin injects the
session contract automatically — agents know to call `imi` before responding,
without manual briefing.

**Claude Code:**
```bash
/plugin marketplace add ProjectAI00/ai-db-imi
/plugin install imi
```

**GitHub Copilot CLI:**
```
/plugin marketplace add ProjectAI00/ai-db-imi
/plugin install imi
```

**Or install the skill manually** (works in Claude Code, Copilot CLI, Cursor — anywhere that reads `~/.copilot/skills` or `~/.claude/skills`):
```bash
npx skills add ProjectAI00/ai-db-imi@imi
```

## Usage

```bash
imi context                           # Human intent first: direction, decisions, current focus
imi plan                              # Goals/tasks/progress plan
imi run <task_id>                     # Execute one task with runtime writeback
imi check                             # Verification snapshot (done work needing review)
imi help                              # Command reference

# Advanced (agent/runtime)
imi next --agent <name>               # Atomically claim next task
imi complete <id> "what you did"      # Mark done + store completion memory
imi fail <id> "why it failed"         # Release task back to queue with failure context
imi goal "name" --why "why now"       # Create a goal
imi task <goal_id> "title" --why "..." # Add a task
imi log "direction note"              # Log strategic direction
imi decide "what" "why"               # Log a decision
imi orchestrate <goal_id> --workers 8 -- <cmd ...>  # Parallel worker loop
```

## The Loop

```
1. You define direction     →  imi context
2. You shape the plan       →  imi plan
3. Agent claims a task      →  imi next --agent claude
4. Agent executes           →  (Claude Code / Copilot / Cursor does the work)
5. Runtime writes back      →  imi wrap <task_id> -- <agent command ...>
                               (auto checkpoints + complete/fail on exit)
6. Next agent picks up      →  imi context + imi plan  ← sees prior context automatically
```

Each cycle compounds. Agents get smarter about your project over time. Works across sessions, machines, and team members.

## Integrations

IMI is the state layer. These tools plug into the execution layer beneath it — IMI doesn't call them, agents choose when to use them.

### Hankweave — task execution & checkpointing

Use Hankweave for long or complex multi-step tasks where you want rollback and checkpointing:

```bash
./imi run <task_id>        # generates hank.json from task context
bunx hankweave hank.json   # execute it
```

Hankweave handles HOW work gets done. IMI handles WHAT was decided and WHAT was learned.

### Entire — session audit & rewind

Use Entire for session replay and audit:

```bash
entire enable --agent claude-code   # hook into your agent sessions
entire rewind                       # replay what happened in a past session
entire explain                      # summarise a session in plain English
```

Entire records what happened. IMI remembers what matters. Forward state + backward audit.

## Stack

- **Rust** — single binary, zero runtime dependencies, ~5ms per command
- **SQLite** — portable, zero-config, project-local (`.imi/state.db`)
- **Hankweave** — optional execution/checkpointing layer
- **Entire** — optional session audit/rewind layer
- **Works with**: Claude Code, GitHub Copilot CLI, Cursor, Codex, any CLI agent

## Agent Prompts

Drop these as system prompts to give any agent full IMI literacy:

| File | Use when |
|------|----------|
| `npm/skills/imi/plan-mode.md` | Agent is decomposing a goal into tasks |
| `prompts/execute-mode.md` | Agent is executing a task |
| `npm/skills/imi/ops-mode.md` | Conversational ops / status / decisions |

## Multi-Agent Support

Multiple agents can work in parallel — each claims a different task atomically:

```bash
imi next --agent engineer-a --toon   # Agent A claims task 1
imi next --agent engineer-b --toon   # Agent B claims task 2 (different task)
imi next --agent engineer-c --toon   # Agent C claims task 3
```

If a task is abandoned, IMI auto-releases it after 30 minutes. The next agent picks it up with full failure context.
