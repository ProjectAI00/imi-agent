#!/usr/bin/env bash
# ────────────────────────────────────────────────────────────
# IMI CLI – Comprehensive Integration Tests
# Runs every command end-to-end against a temp SQLite DB.
# No external test framework required.
# ────────────────────────────────────────────────────────────
set -euo pipefail

# ── Binary location ──────────────────────────────────────────
IMI_BIN="${IMI_BIN:-/Users/aimar/Documents/Kitchen/imi/ai-db-imi/target/release/imi}"

if [[ ! -x "$IMI_BIN" ]]; then
  echo "ERROR: imi binary not found (or not executable) at: $IMI_BIN"
  echo "       Build it first:  cargo build --release"
  echo "       Or override:     IMI_BIN=/path/to/imi ./tests/integration.sh"
  exit 1
fi

# ── Temp workspace ───────────────────────────────────────────
TEST_DIR=$(mktemp -d "/tmp/imi-rust-test-$$-XXXXXX")
export IMI_DB="$TEST_DIR/state.db"

cleanup() { rm -rf "$TEST_DIR"; }
trap cleanup EXIT

# ── Counters ─────────────────────────────────────────────────
PASS=0
FAIL=0
TOTAL=0

# ── Helpers ──────────────────────────────────────────────────
pass() {
  PASS=$((PASS + 1)); TOTAL=$((TOTAL + 1))
  printf "  \033[32mPASS\033[0m  %s\n" "$1"
}

fail() {
  FAIL=$((FAIL + 1)); TOTAL=$((TOTAL + 1))
  printf "  \033[31mFAIL\033[0m  %s\n" "$1"
  [[ -n "${2:-}" ]] && printf "        → %s\n" "$2"
}

# Run command, capture stdout+stderr and exit code.
# Sets: CMD_OUT, CMD_EXIT
run() {
  CMD_OUT=""
  CMD_EXIT=0
  CMD_OUT=$("$IMI_BIN" "$@" 2>&1) || CMD_EXIT=$?
}

# Assert exit code equals expected.
assert_exit() {
  local label="$1" expected="$2"
  if [[ "$CMD_EXIT" -eq "$expected" ]]; then
    pass "$label (exit=$expected)"
  else
    fail "$label (expected exit=$expected, got exit=$CMD_EXIT)" "$CMD_OUT"
  fi
}

# Assert stdout contains substring (case-insensitive).
assert_contains() {
  local label="$1" substring="$2"
  if echo "$CMD_OUT" | grep -qi "$substring"; then
    pass "$label (contains '$substring')"
  else
    fail "$label (missing '$substring')" "output: $CMD_OUT"
  fi
}

# Assert stdout does NOT contain substring.
assert_not_contains() {
  local label="$1" substring="$2"
  if echo "$CMD_OUT" | grep -qi "$substring"; then
    fail "$label (should NOT contain '$substring')" "output: $CMD_OUT"
  else
    pass "$label (does not contain '$substring')"
  fi
}

# Assert stdout starts with prefix.
assert_starts_with() {
  local label="$1" prefix="$2"
  if [[ "$CMD_OUT" == "$prefix"* ]]; then
    pass "$label (starts with '$prefix')"
  else
    fail "$label (does not start with '$prefix')" "output starts: ${CMD_OUT:0:80}"
  fi
}

# Query the DB directly. Sets DB_OUT.
db_query() {
  DB_OUT=$(sqlite3 "$IMI_DB" "$1" 2>&1) || true
}

# ── Banner ───────────────────────────────────────────────────
echo ""
echo "╔══════════════════════════════════════════════════════╗"
echo "║        IMI CLI Integration Tests                    ║"
echo "╠══════════════════════════════════════════════════════╣"
printf "║  Binary:  %-42s║\n" "$IMI_BIN"
printf "║  DB:      %-42s║\n" "$IMI_DB"
printf "║  Temp:    %-42s║\n" "$TEST_DIR"
echo "╚══════════════════════════════════════════════════════╝"
echo ""

# ═════════════════════════════════════════════════════════════
# 1. VERSION
# ═════════════════════════════════════════════════════════════
echo "── 1. Version ──────────────────────────────────────────"

run --version
assert_exit   "imi --version exits 0"   0
assert_contains "imi --version output"   "imi"
assert_contains "imi --version semver"   "0.3.4"

