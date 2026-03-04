#!/usr/bin/env bash
set -u -o pipefail

IMI_BIN="${IMI_BIN:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/release/imi}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SESSION_START_SCRIPT="$REPO_ROOT/scripts/session-start.sh"

PASS=0
FAIL=0
TMP_DIRS=()
CMD_OUT=""
CMD_EXIT=0

cleanup() {
  for d in "${TMP_DIRS[@]:-}"; do
    [[ -n "$d" && -d "$d" ]] && rm -rf "$d"
  done
}
trap cleanup EXIT

mktemp_dir() {
  local d
  d=$(mktemp -d "/tmp/imi-eval-loop-XXXXXX")
  TMP_DIRS+=("$d")
  echo "$d"
}

run_in_dir() {
  local dir="$1"
  shift
  CMD_OUT="$(cd "$dir" && "$@" 2>&1)"
  CMD_EXIT=$?
}

capture_goal_id() {
  echo "$1" | grep -oE '[a-z0-9]{14,}' | head -1
}

scenario_pass() {
  PASS=$((PASS + 1))
  printf "\033[32mPASS\033[0m  %s\n" "$1"
}

scenario_fail() {
  FAIL=$((FAIL + 1))
  printf "\033[31mFAIL\033[0m  %s\n" "$1"
  [[ -n "${2:-}" ]] && printf "      expected: %s\n" "$2"
  [[ -n "${3:-}" ]] && printf "      got: %s\n" "$3"
}

echo ""
echo "=== IMI loop eval (5 scenarios) ==="
printf "Binary: %s\n" "$IMI_BIN"

if [[ ! -x "$IMI_BIN" ]]; then
  echo "FAIL  setup"
  echo "      expected: executable IMI binary at $IMI_BIN"
  echo "      got: missing or not executable"
  echo ""
  echo "0/5 scenarios passed"
  exit 1
fi

# 1) sim-cold-start
{
  local_ok=1
  reason=""
  got=""
  d=$(mktemp_dir)

  run_in_dir "$d" "$IMI_BIN" init
  if [[ "$CMD_EXIT" -ne 0 ]]; then
    local_ok=0
    reason="imi init exits 0"
    got="$CMD_OUT"
  fi

  if [[ "$local_ok" -eq 1 && ! -f "$d/.imi/state.db" ]]; then
    local_ok=0
    reason=".imi/state.db exists after init"
    got="missing $d/.imi/state.db"
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" context
    if [[ "$CMD_EXIT" -ne 0 || -z "$CMD_OUT" || "$CMD_OUT" =~ [Ee]rror ]]; then
      local_ok=0
      reason="context exits 0 with meaningful non-error output on empty state"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" status
    if [[ "$CMD_EXIT" -ne 0 || ! "$CMD_OUT" =~ [Gg]oals[[:space:]]+0 ]]; then
      local_ok=0
      reason="status shows 0 goals cleanly"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    scenario_pass "sim-cold-start"
  else
    scenario_fail "sim-cold-start" "$reason" "$got"
  fi
}

