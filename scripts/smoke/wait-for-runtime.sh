#!/usr/bin/env bash
set -euo pipefail

source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/common.sh"

smoke_require_commands docker curl

repo_root=$(smoke_repo_root)
api_base=${RUSTRAG_SMOKE_API_BASE:-http://127.0.0.1:18080/v1}
frontend_base=${RUSTRAG_SMOKE_FRONTEND_BASE:-http://127.0.0.1:19000}
timeout_seconds=${RUSTRAG_SMOKE_WAIT_TIMEOUT_SECONDS:-180}
sleep_seconds=${RUSTRAG_SMOKE_WAIT_SLEEP_SECONDS:-2}
services=(postgres redis neo4j backend frontend)

service_status() {
  local service=$1
  local container_id
  container_id=$(cd "${repo_root}" && docker compose ps -q "${service}")
  if [[ -z ${container_id} ]]; then
    return 1
  fi
  docker inspect \
    --format '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' \
    "${container_id}"
}

deadline=$((SECONDS + timeout_seconds))
while ((SECONDS <= deadline)); do
  ready=true
  for service in "${services[@]}"; do
    status=$(service_status "${service}" || true)
    if [[ ${status} != "healthy" && ${status} != "running" ]]; then
      ready=false
      break
    fi
  done

  if [[ ${ready} == "true" ]] \
    && curl -fsS "${api_base}/health" >/dev/null \
    && curl -fsS "${api_base}/ready" >/dev/null \
    && curl -fsS "${frontend_base}" >/dev/null; then
    printf 'runtime ready at %s\n' "$(smoke_timestamp_utc)"
    exit 0
  fi

  sleep "${sleep_seconds}"
done

printf 'timed out waiting for runtime stack to become healthy\n' >&2
exit 1
