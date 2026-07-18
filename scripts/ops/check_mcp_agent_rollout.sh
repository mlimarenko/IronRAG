#!/usr/bin/env bash
# Verify all preconditions before enabling the UI MCP-agent handler.
#
# Usage:
#   IRONRAG_API_BASE_URL=https://example.com \
#   IRONRAG_API_TOKEN=<token> \
#   [IRONRAG_LIBRARY_IDS=uuid1,uuid2] \
#   [IRONRAG_PG_DSN=postgres://...] \
#   scripts/ops/check_mcp_agent_rollout.sh [--with-bench]
#   # or replace IRONRAG_API_TOKEN with IRONRAG_PROBE_PASSWORD=<password>
#
# Preconditions checked:
#   1. Migration version 6 (0006_ui_mcp_agent.sql) is applied.
#      Requires IRONRAG_PG_DSN; skipped with a warning otherwise.
#   2. Each active library has at least one active "query_answer" binding,
#      which is the canonical binding resolved by the UI MCP-tool agent.
#      Cookie-auth runs check /v1/iam/session/resolve shell readiness, which
#      includes inherited effective bindings. Bearer-only runs can only check
#      direct library-scope bindings through /v1/ai/bindings.
#   3. (--with-bench) scripts/bench/agent_turn_p95.py quality gates pass and
#      p95 <= 25 000 ms (the rollout canary target; the hard full-turn SLO is 90 s).
#
# Output:
#   stderr  — coloured human-readable status table
#   stdout  — single JSON summary line
#
# Exit codes:
#   0  all preconditions pass (ready_to_rollout=true)
#   1  one or more preconditions failed

set -euo pipefail

# ---------------------------------------------------------------------------
# Colour helpers (degrade gracefully when not a TTY)
# ---------------------------------------------------------------------------
if [ -t 2 ]; then
    RED='\033[0;31m'
    GREEN='\033[0;32m'
    YELLOW='\033[0;33m'
    BOLD='\033[1m'
    RESET='\033[0m'
else
    RED='' GREEN='' YELLOW='' BOLD='' RESET=''
fi

