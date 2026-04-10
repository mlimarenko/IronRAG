#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHART_DIR="${ROOT_DIR}/charts/rustrag"
VALUES_FILE="${CHART_DIR}/values/examples/bundled-s3.yaml"
BACKEND_IMAGE="${BACKEND_IMAGE:-rustrag-backend:dev}"
FRONTEND_IMAGE="${FRONTEND_IMAGE:-rustrag-frontend:dev}"
NAMESPACE="${NAMESPACE:-rustrag}"
RELEASE="${RELEASE:-rustrag}"
START_MINIKUBE="${START_MINIKUBE:-1}"
SKIP_IMAGE_BUILD="${SKIP_IMAGE_BUILD:-0}"
MINIKUBE_RESET_ON_FAILURE="${MINIKUBE_RESET_ON_FAILURE:-1}"

. "${ROOT_DIR}/scripts/minikube/common.sh"

MINIKUBE_BIN="$(resolve_bin minikube "${ROOT_DIR}")"
KUBECTL_BIN="$(resolve_bin kubectl "${ROOT_DIR}")"
HELM_BIN="$(resolve_bin helm "${ROOT_DIR}")"

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

if [ "${SKIP_IMAGE_BUILD}" != "1" ]; then
  docker build -t "${BACKEND_IMAGE}" -f "${ROOT_DIR}/apps/api/Dockerfile" "${ROOT_DIR}"
  docker build -t "${FRONTEND_IMAGE}" -f "${ROOT_DIR}/apps/web/Dockerfile" "${ROOT_DIR}/apps/web"
fi

recover_helm_release "${HELM_BIN}" "${KUBECTL_BIN}" "${NAMESPACE}" "${RELEASE}"

"${HELM_BIN}" upgrade --install "${RELEASE}" "${CHART_DIR}" \
  --namespace "${NAMESPACE}" \
  --create-namespace \
  --values "${VALUES_FILE}" \
  --set api.image.repository="${BACKEND_IMAGE%%:*}" \
  --set api.image.tag="${BACKEND_IMAGE#*:}" \
  --set worker.image.repository="${BACKEND_IMAGE%%:*}" \
  --set worker.image.tag="${BACKEND_IMAGE#*:}" \
  --set web.image.repository="${FRONTEND_IMAGE%%:*}" \
  --set web.image.tag="${FRONTEND_IMAGE#*:}"

STARTUP_JOB="$("${KUBECTL_BIN}" --namespace "${NAMESPACE}" get jobs \
  -l app.kubernetes.io/instance="${RELEASE}",app.kubernetes.io/component=startup \
  --sort-by=.metadata.creationTimestamp \
  -o name | tail -n1 | cut -d/ -f2)"
if [ -n "${STARTUP_JOB}" ]; then
  "${KUBECTL_BIN}" wait --namespace "${NAMESPACE}" --for=condition=complete "job/${STARTUP_JOB}" --timeout=10m
fi
"${KUBECTL_BIN}" rollout status --namespace "${NAMESPACE}" "deployment/${RELEASE}-rustrag-api" --timeout=10m
"${KUBECTL_BIN}" rollout status --namespace "${NAMESPACE}" "deployment/${RELEASE}-rustrag-worker" --timeout=10m
"${KUBECTL_BIN}" rollout status --namespace "${NAMESPACE}" "deployment/${RELEASE}-rustrag-web" --timeout=10m

"${KUBECTL_BIN}" get pods --namespace "${NAMESPACE}" -o wide
echo "port-forward: ${KUBECTL_BIN} -n ${NAMESPACE} port-forward svc/${RELEASE}-rustrag-web 19000:80"
