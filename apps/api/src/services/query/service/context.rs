use std::collections::HashMap;

use anyhow::Context;
use chrono::Utc;
use serde_json::json;
use std::time::Instant;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::query::RuntimeQueryMode,
    infra::{
        arangodb::{
            collections::{
                KNOWLEDGE_CHUNK_COLLECTION, KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION,
                KNOWLEDGE_ENTITY_COLLECTION, KNOWLEDGE_EVIDENCE_COLLECTION,
                KNOWLEDGE_RELATION_COLLECTION,
            },
            context_store::{
                KnowledgeBundleChunkEdgeRow, KnowledgeBundleEntityEdgeRow,
                KnowledgeBundleEvidenceEdgeRow, KnowledgeBundleRelationEdgeRow,
                KnowledgeContextBundleReferenceSetRow, KnowledgeContextBundleRow,
                KnowledgeRetrievalTraceRow,
            },
            document_store::{KnowledgeLibraryGenerationRow, KnowledgeTechnicalFactRow},
            graph_store::{KnowledgeEvidenceRow, KnowledgeGraphTraversalRow},
        },
        repositories::query_repository,
    },
    integrations::llm::EmbeddingRequest,
    interfaces::http::router_support::ApiError,
    services::query::assistant_grounding::AssistantGroundingDocumentReference,
};

use super::{
    ExecutionPreparedReferenceContext, PreparedSegmentRevisionInfo, QueryEmbeddingContext,
    RankedBundleReference, merge_ranked_reference, runtime_mode_label, saturating_rank,
    top_ranked_ids,
};

