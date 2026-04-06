#!/bin/sh
set -eu

CONTENT_STORAGE_ROOT="${RUSTRAG_CONTENT_STORAGE_ROOT:-/var/lib/rustrag/content-storage}"

if [ "$(id -u)" -eq 0 ]; then
  mkdir -p "$CONTENT_STORAGE_ROOT"
  chown -R appuser:appuser "$CONTENT_STORAGE_ROOT"
  chmod -R u+rwX "$CONTENT_STORAGE_ROOT"
  exec su -s /bin/sh appuser -c 'exec "$0" "$@"' "$@"
fi

exec "$@"
