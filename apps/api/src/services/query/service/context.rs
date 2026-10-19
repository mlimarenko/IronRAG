pub(super) mod bundle_references;
mod prepared_references;

use std::collections::{HashMap, HashSet};

use anyhow::Context;
use chrono::Utc;
use serde_json::json;
use std::time::Instant;
use uuid::Uuid;

use bundle_references::{
    absorb_traversal_row, augment_fact_rank_refs_from_chunk_support, build_chunk_bundle_edges,
    build_entity_bundle_edges, build_evidence_bundle_edges, build_relation_bundle_edges,
    derive_block_rank_refs, evidence_rank_for_bundle, evidence_score_for_bundle,
    freshness_snapshot_json, merge_technical_fact_rows, seed_chunk_refs_from_answer_context,
    seed_entity_refs_from_answer_graph, seed_relation_endpoint_refs_from_answer_graph,
    seed_relation_refs_from_answer_graph, select_diversified_fact_ids,
};
pub(crate) use prepared_references::{
    load_execution_prepared_reference_context, query_ir_from_bundle_diagnostics,
};

use super::{
    ExecutionPreparedReferenceContext, MAX_DETAIL_TECHNICAL_FACT_REFERENCES,
    PreparedSegmentRevisionInfo, RankedBundleReference, merge_ranked_reference, runtime_mode_label,
    saturating_rank, top_ranked_ids,
};
use crate::{
    app::state::AppState,
    domains::query::RuntimeQueryMode,
    infra::{
        knowledge_rows::{
            KnowledgeBundleChunkEdgeRow, KnowledgeBundleEntityEdgeRow,
            KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleRelationEdgeRow,
            KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow, KnowledgeEvidenceRow,
            KnowledgeGraphTraversalRow, KnowledgeLibraryGenerationRow, KnowledgeRetrievalTraceRow,
            KnowledgeTechnicalFactRow,
        },
        repositories::{catalog_repository, query_repository},
    },
    interfaces::http::router_support::ApiError,
    services::content::document_hint::resolve_document_hint,
    services::query::assistant_grounding::AssistantGroundingDocumentReference,
    services::query::execution::{
        QueryChunkReferenceSnapshot, RuntimeMatchedEntity, RuntimeMatchedRelationship,
    },
    shared::extraction::technical_facts::{TechnicalFactKind, TechnicalFactQualifier},
};

const CONTEXT_BUNDLE_GRAPH_SEED_LIMIT: usize = 4;
const CONTEXT_BUNDLE_GRAPH_TRAVERSAL_DEPTH: usize = 1;
const CONTEXT_BUNDLE_GRAPH_TRAVERSAL_LIMIT_MULTIPLIER: usize = 2;

pub(crate) struct AssembleContextBundleRequest<'a> {
    pub(crate) state: &'a AppState,
    pub(crate) conversation: &'a query_repository::QueryConversationRow,
    pub(crate) execution_id: Uuid,
    pub(crate) bundle_id: Uuid,
    pub(crate) query_text: &'a str,
    pub(crate) query_ir: &'a crate::domains::query_ir::QueryIR,
    pub(crate) requested_mode: RuntimeQueryMode,
    pub(crate) top_k: usize,
    pub(crate) include_debug: bool,
    pub(crate) resolved_mode: RuntimeQueryMode,
    pub(crate) answer_chunk_references: &'a [QueryChunkReferenceSnapshot],
    pub(crate) answer_entity_references: &'a [RuntimeMatchedEntity],
    pub(crate) answer_relation_references: &'a [RuntimeMatchedRelationship],
}

fn merge_lexical_references(
    fact_hits: &[crate::infra::knowledge_rows::KnowledgeTechnicalFactSearchRow],
    entity_hits: &[crate::infra::knowledge_rows::KnowledgeEntitySearchRow],
    relation_hits: &[crate::infra::knowledge_rows::KnowledgeRelationSearchRow],
    fact_refs: &mut HashMap<Uuid, RankedBundleReference>,
    entity_refs: &mut HashMap<Uuid, RankedBundleReference>,
    relation_refs: &mut HashMap<Uuid, RankedBundleReference>,
) {
    for (index, hit) in fact_hits.iter().enumerate() {
        merge_ranked_reference(
            fact_refs,
            hit.fact_id,
            saturating_rank(index),
            hit.score,
            if hit.exact_match { "lexical_fact_exact" } else { "lexical_fact" },
        );
    }
    for (index, hit) in entity_hits.iter().enumerate() {
        merge_ranked_reference(
            entity_refs,
            hit.entity_id,
            saturating_rank(index),
            hit.score,
            "lexical_entity",
        );
    }
    for (index, hit) in relation_hits.iter().enumerate() {
        merge_ranked_reference(
            relation_refs,
            hit.relation_id,
            saturating_rank(index),
            hit.score,
            "lexical_relation",
        );
    }
}

