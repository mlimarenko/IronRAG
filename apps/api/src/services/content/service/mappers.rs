use sha2::{Digest, Sha256};

use crate::{
    domains::content::{
        ContentChunk, ContentDocument, ContentDocumentPipelineJob, ContentMutation,
        ContentMutationItem, ContentRevision, ContentRevisionReadiness, WebPageProvenance,
    },
    domains::knowledge::StructuredDocumentRevision,
    infra::arangodb::document_store::{
        KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeRevisionRow,
        KnowledgeStructuredRevisionRow,
    },
    infra::repositories::{content_repository, ingest_repository},
    services::ingest::service::IngestJobHandle,
};

pub(super) fn segment_excerpt(text: &str) -> String {
    const EXCERPT_LIMIT: usize = 180;
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= EXCERPT_LIMIT {
        compact
    } else {
        let prefix = compact.chars().take(EXCERPT_LIMIT).collect::<String>();
        format!("{prefix}...")
    }
}

pub(super) fn map_knowledge_document_row(row: &KnowledgeDocumentRow) -> ContentDocument {
    ContentDocument {
        id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key.clone(),
        document_state: row.document_state.clone(),
        created_at: row.created_at,
    }
}

pub(super) fn map_document_row(row: content_repository::ContentDocumentRow) -> ContentDocument {
    ContentDocument {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        external_key: row.external_key,
        document_state: row.document_state,
        created_at: row.created_at,
    }
}

pub(super) fn map_revision_row(row: content_repository::ContentRevisionRow) -> ContentRevision {
    ContentRevision {
        id: row.id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        revision_number: row.revision_number,
        parent_revision_id: row.parent_revision_id,
        content_source_kind: row.content_source_kind,
        checksum: row.checksum,
        mime_type: row.mime_type,
        byte_size: row.byte_size,
        title: row.title,
        language_code: row.language_code,
        source_uri: row.source_uri,
        document_hint: row.document_hint,
        storage_key: row.storage_key,
        created_by_principal_id: row.created_by_principal_id,
        created_at: row.created_at,
    }
}

pub(super) fn map_knowledge_revision_row(row: KnowledgeRevisionRow) -> ContentRevision {
    ContentRevision {
        id: row.revision_id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        revision_number: i32::try_from(row.revision_number).unwrap_or(i32::MAX),
        parent_revision_id: None,
        content_source_kind: row.revision_kind,
        checksum: row.checksum,
        mime_type: row.mime_type,
        byte_size: row.byte_size,
        title: row.title,
        language_code: None,
        source_uri: row.source_uri,
        document_hint: row.document_hint,
        storage_key: row.storage_ref,
        created_by_principal_id: None,
        created_at: row.created_at,
    }
}

pub(super) fn map_knowledge_revision_readiness(
    row: KnowledgeRevisionRow,
) -> ContentRevisionReadiness {
    ContentRevisionReadiness {
        revision_id: row.revision_id,
        text_state: row.text_state,
        vector_state: row.vector_state,
        graph_state: row.graph_state,
        text_readable_at: row.text_readable_at,
        vector_ready_at: row.vector_ready_at,
        graph_ready_at: row.graph_ready_at,
    }
}

pub(super) fn map_structured_revision_row(
    row: KnowledgeStructuredRevisionRow,
) -> StructuredDocumentRevision {
    let outline = serde_json::from_value(row.outline_json).unwrap_or_default();
    StructuredDocumentRevision {
        revision_id: row.revision_id,
        document_id: row.document_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        preparation_state: row.preparation_state,
        normalization_profile: row.normalization_profile,
        source_format: row.source_format,
        language_code: row.language_code,
        block_count: row.block_count,
        chunk_count: row.chunk_count,
        typed_fact_count: row.typed_fact_count,
        outline,
        prepared_at: row.prepared_at,
    }
}

pub(super) fn map_web_page_provenance_row(
    row: &ingest_repository::WebDiscoveredPageRow,
) -> WebPageProvenance {
    WebPageProvenance {
        run_id: Some(row.run_id),
        candidate_id: Some(row.id),
        source_uri: row.final_url.clone().or(row.discovered_url.clone()),
        canonical_url: row.canonical_url.clone().or(row.final_url.clone()),
    }
}

pub(super) fn map_structured_revision_data(
    data: &crate::shared::extraction::structured_document::StructuredDocumentRevisionData,
) -> StructuredDocumentRevision {
    StructuredDocumentRevision {
        revision_id: data.revision_id,
        document_id: data.document_id,
        workspace_id: data.workspace_id,
        library_id: data.library_id,
        preparation_state: data.preparation_state.clone(),
        normalization_profile: data.normalization_profile.clone(),
        source_format: data.source_format.clone(),
        language_code: data.language_code.clone(),
        block_count: data.block_count,
        chunk_count: data.chunk_count,
        typed_fact_count: data.typed_fact_count,
        outline: data.outline.clone(),
        prepared_at: data.prepared_at,
    }
}