# ═════════════════════════════════════════════════════════════
# 2. INIT
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 2. Init ───────────────────────────────────────────"

run init
assert_exit   "imi init exits 0"   0

if [[ -f "$IMI_DB" ]]; then
  pass "imi init creates DB file"
else
  fail "imi init creates DB file" "DB not found at $IMI_DB"
fi

# Verify tables exist
db_query ".tables"
if echo "$DB_OUT" | grep -q "goals"; then
  pass "imi init creates goals table"
else
  fail "imi init creates goals table" "tables: $DB_OUT"
fi

if echo "$DB_OUT" | grep -q "tasks"; then
  pass "imi init creates tasks table"
else
  fail "imi init creates tasks table" "tables: $DB_OUT"
fi

# Double init should be idempotent
run init
assert_exit "imi init (idempotent)" 0

# ═════════════════════════════════════════════════════════════
# 3. ADD-GOAL
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 3. Add Goal ─────────────────────────────────────────"

run add-goal "Ship auth" "Critical for launch"
assert_exit     "add-goal exits 0"        0
assert_contains "add-goal prints id"      "goal"

# Extract goal_id from output (look for something like goal[xxxx] or a UUID-like id)
GOAL_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -z "$GOAL_ID" ]]; then
  # Fallback: try to get from DB
  db_query "SELECT id FROM goals ORDER BY rowid DESC LIMIT 1;"
  GOAL_ID="$DB_OUT"
fi

if [[ -n "$GOAL_ID" ]]; then
  pass "add-goal produced goal_id: $GOAL_ID"
else
  fail "add-goal: could not capture goal_id" "$CMD_OUT"
  GOAL_ID="UNKNOWN"
fi

# Verify goal in DB
db_query "SELECT name FROM goals WHERE id='$GOAL_ID';"
if [[ "$DB_OUT" == *"Ship auth"* ]]; then
  pass "add-goal: goal persisted in DB"
else
  fail "add-goal: goal not found in DB" "DB said: $DB_OUT"
fi

# ═════════════════════════════════════════════════════════════
# 4. STATUS (goals visible)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 4. Status (goals) ───────────────────────────────────"

run status
assert_exit     "status exits 0"              0
assert_contains "status lists 'Ship auth'"    "Ship auth"

# ═════════════════════════════════════════════════════════════
# 5. ADD-TASK
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 5. Add Task ─────────────────────────────────────────"

run add-task "$GOAL_ID" "Implement JWT" "Add JWT token signing and verification"
assert_exit     "add-task exits 0"     0
assert_contains "add-task prints id"   "task"

TASK_ID_JWT=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -z "$TASK_ID_JWT" ]]; then
  db_query "SELECT id FROM tasks ORDER BY rowid DESC LIMIT 1;"
  TASK_ID_JWT="$DB_OUT"
fi

if [[ -n "$TASK_ID_JWT" ]]; then
  pass "add-task produced task_id: $TASK_ID_JWT"
else
  fail "add-task: could not capture task_id" "$CMD_OUT"
  TASK_ID_JWT="UNKNOWN"
fi

# Add a second task for richer testing
run add-task "$GOAL_ID" "Write auth tests" "Unit and integration tests for auth module"
TASK_ID_TESTS=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -z "$TASK_ID_TESTS" ]]; then
  db_query "SELECT id FROM tasks ORDER BY rowid DESC LIMIT 1;"
  TASK_ID_TESTS="$DB_OUT"
fi

# ═════════════════════════════════════════════════════════════
# 6. STATUS (tasks visible)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 6. Status (tasks) ───────────────────────────────────"

run status
assert_exit     "status exits 0"                  0
assert_contains "status lists 'Implement JWT'"    "Implement JWT"
assert_contains "status lists 'Write auth tests'" "Write auth tests"

# ═════════════════════════════════════════════════════════════
# 7. NEXT (claim a task)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 7. Next ─────────────────────────────────────────────"

run next --toon
assert_exit       "next --toon exits 0"       0
assert_starts_with "next --toon starts with task[" "task["

# Capture the task_id returned by next
NEXT_TASK_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -n "$NEXT_TASK_ID" ]]; then
  pass "next --toon returned task_id: $NEXT_TASK_ID"
