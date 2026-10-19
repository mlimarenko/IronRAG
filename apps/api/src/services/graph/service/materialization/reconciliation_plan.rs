use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use crate::{
    infra::knowledge_rows::{
        KnowledgeEntityCandidateRow, NewKnowledgeEntity, NewKnowledgeRelation,
    },
    services::graph::service::{
        GraphRevisionContext, GraphService, ReconciledEntityCandidate, ReconciledRelationCandidate,
        deterministic_entity_id, deterministic_relation_id, placeholder_entity_parts_from_key,
        select_primary_entity_label,
    },
};

fn select_materialized_relation_predicate<'a>(
    predicates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    predicates.into_iter().find_map(|predicate| {
        let predicate = crate::services::graph::identity::normalize_relation_type(predicate);
        (!predicate.is_empty()).then_some(predicate)
    })
}

pub(super) struct EntityReconcileGroup {
    pub(super) normalization_key: String,
    pub(super) revision_context: GraphRevisionContext,
    pub(super) candidates: Vec<KnowledgeEntityCandidateRow>,
    pub(super) entity_id: Uuid,
}

pub(super) struct RelationReconcileGroup {
    pub(super) revision_context: GraphRevisionContext,
    pub(super) candidates: Vec<ReconciledRelationCandidate>,
    pub(super) relation_id: Uuid,
}

pub(super) struct EntityReconciliationPlan {
    pub(super) groups: Vec<EntityReconcileGroup>,
    pub(super) requests: Vec<NewKnowledgeEntity>,
    pub(super) request_ids: BTreeSet<Uuid>,
}

pub(super) struct RelationReconciliationPlan {
    pub(super) groups: Vec<RelationReconcileGroup>,
    pub(super) requests: Vec<NewKnowledgeRelation>,
    pub(super) placeholder_requests: BTreeMap<Uuid, NewKnowledgeEntity>,
}

impl GraphService {
    pub(super) fn build_entity_reconciliation_plan(
        &self,
        library_id: Uuid,
        entity_candidates: Vec<ReconciledEntityCandidate>,
        revision_contexts: &BTreeMap<Uuid, GraphRevisionContext>,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<EntityReconciliationPlan> {
        let mut entity_groups = BTreeMap::<String, Vec<ReconciledEntityCandidate>>::new();
        for candidate in entity_candidates {
            entity_groups.entry(candidate.normalization_key.clone()).or_default().push(candidate);
        }
        let mut groups = Vec::<EntityReconcileGroup>::new();
        let mut requests = Vec::<NewKnowledgeEntity>::new();
        let mut request_ids = BTreeSet::<Uuid>::new();
        for (normalization_key, rows) in entity_groups {
            let row = rows
                .last()
                .ok_or_else(|| anyhow::anyhow!("entity candidate group is unexpectedly empty"))?;
            let revision_context =
                revision_contexts.get(&row.row.revision_id).ok_or_else(|| {
                    anyhow::anyhow!("missing revision context for {}", row.row.revision_id)
                })?;
            let primary_label = select_primary_entity_label(&rows, &normalization_key)
                .unwrap_or_else(|| {
                    placeholder_entity_parts_from_key(&normalization_key)
                        .map(|(_, primary_label)| primary_label)
                        .unwrap_or_default()
                });
            let entity_type = rows
                .iter()
                .find_map(|candidate| {
                    (!candidate.row.candidate_type.trim().is_empty())
                        .then(|| candidate.row.candidate_type.trim().to_string())
                })
                .unwrap_or_else(|| {
                    crate::services::graph::identity::runtime_node_type_slug(
                        &crate::services::graph::identity::runtime_node_type_from_key(
                            &normalization_key,
                        ),
                    )
                    .to_string()
                });
            let entity_sub_type = rows.iter().find_map(|candidate| {
                candidate
                    .row
                    .candidate_sub_type
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| value.trim().to_string())
            });
            let alias_rows = rows.iter().map(|candidate| candidate.row.clone()).collect::<Vec<_>>();
            let aliases = self.collect_entity_aliases(
                &alias_rows,
                alias_overrides,
                &normalization_key,
                &primary_label,
            );
            let confidence = rows
                .iter()
                .filter_map(|candidate| candidate.row.confidence)
                .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
            let entity_id = deterministic_entity_id(library_id, &normalization_key);
            request_ids.insert(entity_id);
            requests.push(NewKnowledgeEntity {
                entity_id,
                workspace_id: revision_context.workspace_id,
                library_id,
                primary_label,
                aliases: aliases.into_iter().collect(),
                entity_type,
                entity_sub_type,
                summary: None,
                confidence,
                support_count: rows.len() as i64,
                freshness_generation: revision_context.revision_number,
                entity_state: "active".to_string(),
                created_at: None,
                updated_at: Some(Utc::now()),
            });
            groups.push(EntityReconcileGroup {
                normalization_key,
                revision_context: revision_context.clone(),
                candidates: rows.into_iter().map(|candidate| candidate.row).collect(),
                entity_id,
            });
        }
        Ok(EntityReconciliationPlan { groups, requests, request_ids })
    }

