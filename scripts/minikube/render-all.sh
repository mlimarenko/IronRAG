#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHART_DIR="${ROOT_DIR}/charts/rustrag"

. "${ROOT_DIR}/scripts/minikube/common.sh"

HELM_BIN="$(resolve_bin helm "${ROOT_DIR}")"

"${HELM_BIN}" lint "${CHART_DIR}"
"${HELM_BIN}" template rustrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/bundled-s3.yaml" >/tmp/rustrag-bundled.yaml
"${HELM_BIN}" template rustrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/filesystem-single-node.yaml" >/tmp/rustrag-filesystem.yaml
"${HELM_BIN}" template rustrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/external-services.yaml" >/tmp/rustrag-external.yaml

printf 'rendered %s\n' /tmp/rustrag-bundled.yaml
printf 'rendered %s\n' /tmp/rustrag-filesystem.yaml
printf 'rendered %s\n' /tmp/rustrag-external.yaml
