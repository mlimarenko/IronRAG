#!/usr/bin/env bash
set -euo pipefail

REPOSITORY="${IRONRAG_GITHUB_REPOSITORY:-mlimarenko/IronRAG}"
VERSION_INPUT="${1:-latest}"
INSTALL_DIR="${2:-ironrag}"

require_command() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: required command not found: $1" >&2
    exit 1
  fi
}

download() {
  local url="$1"
  local destination="$2"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$destination"
    return
  fi

  if command -v wget >/dev/null 2>&1; then
    wget -qO "$destination" "$url"
    return
  fi

  echo "error: curl or wget is required" >&2
  exit 1
}

resolve_release_tag() {
  local api_url="https://api.github.com/repos/${REPOSITORY}/releases/latest"
  local tmp_file

  tmp_file="$(mktemp)"
  trap 'rm -f "$tmp_file"' RETURN
  download "$api_url" "$tmp_file"

  local tag
  tag="$(sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' "$tmp_file" | head -n 1)"
  if [ -z "$tag" ]; then
    echo "error: failed to resolve latest release tag from ${api_url}" >&2
    exit 1
  fi

  printf '%s\n' "$tag"
}

# Hex secret, length in bytes (output is 2*n hex chars). Uses openssl when available.
rand_hex_bytes() {
  local nbytes="${1:-24}"
  if command -v openssl >/dev/null 2>&1; then
    openssl rand -hex "$nbytes"
    return
  fi
  LC_ALL=C tr -dc 'a-f0-9' </dev/urandom | head -c "$((nbytes * 2))"
}

env_file_set() {
  local key="$1"
  local val="$2"
  local file="$3"
  if grep -q "^${key}=" "$file" 2>/dev/null; then
    sed -i "s|^${key}=.*|${key}=${val}|" "$file"
  else
    printf '\n%s=%s\n' "$key" "$val" >>"$file"
  fi
}

# Value of KEY= from the last matching line (empty if missing).
env_get() {
  local key="$1"
  local file="$2"
  sed -n "s/^${key}=//p" "$file" 2>/dev/null | tail -n1 | tr -d '\r'
}

env_value_nonempty() {
  local v
  v="$(env_get "$1" "$2")"
  [ -n "${v//[[:space:]]/}" ]
}

sync_frontend_origin_to_port() {
  local file="$1"
  local port="$2"
  local origin="http://127.0.0.1:${port},http://localhost:${port}"
  env_file_set "IRONRAG_FRONTEND_ORIGIN" "$origin" "$file"
}

print_configuration_summary() {
  local env_file="$1"
  echo ""
  echo "---"
  echo "Stack secrets:"
  if [ "${IRONRAG_NEW_ENV_SECRETS:-0}" = "1" ]; then
    echo "  New .env: random Postgres, Arango, IRONRAG_BOOTSTRAP_TOKEN (see .env; not printed)."
  else
    echo "  Existing .env: secrets unchanged."
  fi
  echo "Admin (UI):"
  if env_value_nonempty "IRONRAG_UI_BOOTSTRAP_ADMIN_PASSWORD" "$env_file"; then
    echo "  Set in .env: IRONRAG_UI_BOOTSTRAP_ADMIN_LOGIN / _PASSWORD."
  else
    echo "  Not in .env: create admin in UI on first visit."
  fi
  echo "LLM keys:"
  if env_value_nonempty "IRONRAG_OPENAI_API_KEY" "$env_file" \
    || env_value_nonempty "IRONRAG_DEEPSEEK_API_KEY" "$env_file" \
    || env_value_nonempty "IRONRAG_QWEN_API_KEY" "$env_file"; then
    echo "  At least one provider key in .env."
  else
    echo "  None in .env: set IRONRAG_*_API_KEY or in UI."
  fi
  echo "---"
}

require_command docker
docker compose version >/dev/null

if [ "$VERSION_INPUT" = "latest" ]; then
  VERSION="$(resolve_release_tag)"
else
  VERSION="$VERSION_INPUT"
fi

RAW_BASE_URL="https://raw.githubusercontent.com/${REPOSITORY}/${VERSION}"

mkdir -p "$INSTALL_DIR"

echo "Installing IronRAG ${VERSION} into ${INSTALL_DIR}"

