#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEFAULT_ENV_FILE="$ROOT_DIR/local.env"
LEGACY_ENV_FILE="$ROOT_DIR/scripts/.env.posthog"
ENV_FILE="${POSTHOG_ENV_FILE:-}"

if [[ -z "$ENV_FILE" ]]; then
  if [[ -f "$DEFAULT_ENV_FILE" ]]; then
    ENV_FILE="$DEFAULT_ENV_FILE"
  else
    ENV_FILE="$LEGACY_ENV_FILE"
  fi
fi

if [[ -f "$ENV_FILE" ]]; then
  # shellcheck disable=SC1090
  source "$ENV_FILE"
fi

POSTHOG_HOST="${POSTHOG_HOST:-https://us.posthog.com}"
POSTHOG_PROJECT_ID="${POSTHOG_PROJECT_ID:-}"
POSTHOG_API_KEY="${POSTHOG_API_KEY:-}"
DAYS="${DAYS:-7}"
ROUTE="${1:-all}"

if ! [[ "$DAYS" =~ ^[0-9]+$ ]]; then
  echo "DAYS must be a whole number."
  exit 1
fi

if [[ -z "$POSTHOG_PROJECT_ID" || -z "$POSTHOG_API_KEY" ]]; then
  echo "Missing PostHog config."
  echo "Create $ROOT_DIR/local.env (or set POSTHOG_ENV_FILE) with POSTHOG_PROJECT_ID and POSTHOG_API_KEY."
  exit 1
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "jq is required. Install with: brew install jq"
  exit 1
fi

hogql() {
  local q="$1"
  curl -sS -X POST \
    "$POSTHOG_HOST/api/projects/$POSTHOG_PROJECT_ID/query/" \
    -H "Authorization: Bearer $POSTHOG_API_KEY" \
    -H "Content-Type: application/json" \
    -d "$(jq -nc --arg q "$q" '{query:{kind:"HogQLQuery",query:$q}}')"
}

scalar() {
  local q="$1"
  hogql "$q" | jq -r '.results[0][0] // 0'
}

print_overview() {
  echo "PostHog usage window: last ${DAYS} day(s)"
  echo

  TOTAL="$(scalar "SELECT count() FROM events WHERE event LIKE 'imi_%' AND timestamp >= now() - INTERVAL ${DAYS} day")"
  HUMAN="$(scalar "SELECT count() FROM events WHERE event LIKE 'imi_%' AND timestamp >= now() - INTERVAL ${DAYS} day AND coalesce(properties.interactive, 0) = 1 AND coalesce(properties.is_ci, 0) = 0")"
  AGENT="$(scalar "SELECT count() FROM events WHERE event LIKE 'imi_%' AND timestamp >= now() - INTERVAL ${DAYS} day AND coalesce(properties.interactive, 0) = 0 AND coalesce(properties.is_ci, 0) = 0")"
  BOT_CI="$(scalar "SELECT count() FROM events WHERE event LIKE 'imi_%' AND timestamp >= now() - INTERVAL ${DAYS} day AND coalesce(properties.is_ci, 0) = 1")"
  UNIQUE_INSTALLS="$(scalar "SELECT uniqExact(properties.install_id) FROM events WHERE event LIKE 'imi_%' AND timestamp >= now() - INTERVAL ${DAYS} day")"
  UNIQUE_DEVICES="$(scalar "SELECT uniqExact(distinct_id) FROM events WHERE event LIKE 'imi_%' AND timestamp >= now() - INTERVAL ${DAYS} day")"

  echo "Events total:         $TOTAL"
  echo "Human sessions:       $HUMAN"
  echo "Agent sessions:       $AGENT"
  echo "Bot/CI sessions:      $BOT_CI"
  echo "Unique installs:      $UNIQUE_INSTALLS"
  echo "Unique devices:       $UNIQUE_DEVICES"
  echo

  echo "Top IMI commands/events:"
  hogql "SELECT event, count() AS c FROM events WHERE event LIKE 'imi_%' AND timestamp >= now() - INTERVAL ${DAYS} day GROUP BY event ORDER BY c DESC LIMIT 12" \
    | jq -r '.results[] | @tsv' \
    | awk -F '\t' '{printf "  %-35s %s\n",$1,$2}'
}

print_daily() {
  echo "Daily analytics by day (last ${DAYS} day(s))"
  echo "day         total   human   agent   bot_ci"
  hogql "SELECT toDate(timestamp) AS day, count() AS total, sum(if(coalesce(properties.interactive, 0) = 1 AND coalesce(properties.is_ci, 0) = 0, 1, 0)) AS human, sum(if(coalesce(properties.interactive, 0) = 0 AND coalesce(properties.is_ci, 0) = 0, 1, 0)) AS agent, sum(if(coalesce(properties.is_ci, 0) = 1, 1, 0)) AS bot_ci FROM events WHERE event LIKE 'imi_%' AND timestamp >= now() - INTERVAL ${DAYS} day GROUP BY day ORDER BY day DESC" \
    | jq -r '.results[] | @tsv' \
    | awk -F '\t' '{printf "%-10s %7s %7s %7s %8s\n",$1,$2,$3,$4,$5}'
}

case "$ROUTE" in
  overview)
    print_overview
    ;;
  daily)
    print_daily
    ;;
  all)
    print_overview
    echo
    print_daily
    ;;
  *)
    echo "Usage: scripts/posthog-usage.sh [overview|daily|all]"
    exit 1
    ;;
esac
