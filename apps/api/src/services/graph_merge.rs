use std::collections::BTreeSet;

use anyhow::Result;

use crate::{
    domains::{
        graph_quality::{ExtractionOutcomeStatus, ExtractionRecoverySummary},
        runtime_graph::RuntimeNodeType,
    },
    infra::repositories::{self, ChunkRow, DocumentRow, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    services::{
        graph_extract::{GraphExtractionCandidateSet, GraphRelationCandidate},
        graph_identity,
        graph_quality_guard::GraphQualityGuardService,
    },
};

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
        self.edges.iter().map(|edge| edge.id).collect::<BTreeSet<_>>().into_iter().collect()
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

enum RelationMergeOutcome {
    Admitted { edge: RuntimeGraphEdgeRow, nodes: Vec<RuntimeGraphNodeRow> },
    Filtered,
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
    let document_node = upsert_document_node(pool, scope, document).await?;
    repositories::create_runtime_graph_evidence(
        pool,
        scope.library_id,
        "node",
        document_node.id,
        Some(document.id),
        scope.revision_id,
        scope.activated_by_attempt_id,
        Some(chunk.id),
        Some(&document.external_key),
        None,
        &chunk.content,
        None,
        "document_node",
    )
    .await?;
    let mut nodes = vec![document_node.clone()];
    let mut edges = Vec::new();
    let mut evidence_count = 1usize;
    let mut filtered_artifact_count = 0usize;

    for entity in &candidates.entities {
        let aliases = normalize_graph_aliases(&entity.label, &entity.aliases);
        let canonical_node_key = entity_key_index.canonical_node_key_for_label(&entity.label);
        let canonical_node_type = graph_identity::runtime_node_type_from_key(&canonical_node_key);
        let node = upsert_graph_node(
            pool,
            scope,
            &entity.label,
            canonical_node_type,
            &aliases,
            entity.summary.as_deref(),
            extraction_recovery,
        )
        .await?;
        repositories::create_runtime_graph_evidence(
            pool,
            scope.library_id,
            "node",
            node.id,
            Some(document.id),
            scope.revision_id,
            scope.activated_by_attempt_id,
            Some(chunk.id),
            Some(&document.external_key),
            None,
            &chunk.content,
            None,
            "entity_node",
        )
        .await?;
        let document_edge = upsert_graph_edge(
            pool,
            scope,
            &document_node,
            &node,
            "mentions",
            Some("Document mentions extracted entity"),
            extraction_recovery,
        )
        .await?;
        nodes.push(node);
        match document_edge {
            EdgePersistenceOutcome::Admitted(document_edge) => {
                repositories::create_runtime_graph_evidence(
                    pool,
                    scope.library_id,
                    "edge",
                    document_edge.id,
                    Some(document.id),
                    scope.revision_id,
                    scope.activated_by_attempt_id,
                    Some(chunk.id),
                    Some(&document.external_key),
                    None,
                    &chunk.content,
                    None,
                    "document_mentions_edge",
                )
                .await?;
                edges.push(document_edge);
                evidence_count += 2;
            }
            EdgePersistenceOutcome::Filtered => {
                evidence_count += 1;
                filtered_artifact_count += 1;
            }
        }
    }

    for relation in &candidates.relations {
        match merge_relation_candidate(
            pool,
            graph_quality_guard,
            scope,
            document,
            chunk,
            &entity_key_index,
            relation,
            extraction_recovery,
        )
        .await?
        {
            RelationMergeOutcome::Admitted { edge, nodes: relation_nodes } => {
                repositories::create_runtime_graph_evidence(
                    pool,
                    scope.library_id,
                    "edge",
                    edge.id,
                    Some(document.id),
                    scope.revision_id,
                    scope.activated_by_attempt_id,
                    Some(chunk.id),
                    Some(&document.external_key),
                    None,
                    &chunk.content,
                    None,
                    "relation_edge",
                )
                .await?;
                nodes.extend(relation_nodes);
                edges.push(edge);
                evidence_count += 3;
            }
            RelationMergeOutcome::Filtered => {
                filtered_artifact_count += 1;
            }
        }
    }

    Ok(GraphMergeOutcome { nodes, edges, evidence_count, filtered_artifact_count })
}

async fn merge_relation_candidate(
    pool: &sqlx::PgPool,
    graph_quality_guard: &GraphQualityGuardService,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    chunk: &ChunkRow,
    entity_key_index: &graph_identity::GraphLabelNodeTypeIndex,
    relation: &GraphRelationCandidate,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> Result<RelationMergeOutcome> {
    let source_node_key = entity_key_index.canonical_node_key_for_label(&relation.source_label);
    let target_node_key = entity_key_index.canonical_node_key_for_label(&relation.target_label);
    if let Some(filter_reason) = graph_quality_guard.filter_reason(
        &source_node_key,
        &target_node_key,
        &relation.relation_type,
    ) {
        repositories::create_runtime_graph_filtered_artifact(
            pool,
            scope.library_id,
            scope.activated_by_attempt_id,
            scope.revision_id,
            "edge",
            &graph_identity::canonical_edge_key(
                &source_node_key,
                &relation.relation_type,
                &target_node_key,
            ),
            Some(&source_node_key),
            Some(&target_node_key),
            Some(&graph_quality_guard.normalized_relation_type(&relation.relation_type)),
            match filter_reason {
                crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::EmptyRelation => {
                    "empty_relation"
                }
                crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::DegenerateSelfLoop => {
                    "degenerate_self_loop"
                }
                crate::domains::runtime_graph::RuntimeGraphArtifactFilterReason::LowValueArtifact => {
                    "low_value_artifact"
                }
            },
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
        return Ok(RelationMergeOutcome::Filtered);
    }

    let source_node = upsert_graph_node(
        pool,
        scope,
        &relation.source_label,
        graph_identity::runtime_node_type_from_key(&source_node_key),
        std::slice::from_ref(&relation.source_label),
        None,
        extraction_recovery,
    )
    .await?;
    let target_node = upsert_graph_node(
        pool,
        scope,
        &relation.target_label,
        graph_identity::runtime_node_type_from_key(&target_node_key),
        std::slice::from_ref(&relation.target_label),
        None,
        extraction_recovery,
    )
    .await?;
    repositories::create_runtime_graph_evidence(
        pool,
        scope.library_id,
        "node",
        source_node.id,
        Some(document.id),
        scope.revision_id,
        scope.activated_by_attempt_id,
        Some(chunk.id),
        Some(&document.external_key),
        None,
        &chunk.content,
        None,
        "relation_source_node",
    )
    .await?;
    repositories::create_runtime_graph_evidence(
        pool,
        scope.library_id,
        "node",
        target_node.id,
        Some(document.id),
        scope.revision_id,
        scope.activated_by_attempt_id,
        Some(chunk.id),
        Some(&document.external_key),
        None,
        &chunk.content,
        None,
        "relation_target_node",
    )
    .await?;

    Ok(RelationMergeOutcome::Admitted {
        edge: match upsert_graph_edge(
            pool,
            scope,
            &source_node,
            &target_node,
            &relation.relation_type,
            relation.summary.as_deref(),
            extraction_recovery,
        )
        .await?
        {
            EdgePersistenceOutcome::Admitted(edge) => edge,
            EdgePersistenceOutcome::Filtered => return Ok(RelationMergeOutcome::Filtered),
        },
        nodes: vec![source_node, target_node],
    })
}

#[must_use]
fn build_entity_key_index(
    candidates: &GraphExtractionCandidateSet,
) -> graph_identity::GraphLabelNodeTypeIndex {
    let mut index = graph_identity::GraphLabelNodeTypeIndex::new();
    for entity in &candidates.entities {
        index.insert_aliases(&entity.label, &entity.aliases, entity.node_type.clone());
    }
    index
}

async fn upsert_document_node(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    document: &DocumentRow,
) -> Result<RuntimeGraphNodeRow> {
    let label = document
        .title
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&document.external_key);
    let canonical_key = format!("document:{}", document.id);
    let existing = repositories::get_runtime_graph_node_by_key(
        pool,
        scope.library_id,
        &canonical_key,
        scope.projection_version,
    )
    .await?;
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

async fn upsert_graph_node(
    pool: &sqlx::PgPool,
    scope: &GraphMergeScope,
    label: &str,
    node_type: RuntimeNodeType,
    aliases: &[String],
    summary: Option<&str>,
    extraction_recovery: Option<&ExtractionRecoverySummary>,
) -> Result<RuntimeGraphNodeRow> {
    let canonical_key = graph_identity::canonical_node_key(node_type.clone(), label);
    let existing = repositories::get_runtime_graph_node_by_key(
        pool,
        scope.library_id,
        &canonical_key,
        scope.projection_version,
    )
    .await?;
    let support_count = existing.as_ref().map_or(1, |row| row.support_count.max(1));

    repositories::upsert_runtime_graph_node(
        pool,
        scope.library_id,
        &canonical_key,
        label.trim(),
        graph_identity::runtime_node_type_slug(&node_type),
        serde_json::to_value(normalize_graph_aliases(label, aliases))
            .unwrap_or_else(|_| serde_json::json!([])),
        summary,
        merge_graph_quality_metadata(
            existing.as_ref().map(|row| &row.metadata_json),
            extraction_recovery,
            summary,
        ),
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
    let normalized_relation_type = graph_identity::normalize_relation_type(relation_type);
    if normalized_relation_type.is_empty() {
        return Ok(EdgePersistenceOutcome::Filtered);
    }
    let canonical_key = graph_identity::canonical_edge_key(
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
    use crate::services::graph_extract::GraphEntityCandidate;

    #[test]
    fn normalizes_aliases_and_deduplicates_them() {
        let aliases = normalize_graph_aliases("OpenAI", &["OpenAI".into(), " Open AI ".into()]);
        assert_eq!(aliases, vec!["Open AI".to_string(), "OpenAI".to_string()]);
    }

    #[test]
    fn normalizes_relation_type_to_snake_case() {
        assert_eq!(graph_identity::normalize_relation_type("Mentions In"), "mentions_in");
    }

    #[test]
    fn builds_canonical_edge_key_from_nodes_and_relation() {
        assert_eq!(
            graph_identity::canonical_edge_key("document:1", "mentions in", "entity:openai"),
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
            graph_identity::normalize_graph_identity_component(" Knowledge   Graph 2.0 "),
            "knowledge_graph_2_0"
        );
    }

    #[test]
    fn normalizes_cyrillic_labels_to_graph_safe_slug() {
        assert_eq!(
            graph_identity::normalize_graph_identity_component(" Первый печатный двор "),
            "первый_печатный_двор"
        );
    }

    #[test]
    fn normalizes_mixed_script_labels_to_graph_safe_slug() {
        assert_eq!(
            graph_identity::normalize_graph_identity_component(" Acme: Чек V2 / QR "),
            "acme_чек_v2_qr"
        );
    }

    #[test]
    fn rejects_non_canonical_cyrillic_relation_types() {
        assert!(graph_identity::normalize_relation_type(" Является частью ").is_empty());
    }

    #[test]
    fn build_entity_key_index_prefers_entity_over_topic_for_same_label() {
        let candidates = GraphExtractionCandidateSet {
            entities: vec![
                GraphEntityCandidate {
                    label: "Касса".to_string(),
                    node_type: RuntimeNodeType::Topic,
                    aliases: vec![],
                    summary: None,
                },
                GraphEntityCandidate {
                    label: "Касса".to_string(),
                    node_type: RuntimeNodeType::Entity,
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
