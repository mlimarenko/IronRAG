use std::collections::HashMap;

use crate::{
    app::state::AppState,
    domains::query::{
        RerankMetadata, RerankStatus, SemanticRerankMetadata, SemanticRerankMode,
        SemanticRerankOutcome, SemanticRerankStrategy,
    },
    services::query::{
        planner::RuntimeQueryPlan,
        support::{
            QueryRerankTaskInput, RerankCandidate, RerankOutcome, RerankRequest,
            rerank_query_candidates,
        },
    },
};

use super::semantic_rerank::{
    SemanticCandidateOrder, SemanticRerankAttempt, SemanticRerankFailure, SemanticRerankPolicy,
    execute_semantic_rerank, prepare_semantic_rerank_request, try_acquire_distributed_shadow_lease,
    try_acquire_shadow_task_permit,
};
use super::types::{
    RetrievalBundle, RuntimeChunkScoreKind, RuntimeMatchedChunk, RuntimeMatchedEntity,
    RuntimeMatchedRelationship, SemanticRerankExecutionContext,
};

const RERANK_RUNTIME_EVIDENCE_TEXT_CHARS: usize = 2_400;

pub(crate) async fn apply_configured_rerank(
    state: &AppState,
    library_id: uuid::Uuid,
    semantic_rerank_context: SemanticRerankExecutionContext,
    question: &str,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
    semantic_chunk_ranks: &mut HashMap<uuid::Uuid, usize>,
) -> RerankMetadata {
    let mode = state.retrieval_intelligence.semantic_rerank.mode;
    if mode == SemanticRerankMode::Off || !state.retrieval_intelligence.rerank_enabled {
        return apply_deterministic_rerank(state, question, plan, bundle);
    }

    let entity_candidates = build_entity_candidates(&bundle.entities);
    let relationship_candidates = build_relationship_candidates(&bundle.relationships);
    let chunk_candidates = build_chunk_candidates(&bundle.chunks);
    let policy =
        SemanticRerankPolicy::from_runtime_settings(state.retrieval_intelligence.semantic_rerank);
    let prepared = prepare_semantic_rerank_request(
        question,
        &entity_candidates,
        &relationship_candidates,
        &chunk_candidates,
        policy,
    );
    let prepared_candidate_count =
        prepared.as_ref().map_or(0, |request| request.prepared_candidate_count());
    let Some(prepared) = prepared.filter(|request| request.can_change_order()) else {
        let mut metadata = apply_deterministic_rerank(state, question, plan, bundle);
        metadata.semantic_rerank = Some(SemanticRerankMetadata {
            mode,
            strategy: SemanticRerankStrategy::LexicalHeuristicFallback,
            outcome: SemanticRerankOutcome::NotApplicable,
            prepared_candidate_count,
        });
        return metadata;
    };
    match mode {
        SemanticRerankMode::Off => apply_deterministic_rerank(state, question, plan, bundle),
        SemanticRerankMode::Shadow => {
            let mut metadata = apply_deterministic_rerank(state, question, plan, bundle);
            let heuristic_baseline = semantic_candidate_order(bundle);
            let scheduled = spawn_semantic_rerank_shadow(
                state,
                library_id,
                semantic_rerank_context,
                prepared,
                heuristic_baseline,
                policy,
            );
            metadata.semantic_rerank = Some(SemanticRerankMetadata {
                mode,
                strategy: SemanticRerankStrategy::LexicalHeuristicWithProviderShadow,
                outcome: if scheduled {
                    SemanticRerankOutcome::ShadowScheduled
                } else {
                    SemanticRerankOutcome::ShadowCapacitySkipped
                },
                prepared_candidate_count,
            });
            metadata
        }
        SemanticRerankMode::Active => {
            match execute_semantic_rerank(
                state,
                library_id,
                semantic_rerank_context,
                prepared,
                policy,
            )
            .await
            {
                SemanticRerankAttempt::Applied {
                    order,
                    prepared_candidate_count,
                    reordered_count,
                } => {
                    semantic_chunk_ranks.extend(
                        order.provider_ranked_chunk_ids().iter().enumerate().filter_map(
                            |(rank, chunk_id)| {
                                chunk_id.parse::<uuid::Uuid>().ok().map(|chunk_id| (chunk_id, rank))
                            },
                        ),
                    );
                    apply_semantic_candidate_order(bundle, &order);
                    RerankMetadata {
                        status: RerankStatus::Applied,
                        candidate_count: entity_candidates.len()
                            + relationship_candidates.len()
                            + chunk_candidates.len(),
                        reordered_count: Some(reordered_count),
                        semantic_rerank: Some(SemanticRerankMetadata {
                            mode,
                            strategy: SemanticRerankStrategy::ProviderSemantic,
                            outcome: SemanticRerankOutcome::Applied,
                            prepared_candidate_count,
                        }),
                    }
                }
                SemanticRerankAttempt::Failed { failure, prepared_candidate_count } => {
                    let mut metadata = apply_deterministic_rerank(state, question, plan, bundle);
                    metadata.semantic_rerank = Some(SemanticRerankMetadata {
                        mode,
                        strategy: SemanticRerankStrategy::LexicalHeuristicFallback,
                        outcome: failure_metadata_outcome(failure),
                        prepared_candidate_count,
                    });
                    metadata
                }
            }
        }
    }
}

