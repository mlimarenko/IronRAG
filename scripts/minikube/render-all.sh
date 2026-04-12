#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHART_DIR="${ROOT_DIR}/charts/ironrag"

. "${ROOT_DIR}/scripts/minikube/common.sh"

HELM_BIN="$(resolve_bin helm "${ROOT_DIR}")"

"${HELM_BIN}" lint "${CHART_DIR}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/bundled-s3.yaml" >/tmp/ironrag-bundled.yaml
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/filesystem-single-node.yaml" >/tmp/ironrag-filesystem.yaml
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/external-services.yaml" >/tmp/ironrag-external.yaml

printf 'rendered %s\n' /tmp/ironrag-bundled.yaml
printf 'rendered %s\n' /tmp/ironrag-filesystem.yaml
printf 'rendered %s\n' /tmp/ironrag-external.yaml
