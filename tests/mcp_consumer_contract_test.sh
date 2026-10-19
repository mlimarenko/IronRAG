#!/usr/bin/env bash
# Guards public MCP consumers against identifiers retired by the clean-v1 contract.
set -euo pipefail

readonly RETIRED_TOOL_PATTERN='upload_documents|update_document|get_mutation_status|submit_web_ingest_run|get_web_ingest_run|list_web_ingest_run_pages|cancel_web_ingest_run'
readonly RETIRED_MUTATION_FIELD_PATTERN='operationKind|receiptId|libraryId'
readonly -a CONSUMER_FILES=(
  'tests/graph_mutation_test.sh'
  'tests/diff_ingest_test.sh'
  'tests/quality_gate.sh'
  'docs/en/MCP.md'
  'docs/ru/MCP.md'
  'README.md'
)
readonly -a MCP_MUTATION_CONSUMER_FILES=(
  'tests/graph_mutation_test.sh'
  'tests/diff_ingest_test.sh'
  'docs/en/MCP.md'
  'docs/ru/MCP.md'
)
readonly -a REQUIRED_DOCUMENT_TOOLS=(
  'create_documents'
  'create_document_revision'
  'delete_document'
  'get_operation'
)
readonly -a REQUIRED_WEB_TOOLS=(
  'submit_web_run'
  'get_web_run'
  'list_web_run_pages'
  'cancel_web_run'
)

for file in "${CONSUMER_FILES[@]}"; do
  test -f "$file" || {
    printf 'Missing MCP consumer file: %s\n' "$file" >&2
    exit 1
  }

done

if grep -nE "$RETIRED_TOOL_PATTERN" "${CONSUMER_FILES[@]}" ||
  grep -nE "$RETIRED_MUTATION_FIELD_PATTERN" "${MCP_MUTATION_CONSUMER_FILES[@]}"; then
  printf 'Retired clean-v1 MCP tool name or field found above.\n' >&2
  exit 1
fi

for tool in "${REQUIRED_DOCUMENT_TOOLS[@]}" "${REQUIRED_WEB_TOOLS[@]}"; do
  if ! grep -qF "$tool" docs/en/MCP.md || ! grep -qF "$tool" docs/ru/MCP.md; then
    printf 'Required MCP tool missing from localized documentation: %s\n' "$tool" >&2
    exit 1
  fi
done

grep -qF 'create_document_revision' tests/graph_mutation_test.sh
grep -qF 'create_document_revision' tests/diff_ingest_test.sh
grep -qF '/mcp/diagnostics' tests/graph_mutation_test.sh
grep -qF '/mcp/diagnostics' tests/diff_ingest_test.sh
grep -qF 'grounded_answer' README.md

echo 'MCP consumer contract check passed.'