struct ContextDebugTrace<'a> {
    state: &'a AppState,
    conversation: &'a query_repository::QueryConversationRow,
    execution_id: Uuid,
    bundle_id: Uuid,
    include_debug: bool,
    started_at: Instant,
    candidate_summary: serde_json::Value,
    assembly_diagnostics: serde_json::Value,
    retrieval_strategy: String,
    now: chrono::DateTime<Utc>,
}

async fn persist_context_debug_trace(trace_input: ContextDebugTrace<'_>) -> anyhow::Result<()> {
    let ContextDebugTrace {
        state,
        conversation,
        execution_id,
        bundle_id,
        include_debug,
        started_at,
        candidate_summary,
        assembly_diagnostics,
        retrieval_strategy,
        now,
    } = trace_input;
    if !include_debug {
        return Ok(());
    }
    let trace = KnowledgeRetrievalTraceRow {
        trace_id: bundle_id,
        workspace_id: conversation.workspace_id,
        library_id: conversation.library_id,
        query_execution_id: Some(execution_id),
        bundle_id,
        trace_state: "ready".to_string(),
        retrieval_strategy,
        candidate_counts: candidate_summary,
        dropped_reasons: json!([]),
        timing_breakdown: json!({
            "bundleAssemblyMs": started_at.elapsed().as_millis(),
        }),
        diagnostics_json: assembly_diagnostics,
        created_at: now,
        updated_at: now,
    };
    state
        .context_store
        .upsert_trace(&trace)
        .await
        .context("failed to upsert knowledge retrieval trace")?;
    Ok(())
}

/// Parallel entity-neighborhood fan-out. Each seed entity issues a single
/// graph traversal; they're fully independent, so running them sequentially
/// wastes wall-clock on N round-trips. `join_all` pins the batch at
/// max(per-entity latency).
async fn load_entity_neighborhoods(
    state: &AppState,
    library_id: Uuid,
    entity_seed_ids: &[Uuid],
    graph_traversal_limit: usize,
) -> anyhow::Result<Vec<Vec<KnowledgeGraphTraversalRow>>> {
    let futures = entity_seed_ids.iter().map(|entity_id| {
        let entity_id = *entity_id;
        async move {
            state
                .graph_store
                .list_entity_neighborhood(
                    entity_id,
                    library_id,
                    CONTEXT_BUNDLE_GRAPH_TRAVERSAL_DEPTH,
                    graph_traversal_limit.min(super::MAX_DETAIL_GRAPH_NODE_REFERENCES),
                )
                .await
                .with_context(|| {
                    format!(
                        "failed to load entity neighborhood while assembling query context bundle for entity {entity_id}"
                    )
                })
        }
    });
    futures::future::try_join_all(futures).await
}

/// Serialises the entity-neighborhood merge so rank indices stay stable.
fn absorb_entity_neighborhoods(
    neighborhoods: Vec<Vec<KnowledgeGraphTraversalRow>>,
    entity_refs: &mut HashMap<Uuid, RankedBundleReference>,
    relation_refs: &mut HashMap<Uuid, RankedBundleReference>,
    evidence_refs: &mut HashMap<Uuid, RankedBundleReference>,
) -> usize {
    let mut entity_neighborhood_rows = 0usize;
    for neighborhood in neighborhoods {
        entity_neighborhood_rows = entity_neighborhood_rows.saturating_add(neighborhood.len());
        for row in neighborhood {
            absorb_traversal_row(
                &row,
                entity_refs,
                relation_refs,
                evidence_refs,
                "entity_neighborhood",
            );
        }
    }
    entity_neighborhood_rows
}

