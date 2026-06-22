# Dead Code Inventory

Produced by scanning `src/main.rs`, `scripts/`, `npm/skills/`, `skills/`, `prompts/`, and `tests/` for items with no imports, calls, or runtime/test references.

Each candidate is labeled **SAFE** (confirmed non-reachable; evidence is conclusive) or **UNCERTAIN** (possible non-use but needs manual verification before deletion).

---

## SAFE — Confirmed dead code

### 1. `src/main.rs` — function `cmd_goals` (lines 1116–1176)

**Evidence of non-use:**
- `cargo check` emits: `warning: function 'cmd_goals' is never used`
- No `Commands::Goals` variant exists anywhere in the `Commands` enum — the CLI subcommand was never wired up
- The `dispatch()` function in `src/main.rs` has no arm for a Goals command
- No test in `tests/integration.sh`, `tests/eval-loop.sh`, or `tests/eval-human-sim.sh` invokes `imi goals`
- `grep -n "cmd_goals" src/main.rs` returns only the function definition at line 1116; zero call sites

**What it does:** Lists active or archived goals in human/JSON/toon format. Functionality is now covered by `imi plan`, `imi context`, and `imi status`.

**Safe to remove:** yes — dead since the Goals subcommand was removed from the CLI surface.

---

### 2. `src/main.rs` — function `cmd_tasks` (lines 1190–1287)

**Evidence of non-use:**
- `cargo check` emits: `warning: function 'cmd_tasks' is never used`
- No `Commands::Tasks` variant exists in the `Commands` enum
- The `dispatch()` function has no arm for a Tasks command
- No test in any test file invokes `imi tasks`
- `grep -n "cmd_tasks" src/main.rs` returns only the function definition at line 1190; zero call sites

**What it does:** Lists tasks filtered by status or goal prefix in human/JSON/toon format. Functionality is now covered by `imi plan`, `imi context <goal_id>`, and `imi status`.

**Safe to remove:** yes — dead since the Tasks subcommand was removed from the CLI surface.

---

## UNCERTAIN — Needs manual review

### 3. `tests/eval-loop.sh`

**Evidence of possible non-use:**
- Not listed in the `Makefile` `test` target (only `tests/integration.sh` runs via `make test`)
- Not referenced by the CI release workflow (`.github/workflows/release.yml`)
- Not mentioned in `README.md`
- Binary path is hardcoded to an absolute local path (`/Users/aimar/Documents/Kitchen/imi/ai-db-imi/target/release/imi`); falls back to `command -v imi` — the hardcoded path is a developer machine path that does not exist in CI

**Why it may be intentional:**
- The 5-scenario end-to-end loop simulations (cold-start, goal/task lifecycle, session-start script) cover distinct flows not explicitly tested in `integration.sh`
- References `scripts/session-start.sh`, validating that hook script works correctly
- The script does have an `${IMI_BIN:-...}` fallback, so it can run if the env var is set

**Recommendation:** Verify whether this test script is still actively maintained and should be added to `make test` or run in CI; otherwise it risks bit-rotting undetected.

---

### 4. `tests/eval-human-sim.sh`

**Evidence of possible non-use:**
- Not listed in the `Makefile` `test` target
- Not referenced by the CI release workflow
- Not mentioned in `README.md`
- No reference from any other file in the repository
- Binary path falls back to `$REPO_ROOT/target/release/imi`, which requires a prior build step not scripted anywhere for this file

**Why it may be intentional:**
- Covers human-simulation scenarios (goal/task creation from the human's perspective, context output, decision capture) at a higher level than `integration.sh`
- Could be a developer QA script kept alongside `eval-loop.sh` for manual verification

**Recommendation:** Either wire into `make test` / CI, or document its purpose in README so it doesn't silently diverge.

---

### 5. `skills/imi/scripts/session-start.sh` (top-level `skills/` directory)

**Evidence of possible non-use:**
- The `npm/run.ts` `installSkills()` function reads from `npm/skills/imi/` — it does **not** read from the top-level `skills/imi/` directory. Nothing in the npm install path touches this file.
- The top-level `skills/imi/` directory contains only `scripts/session-start.sh`; the SKILL.md and prompt `.md` files that a Claude Code plugin expects are absent here (they live in `npm/skills/imi/`).
- No hook or agent config file (`.claude/settings.json`, `.cursor/rules/`, `.opencode/instructions/`) references `skills/imi/scripts/session-start.sh`; those hooks reference `scripts/session-start.sh` (top-level) instead.

**Why it may be intentional:**
- `.claude-plugin/marketplace.json` lists `./skills/imi` as the skills source for the Claude Code plugin registration, suggesting this directory is the plugin's skill root. The session-start script content (`imi status` + `imi audit` in parallel) differs from `scripts/session-start.sh` (`imi status` + `imi context`), implying a distinct purpose.
- `tests/integration.sh` section 23 explicitly tests that `skills/imi/scripts/session-start.sh` exists and is syntactically valid — the test suite treats this file as a required artifact.

**Recommendation:** Clarify whether `skills/imi/` is the authoritative plugin skill root that should also contain SKILL.md and the prompt files (making it a copy/symlink of `npm/skills/imi/`), or whether the session-start script alone is intentional and the `marketplace.json` skills reference is stale.

---

## Active (included for reference — not dead)

| Path | Active reference(s) |
|---|---|
| `scripts/session-start.sh` | `.claude/settings.json` SessionStart hook; tested by `tests/eval-loop.sh` |
| `prompts/execute-mode.md` | `src/main.rs` lines 2367, 2467 — used as hankweave system prompt in `cmd_run` / `cmd_wrap`; documented in `README.md` |
| `prompts/plan-mode.md`, `prompts/ops-mode.md`, `prompts/ai-voice.md` | Documented in `README.md` as drop-in system prompts; part of the same prompts/ package as execute-mode.md |
| `npm/skills/imi/*.md` | Installed by `npm/run.ts` `installSkills()` to agent directories |
| `tests/integration.sh` | Run by `make test`; primary CI test suite |

---

## Summary

| Label | Item | Location |
|---|---|---|
| **SAFE** | `cmd_goals` function | `src/main.rs:1116–1176` |
| **SAFE** | `cmd_tasks` function | `src/main.rs:1190–1287` |
| **UNCERTAIN** | `eval-loop.sh` | `tests/eval-loop.sh` |
| **UNCERTAIN** | `eval-human-sim.sh` | `tests/eval-human-sim.sh` |
| **UNCERTAIN** | `skills/imi/scripts/session-start.sh` | `skills/imi/scripts/` |