fn apply_deterministic_rerank(
    state: &AppState,
    question: &str,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
) -> RerankMetadata {
    match plan.planned_mode {
        crate::domains::query::RuntimeQueryMode::Hybrid => {
            apply_hybrid_rerank(state, question, plan, bundle)
        }
        crate::domains::query::RuntimeQueryMode::Mix => {
            apply_mix_rerank(state, question, plan, bundle)
        }
        _ => crate::services::query::support::derive_rerank_metadata(&RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        }),
    }
}

fn spawn_semantic_rerank_shadow(
    state: &AppState,
    library_id: uuid::Uuid,
    execution_context: SemanticRerankExecutionContext,
    prepared: super::semantic_rerank::PreparedSemanticRerankRequest,
    heuristic_baseline: SemanticCandidateOrder,
    policy: SemanticRerankPolicy,
) -> bool {
    let Some(shadow_permit) = try_acquire_shadow_task_permit() else {
        tracing::debug!(
            stage = "retrieval.semantic_rerank_shadow",
            library_id = %library_id,
            query_execution_id = %execution_context.query_execution_id,
            "semantic rerank shadow skipped because the background task budget is full"
        );
        return false;
    };
    let state = state.clone();
    tokio::spawn(async move {
        let _shadow_permit = shadow_permit;
        let _distributed_lease = match try_acquire_distributed_shadow_lease(
            &state.persistence.redis,
        )
        .await
        {
            Ok(Some(lease)) => lease,
            Ok(None) => {
                tracing::debug!(
                    stage = "retrieval.semantic_rerank_shadow",
                    library_id = %library_id,
                    query_execution_id = %execution_context.query_execution_id,
                    "semantic rerank shadow skipped because another replica holds the deployment lease"
                );
                return;
            }
            Err(error) => {
                tracing::warn!(
                    stage = "retrieval.semantic_rerank_shadow",
                    library_id = %library_id,
                    query_execution_id = %execution_context.query_execution_id,
                    %error,
                    "semantic rerank shadow coordination failed; provider call suppressed"
                );
                return;
            }
        };
        let started = std::time::Instant::now();
        let attempt = Box::pin(crate::integrations::provider_budget::with_lane(
            crate::integrations::provider_budget::ProviderLane::Ingest,
            execute_semantic_rerank(&state, library_id, execution_context, prepared, policy),
        ))
        .await;
        match attempt {
            SemanticRerankAttempt::Applied { order, prepared_candidate_count, .. } => {
                let disagreement_count = order.reordered_count_against(
                    &heuristic_baseline.entities,
                    &heuristic_baseline.relationships,
                    &heuristic_baseline.chunks,
                );
                tracing::info!(
                    stage = "retrieval.semantic_rerank_shadow",
                    library_id = %library_id,
                    query_execution_id = %execution_context.query_execution_id,
                    prepared_candidate_count,
                    disagreement_count,
                    elapsed_ms = started.elapsed().as_millis(),
                    "semantic rerank shadow compared against the production heuristic order"
                );
            }
            SemanticRerankAttempt::Failed { failure, prepared_candidate_count } => tracing::warn!(
                stage = "retrieval.semantic_rerank_shadow",
                library_id = %library_id,
                query_execution_id = %execution_context.query_execution_id,
                ?failure,
                prepared_candidate_count,
                elapsed_ms = started.elapsed().as_millis(),
                "semantic rerank shadow failed without affecting answer order"
            ),
        }
    });
    true
}

