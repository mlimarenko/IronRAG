#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
use std::collections::HashMap;

use crate::{
    app::state::AppState,
    services::query::{
        planner::RuntimeQueryPlan,
        support::{
            QueryRerankTaskInput, RerankCandidate, RerankOutcome, RerankRequest,
            rerank_query_candidates,
        },
    },
};

use super::types::*;

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
            text: format!(
                "{} {} {}",
                relationship.from_label, relationship.relation_type, relationship.to_label
            ),
            score: relationship.score,
        })
        .collect()
}

pub(crate) fn build_chunk_candidates(chunks: &[RuntimeMatchedChunk]) -> Vec<RerankCandidate> {
    chunks
        .iter()
        .map(|chunk| RerankCandidate {
            id: chunk.chunk_id.to_string(),
            text: format!("{} {}", chunk.document_label, chunk.excerpt),
            score: chunk.score,
        })
        .collect()
}

pub(crate) fn apply_rerank_outcome(bundle: &mut RetrievalBundle, outcome: &RerankOutcome) {
    bundle.entities = reorder_entities(std::mem::take(&mut bundle.entities), &outcome.entities);
    bundle.relationships =
        reorder_relationships(std::mem::take(&mut bundle.relationships), &outcome.relationships);
    bundle.chunks = reorder_chunks(std::mem::take(&mut bundle.chunks), &outcome.chunks);
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
    reorder_by_ids(chunks, ordered_ids, |chunk| chunk.chunk_id.to_string())
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
