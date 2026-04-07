use tracing::error;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::extract::{
        ExtractChunkResult, ExtractContent, ExtractEdgeCandidate, ExtractNodeCandidate,
        ExtractResumeCursor,
    },
    domains::runtime_graph::RuntimeNodeType,
    infra::repositories::extract_repository,
    interfaces::http::router_support::ApiError,
    services::graph_identity,
};

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
    pub from_display_label: String,
    pub from_canonical_key: String,
    pub to_display_label: String,
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
            extract_state: map_text_state_to_extract_state(&revision.text_state).to_string(),
            normalized_text: revision.normalized_text,
            text_checksum: revision.text_checksum,
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

        let prepared_nodes = prepare_materialized_node_candidates(&command.node_candidates);
        let prepared_edges =
            prepare_materialized_edge_candidates(&prepared_nodes, &command.edge_candidates);
        let node_candidates = prepared_nodes
            .iter()
            .map(|candidate| extract_repository::NewExtractNodeCandidate {
                canonical_key: &candidate.canonical_key,
                node_kind: &candidate.node_kind,
                display_label: &candidate.display_label,
                summary: candidate.summary.as_deref(),
            })
            .collect::<Vec<_>>();
        let edge_candidates = prepared_edges
            .iter()
            .map(|candidate| extract_repository::NewExtractEdgeCandidate {
                canonical_key: &candidate.canonical_key,
                edge_kind: &candidate.edge_kind,
                from_canonical_key: &candidate.from_canonical_key,
                to_canonical_key: &candidate.to_canonical_key,
                summary: candidate.summary.as_deref(),
            })
            .collect::<Vec<_>>();
        extract_repository::replace_extract_node_candidates(
            &state.persistence.postgres,
            chunk_result.id,
            &node_candidates,
        )
        .await
        .map_err(|error| {
            error!(
                chunk_result_id = %chunk_result.id,
                chunk_id = %command.chunk_id,
                ?error,
                "failed to replace canonical extract node candidates"
            );
            ApiError::Internal
        })?;
        extract_repository::replace_extract_edge_candidates(
            &state.persistence.postgres,
            chunk_result.id,
            &edge_candidates,
        )
        .await
        .map_err(|error| {
            error!(
                chunk_result_id = %chunk_result.id,
                chunk_id = %command.chunk_id,
                ?error,
                "failed to replace canonical extract edge candidates"
            );
            ApiError::Internal
        })?;

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
        extract_repository::get_extract_chunk_result_by_id(
            &state.persistence.postgres,
            chunk_result_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("extract_chunk_result", chunk_result_id))?;
        let rows = extract_repository::list_extract_node_candidates_by_chunk_result(
            &state.persistence.postgres,
            chunk_result_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let mut mapped = rows
            .into_iter()
            .map(|row| ExtractNodeCandidate {
                id: row.id,
                chunk_result_id: row.chunk_result_id,
                canonical_key: row.canonical_key,
                node_kind: row.node_kind,
                display_label: row.display_label,
                summary: row.summary,
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
        extract_repository::get_extract_chunk_result_by_id(
            &state.persistence.postgres,
            chunk_result_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("extract_chunk_result", chunk_result_id))?;
        let rows = extract_repository::list_extract_edge_candidates_by_chunk_result(
            &state.persistence.postgres,
            chunk_result_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let mut mapped = rows
            .into_iter()
            .map(|row| ExtractEdgeCandidate {
                id: row.id,
                chunk_result_id: row.chunk_result_id,
                canonical_key: row.canonical_key,
                edge_kind: row.edge_kind,
                from_canonical_key: row.from_canonical_key,
                to_canonical_key: row.to_canonical_key,
                summary: row.summary,
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedMaterializedNodeCandidate {
    canonical_key: String,
    node_kind: String,
    display_label: String,
    summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedMaterializedEdgeCandidate {
    canonical_key: String,
    edge_kind: String,
    from_display_label: String,
    from_canonical_key: String,
    to_display_label: String,
    to_canonical_key: String,
    summary: Option<String>,
}

fn prepare_materialized_node_candidates(
    node_candidates: &[NewNodeCandidate],
) -> Vec<PreparedMaterializedNodeCandidate> {
    let mut entity_key_index = graph_identity::GraphLabelNodeTypeIndex::new();
    for candidate in node_candidates {
        let display_label = candidate.display_label.trim();
        if display_label.is_empty() {
            continue;
        }
        entity_key_index.insert(display_label, parse_materialized_node_type(&candidate.node_kind));
    }
    node_candidates
        .iter()
        .filter_map(|candidate| {
            let display_label = candidate.display_label.trim();
            if display_label.is_empty() {
                return None;
            }
            let canonical_key = entity_key_index.canonical_node_key_for_label(display_label);
            let node_type = graph_identity::runtime_node_type_from_key(&canonical_key);
            Some(PreparedMaterializedNodeCandidate {
                canonical_key,
                node_kind: graph_identity::runtime_node_type_slug(&node_type).to_string(),
                display_label: display_label.to_string(),
                summary: candidate
                    .summary
                    .as_deref()
                    .map(str::trim)
                    .filter(|summary| !summary.is_empty())
                    .map(ToOwned::to_owned),
            })
        })
        .collect()
}

fn prepare_materialized_edge_candidates(
    node_candidates: &[PreparedMaterializedNodeCandidate],
    edge_candidates: &[NewEdgeCandidate],
) -> Vec<PreparedMaterializedEdgeCandidate> {
    let mut entity_key_index = graph_identity::GraphLabelNodeTypeIndex::new();
    for candidate in node_candidates {
        entity_key_index
            .insert(&candidate.display_label, parse_materialized_node_type(&candidate.node_kind));
    }

    edge_candidates
        .iter()
        .filter_map(|candidate| {
            let relation_type = candidate.edge_kind.trim().to_ascii_lowercase();
            if !graph_identity::is_canonical_relation_type(&relation_type) {
                return None;
            }
            let from_display_label = candidate.from_display_label.trim();
            let to_display_label = candidate.to_display_label.trim();
            if from_display_label.is_empty() || to_display_label.is_empty() {
                return None;
            }
            let from_canonical_key =
                entity_key_index.canonical_node_key_for_label(from_display_label);
            let to_canonical_key = entity_key_index.canonical_node_key_for_label(to_display_label);
            Some(PreparedMaterializedEdgeCandidate {
                canonical_key: graph_identity::canonical_edge_key(
                    &from_canonical_key,
                    &relation_type,
                    &to_canonical_key,
                ),
                edge_kind: relation_type,
                from_display_label: from_display_label.to_string(),
                from_canonical_key,
                to_display_label: to_display_label.to_string(),
                to_canonical_key,
                summary: candidate
                    .summary
                    .as_deref()
                    .map(str::trim)
                    .filter(|summary| !summary.is_empty())
                    .map(ToOwned::to_owned),
            })
        })
        .collect()
}

fn parse_materialized_node_type(node_kind: &str) -> RuntimeNodeType {
    match node_kind.trim().to_ascii_lowercase().as_str() {
        "document" => RuntimeNodeType::Document,
        "person" => RuntimeNodeType::Person,
        "organization" => RuntimeNodeType::Organization,
        "location" => RuntimeNodeType::Location,
        "event" => RuntimeNodeType::Event,
        "artifact" => RuntimeNodeType::Artifact,
        "natural" => RuntimeNodeType::Natural,
        "process" => RuntimeNodeType::Process,
        "concept" => RuntimeNodeType::Concept,
        "attribute" => RuntimeNodeType::Attribute,
        // Backward compatibility
        "topic" => RuntimeNodeType::Concept,
        "technology" => RuntimeNodeType::Artifact,
        "api" => RuntimeNodeType::Artifact,
        "code_symbol" => RuntimeNodeType::Artifact,
        "natural_kind" => RuntimeNodeType::Natural,
        "metric" => RuntimeNodeType::Attribute,
        "regulation" => RuntimeNodeType::Artifact,
        _ => RuntimeNodeType::Entity,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_materialized_node_candidates_recanonicalizes_unicode_labels() {
        let candidates = vec![NewNodeCandidate {
            canonical_key: String::new(),
            node_kind: "entity".to_string(),
            display_label: "Первый печатный двор".to_string(),
            summary: None,
        }];

        let prepared = prepare_materialized_node_candidates(&candidates);

        assert_eq!(prepared.len(), 1);
        assert_eq!(prepared[0].canonical_key, "entity:первый_печатный_двор");
        assert_eq!(prepared[0].node_kind, "entity");
        assert_eq!(prepared[0].display_label, "Первый печатный двор");
    }

    #[test]
    fn prepare_materialized_edge_candidates_uses_labels_to_build_canonical_keys() {
        let nodes = prepare_materialized_node_candidates(&[
            NewNodeCandidate {
                canonical_key: String::new(),
                node_kind: "entity".to_string(),
                display_label: "Первый печатный двор".to_string(),
                summary: None,
            },
            NewNodeCandidate {
                canonical_key: String::new(),
                node_kind: "topic".to_string(),
                display_label: "Касса".to_string(),
                summary: None,
            },
        ]);
        let edges = vec![
            NewEdgeCandidate {
                canonical_key: String::new(),
                edge_kind: "mentions".to_string(),
                from_display_label: "Первый печатный двор".to_string(),
                from_canonical_key: String::new(),
                to_display_label: "Касса".to_string(),
                to_canonical_key: String::new(),
                summary: None,
            },
            NewEdgeCandidate {
                canonical_key: String::new(),
                edge_kind: "   ".to_string(),
                from_display_label: "Первый печатный двор".to_string(),
                from_canonical_key: String::new(),
                to_display_label: "Касса".to_string(),
                to_canonical_key: String::new(),
                summary: None,
            },
        ];

        let prepared = prepare_materialized_edge_candidates(&nodes, &edges);

        assert_eq!(prepared.len(), 1);
        assert_eq!(
            prepared[0].canonical_key,
            "entity:первый_печатный_двор--mentions--concept:касса"
        );
        assert_eq!(prepared[0].edge_kind, "mentions");
        assert_eq!(prepared[0].from_canonical_key, "entity:первый_печатный_двор");
        assert_eq!(prepared[0].to_canonical_key, "concept:касса");
    }

    #[test]
    fn prepare_materialized_node_candidates_prefers_entity_for_ambiguous_labels() {
        let prepared = prepare_materialized_node_candidates(&[
            NewNodeCandidate {
                canonical_key: String::new(),
                node_kind: "topic".to_string(),
                display_label: "Касса".to_string(),
                summary: None,
            },
            NewNodeCandidate {
                canonical_key: String::new(),
                node_kind: "entity".to_string(),
                display_label: "Касса".to_string(),
                summary: None,
            },
        ]);

        assert_eq!(prepared.len(), 2);
        assert!(prepared.iter().all(|candidate| candidate.canonical_key == "entity:касса"));
        assert!(prepared.iter().all(|candidate| candidate.node_kind == "entity"));
    }
}