fn semantic_candidate_order(bundle: &RetrievalBundle) -> SemanticCandidateOrder {
    SemanticCandidateOrder {
        entities: bundle.entities.iter().map(|entity| entity.node_id.to_string()).collect(),
        relationships: bundle
            .relationships
            .iter()
            .map(|relationship| relationship.edge_id.to_string())
            .collect(),
        chunks: bundle.chunks.iter().map(|chunk| chunk.chunk_id.to_string()).collect(),
        provider_ranked_chunks: Vec::new(),
    }
}

const fn failure_metadata_outcome(failure: SemanticRerankFailure) -> SemanticRerankOutcome {
    match failure {
        SemanticRerankFailure::MissingBinding => SemanticRerankOutcome::MissingBinding,
        SemanticRerankFailure::TimedOut => SemanticRerankOutcome::TimedOut,
        SemanticRerankFailure::ProviderFailure => SemanticRerankOutcome::ProviderFailure,
        SemanticRerankFailure::InvalidResponse => SemanticRerankOutcome::InvalidResponse,
    }
}

pub(crate) fn apply_hybrid_rerank(
    state: &AppState,
    question: &str,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
) -> crate::domains::query::RerankMetadata {
    let outcome = rerank_query_candidates(&QueryRerankTaskInput {
        request: RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        },
        entity_candidates: build_entity_candidates(&bundle.entities),
        relationship_candidates: build_relationship_candidates(&bundle.relationships),
        chunk_candidates: build_chunk_candidates(&bundle.chunks),
    })
    .unwrap_or_else(|_| {
        super::super::support::build_failed_rerank_outcome(
            &build_entity_candidates(&bundle.entities),
            &build_relationship_candidates(&bundle.relationships),
            &build_chunk_candidates(&bundle.chunks),
        )
    });
    apply_rerank_outcome(bundle, &outcome);
    outcome.metadata
}

pub(crate) fn apply_mix_rerank(
    state: &AppState,
    question: &str,
    plan: &RuntimeQueryPlan,
    bundle: &mut RetrievalBundle,
) -> crate::domains::query::RerankMetadata {
    let outcome = rerank_query_candidates(&QueryRerankTaskInput {
        request: RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: bundle.entities.len()
                + bundle.relationships.len()
                + bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        },
        entity_candidates: build_entity_candidates(&bundle.entities),
        relationship_candidates: build_relationship_candidates(&bundle.relationships),
        chunk_candidates: build_chunk_candidates(&bundle.chunks),
    })
    .unwrap_or_else(|_| {
        super::super::support::build_failed_rerank_outcome(
            &build_entity_candidates(&bundle.entities),
            &build_relationship_candidates(&bundle.relationships),
            &build_chunk_candidates(&bundle.chunks),
        )
    });
    apply_rerank_outcome(bundle, &outcome);
    outcome.metadata
}

pub(crate) fn build_entity_candidates(entities: &[RuntimeMatchedEntity]) -> Vec<RerankCandidate> {
    entities
        .iter()
        .map(|entity| RerankCandidate {
            id: entity.node_id.to_string(),
            text: format!("{} {}", entity.label, entity.node_type),
            score: entity.score,
        })
        .collect()
}

pub(crate) fn build_relationship_candidates(
    relationships: &[RuntimeMatchedRelationship],
) -> Vec<RerankCandidate> {
    relationships
        .iter()
        .map(|relationship| RerankCandidate {
            id: relationship.edge_id.to_string(),
            text: relationship.reference_excerpt(),
            score: relationship.score,
        })
        .collect()
}

