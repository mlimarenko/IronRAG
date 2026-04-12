#!/usr/bin/env bash
# ============================================================================
# IronRAG Graph Mutation Test
# ============================================================================
#
# Verifies that document mutations correctly rebuild the knowledge graph:
#
#   1. UPLOAD doc A with unique entity "Quantasaurus Rex"
#      → verify entity appears in graph
#
#   2. REPLACE doc A with completely different content (no Quantasaurus)
#      → verify "Quantasaurus Rex" is removed from graph
#      → verify NEW unique entity from new content appears
#
#   3. DELETE doc A
#      → verify ALL contributed entities removed
#
#   4. SHARED entity scenario:
#      Upload doc B and doc C both mentioning "Hydraulix Industries"
#      Delete doc B → verify "Hydraulix Industries" still exists (still in C)
#      Delete doc C → verify "Hydraulix Industries" is removed
#
# Usage:
#   IRONRAG_TOKEN=irt_... ./tests/graph_mutation_test.sh
# ============================================================================

set -euo pipefail
export LC_ALL=C

BASE_URL="${IRONRAG_BASE_URL:-http://localhost:19000}"
TOKEN="${IRONRAG_TOKEN:?IRONRAG_TOKEN required}"
POLL_INTERVAL=4
MAX_WAIT=300
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

api_json() {
  api "$1" "$2" -H "Content-Type: application/json" "${@:3}"
}

# Wait for a document to reach graph_ready or graph_sparse state
wait_for_doc() {
  local doc_id="$1" label="$2"
  local started=$SECONDS
  while true; do
    local elapsed=$((SECONDS - started))
    if [[ $elapsed -gt $MAX_WAIT ]]; then
      fail "$label: timeout after ${MAX_WAIT}s"
      return 1
    fi
    local readiness
    readiness=$(api_json GET "/content/documents/$doc_id" | jq -r '.readinessSummary.readinessKind // "unknown"')
    if [[ "$readiness" == "graph_ready" || "$readiness" == "graph_sparse" || "$readiness" == "readable" ]]; then
      pass "$label: $readiness (${elapsed}s)"
      return 0
    fi
    if [[ "$readiness" == "failed" ]]; then
      fail "$label: failed"
      return 1
    fi
    sleep "$POLL_INTERVAL"
  done
}

# Wait for the latest mutation on a document to complete
wait_for_mutation_complete() {
  local doc_id="$1" label="$2"
  local started=$SECONDS
  while true; do
    local elapsed=$((SECONDS - started))
    if [[ $elapsed -gt $MAX_WAIT ]]; then
      fail "$label: mutation timeout after ${MAX_WAIT}s"
      return 1
    fi
    local detail
    detail=$(api_json GET "/content/documents/$doc_id")
    local mutation_state
    mutation_state=$(echo "$detail" | jq -r '.pipeline.latest_mutation.mutation_state // "none"')
    local job_state
    job_state=$(echo "$detail" | jq -r '.pipeline.latest_job.queue_state // "none"')
    if [[ "$mutation_state" == "completed" || "$mutation_state" == "succeeded" ]]; then
      pass "$label: mutation $mutation_state (${elapsed}s, job=$job_state)"
      return 0
    fi
    if [[ "$mutation_state" == "failed" ]]; then
      fail "$label: mutation failed"
      return 1
    fi
    sleep "$POLL_INTERVAL"
  done
}

