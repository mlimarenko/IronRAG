use std::collections::BTreeSet;

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories,
    services::{
        graph_extract::{
            GraphExtractionCandidateSet, extraction_lifecycle_from_record,
            extraction_recovery_summary_from_record,
        },
        graph_merge::{
            GraphMergeScope, merge_chunk_graph_candidates, reconcile_merge_support_counts,
        },
        graph_projection::{
            GraphProjectionOutcome, GraphProjectionScope, active_projection_version,
            ensure_empty_graph_snapshot, next_projection_version, project_canonical_graph,
        },
        runtime_ingestion::{
            embed_runtime_graph_edges, embed_runtime_graph_nodes,
            resolve_effective_provider_profile,
        },
    },
};

pub async fn rebuild_library_graph(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<GraphProjectionOutcome> {
    if let Some(targeted_scope) = load_high_confidence_targeted_scope(state, library_id).await? {
        let snapshot =
            repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
                .await
                .context("failed to load graph snapshot before targeted rebuild")?;
        let projection_scope =
            GraphProjectionScope::new(library_id, active_projection_version(snapshot.as_ref()))
                .with_targeted_refresh(
                    parse_scope_ids(&targeted_scope.affected_node_ids_json),
                    parse_scope_ids(&targeted_scope.affected_relationship_ids_json),
                );
        return run_rebuild_projection(
            state,
            &projection_scope,
            "failed to apply targeted graph reconciliation rebuild",
        )
        .await;
    }

    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while planning rebuild")?;
    let projection_version = next_projection_version(snapshot.as_ref());
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    let extractions = repositories::list_runtime_graph_extraction_records_by_project(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to reload runtime graph extraction records for rebuild")?;

    if extractions.is_empty() {
        return ensure_empty_graph_snapshot(state, library_id, projection_version).await;
    }

    let mut merged_any = false;
    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();

    for record in extractions {
        if record.status != "ready" {
            continue;
        }

        let Some(document) =
            repositories::get_document_by_id(&state.persistence.postgres, record.document_id)
                .await
                .with_context(|| format!("failed to load document {}", record.document_id))?
        else {
            continue;
        };
        if document.deleted_at.is_some() {
            continue;
        }
        let extraction_lifecycle = extraction_lifecycle_from_record(&record);
        if extraction_lifecycle.revision_id.is_some()
            && extraction_lifecycle.revision_id != document.current_revision_id
        {
            continue;
        }
        let Some(chunk) =
            repositories::get_chunk_by_id(&state.persistence.postgres, record.chunk_id)
                .await
                .with_context(|| format!("failed to load chunk {}", record.chunk_id))?
        else {
            continue;
        };
        let candidates = serde_json::from_value::<GraphExtractionCandidateSet>(
            record.normalized_output_json.clone(),
        )
        .unwrap_or_default();
        if candidates.entities.is_empty() && candidates.relations.is_empty() {
            continue;
        }

        let merge_scope = GraphMergeScope::new(library_id, projection_version).with_lifecycle(
            extraction_lifecycle.revision_id.or(document.current_revision_id),
            extraction_lifecycle.activated_by_attempt_id,
        );
        let merge_outcome = merge_chunk_graph_candidates(
            &state.persistence.postgres,
            &state.bulk_ingest_hardening_services.graph_quality_guard,
            &merge_scope,
            &document,
            &chunk,
            &candidates,
            extraction_recovery_summary_from_record(&record).as_ref(),
        )
        .await
        .with_context(|| {
            format!(
                "failed to rebuild graph knowledge for document {} chunk {}",
                document.id, chunk.id
            )
        })?;
        changed_node_ids.extend(merge_outcome.summary_refresh_node_ids());
        changed_edge_ids.extend(merge_outcome.summary_refresh_edge_ids());
        merged_any = true;
    }

    reconcile_merge_support_counts(
        &state.persistence.postgres,
        &GraphMergeScope::new(library_id, projection_version),
        &changed_node_ids.iter().copied().collect::<Vec<_>>(),
        &changed_edge_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await
    .context("failed to reconcile rebuilt graph support counts")?;

    let merged_nodes = repositories::list_admitted_runtime_graph_nodes_by_projection(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load rebuilt graph nodes")?;
    let merged_edges = repositories::list_admitted_runtime_graph_edges_by_projection(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load rebuilt graph edges")?;

    if merged_nodes.is_empty() && merged_edges.is_empty() {
        return ensure_empty_graph_snapshot(state, library_id, projection_version).await;
    }

    if merged_any {
        embed_runtime_graph_nodes(state, &provider_profile, &merged_nodes, None)
            .await
            .context("failed to embed rebuilt graph nodes")?;
        embed_runtime_graph_edges(state, &provider_profile, &merged_nodes, &merged_edges, None)
            .await
            .context("failed to embed rebuilt graph edges")?;
    }

    let projection_scope = GraphProjectionScope::new(library_id, projection_version);
    run_rebuild_projection(state, &projection_scope, "failed to project rebuilt graph").await
}

#[cfg(test)]
fn count_surviving_documents(records: &[repositories::RuntimeGraphExtractionRecordRow]) -> usize {
    records.iter().map(|record| record.document_id).collect::<BTreeSet<_>>().len()
}

async fn load_high_confidence_targeted_scope(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<Option<repositories::DocumentMutationImpactScopeRow>> {
    let scopes = repositories::list_active_document_mutation_impact_scopes_by_project(
        &state.persistence.postgres,
        library_id,
    )
    .await
    .context("failed to load active mutation impact scopes while planning rebuild")?;
    Ok(scopes.into_iter().find(|scope| {
        scope.scope_status == "targeted"
            && scope.confidence_status == "high"
            && (!parse_scope_ids(&scope.affected_node_ids_json).is_empty()
                || !parse_scope_ids(&scope.affected_relationship_ids_json).is_empty())
    }))
}

fn parse_scope_ids(value: &serde_json::Value) -> Vec<Uuid> {
    serde_json::from_value::<Vec<Uuid>>(value.clone()).unwrap_or_default()
}

async fn run_rebuild_projection(
    state: &AppState,
    scope: &GraphProjectionScope,
    failure_context: &str,
) -> anyhow::Result<GraphProjectionOutcome> {
    project_canonical_graph(state, scope).await.with_context(|| failure_context.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::repositories::RuntimeGraphExtractionRecordRow;

    #[test]
    fn counts_unique_documents_in_rebuild_plan() {
        let document_id = Uuid::now_v7();
        let other_document_id = Uuid::now_v7();

        let records = vec![
            RuntimeGraphExtractionRecordRow {
                id: Uuid::now_v7(),
                project_id: Uuid::now_v7(),
                document_id,
                chunk_id: Uuid::now_v7(),
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                extraction_version: "graph_extract_v1".to_string(),
                prompt_hash: "a".to_string(),
                status: "completed".to_string(),
                raw_output_json: serde_json::json!({}),
                normalized_output_json: serde_json::json!({}),
                glean_pass_count: 1,
                error_message: None,
                created_at: chrono::Utc::now(),
            },
            RuntimeGraphExtractionRecordRow {
                id: Uuid::now_v7(),
                project_id: Uuid::now_v7(),
                document_id,
                chunk_id: Uuid::now_v7(),
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                extraction_version: "graph_extract_v1".to_string(),
                prompt_hash: "b".to_string(),
                status: "completed".to_string(),
                raw_output_json: serde_json::json!({}),
                normalized_output_json: serde_json::json!({}),
                glean_pass_count: 1,
                error_message: None,
                created_at: chrono::Utc::now(),
            },
            RuntimeGraphExtractionRecordRow {
                id: Uuid::now_v7(),
                project_id: Uuid::now_v7(),
                document_id: other_document_id,
                chunk_id: Uuid::now_v7(),
                provider_kind: "openai".to_string(),
                model_name: "gpt-5.4-mini".to_string(),
                extraction_version: "graph_extract_v1".to_string(),
                prompt_hash: "c".to_string(),
                status: "completed".to_string(),
                raw_output_json: serde_json::json!({}),
                normalized_output_json: serde_json::json!({}),
                glean_pass_count: 1,
                error_message: None,
                created_at: chrono::Utc::now(),
            },
        ];

        assert_eq!(count_surviving_documents(&records), 2);
    }
}
