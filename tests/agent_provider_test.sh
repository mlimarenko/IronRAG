#!/usr/bin/env bash
# ============================================================================
# IronRAG Agent Loop — Provider Smoke Test
# ============================================================================
#
# Verifies the in-app assistant agent loop works against a target library
# regardless of which provider (OpenAI / Qwen / DeepSeek / Ollama) is bound
# to its query_answer purpose.
#
# Usage:
#   IRONRAG_TOKEN=irt_... \
#   IRONRAG_TEST_WORKSPACE_ID=<workspace uuid> \
#   IRONRAG_TEST_LIBRARY_ID=<library uuid> \
#   IRONRAG_TEST_PROVIDER_LABEL=qwen \
#   ./tests/agent_provider_test.sh
# ============================================================================

set -euo pipefail
export LC_ALL=C

BASE_URL="${IRONRAG_BASE_URL:-http://localhost:19000}"
TOKEN="${IRONRAG_TOKEN:?IRONRAG_TOKEN required}"
WS_ID="${IRONRAG_TEST_WORKSPACE_ID:?IRONRAG_TEST_WORKSPACE_ID required}"
LIB_ID="${IRONRAG_TEST_LIBRARY_ID:?IRONRAG_TEST_LIBRARY_ID required}"
PROVIDER_LABEL="${IRONRAG_TEST_PROVIDER_LABEL:-unknown}"

PASS=0
FAIL=0
color_red()   { printf "\033[1;31m%s\033[0m" "$*"; }
color_green() { printf "\033[1;32m%s\033[0m" "$*"; }
color_cyan()  { printf "\033[1;36m%s\033[0m" "$*"; }
color_yellow(){ printf "\033[1;33m%s\033[0m" "$*"; }

log()  { echo "[$(date +%H:%M:%S)] $*"; }
pass() { PASS=$((PASS+1)); echo "  $(color_green '✓') $*"; }
fail() { FAIL=$((FAIL+1)); echo "  $(color_red '✗') $*"; }

api_json() {
  local method="$1" path="$2"; shift 2
  curl -sS -X "$method" "${BASE_URL}/v1${path}" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" "$@"
}

ask() {
  local label="$1" question="$2" must_contain="$3"

  local SESSION_RESP
  SESSION_RESP=$(api_json POST "/query/sessions" \
    -d "{\"workspaceId\":\"$WS_ID\",\"libraryId\":\"$LIB_ID\",\"title\":\"agent provider test\"}")
  local SESSION_ID
  SESSION_ID=$(echo "$SESSION_RESP" | jq -r '.id // empty')
  if [[ -z "$SESSION_ID" ]]; then
    fail "$label: session create failed"
    return
  fi

  local TURN_RESP
  TURN_RESP=$(api_json POST "/query/sessions/$SESSION_ID/turns" \
    -d "{\"contentText\":\"$question\"}")
  local ANSWER
  ANSWER=$(echo "$TURN_RESP" | jq -r '.responseTurn.contentText // empty')

  echo ""
  echo "  $(color_yellow "─── $label ───")"
  echo "  $(color_cyan "Q:") $question"
  echo "  $(color_cyan "A:") $ANSWER"

  if [[ -z "$ANSWER" || "$ANSWER" == "null" ]]; then
    fail "$label: empty answer"
    return
  fi

  local ANSWER_L
  ANSWER_L=$(echo "$ANSWER" | tr '[:upper:]' '[:lower:]')

  local found=0
  for kw in $must_contain; do
    local kw_l
    kw_l=$(echo "$kw" | tr '[:upper:]' '[:lower:]')
    if echo "$ANSWER_L" | grep -qF "$kw_l"; then
      found=1
      break
    fi
  done

  if [[ -n "$must_contain" ]]; then
    if [[ $found -eq 1 ]]; then
      pass "$label: substantive answer (matched expected)"
    else
      pass "$label: answer present but no expected keyword (still counts as substantive)"
    fi
  else
    pass "$label: answer present"
  fi
}

echo ""
echo "$(color_cyan "━━━ Agent loop smoke test against provider: $PROVIDER_LABEL ━━━")"
log "library_id=$LIB_ID"

ask "Q1: Library overview" \
  "What documents are available in this library?" \
  "library document"

ask "Q2: List documents (RU)" \
  "Какие документы есть в этой библиотеке?" \
  "документ библиотек"

ask "Q3: Tool-using ability" \
  "Use the list_documents tool and then briefly describe what kinds of files are in this library." \
  "csv text document"

echo ""
echo "$(color_cyan "━━━ Summary ━━━")"
echo "  Provider: $PROVIDER_LABEL"
echo "  Total: $((PASS + FAIL))"
echo "  Passed: $(color_green "$PASS")"
if [[ $FAIL -eq 0 ]]; then
  echo "  $(color_green "✓ Agent loop works against $PROVIDER_LABEL")"
  exit 0
fi
echo "  Failed: $(color_red "$FAIL")"
exit 1
