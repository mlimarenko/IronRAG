use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, anyhow};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::graph_quality::GraphSummaryConfidenceStatus,
    infra::repositories::{
        self, RuntimeGraphEdgeRow, RuntimeGraphEvidenceRow, RuntimeGraphNodeRow, catalog_repository,
    },
    services::graph::projection::active_projection_version,
};

#[derive(Debug, Clone, Default)]
pub struct GraphSummaryService;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphSummaryRefreshRequest {
    pub source_truth_version: Option<i64>,
    pub node_ids: Vec<Uuid>,
    pub edge_ids: Vec<Uuid>,
    pub broad_refresh: bool,
}

impl GraphSummaryRefreshRequest {
    #[must_use]
    pub fn broad() -> Self {
        Self {
            source_truth_version: None,
            node_ids: Vec::new(),
            edge_ids: Vec::new(),
            broad_refresh: true,
        }
    }

    #[must_use]
    pub fn targeted(node_ids: Vec<Uuid>, edge_ids: Vec<Uuid>) -> Self {
        Self { source_truth_version: None, node_ids, edge_ids, broad_refresh: false }
    }

    #[must_use]
    pub fn with_source_truth_version(mut self, source_truth_version: i64) -> Self {
        self.source_truth_version = Some(source_truth_version);
        self
    }

    #[must_use]
    pub fn for_mutation_scope(
        scope_status: &str,
        node_ids: Vec<Uuid>,
        edge_ids: Vec<Uuid>,
    ) -> Self {
        if scope_status == "targeted" && (!node_ids.is_empty() || !edge_ids.is_empty()) {
            Self::targeted(node_ids, edge_ids)
        } else {
            Self::broad()
        }
    }

    #[must_use]
    pub fn is_targeted(&self) -> bool {
        !self.broad_refresh && (!self.node_ids.is_empty() || !self.edge_ids.is_empty())
    }

    #[must_use]
    pub fn is_active(&self) -> bool {
        self.source_truth_version.is_some() && (self.broad_refresh || self.is_targeted())
    }
}

impl GraphSummaryService {
    pub async fn invalidate_summaries(
        &self,
        state: &AppState,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
    ) -> anyhow::Result<u64> {
        let Some(source_truth_version) = refresh.source_truth_version else {
            return Ok(0);
        };

        if refresh.is_targeted() {
            repositories::supersede_runtime_graph_canonical_summaries_for_targets(
                &state.persistence.postgres,
                library_id,
                source_truth_version,
                &refresh.node_ids,
                &refresh.edge_ids,
            )
            .await
            .context("failed to supersede targeted canonical summaries after source-truth change")
        } else if refresh.broad_refresh {
            repositories::supersede_runtime_graph_canonical_summaries_for_library(
                &state.persistence.postgres,
                library_id,
                source_truth_version,
            )
            .await
            .context("failed to supersede library canonical summaries after source-truth change")
        } else {
            Ok(0)
        }
    }

    #[must_use]
    pub fn should_batch_refresh(&self, affected_targets: usize, batch_limit: usize) -> bool {
        affected_targets > 0 && affected_targets <= batch_limit
    }

