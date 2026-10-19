use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::knowledge_rows::{NewKnowledgeEntityCandidate, NewKnowledgeRelationCandidate},
    services::graph::extract::{
        GraphEntityCandidate, GraphExtractionCandidateSet, GraphRelationCandidate,
    },
    services::graph::service::{
        EntityEvidenceContext, GraphRevisionContext, GraphService, RelationCandidateIdentity,
        RelationEvidenceContext, UpsertGraphEntityInput, UpsertGraphRelationInput,
        build_relation_entity_key_index, deterministic_entity_candidate_id,
        deterministic_relation_candidate_id, normalized_relation_assertion_from_keys,
        relation_fields_are_semantically_empty,
    },
};

/// Borrowed inputs shared by one chunk's candidate materialization pass.
///
/// The per-item helpers read the same chunk, revision and preloaded index so
/// the loops stay flat without re-threading the many same-typed arguments.
struct ChunkMaterializationContext<'a> {
    state: &'a AppState,
    revision: &'a crate::infra::knowledge_rows::KnowledgeRevisionRow,
    revision_context: &'a GraphRevisionContext,
    chunk_id: Uuid,
    entity_key_index: &'a crate::services::graph::identity::GraphLabelNodeTypeIndex,
    entity_alias_overrides: &'a BTreeMap<String, BTreeSet<String>>,
    chunk_row: Option<&'a crate::infra::knowledge_rows::KnowledgeChunkRow>,
    revision_facts: &'a [crate::domains::knowledge::TypedTechnicalFact],
    mark_existing_only: bool,
}

impl GraphService {
    pub(in crate::services::graph::service) async fn materialize_current_candidate_batch(
        &self,
        state: &AppState,
        revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
        chunk_id: Uuid,
        candidates: &GraphExtractionCandidateSet,
        mark_existing_only: bool,
    ) -> Result<()> {
        let revision_context = GraphRevisionContext::from(revision.clone());
        let entity_key_index = build_relation_entity_key_index(candidates);
        let entity_alias_overrides = self.build_alias_overrides(candidates, &entity_key_index);
        let chunk_row = state.document_store.get_chunk(chunk_id).await.with_context(|| {
            format!("failed to load chunk {chunk_id} for graph materialization")
        })?;
        let revision_facts = state
            .application_services
            .knowledge
            .list_typed_technical_facts(state, revision.revision_id)
            .await
            .with_context(|| {
                format!(
                    "failed to load typed technical facts for graph materialization revision {}",
                    revision.revision_id
                )
            })?;
        self.upsert_revision_edges(state, &revision_context).await?;
        self.upsert_chunk_edge(state, &revision_context, chunk_id).await?;

        let context = ChunkMaterializationContext {
            state,
            revision,
            revision_context: &revision_context,
            chunk_id,
            entity_key_index: &entity_key_index,
            entity_alias_overrides: &entity_alias_overrides,
            chunk_row: chunk_row.as_ref(),
            revision_facts: revision_facts.as_slice(),
            mark_existing_only,
        };
        for entity in &candidates.entities {
            self.materialize_entity_candidate(&context, entity).await?;
        }
        for relation in &candidates.relations {
            self.materialize_relation_candidate(&context, relation).await?;
        }

        Ok(())
    }

    async fn materialize_entity_candidate(
        &self,
        context: &ChunkMaterializationContext<'_>,
        entity: &GraphEntityCandidate,
    ) -> Result<()> {
        let candidate = self.build_entity_candidate_row(
            context.revision,
            context.chunk_id,
            entity,
            context.entity_key_index,
        );
        let candidate_row = context
            .state
            .graph_store
            .upsert_entity_candidate(&candidate)
            .await
            .context("failed to upsert knowledge entity candidate")?;
        if context.mark_existing_only {
            return Ok(());
        }
        let entity_row = self
            .upsert_graph_entity(
                context.state,
                UpsertGraphEntityInput {
                    library_id: context.revision.library_id,
                    workspace_id: context.revision.workspace_id,
                    normalization_key: &candidate.normalization_key,
                    primary_label: candidate.candidate_label.trim(),
                    entity_type: &candidate.candidate_type,
                    aliases: context
                        .entity_alias_overrides
                        .get(&candidate.normalization_key)
                        .cloned()
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                    confidence: candidate.confidence,
                    support_count: 1,
                    freshness_generation: context.revision.revision_number,
                },
            )
            .await?;
        self.upsert_current_entity_evidence(
            context.state,
            context.revision_context,
            &candidate_row,
            &entity_row,
            EntityEvidenceContext {
                identity_key: &candidate_row.normalization_key,
                supporting_chunk: context.chunk_row,
                revision_facts: context.revision_facts,
            },
        )
        .await?;
        self.upsert_chunk_mentions_entity_edge(
            context.state,
            context.chunk_id,
            entity_row.entity_id,
            candidate_row.confidence,
            context.revision.library_id,
        )
        .await?;
        Ok(())
    }