else
  fail "next --toon: could not capture task_id" "$CMD_OUT"
  NEXT_TASK_ID="$TASK_ID_JWT"
fi

# Verify task status changed to in_progress in DB
db_query "SELECT status FROM tasks WHERE id='$NEXT_TASK_ID';"
if [[ "$DB_OUT" == *"in_progress"* ]]; then
  pass "next: task status = in_progress in DB"
else
  fail "next: task status should be in_progress" "DB said: $DB_OUT"
fi

# ═════════════════════════════════════════════════════════════
# 8. PING
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 8. Ping ─────────────────────────────────────────────"

run ping "$NEXT_TASK_ID"
assert_exit "ping exits 0" 0

# ═════════════════════════════════════════════════════════════
# 9. COMPLETE
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 9. Complete ─────────────────────────────────────────"

run complete "$NEXT_TASK_ID" "done it"
assert_exit "complete exits 0" 0

# Verify task status changed to done in DB
db_query "SELECT status FROM tasks WHERE id='$NEXT_TASK_ID';"
if [[ "$DB_OUT" == *"done"* ]]; then
  pass "complete: task status = done in DB"
else
  fail "complete: task status should be done" "DB said: $DB_OUT"
fi

# ═════════════════════════════════════════════════════════════
# 9B. WRAP (runtime lifecycle automation)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 9B. Wrap ────────────────────────────────────────────"

run add-task "$GOAL_ID" "Autopilot wrapper success" "Run a command under IMI wrapper"
WRAP_TASK_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -z "$WRAP_TASK_ID" ]]; then
  db_query "SELECT id FROM tasks ORDER BY rowid DESC LIMIT 1;"
  WRAP_TASK_ID="$DB_OUT"
fi

run wrap "$WRAP_TASK_ID" --ping-secs 1 --checkpoint-secs 2 -- bash -lc "sleep 3"
assert_exit "wrap success exits 0" 0

db_query "SELECT status FROM tasks WHERE id='$WRAP_TASK_ID';"
if [[ "$DB_OUT" == "done" ]]; then
  pass "wrap success: task status = done"
else
  fail "wrap success: expected done status" "DB said: $DB_OUT"
fi

db_query "SELECT COUNT(*) FROM memories WHERE task_id='$WRAP_TASK_ID' AND key='checkpoint';"
if [[ "${DB_OUT:-0}" -ge 1 ]]; then
  pass "wrap success: checkpoint memory written"
else
  fail "wrap success: checkpoint memory missing" "DB said: $DB_OUT"
fi

run add-task "$GOAL_ID" "Autopilot wrapper failure" "Fail command under wrapper"
WRAP_FAIL_TASK_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -z "$WRAP_FAIL_TASK_ID" ]]; then
  db_query "SELECT id FROM tasks ORDER BY rowid DESC LIMIT 1;"
  WRAP_FAIL_TASK_ID="$DB_OUT"
fi

run wrap "$WRAP_FAIL_TASK_ID" --ping-secs 1 --checkpoint-secs 0 -- bash -lc "exit 7"
assert_exit "wrap failure exits 1" 1

db_query "SELECT status FROM tasks WHERE id='$WRAP_FAIL_TASK_ID';"
if [[ "$DB_OUT" == "todo" ]]; then
  pass "wrap failure: task status reset to todo"
else
  fail "wrap failure: expected todo status" "DB said: $DB_OUT"
fi

db_query "SELECT COUNT(*) FROM memories WHERE task_id='$WRAP_FAIL_TASK_ID' AND key='failure_reason';"
if [[ "${DB_OUT:-0}" -ge 1 ]]; then
  pass "wrap failure: failure memory written"
else
  fail "wrap failure: failure memory missing" "DB said: $DB_OUT"
fi

run complete "$WRAP_FAIL_TASK_ID" "wrapped failure path verified"
assert_exit "wrap failure cleanup complete exits 0" 0

# ═════════════════════════════════════════════════════════════
# 9C. ORCHESTRATE (parallel worker loop)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 9C. Orchestrate ─────────────────────────────────────"

run add-goal "Orchestrate goal" "Parallel execution loop"
ORCH_GOAL_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -z "$ORCH_GOAL_ID" ]]; then
  db_query "SELECT id FROM goals ORDER BY rowid DESC LIMIT 1;"
  ORCH_GOAL_ID="$DB_OUT"
