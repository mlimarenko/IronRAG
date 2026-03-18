use std::collections::BTreeSet;

use anyhow::Result;

use crate::{
    domains::runtime_graph::RuntimeNodeType,
    infra::repositories::{self, ChunkRow, DocumentRow, RuntimeGraphEdgeRow, RuntimeGraphNodeRow},
    services::{
        graph_extract::{GraphExtractionCandidateSet, GraphRelationCandidate},
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

impl GraphMergeOutcome {
    #[must_use]
    pub fn changed_node_ids(&self) -> Vec<uuid::Uuid> {
        self.nodes.iter().map(|node| node.id).collect::<BTreeSet<_>>().into_iter().collect()
    }

    #[must_use]
    pub fn changed_edge_ids(&self) -> Vec<uuid::Uuid> {
        self.edges.iter().map(|edge| edge.id).collect::<BTreeSet<_>>().into_iter().collect()
    }
}

enum RelationMergeOutcome {
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
pub fn canonical_node_key(node_type: RuntimeNodeType, label: &str) -> String {
    format!("{}:{}", node_type_slug(node_type), normalize_graph_label(label))
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

#[must_use]
pub fn normalize_relation_type(relation_type: &str) -> String {
    relation_type
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|char| if char.is_ascii_alphanumeric() { char } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

#[must_use]
pub fn canonical_edge_key(from_node_key: &str, relation_type: &str, to_node_key: &str) -> String {
    format!("{from_node_key}--{}--{to_node_key}", normalize_relation_type(relation_type))
}

pub async fn merge_chunk_graph_candidates(
    pool: &sqlx::PgPool,
    graph_quality_guard: &GraphQualityGuardService,
    scope: &GraphMergeScope,
    document: &DocumentRow,
    chunk: &ChunkRow,
    candidates: &GraphExtractionCandidateSet,
) -> Result<GraphMergeOutcome> {
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
    )
    .await?;
    let mut nodes = vec![document_node.clone()];
    let mut edges = Vec::new();
    let mut evidence_count = 1usize;
    let mut filtered_artifact_count = 0usize;

    for entity in &candidates.entities {
        let aliases = normalize_graph_aliases(&entity.label, &entity.aliases);
        let node = upsert_graph_node(
            pool,
            scope,
            &entity.label,
            entity.node_type.clone(),
            &aliases,
            entity.summary.as_deref(),
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
        )
        .await?;
        let document_edge = upsert_graph_edge(
            pool,
            scope,
            &document_node,
            &node,
            "mentions",
            Some("Document mentions extracted entity"),
        )
        .await?;
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
        )
        .await?;
        nodes.push(node);
        edges.push(document_edge);
        evidence_count += 2;
    }

    for relation in &candidates.relations {
        match merge_relation_candidate(pool, graph_quality_guard, scope, document, chunk, relation)
            .await?
        {
            RelationMergeOutcome::Admitted(edge) => {
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
                )
                .await?;
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
    relation: &GraphRelationCandidate,
) -> Result<RelationMergeOutcome> {
    let source_node_key = canonical_node_key(RuntimeNodeType::Entity, &relation.source_label);
    let target_node_key = canonical_node_key(RuntimeNodeType::Entity, &relation.target_label);
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
            &canonical_edge_key(&source_node_key, &relation.relation_type, &target_node_key),
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
        RuntimeNodeType::Entity,
        &[relation.source_label.clone()],
        None,
    )
    .await?;
    let target_node = upsert_graph_node(
        pool,
        scope,
        &relation.target_label,
        RuntimeNodeType::Entity,
        &[relation.target_label.clone()],
        None,
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
    )
    .await?;

    Ok(RelationMergeOutcome::Admitted(
        upsert_graph_edge(
            pool,
            scope,
            &source_node,
            &target_node,
            &relation.relation_type,
            relation.summary.as_deref(),
        )
        .await?,
    ))
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
    let support_count = existing.as_ref().map_or(1, |row| row.support_count + 1);
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
) -> Result<RuntimeGraphNodeRow> {
    let canonical_key = canonical_node_key(node_type.clone(), label);
    let existing = repositories::get_runtime_graph_node_by_key(
        pool,
        scope.library_id,
        &canonical_key,
        scope.projection_version,
    )
    .await?;
    let support_count = existing.as_ref().map_or(1, |row| row.support_count + 1);

    repositories::upsert_runtime_graph_node(
        pool,
        scope.library_id,
        &canonical_key,
        label.trim(),
        node_type_slug(node_type),
        serde_json::to_value(normalize_graph_aliases(label, aliases))
            .unwrap_or_else(|_| serde_json::json!([])),
        summary,
        serde_json::json!({}),
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
) -> Result<RuntimeGraphEdgeRow> {
    let normalized_relation_type = normalize_relation_type(relation_type);
    let canonical_key = canonical_edge_key(
        &from_node.canonical_key,
        &normalized_relation_type,
        &to_node.canonical_key,
    );
    let existing = repositories::get_runtime_graph_edge_by_key(
        pool,
        scope.library_id,
        &canonical_key,
        scope.projection_version,
    )
    .await?;
    let support_count = existing.as_ref().map_or(1, |row| row.support_count + 1);

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
        serde_json::json!({}),
        scope.projection_version,
    )
    .await
    .map_err(Into::into)
}

fn normalize_graph_label(label: &str) -> String {
    label
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|char| if char.is_ascii_alphanumeric() { char } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

fn node_type_slug(node_type: RuntimeNodeType) -> &'static str {
    match node_type {
        RuntimeNodeType::Document => "document",
        RuntimeNodeType::Entity => "entity",
        RuntimeNodeType::Topic => "topic",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_canonical_node_key_from_type_and_label() {
        assert_eq!(
            canonical_node_key(RuntimeNodeType::Entity, "Dr. Sarah Chen"),
            "entity:dr_sarah_chen"
        );
    }

    #[test]
    fn normalizes_aliases_and_deduplicates_them() {
        let aliases = normalize_graph_aliases("OpenAI", &["OpenAI".into(), " Open AI ".into()]);
        assert_eq!(aliases, vec!["Open AI".to_string(), "OpenAI".to_string()]);
    }

    #[test]
    fn normalizes_relation_type_to_snake_case() {
        assert_eq!(normalize_relation_type("Mentions In"), "mentions_in");
    }

    #[test]
    fn builds_canonical_edge_key_from_nodes_and_relation() {
        assert_eq!(
            canonical_edge_key("document:1", "mentions in", "entity:openai"),
            "document:1--mentions_in--entity:openai"
        );
    }

    #[test]
    fn normalizes_labels_to_graph_safe_slug() {
        assert_eq!(normalize_graph_label(" Knowledge   Graph 2.0 "), "knowledge_graph_2_0");
    }
}
