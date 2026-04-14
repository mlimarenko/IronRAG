use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::Context;
use chrono::Utc;
use futures::stream::{self, StreamExt, TryStreamExt};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, ChunkRow, DocumentRow, content_repository},
    services::{
        graph::extract::{
            GraphExtractionCandidateSet, extraction_lifecycle_from_record,
            extraction_recovery_summary_from_record,
        },
        graph::merge::{
            GraphMergeScope, merge_chunk_graph_candidates, reconcile_merge_support_counts,
        },
        graph::projection::{
            GraphProjectionOutcome, GraphProjectionScope, ensure_empty_graph_snapshot,
            next_projection_version, project_canonical_graph,
        },
        ingest::runtime::{
            RuntimeStageUsageSummary, embed_runtime_graph_edges, embed_runtime_graph_nodes,
            resolve_effective_provider_profile,
        },
    },
};

pub async fn rebuild_library_graph(
    state: &AppState,
    library_id: Uuid,
) -> anyhow::Result<GraphProjectionOutcome> {
    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while planning rebuild")?;
    let projection_version = next_projection_version(snapshot.as_ref());
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    let extractions = repositories::list_runtime_graph_extraction_records_by_library(
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

        let Some(document_row) =
            content_repository::get_document_by_id(&state.persistence.postgres, record.document_id)
                .await
                .with_context(|| format!("failed to load document {}", record.document_id))?
        else {
            continue;
        };
        if document_row.deleted_at.is_some() {
            continue;
        }
        let Some(document_head) =
            content_repository::get_document_head(&state.persistence.postgres, record.document_id)
                .await
                .with_context(|| format!("failed to load document head {}", record.document_id))?
        else {
            continue;
        };
        let extraction_lifecycle = extraction_lifecycle_from_record(&record);
        if extraction_lifecycle.revision_id.is_some()
            && extraction_lifecycle.revision_id != document_head.active_revision_id
        {
            continue;
        }
        let active_revision_id =
            extraction_lifecycle.revision_id.or(document_head.active_revision_id);
        let revision = match active_revision_id {
            Some(revision_id) => {
                content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
                    .await
                    .with_context(|| format!("failed to load revision {}", revision_id))?
            }
            None => None,
        };
        let Some(chunk_row) =
            content_repository::get_chunk_by_id(&state.persistence.postgres, record.chunk_id)
                .await
                .with_context(|| format!("failed to load chunk {}", record.chunk_id))?
        else {
            continue;
        };
        let document = DocumentRow {
            id: document_row.id,
            library_id,
            source_id: None,
            external_key: document_row.external_key.clone(),
            title: revision.as_ref().and_then(|value| value.title.clone()),
            mime_type: revision.as_ref().map(|value| value.mime_type.clone()),
            checksum: revision.as_ref().map(|value| value.checksum.clone()),
            active_revision_id: document_head.active_revision_id,
            document_state: document_row.document_state.clone(),
            mutation_kind: None,
            mutation_status: None,
            deleted_at: document_row.deleted_at,
            created_at: document_row.created_at,
            updated_at: document_head.head_updated_at,
        };
        let chunk = ChunkRow {
            id: chunk_row.id,
            document_id: document_row.id,
            library_id,
            ordinal: chunk_row.chunk_index,
            content: chunk_row.normalized_text.clone(),
            token_count: chunk_row.token_count,
            metadata_json: serde_json::json!({
                "revision_id": chunk_row.revision_id,
                "start_offset": chunk_row.start_offset,
                "end_offset": chunk_row.end_offset,
                "text_checksum": chunk_row.text_checksum,
            }),
            created_at: revision.as_ref().map(|value| value.created_at).unwrap_or_else(Utc::now),
        };
        let candidates = serde_json::from_value::<GraphExtractionCandidateSet>(
            record.normalized_output_json.clone(),
        )
        .unwrap_or_default();
        if candidates.entities.is_empty() && candidates.relations.is_empty() {
            continue;
        }

        let merge_scope = GraphMergeScope::new(library_id, projection_version)
            .with_lifecycle(active_revision_id, extraction_lifecycle.activated_by_attempt_id);
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

    let merged_nodes = repositories::list_admitted_runtime_graph_nodes_by_library(
        &state.persistence.postgres,
        library_id,
        projection_version,
    )
    .await
    .context("failed to load rebuilt graph nodes")?;
    let merged_edges = repositories::list_admitted_runtime_graph_edges_by_library(
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
        embed_runtime_graph_nodes(state, &provider_profile, &merged_nodes)
            .await
            .context("failed to embed rebuilt graph nodes")?;
        embed_runtime_graph_edges(state, &provider_profile, &merged_nodes, &merged_edges)
            .await
            .context("failed to embed rebuilt graph edges")?;
    }

    let projection_scope = GraphProjectionScope::new(library_id, projection_version);
    run_rebuild_projection(state, &projection_scope, "failed to project rebuilt graph").await
}

#[derive(Debug, Clone)]
pub struct RevisionGraphReconcileOutcome {
    pub projection: GraphProjectionOutcome,
    pub graph_contribution_count: usize,
    pub graph_ready: bool,
    pub embedding_usage: Option<RuntimeStageUsageSummary>,
}

pub async fn reconcile_revision_graph(
    state: &AppState,
    library_id: Uuid,
    document_id: Uuid,
    revision_id: Uuid,
    activated_by_attempt_id: Option<Uuid>,
) -> anyhow::Result<RevisionGraphReconcileOutcome> {
    let document_row =
        content_repository::get_document_by_id(&state.persistence.postgres, document_id)
            .await
            .with_context(|| format!("failed to load content document {document_id}"))?
            .with_context(|| format!("content document {document_id} not found"))?;
    let document_head =
        content_repository::get_document_head(&state.persistence.postgres, document_id)
            .await
            .with_context(|| format!("failed to load content document head {document_id}"))?
            .with_context(|| format!("content document head {document_id} not found"))?;
    let revision = content_repository::get_revision_by_id(&state.persistence.postgres, revision_id)
        .await
        .with_context(|| format!("failed to load content revision {revision_id}"))?
        .with_context(|| format!("content revision {revision_id} not found"))?;
    let revision_chunks =
        content_repository::list_chunks_by_revision(&state.persistence.postgres, revision_id)
            .await
            .with_context(|| format!("failed to list chunks for content revision {revision_id}"))?;

    let document = synthesize_document_row(&document_row, &document_head, Some(&revision));
    let revision_chunk_ids = revision_chunks.iter().map(|chunk| chunk.id).collect::<BTreeSet<_>>();
    let chunk_rows_by_id = revision_chunks
        .iter()
        .map(|chunk| {
            (chunk.id, synthesize_chunk_row(chunk, document_id, library_id, revision.created_at))
        })
        .collect::<BTreeMap<_, _>>();

    let previous_active_revision_id = document_head
        .active_revision_id
        .filter(|active_revision_id| *active_revision_id != revision_id);
    if let Some(previous_active_revision_id) = previous_active_revision_id {
        repositories::delete_query_execution_references_by_content_revision(
            &state.persistence.postgres,
            library_id,
            document_id,
            previous_active_revision_id,
        )
        .await
        .with_context(|| {
            format!(
                "failed to delete stale query references for document {document_id} revision {previous_active_revision_id}"
            )
        })?;
        repositories::deactivate_runtime_graph_evidence_by_content_revision(
            &state.persistence.postgres,
            library_id,
            document_id,
            previous_active_revision_id,
            None,
        )
        .await
        .with_context(|| {
            format!(
                "failed to deactivate stale graph evidence for document {document_id} revision {previous_active_revision_id}"
            )
        })?;
    }

    let snapshot =
        repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
            .await
            .context("failed to load graph snapshot while reconciling revision graph")?;
    let mut projection_scope =
        crate::services::graph::projection::resolve_projection_scope(state, library_id)
            .await
            .context("failed to resolve active projection scope for revision graph reconcile")?;
    let existing_graph_is_empty =
        snapshot.as_ref().is_none_or(|value| value.node_count <= 0 && value.edge_count <= 0);

    let extraction_records = repositories::list_runtime_graph_extraction_records_by_document(
        &state.persistence.postgres,
        document_id,
    )
    .await
    .with_context(|| {
        format!("failed to list graph extraction records for document {document_id}")
    })?;
    let mut latest_records_by_chunk =
        BTreeMap::<Uuid, repositories::RuntimeGraphExtractionRecordRow>::new();
    for record in extraction_records {
        if record.status != "ready" || !revision_chunk_ids.contains(&record.chunk_id) {
            continue;
        }
        let extraction_lifecycle = extraction_lifecycle_from_record(&record);
        if extraction_lifecycle.revision_id.is_some()
            && extraction_lifecycle.revision_id != Some(revision_id)
        {
            continue;
        }
        latest_records_by_chunk.insert(record.chunk_id, record);
    }

    let merge_scope = GraphMergeScope::new(library_id, projection_scope.projection_version)
        .with_lifecycle(Some(revision_id), activated_by_attempt_id);

    // Parallelize the per-chunk merge fan-out so the document's graph
    // contribution does not stall on a sequential N×(SELECT+UPSERT+INSERT)
    // walk. Empirically this loop was the dominant CPU starvation on prod
    // workers at 0.2.2: each chunk burns ~7 sequential DB round-trips per
    // entity + ~6 per relation, all on the async runtime, blocking the
    // tokio worker threads from making progress on heartbeat / cancel poll
    // / dispatcher claims. By driving the chunk fan-out through
    // `buffer_unordered(MERGE_PARALLELISM)` we let independent chunks
    // overlap their DB waits and free the runtime to service other tasks.
    //
    // Each per-chunk future captures only what it needs through `Arc`-ed
    // shared state to keep capture cost down (the postgres pool clones
    // cheaply, but `DocumentRow` and the GraphQualityGuardService get one
    // explicit `Arc` apiece). We also consume `latest_records_by_chunk` by
    // value via `into_values()` and `mem::take` the heavy
    // `normalized_output_json` `serde_json::Value` straight into the
    // deserializer — eliminating the per-chunk deep clone that previously
    // dominated allocator pressure on documents with many chunks.
    const MERGE_PARALLELISM: usize = 8;
    let pool = state.persistence.postgres.clone();
    let quality_guard = state.bulk_ingest_hardening_services.graph_quality_guard.clone();
    let document_arc = Arc::new(document.clone());
    let chunk_rows_by_id_arc = Arc::new(chunk_rows_by_id);
    let merge_scope = Arc::new(merge_scope);

    #[derive(Debug, Default)]
    struct ChunkMergeOutcome {
        contribution: usize,
        follow_up: bool,
        node_ids: Vec<Uuid>,
        edge_ids: Vec<Uuid>,
    }

    let merge_results = stream::iter(latest_records_by_chunk.into_values().map(|record| {
        let pool = pool.clone();
        let quality_guard = quality_guard.clone();
        let document = Arc::clone(&document_arc);
        let chunk_rows_by_id = Arc::clone(&chunk_rows_by_id_arc);
        let merge_scope = Arc::clone(&merge_scope);
        async move {
            let chunk_id = record.chunk_id;
            let Some(chunk_row) = chunk_rows_by_id.get(&chunk_id).cloned() else {
                return Ok::<ChunkMergeOutcome, anyhow::Error>(ChunkMergeOutcome::default());
            };
            let mut record = record;
            let normalized = std::mem::take(&mut record.normalized_output_json);
            let recovery = extraction_recovery_summary_from_record(&record);
            let candidates =
                serde_json::from_value::<GraphExtractionCandidateSet>(normalized)
                    .unwrap_or_default();
            if candidates.entities.is_empty() && candidates.relations.is_empty() {
                return Ok(ChunkMergeOutcome::default());
            }
            let merge_outcome = merge_chunk_graph_candidates(
                &pool,
                &quality_guard,
                &merge_scope,
                document.as_ref(),
                &chunk_row,
                &candidates,
                recovery.as_ref(),
            )
            .await
            .with_context(|| {
                format!(
                    "failed to merge graph candidates for document {} chunk {}",
                    document.id, chunk_id
                )
            })?;
            Ok(ChunkMergeOutcome {
                contribution: merge_outcome.nodes.len() + merge_outcome.edges.len(),
                follow_up: merge_outcome.has_projection_follow_up(),
                node_ids: merge_outcome.summary_refresh_node_ids().into_iter().collect(),
                edge_ids: merge_outcome.summary_refresh_edge_ids().into_iter().collect(),
            })
        }
    }))
    .buffer_unordered(MERGE_PARALLELISM)
    .try_collect::<Vec<_>>()
    .await?;

    let mut graph_contribution_count = 0usize;
    let mut merge_follow_up_required = false;
    let mut changed_node_ids = BTreeSet::new();
    let mut changed_edge_ids = BTreeSet::new();
    for outcome in merge_results {
        graph_contribution_count = graph_contribution_count.saturating_add(outcome.contribution);
        merge_follow_up_required |= outcome.follow_up;
        changed_node_ids.extend(outcome.node_ids);
        changed_edge_ids.extend(outcome.edge_ids);
    }

    reconcile_merge_support_counts(
        &state.persistence.postgres,
        merge_scope.as_ref(),
        &changed_node_ids.iter().copied().collect::<Vec<_>>(),
        &changed_edge_ids.iter().copied().collect::<Vec<_>>(),
    )
    .await
    .context("failed to reconcile graph support counts during revision graph reconcile")?;

    let changed_edge_ids = changed_edge_ids.into_iter().collect::<Vec<_>>();
    let changed_node_ids = changed_node_ids.into_iter().collect::<Vec<_>>();
    let changed_edge_rows = repositories::list_admitted_runtime_graph_edges_by_ids(
        &state.persistence.postgres,
        library_id,
        projection_scope.projection_version,
        &changed_edge_ids,
    )
    .await
    .context("failed to load changed graph edges during revision graph reconcile")?;
    let changed_node_rows = repositories::list_admitted_runtime_graph_nodes_by_ids(
        &state.persistence.postgres,
        library_id,
        projection_scope.projection_version,
        &changed_node_ids,
    )
    .await
    .context("failed to load changed graph nodes during revision graph reconcile")?;

    let mut embedding_usage: Option<RuntimeStageUsageSummary> = None;
    if merge_follow_up_required {
        let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
        let supporting_node_rows = if changed_edge_rows.is_empty() {
            Vec::new()
        } else {
            let supporting_node_ids =
                collect_supporting_node_ids(&changed_node_ids, &changed_edge_rows);
            repositories::list_admitted_runtime_graph_nodes_by_ids(
                &state.persistence.postgres,
                library_id,
                projection_scope.projection_version,
                &supporting_node_ids,
            )
            .await
            .context("failed to load supporting graph nodes during revision graph reconcile")?
        };
        if !changed_node_rows.is_empty() {
            let node_usage =
                embed_runtime_graph_nodes(state, &provider_profile, &changed_node_rows)
                    .await
                    .context(
                        "failed to embed changed graph nodes during revision graph reconcile",
                    )?;
            embedding_usage = Some(node_usage);
        }
        if !changed_edge_rows.is_empty() {
            let edge_usage = embed_runtime_graph_edges(
                state,
                &provider_profile,
                &supporting_node_rows,
                &changed_edge_rows,
            )
            .await
            .context("failed to embed changed graph edges during revision graph reconcile")?;
            if let Some(ref mut existing) = embedding_usage {
                existing.merge(&edge_usage);
            } else {
                embedding_usage = Some(edge_usage);
            }
        }
    }

    let source_truth_version =
        crate::services::query::support::invalidate_library_source_truth(state, library_id)
            .await
            .context("failed to advance source truth during revision graph reconcile")?;
    let summary_refresh = if previous_active_revision_id.is_some()
        || (changed_node_ids.is_empty() && changed_edge_ids.is_empty())
    {
        crate::services::graph::summary::GraphSummaryRefreshRequest::broad()
    } else {
        crate::services::graph::summary::GraphSummaryRefreshRequest::targeted(
            changed_node_ids.clone(),
            changed_edge_ids.clone(),
        )
    }
    .with_source_truth_version(source_truth_version);
    projection_scope = projection_scope.with_summary_refresh(summary_refresh);
    if previous_active_revision_id.is_none()
        && !existing_graph_is_empty
        && (!changed_node_ids.is_empty() || !changed_edge_ids.is_empty())
    {
        projection_scope = projection_scope
            .with_targeted_refresh(changed_node_ids.clone(), changed_edge_ids.clone());
    }

    let projection = if graph_contribution_count > 0
        || previous_active_revision_id.is_some()
        || existing_graph_is_empty
    {
        project_canonical_graph(state, &projection_scope)
            .await
            .context("failed to project reconciled revision graph")?
    } else if let Some(snapshot) = snapshot {
        repositories::upsert_runtime_graph_snapshot(
            &state.persistence.postgres,
            library_id,
            "ready",
            projection_scope.projection_version,
            snapshot.node_count,
            snapshot.edge_count,
            Some(snapshot.provenance_coverage_percent.unwrap_or(100.0)),
            None,
        )
        .await
        .context("failed to preserve ready graph snapshot during no-op revision reconcile")?;
        GraphProjectionOutcome {
            projection_version: projection_scope.projection_version,
            node_count: usize::try_from(snapshot.node_count).unwrap_or_default(),
            edge_count: usize::try_from(snapshot.edge_count).unwrap_or_default(),
            graph_status: "ready".to_string(),
        }
    } else {
        ensure_empty_graph_snapshot(state, library_id, projection_scope.projection_version)
            .await
            .context("failed to persist empty graph snapshot during no-op revision reconcile")?
    };

    Ok(RevisionGraphReconcileOutcome {
        graph_ready: graph_contribution_count > 0 && projection.graph_status == "ready",
        graph_contribution_count,
        projection,
        embedding_usage,
    })
}

#[cfg(test)]
fn count_surviving_documents(records: &[repositories::RuntimeGraphExtractionRecordRow]) -> usize {
    records.iter().map(|record| record.document_id).collect::<BTreeSet<_>>().len()
}

async fn run_rebuild_projection(
    state: &AppState,
    scope: &GraphProjectionScope,
    failure_context: &str,
) -> anyhow::Result<GraphProjectionOutcome> {
    project_canonical_graph(state, scope).await.with_context(|| failure_context.to_string())
}

fn synthesize_document_row(
    document_row: &content_repository::ContentDocumentRow,
    document_head: &content_repository::ContentDocumentHeadRow,
    revision: Option<&content_repository::ContentRevisionRow>,
) -> DocumentRow {
    DocumentRow {
        id: document_row.id,
        library_id: document_row.library_id,
        source_id: None,
        external_key: document_row.external_key.clone(),
        title: revision.and_then(|value| value.title.clone()),
        mime_type: revision.map(|value| value.mime_type.clone()),
        checksum: revision.map(|value| value.checksum.clone()),
        active_revision_id: document_head.active_revision_id,
        document_state: document_row.document_state.clone(),
        mutation_kind: None,
        mutation_status: None,
        deleted_at: document_row.deleted_at,
        created_at: document_row.created_at,
        updated_at: document_head.head_updated_at,
    }
}

fn synthesize_chunk_row(
    chunk_row: &content_repository::ContentChunkRow,
    document_id: Uuid,
    library_id: Uuid,
    created_at: chrono::DateTime<Utc>,
) -> ChunkRow {
    ChunkRow {
        id: chunk_row.id,
        document_id,
        library_id,
        ordinal: chunk_row.chunk_index,
        content: chunk_row.normalized_text.clone(),
        token_count: chunk_row.token_count,
        metadata_json: serde_json::json!({
            "revision_id": chunk_row.revision_id,
            "start_offset": chunk_row.start_offset,
            "end_offset": chunk_row.end_offset,
            "text_checksum": chunk_row.text_checksum,
        }),
        created_at,
    }
}

fn collect_supporting_node_ids(
    changed_node_ids: &[Uuid],
    changed_edges: &[repositories::RuntimeGraphEdgeRow],
) -> Vec<Uuid> {
    let mut node_ids = changed_node_ids.iter().copied().collect::<BTreeSet<_>>();
    for edge in changed_edges {
        node_ids.insert(edge.from_node_id);
        node_ids.insert(edge.to_node_id);
    }
    node_ids.into_iter().collect()
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
                runtime_execution_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
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
                runtime_execution_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
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
                runtime_execution_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
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
