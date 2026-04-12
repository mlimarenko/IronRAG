use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{arangodb::document_store::KnowledgeRevisionRow, repositories::ingest_repository},
    services::ingest::worker::{CanonicalExtractContentError, CanonicalExtractedContent},
    shared::extraction::file_extract::build_inline_text_extraction_plan,
};

fn canonical_revision_file_name(revision: &KnowledgeRevisionRow) -> String {
    let source_name = revision
        .source_uri
        .as_deref()
        .and_then(|value| value.split_once("://").map(|(_, rest)| rest).or(Some(value)))
        .and_then(|value| value.rsplit('/').next())
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "inline")
        .map(str::to_string);
    source_name
        .or_else(|| {
            revision
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
        .unwrap_or_else(|| format!("revision-{}", revision.revision_id))
}

pub(super) async fn resolve_canonical_extract_content(
    state: &AppState,
    job: &ingest_repository::IngestJobRow,
    revision: &KnowledgeRevisionRow,
) -> Result<CanonicalExtractedContent, CanonicalExtractContentError> {
    if let Some(text) = revision
        .normalized_text
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
    {
        let extraction_plan = build_inline_text_extraction_plan(&text);
        return Ok(CanonicalExtractedContent {
            provider_kind: extraction_plan.provider_kind.clone(),
            model_name: extraction_plan.model_name.clone(),
            usage_json: extraction_plan.usage_json.clone(),
            extraction_plan,
            stage_details: serde_json::json!({
                "contentLength": text.chars().count(),
                "source": "knowledge_revision",
            }),
        });
    }

    let storage_ref = match revision
        .storage_ref
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(storage_ref) => storage_ref.to_string(),
        None => state
            .canonical_services
            .content
            .resolve_revision_storage_key(state, revision.revision_id)
            .await
            .map_err(|_| {
                CanonicalExtractContentError::missing_stored_source(job.id, revision.revision_id)
            })?
            .ok_or_else(|| {
                CanonicalExtractContentError::missing_stored_source(job.id, revision.revision_id)
            })?,
    };
    let stored_bytes =
        state.content_storage.read_revision_source(&storage_ref).await.map_err(|error| {
            CanonicalExtractContentError::stored_source_read(&storage_ref, error)
        })?;
    let file_name = canonical_revision_file_name(revision);
    let plan = state
        .canonical_services
        .content
        .build_runtime_extraction_plan(
            state,
            revision.library_id,
            &file_name,
            Some(revision.mime_type.as_str()),
            &stored_bytes,
        )
        .await
        .map_err(|rejection| CanonicalExtractContentError::extraction_rejected(&rejection))?;
    let text = plan.normalized_text.clone().unwrap_or_default();
    Ok(CanonicalExtractedContent {
        provider_kind: plan.provider_kind.clone(),
        model_name: plan.model_name.clone(),
        usage_json: plan.usage_json.clone(),
        extraction_plan: plan.clone(),
        stage_details: serde_json::json!({
            "contentLength": text.chars().count(),
            "fileKind": plan.file_kind.as_str(),
            "warningCount": plan.extraction_warnings.len(),
            "lineCount": plan.source_format_metadata.line_count,
            "pageCount": plan.source_format_metadata.page_count,
            "normalizationProfile": plan.normalization_profile,
            "source": "content_storage",
            "storageRef": storage_ref,
        }),
    })
}

pub(super) async fn generate_document_summary_from_blocks(
    state: &AppState,
    revision_id: Uuid,
) -> anyhow::Result<String> {
    let blocks = state
        .arango_document_store
        .list_structured_blocks_by_revision(revision_id)
        .await
        .unwrap_or_default();

    if blocks.is_empty() {
        return Ok(String::new());
    }

    let mut parts = Vec::new();
    let mut chars_used = 0_usize;
    let max_summary_chars = 600;

    for block in &blocks {
        if chars_used >= max_summary_chars {
            break;
        }

        let text = block.text.trim();
        if text.is_empty() {
            continue;
        }

        if text.len() < 10 && block.block_kind != "heading" {
            continue;
        }

        let remaining = max_summary_chars.saturating_sub(chars_used);
        let truncated = if text.len() > remaining {
            &text[..text.floor_char_boundary(remaining)]
        } else {
            text
        };

        parts.push(truncated.to_string());
        chars_used += truncated.len();
    }

    Ok(parts.join(" ").trim().to_string())
}