pub(crate) async fn assemble_context_bundle(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    execution_id: Uuid,
    bundle_id: Uuid,
    query_text: &str,
    requested_mode: RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
    resolved_mode: RuntimeQueryMode,
) -> anyhow::Result<()> {
    let started_at = Instant::now();
    let candidate_limit = top_k.saturating_mul(3).max(6);
    let lexical_search = state
        .canonical_services
        .search
        .search_query_evidence(state, conversation.library_id, query_text, candidate_limit)
        .await
        .context(
            "failed canonical lexical evidence search while assembling query context bundle",
        )?;
    let lexical_chunk_hits = lexical_search.chunk_hits;
    let lexical_entity_hits = lexical_search.entity_hits;
    let lexical_relation_hits = lexical_search.relation_hits;
    let lexical_fact_hits = lexical_search.technical_fact_hits;
    let exact_literal_bias = lexical_search.exact_literal_bias;

    let embedding_context =
        match resolve_query_embedding_context(state, conversation.library_id, query_text).await {
            Ok(context) => context,
            Err(error) => {
                warn!(
                    error = %error,
                    library_id = %conversation.library_id,
                    execution_id = %execution_id,
                    "canonical query bundle fell back to lexical retrieval"
                );
                None
            }
        };

    let vector_limit = candidate_limit.saturating_mul(2).max(8);
    let vector_chunk_hits = if let Some(context) = embedding_context.as_ref() {
        state
            .arango_search_store
            .search_chunk_vectors_by_similarity(
                conversation.library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                &context.query_vector,
                vector_limit,
                Some(16),
            )
            .await
            .context("failed vector chunk search while assembling query context bundle")?
    } else {
        Vec::new()
    };
    let vector_entity_hits = if let Some(context) = embedding_context.as_ref() {
        state
            .arango_search_store
            .search_entity_vectors_by_similarity(
                conversation.library_id,
                &context.model_catalog_id.to_string(),
                context.freshness_generation,
                &context.query_vector,
                vector_limit,
                Some(16),
            )
            .await
            .context("failed vector entity search while assembling query context bundle")?
    } else {
        Vec::new()
    };

    let mut chunk_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut fact_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut entity_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut relation_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
    let mut evidence_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();

    for (index, hit) in lexical_chunk_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut chunk_refs,
            hit.chunk_id,
            saturating_rank(index),
            hit.score,
            "lexical_chunk",
        );
    }
    for (index, hit) in lexical_fact_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut fact_refs,
            hit.fact_id,
            saturating_rank(index),
            hit.score,
            if hit.exact_match { "lexical_fact_exact" } else { "lexical_fact" },
        );
    }
    for (index, hit) in vector_chunk_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut chunk_refs,
            hit.chunk_id,
            saturating_rank(index),
            hit.score,
            "vector_chunk",
        );
    }
    for (index, hit) in lexical_entity_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut entity_refs,
            hit.entity_id,
            saturating_rank(index),
            hit.score,
            "lexical_entity",
        );
    }
    for (index, hit) in vector_entity_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut entity_refs,
            hit.entity_id,
            saturating_rank(index),
            hit.score,
            "vector_entity",
        );
    }
    for (index, hit) in lexical_relation_hits.iter().enumerate() {
        merge_ranked_reference(
            &mut relation_refs,
            hit.relation_id,
            saturating_rank(index),
            hit.score,
            "lexical_relation",
        );
    }

    let entity_seed_ids = top_ranked_ids(&entity_refs, top_k.max(3));
    let mut entity_neighborhood_rows = 0usize;
    for entity_id in entity_seed_ids {
        let neighborhood = state
            .arango_graph_store
            .list_entity_neighborhood(entity_id, conversation.library_id, 2, candidate_limit * 4)
            .await
            .with_context(|| {
                format!(
                    "failed to load entity neighborhood while assembling query context bundle for entity {entity_id}"
                )
            })?;
        entity_neighborhood_rows = entity_neighborhood_rows.saturating_add(neighborhood.len());
        for row in neighborhood {
            absorb_traversal_row(
                &row,
                &mut chunk_refs,
                &mut entity_refs,
                &mut relation_refs,
                &mut evidence_refs,
                "entity_neighborhood",
            );
        }
    }

    let relation_seed_ids = top_ranked_ids(&relation_refs, top_k.max(3));
    let mut relation_traversal_rows = 0usize;
    let mut relation_evidence_rows = 0usize;
    for relation_id in relation_seed_ids {
        let traversal = state
            .arango_graph_store
            .expand_relation_centric(relation_id, conversation.library_id, 2, candidate_limit * 4)
            .await
            .with_context(|| {
                format!(
                    "failed to expand relation-centric neighborhood while assembling query context bundle for relation {relation_id}"
                )
            })?;
        relation_traversal_rows = relation_traversal_rows.saturating_add(traversal.len());
        for row in traversal {
            absorb_traversal_row(
                &row,
                &mut chunk_refs,
                &mut entity_refs,
                &mut relation_refs,
                &mut evidence_refs,
                "relation_traversal",
            );
        }

        let evidence_lookup = state
            .arango_graph_store
            .list_relation_evidence_lookup(relation_id, conversation.library_id, candidate_limit)
            .await
            .with_context(|| {
                format!(
                    "failed to load relation evidence lookup while assembling query context bundle for relation {relation_id}"
                )
            })?;
        relation_evidence_rows = relation_evidence_rows.saturating_add(evidence_lookup.len());
        for (index, row) in evidence_lookup.into_iter().enumerate() {
            merge_ranked_reference(
                &mut relation_refs,
                row.relation.relation_id,
                saturating_rank(index),
                row.support_edge_score.unwrap_or_default(),
                "relation_provenance",
            );
            merge_ranked_reference(
                &mut evidence_refs,
                row.evidence.evidence_id,
                saturating_rank(index),
                row.support_edge_score.unwrap_or_default(),
                "relation_evidence",
            );
            if let Some(chunk) = row.source_chunk {
                merge_ranked_reference(
                    &mut chunk_refs,
                    chunk.chunk_id,
                    saturating_rank(index),
                    row.support_edge_score.unwrap_or_default(),
                    "evidence_source",
                );
            }
        }
    }

    let evidence_rows = state
        .arango_graph_store
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
        .arango_document_store
        .list_technical_facts_by_ids(&top_ranked_ids(&fact_refs, candidate_limit * 3))
        .await
        .context("failed to load technical facts while assembling query context bundle")?;
    for fact in &fact_rows {
        let rank = fact_rank_for_bundle(&fact_refs, fact.fact_id);
        let score = fact_score_for_bundle(&fact_refs, fact.fact_id);
        for chunk_id in &fact.support_chunk_ids {
            merge_ranked_reference(
                &mut chunk_refs,
                *chunk_id,
                rank,
                score,
                "technical_fact_support",
            );
        }
    }

    let now = Utc::now();
    let generations = state
        .canonical_services
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
    let retrieval_strategy =
        if embedding_context.is_some() { "hybrid".to_string() } else { "lexical".to_string() };
    let chunk_edges = build_chunk_bundle_edges(bundle_id, &chunk_refs, now);
    let entity_edges = build_entity_bundle_edges(bundle_id, &entity_refs, now);
    let relation_edges = build_relation_bundle_edges(bundle_id, &relation_refs, now);
    let evidence_edges = build_evidence_bundle_edges(bundle_id, &evidence_refs, now);
    let selected_chunk_rows = if chunk_refs.is_empty() {
        Vec::new()
    } else {
        state
            .arango_document_store
            .list_chunks_by_ids(&top_ranked_ids(&chunk_refs, candidate_limit * 3))
            .await
            .context("failed to load chunk rows while assembling query context bundle")?
    };
    let mut fact_refs = fact_refs;
    let chunk_supported_fact_rows = if selected_chunk_rows.is_empty() {
        Vec::new()
    } else {
        state
            .arango_document_store
            .list_technical_facts_by_chunk_ids(
                &selected_chunk_rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            )
            .await
            .context("failed to load technical facts by chunk support while assembling query context bundle")?
    };
    let provisional_bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
            .map(|edge| crate::infra::arangodb::context_store::KnowledgeBundleChunkReferenceRow {
                key: edge.key.clone(),
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
            .map(|edge| crate::infra::arangodb::context_store::KnowledgeBundleEntityReferenceRow {
                key: edge.key.clone(),
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
            .map(|edge| {
                crate::infra::arangodb::context_store::KnowledgeBundleRelationReferenceRow {
                    key: edge.key.clone(),
                    bundle_id: edge.bundle_id,
                    relation_id: edge.relation_id,
                    rank: edge.rank,
                    score: edge.score,
                    inclusion_reason: edge.inclusion_reason.clone(),
                    created_at: edge.created_at,
                }
            })
            .collect(),
        evidence_references: evidence_edges
            .iter()
            .map(|edge| {
                crate::infra::arangodb::context_store::KnowledgeBundleEvidenceReferenceRow {
                    key: edge.key.clone(),
                    bundle_id: edge.bundle_id,
                    evidence_id: edge.evidence_id,
                    rank: edge.rank,
                    score: edge.score,
                    inclusion_reason: edge.inclusion_reason.clone(),
                    created_at: edge.created_at,
                }
            })
            .collect(),
    };
    augment_fact_rank_refs_from_chunk_support(
        Some(&provisional_bundle),
        &chunk_supported_fact_rows,
        &mut fact_refs,
    );
    merge_technical_fact_rows(&mut fact_rows, &chunk_supported_fact_rows);
    let selected_fact_ids = top_ranked_ids(&fact_refs, top_k.max(6));
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
        "lexicalChunkHits": lexical_chunk_hits.len(),
        "lexicalFactHits": lexical_fact_hits.len(),
        "vectorChunkHits": vector_chunk_hits.len(),
        "lexicalEntityHits": lexical_entity_hits.len(),
        "vectorEntityHits": vector_entity_hits.len(),
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
        "vectorCandidateLimit": vector_limit,
        "vectorEnabled": embedding_context.is_some(),
        "exactLiteralBias": exact_literal_bias,
        "bundleId": bundle_id,
        "queryExecutionId": execution_id,
    });

    let bundle_row = KnowledgeContextBundleRow {
        key: bundle_id.to_string(),
        arango_id: None,
        arango_rev: None,
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
    state
        .arango_context_store
        .upsert_bundle(&bundle_row)
        .await
        .context("failed to upsert knowledge context bundle document")?;
    state
        .arango_context_store
        .replace_bundle_chunk_edges(bundle_id, conversation.library_id, &chunk_edges)
        .await
        .context("failed to replace bundle chunk edges")?;
    state
        .arango_context_store
        .replace_bundle_entity_edges(bundle_id, conversation.library_id, &entity_edges)
        .await
        .context("failed to replace bundle entity edges")?;
    state
        .arango_context_store
        .replace_bundle_relation_edges(bundle_id, conversation.library_id, &relation_edges)
        .await
        .context("failed to replace bundle relation edges")?;
    state
        .arango_context_store
        .replace_bundle_evidence_edges(bundle_id, conversation.library_id, &evidence_edges)
        .await
        .context("failed to replace bundle evidence edges")?;

    if include_debug {
        let trace = KnowledgeRetrievalTraceRow {
            key: bundle_id.to_string(),
            arango_id: None,
            arango_rev: None,
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
            .arango_context_store
            .upsert_trace(&trace)
            .await
            .context("failed to upsert knowledge retrieval trace")?;
    }

    Ok(())
}

async fn resolve_query_embedding_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<Option<QueryEmbeddingContext>, ApiError> {
    let binding = state
        .canonical_services
        .ai_catalog
        .resolve_active_runtime_binding(state, library_id, AiBindingPurpose::EmbedChunk)
        .await?;
    let Some(binding) = binding else {
        return Ok(None);
    };

    let generations = state
        .canonical_services
        .knowledge
        .derive_library_generation_rows(state, library_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let Some(generation) = generations.first() else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    let embedding = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind: binding.provider_kind.clone(),
            model_name: binding.model_name.clone(),
            input: query_text.to_string(),
            api_key_override: binding.api_key.clone(),
            base_url_override: binding.provider_base_url,
        })
        .await
        .map_err(|error| {
            ApiError::ProviderFailure(format!("failed to embed query bundle request: {error}"))
        })?;

    Ok(Some(QueryEmbeddingContext {
        model_catalog_id: binding.model_catalog_id,
        freshness_generation: generation.active_vector_generation,
        query_vector: embedding.embedding,
    }))
}

fn absorb_traversal_row(
    row: &KnowledgeGraphTraversalRow,
    chunk_refs: &mut HashMap<Uuid, RankedBundleReference>,
    entity_refs: &mut HashMap<Uuid, RankedBundleReference>,
    relation_refs: &mut HashMap<Uuid, RankedBundleReference>,
    evidence_refs: &mut HashMap<Uuid, RankedBundleReference>,
    reason: &str,
) {
    let rank = traversal_rank(row.path_length);
    let score = row.edge_score.unwrap_or_else(|| traversal_score(row.path_length));
    match row.vertex_kind.as_str() {
        KNOWLEDGE_CHUNK_COLLECTION => {
            merge_ranked_reference(chunk_refs, row.vertex_id, rank, score, reason);
        }
        KNOWLEDGE_ENTITY_COLLECTION => {
            merge_ranked_reference(entity_refs, row.vertex_id, rank, score, reason);
        }
        KNOWLEDGE_RELATION_COLLECTION => {
            merge_ranked_reference(relation_refs, row.vertex_id, rank, score, reason);
        }
        KNOWLEDGE_EVIDENCE_COLLECTION => {
            merge_ranked_reference(evidence_refs, row.vertex_id, rank, score, reason);
        }
        _ => {}
    }
}

fn traversal_rank(path_length: i64) -> i32 {
    i32::try_from(path_length.saturating_add(1)).unwrap_or(i32::MAX)
}

fn traversal_score(path_length: i64) -> f64 {
    match path_length {
        0 => 1.0,
        1 => 0.8,
        2 => 0.6,
        3 => 0.4,
        _ => 0.2,
    }
}

fn build_chunk_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleChunkEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(chunk_id, reference)| KnowledgeBundleChunkEdgeRow {
            key: format!("{bundle_id}:{chunk_id}"),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_CHUNK_COLLECTION}/{chunk_id}"),
            bundle_id,
            chunk_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

fn build_entity_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleEntityEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(entity_id, reference)| KnowledgeBundleEntityEdgeRow {
            key: format!("{bundle_id}:{entity_id}"),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_ENTITY_COLLECTION}/{entity_id}"),
            bundle_id,
            entity_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

fn build_relation_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleRelationEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(relation_id, reference)| KnowledgeBundleRelationEdgeRow {
            key: format!("{bundle_id}:{relation_id}"),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_RELATION_COLLECTION}/{relation_id}"),
            bundle_id,
            relation_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

fn build_evidence_bundle_edges(
    bundle_id: Uuid,
    refs: &HashMap<Uuid, RankedBundleReference>,
    created_at: chrono::DateTime<chrono::Utc>,
) -> Vec<KnowledgeBundleEvidenceEdgeRow> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, value)| (*id, value)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items
        .into_iter()
        .map(|(evidence_id, reference)| KnowledgeBundleEvidenceEdgeRow {
            key: format!("{bundle_id}:{evidence_id}"),
            arango_id: None,
            arango_rev: None,
            from: format!("{KNOWLEDGE_CONTEXT_BUNDLE_COLLECTION}/{bundle_id}"),
            to: format!("{KNOWLEDGE_EVIDENCE_COLLECTION}/{evidence_id}"),
            bundle_id,
            evidence_id,
            rank: reference.rank,
            score: reference.score,
            inclusion_reason: Some(reference.reasons.iter().cloned().collect::<Vec<_>>().join("+")),
            created_at,
        })
        .collect()
}

