use std::collections::{BTreeSet, HashMap};

use anyhow::Result as AnyhowResult;
use futures::stream::{self, StreamExt, TryStreamExt};

use crate::{
    domains::graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
    infra::repositories::{self, ChunkRow, DocumentRow, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    services::{
        graph::error::GraphServiceError, graph::extract::GraphExtractionCandidateSet,
        graph::quality_guard::GraphQualityGuardService,
    },
};

/// How many per-entity (upsert node + upsert mentions-edge) pipelines we
/// allow to run in parallel while merging one chunk's extraction output.
///
/// 4 is well under the Postgres pool ceiling (worker pool is 40, and a
/// single job never monopolises more than its own slot), and round-trips
/// in the pipeline are ON CONFLICT upserts so racing tasks reconcile
/// through the unique index rather than deadlocking.
const ENTITY_UPSERT_CONCURRENCY: usize = 4;

#[derive(Debug, Clone)]
pub struct GraphMergeScope {
    pub library_id: uuid::Uuid,
    pub projection_version: i64,
    pub revision_id: Option<uuid::Uuid>,
    pub activated_by_attempt_id: Option<uuid::Uuid>,
}

#[derive(Debug, Clone, Default)]
pub struct GraphMergeOutcome {
    pub nodes: Vec<RuntimeGraphNodeRow>,
    pub edges: Vec<RuntimeGraphEdgeRow>,
    pub evidence_count: usize,
    pub filtered_artifact_count: usize,
}

pub(crate) async fn reconcile_merge_support_counts(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    changed_node_ids: &[uuid::Uuid],
    changed_edge_ids: &[uuid::Uuid],
) -> std::result::Result<(), GraphServiceError> {
    repositories::recalculate_runtime_graph_support_counts_by_ids_atomically(
        pool,
        scope.library_id,
        scope.projection_version,
        changed_node_ids,
        changed_edge_ids,
    )
    .await?;
    Ok(())
}

impl GraphMergeOutcome {
    #[must_use]
    pub const fn has_projection_follow_up(&self) -> bool {
        !self.nodes.is_empty() || !self.edges.is_empty() || self.evidence_count > 0
    }

    #[must_use]
    pub fn changed_node_ids(&self) -> Vec<uuid::Uuid> {
        self.nodes
            .iter()
            .map(|node| node.id)
            .chain(self.edges.iter().flat_map(|edge| [edge.from_node_id, edge.to_node_id]))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    #[must_use]
    pub fn changed_edge_ids(&self) -> Vec<uuid::Uuid> {
        let mut ids: Vec<uuid::Uuid> = self.edges.iter().map(|edge| edge.id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    #[must_use]
    pub fn summary_refresh_node_ids(&self) -> Vec<uuid::Uuid> {
        self.changed_node_ids()
    }

    #[must_use]
    pub fn summary_refresh_edge_ids(&self) -> Vec<uuid::Uuid> {
        self.changed_edge_ids()
    }
}

enum EdgePersistenceOutcome {
    Admitted(Box<RuntimeGraphEdgeRow>),
    Filtered,
}

impl GraphMergeScope {
    #[must_use]
    pub const fn new(library_id: uuid::Uuid, projection_version: i64) -> Self {
        Self { library_id, projection_version, revision_id: None, activated_by_attempt_id: None }
    }

    #[must_use]
    pub const fn with_lifecycle(
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
        GraphMergeOutcome {
            nodes: self.nodes,
            edges: self.edges,
            evidence_count: self.evidence_count,
            filtered_artifact_count: self.filtered_artifact_count,
        }
    }
}

struct EntityMergeResult {
    nodes_by_key: HashMap<String, RuntimeGraphNodeRow>,
    canonical_keys: Vec<String>,
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

pub(crate) async fn merge_chunk_graph_candidates(
    pool: &sqlx::PgPool,
    graph_quality_guard: &GraphQualityGuardService,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    chunk: &ChunkRow,
    candidates: &GraphExtractionCandidateSet,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> std::result::Result<GraphMergeOutcome, GraphServiceError> {
    let entity_key_index = build_entity_key_index(candidates);
    let preloaded_existing =
        preload_existing_nodes_for_merge(pool, scope, document, candidates, &entity_key_index)
            .await?;
    let document_node = upsert_document_node(pool, scope, document, &preloaded_existing).await?;
    let mut accumulator = MergeAccumulator::new(document_node.clone());

    let entity_merge = upsert_entity_nodes(
        pool,
        scope,
        chunk,
        candidates,
        &entity_key_index,
        &preloaded_existing,
        extraction_recovery,
    )
    .await?;
    let entity_results = upsert_entity_mention_edges(
        pool,
        scope,
        &document_node,
        &entity_merge,
        extraction_recovery,
    )
    .await?;
    for (node, edge) in entity_results {
        accumulator.record_entity_result(node, edge);
    }

    let relations = partition_relations(graph_quality_guard, &entity_key_index, candidates);
    accumulator.filtered_artifact_count += persist_filtered_relations(
        pool,
        graph_quality_guard,
        scope,
        document,
        chunk,
        &relations.filtered,
    )
    .await?;
    let all_nodes_by_key = upsert_relation_endpoint_nodes(
        pool,
        scope,
        &relations.admitted,
        entity_merge.nodes_by_key,
        &preloaded_existing,
        extraction_recovery,
    )
    .await?;
    for result in upsert_admitted_relation_edges(
        pool,
        scope,
        &relations.admitted,
        &all_nodes_by_key,
        extraction_recovery,
    )
    .await?
    {
        accumulator.record_relation_result(result);
    }

    repositories::bulk_create_runtime_graph_evidence_for_chunk(
        pool,
        scope.library_id,
        Some(document.id),
        scope.revision_id,
        scope.activated_by_attempt_id,
        Some(chunk.id),
        Some(&document.external_key),
        &chunk.content,
        None,
        &accumulator.evidence_targets,
    )
    .await?;
    Ok(accumulator.into_outcome())
}

async fn upsert_entity_nodes(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    chunk: &ChunkRow,
    candidates: &GraphExtractionCandidateSet,
    entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    preloaded_existing: &HashMap<String, RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> std::result::Result<EntityMergeResult, GraphServiceError> {
    let mut inputs = Vec::with_capacity(candidates.entities.len());
    let mut canonical_keys = Vec::with_capacity(candidates.entities.len());
    let mut seen_keys = BTreeSet::new();
    for entity in &candidates.entities {
        let canonical_key = entity_key_index.canonical_node_key_for_label(&entity.label);
        canonical_keys.push(canonical_key.clone());
        if !seen_keys.insert(canonical_key.clone()) {
            continue;
        }
        inputs.push(entity_node_input(
            entity,
            chunk,
            &canonical_key,
            preloaded_existing.get(&canonical_key),
            extraction_recovery,
        ));
    }
    let rows = if inputs.is_empty() {
        Vec::new()
    } else {
        repositories::bulk_upsert_runtime_graph_nodes(
            pool,
            scope.library_id,
            scope.projection_version,
            &inputs,
        )
        .await?
    };
    Ok(EntityMergeResult {
        nodes_by_key: rows.into_iter().map(|row| (row.canonical_key.clone(), row)).collect(),
        canonical_keys,
    })
}

fn entity_node_input(
    entity: &crate::services::graph::extract::GraphEntityCandidate,
    chunk: &ChunkRow,
    canonical_key: &str,
    existing: Option<&RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> repositories::RuntimeGraphNodeUpsertInput {
    let mut entity_aliases = entity.aliases.clone();
    entity_aliases.extend(
        crate::services::graph::extract::acronym_gloss::detect_acronym_aliases_for_label(
            &chunk.content,
            &entity.label,
        ),
    );
    let aliases = normalize_graph_aliases(&entity.label, &entity_aliases);
    let canonical_node_type =
        crate::services::graph::identity::runtime_node_type_from_key(canonical_key);
    let mut metadata = merge_graph_quality_metadata(
        existing.map(|row| &row.metadata_json),
        extraction_recovery,
        entity.summary.as_deref(),
    );
    if let Some(sub_type) = entity.sub_type.as_deref()
        && let Some(object) = metadata.as_object_mut()
    {
        object.insert("sub_type".to_string(), serde_json::Value::String(sub_type.to_string()));
    }
    repositories::RuntimeGraphNodeUpsertInput {
        canonical_key: canonical_key.to_string(),
        label: entity.label.trim().to_string(),
        node_type: crate::services::graph::identity::runtime_node_type_slug(&canonical_node_type)
            .to_string(),
        aliases_json: serde_json::to_value(aliases).unwrap_or_else(|_| serde_json::json!([])),
        summary: entity.summary.clone(),
        metadata_json: metadata,
        support_count: existing.map_or(1, |row| row.support_count.max(1)),
    }
}

async fn upsert_entity_mention_edges(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    document_node: &RuntimeGraphNodeRow,
    entity_merge: &EntityMergeResult,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> std::result::Result<Vec<(RuntimeGraphNodeRow, EdgePersistenceOutcome)>, GraphServiceError> {
    let edge_inputs = entity_merge
        .canonical_keys
        .iter()
        .filter_map(|key| entity_merge.nodes_by_key.get(key).cloned())
        .collect::<Vec<_>>();
    let extraction_recovery = extraction_recovery.cloned();
    stream::iter(edge_inputs)
        .map(|entity_node| {
            let pool = pool.clone();
            let scope = scope.clone();
            let document_node = document_node.clone();
            let extraction_recovery = extraction_recovery.clone();
            async move {
                let edge = upsert_graph_edge(
                    &pool,
                    &scope,
                    &document_node,
                    &entity_node,
                    "mentions",
                    Some("Document mentions extracted entity"),
                    extraction_recovery.as_ref(),
                )
                .await?;
                anyhow::Ok((entity_node, edge))
            }
        })
        .buffered(ENTITY_UPSERT_CONCURRENCY)
        .try_collect()
        .await
        .map_err(GraphServiceError::from)
}

fn partition_relations<'a>(
    graph_quality_guard: &GraphQualityGuardService,
    entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    candidates: &'a GraphExtractionCandidateSet,
) -> RelationPartition<'a> {
    let mut filtered = Vec::new();
    let mut admitted = Vec::new();
    for relation in &candidates.relations {
        let source_key = entity_key_index.canonical_node_key_for_label(&relation.source_label);
        let target_key = entity_key_index.canonical_node_key_for_label(&relation.target_label);
        if let Some(reason) =
            graph_quality_guard.filter_reason(&source_key, &target_key, &relation.relation_type)
        {
            filtered.push(FilteredRelation {
                relation,
                source_key,
                target_key,
                reason_label: artifact_filter_reason_label(reason),
            });
        } else {
            admitted.push(AdmittedRelation { relation, source_key, target_key });
        }
    }
    RelationPartition { filtered, admitted }
}

const fn artifact_filter_reason_label(
    reason: crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason,
) -> &'static str {
    match reason {
        crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::EmptyRelation => {
            "empty_relation"
        }
        crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::DegenerateSelfLoop => {
            "degenerate_self_loop"
        }
        crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::LowValueArtifact => {
            "low_value_artifact"
        }
    }
}

async fn persist_filtered_relations(
    pool: &sqlx::PgPool,
    graph_quality_guard: &GraphQualityGuardService,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    chunk: &ChunkRow,
    relations: &[FilteredRelation<'_>],
) -> std::result::Result<usize, GraphServiceError> {
    for filtered in relations {
        let relation = filtered.relation;
        repositories::create_runtime_graph_filtered_artifact(
            pool,
            scope.library_id,
            scope.activated_by_attempt_id,
            scope.revision_id,
            "edge",
            &crate::services::graph::identity::canonical_edge_key(
                &filtered.source_key,
                &relation.relation_type,
                &filtered.target_key,
            ),
            Some(filtered.source_key.as_str()),
            Some(filtered.target_key.as_str()),
            Some(&graph_quality_guard.normalized_relation_type(&relation.relation_type)),
            filtered.reason_label,
            relation.summary.as_deref(),
            serde_json::json!({
                "document_id": document.id,
                "chunk_id": chunk.id,
                "source_label": &relation.source_label,
                "target_label": &relation.target_label,
                "raw_relation_type": &relation.relation_type,
                "source_file_name": &document.external_key,
            }),
        )
        .await?;
    }
    Ok(relations.len())
}

async fn upsert_relation_endpoint_nodes(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    relations: &[AdmittedRelation<'_>],
    mut nodes_by_key: HashMap<String, RuntimeGraphNodeRow>,
    preloaded_existing: &HashMap<String, RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> std::result::Result<HashMap<String, RuntimeGraphNodeRow>, GraphServiceError> {
    let mut inputs = Vec::new();
    let mut seen_keys = BTreeSet::new();
    for admitted in relations {
        for (key, label) in [
            (admitted.source_key.as_str(), admitted.relation.source_label.as_str()),
            (admitted.target_key.as_str(), admitted.relation.target_label.as_str()),
        ] {
            if nodes_by_key.contains_key(key) || !seen_keys.insert(key.to_string()) {
                continue;
            }
            inputs.push(relation_endpoint_input(
                key,
                label,
                preloaded_existing.get(key),
                extraction_recovery,
            ));
        }
    }
    if !inputs.is_empty() {
        let rows = repositories::bulk_upsert_runtime_graph_nodes(
            pool,
            scope.library_id,
            scope.projection_version,
            &inputs,
        )
        .await?;
        nodes_by_key.extend(rows.into_iter().map(|row| (row.canonical_key.clone(), row)));
    }
    Ok(nodes_by_key)
}

fn relation_endpoint_input(
    canonical_key: &str,
    label: &str,
    existing: Option<&RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> repositories::RuntimeGraphNodeUpsertInput {
    let node_type = crate::services::graph::identity::runtime_node_type_from_key(canonical_key);
    let aliases = normalize_graph_aliases(label, &[label.to_string()]);
    repositories::RuntimeGraphNodeUpsertInput {
        canonical_key: canonical_key.to_string(),
        label: label.trim().to_string(),
        node_type: crate::services::graph::identity::runtime_node_type_slug(&node_type).to_string(),
        aliases_json: serde_json::to_value(aliases).unwrap_or_else(|_| serde_json::json!([])),
        summary: None,
        metadata_json: merge_graph_quality_metadata(
            existing.map(|row| &row.metadata_json),
            extraction_recovery,
            None,
        ),
        support_count: existing.map_or(1, |row| row.support_count.max(1)),
    }
}

async fn upsert_admitted_relation_edges(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    relations: &[AdmittedRelation<'_>],
    nodes_by_key: &HashMap<String, RuntimeGraphNodeRow>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> std::result::Result<Vec<RelationMergeResult>, GraphServiceError> {
    let mut results = Vec::with_capacity(relations.len());
    for admitted in relations {
        let source_node = relation_endpoint(nodes_by_key, &admitted.source_key, "source")?;
        let target_node = relation_endpoint(nodes_by_key, &admitted.target_key, "target")?;
        let edge = upsert_graph_edge(
            pool,
            scope,
            &source_node,
            &target_node,
            &admitted.relation.relation_type,
            admitted.relation.summary.as_deref(),
            extraction_recovery,
        )
        .await?;
        results.push(match edge {
            EdgePersistenceOutcome::Admitted(edge) => RelationMergeResult::Admitted {
                source_node: Box::new(source_node),
                target_node: Box::new(target_node),
                edge,
            },
            EdgePersistenceOutcome::Filtered => RelationMergeResult::Filtered {
                source_node: Box::new(source_node),
                target_node: Box::new(target_node),
            },
        });
    }
    Ok(results)
}

fn relation_endpoint(
    nodes_by_key: &HashMap<String, RuntimeGraphNodeRow>,
    canonical_key: &str,
    role: &str,
) -> std::result::Result<RuntimeGraphNodeRow, GraphServiceError> {
    nodes_by_key.get(canonical_key).cloned().ok_or_else(|| {
        GraphServiceError::Internal(anyhow::anyhow!(
            "bulk upsert did not return {role} node for relation endpoint {canonical_key}"
        ))
    })
}

#[must_use]
fn build_entity_key_index(
    candidates: &GraphExtractionCandidateSet,
) -> crate::services::graph::identity::GraphLabelNodeTypeIndex {
    let mut index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    for entity in &candidates.entities {
        index.insert_aliases(&entity.label, &entity.aliases, entity.node_type);
    }
    index
}

/// One-shot preload of every `runtime_graph_node` row this chunk merge
/// might need (document + every entity + every relation endpoint) by
/// canonical key. Returns a keyed map the upsert helpers consult in
/// place of per-key `get_runtime_graph_node_by_key` SELECTs.
///
/// Collisions (same canonical key appearing in multiple entities or
/// on both sides of a relation) are naturally de-duplicated by the
/// `BTreeSet`. The preloaded row is a read-time snapshot: concurrent
/// upserts inside the same merge may observe stale `support_count` /
/// `metadata_json`, which is the same semantics the single-key path
/// already has today (support counts are reconciled canonically in
/// `reconcile_merge_support_counts`, not per-upsert).
async fn preload_existing_nodes_for_merge(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    candidates: &GraphExtractionCandidateSet,
    entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
) -> AnyhowResult<HashMap<String, RuntimeGraphNodeRow>> {
    let mut canonical_keys: BTreeSet<String> = BTreeSet::new();
    canonical_keys.insert(format!("document:{}", document.id));
    for entity in &candidates.entities {
        let canonical = entity_key_index.canonical_node_key_for_label(&entity.label);
        canonical_keys.insert(canonical);
    }
    for relation in &candidates.relations {
        canonical_keys
            .insert(entity_key_index.canonical_node_key_for_label(&relation.source_label));
        canonical_keys
            .insert(entity_key_index.canonical_node_key_for_label(&relation.target_label));
    }
    if canonical_keys.is_empty() {
        return Ok(HashMap::new());
    }
    let key_vec: Vec<String> = canonical_keys.into_iter().collect();
    let rows = repositories::list_runtime_graph_nodes_by_canonical_keys(
        pool,
        scope.library_id,
        &key_vec,
        scope.projection_version,
    )
    .await?;
    let mut map = HashMap::with_capacity(rows.len());
    for row in rows {
        map.insert(row.canonical_key.clone(), row);
    }
    Ok(map)
}

async fn upsert_document_node(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    preloaded_existing: &HashMap<String, RuntimeGraphNodeRow>,
) -> AnyhowResult<RuntimeGraphNodeRow> {
    let label = document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&document.external_key);
    let canonical_key = format!("document:{}", document.id);
    let existing = preloaded_existing.get(&canonical_key).cloned();
    let support_count = existing.as_ref().map_or(1, |row| row.support_count.max(1));
    let aliases = serde_json::json!([label, document.external_key.clone()]);

    repositories::upsert_runtime_graph_document_node(
        pool,
        scope.library_id,
        document.id,
        &canonical_key,
        label,
        aliases,
        Some("Source document node"),
        serde_json::json!({
            "document_id": document.id,
            "mime_type": document.mime_type,
        }),
        support_count,
        scope.projection_version,
    )
    .await
    .map_err(Into::into)
}

async fn upsert_graph_edge(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    from_node: &RuntimeGraphNodeRow,
    to_node: &RuntimeGraphNodeRow,
    relation_type: &str,
    summary: Option<&str>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> AnyhowResult<EdgePersistenceOutcome> {
    let normalized_relation_type =
        crate::services::graph::identity::normalize_relation_type(relation_type);
    if normalized_relation_type.is_empty() {
        return Ok(EdgePersistenceOutcome::Filtered);
    }
    let canonical_key = crate::services::graph::identity::canonical_edge_key(
        &from_node.canonical_key,
        &normalized_relation_type,
        &to_node.canonical_key,
    );
    if let Some(reason) = graph_edge_integrity_skip_reason(scope, from_node, to_node) {
        repositories::create_runtime_graph_filtered_artifact(
            pool,
            scope.library_id,
            scope.activated_by_attempt_id,
            scope.revision_id,
            "edge",
            &canonical_key,
            Some(&from_node.canonical_key),
            Some(&to_node.canonical_key),
            Some(&normalized_relation_type),
            "graph_persistence_integrity",
            summary,
            serde_json::json!({
                "skip_reason": reason,
                "from_node_id": from_node.id,
                "to_node_id": to_node.id,
                "from_projection_version": from_node.projection_version,
                "to_projection_version": to_node.projection_version,
                "expected_projection_version": scope.projection_version,
            }),
        )
        .await?;
        return Ok(EdgePersistenceOutcome::Filtered);
    }
    let existing = repositories::get_runtime_graph_edge_by_key(
        pool,
        scope.library_id,
        &canonical_key,
        scope.projection_version,
    )
    .await?;
    let support_count = existing.as_ref().map_or(1, |row| row.support_count.max(1));

    repositories::upsert_runtime_graph_edge(
        pool,
        scope.library_id,
        from_node.id,
        to_node.id,
        &normalized_relation_type,
        &canonical_key,
        summary,
        None,
        support_count,
        merge_graph_quality_metadata(
            existing.as_ref().map(|row| &row.metadata_json),
            extraction_recovery,
            summary,
        ),
        scope.projection_version,
    )
    .await
    .map(|edge| EdgePersistenceOutcome::Admitted(Box::new(edge)))
    .map_err(Into::into)
}

fn graph_edge_integrity_skip_reason(
    scope: &GraphMergeScope,
    from_node: &RuntimeGraphNodeRow,
    to_node: &RuntimeGraphNodeRow,
) -> Option<&'static str> {
    if from_node.id.is_nil() || to_node.id.is_nil() {
        return Some("missing_node_id");
    }
    let from_library_id = from_node.library_id;
    let to_library_id = to_node.library_id;
    if from_library_id != scope.library_id || to_library_id != scope.library_id {
        return Some("cross_library_node_reference");
    }
    if from_node.projection_version != scope.projection_version
        || to_node.projection_version != scope.projection_version
    {
        return Some("projection_version_mismatch");
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::runtime_graph::RuntimeNodeType;
    use crate::services::graph::extract::GraphEntityCandidate;

    #[test]
    fn normalizes_aliases_and_deduplicates_them() {
        let aliases = normalize_graph_aliases("OpenAI", &["OpenAI".into(), " Open AI ".into()]);
        assert_eq!(aliases, vec!["Open AI".to_string(), "OpenAI".to_string()]);
    }

    #[test]
    fn normalizes_relation_type_to_snake_case() {
        assert_eq!(
            crate::services::graph::identity::normalize_relation_type("mentions_in"),
            "mentions_in"
        );
    }

    #[test]
    fn builds_canonical_edge_key_from_nodes_and_relation() {
        assert_eq!(
            crate::services::graph::identity::canonical_edge_key(
                "document:1",
                "mentions_in",
                "entity:openai"
            ),
            "document:1--mentions_in--entity:openai"
        );
    }

    #[test]
    fn changed_node_ids_include_edge_endpoints() {
        let source_id = uuid::Uuid::now_v7();
        let target_id = uuid::Uuid::now_v7();
        let outcome = GraphMergeOutcome {
            nodes: vec![],
            edges: vec![RuntimeGraphEdgeRow {
                id: uuid::Uuid::now_v7(),
                library_id: uuid::Uuid::now_v7(),
                from_node_id: source_id,
                to_node_id: target_id,
                relation_type: "depends_on".to_string(),
                canonical_key: "entity:a--depends_on--entity:b".to_string(),
                summary: None,
                weight: None,
                support_count: 1,
                metadata_json: serde_json::json!({}),
                projection_version: 1,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }],
            evidence_count: 0,
            filtered_artifact_count: 0,
        };

        let changed = outcome.changed_node_ids();

        assert!(changed.contains(&source_id));
        assert!(changed.contains(&target_id));
    }

    #[test]
    fn normalizes_labels_to_graph_safe_slug() {
        assert_eq!(
            crate::services::graph::identity::normalize_graph_identity_component(
                " Knowledge   Graph 2.0 "
            ),
            "knowledge_graph_2_0"
        );
    }

    #[test]
    fn normalizes_non_ascii_labels_to_graph_safe_slug() {
        assert_eq!(
            crate::services::graph::identity::normalize_graph_identity_component(
                " Acme Imprenta Düsseldorf "
            ),
            "acme_imprenta_düsseldorf"
        );
    }

    #[test]
    fn normalizes_mixed_script_labels_to_graph_safe_slug() {
        assert_eq!(
            crate::services::graph::identity::normalize_graph_identity_component(
                " Acme: Receipt V2 / QR "
            ),
            "acme_receipt_v2_qr"
        );
    }

    #[test]
    fn rejects_non_canonical_non_ascii_relation_types() {
        assert!(
            crate::services::graph::identity::normalize_relation_type(" είναι μέρος του ")
                .is_empty()
        );
    }

    #[test]
    fn build_entity_key_index_prefers_entity_over_topic_for_same_label() {
        let candidates = GraphExtractionCandidateSet {
            entities: vec![
                GraphEntityCandidate {
                    label: "Register".to_string(),
                    node_type: RuntimeNodeType::Concept,
                    sub_type: None,
                    aliases: vec![],
                    summary: None,
                },
                GraphEntityCandidate {
                    label: "Register".to_string(),
                    node_type: RuntimeNodeType::Entity,
                    sub_type: None,
                    aliases: vec![],
                    summary: None,
                },
            ],
            relations: vec![],
        };

        let index = build_entity_key_index(&candidates);

        assert_eq!(index.canonical_node_key_for_label("Register"), "entity:register");
    }

    #[test]
    fn merge_graph_quality_metadata_tracks_summary_fragments() {
        let metadata =
            merge_graph_quality_metadata(None, None, Some("Budget approval moved to Q2."));

        assert_eq!(
            metadata["summary_fragments"],
            serde_json::json!(["Budget approval moved to Q2."])
        );
    }

    #[test]
    fn filtered_relation_preserves_endpoint_evidence() {
        let library_id = uuid::Uuid::now_v7();
        let source_node = test_graph_node(library_id, "entity:source", "Source");
        let target_node = test_graph_node(library_id, "entity:target", "Target");
        let document_node = test_graph_node(library_id, "document:source", "Document");
        let mut accumulator = MergeAccumulator::new(document_node);

        accumulator.record_relation_result(RelationMergeResult::Filtered {
            source_node: Box::new(source_node.clone()),
            target_node: Box::new(target_node.clone()),
        });

        assert_eq!(
            accumulator.nodes.iter().skip(1).map(|node| node.id).collect::<Vec<_>>(),
            vec![source_node.id, target_node.id]
        );
        assert_eq!(accumulator.evidence_count, 3);
        assert_eq!(accumulator.filtered_artifact_count, 1);
        assert_eq!(
            accumulator
                .evidence_targets
                .iter()
                .filter(|target| target.evidence_context_key.starts_with("relation_"))
                .map(|target| target.target_id)
                .collect::<Vec<_>>(),
            vec![source_node.id, target_node.id]
        );
    }

    fn test_graph_node(
        library_id: uuid::Uuid,
        canonical_key: &str,
        label: &str,
    ) -> RuntimeGraphNodeRow {
        RuntimeGraphNodeRow {
            id: uuid::Uuid::now_v7(),
            library_id,
            canonical_key: canonical_key.to_string(),
            label: label.to_string(),
            node_type: "entity".to_string(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 1,
            projection_version: 1,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn flags_projection_version_mismatch_as_graph_integrity_skip() {
        let scope = GraphMergeScope::new(uuid::Uuid::now_v7(), 4);
        let from_node = RuntimeGraphNodeRow {
            id: uuid::Uuid::now_v7(),
            library_id: scope.library_id,
            canonical_key: "entity:a".to_string(),
            label: "A".to_string(),
            node_type: "entity".to_string(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 1,
            projection_version: 3,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let to_node = RuntimeGraphNodeRow {
            id: uuid::Uuid::now_v7(),
            library_id: scope.library_id,
            canonical_key: "entity:b".to_string(),
            label: "B".to_string(),
            node_type: "entity".to_string(),
            aliases_json: serde_json::json!([]),
            summary: None,
            metadata_json: serde_json::json!({}),
            support_count: 1,
            projection_version: 4,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };

        assert_eq!(
            graph_edge_integrity_skip_reason(&scope, &from_node, &to_node),
            Some("projection_version_mismatch")
        );
    }
}
