#!/usr/bin/env bash
set -euo pipefail

fixtures_dir=${1:-}
if [[ -z "${fixtures_dir}" ]]; then
  echo "usage: check-fixtures.sh <fixtures-dir>" >&2
  exit 1
fi

required=(
  release.txt
  release.md
  release.csv
  release.json
  release.html
  release.rtf
  release.docx
  release.pdf
  release.png
)

for name in "${required[@]}"; do
  path="${fixtures_dir}/${name}"
  if [[ ! -f "${path}" ]]; then
    echo "missing fixture: ${name}" >&2
    exit 2
  fi
  if [[ ! -s "${path}" ]]; then
    echo "empty fixture: ${name}" >&2
    exit 3
  fi
done

echo "ok"