    pub(super) fn build_relation_reconciliation_plan(
        &self,
        library_id: Uuid,
        relation_candidates: Vec<ReconciledRelationCandidate>,
        revision_contexts: &BTreeMap<Uuid, GraphRevisionContext>,
        entity_request_ids: &BTreeSet<Uuid>,
    ) -> Result<RelationReconciliationPlan> {
        let mut relation_groups = BTreeMap::<String, Vec<ReconciledRelationCandidate>>::new();
        for candidate in relation_candidates {
            relation_groups
                .entry(candidate.normalized_assertion.clone())
                .or_default()
                .push(candidate);
        }

        let mut groups = Vec::<RelationReconcileGroup>::new();
        let mut requests = Vec::<NewKnowledgeRelation>::new();
        let mut placeholder_requests = BTreeMap::<Uuid, NewKnowledgeEntity>::new();
        for (normalized_assertion, rows) in relation_groups {
            let row = rows
                .last()
                .ok_or_else(|| anyhow::anyhow!("relation candidate group is unexpectedly empty"))?;
            let revision_context =
                revision_contexts.get(&row.row.revision_id).ok_or_else(|| {
                    anyhow::anyhow!("missing revision context for {}", row.row.revision_id)
                })?;
            let Some(predicate) = select_materialized_relation_predicate(
                rows.iter().map(|candidate| candidate.predicate.as_str()),
            ) else {
                tracing::warn!(
                    candidate_count = rows.len(),
                    "skipping relation candidate group without a structurally valid predicate"
                );
                continue;
            };
            let confidence = rows
                .iter()
                .filter_map(|candidate| candidate.row.confidence)
                .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
            let relation_id = deterministic_relation_id(library_id, &normalized_assertion);
            requests.push(NewKnowledgeRelation {
                relation_id,
                workspace_id: revision_context.workspace_id,
                library_id,
                predicate,
                normalized_assertion,
                confidence,
                support_count: rows.len() as i64,
                contradiction_state: "unknown".to_string(),
                freshness_generation: revision_context.revision_number,
                relation_state: "active".to_string(),
                created_at: None,
                updated_at: Some(Utc::now()),
            });
            self.collect_placeholder_entity_requests(
                library_id,
                &rows,
                revision_context,
                entity_request_ids,
                &mut placeholder_requests,
            );
            groups.push(RelationReconcileGroup {
                revision_context: revision_context.clone(),
                candidates: rows,
                relation_id,
            });
        }
        Ok(RelationReconciliationPlan { groups, requests, placeholder_requests })
    }

    fn collect_placeholder_entity_requests(
        &self,
        library_id: Uuid,
        candidates: &[ReconciledRelationCandidate],
        revision_context: &GraphRevisionContext,
        entity_request_ids: &BTreeSet<Uuid>,
        requests: &mut BTreeMap<Uuid, NewKnowledgeEntity>,
    ) {
        for normalization_key in candidates.iter().flat_map(|candidate| {
            [&candidate.subject_candidate_key, &candidate.object_candidate_key]
        }) {
            let Some((node_type, primary_label)) =
                placeholder_entity_parts_from_key(normalization_key)
            else {
                continue;
            };
            let entity_id = deterministic_entity_id(library_id, normalization_key);
            if entity_request_ids.contains(&entity_id) {
                continue;
            }
            let entry = requests.entry(entity_id).or_insert_with(|| NewKnowledgeEntity {
                entity_id,
                workspace_id: revision_context.workspace_id,
                library_id,
                primary_label: primary_label.clone(),
                aliases: vec![primary_label],
                entity_type: crate::services::graph::identity::runtime_node_type_slug(&node_type)
                    .to_string(),
                entity_sub_type: None,
                summary: None,
                confidence: None,
                support_count: 0,
                freshness_generation: revision_context.revision_number,
                entity_state: "active".to_string(),
                created_at: None,
                updated_at: Some(Utc::now()),
            });
            entry.support_count += 1;
            entry.freshness_generation =
                entry.freshness_generation.max(revision_context.revision_number);
            entry.updated_at = Some(Utc::now());
        }
    }

    fn collect_entity_aliases(
        &self,
        rows: &[KnowledgeEntityCandidateRow],
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
        normalization_key: &str,
        primary_label: &str,
    ) -> BTreeSet<String> {
        let mut aliases = BTreeSet::<String>::new();
        if !primary_label.trim().is_empty() {
            aliases.insert(primary_label.trim().to_string());
        }
        for row in rows {
            if !row.candidate_label.trim().is_empty() {
                aliases.insert(row.candidate_label.trim().to_string());
            }
        }
        if let Some(overrides) = alias_overrides
            && let Some(values) = overrides.get(normalization_key)
        {
            aliases.extend(values.iter().cloned());
        }
        aliases
    }
}

#[cfg(test)]
mod tests {
    use super::select_materialized_relation_predicate;

    #[test]
    fn relation_predicate_selection_abstains_when_typed_predicate_is_absent() {
        assert_eq!(select_materialized_relation_predicate(["", "   "]), None);
    }

    #[test]
    fn relation_predicate_selection_accepts_arbitrary_structural_predicate() {
        assert_eq!(
            select_materialized_relation_predicate(["", " opaque_predicate_7 "]),
            Some("opaque_predicate_7".to_string())
        );
    }

    #[test]
    fn relation_predicate_selection_abstains_on_invalid_predicates() {
        assert_eq!(select_materialized_relation_predicate(["Invalid Predicate"]), None);
    }
}