fi

run add-task "$ORCH_GOAL_ID" "orchestrate task 1" "parallel worker one"
run add-task "$ORCH_GOAL_ID" "orchestrate task 2" "parallel worker two"

run orchestrate "$ORCH_GOAL_ID" --workers 2 --max-tasks 2 --agent-prefix bench --ping-secs 1 --checkpoint-secs 1 -- bash -lc "sleep 2"
assert_exit "orchestrate exits 0" 0

db_query "SELECT COUNT(*) FROM tasks WHERE goal_id='$ORCH_GOAL_ID' AND status='done';"
if [[ "$DB_OUT" == "2" ]]; then
  pass "orchestrate: all goal tasks completed"
else
  fail "orchestrate: expected 2 done tasks" "DB said: $DB_OUT"
fi

db_query "SELECT COUNT(*) FROM memories WHERE task_id IN (SELECT id FROM tasks WHERE goal_id='$ORCH_GOAL_ID') AND key='completion_summary';"
if [[ "${DB_OUT:-0}" -ge 2 ]]; then
  pass "orchestrate: completion summaries written"
else
  fail "orchestrate: completion summaries missing" "DB said: $DB_OUT"
fi

# ─────────────────────────────────────────────────────────────
# 9D. ORCHESTRATE --cli auto (auto-selection)
# ─────────────────────────────────────────────────────────────
echo ""
echo "── 9D. Orchestrate --cli auto ──────────────────────────"

run add-goal "Auto-select goal" "Verify --cli auto selects correct agent CLI"
AUTO_GOAL_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -z "$AUTO_GOAL_ID" ]]; then
  db_query "SELECT id FROM goals ORDER BY rowid DESC LIMIT 1;"
  AUTO_GOAL_ID="$DB_OUT"
fi

run add-task "$AUTO_GOAL_ID" "auto-select task 1" "auto worker one"
run add-task "$AUTO_GOAL_ID" "auto-select task 2" "auto worker two"

# Create a fake 'claude' binary that simply exits 0 (simulates a real agent run).
# $FAKE_BIN_DIR lives inside $TEST_DIR which is removed by the EXIT trap above.
FAKE_BIN_DIR="$TEST_DIR/fake-bin"
mkdir -p "$FAKE_BIN_DIR"
cat > "$FAKE_BIN_DIR/claude" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod +x "$FAKE_BIN_DIR/claude"

# With CLAUDE_CODE_ENTRYPOINT set, --cli auto should select the 'claude' path.
# The resolved command becomes: sh -c 'claude -p "$(cat "$IMI_TASK_CONTEXT_FILE")" ...'
# Our fake claude ignores arguments and exits 0, so wrap auto-completes the task.
CLAUDE_CODE_ENTRYPOINT=1 PATH="$FAKE_BIN_DIR:$PATH" \
  "$IMI_BIN" orchestrate "$AUTO_GOAL_ID" \
    --workers 2 --max-tasks 2 --cli auto --agent-prefix auto-bench --ping-secs 1 --checkpoint-secs 1 \
  2>&1 && CMD_EXIT=0 || CMD_EXIT=$?
if [[ "$CMD_EXIT" -eq 0 ]]; then
  pass "orchestrate --cli auto (CLAUDE_CODE_ENTRYPOINT) exits 0"
else
  fail "orchestrate --cli auto (CLAUDE_CODE_ENTRYPOINT) exited $CMD_EXIT"
fi

db_query "SELECT COUNT(*) FROM tasks WHERE goal_id='$AUTO_GOAL_ID' AND status='done';"
if [[ "$DB_OUT" == "2" ]]; then
  pass "orchestrate --cli auto: all tasks completed"
else
  fail "orchestrate --cli auto: expected 2 done tasks" "DB said: $DB_OUT"
fi

# With no agent env vars, --cli auto should fall back to hankweave (imi run).
# Use --max-tasks 0 so no workers are launched — just verifies the flag is accepted.
run add-goal "Auto-select fallback" "Verify --cli auto with no env vars"
AUTO_FB_GOAL_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -z "$AUTO_FB_GOAL_ID" ]]; then
  db_query "SELECT id FROM goals ORDER BY rowid DESC LIMIT 1;"
  AUTO_FB_GOAL_ID="$DB_OUT"