    async fn materialize_relation_candidate(
        &self,
        context: &ChunkMaterializationContext<'_>,
        relation: &GraphRelationCandidate,
    ) -> Result<()> {
        if relation_fields_are_semantically_empty(
            &relation.source_label,
            &relation.relation_type,
            &relation.target_label,
        ) {
            return Ok(());
        }
        let candidate = self.build_relation_candidate_row(
            context.revision,
            context.chunk_id,
            relation,
            context.entity_key_index,
        );
        let candidate_row = context
            .state
            .graph_store
            .upsert_relation_candidate(&candidate)
            .await
            .context("failed to upsert knowledge relation candidate")?;
        if context.mark_existing_only {
            return Ok(());
        }
        let relation_row = self
            .upsert_graph_relation(
                context.state,
                UpsertGraphRelationInput {
                    library_id: context.revision.library_id,
                    workspace_id: context.revision.workspace_id,
                    normalized_assertion: &candidate.normalized_assertion,
                    predicate: candidate.predicate.trim(),
                    confidence: candidate.confidence,
                    support_count: 1,
                    freshness_generation: context.revision.revision_number,
                },
            )
            .await?;
        let subject = self
            .upsert_placeholder_entity_for_key(
                context.state,
                context.revision.library_id,
                context.revision.workspace_id,
                &candidate.subject_candidate_key,
            )
            .await?;
        let object = self
            .upsert_placeholder_entity_for_key(
                context.state,
                context.revision.library_id,
                context.revision.workspace_id,
                &candidate.object_candidate_key,
            )
            .await?;
        self.upsert_relation_edges(context.state, &relation_row, &subject, &object).await?;
        self.upsert_current_relation_evidence(
            context.state,
            context.revision_context,
            &candidate_row,
            &relation_row,
            RelationEvidenceContext {
                supporting_chunk: context.chunk_row,
                revision_facts: context.revision_facts,
            },
        )
        .await?;
        Ok(())
    }

    fn build_alias_overrides(
        &self,
        candidates: &GraphExtractionCandidateSet,
        entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut overrides = BTreeMap::<String, BTreeSet<String>>::new();
        for entity in &candidates.entities {
            let key = entity_key_index.node_identity_key_for_label(&entity.label);
            let aliases = overrides.entry(key).or_default();
            aliases.insert(entity.label.trim().to_string());
            for alias in &entity.aliases {
                let trimmed = alias.trim();
                if !trimmed.is_empty() {
                    aliases.insert(trimmed.to_string());
                }
            }
        }
        overrides
    }

    fn build_entity_candidate_row(
        &self,
        revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
        chunk_id: Uuid,
        entity: &GraphEntityCandidate,
        entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    ) -> NewKnowledgeEntityCandidate {
        let normalization_key = entity_key_index.node_identity_key_for_label(&entity.label);
        let node_type_from_identity_key =
            crate::services::graph::identity::runtime_node_type_from_key(&normalization_key);
        let candidate_id = deterministic_entity_candidate_id(
            revision.library_id,
            revision.revision_id,
            chunk_id,
            &normalization_key,
            &entity.label,
            &node_type_from_identity_key,
        );
        NewKnowledgeEntityCandidate {
            candidate_id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: Some(chunk_id),
            candidate_label: entity.label.trim().to_string(),
            candidate_type: crate::services::graph::identity::runtime_node_type_slug(
                &node_type_from_identity_key,
            )
            .to_string(),
            candidate_sub_type: entity.sub_type.clone(),
            normalization_key,
            confidence: None,
            extraction_method: "graph_extract".to_string(),
            candidate_state: "active".to_string(),
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }
    }

    fn build_relation_candidate_row(
        &self,
        revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
        chunk_id: Uuid,
        relation: &GraphRelationCandidate,
        entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    ) -> NewKnowledgeRelationCandidate {
        let subject_candidate_key =
            entity_key_index.node_identity_key_for_label(&relation.source_label);
        let object_candidate_key =
            entity_key_index.node_identity_key_for_label(&relation.target_label);
        let normalized_assertion = normalized_relation_assertion_from_keys(
            &subject_candidate_key,
            &relation.relation_type,
            &object_candidate_key,
        );
        let candidate_id = deterministic_relation_candidate_id(
            revision.library_id,
            revision.revision_id,
            chunk_id,
            RelationCandidateIdentity {
                normalized_assertion: &normalized_assertion,
                source_label: &relation.source_label,
                target_label: &relation.target_label,
                relation_type: &relation.relation_type,
            },
        );
        NewKnowledgeRelationCandidate {
            candidate_id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: Some(chunk_id),
            subject_label: relation.source_label.trim().to_string(),
            subject_candidate_key,
            predicate: relation.relation_type.trim().to_string(),
            object_label: relation.target_label.trim().to_string(),
            object_candidate_key,
            normalized_assertion,
            confidence: None,
            extraction_method: "graph_extract".to_string(),
            candidate_state: "active".to_string(),
            created_at: Some(Utc::now()),
            updated_at: Some(Utc::now()),
        }
    }
}