    pub async fn refresh_summaries(
        &self,
        state: &AppState,
        library_id: Uuid,
        refresh: &GraphSummaryRefreshRequest,
    ) -> anyhow::Result<u64> {
        let Some(source_truth_version) = refresh.source_truth_version else {
            return Ok(0);
        };

        let Some(library) =
            catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
                .await
                .context("failed to load library while refreshing canonical summaries")?
        else {
            return Err(anyhow!("library {library_id} not found while refreshing summaries"));
        };

        let snapshot =
            repositories::get_runtime_graph_snapshot(&state.persistence.postgres, library_id)
                .await
                .context("failed to load runtime graph snapshot while refreshing summaries")?;
        let projection_version = active_projection_version(snapshot.as_ref());

        let nodes = load_target_nodes(state, library_id, projection_version, refresh).await?;
        let edges = load_target_edges(state, library_id, projection_version, refresh).await?;
        if nodes.is_empty() && edges.is_empty() {
            return Ok(0);
        }

        let mut node_label_index =
            nodes.iter().map(|node| (node.id, node.label.clone())).collect::<HashMap<_, _>>();
        let missing_endpoint_ids = edges
            .iter()
            .flat_map(|edge| [edge.from_node_id, edge.to_node_id])
            .filter(|node_id| !node_label_index.contains_key(node_id))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        if !missing_endpoint_ids.is_empty() {
            let endpoint_nodes = repositories::list_admitted_runtime_graph_nodes_by_ids(
                &state.persistence.postgres,
                library_id,
                projection_version,
                &missing_endpoint_ids,
            )
            .await
            .context("failed to load endpoint nodes for canonical edge summaries")?;
            for node in endpoint_nodes {
                node_label_index.insert(node.id, node.label);
            }
        }

        let mut refreshed = 0u64;
        for node in &nodes {
            let evidence = repositories::list_runtime_graph_evidence_by_target(
                &state.persistence.postgres,
                library_id,
                "node",
                node.id,
            )
            .await
            .with_context(|| format!("failed to load active evidence for node {}", node.id))?;
            let (summary_text, confidence_status, warning_text) =
                build_node_summary(node, &evidence);
            repositories::upsert_runtime_graph_canonical_summary(
                &state.persistence.postgres,
                &repositories::UpsertRuntimeGraphCanonicalSummaryInput {
                    workspace_id: library.workspace_id,
                    library_id,
                    target_kind: "node".to_string(),
                    target_id: node.id,
                    summary_text,
                    confidence_status,
                    support_count: node.support_count,
                    source_truth_version,
                    generated_from_mutation_id: None,
                    warning_text,
                },
            )
            .await
            .with_context(|| format!("failed to upsert canonical summary for node {}", node.id))?;
            refreshed += 1;
        }

        for edge in &edges {
            let evidence = repositories::list_runtime_graph_evidence_by_target(
                &state.persistence.postgres,
                library_id,
                "edge",
                edge.id,
            )
            .await
            .with_context(|| format!("failed to load active evidence for edge {}", edge.id))?;
            let from_label = node_label_index
                .get(&edge.from_node_id)
                .cloned()
                .unwrap_or_else(|| edge.from_node_id.to_string());
            let to_label = node_label_index
                .get(&edge.to_node_id)
                .cloned()
                .unwrap_or_else(|| edge.to_node_id.to_string());
            let (summary_text, confidence_status, warning_text) =
                build_edge_summary(edge, &from_label, &to_label, &evidence);
            repositories::upsert_runtime_graph_canonical_summary(
                &state.persistence.postgres,
                &repositories::UpsertRuntimeGraphCanonicalSummaryInput {
                    workspace_id: library.workspace_id,
                    library_id,
                    target_kind: "edge".to_string(),
                    target_id: edge.id,
                    summary_text,
                    confidence_status,
                    support_count: edge.support_count,
                    source_truth_version,
                    generated_from_mutation_id: None,
                    warning_text,
                },
            )
            .await
            .with_context(|| format!("failed to upsert canonical summary for edge {}", edge.id))?;
            refreshed += 1;
        }

        Ok(refreshed)
    }
}

async fn load_target_nodes(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    refresh: &GraphSummaryRefreshRequest,
) -> anyhow::Result<Vec<RuntimeGraphNodeRow>> {
    if refresh.is_targeted() {
        repositories::list_admitted_runtime_graph_nodes_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_version,
            &refresh.node_ids,
        )
        .await
        .context("failed to load targeted graph nodes for summary refresh")
    } else {
        repositories::list_admitted_runtime_graph_nodes_by_library(
            &state.persistence.postgres,
            library_id,
            projection_version,
        )
        .await
        .context("failed to load graph nodes for summary refresh")
    }
}

async fn load_target_edges(
    state: &AppState,
    library_id: Uuid,
    projection_version: i64,
    refresh: &GraphSummaryRefreshRequest,
) -> anyhow::Result<Vec<RuntimeGraphEdgeRow>> {
    if refresh.is_targeted() {
        repositories::list_admitted_runtime_graph_edges_by_ids(
            &state.persistence.postgres,
            library_id,
            projection_version,
            &refresh.edge_ids,
        )
        .await
        .context("failed to load targeted graph edges for summary refresh")
    } else {
        repositories::list_admitted_runtime_graph_edges_by_library(
            &state.persistence.postgres,
            library_id,
            projection_version,
        )
        .await
        .context("failed to load graph edges for summary refresh")
    }
}