pub(crate) fn evidence_rank_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    evidence_id: Uuid,
) -> i32 {
    refs.get(&evidence_id).map_or(i32::MAX, |reference| reference.rank)
}

pub(crate) fn evidence_score_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    evidence_id: Uuid,
) -> f64 {
    refs.get(&evidence_id).map_or(0.0, |reference| reference.score)
}

pub(crate) fn fact_rank_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    fact_id: Uuid,
) -> i32 {
    refs.get(&fact_id).map_or(i32::MAX, |reference| reference.rank)
}

pub(crate) fn fact_score_for_bundle(
    refs: &HashMap<Uuid, RankedBundleReference>,
    fact_id: Uuid,
) -> f64 {
    refs.get(&fact_id).map_or(0.0, |reference| reference.score)
}

pub(crate) fn derive_chunk_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> HashMap<Uuid, RankedBundleReference> {
    let mut chunk_refs = HashMap::<Uuid, RankedBundleReference>::new();
    for reference in &bundle.chunk_references {
        merge_ranked_reference(
            &mut chunk_refs,
            reference.chunk_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_chunk"),
        );
    }
    chunk_refs
}

pub(crate) fn augment_fact_rank_refs_from_chunk_support(
    bundle: Option<&KnowledgeContextBundleReferenceSetRow>,
    technical_fact_rows: &[KnowledgeTechnicalFactRow],
    fact_rank_refs: &mut HashMap<Uuid, RankedBundleReference>,
) {
    let Some(bundle) = bundle else {
        return;
    };
    let chunk_rank_refs = derive_chunk_rank_refs(bundle);
    if chunk_rank_refs.is_empty() {
        return;
    }
    for fact in technical_fact_rows {
        let mut best_rank = None::<i32>;
        let mut best_score = 0.0_f64;
        for chunk_id in &fact.support_chunk_ids {
            let Some(reference) = chunk_rank_refs.get(chunk_id) else {
                continue;
            };
            best_rank = Some(best_rank.map_or(reference.rank, |rank| rank.min(reference.rank)));
            if reference.score > best_score {
                best_score = reference.score;
            }
        }
        let Some(rank) = best_rank else {
            continue;
        };
        merge_ranked_reference(
            fact_rank_refs,
            fact.fact_id,
            rank,
            best_score.max(1.0),
            "selected_chunk_support",
        );
    }
}

