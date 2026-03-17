#!/usr/bin/env bash

smoke_repo_root() {
  local script_dir
  script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
  cd -- "${script_dir}/../.." && pwd
}

smoke_timestamp_utc() {
  date -u +"%Y-%m-%dT%H:%M:%SZ"
}

smoke_require_commands() {
  local missing=()
  local command_name
  for command_name in "$@"; do
    if ! command -v "${command_name}" >/dev/null 2>&1; then
      missing+=("${command_name}")
    fi
  done

  if ((${#missing[@]} > 0)); then
    printf 'missing required commands: %s\n' "${missing[*]}" >&2
    return 1
  fi
}

smoke_request() {
  local output_file=$1
  shift

  local status
  status=$(curl -sS "$@" -o "${output_file}" -w '%{http_code}')
  if [[ ${status:0:1} != "2" ]]; then
    printf 'request failed with status %s\n' "${status}" >&2
    cat "${output_file}" >&2
    return 1
  fi
}

smoke_login_ui() {
  local api_base=$1
  local cookie_jar=$2
  local output_file=$3
  local login=${RUSTRAG_SMOKE_LOGIN:-admin}
  local password=${RUSTRAG_SMOKE_LOGIN_PASSWORD:-rustrag}
  local payload
  payload=$(
    jq -nc \
      --arg login "${login}" \
      --arg password "${password}" \
      '{login: $login, password: $password}'
  )

  smoke_request \
    "${output_file}" \
    -c "${cookie_jar}" \
    -H 'Content-Type: application/json' \
    -d "${payload}" \
    "${api_base}/ui/auth/login"
}

smoke_create_workspace() {
  local api_base=$1
  local cookie_jar=$2
  local name=$3
  local output_file=$4
  local payload
  payload=$(jq -nc --arg name "${name}" '{name: $name}')

  smoke_request \
    "${output_file}" \
    -b "${cookie_jar}" \
    -H 'Content-Type: application/json' \
    -d "${payload}" \
    "${api_base}/ui/workspaces"
}

smoke_create_library() {
  local api_base=$1
  local cookie_jar=$2
  local workspace_id=$3
  local name=$4
  local output_file=$5
  local payload
  payload=$(
    jq -nc \
      --arg workspace_id "${workspace_id}" \
      --arg name "${name}" \
      '{workspace_id: $workspace_id, name: $name}'
  )

  smoke_request \
    "${output_file}" \
    -b "${cookie_jar}" \
    -H 'Content-Type: application/json' \
    -d "${payload}" \
    "${api_base}/ui/libraries"
}

smoke_update_context() {
  local api_base=$1
  local cookie_jar=$2
  local workspace_id=$3
  local library_id=$4
  local output_file=$5
  local payload
  payload=$(
    jq -nc \
      --arg workspace_id "${workspace_id}" \
      --arg library_id "${library_id}" \
      '{workspace_id: $workspace_id, library_id: $library_id}'
  )

  smoke_request \
    "${output_file}" \
    -X PUT \
    -b "${cookie_jar}" \
    -H 'Content-Type: application/json' \
    -d "${payload}" \
    "${api_base}/ui/context"
}

smoke_create_api_token() {
  local api_base=$1
  local cookie_jar=$2
  local label=$3
  local scopes_json=$4
  local output_file=$5
  local expires_in_days=${6:-7}
  local payload
  payload=$(
    jq -nc \
      --arg label "${label}" \
      --argjson scopes "${scopes_json}" \
      --argjson expires_in_days "${expires_in_days}" \
      '{label: $label, scopes: $scopes, expires_in_days: $expires_in_days}'
  )

  smoke_request \
    "${output_file}" \
    -b "${cookie_jar}" \
    -H 'Content-Type: application/json' \
    -d "${payload}" \
    "${api_base}/ui/admin/api-tokens"
}

smoke_apply_provider_profile() {
  local api_base=$1
  local token=$2
  local library_id=$3
  local indexing_provider_kind=$4
  local indexing_model=$5
  local embedding_provider_kind=$6
  local embedding_model=$7
  local answer_provider_kind=$8
  local answer_model=$9
  local vision_provider_kind=${10}
  local vision_model=${11}
  local output_file=${12}
  local payload
  payload=$(
    jq -nc \
      --arg indexing_provider_kind "${indexing_provider_kind}" \
      --arg indexing_model "${indexing_model}" \
      --arg embedding_provider_kind "${embedding_provider_kind}" \
      --arg embedding_model "${embedding_model}" \
      --arg answer_provider_kind "${answer_provider_kind}" \
      --arg answer_model "${answer_model}" \
      --arg vision_provider_kind "${vision_provider_kind}" \
      --arg vision_model "${vision_model}" \
      '{
        indexingProviderKind: $indexing_provider_kind,
        indexingModelName: $indexing_model,
        embeddingProviderKind: $embedding_provider_kind,
        embeddingModelName: $embedding_model,
        answerProviderKind: $answer_provider_kind,
        answerModelName: $answer_model,
        visionProviderKind: $vision_provider_kind,
        visionModelName: $vision_model
      }'
  )

  smoke_request \
    "${output_file}" \
    -X PUT \
    -H "Authorization: Bearer ${token}" \
    -H 'Content-Type: application/json' \
    -d "${payload}" \
    "${api_base}/runtime/libraries/${library_id}/provider-profile"
}

smoke_validate_provider() {
  local api_base=$1
  local token=$2
  local provider_kind=$3
  local model_name=$4
  local capability=$5
  local output_file=$6
  local payload
  payload=$(
    jq -nc \
      --arg provider_kind "${provider_kind}" \
      --arg model_name "${model_name}" \
      --arg capability "${capability}" \
      '{
        providerKind: $provider_kind,
        modelName: $model_name,
        capability: $capability
      }'
  )

  smoke_request \
    "${output_file}" \
    -X POST \
    -H "Authorization: Bearer ${token}" \
    -H 'Content-Type: application/json' \
    -d "${payload}" \
    "${api_base}/runtime/providers/validate"

  local status
  status=$(jq -r '.status // empty' "${output_file}")
  if [[ "${status}" != "passed" ]]; then
    printf 'provider validation failed: %s %s %s\n' "${provider_kind}" "${capability}" "${model_name}" >&2
    cat "${output_file}" >&2
    return 1
  fi
}

smoke_generate_fixture_set() {
  local output_dir=$1

  mkdir -p "${output_dir}" "${output_dir}/.lo-profile" "${output_dir}/.docx/word" \
    "${output_dir}/.docx/_rels" "${output_dir}/.docx/docProps"

  cat > "${output_dir}/runtime-notes.txt" <<'EOF'
RustRAG runtime smoke notes.
Research budget approved for machine learning experiments.
Sarah Chen owns the annual market analysis workstream.
EOF

  cat > "${output_dir}/runtime-brief.md" <<'EOF'
# Runtime Brief

The annual report links machine learning, deep learning, and research budget planning.
EOF

  cat > "${output_dir}/runtime-report-source.txt" <<'EOF'
Runtime PDF source document.
Annual report 2026 includes machine learning growth and research budget links.
EOF

  libreoffice \
    --headless \
    --nologo \
    --nodefault \
    --nofirststartwizard \
    --norestore \
    "-env:UserInstallation=file://${output_dir}/.lo-profile" \
    --convert-to pdf \
    --outdir "${output_dir}" \
    "${output_dir}/runtime-report-source.txt" >/dev/null 2>&1
  mv "${output_dir}/runtime-report-source.pdf" "${output_dir}/runtime-report.pdf"
  rm -f "${output_dir}/runtime-report-source.txt"

  cat > "${output_dir}/.docx/[Content_Types].xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
  <Override PartName="/docProps/core.xml" ContentType="application/vnd.openxmlformats-package.core-properties+xml"/>
  <Override PartName="/docProps/app.xml" ContentType="application/vnd.openxmlformats-officedocument.extended-properties+xml"/>
</Types>
EOF

  cat > "${output_dir}/.docx/_rels/.rels" <<'EOF'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
  <Relationship Id="rId2" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/core-properties" Target="docProps/core.xml"/>
  <Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/extended-properties" Target="docProps/app.xml"/>
</Relationships>
EOF

  cat > "${output_dir}/.docx/docProps/core.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<cp:coreProperties xmlns:cp="http://schemas.openxmlformats.org/package/2006/metadata/core-properties" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:dcterms="http://purl.org/dc/terms/" xmlns:dcmitype="http://purl.org/dc/dcmitype/" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <dc:title>Runtime Plan</dc:title>
  <dc:creator>Codex</dc:creator>
</cp:coreProperties>
EOF

  cat > "${output_dir}/.docx/docProps/app.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/extended-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">
  <Application>Codex</Application>
</Properties>
EOF

  cat > "${output_dir}/.docx/word/document.xml" <<'EOF'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body>
    <w:p><w:r><w:t>Runtime DOCX plan.</w:t></w:r></w:p>
    <w:p><w:r><w:t>Dr. Sarah Chen coordinates neural networks experiments and research budget tracking.</w:t></w:r></w:p>
  </w:body>
</w:document>
EOF

  (
    cd "${output_dir}/.docx"
    zip -qr "${output_dir}/runtime-plan.docx" .
  )

  magick \
    -size 1200x800 \
    xc:white \
    -font DejaVu-Sans \
    -fill '#1F3A8A' \
    -pointsize 38 \
    -gravity NorthWest \
    -annotate +90+110 'Machine Learning Budget' \
    -pointsize 26 \
    -annotate +90+180 'Sarah Chen links annual report and research budget.' \
    "${output_dir}/runtime-graph.png"

  rm -rf "${output_dir}/.docx" "${output_dir}/.lo-profile"
}

smoke_upload_fixture_set() {
  local api_base=$1
  local token=$2
  local library_id=$3
  local fixture_dir=$4
  local output_file=$5

  smoke_request \
    "${output_file}" \
    -X POST \
    -H "Authorization: Bearer ${token}" \
    -F "files=@${fixture_dir}/runtime-notes.txt" \
    -F "files=@${fixture_dir}/runtime-brief.md" \
    -F "files=@${fixture_dir}/runtime-report.pdf" \
    -F "files=@${fixture_dir}/runtime-plan.docx" \
    -F "files=@${fixture_dir}/runtime-graph.png" \
    "${api_base}/runtime/libraries/${library_id}/documents"
}

smoke_poll_documents_until_terminal() {
  local api_base=$1
  local token=$2
  local library_id=$3
  local output_file=$4
  local timeout_seconds=${5:-600}
  local sleep_seconds=${6:-2}
  local deadline=$((SECONDS + timeout_seconds))
  local processing_count

  while ((SECONDS <= deadline)); do
    smoke_request \
      "${output_file}" \
      -H "Authorization: Bearer ${token}" \
      "${api_base}/runtime/libraries/${library_id}/documents"
    processing_count=$(jq '[.rows[] | select(.status == "queued" or .status == "processing")] | length' "${output_file}")
    if [[ ${processing_count} == "0" ]]; then
      return 0
    fi
    sleep "${sleep_seconds}"
  done

  printf 'timed out waiting for terminal document states in library %s\n' "${library_id}" >&2
  cat "${output_file}" >&2
  return 1
}
