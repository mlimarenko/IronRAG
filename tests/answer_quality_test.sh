#!/usr/bin/env bash
# ============================================================================
# IronRAG Answer Quality Test
# ============================================================================
#
# Verifies that the assistant gives substantive answers (not generic
# refusals) for the three failing query patterns identified in production:
#
#   1. Meta question: "о чем библиотека?" — should describe the corpus
#   2. Tabular aggregation: "popular customer cities" — should hit table
#      summary / customer rows even without naming the file
#   3. Document inventory: "что есть из документов" — already worked
#
# Test setup uploads small CSVs that mirror the failing scenario.
# ============================================================================

set -euo pipefail
export LC_ALL=C

BASE_URL="${IRONRAG_BASE_URL:-http://localhost:19000}"
TOKEN="${IRONRAG_TOKEN:?IRONRAG_TOKEN required}"
POLL_INTERVAL=4
MAX_WAIT=240
PASS=0
FAIL=0

color_red()   { printf "\033[1;31m%s\033[0m" "$*"; }
color_green() { printf "\033[1;32m%s\033[0m" "$*"; }
color_cyan()  { printf "\033[1;36m%s\033[0m" "$*"; }
color_yellow(){ printf "\033[1;33m%s\033[0m" "$*"; }

log()     { echo "[$(date +%H:%M:%S)] $*"; }
pass()    { PASS=$((PASS+1)); echo "  $(color_green '✓') $*"; }
fail()    { FAIL=$((FAIL+1)); echo "  $(color_red '✗') $*"; }
section() { echo ""; echo "$(color_cyan "━━━ $* ━━━")"; }

api() {
  local method="$1" path="$2"; shift 2
  curl -sS -X "$method" "${BASE_URL}/v1${path}" \
    -H "Authorization: Bearer $TOKEN" "$@"
}
api_json() { api "$1" "$2" -H "Content-Type: application/json" "${@:3}"; }

wait_for_doc() {
  local doc_id="$1" label="$2"
  local started=$SECONDS
  while true; do
    local elapsed=$((SECONDS - started))
    if [[ $elapsed -gt $MAX_WAIT ]]; then
      fail "$label: timeout"
      return 1
    fi
    local r
    r=$(api_json GET "/content/documents/$doc_id" | jq -r '.readinessSummary.readinessKind // "?"')
    if [[ "$r" == "graph_ready" || "$r" == "graph_sparse" || "$r" == "readable" ]]; then
      pass "$label: $r (${elapsed}s)"
      return 0
    fi
    [[ "$r" == "failed" ]] && { fail "$label: failed"; return 1; }
    sleep "$POLL_INTERVAL"
  done
}

upload_csv() {
  local lib_id="$1" filename="$2" content="$3"
  local tmp; tmp=$(mktemp /tmp/aqt-XXXXXX.csv)
  printf '%s' "$content" > "$tmp"
  local resp
  resp=$(api POST "/content/documents/upload" -F "library_id=$lib_id" -F "file=@${tmp};filename=${filename}")
  rm -f "$tmp"
  echo "$resp" | jq -r '.document.document.id // empty'
}

