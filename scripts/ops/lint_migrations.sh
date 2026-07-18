#!/usr/bin/env bash
# lint_migrations.sh — CI-ready migration policy linter for IronRAG
# Checks:
#   1. Frozen-migration integrity (released files must match the selected base ref)
#   2. Idempotency of every changed or unreleased migration
#   3. Sequential numbering (no skipped migration numbers)
#   4. Filename convention (NNNN_descriptive_name.sql)
#
# Exit codes: 0 = all FAIL-class checks pass, 1 = at least one FAIL, 2 = usage error

set -euo pipefail

SELF_PATH="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"

# ── colour helpers ────────────────────────────────────────────────────────────
RED='\033[0;31m'; YELLOW='\033[0;33m'; GREEN='\033[0;32m'
CYAN='\033[0;36m'; BOLD='\033[1m'; RESET='\033[0m'
if [ ! -t 2 ]; then RED=''; YELLOW=''; GREEN=''; CYAN=''; BOLD=''; RESET=''; fi

err()  { printf "${RED}[FAIL]${RESET}  %s\n" "$*" >&2; }
warn() { printf "${YELLOW}[WARN]${RESET}  %s\n" "$*" >&2; }
ok()   { printf "${GREEN}[OK]${RESET}    %s\n" "$*" >&2; }
info() { printf "${CYAN}[INFO]${RESET}  %s\n" "$*" >&2; }

# ── flags ─────────────────────────────────────────────────────────────────────
STRICT=0; FIX=0; SELF_TEST=0
BASE_REF="${IRONRAG_MIGRATION_BASE_REF:-}"

usage() {
  cat >&2 <<EOF
${BOLD}Usage:${RESET} lint_migrations.sh --base-ref REF [--strict] [--fix] [--self-test] [--help]

Checks migration policy compliance for IronRAG:
  1. Frozen-migration integrity  — released files must match REF byte-for-byte
  2. Idempotency                 — every changed/unreleased migration must be rerunnable
  3. Sequential numbering        — no skipped migration numbers
  4. Filename convention         — NNNN_descriptive_name.sql format

Options:
  --strict     Treat WARN-class findings as FAIL (non-zero exit)
  --fix        Print suggested idempotency patches to stderr
  --base-ref   Local Git commit/ref used as the frozen migration baseline
  --self-test  Run against a synthetic invalid migration; exit 0 if linter detects it
  --help       Print this help and exit 0

Environment fallback: IRONRAG_MIGRATION_BASE_REF

Output: human-readable coloured summary to stderr, JSON line summary to stdout.
EOF
  exit 0
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --strict)    STRICT=1 ;;
    --fix)       FIX=1 ;;
    --self-test) SELF_TEST=1 ;;
    --base-ref)
      shift
      if [ "$#" -eq 0 ] || [ -z "$1" ]; then
        printf "--base-ref requires a non-empty value.\n" >&2
        exit 2
      fi
      BASE_REF="$1"
      ;;
    --base-ref=*)
      BASE_REF="${1#--base-ref=}"
      if [ -z "$BASE_REF" ]; then
        printf "--base-ref requires a non-empty value.\n" >&2
        exit 2
      fi
      ;;
    --help|-h)   usage ;;
    *) printf "Unknown option: %s\n" "$1" >&2; exit 2 ;;
  esac
  shift
done

# ── repo root ─────────────────────────────────────────────────────────────────
REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
  printf "Not inside a git repository.\n" >&2; exit 2
}
MIGRATIONS_DIR="$REPO_ROOT/apps/api/migrations"

