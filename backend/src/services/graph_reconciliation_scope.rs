use std::collections::BTreeSet;

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, RuntimeGraphEvidenceLifecycleRow},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationImpactScopeDetection {
    pub scope_status: String,
    pub confidence_status: String,
    pub affected_node_ids: Vec<Uuid>,
    pub affected_relationship_ids: Vec<Uuid>,
    pub fallback_reason: Option<String>,
}

impl MutationImpactScopeDetection {
    #[must_use]
    pub fn affected_node_ids_json(&self) -> serde_json::Value {
        serde_json::json!(self.affected_node_ids)
    }

    #[must_use]
    pub fn affected_relationship_ids_json(&self) -> serde_json::Value {
        serde_json::json!(self.affected_relationship_ids)
    }

    #[must_use]
    pub fn targeted(
        confidence_status: &str,
        affected_node_ids: Vec<Uuid>,
        affected_relationship_ids: Vec<Uuid>,
    ) -> Self {
        Self {
            scope_status: "targeted".to_string(),
            confidence_status: confidence_status.to_string(),
            affected_node_ids,
            affected_relationship_ids,
            fallback_reason: None,
        }
    }

    #[must_use]
    pub fn fallback_broad(
        confidence_status: &str,
        affected_node_ids: Vec<Uuid>,
        affected_relationship_ids: Vec<Uuid>,
        fallback_reason: impl Into<String>,
    ) -> Self {
        Self {
            scope_status: "fallback_broad".to_string(),
            confidence_status: confidence_status.to_string(),
            affected_node_ids,
            affected_relationship_ids,
            fallback_reason: Some(fallback_reason.into()),
        }
    }

    #[must_use]
    pub fn pending() -> Self {
        Self {
            scope_status: "pending".to_string(),
            confidence_status: "low".to_string(),
            affected_node_ids: Vec::new(),
            affected_relationship_ids: Vec::new(),
            fallback_reason: None,
        }
    }

    #[must_use]
    pub fn is_high_confidence_targeted(&self) -> bool {
        self.scope_status == "targeted" && self.confidence_status == "high"
    }
}

#[derive(Debug, Clone, Default)]
pub struct GraphReconciliationScopeService;

impl GraphReconciliationScopeService {
    pub async fn detect_revision_mutation_scope(
        &self,
        state: &AppState,
        project_id: Uuid,
        document_id: Uuid,
        source_revision_id: Uuid,
        target_revision_id: Uuid,
    ) -> anyhow::Result<MutationImpactScopeDetection> {
        let source_evidence =
            repositories::list_active_runtime_graph_evidence_by_document_revision(
                &state.persistence.postgres,
                project_id,
                document_id,
                source_revision_id,
            )
            .await
            .context(
                "failed to load source revision graph evidence while detecting mutation scope",
            )?;
        let target_evidence =
            repositories::list_active_runtime_graph_evidence_by_document_revision(
                &state.persistence.postgres,
                project_id,
                document_id,
                target_revision_id,
            )
            .await
            .context(
                "failed to load target revision graph evidence while detecting mutation scope",
            )?;

        let mut affected_node_ids = collect_target_ids(&source_evidence, "node");
        affected_node_ids.extend(collect_target_ids(&target_evidence, "node"));
        let mut affected_relationship_ids = collect_target_ids(&source_evidence, "edge");
        affected_relationship_ids.extend(collect_target_ids(&target_evidence, "edge"));
        let affected_node_ids = dedupe_ids(affected_node_ids);
        let affected_relationship_ids = dedupe_ids(affected_relationship_ids);

        Ok(self.finalize_detection(
            state,
            affected_node_ids,
            affected_relationship_ids,
            revision_scope_confidence(&source_evidence, &target_evidence),
            if source_evidence.is_empty() && target_evidence.is_empty() {
                Some(
                    "No old-or-new revision evidence was available, so the graph refresh must stay broad."
                        .to_string(),
                )
            } else if target_evidence.is_empty() {
                Some(
                    "The new revision has not admitted graph evidence yet, so a safe targeted refresh could not be confirmed."
                        .to_string(),
                )
            } else {
                None
            },
        ))
    }

    pub async fn detect_delete_scope(
        &self,
        state: &AppState,
        project_id: Uuid,
        document_id: Uuid,
        source_revision_id: Uuid,
    ) -> anyhow::Result<MutationImpactScopeDetection> {
        let source_evidence =
            repositories::list_active_runtime_graph_evidence_by_document_revision(
                &state.persistence.postgres,
                project_id,
                document_id,
                source_revision_id,
            )
            .await
            .context(
                "failed to load source revision graph evidence while detecting delete scope",
            )?;
        let affected_node_ids = collect_target_ids(&source_evidence, "node");
        let affected_relationship_ids = collect_target_ids(&source_evidence, "edge");
        let surviving_node_ids =
            repositories::list_active_runtime_graph_target_ids_excluding_document_revision(
                &state.persistence.postgres,
                project_id,
                document_id,
                source_revision_id,
                "node",
                &affected_node_ids,
            )
            .await
            .context("failed to inspect surviving node evidence while detecting delete scope")?;
        let surviving_edge_ids =
            repositories::list_active_runtime_graph_target_ids_excluding_document_revision(
                &state.persistence.postgres,
                project_id,
                document_id,
                source_revision_id,
                "edge",
                &affected_relationship_ids,
            )
            .await
            .context(
                "failed to inspect surviving relationship evidence while detecting delete scope",
            )?;

        Ok(self.finalize_detection(
            state,
            dedupe_ids(affected_node_ids),
            dedupe_ids(affected_relationship_ids),
            delete_scope_confidence(&source_evidence, &surviving_node_ids, &surviving_edge_ids),
            if source_evidence.is_empty() {
                Some(
                    "The deleted revision had no active graph evidence, so the reconciliation scope is not trustworthy enough for a narrow refresh."
                        .to_string(),
                )
            } else {
                None
            },
        ))
    }

