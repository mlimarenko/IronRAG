use std::collections::{BTreeSet, HashMap};

use anyhow::Result;
use futures::stream::{self, StreamExt, TryStreamExt};

use crate::{
    domains::graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
    infra::repositories::{self, ChunkRow, DocumentRow, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    services::{
        graph::extract::GraphExtractionCandidateSet, graph::quality_guard::GraphQualityGuardService,
    },
};

/// How many per-entity (upsert node + upsert mentions-edge) pipelines we
/// allow to run in parallel while merging one chunk's extraction output.
/// The entity loop used to be serial — 15 entities × 2 Postgres round-trips
/// = 30 serial awaits per chunk, so a round-trip cost of 5-10 ms bound the
/// whole merge to 150-300 ms even when extraction returned cheap rows.
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

pub async fn reconcile_merge_support_counts(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    changed_node_ids: &[uuid::Uuid],
    changed_edge_ids: &[uuid::Uuid],
) -> Result<()> {
    repositories::recalculate_runtime_graph_node_support_counts_by_ids(
        pool,
        scope.library_id,
        scope.projection_version,
        changed_node_ids,
    )
    .await?;
    repositories::recalculate_runtime_graph_edge_support_counts_by_ids(
        pool,
        scope.library_id,
        scope.projection_version,
        changed_edge_ids,
    )
    .await?;
    Ok(())
}

impl GraphMergeOutcome {
    #[must_use]
    pub fn has_projection_follow_up(&self) -> bool {
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
    Admitted(RuntimeGraphEdgeRow),
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
pub fn normalize_graph_aliases(label: &str, aliases: &[String]) -> Vec<String> {
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

pub async fn merge_chunk_graph_candidates(
    pool: &sqlx::PgPool,
    graph_quality_guard: &GraphQualityGuardService,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    chunk: &ChunkRow,
    candidates: &GraphExtractionCandidateSet,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> Result<GraphMergeOutcome> {
    let entity_key_index = build_entity_key_index(candidates);
    // Collect every evidence row generated during this chunk merge into one
    // batch and flush via `bulk_create_runtime_graph_evidence_for_chunk` at
    // the end. Replaces 50+ sequential single-row INSERTs with a single
    // bulk `INSERT ... SELECT FROM unnest(...)` round-trip.
    let mut evidence_targets: Vec<repositories::GraphEvidenceTarget> = Vec::new();

    // Bulk-preload every runtime_graph_node row this chunk merge will
    // touch (document + each entity + each relation endpoint) in a single
    // round-trip. The per-upsert branch then reads `existing` from this
    // map instead of issuing its own SELECT. On a typical chunk with 15
    // entities and 10 relations, that's ~35 serial SELECTs collapsed
    // into 1 indexed range scan — cuts pool-hold time and lock-wait
    // pressure during the parallel entity upsert fan-out.
    let preloaded_existing =
        preload_existing_nodes_for_merge(pool, scope, document, candidates, &entity_key_index)
            .await?;

    let document_node = upsert_document_node(pool, scope, document, &preloaded_existing).await?;
    evidence_targets.push(repositories::GraphEvidenceTarget {
        target_kind: "node",
        target_id: document_node.id,
        evidence_context_key: "document_node",
    });
    let mut nodes = vec![document_node.clone()];
    let mut edges = Vec::new();
    let mut evidence_count = 1usize;
    let mut filtered_artifact_count = 0usize;

    // Bulk-upsert every entity node in one round-trip. The old shape
    // fanned out per-entity `upsert_graph_node` calls across
    // `ENTITY_UPSERT_CONCURRENCY` concurrent tasks, each holding a
    // pool connection for the node INSERT plus the edge INSERT — on a
    // chunk with 15 entities that's 30 parallel connection grabs on
    // the main Postgres pool and 30 small WAL flushes. Under prod load
    // we measured that pattern dominating the `slow statement` log
    // with row-lock contention even though individual INSERTs were
    // sub-second on an unloaded pool.
    //
    // The bulk UPSERT uses `ON CONFLICT ... DO UPDATE` per row, so
    // same-key duplicates inside one input follow last-writer-wins
    // semantics identical to the old parallel fan-out (which also had
    // no deterministic order under race). Dedup by canonical key
    // preserves the previous behaviour of collapsing repeat labels.
    let extraction_recovery_for_edges = extraction_recovery.cloned();
    let mut entity_bulk_inputs: Vec<repositories::RuntimeGraphNodeUpsertInput> =
        Vec::with_capacity(candidates.entities.len());
    let mut entity_canonical_keys: Vec<String> = Vec::with_capacity(candidates.entities.len());
    let mut seen_entity_keys: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for entity in &candidates.entities {
        let aliases = normalize_graph_aliases(&entity.label, &entity.aliases);
        let canonical_node_key = entity_key_index.canonical_node_key_for_label(&entity.label);
        let canonical_node_type =
            crate::services::graph::identity::runtime_node_type_from_key(&canonical_node_key);
        entity_canonical_keys.push(canonical_node_key.clone());
        if !seen_entity_keys.insert(canonical_node_key.clone()) {
            continue;
        }
        let existing = preloaded_existing.get(&canonical_node_key);
        let support_count = existing.map_or(1, |row| row.support_count.max(1));
        let mut metadata = merge_graph_quality_metadata(
            existing.map(|row| &row.metadata_json),
            extraction_recovery,
            entity.summary.as_deref(),
        );
        if let Some(st) = entity.sub_type.as_deref() {
            if let Some(obj) = metadata.as_object_mut() {
                obj.insert("sub_type".to_string(), serde_json::Value::String(st.to_string()));
            }
        }
        entity_bulk_inputs.push(repositories::RuntimeGraphNodeUpsertInput {
            canonical_key: canonical_node_key,
            label: entity.label.trim().to_string(),
            node_type: crate::services::graph::identity::runtime_node_type_slug(
                &canonical_node_type,
            )
            .to_string(),
            aliases_json: serde_json::to_value(aliases).unwrap_or_else(|_| serde_json::json!([])),
            summary: entity.summary.clone(),
            metadata_json: metadata,
            support_count,
        });
    }
    let entity_nodes_by_key: std::collections::HashMap<String, RuntimeGraphNodeRow> =
        if entity_bulk_inputs.is_empty() {
            std::collections::HashMap::new()
        } else {
            let upserted = repositories::bulk_upsert_runtime_graph_nodes(
                pool,
                scope.library_id,
                scope.projection_version,
                &entity_bulk_inputs,
            )
            .await?;
            upserted.into_iter().map(|row| (row.canonical_key.clone(), row)).collect()
        };

    // Fan out only the doc-mentions edge work — nodes are already
    // persisted via the bulk call above, so each task now runs exactly
    // ONE Postgres round-trip (edge upsert), not two.
    let document_node_for_edges = document_node.clone();
    let edge_inputs: Vec<(RuntimeGraphNodeRow, String)> = entity_canonical_keys
        .iter()
        .filter_map(|key| entity_nodes_by_key.get(key).map(|row| (row.clone(), key.clone())))
        .collect();
    let entity_results: Vec<(RuntimeGraphNodeRow, EdgePersistenceOutcome)> =
        stream::iter(edge_inputs)
            .map(|(entity_node, _canonical_key)| {
                let pool = pool.clone();
                let scope = scope.clone();
                let document_node_for_edge = document_node_for_edges.clone();
                let extraction_recovery = extraction_recovery_for_edges.clone();
                async move {
                    let document_edge = upsert_graph_edge(
                        &pool,
                        &scope,
                        &document_node_for_edge,
                        &entity_node,
                        "mentions",
                        Some("Document mentions extracted entity"),
                        extraction_recovery.as_ref(),
                    )
                    .await?;
                    anyhow::Ok((entity_node, document_edge))
                }
            })
            .buffered(ENTITY_UPSERT_CONCURRENCY)
            .try_collect()
            .await?;
    for (node, document_edge) in entity_results {
        evidence_targets.push(repositories::GraphEvidenceTarget {
            target_kind: "node",
            target_id: node.id,
            evidence_context_key: "entity_node",
        });
        nodes.push(node);
        match document_edge {
            EdgePersistenceOutcome::Admitted(document_edge) => {
                evidence_targets.push(repositories::GraphEvidenceTarget {
                    target_kind: "edge",
                    target_id: document_edge.id,
                    evidence_context_key: "document_mentions_edge",
                });
                edges.push(document_edge);
                evidence_count += 2;
            }
            EdgePersistenceOutcome::Filtered => {
                evidence_count += 1;
                filtered_artifact_count += 1;
            }
        }
    }

    // Split relations by quality-guard decision using a sync filter
    // check (`graph_quality_guard.filter_reason` is a pure function).
    // Filtered relations go straight to an artifact row; admitted
    // relations flow into a second bulk node upsert for any source /
    // target endpoint that wasn't already covered by `entity_bulk_inputs`.
    let mut filtered_relations: Vec<(
        &crate::services::graph::extract::GraphRelationCandidate,
        String,
        String,
        &'static str,
    )> = Vec::new();
    let mut admitted_relations: Vec<(
        &crate::services::graph::extract::GraphRelationCandidate,
        String,
        String,
    )> = Vec::new();
    for relation in &candidates.relations {
        let source_key = entity_key_index.canonical_node_key_for_label(&relation.source_label);
        let target_key = entity_key_index.canonical_node_key_for_label(&relation.target_label);
        if let Some(filter_reason) =
            graph_quality_guard.filter_reason(&source_key, &target_key, &relation.relation_type)
        {
            let reason_label = match filter_reason {
                crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::EmptyRelation => {
                    "empty_relation"
                }
                crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::DegenerateSelfLoop => {
                    "degenerate_self_loop"
                }
                crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::LowValueArtifact => {
                    "low_value_artifact"
                }
            };
            filtered_relations.push((relation, source_key, target_key, reason_label));
        } else {
            admitted_relations.push((relation, source_key, target_key));
        }
    }

    // Filter artifacts — per-row INSERTs. Count is typically small
    // (low-single digits per chunk) so leaving these serial keeps the
    // code simple; if that changes a bulk insert can be added later.
    for (relation, source_key, target_key, reason_label) in &filtered_relations {
        repositories::create_runtime_graph_filtered_artifact(
            pool,
            scope.library_id,
            scope.activated_by_attempt_id,
            scope.revision_id,
            "edge",
            &crate::services::graph::identity::canonical_edge_key(
                source_key,
                &relation.relation_type,
                target_key,
            ),
            Some(source_key.as_str()),
            Some(target_key.as_str()),
            Some(&graph_quality_guard.normalized_relation_type(&relation.relation_type)),
            reason_label,
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
        filtered_artifact_count += 1;
    }

    // Second bulk UPSERT: admitted-relation endpoints that weren't
    // already persisted by the entity bulk. Endpoints that DO appear
    // in `entity_nodes_by_key` are re-used verbatim — no duplicate
    // upsert. Endpoints introduced only by relations (nodes the
    // extractor referenced but didn't list as entities) are collected
    // here and persisted in one round-trip.
    let mut endpoint_bulk_inputs: Vec<repositories::RuntimeGraphNodeUpsertInput> = Vec::new();
    let mut endpoint_seen_keys: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for (relation, source_key, target_key) in &admitted_relations {
        for (key, label) in [
            (source_key.as_str(), relation.source_label.as_str()),
            (target_key.as_str(), relation.target_label.as_str()),
        ] {
            if entity_nodes_by_key.contains_key(key) {
                continue;
            }
            if !endpoint_seen_keys.insert(key.to_string()) {
                continue;
            }
            let node_type = crate::services::graph::identity::runtime_node_type_from_key(key);
            let aliases = normalize_graph_aliases(label, std::slice::from_ref(&label.to_string()));
            let existing = preloaded_existing.get(key);
            let support_count = existing.map_or(1, |row| row.support_count.max(1));
            let metadata = merge_graph_quality_metadata(
                existing.map(|row| &row.metadata_json),
                extraction_recovery,
                None,
            );
            endpoint_bulk_inputs.push(repositories::RuntimeGraphNodeUpsertInput {
                canonical_key: key.to_string(),
                label: label.trim().to_string(),
                node_type: crate::services::graph::identity::runtime_node_type_slug(&node_type)
                    .to_string(),
                aliases_json: serde_json::to_value(aliases)
                    .unwrap_or_else(|_| serde_json::json!([])),
                summary: None,
                metadata_json: metadata,
                support_count,
            });
        }
    }
    let mut all_nodes_by_key = entity_nodes_by_key;
    if !endpoint_bulk_inputs.is_empty() {
        let endpoint_upserted = repositories::bulk_upsert_runtime_graph_nodes(
            pool,
            scope.library_id,
            scope.projection_version,
            &endpoint_bulk_inputs,
        )
        .await?;
        for row in endpoint_upserted {
            all_nodes_by_key.insert(row.canonical_key.clone(), row);
        }
    }

    // Finally, upsert each admitted relation's edge. Nodes are all in
    // `all_nodes_by_key` by this point. Edge upserts are kept serial
    // here — typical chunk has ≤ 15 admitted relations; a fan-out
    // would help only marginally and would complicate the
    // evidence_targets/nodes/edges accumulation.
    for (relation, source_key, target_key) in &admitted_relations {
        let source_node = all_nodes_by_key.get(source_key).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "bulk upsert did not return source node for relation endpoint {source_key}"
            )
        })?;
        let target_node = all_nodes_by_key.get(target_key).cloned().ok_or_else(|| {
            anyhow::anyhow!(
                "bulk upsert did not return target node for relation endpoint {target_key}"
            )
        })?;
        evidence_targets.push(repositories::GraphEvidenceTarget {
            target_kind: "node",
            target_id: source_node.id,
            evidence_context_key: "relation_source_node",
        });
        evidence_targets.push(repositories::GraphEvidenceTarget {
            target_kind: "node",
            target_id: target_node.id,
            evidence_context_key: "relation_target_node",
        });
        let edge_outcome = upsert_graph_edge(
            pool,
            scope,
            &source_node,
            &target_node,
            &relation.relation_type,
            relation.summary.as_deref(),
            extraction_recovery,
        )
        .await?;
        match edge_outcome {
            EdgePersistenceOutcome::Admitted(edge) => {
                evidence_targets.push(repositories::GraphEvidenceTarget {
                    target_kind: "edge",
                    target_id: edge.id,
                    evidence_context_key: "relation_edge",
                });
                nodes.push(source_node);
                nodes.push(target_node);
                edges.push(edge);
                evidence_count += 3;
            }
            EdgePersistenceOutcome::Filtered => {
                filtered_artifact_count += 1;
            }
        }
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
        &evidence_targets,
    )
    .await?;

    Ok(GraphMergeOutcome { nodes, edges, evidence_count, filtered_artifact_count })
}

#[must_use]
fn build_entity_key_index(
    candidates: &GraphExtractionCandidateSet,
) -> crate::services::graph::identity::GraphLabelNodeTypeIndex {
    let mut index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    for entity in &candidates.entities {
        index.insert_aliases(&entity.label, &entity.aliases, entity.node_type.clone());
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
) -> Result<HashMap<String, RuntimeGraphNodeRow>> {
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
) -> Result<RuntimeGraphNodeRow> {
    let label = document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&document.external_key);
    let canonical_key = format!("document:{}", document.id);
    let existing = preloaded_existing.get(&canonical_key).cloned();
    let support_count = existing.as_ref().map_or(1, |row| row.support_count.max(1));
    let aliases = serde_json::json!([label, document.external_key.clone()]);

    repositories::upsert_runtime_graph_node(
        pool,
        scope.library_id,
        &canonical_key,
        label,
        "document",
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
) -> Result<EdgePersistenceOutcome> {
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
    .map(EdgePersistenceOutcome::Admitted)
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
            crate::services::graph::identity::normalize_relation_type("Mentions In"),
            "mentions_in"
        );
    }

    #[test]
    fn builds_canonical_edge_key_from_nodes_and_relation() {
        assert_eq!(
            crate::services::graph::identity::canonical_edge_key(
                "document:1",
                "mentions in",
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
    fn normalizes_cyrillic_labels_to_graph_safe_slug() {
        assert_eq!(
            crate::services::graph::identity::normalize_graph_identity_component(
                " Первый печатный двор "
            ),
            "первый_печатный_двор"
        );
    }

    #[test]
    fn normalizes_mixed_script_labels_to_graph_safe_slug() {
        assert_eq!(
            crate::services::graph::identity::normalize_graph_identity_component(
                " Acme: Чек V2 / QR "
            ),
            "acme_чек_v2_qr"
        );
    }

    #[test]
    fn rejects_non_canonical_cyrillic_relation_types() {
        assert!(
            crate::services::graph::identity::normalize_relation_type(" Является частью ")
                .is_empty()
        );
    }

    #[test]
    fn build_entity_key_index_prefers_entity_over_topic_for_same_label() {
        let candidates = GraphExtractionCandidateSet {
            entities: vec![
                GraphEntityCandidate {
                    label: "Касса".to_string(),
                    node_type: RuntimeNodeType::Concept,
                    sub_type: None,
                    aliases: vec![],
                    summary: None,
                },
                GraphEntityCandidate {
                    label: "Касса".to_string(),
                    node_type: RuntimeNodeType::Entity,
                    sub_type: None,
                    aliases: vec![],
                    summary: None,
                },
            ],
            relations: vec![],
        };

        let index = build_entity_key_index(&candidates);

        assert_eq!(index.canonical_node_key_for_label("Касса"), "entity:касса");
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