pub(crate) fn build_chunk_candidates(chunks: &[RuntimeMatchedChunk]) -> Vec<RerankCandidate> {
    chunks
        .iter()
        .map(|chunk| RerankCandidate {
            id: chunk.chunk_id.to_string(),
            text: chunk_rerank_text(chunk),
            score: chunk.score,
        })
        .collect()
}

fn chunk_rerank_text(chunk: &RuntimeMatchedChunk) -> String {
    let source_text = chunk.source_text.trim();
    let excerpt = chunk.excerpt.trim();
    if source_text.is_empty() || chunk.score_kind == RuntimeChunkScoreKind::Relevance {
        return format!("{} {}", chunk.document_label, excerpt);
    }
    let runtime_text =
        super::retrieve::excerpt_for(source_text, RERANK_RUNTIME_EVIDENCE_TEXT_CHARS);
    if excerpt.is_empty() || runtime_text.contains(excerpt) {
        format!("{} {}", chunk.document_label, runtime_text)
    } else {
        format!("{} {}\n{}", chunk.document_label, excerpt, runtime_text)
    }
}

pub(crate) fn apply_rerank_outcome(bundle: &mut RetrievalBundle, outcome: &RerankOutcome) {
    bundle.entities = reorder_entities(std::mem::take(&mut bundle.entities), &outcome.entities);
    bundle.relationships =
        reorder_relationships(std::mem::take(&mut bundle.relationships), &outcome.relationships);
    bundle.chunks = reorder_chunks(std::mem::take(&mut bundle.chunks), &outcome.chunks);
}

fn apply_semantic_candidate_order(bundle: &mut RetrievalBundle, order: &SemanticCandidateOrder) {
    bundle.entities = reorder_entities(std::mem::take(&mut bundle.entities), &order.entities);
    bundle.relationships =
        reorder_relationships(std::mem::take(&mut bundle.relationships), &order.relationships);
    // Provider scores decide ordering only. Source/lane scores remain intact
    // so provenance and downstream absolute-evidence guards do not conflate a
    // model judgment with the score produced by the retrieval lane.
    bundle.chunks = reorder_by_ids(std::mem::take(&mut bundle.chunks), &order.chunks, |chunk| {
        chunk.chunk_id.to_string()
    });
}

fn reorder_entities(
    entities: Vec<RuntimeMatchedEntity>,
    ordered_ids: &[String],
) -> Vec<RuntimeMatchedEntity> {
    reorder_by_ids(entities, ordered_ids, |entity| entity.node_id.to_string())
}

fn reorder_relationships(
    relationships: Vec<RuntimeMatchedRelationship>,
    ordered_ids: &[String],
) -> Vec<RuntimeMatchedRelationship> {
    reorder_by_ids(relationships, ordered_ids, |relationship| relationship.edge_id.to_string())
}

fn reorder_chunks(
    chunks: Vec<RuntimeMatchedChunk>,
    ordered_ids: &[String],
) -> Vec<RuntimeMatchedChunk> {
    let order_index = ordered_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect::<HashMap<_, _>>();
    let ordered_len = ordered_ids.len();
    let mut indexed = chunks.into_iter().enumerate().collect::<Vec<_>>();
    for (_, chunk) in &mut indexed {
        let Some(rank) = order_index.get(&chunk.chunk_id.to_string()).copied() else {
            continue;
        };
        chunk.score = Some(rerank_preserved_chunk_score(chunk, ordered_len, rank));
    }
    indexed.sort_by(|(left_index, left), (right_index, right)| {
        let left_order = order_index.get(&left.chunk_id.to_string()).copied().unwrap_or(usize::MAX);
        let right_order =
            order_index.get(&right.chunk_id.to_string()).copied().unwrap_or(usize::MAX);
        left_order.cmp(&right_order).then_with(|| left_index.cmp(right_index))
    });
    indexed.into_iter().map(|(_, item)| item).collect()
}

fn rerank_preserved_chunk_score(
    chunk: &RuntimeMatchedChunk,
    ordered_len: usize,
    rank: usize,
) -> f32 {
    let rerank_score = ordered_len.saturating_sub(rank) as f32;
    if chunk.score_kind == RuntimeChunkScoreKind::Relevance {
        return rerank_score;
    }
    chunk.score.unwrap_or(0.0).max(rerank_score)
}

