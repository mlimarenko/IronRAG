#!/usr/bin/env bash
set -euo pipefail

base_dir=${1:-"${TMPDIR:-/tmp}/rustrag-release-validation"}
mkdir -p "${base_dir}"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run_dir="${base_dir}/${run_id}"
mkdir -p "${run_dir}/artifacts" "${run_dir}/fixtures"

cat <<EOF
{
  "runId": "${run_id}",
  "runDir": "${run_dir}",
  "fixturesDir": "${run_dir}/fixtures",
  "artifactsDir": "${run_dir}/artifacts"
}
EOF
