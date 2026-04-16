use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{content::revision_text_state_is_readable, knowledge::TypedTechnicalFact},
    infra::{
        arangodb::graph_store::{
            KnowledgeEntityCandidateRow, KnowledgeRelationCandidateRow, NewKnowledgeEntity,
            NewKnowledgeEntityCandidate, NewKnowledgeRelation, NewKnowledgeRelationCandidate,
        },
        repositories,
    },
    services::graph::extract::{
        GraphEntityCandidate, GraphExtractionCandidateSet, GraphRelationCandidate,
    },
};

use super::{
    ArangoGraphRebuildOutcome, ArangoRevisionContext, GraphService, MaterializedExtractCandidates,
    ReconciledEntityCandidate, ReconciledRelationCandidate,
    apply_entity_key_aliases_to_relation_candidate, build_entity_candidate_key_index,
    build_materialized_extract_candidates, build_prefixed_entity_key_aliases,
    build_relation_entity_key_index, canonical_entity_candidate_id, canonical_entity_id,
    canonical_relation_assertion_from_keys, canonical_relation_candidate_id, canonical_relation_id,
    placeholder_entity_parts_from_key, reconcile_entity_candidate_row,
    reconcile_relation_candidate_row, relation_candidate_keys_are_materializable,
    relation_fields_are_semantically_empty, select_canonical_entity_label,
};

impl GraphService {
    pub(super) async fn reconcile_arango_library_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<ArangoGraphRebuildOutcome> {
        let entity_candidates = state
            .arango_graph_store
            .list_entity_candidates_by_library(library_id)
            .await
            .context("failed to load arango entity candidates")?;
        let relation_candidates = state
            .arango_graph_store
            .list_relation_candidates_by_library(library_id)
            .await
            .context("failed to load arango relation candidates")?;
        self.reconcile_arango_candidates(
            state,
            library_id,
            entity_candidates,
            relation_candidates,
            alias_overrides,
        )
        .await
    }