/// Parallel relation fan-out. For each seed relation we issue two independent
/// graph-store calls — `expand_relation_centric` and
/// `list_relation_evidence_lookup` — in parallel, and the outer `join_all`
/// collapses every seed into one max-latency batch rather than paying 2 × N
/// round-trips serially.
async fn load_relation_batches(
    state: &AppState,
    library_id: Uuid,
    relation_seed_ids: &[Uuid],
    graph_traversal_limit: usize,
    candidate_limit: usize,
) -> anyhow::Result<
    Vec<(
        Uuid,
        Vec<KnowledgeGraphTraversalRow>,
        Vec<crate::infra::knowledge_rows::KnowledgeRelationEvidenceLookupRow>,
    )>,
> {
    let futures = relation_seed_ids.iter().map(|relation_id| {
        let relation_id = *relation_id;
        async move {
            let expand_future = state.graph_store.expand_relation_centric(
                relation_id,
                library_id,
                CONTEXT_BUNDLE_GRAPH_TRAVERSAL_DEPTH,
                graph_traversal_limit,
            );
            let evidence_future = state.graph_store.list_relation_evidence_lookup(
                relation_id,
                library_id,
                candidate_limit,
            );
            let (traversal, evidence_lookup) = tokio::join!(expand_future, evidence_future);
            let traversal = traversal.with_context(|| {
                format!(
                    "failed to expand relation-centric neighborhood while assembling query context bundle for relation {relation_id}"
                )
            })?;
            let evidence_lookup = evidence_lookup.with_context(|| {
                format!(
                    "failed to load relation evidence lookup while assembling query context bundle for relation {relation_id}"
                )
            })?;
            Ok::<_, anyhow::Error>((relation_id, traversal, evidence_lookup))
        }
    });
    futures::future::try_join_all(futures).await
}

fn absorb_relation_batches(
    batches: Vec<(
        Uuid,
        Vec<KnowledgeGraphTraversalRow>,
        Vec<crate::infra::knowledge_rows::KnowledgeRelationEvidenceLookupRow>,
    )>,
    entity_refs: &mut HashMap<Uuid, RankedBundleReference>,
    relation_refs: &mut HashMap<Uuid, RankedBundleReference>,
    evidence_refs: &mut HashMap<Uuid, RankedBundleReference>,
) -> (usize, usize) {
    let mut relation_traversal_rows = 0usize;
    let mut relation_evidence_rows = 0usize;
    for (_relation_id, traversal, evidence_lookup) in batches {
        relation_traversal_rows = relation_traversal_rows.saturating_add(traversal.len());
        for row in traversal {
            absorb_traversal_row(
                &row,
                entity_refs,
                relation_refs,
                evidence_refs,
                "relation_traversal",
            );
        }
        relation_evidence_rows = relation_evidence_rows.saturating_add(evidence_lookup.len());
        for (index, row) in evidence_lookup.into_iter().enumerate() {
            merge_ranked_reference(
                relation_refs,
                row.relation.relation_id,
                saturating_rank(index),
                row.support_edge_score.unwrap_or_default(),
                "relation_provenance",
            );
            merge_ranked_reference(
                evidence_refs,
                row.evidence.evidence_id,
                saturating_rank(index),
                row.support_edge_score.unwrap_or_default(),
                "relation_evidence",
            );
        }
    }
    (relation_traversal_rows, relation_evidence_rows)
}

struct ContextBundleAssemblyCounts {
    chunk_refs: usize,
    entity_refs: usize,
    relation_refs: usize,
    evidence_refs: usize,
}

fn log_context_bundle_assembled(
    execution_id: Uuid,
    bundle_id: Uuid,
    library_id: Uuid,
    started_at: Instant,
    counts: ContextBundleAssemblyCounts,
) {
    let ContextBundleAssemblyCounts { chunk_refs, entity_refs, relation_refs, evidence_refs } =
        counts;
    tracing::info!(
        stage = "query.context_bundle.assembled",
        %execution_id,
        %bundle_id,
        library_id = %library_id,
        elapsed_ms = started_at.elapsed().as_millis(),
        chunk_refs,
        entity_refs,
        relation_refs,
        evidence_refs,
        "assembled query context bundle"
    );
}

struct ContextBundlePersistence<'a> {
    library_id: Uuid,
    bundle_row: &'a KnowledgeContextBundleRow,
    chunk_edges: &'a [KnowledgeBundleChunkEdgeRow],
    entity_edges: &'a [KnowledgeBundleEntityEdgeRow],
    relation_edges: &'a [KnowledgeBundleRelationEdgeRow],
    evidence_edges: &'a [KnowledgeBundleEvidenceEdgeRow],
}

