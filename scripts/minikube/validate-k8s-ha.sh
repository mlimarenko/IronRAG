#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
CHART_DIR="${ROOT_DIR}/charts/ironrag"
VALUES_FILE="${CHART_DIR}/values/examples/bundled-s3.yaml"
BENCHMARK_SCRIPT="${ROOT_DIR}/apps/api/benchmarks/grounded_query/run_live_benchmark.py"
SMOKE_SUITE="${ROOT_DIR}/scripts/minikube/smoke-suite.json"
RELEASE="${RELEASE:-ironrag}"
NAMESPACE="${NAMESPACE:-ironrag}"
BACKEND_IMAGE="${BACKEND_IMAGE:-ironrag-backend:dev}"
FRONTEND_IMAGE="${FRONTEND_IMAGE:-ironrag-frontend:dev}"
START_MINIKUBE="${START_MINIKUBE:-1}"
SKIP_IMAGE_BUILD="${SKIP_IMAGE_BUILD:-0}"
RUN_CONTENT_SMOKE="${RUN_CONTENT_SMOKE:-1}"
STRICT_CONTENT_SMOKE="${STRICT_CONTENT_SMOKE:-0}"
MINIKUBE_RESET_ON_FAILURE="${MINIKUBE_RESET_ON_FAILURE:-1}"
BOOTSTRAP_LOGIN="${BOOTSTRAP_LOGIN:-admin}"
BOOTSTRAP_PASSWORD="${BOOTSTRAP_PASSWORD:-ChangeMe123!}"
BOOTSTRAP_DISPLAY_NAME="${BOOTSTRAP_DISPLAY_NAME:-Admin}"
WEB_LOCAL_PORT="${WEB_LOCAL_PORT:-}"

. "${ROOT_DIR}/scripts/minikube/common.sh"

read_openai_key() {
  if [ -n "${IRONRAG_OPENAI_API_KEY:-}" ]; then
    printf '%s' "${IRONRAG_OPENAI_API_KEY}"
    return
  fi

  if [ -f "${ROOT_DIR}/.env" ]; then
    python3 - <<'PY' "${ROOT_DIR}/.env"
from pathlib import Path
import sys

for line in Path(sys.argv[1]).read_text().splitlines():
    if line.startswith("IRONRAG_OPENAI_API_KEY="):
        print(line.split("=", 1)[1], end="")
        break
PY
  fi
}

MINIKUBE_BIN="$(resolve_bin minikube "${ROOT_DIR}")"
KUBECTL_BIN="$(resolve_bin kubectl "${ROOT_DIR}")"
HELM_BIN="$(resolve_bin helm "${ROOT_DIR}")"
FULLNAME="${RELEASE}-ironrag"
COOKIE_JAR="$(mktemp)"
READY_JSON="$(mktemp)"
BOOTSTRAP_JSON="$(mktemp)"
HELM_OVERRIDE="$(mktemp)"
PF_LOG_FILE="$(mktemp)"
PF_PID=""

cleanup() {
  rm -f "${COOKIE_JAR}" "${READY_JSON}" "${BOOTSTRAP_JSON}" "${HELM_OVERRIDE}" "${PF_LOG_FILE}"
  if [ -n "${PF_PID}" ] && kill -0 "${PF_PID}" >/dev/null 2>&1; then
    kill "${PF_PID}" >/dev/null 2>&1 || true
    wait "${PF_PID}" 2>/dev/null || true
  fi
}
trap cleanup EXIT

