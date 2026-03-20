#!/usr/bin/env bash
set -euo pipefail

source "$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/common.sh"

smoke_require_commands docker jq curl zip magick libreoffice

repo_root=$(smoke_repo_root)
api_base=${RUSTRAG_SMOKE_API_BASE:-http://127.0.0.1:19000/v1}
output_dir=${1:-"${repo_root}/docs/checkpoints/runtime-smoke/deepseek-$(date -u +%Y%m%dT%H%M%SZ)"}
primary_provider_kind=deepseek
indexing_provider_kind=${RUSTRAG_DEEPSEEK_INDEXING_PROVIDER:-deepseek}
indexing_model=${RUSTRAG_DEEPSEEK_INDEXING_MODEL:-deepseek-chat}
embedding_provider_kind=${RUSTRAG_DEEPSEEK_EMBEDDING_PROVIDER:-openai}
embedding_model=${RUSTRAG_DEEPSEEK_EMBEDDING_MODEL:-text-embedding-3-large}
answer_provider_kind=${RUSTRAG_DEEPSEEK_ANSWER_PROVIDER:-deepseek}
answer_model=${RUSTRAG_DEEPSEEK_ANSWER_MODEL:-deepseek-reasoner}
vision_provider_kind=${RUSTRAG_DEEPSEEK_VISION_PROVIDER:-openai}
vision_model=${RUSTRAG_DEEPSEEK_VISION_MODEL:-gpt-5-mini}
full_scopes='["providers:admin","documents:read","documents:write","graph:read","query:read","query:write"]'

mkdir -p "${output_dir}"

(
  cd "${repo_root}"
  docker compose up -d nginx >/dev/null
)
"${repo_root}/scripts/smoke/wait-for-runtime.sh" >/dev/null

cookie_jar=$(mktemp)
trap 'rm -f "${cookie_jar}"' EXIT

smoke_login_ui "${api_base}" "${cookie_jar}" "${output_dir}/login.json"
workspace_name="DeepSeek Runtime Smoke $(date -u +%H%M%S)"
library_name="DeepSeek Fixture Library"
smoke_create_workspace "${api_base}" "${cookie_jar}" "${workspace_name}" "${output_dir}/workspace.json"
workspace_id=$(jq -r '.id' "${output_dir}/workspace.json")
smoke_create_library "${api_base}" "${cookie_jar}" "${workspace_id}" "${library_name}" "${output_dir}/library.json"
library_id=$(jq -r '.id' "${output_dir}/library.json")
smoke_update_context "${api_base}" "${cookie_jar}" "${workspace_id}" "${library_id}" "${output_dir}/context.json"
smoke_create_api_token "${api_base}" "${cookie_jar}" "deepseek runtime smoke" "${full_scopes}" "${output_dir}/token.json"
token=$(jq -r '.plaintext_token' "${output_dir}/token.json")

smoke_apply_provider_profile \
  "${api_base}" \
  "${token}" \
  "${library_id}" \
  "${indexing_provider_kind}" \
  "${indexing_model}" \
  "${embedding_provider_kind}" \
  "${embedding_model}" \
  "${answer_provider_kind}" \
  "${answer_model}" \
  "${vision_provider_kind}" \
  "${vision_model}" \
  "${output_dir}/provider-profile.json"

smoke_validate_provider "${api_base}" "${token}" "${answer_provider_kind}" "${answer_model}" chat "${output_dir}/validate-chat.json"
smoke_validate_provider "${api_base}" "${token}" "${embedding_provider_kind}" "${embedding_model}" embeddings "${output_dir}/validate-embeddings.json"
smoke_validate_provider "${api_base}" "${token}" "${vision_provider_kind}" "${vision_model}" vision "${output_dir}/validate-vision.json"

upload_summary=$(
  "${repo_root}/scripts/smoke/upload-fixture-set.sh" \
    --token "${token}" \
    --library-id "${library_id}" \
    --output-dir "${output_dir}"
)
documents_output=$(jq -r '.documentsOutput' "${upload_summary}")

smoke_request \
  "${output_dir}/graph-surface.json" \
  -H "Authorization: Bearer ${token}" \
  "${api_base}/runtime/libraries/${library_id}/graph/surface"
smoke_request \
  "${output_dir}/graph-diagnostics.json" \
  -H "Authorization: Bearer ${token}" \
  "${api_base}/runtime/libraries/${library_id}/graph/diagnostics"

answer_payload=$(
  jq -nc \
    '{question: "Summarize the main entities and budget relationships in this library.", mode: "hybrid", topK: 8, includeDebug: true}'
)
smoke_request \
  "${output_dir}/answer.json" \
  -X POST \
  -H "Authorization: Bearer ${token}" \
  -H 'Content-Type: application/json' \
  -d "${answer_payload}" \
  "${api_base}/runtime/libraries/${library_id}/queries/answer"

data_payload=$(
  jq -nc \
    '{question: "Which documents support the research budget node?", mode: "mix", topK: 8, includeDebug: true}'
)
smoke_request \
  "${output_dir}/data-query.json" \
  -X POST \
  -H "Authorization: Bearer ${token}" \
  -H 'Content-Type: application/json' \
  -d "${data_payload}" \
  "${api_base}/runtime/libraries/${library_id}/queries/data"

delete_document_id=$(jq -r '.rows[] | select(.fileName == "runtime-plan.docx") | .id' "${documents_output}" | head -n 1)
if [[ -n ${delete_document_id} ]]; then
  smoke_request \
    "${output_dir}/delete.json" \
    -X DELETE \
    -H "Authorization: Bearer ${token}" \
    "${api_base}/runtime/libraries/${library_id}/documents/${delete_document_id}"
  smoke_request \
    "${output_dir}/graph-diagnostics-after-delete.json" \
    -H "Authorization: Bearer ${token}" \
    "${api_base}/runtime/libraries/${library_id}/graph/diagnostics"
fi

reprocess_document_id=$(jq -r '.rows[] | select(.fileName == "runtime-notes.txt") | .id' "${documents_output}" | head -n 1)
if [[ -n ${reprocess_document_id} ]]; then
  smoke_request \
    "${output_dir}/reprocess.json" \
    -X POST \
    -H "Authorization: Bearer ${token}" \
    "${api_base}/runtime/libraries/${library_id}/documents/${reprocess_document_id}/reprocess"
  smoke_poll_documents_until_terminal \
    "${api_base}" \
    "${token}" \
    "${library_id}" \
    "${output_dir}/documents-after-reprocess.json"
  smoke_request \
    "${output_dir}/graph-diagnostics-after-reprocess.json" \
    -H "Authorization: Bearer ${token}" \
    "${api_base}/runtime/libraries/${library_id}/graph/diagnostics"
fi

jq -n \
  --arg provider "${primary_provider_kind}" \
  --arg indexing_provider "${indexing_provider_kind}" \
  --arg indexing_model "${indexing_model}" \
  --arg embedding_provider "${embedding_provider_kind}" \
  --arg embedding_model "${embedding_model}" \
  --arg answer_provider "${answer_provider_kind}" \
  --arg answer_model "${answer_model}" \
  --arg vision_provider "${vision_provider_kind}" \
  --arg vision_model "${vision_model}" \
  --arg workspace_id "${workspace_id}" \
  --arg library_id "${library_id}" \
  --arg generated_at "$(smoke_timestamp_utc)" \
  --argjson documents "$(cat "${documents_output}")" \
  --argjson graph "$(cat "${output_dir}/graph-surface.json")" \
  --argjson answer "$(cat "${output_dir}/answer.json")" \
  '{
    provider: $provider,
    generatedAt: $generated_at,
    workspaceId: $workspace_id,
    libraryId: $library_id,
    profile: {
      indexing: { providerKind: $indexing_provider, modelName: $indexing_model },
      embedding: { providerKind: $embedding_provider, modelName: $embedding_model },
      answer: { providerKind: $answer_provider, modelName: $answer_model },
      vision: { providerKind: $vision_provider, modelName: $vision_model }
    },
    documentStatuses: ($documents.rows | map({fileName, status, stage, progressPercent})),
    graphCounts: {
      nodes: $graph.nodeCount,
      edges: $graph.relationCount
    },
    answerGrounding: {
      mode: $answer.mode,
      groundingStatus: $answer.groundingStatus
    }
  }' > "${output_dir}/summary.json"

printf '%s\n' "${output_dir}"
