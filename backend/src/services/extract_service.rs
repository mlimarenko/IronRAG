use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::extract::{
        ExtractChunkResult, ExtractContent, ExtractEdgeCandidate, ExtractNodeCandidate,
        ExtractResumeCursor,
    },
    infra::repositories::extract_repository::{
        self, NewExtractEdgeCandidate, NewExtractNodeCandidate,
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
        let row = extract_repository::upsert_extract_content(
            &state.persistence.postgres,
            command.revision_id,
            command.attempt_id,
            &command.extract_state,
            command.normalized_text.as_deref(),
            command.text_checksum.as_deref(),
            command.warning_count,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_extract_content_row(row))
    }

    pub async fn get_extract_content(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<ExtractContent, ApiError> {
        let row = extract_repository::get_extract_content_by_revision_id(
            &state.persistence.postgres,
            revision_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("extract_content", revision_id))?;
        Ok(map_extract_content_row(row))
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

        let node_candidates = command
            .node_candidates
            .iter()
            .map(|candidate| NewExtractNodeCandidate {
                canonical_key: &candidate.canonical_key,
                node_kind: &candidate.node_kind,
                display_label: &candidate.display_label,
                summary: candidate.summary.as_deref(),
            })
            .collect::<Vec<_>>();
        let edge_candidates = command
            .edge_candidates
            .iter()
            .map(|candidate| NewExtractEdgeCandidate {
                canonical_key: &candidate.canonical_key,
                edge_kind: &candidate.edge_kind,
                from_canonical_key: &candidate.from_canonical_key,
                to_canonical_key: &candidate.to_canonical_key,
                summary: candidate.summary.as_deref(),
            })
            .collect::<Vec<_>>();

        let _ = extract_repository::replace_extract_node_candidates(
            &state.persistence.postgres,
            chunk_result.id,
            &node_candidates,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let _ = extract_repository::replace_extract_edge_candidates(
            &state.persistence.postgres,
            chunk_result.id,
            &edge_candidates,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

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
        let rows = extract_repository::list_extract_node_candidates_by_chunk_result(
            &state.persistence.postgres,
            chunk_result_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_extract_node_candidate_row).collect())
    }

    pub async fn list_edge_candidates(
        &self,
        state: &AppState,
        chunk_result_id: Uuid,
    ) -> Result<Vec<ExtractEdgeCandidate>, ApiError> {
        let rows = extract_repository::list_extract_edge_candidates_by_chunk_result(
            &state.persistence.postgres,
            chunk_result_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_extract_edge_candidate_row).collect())
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
}

fn map_extract_content_row(row: extract_repository::ExtractContentRow) -> ExtractContent {
    ExtractContent {
        revision_id: row.revision_id,
        attempt_id: row.attempt_id,
        extract_state: row.extract_state,
        normalized_text: row.normalized_text,
        text_checksum: row.text_checksum,
        warning_count: row.warning_count,
        updated_at: row.updated_at,
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

fn map_extract_node_candidate_row(
    row: extract_repository::ExtractNodeCandidateRow,
) -> ExtractNodeCandidate {
    ExtractNodeCandidate {
        id: row.id,
        chunk_result_id: row.chunk_result_id,
        canonical_key: row.canonical_key,
        node_kind: row.node_kind,
        display_label: row.display_label,
        summary: row.summary,
    }
}

fn map_extract_edge_candidate_row(
    row: extract_repository::ExtractEdgeCandidateRow,
) -> ExtractEdgeCandidate {
    ExtractEdgeCandidate {
        id: row.id,
        chunk_result_id: row.chunk_result_id,
        canonical_key: row.canonical_key,
        edge_kind: row.edge_kind,
        from_canonical_key: row.from_canonical_key,
        to_canonical_key: row.to_canonical_key,
        summary: row.summary,
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
