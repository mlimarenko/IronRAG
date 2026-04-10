#!/usr/bin/env bash

resolve_bin() {
  local name="$1"
  local root_dir="$2"

  if [ -x "${root_dir}/.tools/bin/${name}" ]; then
    printf '%s\n' "${root_dir}/.tools/bin/${name}"
    return
  fi

  command -v "${name}"
}

minikube_api_ready() {
  local kubectl_bin="$1"

  "${kubectl_bin}" version --request-timeout=5s >/dev/null 2>&1
}

wait_for_minikube_api() {
  local kubectl_bin="$1"
  local attempts="${2:-24}"
  local delay_seconds="${3:-5}"
  local attempt

  for attempt in $(seq 1 "${attempts}"); do
    if minikube_api_ready "${kubectl_bin}"; then
      return 0
    fi
    sleep "${delay_seconds}"
  done

  return 1
}

ensure_minikube_control_plane() {
  local minikube_bin="$1"
  local kubectl_bin="$2"
  local reset_on_failure="${3:-1}"
  shift 3
  local start_args=("$@")

  if minikube_api_ready "${kubectl_bin}"; then
    return 0
  fi

  echo "minikube control plane is unavailable; running minikube start" >&2
  if ! timeout "${MINIKUBE_START_TIMEOUT:-5m}" "${minikube_bin}" start "${start_args[@]}"; then
    echo "minikube start did not complete cleanly" >&2
  fi

  if wait_for_minikube_api "${kubectl_bin}"; then
    return 0
  fi

  if [ "${reset_on_failure}" != "1" ]; then
    echo "minikube control plane is still unavailable after start" >&2
    return 1
  fi

  echo "minikube control plane is still unavailable after start; recreating profile" >&2
  timeout "${MINIKUBE_DELETE_TIMEOUT:-5m}" "${minikube_bin}" delete >/dev/null 2>&1 || true
  timeout "${MINIKUBE_START_TIMEOUT:-5m}" "${minikube_bin}" start "${start_args[@]}"
  wait_for_minikube_api "${kubectl_bin}"
}

recover_helm_release() {
  local helm_bin="$1"
  local kubectl_bin="$2"
  local namespace="$3"
  local release="$4"
  local history_output latest_revision latest_status deployed_revision

  if ! history_output="$("${helm_bin}" -n "${namespace}" history "${release}" 2>/dev/null)"; then
    return
  fi

  latest_revision="$(printf '%s\n' "${history_output}" | awk -F '\t' 'NR > 1 { revision = $1 } END { gsub(/^ +| +$/, "", revision); print revision }')"
  latest_status="$(printf '%s\n' "${history_output}" | awk -F '\t' 'NR > 1 { status = $3 } END { gsub(/^ +| +$/, "", status); print status }')"
  if [ -z "${latest_revision}" ] || [[ "${latest_status}" != pending-* ]]; then
    return
  fi

  deployed_revision="$(printf '%s\n' "${history_output}" | awk -F '\t' '
    NR > 1 {
      status = $3
      gsub(/^ +| +$/, "", status)
      if (status == "deployed") {
        revision = $1
      }
    }
    END {
      gsub(/^ +| +$/, "", revision)
      print revision
    }
  ')"
  if [ -z "${deployed_revision}" ]; then
    echo "cannot recover helm release ${release}: no deployed revision found before ${latest_status}" >&2
    return 1
  fi

  echo "recovering helm release ${release}: rollback pending revision ${latest_revision} to deployed revision ${deployed_revision}" >&2
  if ! "${helm_bin}" -n "${namespace}" rollback "${release}" "${deployed_revision}" --wait --timeout 20m >/dev/null; then
    echo "rollback failed for ${release}; deleting pending helm secret for revision ${latest_revision}" >&2
    "${kubectl_bin}" -n "${namespace}" delete secret "sh.helm.release.v1.${release}.v${latest_revision}" >/dev/null
  fi
}