pub(crate) fn merge_technical_fact_rows(
    target: &mut Vec<KnowledgeTechnicalFactRow>,
    additional: &[KnowledgeTechnicalFactRow],
) {
    let mut seen = target.iter().map(|row| row.fact_id).collect::<std::collections::BTreeSet<_>>();
    for row in additional {
        if seen.insert(row.fact_id) {
            target.push(row.clone());
        }
    }
    target.sort_by(|left, right| {
        left.fact_kind.cmp(&right.fact_kind).then_with(|| left.fact_id.cmp(&right.fact_id))
    });
}

pub(crate) fn derive_fact_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    evidence_rows: &[KnowledgeEvidenceRow],
) -> HashMap<Uuid, RankedBundleReference> {
    let mut fact_refs = HashMap::<Uuid, RankedBundleReference>::new();
    let evidence_by_id = evidence_rows
        .iter()
        .map(|evidence| (evidence.evidence_id, evidence))
        .collect::<HashMap<_, _>>();
    for reference in &bundle.evidence_references {
        let Some(evidence) = evidence_by_id.get(&reference.evidence_id) else {
            continue;
        };
        let Some(fact_id) = evidence.fact_id else {
            continue;
        };
        merge_ranked_reference(
            &mut fact_refs,
            fact_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_evidence"),
        );
    }
    for (index, fact_id) in bundle.bundle.selected_fact_ids.iter().copied().enumerate() {
        let score = fact_refs.get(&fact_id).map_or(1.0, |reference| reference.score.max(1.0));
        merge_ranked_reference(
            &mut fact_refs,
            fact_id,
            saturating_rank(index),
            score,
            "bundle_selected_fact",
        );
    }
    fact_refs
}