ok()   { printf "${GREEN}  OK    %s${RESET}\n" "$*" >&2; }
fail() { printf "${RED}  FAIL  %s${RESET}\n" "$*" >&2; }
warn() { printf "${YELLOW}  WARN  %s${RESET}\n" "$*" >&2; }
info() { printf "  %-6s %s\n" "" "$*" >&2; }
sep()  { printf "%s\n" "$(printf '%.0s─' {1..60})" >&2; }
trim() {
    local value="$1"
    value="${value#"${value%%[![:space:]]*}"}"
    value="${value%"${value##*[![:space:]]}"}"
    printf '%s' "$value"
}
is_uuid() {
    local value="$1"
    [[ "$value" =~ ^[0-9A-Fa-f]{8}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{4}-[0-9A-Fa-f]{12}$ ]]
}
emit_failure_json() {
    local migration_status="${1:-\"skipped\"}"
    printf '{"migration_v6":%s,"libraries":[],"bench_p95_ms":null,"bench_p95_gate_ms":null,"bench_gate_passed":null,"bench_successes":null,"bench_failures":null,"ready_to_rollout":false}\n' \
        "$migration_status"
}

# ---------------------------------------------------------------------------
# Usage / help
# ---------------------------------------------------------------------------
usage() {
    local exit_code="${1:-0}"
    cat >&2 <<EOF
${BOLD}check_mcp_agent_rollout.sh${RESET} — verify MCP-agent rollout preconditions

${BOLD}Usage${RESET}
  IRONRAG_API_BASE_URL=<url> IRONRAG_API_TOKEN=<token> \\
      $(basename "$0") [--with-bench] [--help]
  IRONRAG_API_BASE_URL=<url> IRONRAG_PROBE_PASSWORD=<password> \\
      $(basename "$0") [--with-bench] [--help]

${BOLD}Required env vars${RESET}
  IRONRAG_API_BASE_URL   Base URL of the IronRAG API (no trailing slash)

${BOLD}Auth env vars${RESET}
  IRONRAG_API_TOKEN      Bearer token for the API, preferred when set.
                         If absent, set IRONRAG_PROBE_PASSWORD for cookie auth.

${BOLD}Optional env vars${RESET}
  IRONRAG_LOGIN          Login for cookie-session auth (default: admin).
  IRONRAG_PROBE_PASSWORD Password for cookie-session auth when no bearer token is set.
  IRONRAG_LIBRARY_IDS    Comma-separated library UUIDs to check.
                         Defaults to all active libraries discovered via the API.
  IRONRAG_BENCH_OUTPUT_PATH
                         Optional JSON artifact path for --with-bench.
  IRONRAG_PG_DSN         PostgreSQL DSN (psql-compatible).
                         When set, confirms migration version 6 is applied.
                         When absent, the migration check is skipped.

${BOLD}Flags${RESET}
  --with-bench           Also run scripts/bench/agent_turn_p95.py and report
                         quality counts plus the p95 latency gate result. Requires
                         IRONRAG_LIBRARY_ID to be set for the bench script (uses
                         the first library in the active list if unset).
  --help                 Print this help and exit 0.

${BOLD}Output${RESET}
  stderr   Coloured human-readable status table.
  stdout   Single JSON summary line:
           {"migration_v6": true|false|"skipped",
            "libraries": [{"id":"...","query_ready":true|false}],
            "bench_p95_ms": null|<number>,
            "bench_p95_gate_ms": null|<number>,
            "bench_gate_passed": null|true|false,
            "bench_successes": null|<number>,
            "bench_failures": null|<number>,
            "ready_to_rollout": true|false}

${BOLD}Exit codes${RESET}
  0  All preconditions pass.
  1  One or more preconditions failed or required env vars are missing.

${BOLD}Dependencies${RESET}
  curl, jq (always required)
  psql (required only when IRONRAG_PG_DSN is set)
  python3 (required only with --with-bench)
EOF
    exit "$exit_code"
}

# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------
WITH_BENCH=0
for arg in "$@"; do
    case "$arg" in
        --help|-h) usage 0 ;;
        --with-bench) WITH_BENCH=1 ;;
        *)
            printf "${RED}Unknown argument: %s${RESET}\n" "$arg" >&2
            emit_failure_json
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Validate required env vars
# ---------------------------------------------------------------------------
errors=()
[[ -n "${IRONRAG_API_BASE_URL:-}" ]] || errors+=("IRONRAG_API_BASE_URL is required")
if [[ -z "${IRONRAG_API_TOKEN:-}" && -z "${IRONRAG_PROBE_PASSWORD:-}" ]]; then
    errors+=("IRONRAG_API_TOKEN or IRONRAG_PROBE_PASSWORD is required")
fi

