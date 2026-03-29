# World Model V1.5 Evaluation Protocol

## Overview

This document defines the reproducible evaluation protocol for world-model v1.5. The protocol ensures all benchmark runs are comparable, reproducible, and resistant to overfitting or benchmark leakage.

**Primary goal:** Validate whether the world-model v1.5 memory+causality loop improves agent outcomes with measurable A/B benchmarks before investing in deeper model-training work.

**Promotion gate:** >=5% improvement on at least one benchmark family.

## Baseline vs Treatment

### Baseline Configuration
- **Agent mode:** Standard IMI v1 without world-model v1.5 features
- **Memory system:** SQLite-backed goals/tasks/decisions/lessons (existing IMI state.db)
- **Session continuity:** `imi context` command provides static context dump
- **No features:** No causal graph, no contradiction detection, no truth-status lifecycle, no retrieval scoring

### Treatment Configuration
- **Agent mode:** IMI with world-model v1.5 features enabled
- **Memory system:** Causal graph + truth-status tracking + SQLite state
- **Session continuity:** Enhanced `imi context` with causal retrieval and contradiction warnings
- **Features enabled:**
  - Session-to-causal extraction
  - Truth-status lifecycle (claimed → verified → stale → superseded)
  - Contradiction detection and resolution
  - Retrieval quality scoring
  - Human-steering signal mapping

## Benchmark Families

### 1. Repeat-Failure Mitigation
**Scenario:** Agent attempts a task that has failed before in similar context.

**Metrics:**
- Repeat-failure rate (% of tasks that fail with same root cause as prior attempt)
- Time-to-first-correct-approach (iterations before agent tries a new strategy)
- False-positive warnings (contradictions flagged incorrectly)

**Test cases:**
- Authentication refactor after prior OAuth failure
- Database migration after schema mismatch failure
- Dependency upgrade after conflict resolution failure

### 2. Context Continuity Across Sessions
**Scenario:** Multi-session tasks where context must persist accurately.

**Metrics:**
- Context-drift rate (% of decisions/constraints lost between sessions)
- Re-explanation frequency (how often agent re-asks questions)
- Stale-information usage (agent uses outdated facts)

**Test cases:**
- 3-session API redesign with evolving requirements
- 5-session refactor with incremental rollout decisions
- Cross-session bug fix where root cause discovered in session 2

### 3. Contradiction Detection Accuracy
**Scenario:** Agent receives conflicting information from different sources.

**Metrics:**
- True positive rate (actual contradictions flagged)
- False positive rate (benign differences flagged as contradictions)
- Resolution quality (% of contradictions resolved correctly)

**Test cases:**
- Deprecated API usage after migration decision
- Conflicting architecture decisions from different dates
- Stale file location after directory restructure

## Fixed Seeds and Reproducibility

### Seed Configuration
All benchmark runs use deterministic seeding:
- **LLM seed:** Fixed per run (e.g., `SEED=42` for baseline, `SEED=43` for treatment)
- **Random state:** Fixed initialization for any stochastic components
- **Timestamp lock:** Benchmark scenarios use fixed "current time" to avoid time-dependent drift

### Environment Control
- **IMI state:** Fresh `.imi/state.db` initialized from fixture per run
- **Filesystem:** Isolated worktree per benchmark run
- **Agent CLI:** Version-pinned dependencies (Claude Code, Copilot CLI, or mock harness)

### Reproducibility Checklist
- [ ] Fixed LLM seed set via environment variable
- [ ] `.imi/state.db` initialized from known fixture
- [ ] All timestamps in fixture data are deterministic
- [ ] No network calls (LLM responses mocked or fixed)
- [ ] Git worktree isolated per run
- [ ] Run ID logged with all outputs

## Run Counts

### Tuning Phase (Anti-Overfit Split)
- **Purpose:** Iterate on world-model v1.5 implementation
- **Baseline runs:** 10 runs per benchmark family
- **Treatment runs:** 10 runs per benchmark family
- **Usage:** Results inform v1.5 tuning but NOT used for promotion decision

### Final Evaluation Phase
- **Purpose:** Clean measurement for promotion gate
- **Baseline runs:** 30 runs per benchmark family (different seeds than tuning phase)
- **Treatment runs:** 30 runs per benchmark family
- **Usage:** Results determine whether v1.5 meets >=5% improvement gate

**Anti-overfit rule:** Final evaluation uses different seeds and fixture variants than tuning phase to prevent benchmark leakage.

## Primary Metrics

### Outcome Metrics (per benchmark family)
- **Success rate:** % of runs where task completes successfully
- **Score:** Numeric quality score (0-100) based on correctness + efficiency
- **Iteration count:** Mean number of agent turns to completion

