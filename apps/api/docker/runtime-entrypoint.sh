#!/bin/sh
set -eu

CONTENT_STORAGE_ROOT="${IRONRAG_CONTENT_STORAGE_ROOT:-/var/lib/ironrag/content-storage}"

if [ "$(id -u)" -eq 0 ]; then
  # Docker / Compose path: image started as root, normalise ownership and
  # drop to appuser before exec'ing the real binary.
  mkdir -p "$CONTENT_STORAGE_ROOT"
  chown -R appuser:appuser "$CONTENT_STORAGE_ROOT"
  chmod -R u+rwX "$CONTENT_STORAGE_ROOT"
  exec su -s /bin/sh appuser -c 'exec "$0" "$@"' "$@"
fi

# Kubernetes / Helm path: pod runs with runAsUser already non-root.
# Best-effort directory creation; PVC permissions and the Dockerfile-baked
# /var/lib/ironrag/content-storage are responsible for making this writable.
mkdir -p "$CONTENT_STORAGE_ROOT" 2>/dev/null || true

exec "$@"