fi
(
  # Unset all env vars documented in README's auto-detection table.
  unset CLAUDE_CODE_SSE_PORT CLAUDE_CODE_ENTRYPOINT OPENCODE_SESSION GH_COPILOT_SESSION_ID COPILOT_AGENT_SESSION
  "$IMI_BIN" orchestrate "$AUTO_FB_GOAL_ID" --workers 1 --max-tasks 0 --cli auto
) && CMD_EXIT=0 || CMD_EXIT=$?
if [[ "$CMD_EXIT" -eq 0 ]]; then
  pass "orchestrate --cli auto (no env vars fallback) exits 0"
else
  fail "orchestrate --cli auto (no env vars fallback) exited $CMD_EXIT"
fi

# ═════════════════════════════════════════════════════════════
# 10. MEMORY ADD + LIST
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 10. Memory ──────────────────────────────────────────"

run memory add "$GOAL_ID" test_key "test value for memory"
assert_exit     "memory add exits 0"   0

run memory list
assert_exit     "memory list exits 0"         0
assert_contains "memory list shows test_key"  "test_key"

# Add a second memory to verify list shows multiple
run memory add "$GOAL_ID" auth_pattern "Use RS256 for JWT signing"
assert_exit "memory add (2nd) exits 0" 0

run memory list
assert_contains "memory list shows auth_pattern" "auth_pattern"

# ═════════════════════════════════════════════════════════════
# 11. DECIDE
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 11. Decide ──────────────────────────────────────────"

run decide "Use RS256" "key rotation is easier"
assert_exit "decide exits 0" 0

# Verify decision stored in DB
db_query "SELECT what FROM decisions ORDER BY rowid DESC LIMIT 1;"
if [[ "$DB_OUT" == *"RS256"* ]]; then
  pass "decide: decision persisted in DB"
else
  fail "decide: decision not found in DB" "DB said: $DB_OUT"
fi

# ═════════════════════════════════════════════════════════════
# 12. LOG
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 12. Log ─────────────────────────────────────────────"

run log "direction note about architecture"
assert_exit "log exits 0" 0

# ═════════════════════════════════════════════════════════════
# 13. CONTEXT
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 13. Context ─────────────────────────────────────────"

run context --toon
assert_exit     "context --toon exits 0"           0
# Context should surface decisions and/or memories
assert_contains "context shows decision content"   "RS256"

# ═════════════════════════════════════════════════════════════
# 14. STATUS
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 14. Status ──────────────────────────────────────────"

run status
assert_exit     "status exits 0"              0
# Status should show the goal or task info
assert_contains "status shows goal"           "Ship auth"

# ═════════════════════════════════════════════════════════════
# 15. STATS
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 15. Stats ───────────────────────────────────────────"

run stats
assert_exit     "stats exits 0"   0
# Stats should show some completion info
assert_contains "stats shows completion" "completion"

# ═════════════════════════════════════════════════════════════
# 16. FAIL (critical bug test)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 16. Fail (critical) ─────────────────────────────────"

# First, claim the second task so we can fail it
run next --toon
FAIL_TASK_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)

if [[ -n "$FAIL_TASK_ID" ]]; then
  # Verify it's in_progress before failing
  db_query "SELECT status FROM tasks WHERE id='$FAIL_TASK_ID';"
  if [[ "$DB_OUT" == *"in_progress"* ]]; then
    pass "fail setup: task is in_progress"
  else
    fail "fail setup: task should be in_progress" "DB said: $DB_OUT"
  fi

  run fail "$FAIL_TASK_ID" "requirements changed, need redesign"
  assert_exit "fail exits 0" 0

  # CRITICAL: After fail, task must revert to 'todo' and agent_id must be NULL
  db_query "SELECT status FROM tasks WHERE id='$FAIL_TASK_ID';"
  if [[ "$DB_OUT" == "todo" ]]; then
    pass "fail: task status reverted to 'todo' (CRITICAL)"
  else
    fail "fail: task status should be 'todo' (CRITICAL BUG)" "DB said: $DB_OUT"
  fi

  db_query "SELECT agent_id FROM tasks WHERE id='$FAIL_TASK_ID';"
  if [[ -z "$DB_OUT" || "$DB_OUT" == "" ]]; then
    pass "fail: agent_id is NULL (CRITICAL)"
  else
    fail "fail: agent_id should be NULL (CRITICAL BUG)" "DB said: $DB_OUT"
  fi