pub(crate) fn derive_block_rank_refs(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    evidence_rows: &[KnowledgeEvidenceRow],
    technical_fact_rows: &[KnowledgeTechnicalFactRow],
    chunk_rows: &[crate::infra::arangodb::document_store::KnowledgeChunkRow],
) -> HashMap<Uuid, RankedBundleReference> {
    let mut block_refs = HashMap::<Uuid, RankedBundleReference>::new();
    let evidence_by_id = evidence_rows
        .iter()
        .map(|evidence| (evidence.evidence_id, evidence))
        .collect::<HashMap<_, _>>();
    for reference in &bundle.evidence_references {
        let Some(evidence) = evidence_by_id.get(&reference.evidence_id) else {
            continue;
        };
        let Some(block_id) = evidence.block_id else {
            continue;
        };
        merge_ranked_reference(
            &mut block_refs,
            block_id,
            reference.rank,
            reference.score,
            reference.inclusion_reason.as_deref().unwrap_or("bundle_evidence"),
        );
    }
    let fact_rank_refs = derive_fact_rank_refs(bundle, evidence_rows);
    for fact in technical_fact_rows {
        let rank = fact_rank_for_bundle(&fact_rank_refs, fact.fact_id);
        let score = fact_score_for_bundle(&fact_rank_refs, fact.fact_id).max(1.0);
        for block_id in &fact.support_block_ids {
            merge_ranked_reference(
                &mut block_refs,
                *block_id,
                rank,
                score,
                "technical_fact_support",
            );
        }
    }
    let chunk_rank_refs = derive_chunk_rank_refs(bundle);
    for chunk in chunk_rows {
        let Some(reference) = chunk_rank_refs.get(&chunk.chunk_id) else {
            continue;
        };
        for block_id in &chunk.support_block_ids {
            merge_ranked_reference(
                &mut block_refs,
                *block_id,
                reference.rank,
                reference.score.max(1.0),
                "selected_chunk_support",
            );
        }
    }
    block_refs
}

