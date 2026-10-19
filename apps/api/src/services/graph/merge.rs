use std::collections::{BTreeSet, HashMap};

mod persistence;
#[cfg(test)]
mod tests;

use crate::{
    domains::graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
    infra::repositories::{self, ChunkRow, DocumentRow, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    services::{
        graph::error::GraphServiceError, graph::extract::GraphExtractionCandidateSet,
        graph::quality_guard::GraphQualityGuardService,
    },
};

use persistence::{
    EntityNodeUpsertInputs, build_entity_key_index, partition_relations,
    persist_filtered_relations_with_connection, preload_existing_nodes_for_merge_with_connection,
    upsert_admitted_relation_edges_with_connection, upsert_document_node_with_connection,
    upsert_entity_mention_edges_with_connection, upsert_entity_nodes_with_connection,
    upsert_relation_endpoint_nodes_with_connection,
};

/// How many per-entity (upsert node + upsert mentions-edge) pipelines we
/// allow to run in parallel while merging one chunk's extraction output.
///
/// 4 is well under the Postgres pool ceiling (worker pool is 40, and a
/// single job never monopolises more than its own slot), and round-trips
/// in the pipeline are ON CONFLICT upserts so racing tasks reconcile
/// through the unique index rather than deadlocking.
#[derive(Debug, Clone)]
pub(crate) struct GraphMergeScope {
    pub library_id: uuid::Uuid,
    pub projection_version: i64,
    pub revision_id: Option<uuid::Uuid>,
    pub activated_by_attempt_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GraphMergeOutcome {
    pub nodes: Vec<RuntimeGraphNodeRow>,
    pub edges: Vec<RuntimeGraphEdgeRow>,
}

impl GraphMergeOutcome {
    #[must_use]
    pub(crate) fn changed_node_ids(&self) -> Vec<uuid::Uuid> {
        self.nodes
            .iter()
            .map(|node| node.id)
            .chain(self.edges.iter().flat_map(|edge| [edge.from_node_id, edge.to_node_id]))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[must_use]
    pub(crate) fn changed_edge_ids(&self) -> Vec<uuid::Uuid> {
        let mut ids: Vec<uuid::Uuid> = self.edges.iter().map(|edge| edge.id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    #[must_use]
    pub(crate) fn summary_refresh_node_ids(&self) -> Vec<uuid::Uuid> {
        self.changed_node_ids()
    }

    #[must_use]
    pub(crate) fn summary_refresh_edge_ids(&self) -> Vec<uuid::Uuid> {
        self.changed_edge_ids()
    }
}

enum EdgePersistenceOutcome {
    Admitted(Box<RuntimeGraphEdgeRow>),
    Filtered,
}

impl GraphMergeScope {
    #[must_use]
    pub(crate) const fn new(library_id: uuid::Uuid, projection_version: i64) -> Self {
        Self { library_id, projection_version, revision_id: None, activated_by_attempt_id: None }
    }

    #[must_use]
    pub(crate) const fn with_lifecycle(
        mut self,
        revision_id: Option<uuid::Uuid>,
        activated_by_attempt_id: Option<uuid::Uuid>,
    ) -> Self {
        self.revision_id = revision_id;
        self.activated_by_attempt_id = activated_by_attempt_id;
        self
    }
}

#[must_use]
pub(crate) fn normalize_graph_aliases(label: &str, aliases: &[String]) -> Vec<String> {
    let mut values = BTreeSet::new();
    values.insert(label.trim().to_string());
    for alias in aliases {
        let trimmed = alias.trim();
        if !trimmed.is_empty() {
            values.insert(trimmed.to_string());
        }
    }
    values.into_iter().collect()
}

struct MergeAccumulator {
    nodes: Vec<RuntimeGraphNodeRow>,
    edges: Vec<RuntimeGraphEdgeRow>,
    evidence_targets: Vec<repositories::GraphEvidenceTarget>,
    evidence_count: usize,
    filtered_artifact_count: usize,
}

impl MergeAccumulator {
    fn new(document_node: RuntimeGraphNodeRow) -> Self {
        Self {
            evidence_targets: vec![repositories::GraphEvidenceTarget {
                target_kind: "node",
                target_id: document_node.id,
                evidence_context_key: "document_node",
            }],
            nodes: vec![document_node],
            edges: Vec::new(),
            evidence_count: 1,
            filtered_artifact_count: 0,
        }
    }

    fn record_entity_result(
        &mut self,
        node: RuntimeGraphNodeRow,
        document_edge: EdgePersistenceOutcome,
    ) {
        self.evidence_targets.push(repositories::GraphEvidenceTarget {
            target_kind: "node",
            target_id: node.id,
            evidence_context_key: "entity_node",
        });
        self.nodes.push(node);
        match document_edge {
            EdgePersistenceOutcome::Admitted(document_edge) => {
                self.evidence_targets.push(repositories::GraphEvidenceTarget {
                    target_kind: "edge",
                    target_id: document_edge.id,
                    evidence_context_key: "document_mentions_edge",
                });
                self.edges.push(*document_edge);
                self.evidence_count += 2;
            }
            EdgePersistenceOutcome::Filtered => {
                self.evidence_count += 1;
                self.filtered_artifact_count += 1;
            }
        }
    }

    fn record_relation_result(&mut self, result: RelationMergeResult) {
        let (source_node, target_node, edge) = match result {
            RelationMergeResult::Admitted { source_node, target_node, edge } => {
                (source_node, target_node, Some(edge))
            }
            RelationMergeResult::Filtered { source_node, target_node } => {
                self.filtered_artifact_count += 1;
                (source_node, target_node, None)
            }
        };
        self.evidence_targets.extend([
            repositories::GraphEvidenceTarget {
                target_kind: "node",
                target_id: source_node.id,
                evidence_context_key: "relation_source_node",
            },
            repositories::GraphEvidenceTarget {
                target_kind: "node",
                target_id: target_node.id,
                evidence_context_key: "relation_target_node",
            },
        ]);
        self.nodes.extend([*source_node, *target_node]);
        self.evidence_count += 2;
        if let Some(edge) = edge {
            self.evidence_targets.push(repositories::GraphEvidenceTarget {
                target_kind: "edge",
                target_id: edge.id,
                evidence_context_key: "relation_edge",
            });
            self.edges.push(*edge);
            self.evidence_count += 1;
        }
    }

    fn into_outcome(self) -> GraphMergeOutcome {
        GraphMergeOutcome { nodes: self.nodes, edges: self.edges }
    }
}

struct EntityMergeResult {
    nodes_by_key: HashMap<String, RuntimeGraphNodeRow>,
    identity_keys: Vec<String>,
}

type RelationCandidate = crate::services::graph::extract::GraphRelationCandidate;

struct FilteredRelation<'a> {
    relation: &'a RelationCandidate,
    source_key: String,
    target_key: String,
    reason_label: &'static str,
}

struct AdmittedRelation<'a> {
    relation: &'a RelationCandidate,
    source_key: String,
    target_key: String,
}

struct RelationPartition<'a> {
    filtered: Vec<FilteredRelation<'a>>,
    admitted: Vec<AdmittedRelation<'a>>,
}

enum RelationMergeResult {
    Admitted {
        source_node: Box<RuntimeGraphNodeRow>,
        target_node: Box<RuntimeGraphNodeRow>,
        edge: Box<RuntimeGraphEdgeRow>,
    },
    Filtered {
        source_node: Box<RuntimeGraphNodeRow>,
        target_node: Box<RuntimeGraphNodeRow>,
    },
}

/// Per-chunk inputs for one graph merge pass.
///
/// Bundles the document/chunk rows, candidate set and recovery summary so the
/// merge entry point stays within the argument budget; every field is read
/// exactly as before.
pub(super) struct MergeChunkInput<'a> {
    pub(super) document: &'a DocumentRow,
    pub(super) chunk: &'a ChunkRow,
    pub(super) candidates: &'a GraphExtractionCandidateSet,
    pub(super) extraction_recovery: Option<&'a ExtractionRecoverySummary>,
}

pub(super) async fn merge_chunk_graph_candidates_with_connection(
    connection: &mut sqlx::PgConnection,
    graph_quality_guard: &GraphQualityGuardService,
    scope: &GraphMergeScope,
    input: MergeChunkInput<'_>,
) -> std::result::Result<GraphMergeOutcome, GraphServiceError> {
    let MergeChunkInput { document, chunk, candidates, extraction_recovery } = input;
    let entity_key_index = build_entity_key_index(candidates);
    let preloaded_existing = preload_existing_nodes_for_merge_with_connection(
        connection,
        scope,
        document,
        candidates,
        &entity_key_index,
    )
    .await?;
    let document_node =
        upsert_document_node_with_connection(connection, scope, document, &preloaded_existing)
            .await?;
    let mut accumulator = MergeAccumulator::new(document_node.clone());
    let entity_merge = upsert_entity_nodes_with_connection(
        connection,
        scope,
        EntityNodeUpsertInputs {
            chunk,
            candidates,
            entity_key_index: &entity_key_index,
            preloaded_existing: &preloaded_existing,
            extraction_recovery,
        },
    )
    .await?;
    for (node, edge) in upsert_entity_mention_edges_with_connection(
        connection,
        scope,
        &document_node,
        &entity_merge,
        extraction_recovery,
    )
    .await?
    {
        accumulator.record_entity_result(node, edge);
    }
    let relations = partition_relations(graph_quality_guard, &entity_key_index, candidates);
    accumulator.filtered_artifact_count += persist_filtered_relations_with_connection(
        connection,
        graph_quality_guard,
        scope,
        document,
        chunk,
        &relations.filtered,
    )
    .await?;
    let all_nodes_by_key = upsert_relation_endpoint_nodes_with_connection(
        connection,
        scope,
        &relations.admitted,
        entity_merge.nodes_by_key,
        &preloaded_existing,
        extraction_recovery,
    )
    .await?;
    for result in upsert_admitted_relation_edges_with_connection(
        connection,
        scope,
        &relations.admitted,
        &all_nodes_by_key,
        extraction_recovery,
    )
    .await?
    {
        accumulator.record_relation_result(result);
    }
    repositories::bulk_create_runtime_graph_evidence_for_chunk_with_executor(
        &mut *connection,
        repositories::GraphEvidenceChunkContext {
            library_id: scope.library_id,
            document_id: Some(document.id),
            revision_id: scope.revision_id,
            activated_by_attempt_id: scope.activated_by_attempt_id,
            chunk_id: Some(chunk.id),
            source_file_name: Some(&document.external_key),
            evidence_text: &chunk.content,
            confidence_score: None,
            targets: &accumulator.evidence_targets,
        },
    )
    .await?;
    Ok(accumulator.into_outcome())
}

fn merge_graph_quality_metadata(
    existing: Option<&serde_json::Value>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
    summary_fragment: Option<&str>,
) -> serde_json::Value {
    let mut metadata = existing.and_then(serde_json::Value::as_object).cloned().unwrap_or_default();

    let existing_has_recovered =
        metadata.get("has_recovered_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let existing_has_partial =
        metadata.get("has_partial_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let existing_has_failed =
        metadata.get("has_failed_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let existing_second_pass =
        metadata.get("second_pass_applied").and_then(serde_json::Value::as_bool).unwrap_or(false);

    let current_status = extraction_recovery.map(|summary| summary.status.clone());
    let has_recovered = existing_has_recovered
        || matches!(
            current_status,
            Some(ExtractionOutcomeStatus::Recovered | ExtractionOutcomeStatus::Partial)
        );
    let has_partial =
        existing_has_partial || matches!(current_status, Some(ExtractionOutcomeStatus::Partial));
    let has_failed =
        existing_has_failed || matches!(current_status, Some(ExtractionOutcomeStatus::Failed));
    let second_pass_applied = existing_second_pass
        || extraction_recovery.is_some_and(|summary| summary.second_pass_applied);

    let recovery_status = if has_failed {
        "failed"
    } else if has_partial {
        "partial"
    } else if has_recovered {
        "recovered"
    } else {
        "clean"
    };
    metadata.insert("has_recovered_support".to_string(), serde_json::Value::Bool(has_recovered));
    metadata.insert("has_partial_support".to_string(), serde_json::Value::Bool(has_partial));
    metadata.insert("has_failed_support".to_string(), serde_json::Value::Bool(has_failed));
    metadata
        .insert("second_pass_applied".to_string(), serde_json::Value::Bool(second_pass_applied));
    metadata.insert(
        "extraction_recovery_status".to_string(),
        serde_json::Value::String(recovery_status.to_string()),
    );
    metadata.insert(
        "summary_fragments".to_string(),
        serde_json::to_value(merge_summary_fragments(existing, summary_fragment))
            .unwrap_or_else(|_| serde_json::json!([])),
    );

    serde_json::Value::Object(metadata)
}

fn merge_summary_fragments(
    existing: Option<&serde_json::Value>,
    summary_fragment: Option<&str>,
) -> Vec<String> {
    let mut values = existing
        .and_then(|value| value.get("summary_fragments"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter_map(normalize_summary_fragment)
        .collect::<BTreeSet<_>>();

    if let Some(summary_fragment) = summary_fragment.and_then(normalize_summary_fragment) {
        values.insert(summary_fragment);
    }

    values.into_iter().take(6).collect()
}

fn normalize_summary_fragment(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() { None } else { Some(normalized) }
}