fn build_node_summary(
    node: &RuntimeGraphNodeRow,
    evidence: &[RuntimeGraphEvidenceRow],
) -> (String, String, Option<String>) {
    let summary_fragments = load_summary_fragments(&node.metadata_json, node.summary.as_deref());
    let evidence_fragments = build_evidence_fragments(evidence, 2);
    let lead = summary_fragments.first().cloned().or_else(|| evidence_fragments.first().cloned());
    let support_sentence = if node.support_count > 1 {
        Some(format!("Supported by {} active evidence lines.", node.support_count))
    } else {
        None
    };
    let fallback = if node.node_type == "document" {
        format!("{} is a source document node in the active library graph.", node.label)
    } else {
        format!("{} is an active {} in the library graph.", node.label, node.node_type)
    };
    let summary_text =
        compose_summary_text(lead.as_deref(), support_sentence.as_deref(), &fallback);
    let (confidence_status, warning_text) = classify_summary_confidence(
        node.support_count,
        &node.metadata_json,
        summary_fragments.len(),
    );
    (summary_text, confidence_status, warning_text)
}

fn build_edge_summary(
    edge: &RuntimeGraphEdgeRow,
    from_label: &str,
    to_label: &str,
    evidence: &[RuntimeGraphEvidenceRow],
) -> (String, String, Option<String>) {
    let summary_fragments = load_summary_fragments(&edge.metadata_json, edge.summary.as_deref());
    let evidence_fragments = build_evidence_fragments(evidence, 1);
    let relationship_sentence =
        format!("{} {} {}.", from_label, edge.relation_type.replace('_', " "), to_label);
    let lead = summary_fragments
        .first()
        .cloned()
        .or_else(|| evidence_fragments.first().cloned())
        .unwrap_or(relationship_sentence);
    let support_sentence = if edge.support_count > 1 {
        Some(format!("Supported by {} active relationship evidence lines.", edge.support_count))
    } else {
        None
    };
    let summary_text = compose_summary_text(Some(&lead), support_sentence.as_deref(), &lead);
    let (confidence_status, warning_text) = classify_summary_confidence(
        edge.support_count,
        &edge.metadata_json,
        summary_fragments.len(),
    );
    (summary_text, confidence_status, warning_text)
}

fn load_summary_fragments(
    metadata: &serde_json::Value,
    fallback_summary: Option<&str>,
) -> Vec<String> {
    let mut values = metadata
        .get("summary_fragments")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .filter_map(normalize_fragment)
        .collect::<BTreeSet<_>>();

    if let Some(fallback_summary) = fallback_summary.and_then(normalize_fragment) {
        values.insert(fallback_summary);
    }

    values.into_iter().collect()
}

fn build_evidence_fragments(evidence: &[RuntimeGraphEvidenceRow], limit: usize) -> Vec<String> {
    evidence
        .iter()
        .filter_map(|row| normalize_fragment(&row.evidence_text))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(limit)
        .collect()
}

fn normalize_fragment(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }

    let truncated = if normalized.chars().count() > 220 {
        let mut shortened = normalized.chars().take(217).collect::<String>();
        shortened.push_str("...");
        shortened
    } else {
        normalized
    };
    Some(ensure_sentence(&truncated))
}

fn compose_summary_text(
    lead: Option<&str>,
    support_sentence: Option<&str>,
    fallback: &str,
) -> String {
    let mut parts = Vec::new();
    parts.push(
        lead.map(ensure_sentence)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| ensure_sentence(fallback)),
    );
    if let Some(support_sentence) = support_sentence.map(ensure_sentence) {
        if !parts.iter().any(|part| part == &support_sentence) {
            parts.push(support_sentence);
        }
    }
    parts.join(" ")
}