pub(crate) fn selected_fact_ids_for_detail(
    bundle: &KnowledgeContextBundleReferenceSetRow,
    fact_rank_refs: &HashMap<Uuid, RankedBundleReference>,
) -> Vec<Uuid> {
    let mut fact_ids = bundle.bundle.selected_fact_ids.clone();
    for fact_id in top_ranked_ids(fact_rank_refs, super::MAX_DETAIL_TECHNICAL_FACT_REFERENCES) {
        if fact_ids.len() >= super::MAX_DETAIL_TECHNICAL_FACT_REFERENCES {
            break;
        }
        if !fact_ids.contains(&fact_id) {
            fact_ids.push(fact_id);
        }
    }
    fact_ids.truncate(super::MAX_DETAIL_TECHNICAL_FACT_REFERENCES);
    fact_ids
}

pub(crate) async fn load_execution_prepared_reference_context(
    state: &AppState,
    execution_id: Uuid,
) -> Result<ExecutionPreparedReferenceContext, ApiError> {
    let bundle_refs = state
        .arango_context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let Some(bundle) = bundle_refs.as_ref() else {
        return Ok(ExecutionPreparedReferenceContext::default());
    };

    let chunk_rows = state
        .arango_document_store
        .list_chunks_by_ids(
            &bundle.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let evidence_rows = state
        .arango_graph_store
        .list_evidence_by_ids(
            &bundle
                .evidence_references
                .iter()
                .map(|reference| reference.evidence_id)
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let mut fact_rank_refs = derive_fact_rank_refs(bundle, &evidence_rows);
    let chunk_supported_fact_rows = if chunk_rows.is_empty() {
        Vec::new()
    } else {
        state
            .arango_document_store
            .list_technical_facts_by_chunk_ids(
                &chunk_rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>(),
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    };
    augment_fact_rank_refs_from_chunk_support(
        Some(bundle),
        &chunk_supported_fact_rows,
        &mut fact_rank_refs,
    );
    let technical_fact_rows =
        if fact_rank_refs.is_empty() && bundle.bundle.selected_fact_ids.is_empty() {
            Vec::new()
        } else {
            state
                .arango_document_store
                .list_technical_facts_by_ids(&selected_fact_ids_for_detail(bundle, &fact_rank_refs))
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        };
    let block_rank_refs =
        derive_block_rank_refs(bundle, &evidence_rows, &technical_fact_rows, &chunk_rows);
    let structured_block_rows = if block_rank_refs.is_empty() {
        Vec::new()
    } else {
        state
            .arango_document_store
            .list_structured_blocks_by_ids(&block_rank_refs.keys().copied().collect::<Vec<_>>())
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    };
    let segment_revision_info =
        load_prepared_segment_revision_info(state, &structured_block_rows).await;
    let assistant_document_references =
        assistant_document_references_from_diagnostics(&bundle.bundle.assembly_diagnostics);

    Ok(ExecutionPreparedReferenceContext {
        bundle_refs,
        fact_rank_refs,
        technical_fact_rows,
        block_rank_refs,
        structured_block_rows,
        segment_revision_info,
        assistant_document_references,
    })
}

fn assistant_document_references_from_diagnostics(
    assembly_diagnostics: &serde_json::Value,
) -> Vec<AssistantGroundingDocumentReference> {
    assembly_diagnostics
        .get("assistantGrounding")
        .and_then(|value| value.get("documentReferences"))
        .cloned()
        .and_then(|value| {
            serde_json::from_value::<Vec<AssistantGroundingDocumentReference>>(value).ok()
        })
        .unwrap_or_default()
}

async fn load_prepared_segment_revision_info(
    state: &AppState,
    blocks: &[crate::infra::arangodb::document_store::KnowledgeStructuredBlockRow],
) -> HashMap<Uuid, PreparedSegmentRevisionInfo> {
    if blocks.is_empty() {
        return HashMap::new();
    }

    let document_ids = blocks
        .iter()
        .map(|block| block.document_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let revision_ids = blocks
        .iter()
        .map(|block| block.revision_id)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();

    let doc_titles: HashMap<Uuid, Option<String>> = state
        .arango_document_store
        .list_documents_by_ids(&document_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|document| (document.document_id, document.title))
        .collect();

    state
        .arango_document_store
        .list_revisions_by_ids(&revision_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|revision| {
            let source_descriptor =
                crate::services::content::source_access::describe_content_source(
                    revision.document_id,
                    Some(revision.revision_id),
                    &revision.revision_kind,
                    revision.source_uri.as_deref(),
                    revision.storage_ref.as_deref(),
                    revision.title.as_deref(),
                    doc_titles
                        .get(&revision.document_id)
                        .and_then(|title| title.as_deref())
                        .unwrap_or("document"),
                );
            (
                revision.revision_id,
                PreparedSegmentRevisionInfo {
                    document_title: doc_titles
                        .get(&revision.document_id)
                        .cloned()
                        .flatten()
                        .or_else(|| revision.title.clone())
                        .or_else(|| Some(source_descriptor.file_name.clone())),
                    source_uri: revision.source_uri.clone(),
                    source_access: source_descriptor.access,
                },
            )
        })
        .collect()
}

fn freshness_snapshot_json(row: &KnowledgeLibraryGenerationRow) -> serde_json::Value {
    json!({
        "generationId": row.generation_id,
        "activeTextGeneration": row.active_text_generation,
        "activeVectorGeneration": row.active_vector_generation,
        "activeGraphGeneration": row.active_graph_generation,
        "degradedState": row.degraded_state,
        "updatedAt": row.updated_at,
    })
}
