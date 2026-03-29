use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::extract::{
        ExtractChunkResult, ExtractContent, ExtractEdgeCandidate, ExtractNodeCandidate,
        ExtractResumeCursor,
    },
    infra::{
        arangodb::graph_store::{NewKnowledgeEntityCandidate, NewKnowledgeRelationCandidate},
        repositories::extract_repository,
    },
    interfaces::http::router_support::ApiError,
};

#[derive(Debug, Clone)]
pub struct PersistExtractContentCommand {
    pub revision_id: Uuid,
    pub attempt_id: Option<Uuid>,
    pub extract_state: String,
    pub normalized_text: Option<String>,
    pub text_checksum: Option<String>,
    pub warning_count: i32,
}

#[derive(Debug, Clone)]
pub struct MaterializeChunkResultCommand {
    pub chunk_id: Uuid,
    pub attempt_id: Uuid,
    pub extract_state: String,
    pub provider_call_id: Option<Uuid>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
    pub failure_code: Option<String>,
    pub node_candidates: Vec<NewNodeCandidate>,
    pub edge_candidates: Vec<NewEdgeCandidate>,
}

#[derive(Debug, Clone)]
pub struct NewNodeCandidate {
    pub canonical_key: String,
    pub node_kind: String,
    pub display_label: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewEdgeCandidate {
    pub canonical_key: String,
    pub edge_kind: String,
    pub from_canonical_key: String,
    pub to_canonical_key: String,
    pub summary: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CheckpointResumeCursorCommand {
    pub attempt_id: Uuid,
    pub last_completed_chunk_index: i32,
}

#[derive(Clone, Default)]
pub struct ExtractService;

impl ExtractService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn persist_extract_content(
        &self,
        state: &AppState,
        command: PersistExtractContentCommand,
    ) -> Result<ExtractContent, ApiError> {
        let updated_at = Utc::now();
        let text_readable_at = matches!(command.extract_state.as_str(), "readable" | "ready")
            .then_some(updated_at)
            .filter(|_| {
                command.normalized_text.as_deref().is_some_and(|text| !text.trim().is_empty())
            });
        let _ = state
            .canonical_services
            .knowledge
            .set_revision_text_state(
                state,
                command.revision_id,
                map_extract_state_to_text_state(&command.extract_state),
                command.normalized_text.as_deref(),
                command.text_checksum.as_deref(),
                text_readable_at,
            )
            .await?;
        // TODO: Remove extract_content persistence after legacy readers are retired.
        Ok(ExtractContent {
            revision_id: command.revision_id,
            attempt_id: command.attempt_id,
            extract_state: command.extract_state,
            normalized_text: command.normalized_text,
            text_checksum: command.text_checksum,
            warning_count: command.warning_count,
            updated_at,
        })
    }

    pub async fn get_extract_content(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<ExtractContent, ApiError> {
        let revision = state
            .arango_document_store
            .get_revision(revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", revision_id))?;
        Ok(ExtractContent {
            revision_id,
            attempt_id: None,
            extract_state: map_text_state_to_extract_state(&revision.text_state).to_string(),
            normalized_text: revision.normalized_text,
            text_checksum: revision.text_checksum,
            warning_count: 0,
            updated_at: revision.text_readable_at.unwrap_or(revision.created_at),
        })
    }

    pub async fn materialize_chunk_result(
        &self,
        state: &AppState,
        command: MaterializeChunkResultCommand,
    ) -> Result<ExtractChunkResult, ApiError> {
        let existing = extract_repository::get_extract_chunk_result_by_chunk_and_attempt(
            &state.persistence.postgres,
            command.chunk_id,
            command.attempt_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        let chunk_result = if let Some(existing) = existing {
            extract_repository::update_extract_chunk_result(
                &state.persistence.postgres,
                existing.id,
                &command.extract_state,
                command.provider_call_id,
                command.finished_at,
                command.failure_code.as_deref(),
            )
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("extract_chunk_result", existing.id))?
        } else {
            extract_repository::create_extract_chunk_result(
                &state.persistence.postgres,
                command.chunk_id,
                command.attempt_id,
                &command.extract_state,
                command.provider_call_id,
                None,
                command.finished_at,
                command.failure_code.as_deref(),
            )
            .await
            .map_err(|_| ApiError::Internal)?
        };

        self.persist_arango_extract_candidates(
            state,
            chunk_result.id,
            command.chunk_id,
            &command.node_candidates,
            &command.edge_candidates,
        )
        .await?;

        Ok(map_extract_chunk_result_row(chunk_result))
    }

    pub async fn list_chunk_results(
        &self,
        state: &AppState,
        attempt_id: Uuid,
    ) -> Result<Vec<ExtractChunkResult>, ApiError> {
        let rows = extract_repository::list_extract_chunk_results_by_attempt(
            &state.persistence.postgres,
            attempt_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_extract_chunk_result_row).collect())
    }

    pub async fn list_node_candidates(
        &self,
        state: &AppState,
        chunk_result_id: Uuid,
    ) -> Result<Vec<ExtractNodeCandidate>, ApiError> {
        let chunk_result = extract_repository::get_extract_chunk_result_by_id(
            &state.persistence.postgres,
            chunk_result_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("extract_chunk_result", chunk_result_id))?;
        let chunk = state
            .arango_document_store
            .get_chunk(chunk_result.chunk_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_chunk", chunk_result.chunk_id)
            })?;
        let rows = state
            .arango_graph_store
            .list_entity_candidates_by_revision(chunk.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let mut mapped = rows
            .into_iter()
            .filter(|row| row.chunk_id == Some(chunk.chunk_id))
            .map(|row| ExtractNodeCandidate {
                id: row.candidate_id,
                chunk_result_id,
                canonical_key: row.normalization_key,
                node_kind: row.candidate_type,
                display_label: row.candidate_label,
                summary: None,
            })
            .collect::<Vec<_>>();
        mapped.sort_by(|a, b| a.canonical_key.cmp(&b.canonical_key).then_with(|| a.id.cmp(&b.id)));
        Ok(mapped)
    }

    pub async fn list_edge_candidates(
        &self,
        state: &AppState,
        chunk_result_id: Uuid,
    ) -> Result<Vec<ExtractEdgeCandidate>, ApiError> {
        let chunk_result = extract_repository::get_extract_chunk_result_by_id(
            &state.persistence.postgres,
            chunk_result_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("extract_chunk_result", chunk_result_id))?;
        let chunk = state
            .arango_document_store
            .get_chunk(chunk_result.chunk_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| {
                ApiError::resource_not_found("knowledge_chunk", chunk_result.chunk_id)
            })?;
        let rows = state
            .arango_graph_store
            .list_relation_candidates_by_revision(chunk.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?;
        let mut mapped = rows
            .into_iter()
            .filter(|row| row.chunk_id == Some(chunk.chunk_id))
            .map(|row| ExtractEdgeCandidate {
                id: row.candidate_id,
                chunk_result_id,
                canonical_key: row.normalized_assertion,
                edge_kind: row.predicate,
                from_canonical_key: row.subject_candidate_key,
                to_canonical_key: row.object_candidate_key,
                summary: None,
            })
            .collect::<Vec<_>>();
        mapped.sort_by(|a, b| a.canonical_key.cmp(&b.canonical_key).then_with(|| a.id.cmp(&b.id)));
        Ok(mapped)
    }

    pub async fn get_resume_cursor(
        &self,
        state: &AppState,
        attempt_id: Uuid,
    ) -> Result<Option<ExtractResumeCursor>, ApiError> {
        let row = extract_repository::get_extract_resume_cursor_by_attempt_id(
            &state.persistence.postgres,
            attempt_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(row.map(map_resume_cursor_row))
    }

    pub async fn checkpoint_resume_cursor(
        &self,
        state: &AppState,
        command: CheckpointResumeCursorCommand,
    ) -> Result<ExtractResumeCursor, ApiError> {
        let row = extract_repository::checkpoint_extract_resume_cursor(
            &state.persistence.postgres,
            command.attempt_id,
            command.last_completed_chunk_index,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_resume_cursor_row(row))
    }

    pub async fn increment_replay_count(
        &self,
        state: &AppState,
        attempt_id: Uuid,
    ) -> Result<ExtractResumeCursor, ApiError> {
        let row = extract_repository::increment_extract_resume_replay_count(
            &state.persistence.postgres,
            attempt_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_resume_cursor_row(row))
    }

    pub async fn increment_downgrade_level(
        &self,
        state: &AppState,
        attempt_id: Uuid,
    ) -> Result<ExtractResumeCursor, ApiError> {
        let row = extract_repository::increment_extract_resume_downgrade_level(
            &state.persistence.postgres,
            attempt_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_resume_cursor_row(row))
    }

    async fn persist_arango_extract_candidates(
        &self,
        state: &AppState,
        chunk_result_id: Uuid,
        chunk_id: Uuid,
        node_candidates: &[NewNodeCandidate],
        edge_candidates: &[NewEdgeCandidate],
    ) -> Result<(), ApiError> {
        let chunk = state
            .arango_document_store
            .get_chunk(chunk_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_chunk", chunk_id))?;
        let knowledge_revision = state
            .arango_document_store
            .get_revision(chunk.revision_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("knowledge_revision", chunk.revision_id))?;

        let entity_rows = node_candidates
            .iter()
            .map(|candidate| NewKnowledgeEntityCandidate {
                candidate_id: stable_candidate_id(
                    "entity",
                    chunk_result_id,
                    &candidate.canonical_key,
                    &candidate.node_kind,
                ),
                workspace_id: knowledge_revision.workspace_id,
                library_id: knowledge_revision.library_id,
                revision_id: knowledge_revision.revision_id,
                chunk_id: Some(chunk_id),
                candidate_label: candidate.display_label.clone(),
                candidate_type: candidate.node_kind.clone(),
                normalization_key: candidate.canonical_key.clone(),
                confidence: None,
                extraction_method: "extract_chunk_result".to_string(),
                candidate_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .collect::<Vec<_>>();
        if !entity_rows.is_empty() {
            let _ = state
                .arango_graph_store
                .upsert_entity_candidates(&entity_rows)
                .await
                .map_err(|_| ApiError::Internal)?;
        }

        let relation_rows = edge_candidates
            .iter()
            .map(|candidate| NewKnowledgeRelationCandidate {
                candidate_id: stable_candidate_id(
                    "relation",
                    chunk_result_id,
                    &candidate.canonical_key,
                    &candidate.edge_kind,
                ),
                workspace_id: knowledge_revision.workspace_id,
                library_id: knowledge_revision.library_id,
                revision_id: knowledge_revision.revision_id,
                chunk_id: Some(chunk_id),
                subject_candidate_key: candidate.from_canonical_key.clone(),
                predicate: candidate.edge_kind.clone(),
                object_candidate_key: candidate.to_canonical_key.clone(),
                normalized_assertion: candidate.canonical_key.clone(),
                confidence: None,
                extraction_method: "extract_chunk_result".to_string(),
                candidate_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
            .collect::<Vec<_>>();
        if !relation_rows.is_empty() {
            let _ = state
                .arango_graph_store
                .upsert_relation_candidates(&relation_rows)
                .await
                .map_err(|_| ApiError::Internal)?;
        }

        Ok(())
    }
}

fn map_extract_state_to_text_state(extract_state: &str) -> &str {
    match extract_state {
        "readable" | "ready" => "text_readable",
        "failed" => "failed",
        "processing" => "extracting_text",
        _ => "accepted",
    }
}

fn map_text_state_to_extract_state(text_state: &str) -> &'static str {
    match text_state {
        "text_readable" => "ready",
        "failed" => "failed",
        "extracting_text" => "processing",
        _ => "accepted",
    }
}

fn map_extract_chunk_result_row(
    row: extract_repository::ExtractChunkResultRow,
) -> ExtractChunkResult {
    ExtractChunkResult {
        id: row.id,
        chunk_id: row.chunk_id,
        attempt_id: row.attempt_id,
        extract_state: row.extract_state,
        provider_call_id: row.provider_call_id,
        started_at: row.started_at,
        finished_at: row.finished_at,
        failure_code: row.failure_code,
    }
}

fn map_resume_cursor_row(row: extract_repository::ExtractResumeCursorRow) -> ExtractResumeCursor {
    ExtractResumeCursor {
        attempt_id: row.attempt_id,
        last_completed_chunk_index: row.last_completed_chunk_index,
        replay_count: row.replay_count,
        downgrade_level: row.downgrade_level,
        updated_at: row.updated_at,
    }
}

#[must_use]
fn stable_candidate_id(
    kind: &str,
    chunk_result_id: Uuid,
    canonical_key: &str,
    flavor: &str,
) -> Uuid {
    let digest = Sha256::digest(
        format!("extract:{kind}:{chunk_result_id}:{canonical_key}:{flavor}").as_bytes(),
    );
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}