# ── self-test mode ────────────────────────────────────────────────────────────
if [ "$SELF_TEST" -eq 1 ]; then
  info "Running self-test with synthetic invalid migration …"

  # Isolate the sandbox from any git state inherited from the invoking
  # process (e.g. a pre-commit hook subprocess sets GIT_DIR/GIT_WORK_TREE/
  # GIT_INDEX_FILE pointing at the real repo). Without this, `git init`
  # below "re-inits" the outer repo instead of creating a fresh one, and the
  # `git add`/`git commit`/`git config user.*` calls silently write synthetic
  # fixture content and identity into the real repo's index/config.
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_CEILING_DIRECTORIES \
        GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR

  TMPDIR_ST="$(mktemp -d)"
  trap 'rm -rf "$TMPDIR_ST"' EXIT

  # Build a fake repo
  FAKE_REPO="$TMPDIR_ST/repo"
  mkdir -p "$FAKE_REPO/apps/api/migrations"
  cd "$FAKE_REPO"
  git init -q -b master
  git config user.email "lint@test"
  git config user.name "Lint Test"

  # Regression guard: fail loudly (instead of silently touching the
  # invoking repository) if the sandbox somehow is not actually isolated.
  SANDBOX_TOPLEVEL="$(git rev-parse --show-toplevel)"
  case "$SANDBOX_TOPLEVEL" in
    "$TMPDIR_ST"/*) ;;
    *)
      err "Self-test sandbox is not isolated: git toplevel resolved to '$SANDBOX_TOPLEVEL' instead of a path under '$TMPDIR_ST'. Refusing to continue to avoid corrupting the invoking repository."
      exit 1
      ;;
  esac

  # Released migration (will form gh/master)
  cat > apps/api/migrations/0001_init.sql <<'SQLE'
create table if not exists foo (id uuid primary key);
SQLE

  git add .
  git commit -q -m "initial"

  # Create a bare clone to serve as the "gh" remote
  BARE_REMOTE="$TMPDIR_ST/gh-bare"
  git clone -q --bare "$FAKE_REPO" "$BARE_REMOTE"
  # Ensure the bare clone has a branch called master
  git -C "$BARE_REMOTE" symbolic-ref HEAD refs/heads/master 2>/dev/null || true

  git remote add gh "$BARE_REMOTE"
  git fetch -q gh

  # Candidate pre-release migration: skipped 0002 and uppercase filename test
  # sequencing, naming, and idempotency. The guarded INSERT deliberately keeps
  # ON CONFLICT more than four lines from INSERT INTO to cover multiline SQL.
  cat > apps/api/migrations/0003_Bad.sql <<'SQLE'
-- BAD: no IF NOT EXISTS, non-idempotent
create table bar (id uuid primary key);
create index bar_idx on bar(id);
alter table bar add column name text;
alter type my_enum add value 'new_val';
insert into unguarded_plain values (gen_random_uuid());
insert into guarded_multiline (
  id,
  name
)
select
  gen_random_uuid(),
  'guarded'
on conflict (id) do nothing;
insert into unguarded_trailing values (gen_random_uuid()); /* trailing block comment */
insert into guarded_after_trailing (id)
values (gen_random_uuid())
on conflict (id) do nothing;
insert into guarded_comment_tokens values (1) on/* token comment */conflict do nothing;
insert into unguarded_commented values (gen_random_uuid())
-- A comment is not an idempotency guard: on conflict do nothing
;
insert into unguarded_literal values ('literal on conflict is not a guard');
insert into guarded_same_line values (1) on conflict do nothing; insert into unguarded_same_line values (2);
/* leading comment */ insert into unguarded_leading_comment values (1);
insert into guarded$archive$ values (1) on conflict do nothing;
with source_row as (select 1) insert into unguarded_cte select * from source_row;
with source_row as (select 1) insert into guarded_cte select * from source_row on conflict do nothing;
do $do_body$
begin
  insert into executed_do_body values (1);
end
$do_body$;
do 'begin insert into hidden_single_quote_do values (1); end';
/*
insert into ignored_block_comment values (1);
*/
create or replace function guarded_function_body() returns void language plpgsql as $body$
begin
  insert into ignored_function_body values (1);
end
$body$;
create function myfunc() returns void language sql as '$$select 1$$';
SQLE

  # A later clean migration proves the linter scans every changed migration,
  # rather than checking only the highest-numbered candidate.
  cat > apps/api/migrations/0004_clean.sql <<'SQLE'
