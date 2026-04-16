#!/usr/bin/env bash
# Canonical snapshot transfer for a single IronRAG library.
#
# Usage:
#   snapshot-library.sh export <base-url> <library-id> <session-cookie> <output.ndjson>
#   snapshot-library.sh import <base-url> <library-id> <session-cookie> <input.ndjson>
#
# Example (pull a library from prod to a local file):
#   ./snapshot-library.sh export \
#       https://your-ironrag.example \
#       <library-uuid> \
#       "$(cat ~/.ironrag-prod-cookie)" \
#       ./library-snapshot.ndjson
#
# Example (restore into local dev stack):
#   ./snapshot-library.sh import \
#       http://localhost:19000 \
#       <library-uuid> \
#       "$(cat ~/.ironrag-local-cookie)" \
#       ./library-snapshot.ndjson
#
# The session cookie is the value of the `ironrag_session` cookie obtained
# by logging in at /v1/iam/session/login. Store it as a raw cookie header
# fragment, e.g. `ironrag_session=abc123`.

set -euo pipefail

if [[ $# -lt 5 ]]; then
    echo "usage: $0 <export|import> <base-url> <library-id> <cookie> <file>" >&2
    exit 2
fi

action="$1"
base_url="${2%/}"
library_id="$3"
cookie="$4"
file="$5"

url="${base_url}/v1/content/libraries/${library_id}/snapshot"

case "$action" in
    export)
        echo "exporting ${library_id} from ${base_url} -> ${file}"
        curl --fail --show-error --location \
            -H "Cookie: ${cookie}" \
            -H "Accept: application/x-ndjson" \
            "$url" \
            -o "$file"
        bytes=$(stat -c%s "$file" 2>/dev/null || stat -f%z "$file")
        echo "export done: ${bytes} bytes"
        ;;
    import)
        if [[ ! -f "$file" ]]; then
            echo "file not found: $file" >&2
            exit 1
        fi
        echo "importing ${file} into ${library_id} at ${base_url}"
        curl --fail --show-error \
            -H "Cookie: ${cookie}" \
            -H "Content-Type: application/x-ndjson" \
            --data-binary "@${file}" \
            "$url"
        echo
        ;;
    *)
        echo "unknown action: $action (expected: export | import)" >&2
        exit 2
        ;;
esac
