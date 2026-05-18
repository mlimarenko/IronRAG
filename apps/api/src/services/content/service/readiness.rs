use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState, domains::ingest::IngestStageEvent,
    infra::arangodb::document_store::KnowledgeRevisionRow,
};

use super::FailedRevisionReadiness;

pub(crate) fn derive_failed_revision_readiness(
    revision: &KnowledgeRevisionRow,
    stage_events: &[IngestStageEvent],
) -> FailedRevisionReadiness {
    let now = Utc::now();
    let extract_completed = has_completed_stage(stage_events, "extract_content");

    let text_state = if revision.text_state == "text_readable" || extract_completed {
        "text_readable"
    } else {
        "failed"
    };
    let vector_state = if revision.vector_state == "ready" { "ready" } else { "failed" };
    let graph_state = if revision.graph_state == "ready" { "ready" } else { "failed" };

    FailedRevisionReadiness {
        text_state: text_state.to_string(),
        vector_state: vector_state.to_string(),
        graph_state: graph_state.to_string(),
        text_readable_at: (text_state == "text_readable")
            .then(|| revision.text_readable_at.unwrap_or(now)),
        vector_ready_at: (vector_state == "ready").then(|| revision.vector_ready_at.unwrap_or(now)),
        graph_ready_at: (graph_state == "ready").then(|| revision.graph_ready_at.unwrap_or(now)),
    }
}

fn has_completed_stage(stage_events: &[IngestStageEvent], stage_name: &str) -> bool {
    stage_events
        .iter()
        .any(|event| event.stage_name == stage_name && event.stage_state == "completed")
}

pub(crate) fn graph_extract_success_message(graph_ready: bool) -> &'static str {
    if graph_ready {
        "graph candidates extracted and reconciled"
    } else {
        "graph extraction completed with no graph contributions"
    }
}

pub(crate) fn graph_state_after_successful_extract(graph_ready: bool) -> &'static str {
    if graph_ready { "ready" } else { "processing" }
}

pub(crate) async fn fail_revision_vector_graph_readiness(
    state: &AppState,
    revision_id: Uuid,
    reason: &str,
) -> anyhow::Result<u64> {
    let revision = state
        .arango_document_store
        .get_revision(revision_id)
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to load failed revision {revision_id}: {error:#}")
        })?
        .ok_or_else(|| anyhow::anyhow!("knowledge revision {revision_id} not found"))?;
    let now = Utc::now();
    let updated = state
        .arango_document_store
        .update_revision_readiness(
            revision_id,
            "text_readable",
            "failed",
            "failed",
            revision.text_readable_at.or(Some(now)),
            None,
            None,
            revision.superseded_by_revision_id,
        )
        .await
        .map_err(|error| {
            anyhow::anyhow!("failed to mark revision {revision_id} vector/graph failed: {error:#}")
        })?;
    if updated.is_none() {
        return Err(anyhow::anyhow!(
            "knowledge revision {revision_id} disappeared during failed readiness update"
        ));
    }

    let deleted =
        state.arango_search_store.delete_chunk_vectors_by_revision(revision_id).await.map_err(
            |error| {
                anyhow::anyhow!(
                    "failed to remove chunk vectors for failed revision {revision_id}: {error:#}"
                )
            },
        )?;
    if deleted > 0 {
        tracing::warn!(
            revision_id = %revision_id,
            deleted,
            reason,
            "removed chunk vectors after marking revision vector/graph failed",
        );
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        derive_failed_revision_readiness, graph_extract_success_message,
        graph_state_after_successful_extract,
    };
    use crate::{
        domains::ingest::IngestStageEvent, infra::arangodb::document_store::KnowledgeRevisionRow,
    };

    fn revision(text_state: &str, vector_state: &str, graph_state: &str) -> KnowledgeRevisionRow {
        let now = Utc::now();
        let revision_id = Uuid::now_v7();
        KnowledgeRevisionRow {
            key: revision_id.to_string(),
            arango_id: None,
            arango_rev: None,
            revision_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_number: 1,
            revision_state: "active".to_string(),
            revision_kind: "upload".to_string(),
            storage_ref: None,
            source_uri: None,
            document_hint: None,
            mime_type: "text/markdown".to_string(),
            checksum: "checksum".to_string(),
            title: Some("Sample document".to_string()),
            byte_size: 128,
            normalized_text: Some("sample content".to_string()),
            text_checksum: Some("text-checksum".to_string()),
            image_checksum: None,
            text_state: text_state.to_string(),
            vector_state: vector_state.to_string(),
            graph_state: graph_state.to_string(),
            text_readable_at: None,
            vector_ready_at: None,
            graph_ready_at: None,
            superseded_by_revision_id: None,
            created_at: now,
        }
    }

    fn stage_event(stage_name: &str, stage_state: &str) -> IngestStageEvent {
        IngestStageEvent {
            id: Uuid::now_v7(),
            attempt_id: Uuid::now_v7(),
            stage_name: stage_name.to_string(),
            stage_state: stage_state.to_string(),
            ordinal: 1,
            message: None,
            details_json: serde_json::json!({}),
            recorded_at: Utc::now(),
        }
    }

    #[test]
    fn failed_readiness_does_not_promote_vector_or_graph_from_stage_events() {
        let revision = revision("text_readable", "failed", "failed");
        let readiness = derive_failed_revision_readiness(
            &revision,
            &[stage_event("embed_chunk", "completed"), stage_event("extract_graph", "completed")],
        );

        assert_eq!(readiness.text_state, "text_readable");
        assert_eq!(readiness.vector_state, "failed");
        assert_eq!(readiness.graph_state, "failed");
        assert!(readiness.vector_ready_at.is_none());
        assert!(readiness.graph_ready_at.is_none());
    }

    #[test]
    fn failed_readiness_preserves_vector_and_graph_source_of_truth() {
        let revision = revision("text_readable", "ready", "ready");
        let readiness = derive_failed_revision_readiness(&revision, &[]);

        assert_eq!(readiness.vector_state, "ready");
        assert_eq!(readiness.graph_state, "ready");
        assert!(readiness.vector_ready_at.is_some());
        assert!(readiness.graph_ready_at.is_some());
    }

    #[test]
    fn successful_empty_graph_extract_is_not_failed_readiness() {
        assert_eq!(
            graph_extract_success_message(false),
            "graph extraction completed with no graph contributions"
        );
        assert_eq!(graph_state_after_successful_extract(false), "processing");
    }

    #[test]
    fn successful_contributory_graph_extract_is_graph_ready() {
        assert_eq!(
            graph_extract_success_message(true),
            "graph candidates extracted and reconciled"
        );
        assert_eq!(graph_state_after_successful_extract(true), "ready");
    }
}
