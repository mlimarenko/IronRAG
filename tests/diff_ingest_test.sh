#!/usr/bin/env bash
# ============================================================================
# IronRAG Diff-Aware Ingest Test
# ============================================================================
#
# Verifies that when a document is replaced with mostly-identical content,
# the pipeline reuses the previous revision's graph extraction output for
# unchanged chunks instead of calling the LLM again.
#
# Test plan:
#   1. Upload doc with 5 distinct paragraphs (each becomes its own chunk).
#   2. Snapshot baseline cost from extract_graph stage.
#   3. Replace with the SAME content but ONE paragraph edited.
#   4. Verify lifecycle stage details report `reusedChunks > 0`.
#   5. Verify `reusedEntities` is reported.
#   6. Verify the new revision's extract_graph cost is significantly lower
#      than the baseline (LLM was only called for the changed chunks).
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

wait_for_active_revision_change() {
  local doc_id="$1" label="$2" old_rev="$3"
  local started=$SECONDS
  while true; do
    local elapsed=$((SECONDS - started))
    if [[ $elapsed -gt $MAX_WAIT ]]; then
      fail "$label: timeout waiting for new revision"
      return 1
    fi
    local current
    current=$(api_json GET "/content/documents/$doc_id" | jq -r '.activeRevision.id // empty')
    if [[ -n "$current" && "$current" != "$old_rev" ]]; then
      pass "$label: new revision $current (${elapsed}s)"
      return 0
    fi
    sleep "$POLL_INTERVAL"
  done
}

# Get the latest extract_graph stage event for a document
latest_extract_graph_stage() {
  local doc_id="$1"
  api_json GET "/content/documents/$doc_id" | jq '
    .lifecycle.attempts[0].stageEvents
    | map(select(.stage == "extract_graph"))
    | .[-1] // {}'
}

# Get the latest extract_graph stage event details from job state for the doc
latest_extract_graph_details() {
  local doc_id="$1"
  api_json GET "/content/documents/$doc_id" | jq '
    .pipeline.latest_job // {}'
}

upload_doc() {
  local lib_id="$1" filename="$2" content="$3"
  local tmpfile
  tmpfile=$(mktemp /tmp/dit-upload-XXXXXX.txt)
  printf '%s' "$content" > "$tmpfile"
  local resp
  resp=$(api POST "/content/documents/upload" \
    -F "library_id=$lib_id" \
    -F "file=@${tmpfile};filename=${filename}")
  rm -f "$tmpfile"
  echo "$resp" | jq -r '.document.document.id // empty'
}

replace_doc() {
  local doc_id="$1" lib_id="$2" filename="$3" content="$4"
  local b64
  b64=$(printf '%s' "$content" | base64 -w0)
  api_json POST "/mcp" \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"update_document\",\"arguments\":{\"libraryId\":\"$lib_id\",\"documentId\":\"$doc_id\",\"operationKind\":\"replace\",\"replacementFileName\":\"$filename\",\"replacementContentBase64\":\"$b64\"}}}"
}

# ---------------------------------------------------------------------------
# Phase 0: Setup
# ---------------------------------------------------------------------------
section "Phase 0: Setup test library"

WS_ID=$(api_json GET "/catalog/workspaces" | jq -r '.[0].id // empty')
[[ -n "$WS_ID" ]] || { fail "no workspace"; exit 1; }
pass "Workspace: $WS_ID"

LIB_SLUG="diff-ingest-$(date +%s)"
LIB_RESP=$(api_json POST "/catalog/workspaces/$WS_ID/libraries" \
  -d "{\"slug\":\"$LIB_SLUG\",\"displayName\":\"Diff Ingest Test\"}")
LIB_ID=$(echo "$LIB_RESP" | jq -r '.id // empty')
[[ -n "$LIB_ID" ]] || { fail "create library"; exit 1; }
pass "Library: $LIB_ID"

# ---------------------------------------------------------------------------
# Phase 1: Upload original document
# ---------------------------------------------------------------------------
section "Phase 1: Upload original document"

