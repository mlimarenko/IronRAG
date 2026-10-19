#!/usr/bin/env bash
# lint_migrations.sh — enforce IronRAG's one primary migration set.
#
# The product boots only from apps/api/migrations/0001_init.sql. Migration
# histories, base-reference comparisons, and upgrade files are intentionally not
# supported.
#
# Exit codes: 0 = policy passes, 1 = policy violation, 2 = usage error.

set -euo pipefail

EXPECTED_MIGRATION="apps/api/migrations/0001_init.sql"
# sqlx refuses to start against a database whose recorded checksum for an
# applied migration no longer matches the file ("migration 1 was previously
# applied but has been modified"). With a single baseline and no additive
# migrations there is no way to repair that from the application side, so a
# baseline edit is only ever deployable to a database created after it.
#
# This file records the checksum the baseline is expected to have. Editing the
# baseline without updating it fails here, at commit time, instead of at the
# next deploy — which is where it surfaced on 2026-08-16, after the images were
# built and shipped.
BASELINE_CHECKSUM_FILE="scripts/ops/baseline-checksum.txt"
SELF_PATH="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"

usage() {
  cat >&2 <<EOF
Usage: lint_migrations.sh [--self-test] [--help]

Checks that the only migration file is $EXPECTED_MIGRATION.

Options:
  --self-test  Run isolated policy regression checks.
  --help       Print this help and exit 0.

Output: a JSON summary line on stdout.
EOF
  exit "${1:-0}"
}

SELF_TEST=0
while [ "$#" -gt 0 ]; do
  case "$1" in
    --self-test) SELF_TEST=1 ;;
    --help|-h) usage 0 ;;
    *) printf 'Unknown option: %s\n' "$1" >&2; exit 2 ;;
  esac
  shift
done

repo_root() {
  git rev-parse --show-toplevel 2>/dev/null || {
    printf 'Not inside a git repository.\n' >&2
    exit 2
  }
}

if [ "$SELF_TEST" -eq 1 ]; then
  unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_CEILING_DIRECTORIES \
    GIT_OBJECT_DIRECTORY GIT_ALTERNATE_OBJECT_DIRECTORIES GIT_COMMON_DIR
  tempdir="$(mktemp -d)"
  trap 'rm -rf "$tempdir"' EXIT
  fake_repo="$tempdir/repo"
  mkdir -p "$fake_repo/apps/api/migrations" "$fake_repo/scripts/ops"
  git -C "$fake_repo" init -q -b main
  printf '%s\n' 'select 1;' > "$fake_repo/$EXPECTED_MIGRATION"
  sha256sum "$fake_repo/$EXPECTED_MIGRATION" | cut -d' ' -f1 \
    > "$fake_repo/$BASELINE_CHECKSUM_FILE"

  pass_output="$(git -C "$fake_repo" rev-parse --show-toplevel >/dev/null && cd "$fake_repo" && bash "$SELF_PATH")" || {
    printf 'Self-test failed: primary singleton was rejected.\n' >&2
    exit 1
  }
  if ! printf '%s' "$pass_output" | grep -q '"valid":true'; then
    printf 'Self-test failed: primary singleton did not report valid JSON.\n' >&2
    exit 1
  fi

  printf '%s\n' 'select 2;' > "$fake_repo/apps/api/migrations/0002_rejected.sql"
  rejected_output="$(cd "$fake_repo" && bash "$SELF_PATH")" && rejected_exit=0 || rejected_exit=$?
  if [ "$rejected_exit" -eq 0 ] \
    || ! printf '%s' "$rejected_output" | grep -q '"check":"single_migration_set"'; then
    printf 'Self-test failed: an additional migration was accepted.\n' >&2
    exit 1
  fi

  rm "$fake_repo/apps/api/migrations/0002_rejected.sql"
  mkdir "$fake_repo/apps/api/migrations/rejected"
  printf '%s\n' 'select 3;' > "$fake_repo/apps/api/migrations/rejected/0002.sql"
  nested_output="$(cd "$fake_repo" && bash "$SELF_PATH")" && nested_exit=0 || nested_exit=$?
  if [ "$nested_exit" -eq 0 ] \
    || ! printf '%s' "$nested_output" | grep -q '"check":"single_migration_set"'; then
    printf 'Self-test failed: a nested migration was accepted.\n' >&2
    exit 1
  fi

  rm -r "$fake_repo/apps/api/migrations/rejected"
  rm "$fake_repo/$EXPECTED_MIGRATION"
  missing_output="$(cd "$fake_repo" && bash "$SELF_PATH")" && missing_exit=0 || missing_exit=$?
  if [ "$missing_exit" -eq 0 ] \
    || ! printf '%s' "$missing_output" | grep -q '"check":"single_migration_set"'; then
    printf 'Self-test failed: missing primary baseline was not rejected.\n' >&2
    exit 1
  fi

  # An edited baseline must be rejected while the recorded checksum still names
  # the old content: that is the shape that reached a real deploy and failed
  # there, after the images were already built.
  printf '%s\n' 'select 1;' > "$fake_repo/$EXPECTED_MIGRATION"
  printf '%s\n' 'select 1; -- drifted' > "$fake_repo/$EXPECTED_MIGRATION"
  drift_output="$(cd "$fake_repo" && bash "$SELF_PATH")" && drift_exit=0 || drift_exit=$?
  if [ "$drift_exit" -eq 0 ] \
    || ! printf '%s' "$drift_output" | grep -q '"check":"baseline_checksum"'; then
    printf 'Self-test failed: an edited baseline was accepted.\n' >&2
    exit 1
  fi

  sha256sum "$fake_repo/$EXPECTED_MIGRATION" | cut -d' ' -f1 \
    > "$fake_repo/$BASELINE_CHECKSUM_FILE"
  acknowledged_output="$(cd "$fake_repo" && bash "$SELF_PATH")" || {
    printf 'Self-test failed: an acknowledged baseline edit was rejected.\n' >&2
    exit 1
  }
  if ! printf '%s' "$acknowledged_output" | grep -q '"valid":true'; then
    printf 'Self-test failed: acknowledging the edit did not clear the finding.\n' >&2
    exit 1
  fi

  rm "$fake_repo/$BASELINE_CHECKSUM_FILE"
  unrecorded_output="$(cd "$fake_repo" && bash "$SELF_PATH")" && unrecorded_exit=0 || unrecorded_exit=$?
  if [ "$unrecorded_exit" -eq 0 ] \
    || ! printf '%s' "$unrecorded_output" | grep -q '"check":"baseline_checksum"'; then
    printf 'Self-test failed: a missing checksum record was accepted.\n' >&2
    exit 1
  fi

  printf 'Self-test passed: primary singleton and baseline checksum are enforced.\n' >&2
  exit 0