pub(super) fn map_knowledge_chunk_row(row: KnowledgeChunkRow) -> ContentChunk {
    let start_offset = row.span_start.unwrap_or(0);
    let end_offset = row.span_end.unwrap_or_else(|| {
        start_offset.saturating_add(i32::try_from(row.normalized_text.len()).unwrap_or(0))
    });
    let checksum =
        format!("sha256:{}", hex::encode(Sha256::digest(row.normalized_text.as_bytes())));
    ContentChunk {
        id: row.chunk_id,
        revision_id: row.revision_id,
        chunk_index: row.chunk_index,
        start_offset,
        end_offset,
        token_count: row.token_count,
        normalized_text: row.normalized_text,
        text_checksum: checksum,
        // KnowledgeChunkRow (Arango) does not yet carry temporal bounds.
        // Sprint T1.3 mirrors `occurred_at` / `occurred_until` into Arango;
        // until then this fallback path returns None and consumers fall back
        // to the Postgres source-of-truth via list_chunks_by_revision.
        occurred_at: None,
        occurred_until: None,
    }
}

pub(super) fn map_mutation_row(row: content_repository::ContentMutationRow) -> ContentMutation {
    ContentMutation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        operation_kind: row.operation_kind,
        mutation_state: row.mutation_state,
        requested_at: row.requested_at,
        completed_at: row.completed_at,
        requested_by_principal_id: row.requested_by_principal_id,
        request_surface: row.request_surface,
        idempotency_key: row.idempotency_key,
        source_identity: row.source_identity,
        failure_code: row.failure_code,
        conflict_code: row.conflict_code,
    }
}

pub(super) fn map_document_pipeline_job(handle: IngestJobHandle) -> ContentDocumentPipelineJob {
    let latest_attempt = handle.latest_attempt;
    ContentDocumentPipelineJob {
        id: handle.job.id,
        workspace_id: handle.job.workspace_id,
        library_id: handle.job.library_id,
        mutation_id: handle.job.mutation_id,
        async_operation_id: handle.job.async_operation_id,
        job_kind: handle.job.job_kind,
        queue_state: handle.job.queue_state,
        queued_at: handle.job.queued_at,
        available_at: handle.job.available_at,
        completed_at: handle.job.completed_at,
        claimed_at: latest_attempt.as_ref().map(|attempt| attempt.started_at),
        last_activity_at: latest_attempt
            .as_ref()
            .and_then(|attempt| {
                attempt.heartbeat_at.or(attempt.finished_at).or(Some(attempt.started_at))
            })
            .or(handle.job.completed_at),
        current_stage: latest_attempt.as_ref().and_then(|attempt| attempt.current_stage.clone()),
        failure_code: latest_attempt.as_ref().and_then(|attempt| attempt.failure_code.clone()),
        retryable: latest_attempt.as_ref().is_some_and(|attempt| attempt.retryable),
    }
}

pub(super) fn map_mutation_item_row(
    row: content_repository::ContentMutationItemRow,
) -> ContentMutationItem {
    ContentMutationItem {
        id: row.id,
        mutation_id: row.mutation_id,
        document_id: row.document_id,
        base_revision_id: row.base_revision_id,
        result_revision_id: row.result_revision_id,
        item_state: row.item_state,
        message: row.message,
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn knowledge_revision_mapping_preserves_document_hint() {
        let now = Utc::now();
        let revision_id = Uuid::now_v7();
        let mapped = map_knowledge_revision_row(KnowledgeRevisionRow {
            key: revision_id.to_string(),
            arango_id: None,
            arango_rev: None,
            revision_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_number: 1,
            revision_state: "active".to_string(),
            revision_kind: "web_page".to_string(),
            storage_ref: Some("content/web.html".to_string()),
            source_uri: Some("https://example.com/source".to_string()),
            document_hint: Some("https://example.com/smoke/v2".to_string()),
            mime_type: "text/html".to_string(),
            checksum: "sha256:source".to_string(),
            title: Some("Smoke".to_string()),
            byte_size: 128,
            normalized_text: Some("body".to_string()),
            text_checksum: Some("sha256:text".to_string()),
            image_checksum: None,
            text_state: "text_readable".to_string(),
            vector_state: "vector_ready".to_string(),
            graph_state: "graph_ready".to_string(),
            text_readable_at: Some(now),
            vector_ready_at: Some(now),
            graph_ready_at: Some(now),
            superseded_by_revision_id: None,
            created_at: now,
        });

        assert_eq!(mapped.document_hint.as_deref(), Some("https://example.com/smoke/v2"));
    }
}