ask_question() {
  local lib_id="$1" question="$2" label="$3"
  local must_contain="$4"   # space-separated list of keywords (case-insensitive); empty means no required
  local must_not_contain="$5"  # space-separated list of forbidden phrases

  local SESSION_RESP
  SESSION_RESP=$(api_json POST "/query/sessions" \
    -d "{\"workspaceId\":\"$WS_ID\",\"libraryId\":\"$lib_id\",\"title\":\"AQT $label\"}")
  local SESSION_ID
  SESSION_ID=$(echo "$SESSION_RESP" | jq -r '.id // empty')
  [[ -n "$SESSION_ID" ]] || { fail "$label: failed to create session"; return; }

  local TURN_RESP
  TURN_RESP=$(api_json POST "/query/sessions/$SESSION_ID/turns" \
    -d "{\"contentText\":\"$question\"}")
  local ANSWER
  ANSWER=$(echo "$TURN_RESP" | jq -r '.responseTurn.contentText // empty')
  local VERIF
  VERIF=$(echo "$TURN_RESP" | jq -r '.verificationState // "?"')
  local REFS
  REFS=$(echo "$TURN_RESP" | jq '[.chunkReferences // [], .preparedSegmentReferences // []] | add | length')

  echo ""
  echo "  $(color_yellow "─── $label ───")"
  echo "  $(color_cyan "Q:") $question"
  echo "  $(color_cyan "A:") $ANSWER"
  echo "  $(color_cyan "Verification:") $VERIF  •  $(color_cyan "Refs:") $REFS"

  if [[ -z "$ANSWER" || "$ANSWER" == "null" ]]; then
    fail "$label: empty answer"
    return
  fi

  # Block on forbidden phrases (the generic refusal text)
  local IS_GENERIC_REFUSAL=0
  for phrase in "$must_not_contain"; do
    [[ -n "$phrase" ]] || continue
    if echo "$ANSWER" | grep -qF "$phrase"; then
      IS_GENERIC_REFUSAL=1
      break
    fi
  done
  if [[ $IS_GENERIC_REFUSAL -eq 1 ]]; then
    fail "$label: assistant returned generic refusal text"
    return
  fi

  # Required keywords (any of them is enough to consider the answer substantive).
  # We do the case-folding in python3 because bash `tr` cannot lowercase
  # cyrillic / unicode characters; commas are stripped so "4,200,000" matches
  # "4200000" and vice versa.
  if [[ -n "$must_contain" ]]; then
    local KW_FOUND
    KW_FOUND=$(python3 - "$ANSWER" "$must_contain" <<'PY' 2>/dev/null
import sys
answer = sys.argv[1].casefold().replace(',', '')
keywords = sys.argv[2].split()
print('1' if any(kw.casefold().replace(',', '') in answer for kw in keywords) else '0')
PY
)
    if [[ "$KW_FOUND" == "1" ]]; then
      pass "$label: substantive answer (matched expected keyword)"
    else
      fail "$label: answer is missing all expected keywords ($must_contain)"
    fi
  else
    pass "$label: answer present"
  fi
}

# ---------------------------------------------------------------------------
# Setup
# ---------------------------------------------------------------------------
section "Phase 0: setup test library + sample CSVs"

# When IRONRAG_TEST_LIBRARY_ID is set, reuse that library (its bindings
# determine which provider answers — useful for cross-provider testing).
# Otherwise create a fresh library in the default workspace.
if [[ -n "${IRONRAG_TEST_LIBRARY_ID:-}" ]]; then
  LIB_ID="$IRONRAG_TEST_LIBRARY_ID"
  if [[ -n "${IRONRAG_TEST_WORKSPACE_ID:-}" ]]; then
    WS_ID="$IRONRAG_TEST_WORKSPACE_ID"
  else
    WS_ID=$(api_json GET "/catalog/workspaces" | jq -r '.[0].id // empty')
  fi
  [[ -n "$WS_ID" ]] || { fail "no workspace"; exit 1; }
  pass "Workspace: $WS_ID"
  pass "Library (reused): $LIB_ID"
else
  WS_ID=$(api_json GET "/catalog/workspaces" | jq -r '.[0].id // empty')
  [[ -n "$WS_ID" ]] || { fail "no workspace"; exit 1; }
  pass "Workspace: $WS_ID"

  LIB_SLUG="answer-quality-$(date +%s)"
  LIB_RESP=$(api_json POST "/catalog/workspaces/$WS_ID/libraries" \
    -d "{\"slug\":\"$LIB_SLUG\",\"displayName\":\"Answer Quality Test\"}")
  LIB_ID=$(echo "$LIB_RESP" | jq -r '.id // empty')
  [[ -n "$LIB_ID" ]] || { fail "create library"; exit 1; }
  pass "Library: $LIB_ID"
fi

# Upload mini-CSVs that mirror the failing-screenshot scenario
CUSTOMERS_CSV='customer_id,name,email,city,country,total_orders
C001,Alice Hartwell,alice@example.com,Berlin,Germany,12
C002,Bruno Tanaka,bruno@example.com,Tokyo,Japan,8
C003,Carla Mendes,carla@example.com,Berlin,Germany,15
C004,Dmitri Volkov,dmitri@example.com,Moscow,Russia,9
C005,Elena Rojas,elena@example.com,Berlin,Germany,7
C006,Felix Owusu,felix@example.com,Accra,Ghana,11
C007,Greta Lindqvist,greta@example.com,Stockholm,Sweden,14
C008,Hiroshi Sato,hiroshi@example.com,Tokyo,Japan,6
C009,Ivana Petrova,ivana@example.com,Berlin,Germany,18
C010,Jasper Yu,jasper@example.com,Singapore,Singapore,13'

