# IMI

IMI is the AI product manager for AI agents.

You open Claude Code in the morning and your agent has no idea what happened yesterday. You re-explain the codebase, the decisions, the blockers, and what matters now. Then you do it again in the next session. IMI fixes that by keeping the thinking layer inside your codebase so every session starts with context instead of drift.

Website: https://useimi.com

## What IMI does

IMI stores the parts agents forget:

- what you're building
- why you're building it
- what decisions were made
- what got blocked
- what changed between sessions

That gives you a simple loop:

1. You steer in natural language.
2. IMI stores the intent, decisions, and direction.
3. Your next agent session picks up from there without a re-brief.

IMI is not another task board. It is the PM layer that sits between you and your coding agents.

## Install

### Option 1 — standalone CLI

```bash
bunx imi-agent@latest
```

That downloads the binary and runs `imi init` in your project.

Or via curl:

```bash
curl -fsSL https://useimi.com/install | bash
```

Make sure `~/.local/bin` is in your `$PATH`:

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.zshrc && source ~/.zshrc
```

### Option 2 — plugin / skill install

Install the IMI plugin into your agent CLI so the session gets IMI context automatically.

**Claude Code**

```bash
/plugin marketplace add ProjectAI00/imi-agent
/plugin install imi
```

**GitHub Copilot CLI**

```bash
/plugin marketplace add ProjectAI00/imi-agent
/plugin install imi
```

**Manual skill install**

```bash
npx skills add ProjectAI00/imi-agent@imi
```

## Start here

These are the commands that matter most when you want to understand the project and steer it:

```bash
imi context   # what we're building, active work, key decisions
imi think     # are we still building the right thing?
imi plan      # full goals and task list
imi check     # verification state
```

To capture human thinking directly:

```bash
imi decide "what" "why"   # firm decision with reasoning
imi log "note"            # direction, observation, or concern
```

In practice, the human should mostly talk to their agent in natural language. The agent reads IMI, writes back to IMI, and keeps the project aligned without making the human manage command syntax all day.

## The product in one sentence

IMI remembers your goals, decisions, blockers, and project direction so your agents can resume work across sessions, teammates, and tools without starting from zero.

## How people use it

Ask your agent things like:

- what are we building?
- how is it going?
- are we still aligned?
- what changed since yesterday?

Under the hood, the agent can use IMI to read context, reason over direction, and persist new decisions or notes back into the repo-local state.

## Optional execution layers

IMI is the state and alignment layer. Execution tools can plug in underneath it.

- **Hankweave** for task execution and checkpointing
- **Entire** for session audit and rewind
- **Claude Code, GitHub Copilot CLI, Cursor, Codex** as the session layer on top

The point is not which execution tool you use. The point is that the project context survives.

## Stack

- **Rust** — single binary
- **SQLite** — local persistent state in `.imi/state.db`
- **Works with** Claude Code, GitHub Copilot CLI, Cursor, Codex, and terminal-based agents

## Agent prompts

The repo includes prompts for the main IMI modes:

| File | Purpose |
|------|---------|
| `prompts/ops-mode.md` | status, alignment, and decision conversations |
| `prompts/plan-mode.md` | turning intent into goals and tasks |
| `prompts/execute-mode.md` | executing tasks and writing back useful summaries |

## World Model V1.5 Evaluation

The world-model v1.5 branch introduces memory+causality features to improve agent outcomes across sessions. The evaluation protocol ensures measurable, reproducible validation before promotion to main.

**Quick start:**
```bash
# Switch to world-model branch
git checkout world-model

# Run tuning phase benchmarks
./benchmarks/run_tuning.sh --mode=baseline --runs=10
./benchmarks/run_tuning.sh --mode=treatment --runs=10

# Run final evaluation (30 runs per family)
./benchmarks/run_final.sh --mode=baseline --runs=30 --seed-offset=1000
./benchmarks/run_final.sh --mode=treatment --runs=30 --seed-offset=1000

# Check promotion gate (>=5% improvement required)
./benchmarks/analyze.sh --phase=final --check-gate
```

**Full protocol:** See [docs/world-model-v1.5-evaluation-protocol.md](docs/world-model-v1.5-evaluation-protocol.md) for complete details on baseline/treatment definitions, benchmark families, fixed seeds, metrics, and artifact formats.