fi

REPO_ROOT="$(repo_root)"
MIGRATIONS_DIR="$REPO_ROOT/apps/api/migrations"
EXPECTED_PATH="$REPO_ROOT/$EXPECTED_MIGRATION"
mapfile -t local_files < <(find "$MIGRATIONS_DIR" -type f -name '*.sql' -printf '%P\n' | sort)

findings=()
record_failure() {
  findings+=("{\"check\":\"$1\",\"file\":\"$2\",\"message\":\"$3\"}")
  printf '[FAIL] %s: %s\n' "$1" "$3" >&2
}

if [ ! -f "$EXPECTED_PATH" ]; then
  record_failure "single_migration_set" "0001_init.sql" "Required primary baseline is missing"
fi

for migration in "${local_files[@]}"; do
  if [ "$migration" != "0001_init.sql" ]; then
    record_failure "single_migration_set" "$migration" "Only 0001_init.sql is permitted"
  fi
done

CHECKSUM_PATH="$REPO_ROOT/$BASELINE_CHECKSUM_FILE"
if [ -f "$EXPECTED_PATH" ]; then
  actual_checksum="$(sha256sum "$EXPECTED_PATH" | cut -d' ' -f1)"
  if [ ! -f "$CHECKSUM_PATH" ]; then
    record_failure "baseline_checksum" "$BASELINE_CHECKSUM_FILE" \
      "Recorded baseline checksum is missing; write $actual_checksum into it"
  else
    recorded_checksum="$(tr -d '[:space:]' < "$CHECKSUM_PATH")"
    if [ "$actual_checksum" != "$recorded_checksum" ]; then
      record_failure "baseline_checksum" "0001_init.sql" \
        "Baseline changed ($recorded_checksum -> $actual_checksum). Every database that already applied it will refuse to start; recreate them, or revert. Update $BASELINE_CHECKSUM_FILE in the same commit to acknowledge."
    fi
  fi
fi

printf '{"valid":%s,"findings":[' "$([ "${#findings[@]}" -eq 0 ] && printf true || printf false)"
(IFS=,; printf '%s' "${findings[*]}")
printf ']}\n'

[ "${#findings[@]}" -eq 0 ]