download "${RAW_BASE_URL}/docker-compose.yml" "${INSTALL_DIR}/docker-compose.yml"
download "${RAW_BASE_URL}/docker-compose-s4.yml" "${INSTALL_DIR}/docker-compose-s4.yml"
download "${RAW_BASE_URL}/.env.example" "${INSTALL_DIR}/.env.example"

IRONRAG_NEW_ENV_SECRETS=0
if [ ! -f "${INSTALL_DIR}/.env" ]; then
  # Refuse to mint fresh random Postgres / ArangoDB passwords when stale
  # data volumes from a previous install survive: Postgres bakes the
  # initial password into PGDATA, ArangoDB never resets the root password
  # after first init, so a fresh `.env` would auth-loop forever otherwise.
  # The operator must explicitly opt into wiping the data with
  # IRONRAG_RESET_VOLUMES=1, or restore the prior `.env` first.
  stale_volumes=""
  if command -v docker >/dev/null 2>&1; then
    for vol in ironrag_postgres_data ironrag_arangodb_data ironrag_content_storage_data; do
      if docker volume inspect "$vol" >/dev/null 2>&1; then
        stale_volumes="${stale_volumes}${stale_volumes:+ }${vol}"
      fi
    done
  fi
  if [ -n "$stale_volumes" ]; then
    if [ "${IRONRAG_RESET_VOLUMES:-0}" = "1" ]; then
      echo "Wiping stale Docker volumes (IRONRAG_RESET_VOLUMES=1): $stale_volumes"
      docker volume rm $stale_volumes >/dev/null
    else
      echo "error: .env is missing but stale Docker volumes survive from a previous install:" >&2
      echo "  $stale_volumes" >&2
      echo "Minting fresh secrets would not match the passwords baked into those" >&2
      echo "volumes (Postgres PGDATA, ArangoDB root). Pick one:" >&2
      echo "  1. Restore the previous .env if you still have it." >&2
      echo "  2. Re-run with IRONRAG_RESET_VOLUMES=1 to wipe the data and start fresh." >&2
      exit 1
    fi
  fi

  cp "${INSTALL_DIR}/.env.example" "${INSTALL_DIR}/.env"
  IRONRAG_NEW_ENV_SECRETS=1
  pg_pass="$(rand_hex_bytes 24)"
  arango_pass="$(rand_hex_bytes 24)"
  boot_token="$(rand_hex_bytes 24)"
  env_file_set "IRONRAG_POSTGRES_PASSWORD" "$pg_pass" "${INSTALL_DIR}/.env"
  env_file_set "IRONRAG_ARANGODB_PASSWORD" "$arango_pass" "${INSTALL_DIR}/.env"
  env_file_set "IRONRAG_BOOTSTRAP_TOKEN" "$boot_token" "${INSTALL_DIR}/.env"
fi

# Optional: pin the published HTTP port (Ansible, CI, or manual: IRONRAG_PORT=8080 install.sh …).
if [ -n "${IRONRAG_PORT:-}" ]; then
  env_file="${INSTALL_DIR}/.env"
  if grep -q '^IRONRAG_PORT=' "$env_file" 2>/dev/null; then
    sed -i "s/^IRONRAG_PORT=.*/IRONRAG_PORT=${IRONRAG_PORT}/" "$env_file"
  else
    printf '\nIRONRAG_PORT=%s\n' "${IRONRAG_PORT}" >>"$env_file"
  fi
fi

published_port="$(
  sed -n 's/^IRONRAG_PORT=//p' "${INSTALL_DIR}/.env" 2>/dev/null | tail -n1 | tr -d '\r'
)"
published_port="${published_port:-19000}"

sync_frontend_origin_to_port "${INSTALL_DIR}/.env" "$published_port"

(
  cd "$INSTALL_DIR"
  docker compose pull
  docker compose up -d postgres redis arangodb startup
)