else
  fail "fail: no task available to fail" "$CMD_OUT"
fi

# ═════════════════════════════════════════════════════════════
# 17. NEXT with --agent (critical test)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 17. Next --agent (critical) ─────────────────────────"

run next --agent agent-alpha --toon
assert_exit       "next --agent exits 0"     0
assert_starts_with "next --agent starts with task[" "task["

AGENT_TASK_ID=$(echo "$CMD_OUT" | grep -oE '[a-z0-9]{14,}' | head -1)
if [[ -n "$AGENT_TASK_ID" ]]; then
  db_query "SELECT agent_id FROM tasks WHERE id='$AGENT_TASK_ID';"
  if [[ "$DB_OUT" == "agent-alpha" ]]; then
    pass "next --agent: agent_id = 'agent-alpha' in DB (CRITICAL)"
  else
    fail "next --agent: agent_id should be 'agent-alpha' (CRITICAL)" "DB said: $DB_OUT"
  fi
else
  fail "next --agent: could not capture task_id" "$CMD_OUT"
fi

# Complete the agent-claimed task to clean up
if [[ -n "$AGENT_TASK_ID" ]]; then
  run complete "$AGENT_TASK_ID" "agent-alpha finished this"
fi

# ═════════════════════════════════════════════════════════════
# 18. NEXT with no tasks available (critical test)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 18. Next (no tasks) ─────────────────────────────────"

# All tasks should now be done. next should handle gracefully.
run next --toon
assert_exit     "next (no tasks) exits 0"          0
assert_contains "next (no tasks) says no_tasks"    "no_tasks"

# ═════════════════════════════════════════════════════════════
# 19. FAIL with fake ID (critical error handling)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 19. Error handling ──────────────────────────────────"

run fail fake-id-xyz "reason for failing"
assert_exit     "fail fake-id exits 1"             1
assert_contains "fail fake-id prints error"        "error\|not found\|Error\|invalid"

run complete fake-id-xyz "summary"
assert_exit     "complete fake-id exits 1"         1
assert_contains "complete fake-id prints error"    "error\|not found\|Error\|invalid"

# ═════════════════════════════════════════════════════════════
# 20. EDGE CASES
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 20. Edge cases ──────────────────────────────────────"

# Goal with special characters in title
run add-goal "Fix <html> & \"quotes\"" "edge case test"
assert_exit "add-goal with special chars exits 0" 0

# Task with empty-ish description
run add-task "$GOAL_ID" "Quick fix" ""
assert_exit "add-task with empty desc exits 0" 0

# Memory with long value
LONG_VAL="This is a deliberately long memory value that tests whether the system can handle somewhat lengthy strings without truncation or corruption of data in the SQLite database"
run memory add "$GOAL_ID" long_memory "$LONG_VAL"
assert_exit "memory add (long value) exits 0" 0

run memory list
assert_contains "memory list shows long_memory" "long_memory"

# Ping a completed task (should not crash)
run ping "$NEXT_TASK_ID"
# We just care it doesn't crash with a segfault or panic
if [[ "$CMD_EXIT" -le 1 ]]; then
  pass "ping completed task: no crash (exit=$CMD_EXIT)"
else
  fail "ping completed task: unexpected exit code" "exit=$CMD_EXIT"
fi

# ═════════════════════════════════════════════════════════════
# 21. DB INTEGRITY
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 21. DB integrity ────────────────────────────────────"

# Run SQLite integrity check
db_query "PRAGMA integrity_check;"
if [[ "$DB_OUT" == "ok" ]]; then
  pass "SQLite integrity check: ok"
else
  fail "SQLite integrity check failed" "$DB_OUT"
fi

# Verify foreign key relationships (goals exist for all tasks)
db_query "SELECT COUNT(*) FROM tasks t LEFT JOIN goals g ON t.goal_id = g.id WHERE g.id IS NULL;"
if [[ "$DB_OUT" == "0" ]]; then
  pass "All tasks reference valid goals"
else
  fail "Orphaned tasks found" "count: $DB_OUT"
fi

# ═════════════════════════════════════════════════════════════
# 22. PARALLEL PERFORMANCE
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 22. Parallel performance ────────────────────────────"