# Count entities in library matching label substring (case-insensitive)
count_entity_label() {
  local lib_id="$1" needle="$2"
  api_json GET "/knowledge/libraries/$lib_id/entities" | jq --arg n "$needle" '
    if type == "array" then
      map(select((.canonical_label // .canonicalLabel // "") | ascii_downcase | contains($n | ascii_downcase))) | length
    else 0 end'
}

# Count total entities in library
count_total_entities() {
  local lib_id="$1"
  api_json GET "/knowledge/libraries/$lib_id/entities" | jq 'if type == "array" then length else 0 end'
}

# Upload a single document via multipart
upload_doc() {
  local lib_id="$1" filename="$2" content="$3"
  local tmpfile
  tmpfile=$(mktemp /tmp/gmt-upload-XXXXXX.txt)
  printf '%s' "$content" > "$tmpfile"
  local resp
  resp=$(api POST "/content/documents/upload" \
    -F "library_id=$lib_id" \
    -F "file=@${tmpfile};filename=${filename}")
  rm -f "$tmpfile"
  echo "$resp" | jq -r '.document.document.id // empty'
}

# Replace a document via PUT to canonical endpoint or MCP
replace_doc() {
  local doc_id="$1" lib_id="$2" filename="$3" content="$4"
  local b64
  b64=$(printf '%s' "$content" | base64 -w0)
  api_json POST "/mcp" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"update_document\",\"arguments\":{\"libraryId\":\"$lib_id\",\"documentId\":\"$doc_id\",\"operationKind\":\"replace\",\"replacementFileName\":\"$filename\",\"replacementContentBase64\":\"$b64\"}}}"
}

delete_doc() {
  local doc_id="$1"
  api_json POST "/mcp" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"delete_document\",\"arguments\":{\"documentId\":\"$doc_id\"}}}"
}

# ---------------------------------------------------------------------------
# Phase 0: Setup
# ---------------------------------------------------------------------------
section "Phase 0: Setup test library"

WORKSPACES=$(api_json GET "/catalog/workspaces")
WS_ID=$(echo "$WORKSPACES" | jq -r '.[0].id // empty')
[[ -n "$WS_ID" ]] || { fail "no workspace"; exit 1; }
pass "Workspace: $WS_ID"

LIB_SLUG="graph-mutation-$(date +%s)"
LIB_RESP=$(api_json POST "/catalog/workspaces/$WS_ID/libraries" \
  -d "{\"slug\":\"$LIB_SLUG\",\"displayName\":\"Graph Mutation Test $(date +%H%M%S)\"}")
LIB_ID=$(echo "$LIB_RESP" | jq -r '.id // empty')
[[ -n "$LIB_ID" ]] || { fail "create library: $LIB_RESP"; exit 1; }
pass "Library: $LIB_ID"

# ---------------------------------------------------------------------------
# Test 1: Replace — old entities removed, new entities added
# ---------------------------------------------------------------------------
section "Test 1: REPLACE rebuilds graph (unique → unique)"

ORIGINAL_TXT='Quantasaurus Rex Field Report

This document describes the Quantasaurus Rex, a fictional creature
discovered by Dr. Penelope Whitmore at the Bramblewood Research Facility
in 2026. The Quantasaurus Rex is renowned for its unique bioluminescent
spines and ability to manipulate quantum fields.

Dr. Penelope Whitmore leads a team of seven researchers studying the
Quantasaurus Rex. The Bramblewood Research Facility is located in
the Whisperwind Valley region of Northern Cascadia.

Other observers include Marcus Hollowell and the team from the
Lumenholt Institute for Cryptobiology.'

REPLACEMENT_TXT='Borealithium Mining Operations Report

This document covers the extraction of Borealithium-7 from the
Glacierdrift Mine, operated by NorthStar Heavy Industries since 2024.
The Glacierdrift Mine is supervised by Engineer Tobias Blackwood.

Borealithium-7 production is processed at the Frostforge Refinery.
NorthStar Heavy Industries reports an annual yield of 47,000 metric tons.

Engineer Tobias Blackwood collaborates with the team from the
Polar Materials Research Consortium.'

log "Uploading original document with 'Quantasaurus Rex'..."
DOC_A=$(upload_doc "$LIB_ID" "creature-report.txt" "$ORIGINAL_TXT")
[[ -n "$DOC_A" ]] || { fail "upload failed"; exit 1; }
pass "Uploaded doc A: $DOC_A"

wait_for_doc "$DOC_A" "Doc A initial processing"

# Verify Quantasaurus Rex is in graph
QR_BEFORE=$(count_entity_label "$LIB_ID" "quantasaurus")
PW_BEFORE=$(count_entity_label "$LIB_ID" "penelope")
BR_BEFORE=$(count_entity_label "$LIB_ID" "bramblewood")
TOTAL_BEFORE=$(count_total_entities "$LIB_ID")
log "Graph snapshot BEFORE replace: total=$TOTAL_BEFORE quantasaurus=$QR_BEFORE penelope=$PW_BEFORE bramblewood=$BR_BEFORE"

if [[ "$QR_BEFORE" -gt 0 ]]; then
  pass "Original entity 'Quantasaurus' present in graph ($QR_BEFORE matches)"
else
  fail "Original entity 'Quantasaurus' NOT extracted from doc A"
fi

# REPLACE
log "Replacing doc A with completely different content..."
RESP=$(replace_doc "$DOC_A" "$LIB_ID" "mining-report.txt" "$REPLACEMENT_TXT")
ERR=$(echo "$RESP" | jq -r '.result.isError // false' 2>/dev/null || echo "true")
if [[ "$ERR" == "false" ]]; then
  pass "Replace mutation accepted"
else
  fail "Replace failed: $(echo "$RESP" | jq -r '.result.content[0].text' 2>/dev/null | head -c 200)"
fi

# Wait for replace to complete
sleep 5
wait_for_doc "$DOC_A" "Doc A after replace"
sleep 5  # extra time for graph projection convergence

# Verify Quantasaurus is GONE
QR_AFTER=$(count_entity_label "$LIB_ID" "quantasaurus")
PW_AFTER=$(count_entity_label "$LIB_ID" "penelope")
BR_AFTER=$(count_entity_label "$LIB_ID" "bramblewood")
BOR_AFTER=$(count_entity_label "$LIB_ID" "borealithium")
TOB_AFTER=$(count_entity_label "$LIB_ID" "tobias")
GLA_AFTER=$(count_entity_label "$LIB_ID" "glacierdrift")
TOTAL_AFTER=$(count_total_entities "$LIB_ID")
log "Graph snapshot AFTER replace: total=$TOTAL_AFTER quantasaurus=$QR_AFTER penelope=$PW_AFTER bramblewood=$BR_AFTER borealithium=$BOR_AFTER tobias=$TOB_AFTER glacierdrift=$GLA_AFTER"

# Old entities should be GONE
if [[ "$QR_AFTER" -eq 0 ]]; then
  pass "Old entity 'Quantasaurus' REMOVED from graph after replace"
else
  fail "Old entity 'Quantasaurus' STILL present after replace ($QR_AFTER matches) — graph not properly rebuilt"
fi

if [[ "$PW_AFTER" -eq 0 ]]; then
  pass "Old entity 'Penelope Whitmore' REMOVED from graph after replace"
else
  fail "Old entity 'Penelope Whitmore' STILL present ($PW_AFTER) — graph not rebuilt"
fi

if [[ "$BR_AFTER" -eq 0 ]]; then
  pass "Old entity 'Bramblewood' REMOVED from graph after replace"
else
  fail "Old entity 'Bramblewood' STILL present ($BR_AFTER) — graph not rebuilt"
fi

# New entities should be PRESENT
if [[ "$BOR_AFTER" -gt 0 || "$TOB_AFTER" -gt 0 || "$GLA_AFTER" -gt 0 ]]; then
  pass "New entities from replacement present (borealithium=$BOR_AFTER tobias=$TOB_AFTER glacierdrift=$GLA_AFTER)"
else
  fail "NO new entities extracted from replacement content"
fi

# ---------------------------------------------------------------------------
# Test 2: Delete — all contributed entities removed
# ---------------------------------------------------------------------------
section "Test 2: DELETE removes all contributed entities"

log "Deleting doc A..."
RESP=$(delete_doc "$DOC_A")
ERR=$(echo "$RESP" | jq -r '.result.isError // false' 2>/dev/null || echo "true")
if [[ "$ERR" == "false" ]]; then
  pass "Delete mutation accepted"
else
  fail "Delete failed: $(echo "$RESP" | jq -r '.result.content[0].text' 2>/dev/null | head -c 200)"
fi

sleep 8  # wait for delete + projection convergence

# All entities from doc A's current content (Borealithium etc.) should be GONE
BOR_DEL=$(count_entity_label "$LIB_ID" "borealithium")
TOB_DEL=$(count_entity_label "$LIB_ID" "tobias")
GLA_DEL=$(count_entity_label "$LIB_ID" "glacierdrift")
TOTAL_DEL=$(count_total_entities "$LIB_ID")
log "Graph snapshot AFTER delete: total=$TOTAL_DEL borealithium=$BOR_DEL tobias=$TOB_DEL glacierdrift=$GLA_DEL"

if [[ "$BOR_DEL" -eq 0 ]]; then
  pass "Entity 'Borealithium' removed after delete"
else
  fail "Entity 'Borealithium' still present after delete ($BOR_DEL)"
fi

if [[ "$TOB_DEL" -eq 0 ]]; then
  pass "Entity 'Tobias Blackwood' removed after delete"
else
  fail "Entity 'Tobias Blackwood' still present after delete ($TOB_DEL)"
fi

if [[ "$GLA_DEL" -eq 0 ]]; then
  pass "Entity 'Glacierdrift Mine' removed after delete"
else
  fail "Entity 'Glacierdrift Mine' still present after delete ($GLA_DEL)"
fi

# ---------------------------------------------------------------------------
# Test 3: Shared entity preservation
# ---------------------------------------------------------------------------
section "Test 3: Shared entity stays alive while another doc references it"

DOC_B_TXT='Hydraulix Industries Annual Report

Hydraulix Industries was founded in 2018 by CEO Margarethe Vanvoort.
The company manufactures pressure systems at its Cinderbloom Plant.
Hydraulix Industries currently employs 1,200 staff.'

DOC_C_TXT='Hydraulix Industries Partnership Announcement

Hydraulix Industries has partnered with Stellaforge Aerospace
on a joint venture announced by Margarethe Vanvoort.
The partnership focuses on next-generation propulsion systems.'

log "Uploading doc B (Hydraulix annual report)..."
DOC_B=$(upload_doc "$LIB_ID" "hydraulix-annual.txt" "$DOC_B_TXT")
pass "Uploaded doc B: $DOC_B"
wait_for_doc "$DOC_B" "Doc B"

log "Uploading doc C (Hydraulix partnership)..."
DOC_C=$(upload_doc "$LIB_ID" "hydraulix-partnership.txt" "$DOC_C_TXT")
pass "Uploaded doc C: $DOC_C"
wait_for_doc "$DOC_C" "Doc C"

sleep 5

HYD_BOTH=$(count_entity_label "$LIB_ID" "hydraulix")
MAR_BOTH=$(count_entity_label "$LIB_ID" "margarethe")
log "Both docs present: hydraulix=$HYD_BOTH margarethe=$MAR_BOTH"
if [[ "$HYD_BOTH" -gt 0 ]]; then
  pass "Shared entity 'Hydraulix' present (both docs reference it)"
fi

# Delete only doc B; Hydraulix should still exist (still in C)
log "Deleting only doc B (Hydraulix should survive in doc C)..."
delete_doc "$DOC_B" > /dev/null
sleep 8

HYD_AFTER_B=$(count_entity_label "$LIB_ID" "hydraulix")
MAR_AFTER_B=$(count_entity_label "$LIB_ID" "margarethe")
CIN_AFTER_B=$(count_entity_label "$LIB_ID" "cinderbloom")
STELL_AFTER_B=$(count_entity_label "$LIB_ID" "stellaforge")
log "After deleting doc B: hydraulix=$HYD_AFTER_B margarethe=$MAR_AFTER_B cinderbloom=$CIN_AFTER_B stellaforge=$STELL_AFTER_B"

if [[ "$HYD_AFTER_B" -gt 0 ]]; then
  pass "Shared entity 'Hydraulix' SURVIVES (still referenced by doc C)"
else
  fail "Shared entity 'Hydraulix' was incorrectly removed (still in doc C!)"
fi

if [[ "$STELL_AFTER_B" -gt 0 ]]; then
  pass "Entity unique to doc C 'Stellaforge' still present"
fi

# Cinderbloom was unique to doc B; should be gone
if [[ "$CIN_AFTER_B" -eq 0 ]]; then
  pass "Entity unique to doc B 'Cinderbloom' removed"
else
  fail "Entity unique to doc B 'Cinderbloom' still present after B's delete ($CIN_AFTER_B)"
fi

# Now delete doc C; Hydraulix should disappear
log "Deleting doc C (last reference to Hydraulix)..."
delete_doc "$DOC_C" > /dev/null
sleep 8

HYD_FINAL=$(count_entity_label "$LIB_ID" "hydraulix")
STELL_FINAL=$(count_entity_label "$LIB_ID" "stellaforge")
log "After deleting both: hydraulix=$HYD_FINAL stellaforge=$STELL_FINAL"

if [[ "$HYD_FINAL" -eq 0 ]]; then
  pass "Shared entity 'Hydraulix' removed when last referencing doc deleted"
else
  fail "Shared entity 'Hydraulix' still present after both contributing docs deleted"
fi

if [[ "$STELL_FINAL" -eq 0 ]]; then
  pass "Entity 'Stellaforge' (was only in doc C) removed"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
section "Summary"

echo ""
echo "  Total checks: $((PASS + FAIL))"
echo "  $(color_green "Passed: $PASS")"
if [[ $FAIL -eq 0 ]]; then
  echo "  $(color_green "Failed: 0")"
  echo ""
  echo "  $(color_green "✓ Graph mutation correctly rebuilds on replace and delete")"
  exit 0
else
  echo "  $(color_red "Failed: $FAIL")"
  echo ""
  echo "  $(color_red "✗ Graph mutation has gaps")"
  exit 1
fi