if [[ ${#errors[@]} -gt 0 ]]; then
    for msg in "${errors[@]}"; do
        printf "${RED}ERROR: %s${RESET}\n" "$msg" >&2
    done
    echo >&2
    emit_failure_json
    usage 1
fi

BASE_URL="${IRONRAG_API_BASE_URL%/}"
PG_DSN="${IRONRAG_PG_DSN:-}"
EXPLICIT_LIBRARY_IDS="${IRONRAG_LIBRARY_IDS:-}"
LOGIN="${IRONRAG_LOGIN:-admin}"
COOKIE_JAR=""

# ---------------------------------------------------------------------------
# Dependency check
# ---------------------------------------------------------------------------
for cmd in curl jq; do
    if ! command -v "$cmd" &>/dev/null; then
        printf "${RED}ERROR: required tool '%s' not found in PATH${RESET}\n" "$cmd" >&2
        emit_failure_json
        exit 1
    fi
done

if [[ -n "$PG_DSN" ]] && ! command -v psql &>/dev/null; then
    fail "psql not found — IRONRAG_PG_DSN migration check cannot run"
    emit_failure_json "false"
    exit 1
fi

if [[ "$WITH_BENCH" -eq 1 ]] && ! command -v python3 &>/dev/null; then
    fail "python3 not found — --with-bench cannot run"
    emit_failure_json
    exit 1
fi

# ---------------------------------------------------------------------------
# Curl helpers (credentials never echoed)
# ---------------------------------------------------------------------------
if [[ -z "${IRONRAG_API_TOKEN:-}" ]]; then
    COOKIE_JAR="$(mktemp)"
    trap '[[ -n "$COOKIE_JAR" ]] && rm -f "$COOKIE_JAR"' EXIT

    login_payload=$(jq -n --arg login "$LOGIN" --arg password "$IRONRAG_PROBE_PASSWORD" \
        '{login: $login, password: $password}')
    curl --fail --silent --show-error \
        -c "$COOKIE_JAR" \
        -H "Content-Type: application/json" \
        -H "Accept: application/json" \
        --data "$login_payload" \
        "${BASE_URL}/v1/iam/session/login" >/dev/null || {
            printf "${RED}ERROR: failed to authenticate benchmark client with cookie session${RESET}\n" >&2
            emit_failure_json
            exit 1
        }
fi

api_get() {
    # $1 = path (starting with /)
    local path="$1"
    local auth_args=()
    if [[ -n "${IRONRAG_API_TOKEN:-}" ]]; then
        auth_args=(-H "Authorization: Bearer ${IRONRAG_API_TOKEN}")
    else
        auth_args=(-b "$COOKIE_JAR")
    fi
    curl --fail --silent --show-error \
        "${auth_args[@]}" \
        -H "Accept: application/json" \
        "${BASE_URL}${path}"
}

# ---------------------------------------------------------------------------
# Header
# ---------------------------------------------------------------------------
sep
printf "${BOLD}  IronRAG MCP-agent rollout precondition check${RESET}\n" >&2
printf "  target: %s\n" "$BASE_URL" >&2
sep

# ---------------------------------------------------------------------------
# Track overall readiness
# ---------------------------------------------------------------------------
READY=1   # 1 = still passing; 0 = at least one failure

# ---------------------------------------------------------------------------
# Precondition 1 — Migration version 6
# ---------------------------------------------------------------------------
printf "\n${BOLD}[1/3] Migration 0006_ui_mcp_agent${RESET}\n" >&2

MIGRATION_V6_STATUS="skipped"

if [[ -n "$PG_DSN" ]]; then
    mig_count=$(psql "$PG_DSN" -t -A -c \
        "SELECT COUNT(*) FROM _sqlx_migrations WHERE version = 6;" 2>&1) || {
        fail "psql query failed: $mig_count"
        MIGRATION_V6_STATUS="false"
        READY=0
        mig_count=0
    }
    mig_count="${mig_count//[[:space:]]/}"
    if [[ "$mig_count" == "1" ]]; then
        ok "migration version 6 is applied"
        MIGRATION_V6_STATUS="true"
    else
        fail "migration version 6 not found in _sqlx_migrations (count=${mig_count})"
        MIGRATION_V6_STATUS="false"
        READY=0
    fi
else
    warn "IRONRAG_PG_DSN not set — skipping DB migration check (HTTP-only mode)"
    MIGRATION_V6_STATUS='"skipped"'
fi

# ---------------------------------------------------------------------------
# Precondition 2 — Discover active libraries
# ---------------------------------------------------------------------------
printf "\n${BOLD}[2/3] QueryAnswer binding check per library${RESET}\n" >&2

declare -a LIBRARY_IDS=()

if [[ -n "$EXPLICIT_LIBRARY_IDS" ]]; then
    IFS=',' read -ra RAW_LIBRARY_IDS <<< "$EXPLICIT_LIBRARY_IDS"
    for raw_lib_id in "${RAW_LIBRARY_IDS[@]}"; do
        lib_id="$(trim "$raw_lib_id")"
        [[ -z "$lib_id" ]] && continue
        if ! is_uuid "$lib_id"; then
            fail "explicit library id is not a UUID: ${lib_id}"
            READY=0
            continue
        fi
        LIBRARY_IDS+=("$lib_id")
    done
    if [[ ${#LIBRARY_IDS[@]} -eq 0 ]]; then
        fail "explicit library list did not contain any non-empty ids"
        READY=0
    else
        info "using explicit library list (${#LIBRARY_IDS[@]} entries)"
    fi
else
    info "discovering active libraries via API..."
    # Enumerate workspaces, then libraries per workspace
    workspaces_json=$(api_get "/v1/catalog/workspaces") || {
        fail "failed to GET /v1/catalog/workspaces"
        emit_failure_json "$MIGRATION_V6_STATUS"
        exit 1
    }
    workspace_ids=$(printf '%s' "$workspaces_json" | jq -r 'if type == "array" then .[] | .id // empty else error("workspaces response must be array") end' 2>/dev/null) || {
        fail "failed to parse workspaces response"
        emit_failure_json "$MIGRATION_V6_STATUS"
        exit 1
    }

    while IFS= read -r ws_id; do
        [[ -z "$ws_id" ]] && continue
        if ! is_uuid "$ws_id"; then
            fail "discovered workspace id is not a UUID: ${ws_id}"
            READY=0
            continue
        fi
        libs_json=$(api_get "/v1/catalog/workspaces/${ws_id}/libraries") || {
            fail "failed to GET libraries for workspace ${ws_id}"
            READY=0
            continue
        }
        library_ids_for_workspace=$(printf '%s' "$libs_json" | jq -r 'if type == "array" then .[] | select(.lifecycleState == "active") | .id // empty else error("libraries response must be array") end' 2>/dev/null) || {
            fail "failed to parse libraries response for workspace ${ws_id}"
            READY=0
            continue
        }
        while IFS= read -r lib_id; do
            [[ -z "$lib_id" ]] && continue
            if ! is_uuid "$lib_id"; then
                fail "discovered active library id is not a UUID: ${lib_id}"
                READY=0
                continue
            fi
            LIBRARY_IDS+=("$lib_id")
        done <<< "$library_ids_for_workspace"
    done <<< "$workspace_ids"

    info "found ${#LIBRARY_IDS[@]} active library/libraries"
fi

if [[ ${#LIBRARY_IDS[@]} -eq 0 ]]; then
    warn "no active libraries found — query_answer binding check skipped"
fi

# For each library, check canonical UI query readiness. Cookie-auth runs use
# /v1/iam/session/resolve because that is the browser shell source of truth and
# includes inherited effective bindings. Bearer-only HTTP checks cannot call the
# browser shell resolver, so they fall back to a direct query_answer binding check.
LIBRARIES_JSON="["
FIRST_LIBRARY_ID=""
SHELL_LIBRARIES_JSON=""

if [[ -n "$COOKIE_JAR" ]]; then
    shell_json=$(api_get "/v1/iam/session/resolve") || {
        fail "failed to GET /v1/iam/session/resolve"
        emit_failure_json "$MIGRATION_V6_STATUS"
        exit 1
    }
    SHELL_LIBRARIES_JSON=$(printf '%s' "$shell_json" | jq -c '(.shellBootstrap.libraries // []) as $libs | if ($libs | type) == "array" then $libs else error("shell bootstrap libraries must be array") end' 2>/dev/null) || {
        fail "failed to parse shell bootstrap library readiness"
        emit_failure_json "$MIGRATION_V6_STATUS"
        exit 1
    }
fi

for lib_id in "${LIBRARY_IDS[@]}"; do
    [[ -z "$FIRST_LIBRARY_ID" ]] && FIRST_LIBRARY_ID="$lib_id"

    if [[ -n "$SHELL_LIBRARIES_JSON" ]]; then
        shell_readiness=$(printf '%s' "$SHELL_LIBRARIES_JSON" | jq -c \
            --arg lib_id "$lib_id" \
            '([.[] | select(.id == $lib_id)][0] // null) as $lib |
             if $lib == null then
               {query_ready: false, missing: []}
             elif (($lib.queryReady // false) | type) != "boolean" then
               error("queryReady must be boolean")
             else
               ($lib.missingBindingPurposes // []) as $missing |
               if ($missing | type) == "array" then
                 {query_ready: ($lib.queryReady // false), missing: $missing}
               else
                 error("missingBindingPurposes must be array")
               end
             end' 2>/dev/null) || {
            fail "library ${lib_id}: failed to parse shell query readiness"
            LIBRARIES_JSON+='{"id":"'"$lib_id"'","query_ready":false},'
            READY=0
            continue
        }
        query_ready=$(printf '%s' "$shell_readiness" | jq -r '.query_ready')
        missing=$(printf '%s' "$shell_readiness" | jq -c '.missing')
        if [[ "$query_ready" == "true" ]]; then
            ok "library ${lib_id}: shell query readiness is true"
            LIBRARIES_JSON+='{"id":"'"$lib_id"'","query_ready":true},'
        else
            fail "library ${lib_id}: shell query readiness is false; missing=${missing}"
            LIBRARIES_JSON+='{"id":"'"$lib_id"'","query_ready":false},'
            READY=0
        fi
        continue
    fi

    warn "library ${lib_id}: bearer HTTP mode checks only direct library-scope query_answer bindings"
    bindings_json=$(api_get "/v1/ai/bindings?scopeKind=library&libraryId=${lib_id}") || {
        fail "library ${lib_id}: failed to GET /v1/ai/bindings"
        LIBRARIES_JSON+='{"id":"'"$lib_id"'","query_ready":false},'
        READY=0
        continue
    }

    has_query_answer=$(printf '%s' "$bindings_json" | jq -r \
        'if type == "array" then [.[] | select(.bindingPurpose == "query_answer" and .bindingState == "active")] | length else error("bindings response must be array") end' 2>/dev/null) || {
        fail "library ${lib_id}: failed to parse /v1/ai/bindings response"
        LIBRARIES_JSON+='{"id":"'"$lib_id"'","query_ready":false},'
        READY=0
        continue
    }

    if [[ "$has_query_answer" -gt 0 ]]; then
        ok "library ${lib_id}: active query_answer binding found (${has_query_answer})"
        LIBRARIES_JSON+='{"id":"'"$lib_id"'","query_ready":true},'
    else
        fail "library ${lib_id}: no active query_answer binding"
        LIBRARIES_JSON+='{"id":"'"$lib_id"'","query_ready":false},'
        READY=0
    fi
done

# Trim trailing comma
LIBRARIES_JSON="${LIBRARIES_JSON%,}]"

# ---------------------------------------------------------------------------
# Precondition 3 — Latency benchmark (optional)
# ---------------------------------------------------------------------------
printf "\n${BOLD}[3/3] Latency benchmark (rollout canary p95 ≤ 25 000 ms)${RESET}\n" >&2

BENCH_P95_MS="null"
BENCH_P95_GATE_MS="null"
BENCH_GATE_PASSED="null"
BENCH_SUCCESSES="null"
BENCH_FAILURES="null"

if [[ "$WITH_BENCH" -eq 0 ]]; then
    info "skipped (pass --with-bench to enable)"
else
    # Resolve which library to bench against
    bench_lib="${IRONRAG_LIBRARY_ID:-${FIRST_LIBRARY_ID:-}}"
    if [[ -z "$bench_lib" ]]; then
        fail "no library available for bench"
        READY=0
    elif ! is_uuid "$bench_lib"; then
        fail "bench library id is not a UUID: ${bench_lib}"
        READY=0
    else
        bench_script_dir="$(cd "$(dirname "$0")/../.." && pwd)"
        bench_script="${bench_script_dir}/scripts/bench/agent_turn_p95.py"

        if [[ ! -f "$bench_script" ]]; then
            fail "bench script not found at ${bench_script}"
            READY=0
        else
            info "running latency benchmark against library ${bench_lib}..."
            bench_args=(--p95-gate-ms 25000)
            if [[ -n "${IRONRAG_BENCH_OUTPUT_PATH:-}" ]]; then
                bench_args+=(--output-path "$IRONRAG_BENCH_OUTPUT_PATH")
            fi
            bench_env=(IRONRAG_API_BASE_URL="$BASE_URL" IRONRAG_LIBRARY_ID="$bench_lib")
            if [[ -n "${IRONRAG_API_TOKEN:-}" ]]; then
                bench_env+=(IRONRAG_API_TOKEN="$IRONRAG_API_TOKEN")
            else
                bench_env+=(IRONRAG_LOGIN="$LOGIN" IRONRAG_PROBE_PASSWORD="$IRONRAG_PROBE_PASSWORD")
            fi
            bench_output=$(env "${bench_env[@]}" \
                           python3 "$bench_script" "${bench_args[@]}" 2>/dev/null) && bench_exit=0 || bench_exit=$?

            bench_json=$(printf '%s' "$bench_output" | tail -1)
            BENCH_P95_MS=$(printf '%s' "$bench_json" | jq -r 'if ((.p95_ms | type) == "number" and .p95_ms >= 0) then .p95_ms else "null" end' 2>/dev/null) || BENCH_P95_MS="null"
            BENCH_P95_GATE_MS=$(printf '%s' "$bench_json" | jq -r 'if ((.p95_gate_ms | type) == "number" and .p95_gate_ms >= 0) then .p95_gate_ms else "null" end' 2>/dev/null) || BENCH_P95_GATE_MS="null"
            BENCH_GATE_PASSED=$(printf '%s' "$bench_json" | jq -r 'if (.gate_passed | type) == "boolean" then .gate_passed else "null" end' 2>/dev/null) || BENCH_GATE_PASSED="null"
            BENCH_SUCCESSES=$(printf '%s' "$bench_json" | jq -r 'if ((.successes | type) == "number" and .successes >= 0 and .successes == (.successes | floor)) then .successes else "null" end' 2>/dev/null) || BENCH_SUCCESSES="null"
            BENCH_FAILURES=$(printf '%s' "$bench_json" | jq -r 'if ((.failures | type) == "number" and .failures >= 0 and .failures == (.failures | floor)) then .failures else "null" end' 2>/dev/null) || BENCH_FAILURES="null"

            if [[ "$bench_exit" -eq 0 && "$BENCH_GATE_PASSED" == "true" && "$BENCH_P95_MS" != "null" && "$BENCH_P95_GATE_MS" != "null" && "$BENCH_SUCCESSES" != "null" && "$BENCH_SUCCESSES" != "0" && "$BENCH_FAILURES" == "0" ]]; then
                ok "p95 latency = ${BENCH_P95_MS} ms; successes=${BENCH_SUCCESSES}, failures=${BENCH_FAILURES} (within gate)"
            else
                fail "p95 latency = ${BENCH_P95_MS} ms; successes=${BENCH_SUCCESSES}, failures=${BENCH_FAILURES} (gate breach or bench error, exit=${bench_exit})"
                READY=0
            fi
        fi
    fi
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------
sep
if [[ "$READY" -eq 1 ]]; then
    READY_BOOL="true"
    printf "\n${GREEN}${BOLD}  RESULT: ready_to_rollout = true${RESET}\n\n" >&2
else
    READY_BOOL="false"
    printf "\n${RED}${BOLD}  RESULT: ready_to_rollout = false — one or more checks failed${RESET}\n\n" >&2
fi
sep

# ---------------------------------------------------------------------------
# JSON summary to stdout
# ---------------------------------------------------------------------------
printf '{"migration_v6":%s,"libraries":%s,"bench_p95_ms":%s,"bench_p95_gate_ms":%s,"bench_gate_passed":%s,"bench_successes":%s,"bench_failures":%s,"ready_to_rollout":%s}\n' \
    "$MIGRATION_V6_STATUS" \
    "$LIBRARIES_JSON" \
    "$BENCH_P95_MS" \
    "$BENCH_P95_GATE_MS" \
    "$BENCH_GATE_PASSED" \
    "$BENCH_SUCCESSES" \
    "$BENCH_FAILURES" \
    "$READY_BOOL"

[[ "$READY_BOOL" == "true" ]] && exit 0 || exit 1