# 2) sim-warm-return
{
  local_ok=1
  reason=""
  got=""
  d=$(mktemp_dir)

  run_in_dir "$d" "$IMI_BIN" init
  if [[ "$CMD_EXIT" -ne 0 ]]; then
    local_ok=0
    reason="imi init exits 0"
    got="$CMD_OUT"
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" add-goal "build auth system" "secure auth baseline"
    GOAL_ID=$(capture_goal_id "$CMD_OUT")
    if [[ "$CMD_EXIT" -ne 0 || -z "$GOAL_ID" ]]; then
      local_ok=0
      reason="add-goal returns goal id"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" add-task "$GOAL_ID" "add login endpoint" "POST /login with password verification"
    if [[ "$CMD_EXIT" -ne 0 ]]; then
      local_ok=0
      reason="first add-task exits 0"
      got="$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" add-task "$GOAL_ID" "add JWT middleware" "verify bearer token and attach user context"
    if [[ "$CMD_EXIT" -ne 0 ]]; then
      local_ok=0
      reason="second add-task exits 0"
      got="$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" context
    if [[ "$CMD_EXIT" -ne 0 || ! "$CMD_OUT" =~ build\ auth\ system || ! "$CMD_OUT" =~ add\ login\ endpoint && ! "$CMD_OUT" =~ add\ JWT\ middleware ]]; then
      local_ok=0
      reason="context contains goal name and at least one task"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" next --toon
    if [[ "$CMD_EXIT" -ne 0 || ! "$CMD_OUT" =~ ^task\[ || ! "$CMD_OUT" =~ desc\[ || ! "$CMD_OUT" =~ goal\[ ]]; then
      local_ok=0
      reason="next --toon returns structured task context"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    scenario_pass "sim-warm-return"
  else
    scenario_fail "sim-warm-return" "$reason" "$got"
  fi
}

# 3) sim-task-completion-writeback
{
  local_ok=1
  reason=""
  got=""
  d=$(mktemp_dir)

  run_in_dir "$d" "$IMI_BIN" init
  if [[ "$CMD_EXIT" -ne 0 ]]; then
    local_ok=0
    reason="imi init exits 0"
    got="$CMD_OUT"
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" add-goal "build auth system" "deliver production-ready auth"
    GOAL_ID=$(capture_goal_id "$CMD_OUT")
    if [[ "$CMD_EXIT" -ne 0 || -z "$GOAL_ID" ]]; then
      local_ok=0
      reason="add-goal returns goal id"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" add-task "$GOAL_ID" "add login endpoint" "implement credential validation and session handling"
    if [[ "$CMD_EXIT" -ne 0 ]]; then
      local_ok=0
      reason="add-task exits 0"
      got="$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" next --toon
    TASK_ID=$(capture_goal_id "$CMD_OUT")
    if [[ "$CMD_EXIT" -ne 0 || -z "$TASK_ID" ]]; then
      local_ok=0
      reason="next claims a task and returns task id"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" complete "$TASK_ID" "implemented login endpoint with JWT, stored token in httpOnly cookie"
    if [[ "$CMD_EXIT" -ne 0 ]]; then
      local_ok=0
      reason="complete exits 0"
      got="$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    DB_STATUS=$(sqlite3 "$d/.imi/state.db" "SELECT status FROM tasks WHERE id='$TASK_ID';" 2>&1)
    if [[ "$DB_STATUS" != "done" ]]; then
      local_ok=0
      reason="task status is done in DB"
      got="$DB_STATUS"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    MEM_COUNT=$(sqlite3 "$d/.imi/state.db" "SELECT COUNT(*) FROM memories WHERE task_id='$TASK_ID';" 2>&1)
    if ! [[ "$MEM_COUNT" =~ ^[0-9]+$ ]] || [[ "$MEM_COUNT" -lt 1 ]]; then
      local_ok=0
      reason="completion writes at least one memory"
      got="$MEM_COUNT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" status
    if [[ "$CMD_EXIT" -ne 0 || ! "$CMD_OUT" =~ ✅1\ done ]]; then
      local_ok=0
      reason="status shows 1 done"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    scenario_pass "sim-task-completion-writeback"
  else
    scenario_fail "sim-task-completion-writeback" "$reason" "$got"
  fi
}

# 4) sim-natural-todo-capture
{
  local_ok=1
  reason=""
  got=""
  d=$(mktemp_dir)

  run_in_dir "$d" "$IMI_BIN" init
  if [[ "$CMD_EXIT" -ne 0 ]]; then
    local_ok=0
    reason="imi init exits 0"
    got="$CMD_OUT"
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" add-goal "build auth system" "capture natural todo items"
    GOAL_ID=$(capture_goal_id "$CMD_OUT")
    if [[ "$CMD_EXIT" -ne 0 || -z "$GOAL_ID" ]]; then
      local_ok=0
      reason="add-goal returns goal id"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  TASK_TITLE="capture login error analytics"
  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" task "$GOAL_ID" "$TASK_TITLE" "record auth failures with actionable tags"
    if [[ "$CMD_EXIT" -ne 0 ]]; then
      local_ok=0
      reason="imi task exits 0"
      got="$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" status
    if [[ "$CMD_EXIT" -ne 0 || ! "$CMD_OUT" =~ $TASK_TITLE ]]; then
      local_ok=0
      reason="new task appears in status"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  NOTE="focus login hardening before auth UX polish"
  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" log "$NOTE"
    if [[ "$CMD_EXIT" -ne 0 ]]; then
      local_ok=0
      reason="imi log exits 0"
      got="$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d" "$IMI_BIN" context
    if [[ "$CMD_EXIT" -ne 0 || ! "$CMD_OUT" =~ $NOTE ]]; then
      local_ok=0
      reason="direction note appears in context output"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    scenario_pass "sim-natural-todo-capture"
  else
    scenario_fail "sim-natural-todo-capture" "$reason" "$got"
  fi
}

# 5) sim-session-start-script
{
  local_ok=1
  reason=""
  got=""

  if [[ ! -f "$SESSION_START_SCRIPT" ]]; then
    local_ok=0
    reason="scripts/session-start.sh exists"
    got="missing $SESSION_START_SCRIPT"
  fi

  d_with=$(mktemp_dir)
  d_without=$(mktemp_dir)

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d_with" "$IMI_BIN" init
    if [[ "$CMD_EXIT" -ne 0 ]]; then
      local_ok=0
      reason="setup init for populated dir exits 0"
      got="$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d_with" env PATH="$(dirname "$IMI_BIN"):$PATH" bash "$SESSION_START_SCRIPT"
    if [[ "$CMD_EXIT" -ne 0 || -z "$CMD_OUT" ]]; then
      local_ok=0
      reason="session-start exits 0 with output in populated .imi project"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    run_in_dir "$d_without" env PATH="$(dirname "$IMI_BIN"):$PATH" bash "$SESSION_START_SCRIPT"
    if [[ "$CMD_EXIT" -ne 0 || -z "$CMD_OUT" ]]; then
      local_ok=0
      reason="session-start exits 0 with output in dir without .imi"
      got="exit=$CMD_EXIT output=$CMD_OUT"
    fi
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    scenario_pass "sim-session-start-script"
  else
    scenario_fail "sim-session-start-script" "$reason" "$got"
  fi
}

echo ""
echo "$PASS/5 scenarios passed"

if [[ "$FAIL" -gt 0 ]]; then
  exit 1
fi