fn reorder_by_ids<T>(
    items: Vec<T>,
    ordered_ids: &[String],
    id_of: impl Fn(&T) -> String,
) -> Vec<T> {
    let order_index = ordered_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), index))
        .collect::<HashMap<_, _>>();
    let mut indexed = items.into_iter().enumerate().collect::<Vec<_>>();
    indexed.sort_by(|(left_index, left), (right_index, right)| {
        let left_order = order_index.get(&id_of(left)).copied().unwrap_or(usize::MAX);
        let right_order = order_index.get(&id_of(right)).copied().unwrap_or(usize::MAX);
        left_order.cmp(&right_order).then_with(|| left_index.cmp(right_index))
    });
    indexed.into_iter().map(|(_, item)| item).collect()
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;

    fn chunk_with_runtime_text(score_kind: RuntimeChunkScoreKind) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_label: "rare-node-notes.md".to_string(),
            excerpt: "Short neighboring heading.".to_string(),
            score_kind,
            score: Some(0.9),
            source_text: "Exact runtime evidence: AlphaSwitch escalates through mailbox Z-19."
                .to_string(),
        }
    }

    #[test]
    fn graph_evidence_rerank_candidate_uses_runtime_source_text() {
        let candidate = build_chunk_candidates(&[chunk_with_runtime_text(
            RuntimeChunkScoreKind::GraphEvidence,
        )])
        .remove(0);

        assert!(candidate.text.contains("AlphaSwitch escalates through mailbox Z-19"));
    }

    #[test]
    fn ordinary_rerank_candidate_keeps_excerpt_text() {
        let candidate =
            build_chunk_candidates(&[chunk_with_runtime_text(RuntimeChunkScoreKind::Relevance)])
                .remove(0);

        assert!(candidate.text.contains("Short neighboring heading."));
        assert!(!candidate.text.contains("AlphaSwitch escalates through mailbox Z-19"));
    }

    #[test]
    fn rerank_does_not_lower_absolute_evidence_lane_score() {
        let mut protected = chunk_with_runtime_text(RuntimeChunkScoreKind::FocusedDocument);
        protected.score = Some(5_000.0);
        let protected_id = protected.chunk_id.to_string();
        let mut ordinary = chunk_with_runtime_text(RuntimeChunkScoreKind::Relevance);
        ordinary.score = Some(0.1);
        let ordinary_id = ordinary.chunk_id.to_string();
        let outcome = RerankOutcome {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![ordinary_id, protected_id],
            metadata: crate::domains::query::RerankMetadata {
                status: crate::domains::query::RerankStatus::Applied,
                candidate_count: 2,
                reordered_count: Some(2),
                semantic_rerank: None,
            },
        };
        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![protected, ordinary],
        };

        apply_rerank_outcome(&mut bundle, &outcome);

        let retained = bundle
            .chunks
            .iter()
            .find(|chunk| chunk.chunk_id.to_string() == outcome.chunks[1])
            .expect("protected chunk must remain present");
        assert_eq!(retained.score, Some(5_000.0));
    }

    #[test]
    fn semantic_order_preserves_original_retrieval_scores() {
        let mut first = chunk_with_runtime_text(RuntimeChunkScoreKind::Relevance);
        first.score = Some(0.9);
        let first_id = first.chunk_id.to_string();
        let mut second = chunk_with_runtime_text(RuntimeChunkScoreKind::Relevance);
        second.score = Some(0.2);
        let second_id = second.chunk_id.to_string();
        let mut bundle = RetrievalBundle {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![first, second],
        };
        let order = SemanticCandidateOrder {
            entities: Vec::new(),
            relationships: Vec::new(),
            chunks: vec![second_id, first_id],
            provider_ranked_chunks: Vec::new(),
        };

        apply_semantic_candidate_order(&mut bundle, &order);

        assert_eq!(bundle.chunks[0].score, Some(0.2));
        assert_eq!(bundle.chunks[1].score, Some(0.9));
    }
}