# Use 5 long, distinct paragraphs separated by blank lines to maximize the
# chance each paragraph becomes its own chunk under the canonical chunker.
# Each paragraph mentions unique fictional entities so we can match by name.
ORIGINAL_TXT='# Borealithium Mining Operations Report

## Section 1 — Glacierdrift Mine Overview
The Glacierdrift Mine is operated by NorthStar Heavy Industries since 2024
under the supervision of Engineer Tobias Blackwood. The mine extracts
Borealithium-7 from the Whisperwind Valley region of Northern Cascadia.
Annual production is approximately forty-seven thousand metric tons.

## Section 2 — Frostforge Refinery
Borealithium-7 ore is processed at the Frostforge Refinery, located near
the city of Cinderbloom. The refinery is supervised by Director Margarethe
Vanvoort. Frostforge employs three hundred and twenty refinery technicians
across two production lines.

## Section 3 — Quality Standards
All Borealithium output is graded against the Polar Materials Research
Consortium specification PMRC-7 revision four. Quality engineer Penelope
Whitmore leads the inspection team at Frostforge. Rejected ore is recycled
through the Stellaforge secondary processing loop.

## Section 4 — Logistics
Refined Borealithium ingots ship via the Northstar Rail Corridor through
the Bramblewood freight terminal. Logistics is handled by Quartermaster
Marcus Hollowell from the Lumenholt Distribution Center. Average transit
time is six days from Frostforge to the export hub.

## Section 5 — Health and Safety
The Glacierdrift Mine maintains its safety record through mandatory
quarterly drills supervised by Safety Officer Yuki Tanakawa. The Polar
Materials Research Consortium certifies the safety program annually.
Reported incidents in the last year totaled three minor surface injuries.'

DOC_ID=$(upload_doc "$LIB_ID" "borealithium-original.md" "$ORIGINAL_TXT")
[[ -n "$DOC_ID" ]] || { fail "upload original failed"; exit 1; }
pass "Uploaded doc: $DOC_ID"

wait_for_doc "$DOC_ID" "Doc original processing"

ORIGINAL_DETAIL=$(api_json GET "/content/documents/$DOC_ID")
ORIGINAL_REV=$(echo "$ORIGINAL_DETAIL" | jq -r '.activeRevision.id // empty')
ORIGINAL_STAGE=$(echo "$ORIGINAL_DETAIL" | jq '.lifecycle.attempts[0].stageEvents | map(select(.stage == "extract_graph"))[-1] // {}')
ORIGINAL_REUSED=$(echo "$ORIGINAL_STAGE" | jq -r '.detailsJson.reusedChunks // .details_json.reusedChunks // 0' 2>/dev/null || echo "0")
ORIGINAL_TOTAL_CHUNKS=$(echo "$ORIGINAL_DETAIL" | jq -r '.preparedSegmentCount // 0')
ORIGINAL_COST=$(echo "$ORIGINAL_DETAIL" | jq -r '.lifecycle.totalCost // 0')
ORIGINAL_TOKENS=$(echo "$ORIGINAL_STAGE" | jq -r '.totalTokens // 0')

log "Baseline: rev=$ORIGINAL_REV chunks=$ORIGINAL_TOTAL_CHUNKS reused=$ORIGINAL_REUSED cost=\$$ORIGINAL_COST tokens=$ORIGINAL_TOKENS"

if [[ "$ORIGINAL_REUSED" == "0" ]]; then
  pass "Original upload: 0 reused (expected, no parent revision)"
else
  fail "Original upload reported reusedChunks=$ORIGINAL_REUSED (expected 0)"
fi

# ---------------------------------------------------------------------------
# Phase 2: Replace with mostly-identical content (one paragraph edited)
# ---------------------------------------------------------------------------
section "Phase 2: Replace with one section edited"

# Same as ORIGINAL_TXT, except Section 3 (Quality Standards) is rewritten.
# Other 4 sections are byte-identical so their chunks should reuse extraction.
REPLACED_TXT='# Borealithium Mining Operations Report

## Section 1 — Glacierdrift Mine Overview
The Glacierdrift Mine is operated by NorthStar Heavy Industries since 2024
under the supervision of Engineer Tobias Blackwood. The mine extracts
Borealithium-7 from the Whisperwind Valley region of Northern Cascadia.
Annual production is approximately forty-seven thousand metric tons.