create table if not exists clean_candidate (id uuid primary key);
SQLE

  # Tamper with a released file locally (after gh/master is set)
  echo "-- tampered" >> apps/api/migrations/0001_init.sql

  # Run ourselves against this fake repo
  LINT_EXIT=0
  LINT_OUTPUT="$(bash "$SELF_PATH" --strict --base-ref gh/master 2>/dev/null)" || LINT_EXIT=$?
  if [ "$LINT_EXIT" -eq 0 ]; then
    err "Self-test FAILED: linter should have exited non-zero but exited 0"
    exit 1
  fi
  if ! printf '%s' "$LINT_OUTPUT" | grep -q '"warn_count":9,'; then
    err "Self-test FAILED: earlier changed migration was not fully scanned"
    exit 1
  fi
  if ! printf '%s' "$LINT_OUTPUT" | grep -q '"file":"0003_Bad.sql"'; then
    err "Self-test FAILED: defect in the earlier of two new migrations was missed"
    exit 1
  fi
  if ! printf '%s' "$LINT_OUTPUT" | grep -q \
    '"check":"frozen_migration","file":"0001_init.sql"'; then
    err "Self-test FAILED: modified frozen migration was not detected"
    exit 1
  fi
  if printf '%s' "$LINT_OUTPUT" | grep -q '"file":"0004_clean.sql"'; then
    err "Self-test FAILED: clean later migration produced a false positive"
    exit 1
  fi
  INSERT_FINDING_COUNT="$({
    printf '%s' "$LINT_OUTPUT" \
      | grep -o '"message":"INSERT INTO without ON CONFLICT clause at line [0-9]*"' \
      || true
  } | wc -l | tr -d '[:space:]')"
  if [ "$INSERT_FINDING_COUNT" -ne 8 ]; then
    err "Self-test FAILED: expected 8 unguarded INSERT findings, got $INSERT_FINDING_COUNT"
    exit 1
  fi
  for marker in \
    unguarded_plain \
    unguarded_trailing \
    unguarded_commented \
    unguarded_literal \
    unguarded_same_line \
    unguarded_leading_comment \
    unguarded_cte
  do
    expected_line="$(grep -n "insert into $marker" apps/api/migrations/0003_Bad.sql | head -n 1 | cut -d: -f1)"
    if ! printf '%s' "$LINT_OUTPUT" | grep -q \
      "INSERT INTO without ON CONFLICT clause at line $expected_line\""; then
      err "Self-test FAILED: missing unguarded INSERT finding for $marker"
      exit 1
    fi
  done
  do_body_line="$(grep -n '^begin$' apps/api/migrations/0003_Bad.sql | head -n 1 | cut -d: -f1)"
  if ! printf '%s' "$LINT_OUTPUT" | grep -q \
    "INSERT INTO without ON CONFLICT clause at line $do_body_line\""; then
    err "Self-test FAILED: executable DO body was not inspected"
    exit 1
  fi
  single_quote_do_line="$(grep -n "^do 'begin" apps/api/migrations/0003_Bad.sql | head -n 1 | cut -d: -f1)"
  if ! printf '%s' "$LINT_OUTPUT" | grep -q \
    "Single-quoted DO body cannot be inspected at line $single_quote_do_line\""; then
    err "Self-test FAILED: opaque executable DO body did not fail closed"
    exit 1
  fi
  for marker in ignored_block_comment ignored_function_body; do
    ignored_line="$(grep -n "insert into $marker" apps/api/migrations/0003_Bad.sql | head -n 1 | cut -d: -f1)"
    if printf '%s' "$LINT_OUTPUT" | grep -q \
      "INSERT INTO without ON CONFLICT clause at line $ignored_line\""; then
      err "Self-test FAILED: non-executable INSERT was reported for $marker"
      exit 1
    fi
  done

  NO_BASE_EXIT=0
  NO_BASE_OUTPUT="$(env -u IRONRAG_MIGRATION_BASE_REF bash "$SELF_PATH" --strict 2>&1)" \
    || NO_BASE_EXIT=$?
  if [ "$NO_BASE_EXIT" -ne 2 ] \
    || ! printf '%s' "$NO_BASE_OUTPUT" | grep -q 'Base ref is required'; then
    err "Self-test FAILED: omitted base ref did not fail closed as a usage error"
    exit 1
  fi

  MISSING_REF_EXIT=0
  MISSING_REF_OUTPUT="$(bash "$SELF_PATH" --strict --base-ref refs/heads/missing 2>&1)" \
    || MISSING_REF_EXIT=$?
  if [ "$MISSING_REF_EXIT" -ne 2 ] \
    || ! printf '%s' "$MISSING_REF_OUTPUT" | grep -q 'Base ref does not exist'; then
    err "Self-test FAILED: missing base ref did not fail closed with a focused error"
    exit 1
  fi

  rm apps/api/migrations/0001_init.sql
  DELETED_EXIT=0
  DELETED_OUTPUT="$(bash "$SELF_PATH" --strict --base-ref gh/master 2>/dev/null)" \
    || DELETED_EXIT=$?
  if [ "$DELETED_EXIT" -eq 0 ] \
    || ! printf '%s' "$DELETED_OUTPUT" | grep -q \
      '"check":"frozen_migration","file":"0001_init.sql","message":"Migration from gh/master was deleted"'; then
    err "Self-test FAILED: deleted frozen migration was not detected"
    exit 1
  fi
  ok "Self-test passed: SQL guard scanner separates code, literals, and comments"
  exit 0
