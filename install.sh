#!/usr/bin/env bash
set -euo pipefail

REPOSITORY="${RUSTRAG_GITHUB_REPOSITORY:-mlimarenko/RustRAG}"
VERSION_INPUT="${1:-latest}"
INSTALL_DIR="${2:-rustrag}"

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

require_command docker
docker compose version >/dev/null

if [ "$VERSION_INPUT" = "latest" ]; then
  VERSION="$(resolve_release_tag)"
else
  VERSION="$VERSION_INPUT"
fi

RAW_BASE_URL="https://raw.githubusercontent.com/${REPOSITORY}/${VERSION}"

mkdir -p "$INSTALL_DIR/docker/nginx"

echo "Installing RustRAG ${VERSION} into ${INSTALL_DIR}"

download "${RAW_BASE_URL}/docker-compose.yml" "${INSTALL_DIR}/docker-compose.yml"
download "${RAW_BASE_URL}/.env.example" "${INSTALL_DIR}/.env.example"
download "${RAW_BASE_URL}/docker/nginx/default.conf" "${INSTALL_DIR}/docker/nginx/default.conf"

if [ ! -f "${INSTALL_DIR}/.env" ]; then
  cp "${INSTALL_DIR}/.env.example" "${INSTALL_DIR}/.env"
fi

(
  cd "$INSTALL_DIR"
  docker compose pull
  docker compose up -d
)

cat <<EOF
RustRAG ${VERSION} is starting.
Directory: ${INSTALL_DIR}
App: http://127.0.0.1:19000
MCP: http://127.0.0.1:19000/v1/mcp
EOF
