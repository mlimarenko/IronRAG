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

const RERANK_RUNTIME_EVIDENCE_TEXT_CHARS: usize = 2_400;

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
}