fn ensure_sentence(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if matches!(trimmed.chars().last(), Some('.' | '!' | '?')) {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn classify_summary_confidence(
    support_count: i32,
    metadata: &serde_json::Value,
    summary_fragment_count: usize,
) -> (String, Option<String>) {
    let has_partial_support =
        metadata.get("has_partial_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let has_failed_support =
        metadata.get("has_failed_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let has_recovered_support =
        metadata.get("has_recovered_support").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let has_conflict = summary_fragment_count > 1;

    let status = if has_conflict {
        GraphSummaryConfidenceStatus::Conflicted
    } else if has_failed_support {
        GraphSummaryConfidenceStatus::Weak
    } else if has_partial_support || has_recovered_support || support_count == 2 {
        GraphSummaryConfidenceStatus::Partial
    } else if support_count >= 3 {
        GraphSummaryConfidenceStatus::Strong
    } else {
        GraphSummaryConfidenceStatus::Weak
    };

    let warning = if has_conflict {
        Some("Active evidence still carries differing summary fragments.".to_string())
    } else if has_failed_support {
        Some(
            "Some supporting extraction attempts failed, so the summary is conservative."
                .to_string(),
        )
    } else if has_partial_support || has_recovered_support {
        Some("Summary includes recovered or partial extraction support.".to_string())
    } else if support_count <= 1 {
        Some("Summary is supported by a single active evidence line.".to_string())
    } else {
        None
    };

    (
        serde_json::to_string(&status)
            .unwrap_or_else(|_| "\"weak\"".to_string())
            .trim_matches('"')
            .to_string(),
        warning,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[test]
    fn targeted_refresh_requires_targets() {
        assert!(!GraphSummaryRefreshRequest::targeted(Vec::new(), Vec::new()).is_targeted());
        assert!(GraphSummaryRefreshRequest::targeted(vec![Uuid::nil()], Vec::new()).is_targeted());
    }

    #[test]
    fn refresh_is_inactive_without_source_truth_version() {
        assert!(!GraphSummaryRefreshRequest::broad().is_active());
        assert!(GraphSummaryRefreshRequest::broad().with_source_truth_version(7).is_active());
    }

    #[test]
    fn broad_refresh_is_used_for_non_targeted_mutation_scope() {
        assert!(
            GraphSummaryRefreshRequest::for_mutation_scope("fallback_broad", vec![], vec![])
                .broad_refresh
        );
        assert!(
            GraphSummaryRefreshRequest::for_mutation_scope("pending", vec![], vec![]).broad_refresh
        );
    }

    #[test]
    fn classify_summary_confidence_marks_conflicts() {
        let (status, warning) = classify_summary_confidence(3, &serde_json::json!({}), 2);

        assert_eq!(status, "conflicted");
        assert!(warning.is_some());
    }

    #[test]
    fn build_node_summary_includes_support_sentence() {
        let node = RuntimeGraphNodeRow {
            id: Uuid::nil(),
            library_id: Uuid::nil(),
            canonical_key: "entity:budget".to_string(),
            label: "Budget".to_string(),
            node_type: "entity".to_string(),
            aliases_json: serde_json::json!(["Budget"]),
            summary: Some("Budget approval moved to Q2".to_string()),
            metadata_json: serde_json::json!({}),
            support_count: 3,
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let (summary, status, warning) = build_node_summary(&node, &[]);

        assert!(summary.contains("Budget approval moved to Q2."));
        assert!(summary.contains("Supported by 3 active evidence lines."));
        assert_eq!(status, "strong");
        assert!(warning.is_none());
    }

    #[test]
    fn build_edge_summary_falls_back_to_relationship_sentence() {
        let edge = RuntimeGraphEdgeRow {
            id: Uuid::nil(),
            library_id: Uuid::nil(),
            from_node_id: Uuid::nil(),
            to_node_id: Uuid::now_v7(),
            relation_type: "reports_to".to_string(),
            canonical_key: "entity:a--reports_to--entity:b".to_string(),
            summary: None,
            weight: None,
            support_count: 1,
            metadata_json: serde_json::json!({}),
            projection_version: 1,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        let (summary, status, warning) = build_edge_summary(&edge, "Alex", "Sam", &[]);

        assert!(summary.contains("Alex reports to Sam."));
        assert_eq!(status, "weak");
        assert!(warning.is_some());
    }
}
