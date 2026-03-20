#!/usr/bin/env bash
set -euo pipefail

source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/common.sh"

smoke_require_commands curl jq zip magick libreoffice

api_base=${RUSTRAG_SMOKE_API_BASE:-http://127.0.0.1:19000/v1}
token=${RUSTRAG_SMOKE_TOKEN:-}
library_id=${RUSTRAG_SMOKE_LIBRARY_ID:-}
output_dir=
fixture_dir=
wait_mode=true

while (($# > 0)); do
  case "$1" in
    --token)
      token=$2
      shift 2
      ;;
    --library-id)
      library_id=$2
      shift 2
      ;;
    --output-dir)
      output_dir=$2
      shift 2
      ;;
    --fixture-dir)
      fixture_dir=$2
      shift 2
      ;;
    --no-wait)
      wait_mode=false
      shift
      ;;
    *)
      printf 'unknown argument: %s\n' "$1" >&2
      exit 1
      ;;
  esac
done

if [[ -z ${token} || -z ${library_id} ]]; then
  printf 'usage: upload-fixture-set.sh --token TOKEN --library-id LIBRARY_ID [--output-dir DIR] [--fixture-dir DIR] [--no-wait]\n' >&2
  exit 1
fi

if [[ -z ${output_dir} ]]; then
  output_dir=$(mktemp -d)
else
  mkdir -p "${output_dir}"
fi

if [[ -z ${fixture_dir} ]]; then
  fixture_dir="${output_dir}/fixtures"
  smoke_generate_fixture_set "${fixture_dir}"
fi

upload_output="${output_dir}/upload.json"
smoke_upload_fixture_set "${api_base}" "${token}" "${library_id}" "${fixture_dir}" "${upload_output}"

documents_output=""
if [[ ${wait_mode} == "true" ]]; then
  documents_output="${output_dir}/documents-final.json"
  smoke_poll_documents_until_terminal "${api_base}" "${token}" "${library_id}" "${documents_output}"
fi

jq -n \
  --arg library_id "${library_id}" \
  --arg fixture_dir "${fixture_dir}" \
  --arg upload_output "${upload_output}" \
  --arg documents_output "${documents_output}" \
  '{
    libraryId: $library_id,
    fixtureDir: $fixture_dir,
    uploadOutput: $upload_output,
    documentsOutput: $documents_output
  }' > "${output_dir}/upload-summary.json"

printf '%s\n' "${output_dir}/upload-summary.json"
