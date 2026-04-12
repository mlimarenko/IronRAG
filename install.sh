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
  docker compose up -d
)

cat <<EOF
IronRAG ${VERSION} is starting.
Directory: ${INSTALL_DIR}
App: http://127.0.0.1:${published_port}
MCP: http://127.0.0.1:${published_port}/v1/mcp
EOF

print_configuration_summary "${INSTALL_DIR}/.env"
