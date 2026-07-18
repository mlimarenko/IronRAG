#!/usr/bin/env bash
# Regression test for argv-safe privilege dropping in the backend entrypoint.
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENTRYPOINT="${ROOT_DIR}/apps/api/docker/runtime-entrypoint.sh"
TEST_DIR="$(mktemp -d)"
trap 'rm -rf "$TEST_DIR"' EXIT

FAKE_BIN="${TEST_DIR}/bin"
ARGV_OUTPUT="${TEST_DIR}/argv.json"
mkdir -p "$FAKE_BIN"

cat >"${FAKE_BIN}/id" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "-u" ]; then
  printf '0\n'
  exit 0
fi
exit 64
EOF

cat >"${FAKE_BIN}/chown" <<'EOF'
#!/bin/sh
exit 0
EOF

# Model util-linux su's option permutation: options are recognized even after
# the user operand until an explicit `--` ends option parsing.
cat >"${FAKE_BIN}/su" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

selected_shell=""
selected_command=""
parse_options=1
operands=()

while (($# > 0)); do
  if ((parse_options)); then
    case "$1" in
      --)
        parse_options=0
        shift
        continue
        ;;
      -s | --shell)
        (($# >= 2)) || exit 64
        selected_shell="$2"
        shift 2
        continue
        ;;
      -c | --command | --session-command)
        (($# >= 2)) || exit 64
        selected_command="$2"
        shift 2
        continue
        ;;
      -*)
        printf 'unexpected su option: %s\n' "$1" >&2
        exit 64
        ;;
    esac
  fi
  operands+=("$1")
  shift
done

[[ "$selected_shell" == "/bin/sh" ]]
[[ "$selected_command" == 'exec "$0" "$@"' ]]
[[ "${operands[0]:-}" == "appuser" ]]
((${#operands[@]} >= 2))

exec "$selected_shell" -c "$selected_command" "${operands[@]:1}"
EOF

cat >"${TEST_DIR}/argv-probe" <<'EOF'
#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

Path(os.environ["IRONRAG_TEST_ARGV_OUTPUT"]).write_text(
    json.dumps(sys.argv[1:]),
    encoding="utf-8",
)
EOF

chmod +x "${FAKE_BIN}/id" "${FAKE_BIN}/chown" "${FAKE_BIN}/su" "${TEST_DIR}/argv-probe"

PATH="${FAKE_BIN}:${PATH}" \
IRONRAG_CONTENT_STORAGE_ROOT="${TEST_DIR}/content-storage" \
IRONRAG_TEST_ARGV_OUTPUT="$ARGV_OUTPUT" \
  /bin/sh "$ENTRYPOINT" \
  "${TEST_DIR}/argv-probe" \
  --source-library \
  "library id with spaces" \
  "" \
  "--label=two words" \
  "plain value"

EXPECTED='["--source-library", "library id with spaces", "", "--label=two words", "plain value"]'
ACTUAL="$(<"$ARGV_OUTPUT")"
if [[ "$ACTUAL" != "$EXPECTED" ]]; then
  printf 'FAIL: argv changed across privilege drop\nexpected: %s\nactual:   %s\n' \
    "$EXPECTED" "$ACTUAL" >&2
  exit 1
fi

printf 'PASS: runtime entrypoint preserves argv across privilege drop\n'