## Section 2 — Frostforge Refinery
Borealithium-7 ore is processed at the Frostforge Refinery, located near
the city of Cinderbloom. The refinery is supervised by Director Margarethe
Vanvoort. Frostforge employs three hundred and twenty refinery technicians
across two production lines.

## Section 3 — Compliance and Audits
Quartzite Compliance Group performs an annual audit of all Borealithium
processing facilities. Lead auditor Captain Ferdinand Quillborough issues
the certification report each March. The audit covers safety, environmental
impact, and chain-of-custody records for the preceding twelve months.

## Section 4 — Logistics
Refined Borealithium ingots ship via the Northstar Rail Corridor through
the Bramblewood freight terminal. Logistics is handled by Quartermaster
Marcus Hollowell from the Lumenholt Distribution Center. Average transit
time is six days from Frostforge to the export hub.

## Section 5 — Health and Safety
The Glacierdrift Mine maintains its safety record through mandatory
quarterly drills supervised by Safety Officer Yuki Tanakawa. The Polar
Materials Research Consortium certifies the safety program annually.
Reported incidents in the last year totaled three minor surface injuries.'

log "Replacing document with one section edited..."
RESP=$(replace_doc "$DOC_ID" "$LIB_ID" "borealithium-edited.md" "$REPLACED_TXT")
ERR=$(echo "$RESP" | jq -r '.result.isError // false')
if [[ "$ERR" == "false" ]]; then
  pass "Replace mutation accepted"
else
  fail "Replace failed: $(echo "$RESP" | jq -r '.result.content[0].text' | head -c 200)"
  exit 1
fi

wait_for_active_revision_change "$DOC_ID" "Doc replace" "$ORIGINAL_REV"
wait_for_doc "$DOC_ID" "Doc reprocessing"
sleep 5  # let lifecycle settle

# ---------------------------------------------------------------------------
# Phase 3: Verify diff-aware reuse stats
# ---------------------------------------------------------------------------
section "Phase 3: Verify reuse stats"

NEW_DETAIL=$(api_json GET "/content/documents/$DOC_ID")
NEW_REV=$(echo "$NEW_DETAIL" | jq -r '.activeRevision.id // empty')

