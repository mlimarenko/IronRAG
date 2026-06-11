#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHART_DIR="${ROOT_DIR}/charts/ironrag"

. "${ROOT_DIR}/scripts/minikube/common.sh"

HELM_BIN="$(resolve_bin helm "${ROOT_DIR}")"
APP_VERSION="$("${HELM_BIN}" show chart "${CHART_DIR}" | awk -F': *' '$1 == "appVersion" { print $2; exit }')"
APP_IMAGE_TAG="v${APP_VERSION}"

"${HELM_BIN}" lint "${CHART_DIR}"
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/bundled-s3.yaml" >/tmp/ironrag-bundled.yaml
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/filesystem-single-node.yaml" >/tmp/ironrag-filesystem.yaml
"${HELM_BIN}" template ironrag "${CHART_DIR}" \
  --values "${CHART_DIR}/values/examples/external-services.yaml" >/tmp/ironrag-external.yaml

if rg -n '127\.0\.0\.11|ironrag-(backend|frontend):0\.3\.1|pipingspace/ironrag-(backend|frontend):0\.3\.1' \
  "${ROOT_DIR}/apps/web/nginx.conf.template" \
  /tmp/ironrag-bundled.yaml \
  /tmp/ironrag-filesystem.yaml \
  /tmp/ironrag-external.yaml
then
  echo "rendered Helm chart or web nginx template contains obsolete Docker-only DNS or image tags" >&2
  exit 1
fi

if ! rg -F -q "pipingspace/ironrag-backend:${APP_IMAGE_TAG}" /tmp/ironrag-bundled.yaml; then
  echo "rendered Helm chart does not contain the backend image tag derived from Chart.appVersion" >&2
  exit 1
fi

if ! rg -F -q "pipingspace/ironrag-frontend:${APP_IMAGE_TAG}" /tmp/ironrag-bundled.yaml; then
  echo "rendered Helm chart does not contain the frontend image tag derived from Chart.appVersion" >&2
  exit 1
fi

for rendered in /tmp/ironrag-bundled.yaml /tmp/ironrag-filesystem.yaml; do
  if ! rg -F -q 'image: "pgvector/pgvector:pg18"' "${rendered}"; then
    echo "rendered Helm chart ${rendered} is missing bundled postgres image pgvector/pgvector:pg18" >&2
    exit 1
  fi
  if ! rg -F -q 'image: "redis:8.8"' "${rendered}"; then
    echo "rendered Helm chart ${rendered} is missing bundled redis image redis:8.8" >&2
    exit 1
  fi
done

printf 'rendered %s\n' /tmp/ironrag-bundled.yaml
printf 'rendered %s\n' /tmp/ironrag-filesystem.yaml
printf 'rendered %s\n' /tmp/ironrag-external.yaml