    #[must_use]
    pub fn prefer_targeted_reconciliation(
        &self,
        affected_node_count: usize,
        affected_relationship_count: usize,
        enabled: bool,
        max_targets: usize,
    ) -> bool {
        enabled
            && affected_node_count + affected_relationship_count > 0
            && affected_node_count + affected_relationship_count <= max_targets
    }

    fn finalize_detection(
        &self,
        state: &AppState,
        affected_node_ids: Vec<Uuid>,
        affected_relationship_ids: Vec<Uuid>,
        confidence_status: &str,
        fallback_reason: Option<String>,
    ) -> MutationImpactScopeDetection {
        if self.prefer_targeted_reconciliation(
            affected_node_ids.len(),
            affected_relationship_ids.len(),
            state.retrieval_intelligence.targeted_reconciliation_enabled,
            state.retrieval_intelligence.targeted_reconciliation_max_targets,
        ) && fallback_reason.is_none()
        {
            return MutationImpactScopeDetection::targeted(
                confidence_status,
                affected_node_ids,
                affected_relationship_ids,
            );
        }

        let fallback_reason = fallback_reason.unwrap_or_else(|| {
            if !state.retrieval_intelligence.targeted_reconciliation_enabled {
                "Targeted graph reconciliation is disabled by runtime configuration.".to_string()
            } else {
                format!(
                    "The affected graph scope exceeds the safe targeted limit of {} targets, so reconciliation falls back to a broader refresh.",
                    state.retrieval_intelligence.targeted_reconciliation_max_targets
                )
            }
        });
        MutationImpactScopeDetection::fallback_broad(
            "low",
            affected_node_ids,
            affected_relationship_ids,
            fallback_reason,
        )
    }
}

pub async fn persist_detected_scope(
    state: &AppState,
    mutation_workflow_id: Uuid,
    detection: &MutationImpactScopeDetection,
) -> anyhow::Result<()> {
    repositories::update_document_mutation_impact_scope(
        &state.persistence.postgres,
        mutation_workflow_id,
        &detection.scope_status,
        &detection.confidence_status,
        detection.affected_node_ids_json(),
        detection.affected_relationship_ids_json(),
        detection.fallback_reason.as_deref(),
    )
    .await
    .context("failed to persist mutation impact scope")?;
    Ok(())
}

fn collect_target_ids(
    evidence_rows: &[RuntimeGraphEvidenceLifecycleRow],
    target_kind: &str,
) -> Vec<Uuid> {
    evidence_rows
        .iter()
        .filter(|row| row.target_kind == target_kind)
        .map(|row| row.target_id)
        .collect()
}

fn dedupe_ids(ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.into_iter().collect::<BTreeSet<_>>().into_iter().collect()
}

fn revision_scope_confidence(
    source_evidence: &[RuntimeGraphEvidenceLifecycleRow],
    target_evidence: &[RuntimeGraphEvidenceLifecycleRow],
) -> &'static str {
    if !source_evidence.is_empty() && !target_evidence.is_empty() {
        "high"
    } else if !source_evidence.is_empty() || !target_evidence.is_empty() {
        "medium"
    } else {
        "low"
    }
}

fn delete_scope_confidence(
    source_evidence: &[RuntimeGraphEvidenceLifecycleRow],
    surviving_node_ids: &[Uuid],
    surviving_edge_ids: &[Uuid],
) -> &'static str {
    if source_evidence.is_empty() {
        "low"
    } else if !surviving_node_ids.is_empty() || !surviving_edge_ids.is_empty() {
        "high"
    } else {
        "medium"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dedupes_ids_before_targeted_decision() {
        let node_id = Uuid::nil();
        let edge_id = Uuid::from_u128(7);
        let detection = MutationImpactScopeDetection::targeted(
            "high",
            dedupe_ids(vec![node_id, node_id]),
            dedupe_ids(vec![edge_id, edge_id]),
        );

        assert_eq!(detection.affected_node_ids, vec![node_id]);
        assert_eq!(detection.affected_relationship_ids, vec![edge_id]);
        assert!(detection.is_high_confidence_targeted());
    }

    #[test]
    fn pending_scope_starts_empty() {
        let pending = MutationImpactScopeDetection::pending();

        assert_eq!(pending.scope_status, "pending");
        assert!(pending.affected_node_ids.is_empty());
        assert!(pending.affected_relationship_ids.is_empty());
    }
}