# Watch the startup container before creating app services. Some Compose
# versions block forever on `service_completed_successfully` dependents when
# startup restarts, so the installer owns the wait loop and can print the real
# failure instead of leaving `ironrag-startup-1 Waiting`.
wait_for_startup_authority() {
  local install_dir="$1"
  local wait_seconds="${IRONRAG_STARTUP_WAIT_SECS:-300}"
  local deadline=$(( $(date +%s) + wait_seconds ))
  local startup_id

  while [ "$(date +%s)" -lt "$deadline" ]; do
    startup_id="$(cd "$install_dir" && docker compose ps -a -q startup 2>/dev/null || true)"
    if [ -z "$startup_id" ]; then
      sleep 2
      continue
    fi

    local startup_logs
    startup_logs="$(docker logs "$startup_id" 2>&1 | tail -n 200)"

    local drift_line
    drift_line="$(printf '%s\n' "$startup_logs" | grep -m 1 -E 'migration [0-9]+ was previously applied but has been modified' || true)"
    if [ -n "$drift_line" ]; then
      local version padded_version
      version="$(printf '%s\n' "$drift_line" | grep -oE '[0-9]+' | head -n 1)"
      padded_version="$(printf '%04d' "$version")"
      cat >&2 <<DRIFT_ERR
ERROR: ironrag-startup-1 keeps restarting because the bundled
       schema for migration ${version} doesn't match the one applied
       to this database. sqlx refuses to start until the recorded
       checksum matches the file in the running image.
       This happens when an existing deployment pulls a release that
       touched a previously-applied migration.

       Resolve in three steps:

       1. Extract and apply the canonical idempotent migration file:

            docker compose -f ${install_dir}/docker-compose.yml run --rm --no-deps \\
              --entrypoint sh backend \\
              -c 'cat /app/migrations/${padded_version}_*.sql' \\
              >/tmp/ironrag-migration-${version}.sql

            docker compose -f ${install_dir}/docker-compose.yml exec -T \\
              postgres psql -U postgres -d ironrag \\
              </tmp/ironrag-migration-${version}.sql

       2. Compute the new checksum for migration ${version} from the
          running backend image:

            docker compose -f ${install_dir}/docker-compose.yml run --rm --no-deps \\
              --entrypoint sha384sum backend \\
              /app/migrations/${padded_version}_*.sql

       3. Update the row in _sqlx_migrations:

            docker compose -f ${install_dir}/docker-compose.yml exec \\
              postgres psql -U postgres -d ironrag -c \\
              "UPDATE _sqlx_migrations SET checksum = decode('<NEW_HEX>','hex') WHERE version = ${version};"

       Then restart the stack:

            docker compose -f ${install_dir}/docker-compose.yml restart \\
              startup backend worker

       Stack is stopped now to avoid a silent restart loop.
DRIFT_ERR
      (cd "$install_dir" && docker compose stop startup backend worker frontend >/dev/null 2>&1 || true)
      return 1
    fi

    local state status exit_code restart_count
    state="$(docker inspect "$startup_id" -f '{{.State.Status}}|{{.State.ExitCode}}|{{.RestartCount}}' 2>/dev/null || echo 'unknown|1|0')"
    status="${state%%|*}"
    state="${state#*|}"
    exit_code="${state%%|*}"
    restart_count="${state##*|}"

    if [ "$status" = "exited" ] && [ "$exit_code" = "0" ]; then
      return 0
    fi

    if { [ "$status" = "exited" ] && [ "$exit_code" != "0" ]; } \
      || { [ "$status" = "restarting" ] && [ "${restart_count:-0}" -gt 0 ]; }; then
      cat >&2 <<STARTUP_ERR
ERROR: ironrag-startup-1 failed before the API could start.

Last startup logs:
${startup_logs}

Stack is stopped now to avoid a silent restart loop.
STARTUP_ERR
      (cd "$install_dir" && docker compose stop startup backend worker frontend >/dev/null 2>&1 || true)
      return 1
    fi

    sleep 3
  done

  startup_id="$(cd "$install_dir" && docker compose ps -a -q startup 2>/dev/null || true)"
  if [ -n "$startup_id" ]; then
    docker logs "$startup_id" --tail 200 >&2 || true
  fi
  echo "ERROR: ironrag-startup-1 did not finish within ${wait_seconds}s." >&2
  (cd "$install_dir" && docker compose stop startup backend worker frontend >/dev/null 2>&1 || true)
  return 1
}

if ! wait_for_startup_authority "$INSTALL_DIR"; then
  exit 1
fi

(
  cd "$INSTALL_DIR"
  docker compose up -d backend worker frontend
)

cat <<EOF
IronRAG ${VERSION} is starting.
Directory: ${INSTALL_DIR}
App: http://127.0.0.1:${published_port}
MCP: http://127.0.0.1:${published_port}/v1/mcp
EOF

print_configuration_summary "${INSTALL_DIR}/.env"