select_web_local_port() {
  if [ -n "${WEB_LOCAL_PORT}" ]; then
    printf '%s\n' "${WEB_LOCAL_PORT}"
    return
  fi

  python3 - <<'PY'
import socket

with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

wait_for_port_forward() {
  local pf_pid="$1"
  local url="$2"
  local log_file="$3"
  local attempt

  for attempt in $(seq 1 20); do
    if ! kill -0 "${pf_pid}" >/dev/null 2>&1; then
      cat "${log_file}" >&2
      return 1
    fi

    if curl -fsS "${url}" >/dev/null 2>&1; then
      return 0
    fi

    sleep 1
  done

  cat "${log_file}" >&2
  return 1
}

WEB_LOCAL_PORT="$(select_web_local_port)"

if [ "${START_MINIKUBE}" = "1" ] || ! minikube_api_ready "${KUBECTL_BIN}"; then
  ensure_minikube_control_plane \
    "${MINIKUBE_BIN}" \
    "${KUBECTL_BIN}" \
    "${MINIKUBE_RESET_ON_FAILURE}" \
    --driver=docker \
    --cpus="${MINIKUBE_CPUS:-4}" \
    --memory="${MINIKUBE_MEMORY:-8192}"
fi

if [ "${SKIP_IMAGE_BUILD}" != "1" ]; then
  BACKEND_IMAGE="${BACKEND_IMAGE}" FRONTEND_IMAGE="${FRONTEND_IMAGE}" START_MINIKUBE=0 \
    "${ROOT_DIR}/scripts/minikube/build-images.sh"
fi

OPENAI_KEY="$(read_openai_key)"

HELM_ARGS=(
  upgrade --install "${RELEASE}" "${CHART_DIR}"
  --namespace "${NAMESPACE}"
  --create-namespace
  --values "${VALUES_FILE}"
  --set "api.image.repository=${BACKEND_IMAGE%%:*}"
  --set "api.image.tag=${BACKEND_IMAGE#*:}"
  --set "worker.image.repository=${BACKEND_IMAGE%%:*}"
  --set "worker.image.tag=${BACKEND_IMAGE#*:}"
  --set "web.image.repository=${FRONTEND_IMAGE%%:*}"
  --set "web.image.tag=${FRONTEND_IMAGE#*:}"
  --wait
  --wait-for-jobs
  --timeout 20m
)
if [ -n "${OPENAI_KEY}" ]; then
  python3 - <<'PY' "${HELM_OVERRIDE}" "${OPENAI_KEY}"
from pathlib import Path
import sys

override_path = Path(sys.argv[1])
openai_key = sys.argv[2].replace("\\", "\\\\").replace('"', '\\"')
override_path.write_text(
    f'app:\n  providerSecrets:\n    openaiApiKey: "{openai_key}"\n',
    encoding="utf-8",
)
PY
  HELM_ARGS+=(--values "${HELM_OVERRIDE}")
fi
recover_helm_release "${HELM_BIN}" "${KUBECTL_BIN}" "${NAMESPACE}" "${RELEASE}"
"${HELM_BIN}" "${HELM_ARGS[@]}"

STARTUP_JOB="$("${KUBECTL_BIN}" -n "${NAMESPACE}" get jobs \
  -l "app.kubernetes.io/instance=${RELEASE},app.kubernetes.io/component=startup" \
  --sort-by=.metadata.creationTimestamp \
  -o name | tail -n1 | cut -d/ -f2)"
if [ -n "${STARTUP_JOB}" ]; then
  "${KUBECTL_BIN}" -n "${NAMESPACE}" wait --for=condition=complete "job/${STARTUP_JOB}" --timeout=10m
fi
"${KUBECTL_BIN}" -n "${NAMESPACE}" rollout status "deployment/${FULLNAME}-api" --timeout=10m
"${KUBECTL_BIN}" -n "${NAMESPACE}" rollout status "deployment/${FULLNAME}-worker" --timeout=10m
"${KUBECTL_BIN}" -n "${NAMESPACE}" rollout status "deployment/${FULLNAME}-web" --timeout=10m

"${KUBECTL_BIN}" -n "${NAMESPACE}" port-forward "svc/${FULLNAME}-web" "${WEB_LOCAL_PORT}:80" >"${PF_LOG_FILE}" 2>&1 &
PF_PID=$!
wait_for_port_forward "${PF_PID}" "http://127.0.0.1:${WEB_LOCAL_PORT}/v1/ready" "${PF_LOG_FILE}"

curl -fsS "http://127.0.0.1:${WEB_LOCAL_PORT}/v1/ready" | tee "${READY_JSON}" >/dev/null
curl -fsSI "http://127.0.0.1:${WEB_LOCAL_PORT}/" >/dev/null

API_POD="$("${KUBECTL_BIN}" -n "${NAMESPACE}" get pods \
  -l "app.kubernetes.io/instance=${RELEASE},app.kubernetes.io/component=api" \
  -o jsonpath='{.items[0].metadata.name}')"
if [ -n "${API_POD}" ]; then
  "${KUBECTL_BIN}" -n "${NAMESPACE}" delete pod "${API_POD}" --wait=true
  "${KUBECTL_BIN}" -n "${NAMESPACE}" rollout status "deployment/${FULLNAME}-api" --timeout=10m
  curl -fsS "http://127.0.0.1:${WEB_LOCAL_PORT}/v1/ready" >/dev/null
fi

if [ "${RUN_CONTENT_SMOKE}" = "1" ] && [ -n "${OPENAI_KEY}" ]; then
  CONTENT_SMOKE_READY=1
  SETUP_REQUIRED="$(curl -fsS "http://127.0.0.1:${WEB_LOCAL_PORT}/v1/iam/bootstrap/status" | python3 -c 'import json,sys; print("true" if json.load(sys.stdin)["setupRequired"] else "false")')"
  if [ "${SETUP_REQUIRED}" = "true" ]; then
    python3 - <<'PY' > "${BOOTSTRAP_JSON}" "${BOOTSTRAP_LOGIN}" "${BOOTSTRAP_PASSWORD}" "${BOOTSTRAP_DISPLAY_NAME}" "${OPENAI_KEY}"
import json
import sys

print(json.dumps({
    "login": sys.argv[1],
    "password": sys.argv[2],
    "displayName": sys.argv[3],
    "aiSetup": {
        "providerKind": "openai",
        "apiKey": sys.argv[4],
    },
}))
PY
    curl -fsS -c "${COOKIE_JAR}" \
      -H 'content-type: application/json' \
      --data @"${BOOTSTRAP_JSON}" \
      "http://127.0.0.1:${WEB_LOCAL_PORT}/v1/iam/bootstrap/setup" >/dev/null
  else
    python3 - <<'PY' > "${BOOTSTRAP_JSON}" "${BOOTSTRAP_LOGIN}" "${BOOTSTRAP_PASSWORD}"
import json
import sys

print(json.dumps({
    "login": sys.argv[1],
    "password": sys.argv[2],
}))
PY
    if ! curl -fsS -c "${COOKIE_JAR}" \
      -H 'content-type: application/json' \
      --data @"${BOOTSTRAP_JSON}" \
      "http://127.0.0.1:${WEB_LOCAL_PORT}/v1/iam/session/login" >/dev/null 2>&1; then
      CONTENT_SMOKE_READY=0
      if [ "${STRICT_CONTENT_SMOKE}" = "1" ]; then
        echo "content smoke failed: existing deployment rejected BOOTSTRAP_LOGIN credentials; provide matching BOOTSTRAP_PASSWORD or reset release state" >&2
        exit 1
      fi
      echo "content smoke skipped: existing deployment is already bootstrapped and BOOTSTRAP_LOGIN credentials were rejected" >&2
    fi
  fi

  if [ "${CONTENT_SMOKE_READY}" = "1" ]; then
    SESSION_COOKIE="$(awk '$6=="ironrag_ui_session" {print $7}' "${COOKIE_JAR}" | tail -n1)"
    if [ -z "${SESSION_COOKIE}" ]; then
      echo "failed to capture ironrag_ui_session cookie" >&2
      exit 1
    fi

    WORKSPACE_ID="$(curl -fsS -b "${COOKIE_JAR}" \
      -H 'content-type: application/json' \
      --data "{\"displayName\":\"K8s HA Smoke $(date +%Y%m%d-%H%M%S)\"}" \
      "http://127.0.0.1:${WEB_LOCAL_PORT}/v1/catalog/workspaces" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')"

    python3 "${BENCHMARK_SCRIPT}" \
      --base-url "http://127.0.0.1:${WEB_LOCAL_PORT}/v1" \
      --workspace-id "${WORKSPACE_ID}" \
      --session-cookie "${SESSION_COOKIE}" \
      --library-name "K8s HA Smoke" \
      --suite "${SMOKE_SUITE}" \
      --strict
  fi
else
  echo "content smoke skipped: IRONRAG_OPENAI_API_KEY not available or RUN_CONTENT_SMOKE=0"
fi

"${KUBECTL_BIN}" -n "${NAMESPACE}" get deploy,pods,jobs