# Run 8 read commands in parallel, measure wall time
PERF_START=$(date +%s%N)
(
  "$IMI_BIN" status  > /dev/null 2>&1 &
  "$IMI_BIN" audit   > /dev/null 2>&1 &
  "$IMI_BIN" context > /dev/null 2>&1 &
  "$IMI_BIN" plan    > /dev/null 2>&1 &
  "$IMI_BIN" status  > /dev/null 2>&1 &
  "$IMI_BIN" audit   > /dev/null 2>&1 &
  "$IMI_BIN" context > /dev/null 2>&1 &
  "$IMI_BIN" plan    > /dev/null 2>&1 &
  wait
)
PERF_END=$(date +%s%N)
PERF_MS=$(( (PERF_END - PERF_START) / 1000000 ))

if [[ "$PERF_MS" -lt 500 ]]; then
  pass "8 parallel read commands completed in ${PERF_MS}ms (< 500ms)"
else
  fail "8 parallel read commands too slow: ${PERF_MS}ms" "expected < 500ms"
fi

# Sequential baseline for comparison
SEQ_START=$(date +%s%N)
"$IMI_BIN" status  > /dev/null 2>&1
"$IMI_BIN" audit   > /dev/null 2>&1
"$IMI_BIN" context > /dev/null 2>&1
"$IMI_BIN" plan    > /dev/null 2>&1
SEQ_END=$(date +%s%N)
SEQ_MS=$(( (SEQ_END - SEQ_START) / 1000000 ))
pass "4 sequential commands baseline: ${SEQ_MS}ms (parallel ran 8 in ${PERF_MS}ms)"

# ═════════════════════════════════════════════════════════════
# 23. SKILL INSTALL PATHS (new vs old install)
# ═════════════════════════════════════════════════════════════
echo ""
echo "── 23. Skill install paths ─────────────────────────────"

SKILL_SRC="$(dirname "$0")/../skills/imi/SKILL.md"
SKILL_SCRIPT_SRC="$(dirname "$0")/../skills/imi/scripts/session-start.sh"

if [[ -f "$SKILL_SRC" ]]; then
  pass "SKILL.md exists at expected path"
else
  fail "SKILL.md missing" "$SKILL_SRC"
fi

if [[ -f "$SKILL_SCRIPT_SRC" ]]; then
  pass "session-start.sh exists"
else
  fail "session-start.sh missing" "$SKILL_SCRIPT_SRC"
fi

# Verify SKILL.md frontmatter has required fields
if grep -q '^name: imi' "$SKILL_SRC" && grep -q '^description:' "$SKILL_SRC"; then
  pass "SKILL.md has required frontmatter (name + description)"
else
  fail "SKILL.md missing required frontmatter fields"
fi

if grep -q 'allowed-tools:' "$SKILL_SRC"; then
  pass "SKILL.md has allowed-tools (pre-approves imi commands)"
else
  fail "SKILL.md missing allowed-tools — agents will prompt for permission"
fi

# Simulate new install: copy skill to a temp dir, verify it lands correctly
FAKE_COPILOT="$TEST_DIR/fake-copilot"
mkdir -p "$FAKE_COPILOT"
SKILL_DEST="$FAKE_COPILOT/skills/imi"
mkdir -p "$SKILL_DEST"
cp "$SKILL_SRC" "$SKILL_DEST/SKILL.md"

if [[ -f "$SKILL_DEST/SKILL.md" ]]; then
  pass "skill install simulation: SKILL.md written to target dir"
else
  fail "skill install simulation: file not found after copy"
fi

# Simulate old install: no skills dir — ensure binary still works alone
run status
assert_exit "binary works standalone (old install path)" 0

# ═════════════════════════════════════════════════════════════
# SUMMARY

# ═════════════════════════════════════════════════════════════
echo ""
echo "══════════════════════════════════════════════════════════"
if [[ "$FAIL" -eq 0 ]]; then
  printf "\033[32m  ALL PASSED: %d/%d tests\033[0m\n" "$PASS" "$TOTAL"
else
  printf "\033[31m  FAILED: %d  |  PASSED: %d  |  TOTAL: %d\033[0m\n" "$FAIL" "$PASS" "$TOTAL"
fi
echo "══════════════════════════════════════════════════════════"
echo ""

exit "$FAIL"
