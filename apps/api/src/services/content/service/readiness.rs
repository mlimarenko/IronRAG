use chrono::Utc;

use crate::{
    domains::ingest::IngestStageEvent,
    infra::knowledge_rows::KnowledgeRevisionRow,
    services::graph::{
        extract::{GRAPH_EXTRACTION_CONTRACT_VERSION, GRAPH_EXTRACTION_VERSION},
        rebuild::RevisionGraphReconcileOutcome,
    },
};

use super::{FailedRevisionReadiness, RevisionGraphCandidateMaterialization};

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

pub(crate) const fn graph_extract_success_message(graph_ready: bool) -> &'static str {
    if graph_ready {
        "graph candidates extracted and reconciled"
    } else {
        "graph extraction completed with no graph contributions"
    }
}

/// Builds the one successful `extract_graph` stage marker shared by the
/// queued worker and inline ingest paths. Recovery treats the extraction
/// version and contribution count as a document-level completion contract, so
/// the two execution paths must never drift.
pub(crate) fn graph_extract_success_stage_details(
    materialization: &RevisionGraphCandidateMaterialization,
    outcome: &RevisionGraphReconcileOutcome,
    graph_ready: bool,
) -> serde_json::Value {
    serde_json::json!({
        "extractionVersion": GRAPH_EXTRACTION_VERSION,
        "extractionContractVersion": GRAPH_EXTRACTION_CONTRACT_VERSION,
        "chunksProcessed": materialization.chunk_count,
        "graphChunksSelected": materialization.selected_graph_chunks,
        "recordStreamSourceUnitsSkipped": materialization.record_stream_source_units_skipped,
        "extractedEntityCandidates": materialization.extracted_entities,
        "extractedRelationCandidates": materialization.extracted_relations,
        "reusedChunks": materialization.reused_chunks,
        "reusedEntities": materialization.reused_entities,
        "reusedRelations": materialization.reused_relations,
        "projectedNodes": outcome.projection.node_count,
        "projectedEdges": outcome.projection.edge_count,
        "projectionVersion": outcome.projection.projection_version,
        "graphStatus": outcome.projection.graph_status,
        "graphContributionCount": outcome.graph_contribution_count,
        "graphReady": graph_ready,
        "providerKind": materialization.provider_kind,
        "modelName": materialization.model_name,
    })
}

pub(crate) const fn graph_state_after_successful_extract(graph_ready: bool) -> &'static str {
    if graph_ready { "ready" } else { GRAPH_STATE_DEGRADED }
}

/// Graph state for a revision whose chunk vectors are embedded and
/// searchable but whose graph layer is terminally not ready: extraction
/// failed after provider retries, or a successful extract/reconcile
/// contributed no graph targets. Graph is an enrichment layer over chunk-vector
/// retrieval, so the document stays answerable via vector + lexical
/// retrieval immediately; the bounded graph recovery scheduler repairs the
/// graph layer on a later tick once the revision is promoted to active.
/// A successful zero-contribution reconcile is terminal, so it must not
/// leave the revision "processing" (IRG-211). Treated as not-graph-ready by every
/// consumer (which match only "`ready"/"graph_ready`"), so introducing it is
/// purely additive and needs no schema migration.
pub(crate) const GRAPH_STATE_DEGRADED: &str = "graph_degraded";

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::{
        GRAPH_STATE_DEGRADED, derive_failed_revision_readiness, graph_extract_success_message,
        graph_extract_success_stage_details, graph_state_after_successful_extract,
    };
    use crate::{
        domains::ingest::IngestStageEvent,
        infra::knowledge_rows::KnowledgeRevisionRow,
        services::{
            content::service::RevisionGraphCandidateMaterialization,
            graph::{
                extract::{GRAPH_EXTRACTION_CONTRACT_VERSION, GRAPH_EXTRACTION_VERSION},
                projection::GraphProjectionOutcome,
                rebuild::RevisionGraphReconcileOutcome,
            },
        },
    };

    #[test]
    fn successful_graph_stage_details_stamp_current_extraction_version() {
        let materialization = RevisionGraphCandidateMaterialization {
            chunk_count: 1,
            selected_graph_chunks: 1,
            extracted_entities: 1,
            extracted_relations: 0,
            provider_kind: Some("test-provider".to_string()),
            model_name: Some("test-model".to_string()),
            usage_json: serde_json::json!({}),
            reused_chunks: 0,
            reused_entities: 0,
            reused_relations: 0,
            record_stream_source_units_skipped: 0,
        };
        let outcome = RevisionGraphReconcileOutcome {
            projection: GraphProjectionOutcome {
                projection_version: 1,
                node_count: 1,
                edge_count: 0,
                graph_status: "ready".to_string(),
            },
            graph_contribution_count: 1,
            graph_ready: true,
            pending_summary_refresh: None,
        };

        let details = graph_extract_success_stage_details(&materialization, &outcome, true);

        assert_eq!(
            details.get("extractionVersion"),
            Some(&serde_json::json!(GRAPH_EXTRACTION_VERSION))
        );
        assert_eq!(
            details.get("extractionContractVersion"),
            Some(&serde_json::json!(GRAPH_EXTRACTION_CONTRACT_VERSION))
        );
        assert!(details.get("extraction_version").is_none());
    }

    fn revision(text_state: &str, vector_state: &str, graph_state: &str) -> KnowledgeRevisionRow {
        let now = Utc::now();
        let revision_id = Uuid::now_v7();
        KnowledgeRevisionRow {
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
        assert_eq!(graph_state_after_successful_extract(false), GRAPH_STATE_DEGRADED);
        assert_ne!(graph_state_after_successful_extract(false), "processing");
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
