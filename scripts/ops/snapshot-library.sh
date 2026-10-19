#!/usr/bin/env bash
# Snapshot transfer for a single IronRAG library.
#
# Usage:
#   snapshot-library.sh export <base-url> <library-id> <cookie> <output.tar.zst> [include]
#   snapshot-library.sh import <base-url> <library-id> <cookie> <input.tar.zst> [reject|replace]
#
# `include` defaults to the server default (`library_data,blobs`); pass a
# comma-separated list of `workspace`, `ai_config`, `library_data`, `blobs`
# to narrow or widen the scope.
#
# Example (pull a library from prod to a local file):
#   ./snapshot-library.sh export \
#       https://your-ironrag.example \
#       <library-uuid> \
#       "$(cat ~/.ironrag-prod-cookie)" \
#       ./library-snapshot.tar.zst
#
# Example (restore into a local dev stack, replacing what is there):
#   ./snapshot-library.sh import \
#       http://localhost:19000 \
#       <library-uuid> \
#       "$(cat ~/.ironrag-local-cookie)" \
#       ./library-snapshot.tar.zst replace
#
# The session cookie is the value of the `ironrag_ui_session` cookie obtained
# by logging in at /v1/iam/sessions. Store it as a raw cookie header
# fragment, e.g. `ironrag_ui_session=abc123`.
#
# Import is asynchronous on the server: the POST answers 202 with an operation
# id and the restore runs in the background. This script therefore polls
# /v1/ops/operations/{id} until the operation reaches a terminal status and
# exits non-zero when it failed — a 202 alone is NOT success.

set -euo pipefail

if [[ $# -lt 5 ]]; then
    echo "usage: $0 <export|import> <base-url> <library-id> <cookie> <file> [include|overwrite]" >&2
    exit 2
fi

action="$1"
base_url="${2%/}"
library_id="$3"
cookie="$4"
file="$5"
option="${6:-}"

url="${base_url}/v1/content/libraries/${library_id}/snapshot"

# Poll an async operation to a terminal status. Exits non-zero on failure.
wait_for_operation() {
    local operation_id="$1" status detail
    local waited=0
    while :; do
        status=$(curl --fail --silent --show-error \
            -H "Cookie: ${cookie}" \
            "${base_url}/v1/ops/operations/${operation_id}" \
            | sed -n 's/.*"status":"\([a-z_]*\)".*/\1/p')
        case "$status" in
            ready)
                echo "import done: operation ${operation_id} is ready"
                return 0
                ;;
            failed|canceled)
                detail=$(curl --fail --silent --show-error \
                    -H "Cookie: ${cookie}" \
                    "${base_url}/v1/ops/operations/${operation_id}" \
                    | sed -n 's/.*"failureCode":"\([a-z_]*\)".*/\1/p')
                echo "import FAILED: operation ${operation_id} is ${status} (${detail:-no failure code})" >&2
                echo "inspect the server log for the full error chain" >&2
                return 1
                ;;
            "")
                echo "import status unknown: operation ${operation_id} returned no status" >&2
                return 1
                ;;
            *)
                sleep 2
                waited=$((waited + 2))
                if (( waited >= 3600 )); then
                    echo "import still ${status} after ${waited}s; giving up on polling" >&2
                    echo "operation ${operation_id} may still be running — poll it yourself" >&2
                    return 1
                fi
                ;;
        esac
    done
}

case "$action" in
    export)
        [[ -n "$option" ]] && url="${url}?include=${option}"
        echo "exporting ${library_id} from ${base_url} -> ${file}"
        curl --fail --show-error --location \
            -H "Cookie: ${cookie}" \
            -H "Accept: application/zstd" \
            "$url" \
            -o "$file"
        bytes=$(stat -c%s "$file" 2>/dev/null || stat -f%z "$file")
        echo "export done: ${bytes} bytes"
        # An export streams 200 before it knows it succeeded; the failure path
        # appends an EXPORT_FAILED.json sentinel entry instead of erroring.
        if command -v zstd >/dev/null 2>&1 && command -v tar >/dev/null 2>&1; then
            if zstd -dc "$file" | tar -t 2>/dev/null | grep -q '^EXPORT_FAILED.json$'; then
                echo "export FAILED: archive carries an EXPORT_FAILED.json sentinel" >&2
                exit 1
            fi
        else
            echo "warning: zstd/tar unavailable, skipped the EXPORT_FAILED sentinel check" >&2
        fi
        ;;
    import)
        if [[ ! -f "$file" ]]; then
            echo "file not found: $file" >&2
            exit 1
        fi
        overwrite="${option:-reject}"
        echo "importing ${file} into ${library_id} at ${base_url} (overwrite=${overwrite})"
        response=$(curl --fail --show-error \
            -H "Cookie: ${cookie}" \
            -H "Content-Type: application/zstd" \
            --data-binary "@${file}" \
            "${url}?overwrite=${overwrite}")
        operation_id=$(printf '%s' "$response" | sed -n 's/.*"operationId":"\([0-9a-f-]*\)".*/\1/p')
        if [[ -z "$operation_id" ]]; then
            echo "import admission returned no operationId: ${response}" >&2
            exit 1
        fi
        echo "import accepted as operation ${operation_id}; waiting for it to finish"
        wait_for_operation "$operation_id"
        ;;
    *)
        echo "unknown action: $action (expected: export | import)" >&2
        exit 2
        ;;
esac