### Process Metrics (cross-cutting)
- **Repeat-failure rate:** % of failures with same root cause as prior attempt
- **Context-drift events:** Count of lost/stale information across sessions
- **Intervention count:** Human corrections or clarifications needed

### Statistical Significance
- Use two-sample t-test (α=0.05) to compare baseline vs treatment
- Report effect size (Cohen's d) alongside p-values
- Require statistical significance + >=5% practical improvement for promotion

## Promotion Gate

World-model v1.5 is promoted to main branch if:
1. **>=5% improvement** on at least one benchmark family's success rate or score
2. **No regression >3%** on any other benchmark family
3. **Statistical significance** (p < 0.05) on improved metric
4. **Final evaluation phase only** (tuning phase results excluded)

## Artifact Format

### Run Artifacts
Each benchmark run generates:
```
benchmarks/
  runs/
    {run_id}/
      metadata.json          # seed, timestamp, config, run_id
      baseline/
        {benchmark_family}/
          scenario_{N}/
            transcript.jsonl  # full agent tool calls and responses
            metrics.json      # outcome and process metrics for this scenario
            state_dump/       # final .imi/state.db and causal graph snapshot
      treatment/
        {benchmark_family}/
          scenario_{N}/
            transcript.jsonl
            metrics.json
            state_dump/
```

### Aggregate Reports
After all runs complete:
```
benchmarks/
  reports/
    {phase}/                 # tuning or final
      summary.json           # aggregated metrics, statistical tests
      promotion_check.json   # gate evaluation (pass/fail)
      comparison.md          # human-readable report
```

### Metadata Schema
`metadata.json`:
```json
{
  "run_id": "run-20240115-001",
  "phase": "final",
  "seed": 42,
  "timestamp": "2024-01-15T10:30:00Z",
  "imi_version": "1.5.0-beta",
  "agent_cli": "copilot-cli-0.2.0",
  "fixture_version": "v2-final"
}
```

`metrics.json`:
```json
{
  "scenario_id": "repeat-failure-oauth-01",
  "success": true,
  "score": 85,
  "iteration_count": 4,
  "repeat_failure": false,
  "context_drift_events": 0,
  "intervention_count": 1,
  "duration_seconds": 127
}
```

## How to Run

### Prerequisites
```bash
# Ensure IMI is built on world-model branch
git checkout world-model
cargo build --release

# Install benchmark runner dependencies
cd benchmarks
npm install  # or pip install -r requirements.txt
```

### Tuning Phase
```bash
# Run baseline (10 runs per family)
./benchmarks/run_tuning.sh --mode=baseline --runs=10

# Run treatment (10 runs per family)
./benchmarks/run_tuning.sh --mode=treatment --runs=10

# Generate tuning report
./benchmarks/analyze.sh --phase=tuning
```

### Final Evaluation Phase
```bash
# Run baseline (30 runs per family, different seeds)
./benchmarks/run_final.sh --mode=baseline --runs=30 --seed-offset=1000

# Run treatment (30 runs per family)
./benchmarks/run_final.sh --mode=treatment --runs=30 --seed-offset=1000

# Generate final report and promotion check
./benchmarks/analyze.sh --phase=final --check-gate
```

### Manual Single Run (for debugging)
```bash
# Run a single scenario with detailed logging
./benchmarks/run_single.sh \
  --mode=treatment \
  --family=repeat-failure \
  --scenario=oauth-01 \
  --seed=42 \
  --verbose
```

## Implementation Notes

### No External LLM API Calls
Per IMI architecture decision: the world model never calls external LLM APIs. Agent CLI (Copilot, Claude Code) handles all LLM interaction. Benchmark harness may mock LLM responses for determinism.

### Isolated Worktrees
Each benchmark run uses `git worktree` to ensure filesystem isolation and prevent cross-contamination between baseline and treatment runs.

### Session Data Locations
- Copilot CLI: `~/.copilot/session-state/*/events.jsonl`
- Claude Code: `~/.claude/projects/*/`

Benchmark runner must either mock these or use isolated session directories per run.

## Future Extensions

### Post-V1.5
- Expand benchmark families (e.g., multi-agent coordination, large refactors)
- Add latency and memory overhead metrics
- Continuous benchmarking on main branch
- Public benchmark leaderboard

### Versioning
- This protocol is versioned as `v1.5-protocol-v1`
- Breaking changes require new protocol version and re-baselining

---

**Last updated:** 2024-01-15  
**Protocol version:** v1.5-protocol-v1  
**Owned by:** world-model-v1.5-validation goal (mn6gwgwgsdcgm4he)
