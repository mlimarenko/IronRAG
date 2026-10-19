use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::{
        knowledge_rows::{KnowledgeEntityCandidateRow, KnowledgeRelationCandidateRow},
        repositories,
    },
    services::graph::service::{
        EntityEvidenceContext, GraphRebuildOutcome, GraphService, RelationEvidenceContext,
        deterministic_entity_id,
    },
    services::query::vector_dimensions::invalidate_library_embedding_profile_inventory,
};

use super::candidate_reconciliation::{ReconcileSupport, reconcile_candidate_collections};
use super::reconciliation_plan::{EntityReconciliationPlan, RelationReconciliationPlan};

/// The entity and relation reconcile plans applied by one persist pass.
///
/// Grouping the two plans keeps `persist_reconciliation_plan` within the
/// argument budget without changing the pass order.
struct ReconciliationPlans {
    entity: EntityReconciliationPlan,
    relation: RelationReconciliationPlan,
}

impl GraphService {
    pub(in crate::services::graph::service) async fn reconcile_library_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<GraphRebuildOutcome> {
        let entity_candidates = state
            .graph_store
            .list_entity_candidates_by_library(library_id)
            .await
            .context("failed to load knowledge entity candidates")?;
        let relation_candidates = state
            .graph_store
            .list_relation_candidates_by_library(library_id)
            .await
            .context("failed to load knowledge relation candidates")?;
        self.reconcile_candidates(
            state,
            library_id,
            entity_candidates,
            relation_candidates,
            alias_overrides,
        )
        .await
    }

    async fn reconcile_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        entity_candidates: Vec<KnowledgeEntityCandidateRow>,
        relation_candidates: Vec<KnowledgeRelationCandidateRow>,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<GraphRebuildOutcome> {
        let reconciled = reconcile_candidate_collections(entity_candidates, relation_candidates);
        let support =
            self.load_reconcile_support(state, &reconciled.entities, &reconciled.relations).await?;
        let mut outcome = GraphRebuildOutcome {
            scanned_entity_candidates: reconciled.entities.len(),
            scanned_relation_candidates: reconciled.relations.len(),
            ..Default::default()
        };
        self.upsert_reconcile_source_edges(state, &support, &mut outcome).await?;
        let entity_plan = self.build_entity_reconciliation_plan(
            library_id,
            reconciled.entities,
            &support.revision_contexts,
            alias_overrides,
        )?;
        let relation_plan = self.build_relation_reconciliation_plan(
            library_id,
            reconciled.relations,
            &support.revision_contexts,
            &entity_plan.request_ids,
        )?;
        self.persist_reconciliation_plan(
            state,
            library_id,
            ReconciliationPlans { entity: entity_plan, relation: relation_plan },
            &support,
            &mut outcome,
        )
        .await?;
        Ok(outcome)
    }

    async fn persist_reconciliation_plan(
        &self,
        state: &AppState,
        library_id: Uuid,
        plans: ReconciliationPlans,
        support: &ReconcileSupport,
        outcome: &mut GraphRebuildOutcome,
    ) -> Result<()> {
        let ReconciliationPlans { entity: entity_plan, relation: relation_plan } = plans;
        let mut entity_requests = entity_plan.requests;
        entity_requests.extend(relation_plan.placeholder_requests.into_values());
        self.reset_library_materialization(state, library_id).await?;
        let entity_rows = state.graph_store.upsert_entities(&entity_requests).await?;
        let entity_by_id =
            entity_rows.into_iter().map(|row| (row.entity_id, row)).collect::<BTreeMap<_, _>>();
        for group in entity_plan.groups {
            let entity = entity_by_id.get(&group.entity_id).ok_or_else(|| {
                anyhow::anyhow!("missing normalized entity {} after bulk upsert", group.entity_id)
            })?;
            outcome.upserted_entities += 1;
            for candidate in group.candidates {
                let supporting_chunk =
                    candidate.chunk_id.and_then(|chunk_id| support.chunk_rows_by_id.get(&chunk_id));
                let revision_facts = support
                    .typed_facts_by_revision
                    .get(&candidate.revision_id)
                    .map_or_else(|| &[][..], Vec::as_slice);
                self.upsert_current_entity_evidence(
                    state,
                    &group.revision_context,
                    &candidate,
                    entity,
                    EntityEvidenceContext {
                        identity_key: &group.normalization_key,
                        supporting_chunk,
                        revision_facts,
                    },
                )
                .await?;
                outcome.upserted_evidence += 1;
                outcome.upserted_evidence_source_edges += 1;
                outcome.upserted_evidence_support_entity_edges += 1;
                if candidate.chunk_id.is_some() {
                    outcome.upserted_revision_chunk_edges += 0;
                    outcome.upserted_chunk_entity_edges += 1;
                }
            }
        }
        let relation_rows = state.graph_store.upsert_relations(&relation_plan.requests).await?;
        let relation_by_id =
            relation_rows.into_iter().map(|row| (row.relation_id, row)).collect::<BTreeMap<_, _>>();
        for group in relation_plan.groups {
            let relation = relation_by_id.get(&group.relation_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "missing normalized relation {} after bulk upsert",
                    group.relation_id
                )
            })?;
            outcome.upserted_relations += 1;
            for candidate in group.candidates {
                let subject_id =
                    deterministic_entity_id(library_id, &candidate.subject_candidate_key);
                let object_id =
                    deterministic_entity_id(library_id, &candidate.object_candidate_key);
                let subject = entity_by_id.get(&subject_id).ok_or_else(|| {
                    anyhow::anyhow!("missing subject placeholder entity {subject_id}")
                })?;
                let object = entity_by_id.get(&object_id).ok_or_else(|| {
                    anyhow::anyhow!("missing object placeholder entity {object_id}")
                })?;
                self.upsert_relation_edges(state, relation, subject, object).await?;
                let supporting_chunk = candidate
                    .row
                    .chunk_id
                    .and_then(|chunk_id| support.chunk_rows_by_id.get(&chunk_id));
                let revision_facts = support
                    .typed_facts_by_revision
                    .get(&candidate.row.revision_id)
                    .map_or_else(|| &[][..], Vec::as_slice);
                self.upsert_current_relation_evidence(
                    state,
                    &group.revision_context,
                    &candidate,
                    relation,
                    RelationEvidenceContext { supporting_chunk, revision_facts },
                )
                .await?;
                outcome.upserted_evidence += 1;
                outcome.upserted_relation_subject_edges += 1;
                outcome.upserted_relation_object_edges += 1;
                outcome.upserted_evidence_source_edges += 1;
                outcome.upserted_evidence_support_relation_edges += 1;
            }
        }
        Ok(())
    }

    pub(in crate::services::graph::service) async fn build_and_refresh_graph_from_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<GraphRebuildOutcome> {
        self.reconcile_library_candidates(state, library_id, alias_overrides).await
    }

    async fn reset_library_materialization(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<()> {
        let _vector_guard = state
            .application_services
            .search
            .vector_plane_write_guard(state, library_id)
            .await
            .context("failed to acquire graph materialization reset guard")?;
        let expected_source_truth_version =
            repositories::get_library_source_truth_version(&state.persistence.postgres, library_id)
                .await
                .context("failed to capture graph materialization reset fence")?;
        Box::pin(
            crate::infra::repositories::knowledge_graph_projection_repository::reset_library_materialized_graph_and_vectors(
                &state.persistence.postgres,
                library_id,
                expected_source_truth_version,
            ),
        )
        .await
        .context("failed to reset graph sources and entity vectors atomically")?;
        invalidate_library_embedding_profile_inventory(library_id);
        Ok(())
    }
}
