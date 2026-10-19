use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::TypedTechnicalFact,
    infra::knowledge_rows::{KnowledgeEntityCandidateRow, KnowledgeRelationCandidateRow},
    services::graph::service::{
        GraphRebuildOutcome, GraphRevisionContext, GraphService, ReconciledEntityCandidate,
        ReconciledRelationCandidate, apply_entity_key_aliases_to_relation_candidate,
        build_entity_candidate_key_index, build_prefixed_entity_key_aliases,
        reconcile_entity_candidate_row, reconcile_relation_candidate_row,
        relation_candidate_keys_are_materializable,
    },
};

pub(super) struct ReconciledCandidateCollections {
    pub(super) entities: Vec<ReconciledEntityCandidate>,
    pub(super) relations: Vec<ReconciledRelationCandidate>,
}

pub(super) struct ReconcileSupport {
    pub(super) revision_contexts: BTreeMap<Uuid, GraphRevisionContext>,
    pub(super) typed_facts_by_revision: BTreeMap<Uuid, Vec<TypedTechnicalFact>>,
    pub(super) chunk_rows_by_id: BTreeMap<Uuid, crate::infra::knowledge_rows::KnowledgeChunkRow>,
    pub(super) revision_chunk_ids: BTreeMap<Uuid, BTreeSet<Uuid>>,
}

pub(super) fn reconcile_candidate_collections(
    entity_candidates: Vec<KnowledgeEntityCandidateRow>,
    relation_candidates: Vec<KnowledgeRelationCandidateRow>,
) -> ReconciledCandidateCollections {
    let entity_key_index = build_entity_candidate_key_index(&entity_candidates);
    let entities = entity_candidates
        .into_iter()
        .filter_map(|row| reconcile_entity_candidate_row(row, &entity_key_index))
        .collect::<Vec<_>>();
    let relations = relation_candidates
        .into_iter()
        .filter_map(|row| reconcile_relation_candidate_row(row, &entity_key_index))
        .filter(|candidate| {
            relation_candidate_keys_are_materializable(
                &candidate.subject_candidate_key,
                &candidate.predicate,
                &candidate.object_candidate_key,
            )
        })
        .collect::<Vec<_>>();
    let entity_key_aliases = build_prefixed_entity_key_aliases(&entities);
    let entities = entities
        .into_iter()
        .map(|mut candidate| {
            if let Some(identity_key) = entity_key_aliases.get(&candidate.normalization_key) {
                candidate.normalization_key = identity_key.clone();
            }
            candidate
        })
        .collect();
    let relations = relations
        .into_iter()
        .map(|mut candidate| {
            apply_entity_key_aliases_to_relation_candidate(&mut candidate, &entity_key_aliases);
            candidate
        })
        .collect();
    ReconciledCandidateCollections { entities, relations }
}

fn revision_chunk_ids(
    entities: &[ReconciledEntityCandidate],
    relations: &[ReconciledRelationCandidate],
) -> BTreeMap<Uuid, BTreeSet<Uuid>> {
    let mut by_revision = BTreeMap::<Uuid, BTreeSet<Uuid>>::new();
    for revision_id_and_chunk in entities
        .iter()
        .filter_map(|candidate| {
            candidate.row.chunk_id.map(|chunk_id| (candidate.row.revision_id, chunk_id))
        })
        .chain(relations.iter().filter_map(|candidate| {
            candidate.row.chunk_id.map(|chunk_id| (candidate.row.revision_id, chunk_id))
        }))
    {
        by_revision.entry(revision_id_and_chunk.0).or_default().insert(revision_id_and_chunk.1);
    }
    by_revision
}

impl GraphService {
    pub(super) async fn load_reconcile_support(
        &self,
        state: &AppState,
        entity_candidates: &[ReconciledEntityCandidate],
        relation_candidates: &[ReconciledRelationCandidate],
    ) -> Result<ReconcileSupport> {
        let mut revision_contexts = BTreeMap::<Uuid, GraphRevisionContext>::new();
        for revision_id in entity_candidates
            .iter()
            .map(|candidate| candidate.row.revision_id)
            .chain(relation_candidates.iter().map(|candidate| candidate.row.revision_id))
        {
            if let Some(revision) = state
                .document_store
                .get_revision(revision_id)
                .await
                .context("failed to load revision for knowledge graph reconciliation")?
            {
                revision_contexts.insert(revision_id, GraphRevisionContext::from(revision));
            }
        }
        let mut typed_facts_by_revision = BTreeMap::<Uuid, Vec<TypedTechnicalFact>>::new();
        for revision_id in revision_contexts.keys().copied() {
            let typed_facts = state
                .application_services
                .knowledge
                .list_typed_technical_facts(state, revision_id)
                .await
                .with_context(|| {
                    format!(
                        "failed to load typed technical facts for knowledge graph reconciliation revision {revision_id}"
                    )
                })?;
            typed_facts_by_revision.insert(revision_id, typed_facts);
        }
        let revision_chunk_ids = revision_chunk_ids(entity_candidates, relation_candidates);
        let chunk_ids = revision_chunk_ids
            .values()
            .flat_map(|chunk_ids| chunk_ids.iter().copied())
            .collect::<BTreeSet<_>>();
        let mut chunk_rows_by_id =
            BTreeMap::<Uuid, crate::infra::knowledge_rows::KnowledgeChunkRow>::new();
        for chunk_id in chunk_ids {
            if let Some(chunk) =
                state.document_store.get_chunk(chunk_id).await.with_context(|| {
                    format!("failed to load chunk {chunk_id} for knowledge graph reconciliation")
                })?
            {
                chunk_rows_by_id.insert(chunk_id, chunk);
            }
        }
        Ok(ReconcileSupport {
            revision_contexts,
            typed_facts_by_revision,
            chunk_rows_by_id,
            revision_chunk_ids,
        })
    }

    pub(super) async fn upsert_reconcile_source_edges(
        &self,
        state: &AppState,
        support: &ReconcileSupport,
        outcome: &mut GraphRebuildOutcome,
    ) -> Result<()> {
        for (revision_id, revision_context) in &support.revision_contexts {
            self.upsert_revision_edges(state, revision_context).await?;
            outcome.upserted_document_revision_edges += 1;
            if let Some(chunk_ids) = support.revision_chunk_ids.get(revision_id) {
                for chunk_id in chunk_ids {
                    self.upsert_chunk_edge(state, revision_context, *chunk_id).await?;
                    outcome.upserted_revision_chunk_edges += 1;
                }
            }
        }
        Ok(())
    }
}