fi

if [ -z "$BASE_REF" ]; then
  err "Base ref is required; pass --base-ref or set IRONRAG_MIGRATION_BASE_REF"
  exit 2
fi
if ! BASE_COMMIT="$(git -C "$REPO_ROOT" rev-parse --verify --end-of-options "${BASE_REF}^{commit}" 2>/dev/null)"; then
  err "Base ref does not exist: $BASE_REF"
  exit 2
fi
info "Using migration base ref $BASE_REF ($BASE_COMMIT)"

# ── collect local migration files ─────────────────────────────────────────────
mapfile -t LOCAL_FILES < <(find "$MIGRATIONS_DIR" -maxdepth 1 -name '*.sql' | sort)

if [ ${#LOCAL_FILES[@]} -eq 0 ]; then
  warn "No migration files found in $MIGRATIONS_DIR"
fi

# ── resolve selected-base migration set ──────────────────────────────────────
declare -A BASE_BLOBS   # basename -> blob-sha
declare -A BASE_PATHS   # basename -> full repo-relative path
while IFS= read -r line; do
  # format: "100644 blob <sha>\t<path>"
  sha="$(printf '%s' "$line" | awk '{print $3}')"
  path="$(printf '%s' "$line" | cut -f2)"
  base="$(basename "$path")"
  BASE_BLOBS["$base"]="$sha"
  BASE_PATHS["$base"]="$path"
done < <(git -C "$REPO_ROOT" ls-tree -r "$BASE_COMMIT" -- apps/api/migrations/)

# ── tracking ─────────────────────────────────────────────────────────────────
FAIL_COUNT=0
WARN_COUNT=0
declare -a FINDINGS=()  # JSON objects

json_str() {
  # Minimal JSON string escaping for pure bash (no python dep)
  local s="$1"
  s="${s//\\/\\\\}"   # backslash
  s="${s//\"/\\\"}"   # double-quote
  s="${s//$'\n'/\\n}" # newline
  s="${s//$'\t'/\\t}" # tab
  printf '"%s"' "$s"
}

record_fail() {
  local check="$1" file="$2" msg="$3"
  FAIL_COUNT=$(( FAIL_COUNT + 1 ))
  FINDINGS+=("{\"level\":\"FAIL\",\"check\":$(json_str "$check"),\"file\":$(json_str "$file"),\"message\":$(json_str "$msg")}")
}

record_warn() {
  local check="$1" file="$2" msg="$3"
  WARN_COUNT=$(( WARN_COUNT + 1 ))
  FINDINGS+=("{\"level\":\"WARN\",\"check\":$(json_str "$check"),\"file\":$(json_str "$file"),\"message\":$(json_str "$msg")}")
  if [ "$STRICT" -eq 1 ]; then
    FAIL_COUNT=$(( FAIL_COUNT + 1 ))
  fi
}

# ── Check 4: filename convention ─────────────────────────────────────────────
info "Check 4: filename convention"
for f in "${LOCAL_FILES[@]}"; do
  base="$(basename "$f")"
  if ! printf '%s' "$base" | grep -qE '^[0-9]{4}_[a-z][a-z0-9_]+\.sql$'; then
    err "Filename convention: '$base' does not match ^[0-9]{4}_[a-z][a-z0-9_]+\\.sql\$"
    record_fail "filename_convention" "$base" "Does not match ^[0-9]{4}_[a-z][a-z0-9_]+.sql$"
  fi
done

# ── Check 3: sequential numbering ────────────────────────────────────────────
info "Check 3: sequential numbering"
PREV_NUM=0
for f in "${LOCAL_FILES[@]}"; do
  base="$(basename "$f")"
  NUM="${base:0:4}"
  # strip leading zeros for arithmetic
  N=$(( 10#$NUM ))
  if [ "$PREV_NUM" -gt 0 ] && [ "$N" -ne $(( PREV_NUM + 1 )) ]; then
    EXPECTED=$(printf "%04d" $(( PREV_NUM + 1 )))
    err "Sequential numbering: gap detected — expected ${EXPECTED}_*.sql before $base"
    record_fail "sequential_numbering" "$base" "Gap: expected ${EXPECTED}_*.sql before $base"
  fi
  PREV_NUM=$N
done

# ── Check 1: frozen-migration integrity ──────────────────────────────────────
info "Check 1: frozen-migration integrity"
declare -A LOCAL_PATHS
declare -a CHANGED_MIGRATIONS=()

for f in "${LOCAL_FILES[@]}"; do
  base="$(basename "$f")"
  LOCAL_PATHS["$base"]="$f"
  if [ -z "${BASE_BLOBS[$base]+_}" ]; then
    CHANGED_MIGRATIONS+=("$f")
    info "Unreleased migration: $base"
    continue
  fi

  BASE_SHA="${BASE_BLOBS[$base]}"
  LOCAL_SHA="$(git -C "$REPO_ROOT" hash-object "$f")"
  if [ "$LOCAL_SHA" = "$BASE_SHA" ]; then
    ok "Frozen OK: $base"
    continue
  fi

  CHANGED_MIGRATIONS+=("$f")
  err "Frozen migration modified: $base (local blob $LOCAL_SHA differs from $BASE_REF blob $BASE_SHA)"
  git -C "$REPO_ROOT" --no-pager diff --no-ext-diff "$BASE_COMMIT" -- "${BASE_PATHS[$base]}" >&2 || true
  record_fail "frozen_migration" "$base" "Local blob $LOCAL_SHA differs from $BASE_REF blob $BASE_SHA"
done

for base in "${!BASE_BLOBS[@]}"; do
  if [ -z "${LOCAL_PATHS[$base]+_}" ]; then
    err "Frozen migration deleted: $base exists in $BASE_REF but not in the worktree"
    record_fail "frozen_migration" "$base" "Migration from $BASE_REF was deleted"
  fi
done

# ── Check 2: idempotency ─────────────────────────────────────────────────────
info "Check 2: idempotency of changed and unreleased migrations"
if [ ${#CHANGED_MIGRATIONS[@]} -eq 0 ]; then
  info "No changed or unreleased migration files found — idempotency check skipped"
else
  # Lex the complete migration once per policy rule and emit
  #   <start-line><TAB><executable SQL>
  # for every top-level statement. Strings, quoted identifiers,
  # dollar-quoted bodies, and comments are replaced with whitespace so guard
  # keywords inside them cannot satisfy the policy. Scanning the full file is
  # important: it handles leading comments and multiple statements per line
  # without losing lexical state from the previous line.
  scan_sql_statements() {
    local migration_file="$1"
    awk '
      function append_space() {
        if (statement != "" && substr(statement, length(statement), 1) != " ") {
          statement = statement " "
        }
      }

      function emit_statement() {
        gsub(/[[:space:]]+/, " ", statement)
        sub(/^ /, "", statement)
        sub(/ $/, "", statement)
        if (statement != "") print statement_start "\t" statement
        statement = ""
        statement_start = 0
      }

      {
        line = $0
        i = 1
        while (i <= length(line)) {
          c = substr(line, i, 1)
          next_c = substr(line, i + 1, 1)

          if (dollar_tag != "") {
            if (substr(line, i, length(dollar_tag)) == dollar_tag) {
              i += length(dollar_tag)
              dollar_tag = ""
            } else {
              i++
            }
            continue
          }

          if (block_depth > 0) {
            if (c == "/" && next_c == "*") {
              block_depth++
              i += 2
            } else if (c == "*" && next_c == "/") {
              block_depth--
              i += 2
              if (block_depth == 0) append_space()
            } else {
              i++
            }
            continue
          }

          if (single_quote) {
            if (c == "\047" && next_c == "\047") {
              i += 2
            } else if (c == "\047") {
              single_quote = 0
              append_space()
              i++
            } else if (single_backslash && c == "\\") {
              i += 2
            } else {
              i++
            }
            continue
          }

          if (double_quote) {
            if (c == "\"" && next_c == "\"") {
              i += 2
            } else if (c == "\"") {
              double_quote = 0
              append_space()
              i++
            } else {
              i++
            }
            continue
          }

          # A DO body executes during the migration, unlike a stored function
          # body. Parse its contents as executable statements and close it only
          # on the matching outer dollar tag while outside nested lexical
          # regions.
          if (outer_do_tag != "" && substr(line, i, length(outer_do_tag)) == outer_do_tag) {
            emit_statement()
            i += length(outer_do_tag)
            outer_do_tag = ""
            continue
          }

          if (c == "-" && next_c == "-") {
            append_space()
            break
          }
          if (c == "/" && next_c == "*") {
            append_space()
            block_depth = 1
            i += 2
            continue
          }
          if (c == "\047") {
            previous = i > 1 ? substr(line, i - 1, 1) : ""
            before_previous = i > 2 ? substr(line, i - 2, 1) : ""
            single_backslash = previous ~ /[eE]/ && before_previous !~ /[[:alnum:]_$]/
            single_quote = 1
            append_space()
            i++
            continue
          }
          if (c == "\"") {
            double_quote = 1
            append_space()
            i++
            continue
          }
          if (c == "$") {
            rest = substr(line, i)
            previous = i > 1 ? substr(line, i - 1, 1) : ""
            if (previous !~ /[[:alnum:]_$]/ && (match(rest, /^\$\$/) || match(rest, /^\$[A-Za-z_][A-Za-z0-9_]*\$/))) {
              matched_tag = substr(rest, RSTART, RLENGTH)
              statement_prefix = tolower(statement)
              gsub(/[[:space:]]+/, " ", statement_prefix)
              sub(/^ /, "", statement_prefix)
              sub(/ $/, "", statement_prefix)
              if (outer_do_tag == "" && statement_prefix ~ /^do([[:space:]]|$)/) {
                outer_do_tag = matched_tag
                statement = ""
                statement_start = 0
              } else {
                dollar_tag = matched_tag
                append_space()
              }
              i += length(matched_tag)
              continue
            }
          }
          if (c == ";") {
            emit_statement()
            i++
            continue
          }

          if (c ~ /[[:space:]]/) {
            append_space()
          } else {
            if (statement_start == 0) statement_start = NR
            statement = statement c
          }
          i++
        }
        append_space()
      }
      END { emit_statement() }
    ' "$migration_file"
  }

  # Helper: check a pattern and report
  check_pattern() {
    local migration_file="$1"
    local migration_name="$2"
    local sql_statement_output="$3"
    local level="$4"   # FAIL or WARN
    local desc="$5"
    local fix_hint="$6"
    local pattern="$7"
    local anti_pattern="$8"
    local lineno statement context line_text

    while IFS=$'\t' read -r lineno statement; do
      context="$(printf '%s' "$statement" | tr '[:upper:]' '[:lower:]')"
      if printf '%s' "$context" | grep -qiE "$pattern"; then
        if [ -n "$anti_pattern" ] && printf '%s' "$context" | grep -qiE "$anti_pattern"; then
          continue
        fi
        line_text="$(sed -n "${lineno}p" "$migration_file" | sed 's/^[[:space:]]*//;s/[[:space:]]*$//')"
        if [ "$level" = "FAIL" ]; then
          err "Idempotency [$migration_name:$lineno]: $desc"
          err "  Line: $line_text"
          [ "$FIX" -eq 1 ] && printf "${YELLOW}  Suggested fix: %s${RESET}\n" "$fix_hint" >&2
          record_fail "idempotency" "$migration_name" "$desc at line $lineno"
        else
          warn "Idempotency [$migration_name:$lineno]: $desc"
          warn "  Line: $line_text"
          [ "$FIX" -eq 1 ] && printf "${YELLOW}  Suggested fix: %s${RESET}\n" "$fix_hint" >&2
          record_warn "idempotency" "$migration_name" "$desc at line $lineno"
        fi
      fi
    done <<< "$sql_statement_output"
  }

  for MIGRATION_FILE in "${CHANGED_MIGRATIONS[@]}"; do
    BASE="$(basename "$MIGRATION_FILE")"
    info "Checking migration: $BASE"

    SQL_STATEMENT_OUTPUT=""
    if ! SQL_STATEMENT_OUTPUT="$(scan_sql_statements "$MIGRATION_FILE")"; then
      err "SQL statement scanner failed for $BASE"
      record_fail "idempotency_scanner" "$BASE" "Could not lex migration"
    elif [ -z "$SQL_STATEMENT_OUTPUT" ]; then
      err "SQL statement scanner produced no executable statements for $BASE"
      record_fail "idempotency_scanner" "$BASE" "Migration has no lexed statements"
    fi

    # FAIL-class checks
    check_pattern "$MIGRATION_FILE" "$BASE" "$SQL_STATEMENT_OUTPUT" FAIL \
      "CREATE TABLE without IF NOT EXISTS" \
      "Add IF NOT EXISTS after TABLE keyword" \
      "(^|[[:space:]])create[[:space:]]+table[[:space:]]" \
      "if\s+not\s+exists"

    check_pattern "$MIGRATION_FILE" "$BASE" "$SQL_STATEMENT_OUTPUT" FAIL \
      "CREATE INDEX without IF NOT EXISTS" \
      "Add IF NOT EXISTS after INDEX keyword" \
      "(^|[[:space:]])create[[:space:]]+(unique[[:space:]]+)?index[[:space:]]" \
      "if\s+not\s+exists"

    check_pattern "$MIGRATION_FILE" "$BASE" "$SQL_STATEMENT_OUTPUT" FAIL \
      "ALTER TYPE ... ADD VALUE without IF NOT EXISTS" \
      "Add IF NOT EXISTS after ADD VALUE" \
      "add\s+value\s" \
      "if\s+not\s+exists"

    check_pattern "$MIGRATION_FILE" "$BASE" "$SQL_STATEMENT_OUTPUT" FAIL \
      "ALTER TABLE ... ADD COLUMN without IF NOT EXISTS" \
      "Add IF NOT EXISTS after ADD COLUMN" \
      "add\s+column\s" \
      "if\s+not\s+exists"

    check_pattern "$MIGRATION_FILE" "$BASE" "$SQL_STATEMENT_OUTPUT" FAIL \
      "Single-quoted DO body cannot be inspected" \
      "Use a dollar-quoted DO body so executable statements can be linted" \
      "^do([[:space:]]|$)" \
      ""

    # WARN-class checks
    check_pattern "$MIGRATION_FILE" "$BASE" "$SQL_STATEMENT_OUTPUT" WARN \
      "INSERT INTO without ON CONFLICT clause" \
      "Add ON CONFLICT DO NOTHING or ON CONFLICT ... DO UPDATE" \
      "(^|[[:space:]])insert[[:space:]]+into[[:space:]]" \
      "on\s+conflict"

    check_pattern "$MIGRATION_FILE" "$BASE" "$SQL_STATEMENT_OUTPUT" WARN \
      "CREATE FUNCTION without OR REPLACE" \
      "Use CREATE OR REPLACE FUNCTION" \
      "(^|[[:space:]])create[[:space:]]+function[[:space:]]" \
      "or\s+replace"
  done
fi

# ── JSON summary to stdout ────────────────────────────────────────────────────
printf '{"fail_count":%d,"warn_count":%d,"findings":[' \
  "$FAIL_COUNT" "$WARN_COUNT"
FIRST=1
for obj in "${FINDINGS[@]}"; do
  [ "$FIRST" -eq 1 ] && FIRST=0 || printf ','
  printf '%s' "$obj"
done
printf ']}\n'

# ── Human summary to stderr ───────────────────────────────────────────────────
printf '\n' >&2
if [ "$FAIL_COUNT" -eq 0 ] && [ "$WARN_COUNT" -eq 0 ]; then
  printf "${GREEN}${BOLD}All checks passed.${RESET}\n" >&2
elif [ "$FAIL_COUNT" -eq 0 ]; then
  printf "${YELLOW}${BOLD}%d warning(s), 0 failures.%s${RESET}\n" \
    "$WARN_COUNT" "$([ "$STRICT" -eq 1 ] && echo ' (--strict: warnings treated as failures)' || echo '')" >&2
else
  printf "${RED}${BOLD}%d failure(s), %d warning(s).${RESET}\n" "$FAIL_COUNT" "$WARN_COUNT" >&2
fi

# Exit non-zero if any FAIL-class findings (WARN elevated to FAIL under --strict
# is already counted in FAIL_COUNT)
[ "$FAIL_COUNT" -eq 0 ]