async fn persist_context_bundle(
    state: &AppState,
    persistence: ContextBundlePersistence<'_>,
) -> anyhow::Result<()> {
    let ContextBundlePersistence {
        library_id,
        bundle_row,
        chunk_edges,
        entity_edges,
        relation_edges,
        evidence_edges,
    } = persistence;
    let bundle_id = bundle_row.bundle_id;
    state
        .context_store
        .upsert_bundle(bundle_row)
        .await
        .context("failed to upsert knowledge context bundle document")?;
    state
        .context_store
        .replace_bundle_chunk_edges(bundle_id, library_id, chunk_edges)
        .await
        .context("failed to replace bundle chunk edges")?;
    state
        .context_store
        .replace_bundle_entity_edges(bundle_id, library_id, entity_edges)
        .await
        .context("failed to replace bundle entity edges")?;
    state
        .context_store
        .replace_bundle_relation_edges(bundle_id, library_id, relation_edges)
        .await
        .context("failed to replace bundle relation edges")?;
    state
        .context_store
        .replace_bundle_evidence_edges(bundle_id, library_id, evidence_edges)
        .await
        .context("failed to replace bundle evidence edges")?;
    Ok(())
}

pub(crate) async fn assemble_context_bundle(
    request: AssembleContextBundleRequest<'_>,
) -> anyhow::Result<()> {
    let AssembleContextBundleRequest {
        state,
        conversation,
        execution_id,
        bundle_id,
        query_text,
        query_ir,
        requested_mode,
        top_k,
        include_debug,
        resolved_mode,
        answer_chunk_references,
        answer_entity_references,
        answer_relation_references,
    } = request;
    let started_at = Instant::now();
    let candidate_limit = top_k.saturating_mul(3).max(6);

    // AssembleContext is a reference/audit materialization step. The
    // answer evidence has already been selected by `prepare_answer_query`,
    // so this stage must not issue a second provider embedding request on
    // the user-visible critical path.
    let lexical_search = state
        .application_services
        .search
        .search_query_evidence(
            state,
            conversation.library_id,
            query_text,
            query_ir,
            candidate_limit,
        )
        .await
        .context("failed primary lexical evidence search while assembling query context bundle")?;
    let lexical_entity_hits = lexical_search.entity_hits;
    let lexical_relation_hits = lexical_search.relation_hits;
    let lexical_fact_hits = lexical_search.technical_fact_hits;
    let exact_literal_bias = lexical_search.exact_literal_bias;

    let chunk_refs = seed_chunk_refs_from_answer_context(answer_chunk_references);
    let mut fact_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut entity_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut relation_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut evidence_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    seed_entity_refs_from_answer_graph(answer_entity_references, &mut entity_refs);
    seed_relation_endpoint_refs_from_answer_graph(answer_relation_references, &mut entity_refs);
    seed_relation_refs_from_answer_graph(answer_relation_references, &mut relation_refs);

    merge_lexical_references(
        &lexical_fact_hits,
        &lexical_entity_hits,
        &lexical_relation_hits,
        &mut fact_refs,
        &mut entity_refs,
        &mut relation_refs,
    );

    let graph_seed_limit = top_k.clamp(3, CONTEXT_BUNDLE_GRAPH_SEED_LIMIT);
    let graph_traversal_limit = candidate_limit
        .saturating_mul(CONTEXT_BUNDLE_GRAPH_TRAVERSAL_LIMIT_MULTIPLIER)
        .max(candidate_limit)
        .min(super::MAX_DETAIL_GRAPH_EDGE_REFERENCES);
    let entity_seed_ids = top_ranked_ids(&entity_refs, graph_seed_limit);
    let entity_neighborhoods = load_entity_neighborhoods(
        state,
        conversation.library_id,
        &entity_seed_ids,
        graph_traversal_limit,
    )
    .await?;
    let entity_neighborhood_rows = absorb_entity_neighborhoods(
        entity_neighborhoods,
        &mut entity_refs,
        &mut relation_refs,
        &mut evidence_refs,
    );

    let relation_seed_ids = top_ranked_ids(&relation_refs, graph_seed_limit);
    let relation_batches = load_relation_batches(
        state,
        conversation.library_id,
        &relation_seed_ids,
        graph_traversal_limit,
        candidate_limit,
    )
    .await?;
    let (relation_traversal_rows, relation_evidence_rows) = absorb_relation_batches(
        relation_batches,
        &mut entity_refs,
        &mut relation_refs,
        &mut evidence_refs,
    );

    let evidence_rows = state
        .graph_store
        .list_evidence_by_ids(&top_ranked_ids(&evidence_refs, candidate_limit * 4))
        .await
        .context("failed to load evidence rows while assembling query context bundle")?;
    for evidence in &evidence_rows {
        if let Some(fact_id) = evidence.fact_id {
            merge_ranked_reference(
                &mut fact_refs,
                fact_id,
                evidence_rank_for_bundle(&evidence_refs, evidence.evidence_id),
                evidence_score_for_bundle(&evidence_refs, evidence.evidence_id),
                "evidence_fact",
            );
        }
    }

    let mut fact_rows = state
        .document_store
        .list_technical_facts_by_ids(&top_ranked_ids(&fact_refs, candidate_limit * 3))
        .await
        .context("failed to load technical facts while assembling query context bundle")?;
    let now = Utc::now();
    let generations = state
        .application_services
        .knowledge
        .derive_library_generation_rows(state, conversation.library_id)
        .await
        .map_err(|error| {
            anyhow::anyhow!(
                "failed to derive library generations while assembling query context bundle: {error}"
            )
        })?;
    let generation = generations.first().cloned();
    let freshness_snapshot = generation.as_ref().map_or_else(|| json!({}), freshness_snapshot_json);
    let retrieval_strategy = "reference_lexical".to_string();
    let chunk_edges = build_chunk_bundle_edges(bundle_id, &chunk_refs, now);
    let entity_edges = build_entity_bundle_edges(bundle_id, &entity_refs, now);
    let relation_edges = build_relation_bundle_edges(bundle_id, &relation_refs, now);
    let evidence_edges = build_evidence_bundle_edges(bundle_id, &evidence_refs, now);
    let selected_chunk_rows = if chunk_refs.is_empty() {
        Vec::new()
    } else {
        state
            .document_store
            .list_chunks_by_ids(&top_ranked_ids(&chunk_refs, candidate_limit * 3))
            .await
            .context("failed to load chunk rows while assembling query context bundle")?
    };
    let chunk_supported_fact_rows = if selected_chunk_rows.is_empty() {
        Vec::new()
    } else {
        state
            .document_store
            .list_technical_facts_by_chunk_ids(
                &selected_chunk_rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            )
            .await
            .context("failed to load technical facts by chunk support while assembling query context bundle")?
    };
    let provisional_bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: conversation.workspace_id,
            library_id: conversation.library_id,
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: retrieval_strategy.clone(),
            requested_mode: runtime_mode_label(requested_mode).to_string(),
            resolved_mode: runtime_mode_label(resolved_mode).to_string(),
            selected_fact_ids: Vec::new(),
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: freshness_snapshot.clone(),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: now,
            updated_at: now,
        },
        chunk_references: chunk_edges
            .iter()
            .map(|edge| crate::infra::knowledge_rows::KnowledgeBundleChunkReferenceRow {
                bundle_id: edge.bundle_id,
                chunk_id: edge.chunk_id,
                rank: edge.rank,
                score: edge.score,
                inclusion_reason: edge.inclusion_reason.clone(),
                created_at: edge.created_at,
            })
            .collect(),
        entity_references: entity_edges
            .iter()
            .map(|edge| crate::infra::knowledge_rows::KnowledgeBundleEntityReferenceRow {
                bundle_id: edge.bundle_id,
                entity_id: edge.entity_id,
                rank: edge.rank,
                score: edge.score,
                inclusion_reason: edge.inclusion_reason.clone(),
                created_at: edge.created_at,
            })
            .collect(),
        relation_references: relation_edges
            .iter()
            .map(|edge| crate::infra::knowledge_rows::KnowledgeBundleRelationReferenceRow {
                bundle_id: edge.bundle_id,
                relation_id: edge.relation_id,
                rank: edge.rank,
                score: edge.score,
                inclusion_reason: edge.inclusion_reason.clone(),
                created_at: edge.created_at,
            })
            .collect(),
        evidence_references: evidence_edges
            .iter()
            .map(|edge| crate::infra::knowledge_rows::KnowledgeBundleEvidenceReferenceRow {
                bundle_id: edge.bundle_id,
                evidence_id: edge.evidence_id,
                rank: edge.rank,
                score: edge.score,
                inclusion_reason: edge.inclusion_reason.clone(),
                created_at: edge.created_at,
            })
            .collect(),
    };
    augment_fact_rank_refs_from_chunk_support(
        Some(&provisional_bundle),
        &chunk_supported_fact_rows,
        &mut fact_refs,
    );
    merge_technical_fact_rows(&mut fact_rows, &chunk_supported_fact_rows);
    let selected_fact_ids = select_diversified_fact_ids(&fact_refs, &fact_rows, top_k.max(6));
    let block_rank_refs = derive_block_rank_refs(
        &KnowledgeContextBundleReferenceSetRow {
            bundle: KnowledgeContextBundleRow {
                selected_fact_ids: selected_fact_ids.clone(),
                ..provisional_bundle.bundle.clone()
            },
            ..provisional_bundle
        },
        &evidence_rows,
        &fact_rows,
        &selected_chunk_rows,
    );

    let candidate_summary = json!({
        "answerContextChunkReferences": answer_chunk_references.len(),
        "graphSeedLimit": graph_seed_limit,
        "graphTraversalDepth": CONTEXT_BUNDLE_GRAPH_TRAVERSAL_DEPTH,
        "graphTraversalLimit": graph_traversal_limit,
        "lexicalFactHits": lexical_fact_hits.len(),
        "lexicalEntityHits": lexical_entity_hits.len(),
        "vectorEntityHits": 0,
        "lexicalRelationHits": lexical_relation_hits.len(),
        "exactLiteralBias": exact_literal_bias,
        "entityNeighborhoodRows": entity_neighborhood_rows,
        "relationTraversalRows": relation_traversal_rows,
        "relationEvidenceRows": relation_evidence_rows,
        "evidenceRows": evidence_rows.len(),
        "factRows": fact_rows.len(),
        "finalChunkReferences": chunk_edges.len(),
        "finalPreparedSegmentReferences": block_rank_refs.len(),
        "finalTechnicalFactReferences": selected_fact_ids.len(),
        "finalEntityReferences": entity_edges.len(),
        "finalRelationReferences": relation_edges.len(),
        "finalEvidenceReferences": evidence_edges.len(),
    });
    let assembly_diagnostics = json!({
        "requestedMode": runtime_mode_label(requested_mode),
        "resolvedMode": runtime_mode_label(resolved_mode),
        "candidateLimit": candidate_limit,
        "graphSeedLimit": graph_seed_limit,
        "graphTraversalDepth": CONTEXT_BUNDLE_GRAPH_TRAVERSAL_DEPTH,
        "graphTraversalLimit": graph_traversal_limit,
        "vectorCandidateLimit": 0,
        "vectorEnabled": false,
        "exactLiteralBias": exact_literal_bias,
        "bundleId": bundle_id,
        "queryExecutionId": execution_id,
        "queryIr": query_ir,
    });

    let bundle_row = KnowledgeContextBundleRow {
        bundle_id,
        workspace_id: conversation.workspace_id,
        library_id: conversation.library_id,
        query_execution_id: Some(execution_id),
        bundle_state: "ready".to_string(),
        bundle_strategy: retrieval_strategy.clone(),
        requested_mode: runtime_mode_label(requested_mode).to_string(),
        resolved_mode: runtime_mode_label(resolved_mode).to_string(),
        selected_fact_ids,
        verification_state: "not_run".to_string(),
        verification_warnings: json!([]),
        freshness_snapshot: freshness_snapshot.clone(),
        candidate_summary: candidate_summary.clone(),
        assembly_diagnostics: assembly_diagnostics.clone(),
        created_at: now,
        updated_at: now,
    };
    persist_context_bundle(
        state,
        ContextBundlePersistence {
            library_id: conversation.library_id,
            bundle_row: &bundle_row,
            chunk_edges: &chunk_edges,
            entity_edges: &entity_edges,
            relation_edges: &relation_edges,
            evidence_edges: &evidence_edges,
        },
    )
    .await?;

    persist_context_debug_trace(ContextDebugTrace {
        state,
        conversation,
        execution_id,
        bundle_id,
        include_debug,
        started_at,
        candidate_summary,
        assembly_diagnostics,
        retrieval_strategy,
        now,
    })
    .await?;

    log_context_bundle_assembled(
        execution_id,
        bundle_id,
        conversation.library_id,
        started_at,
        ContextBundleAssemblyCounts {
            chunk_refs: chunk_edges.len(),
            entity_refs: entity_edges.len(),
            relation_refs: relation_edges.len(),
            evidence_refs: evidence_edges.len(),
        },
    );

    Ok(())
}