    async fn reconcile_arango_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        entity_candidates: Vec<KnowledgeEntityCandidateRow>,
        relation_candidates: Vec<KnowledgeRelationCandidateRow>,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<ArangoGraphRebuildOutcome> {
        #[derive(Debug)]
        struct EntityReconcileGroup {
            normalization_key: String,
            revision_context: ArangoRevisionContext,
            candidates: Vec<KnowledgeEntityCandidateRow>,
            entity_id: Uuid,
        }

        #[derive(Debug)]
        struct RelationReconcileGroup {
            revision_context: ArangoRevisionContext,
            candidates: Vec<ReconciledRelationCandidate>,
            relation_id: Uuid,
        }

        let entity_key_index = build_entity_candidate_key_index(&entity_candidates);
        let entity_candidates = entity_candidates
            .into_iter()
            .filter_map(|row| reconcile_entity_candidate_row(row, &entity_key_index))
            .collect::<Vec<_>>();
        let filtered_relation_candidates = relation_candidates
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
        let entity_key_aliases = build_prefixed_entity_key_aliases(&entity_candidates);
        let entity_candidates = entity_candidates
            .into_iter()
            .map(|mut candidate| {
                if let Some(canonical_key) = entity_key_aliases.get(&candidate.normalization_key) {
                    candidate.normalization_key = canonical_key.clone();
                }
                candidate
            })
            .collect::<Vec<_>>();
        let filtered_relation_candidates = filtered_relation_candidates
            .into_iter()
            .map(|mut candidate| {
                apply_entity_key_aliases_to_relation_candidate(&mut candidate, &entity_key_aliases);
                candidate
            })
            .collect::<Vec<_>>();

        let mut revision_contexts = BTreeMap::<Uuid, ArangoRevisionContext>::new();
        for revision_id in entity_candidates
            .iter()
            .map(|candidate| candidate.row.revision_id)
            .chain(filtered_relation_candidates.iter().map(|candidate| candidate.row.revision_id))
            .collect::<BTreeSet<_>>()
        {
            if let Some(revision) = state
                .arango_document_store
                .get_revision(revision_id)
                .await
                .context("failed to load revision for arango graph reconciliation")?
            {
                revision_contexts.insert(revision_id, ArangoRevisionContext::from(revision));
            }
        }

        let mut typed_facts_by_revision = BTreeMap::<Uuid, Vec<TypedTechnicalFact>>::new();
        for revision_id in revision_contexts.keys().copied() {
            let typed_facts = state
                .canonical_services
                .knowledge
                .list_typed_technical_facts(state, revision_id)
                .await
                .with_context(|| {
                    format!(
                        "failed to load typed technical facts for arango graph reconciliation revision {revision_id}"
                    )
                })?;
            typed_facts_by_revision.insert(revision_id, typed_facts);
        }

        let chunk_ids = entity_candidates
            .iter()
            .filter_map(|candidate| candidate.row.chunk_id)
            .chain(
                filtered_relation_candidates.iter().filter_map(|candidate| candidate.row.chunk_id),
            )
            .collect::<BTreeSet<_>>();
        let mut revision_chunk_ids = BTreeMap::<Uuid, BTreeSet<Uuid>>::new();
        for candidate in &entity_candidates {
            if let Some(chunk_id) = candidate.row.chunk_id {
                revision_chunk_ids.entry(candidate.row.revision_id).or_default().insert(chunk_id);
            }
        }
        for candidate in &filtered_relation_candidates {
            if let Some(chunk_id) = candidate.row.chunk_id {
                revision_chunk_ids.entry(candidate.row.revision_id).or_default().insert(chunk_id);
            }
        }
        let mut chunk_rows_by_id =
            BTreeMap::<Uuid, crate::infra::arangodb::document_store::KnowledgeChunkRow>::new();
        for chunk_id in chunk_ids {
            if let Some(chunk) =
                state.arango_document_store.get_chunk(chunk_id).await.with_context(|| {
                    format!("failed to load chunk {chunk_id} for arango graph reconciliation")
                })?
            {
                chunk_rows_by_id.insert(chunk_id, chunk);
            }
        }

        let mut outcome = ArangoGraphRebuildOutcome {
            scanned_entity_candidates: entity_candidates.len(),
            scanned_relation_candidates: filtered_relation_candidates.len(),
            ..Default::default()
        };

        for (revision_id, revision_context) in &revision_contexts {
            self.upsert_revision_edges(state, revision_context).await?;
            outcome.upserted_document_revision_edges += 1;
            if let Some(chunk_ids) = revision_chunk_ids.get(revision_id) {
                for chunk_id in chunk_ids {
                    self.upsert_chunk_edge(state, revision_context, *chunk_id).await?;
                    outcome.upserted_revision_chunk_edges += 1;
                }
            }
        }

        let mut entity_groups = BTreeMap::<String, Vec<ReconciledEntityCandidate>>::new();
        for candidate in entity_candidates {
            entity_groups.entry(candidate.normalization_key.clone()).or_default().push(candidate);
        }

        let mut entity_reconcile_groups = Vec::<EntityReconcileGroup>::new();
        let mut entity_requests = Vec::<NewKnowledgeEntity>::new();
        let mut entity_request_ids = BTreeSet::<Uuid>::new();
        for (normalization_key, rows) in entity_groups {
            let row = rows
                .last()
                .ok_or_else(|| anyhow::anyhow!("entity candidate group is unexpectedly empty"))?;
            let revision_context =
                revision_contexts.get(&row.row.revision_id).ok_or_else(|| {
                    anyhow::anyhow!("missing revision context for {}", row.row.revision_id)
                })?;
            let canonical_label = select_canonical_entity_label(&rows, &normalization_key)
                .unwrap_or_else(|| {
                    placeholder_entity_parts_from_key(&normalization_key)
                        .map(|(_, canonical_label)| canonical_label)
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
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.trim().to_string())
            });
            let alias_rows = rows.iter().map(|candidate| candidate.row.clone()).collect::<Vec<_>>();
            let aliases = self.collect_entity_aliases(
                &alias_rows,
                alias_overrides,
                &normalization_key,
                &canonical_label,
            );
            let confidence = rows
                .iter()
                .filter_map(|candidate| candidate.row.confidence)
                .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
            let entity_id = canonical_entity_id(library_id, &normalization_key);
            entity_request_ids.insert(entity_id);
            entity_requests.push(NewKnowledgeEntity {
                entity_id,
                workspace_id: revision_context.workspace_id,
                library_id,
                canonical_label,
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
            entity_reconcile_groups.push(EntityReconcileGroup {
                normalization_key,
                revision_context: revision_context.clone(),
                candidates: rows.into_iter().map(|candidate| candidate.row).collect(),
                entity_id,
            });
        }

        let mut relation_groups = BTreeMap::<String, Vec<ReconciledRelationCandidate>>::new();
        for candidate in filtered_relation_candidates {
            relation_groups
                .entry(candidate.normalized_assertion.clone())
                .or_default()
                .push(candidate);
        }

        let mut relation_reconcile_groups = Vec::<RelationReconcileGroup>::new();
        let mut relation_requests = Vec::<NewKnowledgeRelation>::new();
        let mut placeholder_entity_requests = BTreeMap::<Uuid, NewKnowledgeEntity>::new();
        for (normalized_assertion, rows) in relation_groups {
            let row = rows
                .last()
                .ok_or_else(|| anyhow::anyhow!("relation candidate group is unexpectedly empty"))?;
            let revision_context =
                revision_contexts.get(&row.row.revision_id).ok_or_else(|| {
                    anyhow::anyhow!("missing revision context for {}", row.row.revision_id)
                })?;
            let predicate = rows
                .iter()
                .find_map(|candidate| {
                    (!candidate.predicate.trim().is_empty())
                        .then(|| candidate.predicate.trim().to_string())
                })
                .unwrap_or_else(|| "related_to".to_string());
            let confidence = rows
                .iter()
                .filter_map(|candidate| candidate.row.confidence)
                .max_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
            let relation_id = canonical_relation_id(library_id, &normalized_assertion);
            relation_requests.push(NewKnowledgeRelation {
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
            for candidate in &rows {
                for normalization_key in
                    [&candidate.subject_candidate_key, &candidate.object_candidate_key]
                {
                    let Some((node_type, canonical_label)) =
                        placeholder_entity_parts_from_key(normalization_key)
                    else {
                        continue;
                    };
                    let entity_id = canonical_entity_id(library_id, normalization_key);
                    if entity_request_ids.contains(&entity_id) {
                        continue;
                    }
                    let entry = placeholder_entity_requests.entry(entity_id).or_insert_with(|| {
                        NewKnowledgeEntity {
                            entity_id,
                            workspace_id: revision_context.workspace_id,
                            library_id,
                            canonical_label: canonical_label.clone(),
                            aliases: vec![canonical_label.clone()],
                            entity_type: crate::services::graph::identity::runtime_node_type_slug(
                                &node_type,
                            )
                            .to_string(),
                            entity_sub_type: None,
                            summary: None,
                            confidence: None,
                            support_count: 0,
                            freshness_generation: revision_context.revision_number,
                            entity_state: "active".to_string(),
                            created_at: None,
                            updated_at: Some(Utc::now()),
                        }
                    });
                    entry.support_count += 1;
                    entry.freshness_generation =
                        entry.freshness_generation.max(revision_context.revision_number);
                    entry.updated_at = Some(Utc::now());
                }
            }
            relation_reconcile_groups.push(RelationReconcileGroup {
                revision_context: revision_context.clone(),
                candidates: rows,
                relation_id,
            });
        }

        entity_requests.extend(placeholder_entity_requests.into_values());
        self.reset_arango_library_materialization(state, library_id).await?;
        let entity_rows = state.arango_graph_store.upsert_entities(&entity_requests).await?;
        let entity_by_id =
            entity_rows.into_iter().map(|row| (row.entity_id, row)).collect::<BTreeMap<_, _>>();

        for group in entity_reconcile_groups {
            let entity = entity_by_id.get(&group.entity_id).ok_or_else(|| {
                anyhow::anyhow!("missing canonical entity {} after bulk upsert", group.entity_id)
            })?;
            outcome.upserted_entities += 1;
            for candidate in group.candidates {
                let supporting_chunk =
                    candidate.chunk_id.and_then(|chunk_id| chunk_rows_by_id.get(&chunk_id));
                let revision_facts = typed_facts_by_revision
                    .get(&candidate.revision_id)
                    .map_or_else(|| &[][..], Vec::as_slice);
                self.upsert_current_entity_evidence(
                    state,
                    &group.revision_context,
                    &candidate,
                    entity,
                    &group.normalization_key,
                    supporting_chunk,
                    revision_facts,
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

        let relation_rows = state.arango_graph_store.upsert_relations(&relation_requests).await?;
        let relation_by_id =
            relation_rows.into_iter().map(|row| (row.relation_id, row)).collect::<BTreeMap<_, _>>();

        for group in relation_reconcile_groups {
            let relation = relation_by_id.get(&group.relation_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "missing canonical relation {} after bulk upsert",
                    group.relation_id
                )
            })?;
            outcome.upserted_relations += 1;
            for candidate in group.candidates {
                let subject_id = canonical_entity_id(library_id, &candidate.subject_candidate_key);
                let object_id = canonical_entity_id(library_id, &candidate.object_candidate_key);
                let subject = entity_by_id.get(&subject_id).ok_or_else(|| {
                    anyhow::anyhow!("missing subject placeholder entity {}", subject_id)
                })?;
                let object = entity_by_id.get(&object_id).ok_or_else(|| {
                    anyhow::anyhow!("missing object placeholder entity {}", object_id)
                })?;
                self.upsert_relation_edges(state, relation, subject, object).await?;
                let supporting_chunk =
                    candidate.row.chunk_id.and_then(|chunk_id| chunk_rows_by_id.get(&chunk_id));
                let revision_facts = typed_facts_by_revision
                    .get(&candidate.row.revision_id)
                    .map_or_else(|| &[][..], Vec::as_slice);
                self.upsert_current_relation_evidence(
                    state,
                    &group.revision_context,
                    &candidate,
                    relation,
                    supporting_chunk,
                    revision_facts,
                )
                .await?;
                outcome.upserted_evidence += 1;
                outcome.upserted_relation_subject_edges += 1;
                outcome.upserted_relation_object_edges += 1;
                outcome.upserted_evidence_source_edges += 1;
                outcome.upserted_evidence_support_relation_edges += 1;
            }
        }

        Ok(outcome)
    }

    pub(super) async fn materialize_current_candidate_batch(
        &self,
        state: &AppState,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        chunk_id: Uuid,
        candidates: &GraphExtractionCandidateSet,
        mark_existing_only: bool,
    ) -> Result<()> {
        let revision_context = ArangoRevisionContext::from(revision.clone());
        let entity_key_index = build_relation_entity_key_index(candidates);
        let entity_alias_overrides = self.build_alias_overrides(candidates, &entity_key_index);
        let chunk_row =
            state.arango_document_store.get_chunk(chunk_id).await.with_context(|| {
                format!("failed to load chunk {chunk_id} for graph materialization")
            })?;
        let revision_facts = state
            .canonical_services
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

        for entity in &candidates.entities {
            let candidate =
                self.build_entity_candidate_row(revision, chunk_id, entity, &entity_key_index);
            let candidate_row = state
                .arango_graph_store
                .upsert_entity_candidate(&candidate)
                .await
                .context("failed to upsert arango entity candidate")?;
            if !mark_existing_only {
                let entity_row = self
                    .upsert_canonical_entity(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.normalization_key,
                        candidate.candidate_label.trim(),
                        &candidate.candidate_type,
                        entity_alias_overrides
                            .get(&candidate.normalization_key)
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .collect(),
                        candidate.confidence,
                        1,
                        revision.revision_number,
                    )
                    .await?;
                self.upsert_current_entity_evidence(
                    state,
                    &revision_context,
                    &candidate_row,
                    &entity_row,
                    &candidate_row.normalization_key,
                    chunk_row.as_ref(),
                    revision_facts.as_slice(),
                )
                .await?;
                self.upsert_chunk_mentions_entity_edge(
                    state,
                    chunk_id,
                    entity_row.entity_id,
                    candidate_row.confidence,
                    revision.library_id,
                )
                .await?;
            }
        }

        for relation in &candidates.relations {
            if relation_fields_are_semantically_empty(
                &relation.source_label,
                &relation.relation_type,
                &relation.target_label,
            ) {
                continue;
            }
            let candidate =
                self.build_relation_candidate_row(revision, chunk_id, relation, &entity_key_index);
            let candidate_row = state
                .arango_graph_store
                .upsert_relation_candidate(&candidate)
                .await
                .context("failed to upsert arango relation candidate")?;
            if !mark_existing_only {
                let relation_row = self
                    .upsert_canonical_relation(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.normalized_assertion,
                        candidate.predicate.trim(),
                        candidate.confidence,
                        1,
                        revision.revision_number,
                    )
                    .await?;
                let subject = self
                    .upsert_placeholder_entity_for_key(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.subject_candidate_key,
                    )
                    .await?;
                let object = self
                    .upsert_placeholder_entity_for_key(
                        state,
                        revision.library_id,
                        revision.workspace_id,
                        &candidate.object_candidate_key,
                    )
                    .await?;
                self.upsert_relation_edges(state, &relation_row, &subject, &object).await?;
                self.upsert_current_relation_evidence(
                    state,
                    &revision_context,
                    &candidate_row,
                    &relation_row,
                    chunk_row.as_ref(),
                    revision_facts.as_slice(),
                )
                .await?;
            }
        }

        Ok(())
    }

    fn build_alias_overrides(
        &self,
        candidates: &GraphExtractionCandidateSet,
        entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    ) -> BTreeMap<String, BTreeSet<String>> {
        let mut overrides = BTreeMap::<String, BTreeSet<String>>::new();
        for entity in &candidates.entities {
            let key = entity_key_index.canonical_node_key_for_label(&entity.label);
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
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        chunk_id: Uuid,
        entity: &GraphEntityCandidate,
        entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    ) -> NewKnowledgeEntityCandidate {
        let normalization_key = entity_key_index.canonical_node_key_for_label(&entity.label);
        let canonical_node_type =
            crate::services::graph::identity::runtime_node_type_from_key(&normalization_key);
        let candidate_id = canonical_entity_candidate_id(
            revision.library_id,
            revision.revision_id,
            chunk_id,
            &normalization_key,
            &entity.label,
            &canonical_node_type,
        );
        NewKnowledgeEntityCandidate {
            candidate_id,
            workspace_id: revision.workspace_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: Some(chunk_id),
            candidate_label: entity.label.trim().to_string(),
            candidate_type: crate::services::graph::identity::runtime_node_type_slug(
                &canonical_node_type,
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
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        chunk_id: Uuid,
        relation: &GraphRelationCandidate,
        entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
    ) -> NewKnowledgeRelationCandidate {
        let subject_candidate_key =
            entity_key_index.canonical_node_key_for_label(&relation.source_label);
        let object_candidate_key =
            entity_key_index.canonical_node_key_for_label(&relation.target_label);
        let normalized_assertion = canonical_relation_assertion_from_keys(
            &subject_candidate_key,
            &relation.relation_type,
            &object_candidate_key,
        );
        let candidate_id = canonical_relation_candidate_id(
            revision.library_id,
            revision.revision_id,
            chunk_id,
            &normalized_assertion,
            &relation.source_label,
            &relation.target_label,
            &relation.relation_type,
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

    pub(super) async fn build_and_refresh_arango_graph_from_candidates(
        &self,
        state: &AppState,
        library_id: Uuid,
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
    ) -> Result<ArangoGraphRebuildOutcome> {
        self.reconcile_arango_library_candidates(state, library_id, alias_overrides).await
    }

    pub(super) async fn refresh_arango_library_candidate_materialization(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<()> {
        let library = state
            .canonical_services
            .catalog
            .get_library(state, library_id)
            .await
            .context("failed to load library for arango candidate rebuild")?;
        let documents = state
            .arango_document_store
            .list_documents_by_library(library.workspace_id, library_id, false)
            .await
            .context("failed to list documents for arango candidate rebuild")?;
        let revision_ids = documents
            .iter()
            .filter(|document| document.deleted_at.is_none())
            .flat_map(|document| [document.readable_revision_id, document.active_revision_id])
            .flatten()
            .collect::<BTreeSet<_>>();

        let mut entity_candidates = Vec::<NewKnowledgeEntityCandidate>::new();
        let mut relation_candidates = Vec::<NewKnowledgeRelationCandidate>::new();
        for revision_id in revision_ids {
            let Some(revision) =
                state.arango_document_store.get_revision(revision_id).await.with_context(|| {
                    format!("failed to load arango revision {revision_id} for candidate rebuild")
                })?
            else {
                continue;
            };
            if revision.superseded_by_revision_id.is_some()
                || !revision_text_state_is_readable(&revision.text_state)
            {
                continue;
            }

            let materialized =
                self.load_revision_materialized_extract_candidates(state, &revision).await?;
            entity_candidates.extend(materialized.entity_candidates);
            relation_candidates.extend(materialized.relation_candidates);
        }

        state
            .arango_graph_store
            .delete_entity_candidates_by_library(library_id)
            .await
            .context("failed to clear stale arango entity candidates before rebuild")?;
        state
            .arango_graph_store
            .delete_relation_candidates_by_library(library_id)
            .await
            .context("failed to clear stale arango relation candidates before rebuild")?;

        for batch in entity_candidates.chunks(256) {
            state.arango_graph_store.upsert_entity_candidates(batch).await.with_context(|| {
                format!("failed to persist {} arango entity candidates during rebuild", batch.len())
            })?;
        }

        for batch in relation_candidates.chunks(256) {
            state.arango_graph_store.upsert_relation_candidates(batch).await.with_context(
                || {
                    format!(
                        "failed to persist {} arango relation candidates during rebuild",
                        batch.len()
                    )
                },
            )?;
        }

        Ok(())
    }

    async fn load_revision_materialized_extract_candidates(
        &self,
        state: &AppState,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
    ) -> Result<MaterializedExtractCandidates> {
        let chunk_results =
            repositories::extract_repository::list_ready_extract_chunk_results_by_revision(
                &state.persistence.postgres,
                revision.revision_id,
            )
            .await
            .with_context(|| {
                format!(
                    "failed to load canonical extract chunk results for revision {}",
                    revision.revision_id
                )
            })?;

        self.collect_revision_materialized_extract_candidates(state, revision, &chunk_results).await
    }

    async fn collect_revision_materialized_extract_candidates(
        &self,
        state: &AppState,
        revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
        chunk_results: &[repositories::extract_repository::ExtractChunkResultRow],
    ) -> Result<MaterializedExtractCandidates> {
        let mut materialized = MaterializedExtractCandidates::default();
        for chunk_result in chunk_results {
            let node_candidates =
                repositories::extract_repository::list_extract_node_candidates_by_chunk_result(
                    &state.persistence.postgres,
                    chunk_result.id,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load canonical extract node candidates for chunk result {}",
                        chunk_result.id
                    )
                })?;
            let edge_candidates =
                repositories::extract_repository::list_extract_edge_candidates_by_chunk_result(
                    &state.persistence.postgres,
                    chunk_result.id,
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load canonical extract edge candidates for chunk result {}",
                        chunk_result.id
                    )
                })?;
            let chunk_materialized = build_materialized_extract_candidates(
                revision,
                chunk_result,
                &node_candidates,
                &edge_candidates,
            );
            materialized.entity_candidates.extend(chunk_materialized.entity_candidates);
            materialized.relation_candidates.extend(chunk_materialized.relation_candidates);
        }
        Ok(materialized)
    }

    async fn reset_arango_library_materialization(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<()> {
        state
            .arango_graph_store
            .reset_library_materialized_graph(library_id)
            .await
            .context("failed to reset arango graph materialization")?;
        state
            .arango_search_store
            .delete_entity_vectors_by_library(library_id)
            .await
            .context("failed to delete stale entity vectors for graph reset")?;
        Ok(())
    }

    pub(super) async fn recalculate_arango_library_generations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<()> {
        let library = state
            .canonical_services
            .catalog
            .get_library(state, library_id)
            .await
            .context("failed to load library for generation refresh")?;
        let documents = state
            .arango_document_store
            .list_documents_by_library(library.workspace_id, library_id, false)
            .await
            .context("failed to list documents for generation refresh")?;
        let mut active_text_generation = 0i64;
        let mut active_vector_generation = 0i64;
        let mut active_graph_generation = 0i64;
        let mut has_ready_text = false;
        let mut has_ready_vector = false;
        let mut has_ready_graph = false;

        for document in documents {
            let revisions = state
                .arango_document_store
                .list_revisions_by_document(document.document_id)
                .await
                .context("failed to list revisions for generation refresh")?;
            for revision in revisions {
                if revision_text_state_is_readable(&revision.text_state) {
                    has_ready_text = true;
                    active_text_generation = active_text_generation.max(revision.revision_number);
                }
                if revision.vector_state == "ready" {
                    has_ready_vector = true;
                    active_vector_generation =
                        active_vector_generation.max(revision.revision_number);
                }
                if revision.graph_state == "ready" {
                    has_ready_graph = true;
                    active_graph_generation = active_graph_generation.max(revision.revision_number);
                }
            }
        }

        let _ = state
            .canonical_services
            .knowledge
            .refresh_library_generation(
                state,
                crate::services::knowledge::service::RefreshKnowledgeLibraryGenerationCommand {
                    generation_id: Uuid::now_v7(),
                    workspace_id: library.workspace_id,
                    library_id,
                    active_text_generation: if has_ready_text { active_text_generation } else { 0 },
                    active_vector_generation: if has_ready_vector {
                        active_vector_generation
                    } else {
                        0
                    },
                    active_graph_generation: if has_ready_graph {
                        active_graph_generation
                    } else {
                        0
                    },
                    degraded_state: if has_ready_text && has_ready_vector && has_ready_graph {
                        "ready".to_string()
                    } else {
                        "degraded".to_string()
                    },
                },
            )
            .await
            .context("failed to refresh arango library generation")?;
        Ok(())
    }

    fn collect_entity_aliases(
        &self,
        rows: &[KnowledgeEntityCandidateRow],
        alias_overrides: Option<&BTreeMap<String, BTreeSet<String>>>,
        normalization_key: &str,
        canonical_label: &str,
    ) -> BTreeSet<String> {
        let mut aliases = BTreeSet::<String>::new();
        if !canonical_label.trim().is_empty() {
            aliases.insert(canonical_label.trim().to_string());
        }
        for row in rows {
            if !row.candidate_label.trim().is_empty() {
                aliases.insert(row.candidate_label.trim().to_string());
            }
        }
        if let Some(overrides) = alias_overrides {
            if let Some(values) = overrides.get(normalization_key) {
                aliases.extend(values.iter().cloned());
            }
        }
        aliases
    }
}