# The lifecycle returns multiple attempts sorted by queueStartedAt desc;
# the first one is the latest revision's attempt.
NEW_STAGE=$(echo "$NEW_DETAIL" | jq '
  .lifecycle.attempts[0].stageEvents
  | map(select(.stage == "extract_graph"))[-1] // {}')

# Reuse stats are now first-class fields on the stage event itself
NEW_REUSED_CHUNKS=$(echo "$NEW_STAGE" | jq -r '.reusedChunks // 0')
NEW_REUSED_ENTITIES=$(echo "$NEW_STAGE" | jq -r '.reusedEntities // 0')
NEW_REUSED_RELATIONS=$(echo "$NEW_STAGE" | jq -r '.reusedRelations // 0')
NEW_CHUNKS_PROCESSED=$(echo "$NEW_STAGE" | jq -r '.chunksProcessed // 0')
NEW_TOTAL_TOKENS=$(echo "$NEW_STAGE" | jq -r '.totalTokens // 0')
NEW_COST=$(echo "$NEW_DETAIL" | jq -r '.lifecycle.totalCost // 0')

log "After replace: rev=$NEW_REV chunks=$NEW_CHUNKS_PROCESSED reused=$NEW_REUSED_CHUNKS reusedEntities=$NEW_REUSED_ENTITIES reusedRelations=$NEW_REUSED_RELATIONS tokens=$NEW_TOTAL_TOKENS"

# Assertions
if [[ "$NEW_REV" != "$ORIGINAL_REV" ]]; then
  pass "Revision changed: $ORIGINAL_REV → $NEW_REV"
else
  fail "Revision did not change"
fi

if [[ "$NEW_REUSED_CHUNKS" -gt 0 ]] 2>/dev/null; then
  pass "Diff-aware reuse triggered: $NEW_REUSED_CHUNKS chunks reused"
else
  fail "Diff-aware reuse did NOT trigger (expected >0 reused chunks)"
fi

if [[ "$NEW_REUSED_ENTITIES" -gt 0 ]] 2>/dev/null; then
  pass "Reused entity contributions: $NEW_REUSED_ENTITIES"
else
  fail "Expected reusedEntities > 0, got $NEW_REUSED_ENTITIES"
fi

# Compute reuse ratio
if [[ "$NEW_CHUNKS_PROCESSED" -gt 0 ]] 2>/dev/null; then
  REUSE_PCT=$(echo "scale=0; $NEW_REUSED_CHUNKS * 100 / $NEW_CHUNKS_PROCESSED" | bc -l 2>/dev/null || echo 0)
  if [[ "$REUSE_PCT" -ge 50 ]] 2>/dev/null; then
    pass "Reuse ratio: $REUSE_PCT% ($NEW_REUSED_CHUNKS / $NEW_CHUNKS_PROCESSED)"
  else
    fail "Reuse ratio low: $REUSE_PCT% (expected ≥50% since 4 of 5 sections unchanged)"
  fi
fi

# Verify token usage went down (only changed chunks should hit LLM)
if [[ "$NEW_TOTAL_TOKENS" -lt "$ORIGINAL_TOKENS" ]] 2>/dev/null; then
  SAVED_TOKENS=$((ORIGINAL_TOKENS - NEW_TOTAL_TOKENS))
  SAVED_PCT=$(echo "scale=0; $SAVED_TOKENS * 100 / $ORIGINAL_TOKENS" | bc -l 2>/dev/null || echo 0)
  pass "Token usage decreased: $ORIGINAL_TOKENS → $NEW_TOTAL_TOKENS (saved ${SAVED_PCT}%)"
else
  fail "Token usage did not decrease ($ORIGINAL_TOKENS → $NEW_TOTAL_TOKENS)"
fi

# ---------------------------------------------------------------------------
# Phase 4: Verify graph correctness (reused entities still present)
# ---------------------------------------------------------------------------
section "Phase 4: Verify graph still correct"

# Entities from unchanged sections should still be in the graph
ENTITIES=$(api_json GET "/knowledge/libraries/$LIB_ID/entities")
count_entity() {
  local needle="$1"
  echo "$ENTITIES" | jq --arg n "$needle" '
    if type == "array" then
      map(select((.canonical_label // .canonicalLabel // "") | ascii_downcase | contains($n | ascii_downcase))) | length
    else 0 end'
}

GLA=$(count_entity "glacierdrift")
TOB=$(count_entity "tobias")
FRO=$(count_entity "frostforge")
MAR=$(count_entity "margarethe")
QUA=$(count_entity "quartzite")
FER=$(count_entity "ferdinand")
PEN=$(count_entity "penelope")  # was in old Section 3 — should be GONE

log "Entity check: glacierdrift=$GLA tobias=$TOB frostforge=$FRO margarethe=$MAR quartzite=$QUA ferdinand=$FER penelope=$PEN"

# Surviving entities (from unchanged sections)
if [[ "$GLA" -gt 0 || "$TOB" -gt 0 ]]; then
  pass "Section 1 entities (Glacierdrift, Tobias) still in graph after edit"
fi
if [[ "$FRO" -gt 0 || "$MAR" -gt 0 ]]; then
  pass "Section 2 entities (Frostforge, Margarethe) still in graph after edit"
fi

# New entities from edited Section 3
if [[ "$QUA" -gt 0 || "$FER" -gt 0 ]]; then
  pass "New Section 3 entities (Quartzite, Ferdinand) present in graph"
else
  fail "Edited Section 3 entities (Quartzite, Ferdinand) NOT extracted"
fi

# Removed entities from old Section 3
if [[ "$PEN" -eq 0 ]]; then
  pass "Old Section 3 entity (Penelope) removed after edit"
else
  fail "Old Section 3 entity (Penelope) still present after edit ($PEN matches)"
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
  echo "  $(color_green "✓ Diff-aware ingest works correctly")"
  exit 0
else
  echo "  $(color_red "Failed: $FAIL")"
  exit 1
fi
