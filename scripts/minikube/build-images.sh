#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BACKEND_IMAGE="${BACKEND_IMAGE:-rustrag-backend:dev}"
FRONTEND_IMAGE="${FRONTEND_IMAGE:-rustrag-frontend:dev}"
START_MINIKUBE="${START_MINIKUBE:-0}"
MINIKUBE_RESET_ON_FAILURE="${MINIKUBE_RESET_ON_FAILURE:-1}"

. "${ROOT_DIR}/scripts/minikube/common.sh"

MINIKUBE_BIN="$(resolve_bin minikube "${ROOT_DIR}")"
KUBECTL_BIN="$(resolve_bin kubectl "${ROOT_DIR}")"

if [ "${START_MINIKUBE}" = "1" ] || ! minikube_api_ready "${KUBECTL_BIN}"; then
  ensure_minikube_control_plane \
    "${MINIKUBE_BIN}" \
    "${KUBECTL_BIN}" \
    "${MINIKUBE_RESET_ON_FAILURE}" \
    --driver=docker \
    --cpus="${MINIKUBE_CPUS:-4}" \
    --memory="${MINIKUBE_MEMORY:-8192}"
fi

eval "$("${MINIKUBE_BIN}" docker-env)"

docker build -t "${BACKEND_IMAGE}" -f "${ROOT_DIR}/apps/api/Dockerfile" "${ROOT_DIR}"
docker build -t "${FRONTEND_IMAGE}" -f "${ROOT_DIR}/apps/web/Dockerfile" "${ROOT_DIR}/apps/web"

printf 'backend image: %s\n' "${BACKEND_IMAGE}"
printf 'frontend image: %s\n' "${FRONTEND_IMAGE}"