PRODUCTS_CSV='product_id,name,category,price_usd,units_sold
P001,Pulsar Lamp,Lighting,89,420
P002,Hydra Mug,Kitchen,24,890
P003,Vector Notebook,Office,15,1240
P004,Echo Pillow,Bedroom,42,310
P005,Comet Speaker,Audio,129,205
P006,Atlas Backpack,Travel,76,580
P007,Cipher Lock,Security,55,330'

ORG_CSV='org_id,name,industry,headquarters
O001,Quantasaur Industries,Biotech,Singapore
O002,NorthStar Heavy,Mining,Reykjavik
O003,Aurora Robotics,Automation,Tokyo'

LEADS_CSV='lead_id,name,source,stage
L001,Alpha Co,referral,qualified
L002,Beta LLC,inbound,demo_scheduled
L003,Gamma Inc,outbound,negotiation'

log "Uploading 4 sample CSVs..."
DOC_C=$(upload_csv "$LIB_ID" "customers.csv" "$CUSTOMERS_CSV")
DOC_P=$(upload_csv "$LIB_ID" "products.csv" "$PRODUCTS_CSV")
DOC_O=$(upload_csv "$LIB_ID" "organizations.csv" "$ORG_CSV")
DOC_L=$(upload_csv "$LIB_ID" "leads.csv" "$LEADS_CSV")
pass "Uploaded 4 documents"

wait_for_doc "$DOC_C" "customers.csv"
wait_for_doc "$DOC_P" "products.csv"
wait_for_doc "$DOC_O" "organizations.csv"
wait_for_doc "$DOC_L" "leads.csv"
sleep 4

# ---------------------------------------------------------------------------
# Q&A
# ---------------------------------------------------------------------------
section "Phase 1: Q&A scenarios"

GENERIC_REFUSAL_PHRASE="don't have enough grounded evidence"

ask_question "$LIB_ID" \
  "о чем эта библиотека?" \
  "Q1: Meta question (RU)" \
  "customer product organization lead клиент продукт организац лид csv документ table таблиц test тест" \
  "$GENERIC_REFUSAL_PHRASE"

ask_question "$LIB_ID" \
  "what is this library about?" \
  "Q2: Meta question (EN)" \
  "customer product organization lead csv table" \
  "$GENERIC_REFUSAL_PHRASE"

ask_question "$LIB_ID" \
  "что есть из документов" \
  "Q3: Document inventory" \
  "customer product organization lead csv 4 четыре" \
  "$GENERIC_REFUSAL_PHRASE"

ask_question "$LIB_ID" \
  "какие самые популярные города клиентов" \
  "Q4: Popular customer cities (RU)" \
  "berlin tokyo берлин токио" \
  "$GENERIC_REFUSAL_PHRASE"

ask_question "$LIB_ID" \
  "what are the most popular customer cities?" \
  "Q5: Popular customer cities (EN)" \
  "berlin tokyo" \
  "$GENERIC_REFUSAL_PHRASE"

ask_question "$LIB_ID" \
  "top selling products by units" \
  "Q6: Top selling products (table aggregation)" \
  "vector notebook hydra mug atlas backpack 1240 890 580 units_sold product" \
  "$GENERIC_REFUSAL_PHRASE"

ask_question "$LIB_ID" \
  "сколько клиентов из Берлина?" \
  "Q7: Count by city (specific lookup)" \
  "4 четыре berlin alice carla elena ivana" \
  "$GENERIC_REFUSAL_PHRASE"

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
section "Summary"
echo ""
echo "  Total: $((PASS + FAIL))"
echo "  $(color_green "Passed: $PASS")"
[[ $FAIL -eq 0 ]] && {
  echo "  $(color_green "Failed: 0")"
  echo ""
  echo "  $(color_green "✓ Answer quality is good")"
  exit 0
}
echo "  $(color_red "Failed: $FAIL")"
exit 1
