use anyhow::Context;
use uuid::Uuid;

use crate::{
    agent_runtime::pipeline::try_op::run_async_try_op,
    app::state::AppState,
    domains::query::RuntimeQueryMode,
    infra::repositories,
    services::{
        ingest::runtime::resolve_effective_provider_profile,
        query::planner::{QueryPlanTaskInput, build_task_query_plan},
        query::support::{
            IntentResolutionRequest, derive_query_planning_metadata, derive_rerank_metadata,
        },
    },
};

use super::*;

/// Finalize a reranked bundle into a `RuntimeStructuredQueryResult`
/// (context assembly + diagnostics). Runs AFTER the caller has had a
/// chance to mutate `rerank_stage.retrieval.bundle` (e.g. via
/// `focused_document_consolidation`) so the assembled context reflects
/// those edits.
pub(crate) async fn finalize_structured_query(
    state: &AppState,
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    rerank_stage: StructuredQueryRerankStage,
    include_debug: bool,
    focused_document_id: Option<Uuid>,
) -> anyhow::Result<RuntimeStructuredQueryResult> {
    let assemble_started = std::time::Instant::now();
    let assembly_stage = run_async_try_op(rerank_stage, |rerank_stage| {
        assemble_structured_query(
            state,
            question,
            query_ir,
            rerank_stage,
            include_debug,
            focused_document_id,
        )
    })
    .await?;
    let assemble_elapsed_ms = assemble_started.elapsed().as_millis();
    tracing::info!(
        stage = "retrieval.assemble",
        assemble_ms = assemble_elapsed_ms,
        "structured retrieval assemble stage"
    );

    let enrichment = QueryExecutionEnrichment {
        planning: assembly_stage.rerank.retrieval.planning.planning.clone(),
        rerank: assembly_stage.rerank.rerank.clone(),
        context_assembly: assembly_stage.context_assembly.clone(),
        grouped_references: assembly_stage.grouped_references.clone(),
    };
    let diagnostics = build_structured_query_diagnostics(
        &assembly_stage.rerank.retrieval.planning.plan,
        &assembly_stage.rerank.retrieval.bundle,
        &assembly_stage.rerank.retrieval.planning.graph_index,
        &enrichment,
        include_debug,
        &assembly_stage.context_text,
    );

    // Snapshot the final ranked chunks so the turn layer can write
    // `query_chunk_reference` audit rows keyed by the execution_id.
    // Rank is 1-based, score is f64 (f32 retrieval score widened) to
    // match the table definition.
    let retrieved_context_document_titles =
        distinct_context_document_titles(&assembly_stage.rerank.retrieval.bundle.chunks);
    let chunk_references =
        build_query_chunk_reference_snapshots(&assembly_stage.rerank.retrieval.bundle.chunks);
    let context_chunks = assembly_stage.rerank.retrieval.bundle.chunks.clone();
    let ordered_source_units =
        collect_ordered_source_units(&assembly_stage.rerank.retrieval.bundle.chunks);
    let graph_evidence_context_lines =
        assembly_stage.rerank.retrieval.graph_evidence_context_lines.clone();

    Ok(RuntimeStructuredQueryResult {
        planned_mode: assembly_stage.rerank.retrieval.planning.plan.planned_mode,
        embedding_usage: assembly_stage.rerank.retrieval.planning.embedding_usage,
        intent_profile: assembly_stage.rerank.retrieval.planning.plan.intent_profile,
        context_text: assembly_stage.context_text,
        technical_literals_text: assembly_stage.technical_literals_text,
        technical_literal_chunks: assembly_stage.technical_literal_chunks,
        diagnostics,
        retrieved_documents: assembly_stage.retrieved_documents,
        retrieved_context_document_titles,
        chunk_references,
        context_chunks,
        ordered_source_units,
        graph_evidence_context_lines,
    })
}

fn distinct_context_document_titles(chunks: &[RuntimeMatchedChunk]) -> Vec<String> {
    let mut seen = std::collections::HashSet::<String>::new();
    let mut titles = Vec::new();
    for chunk in chunks {
        let title = chunk.document_label.trim();
        if title.is_empty() {
            continue;
        }
        let key = title.to_lowercase();
        if seen.insert(key) {
            titles.push(title.to_string());
        }
    }
    titles
}

fn build_query_chunk_reference_snapshots(
    chunks: &[RuntimeMatchedChunk],
) -> Vec<QueryChunkReferenceSnapshot> {
    chunks
        .iter()
        .filter(|chunk| !is_source_unit_runtime_chunk(chunk))
        .enumerate()
        .map(|(index, chunk)| QueryChunkReferenceSnapshot {
            chunk_id: chunk.chunk_id,
            rank: (index as i32) + 1,
            score: chunk.score.unwrap_or(0.0) as f64,
        })
        .collect()
}

fn collect_ordered_source_units(chunks: &[RuntimeMatchedChunk]) -> Vec<RuntimeMatchedChunk> {
    let mut units = chunks
        .iter()
        .filter(|chunk| is_source_unit_runtime_chunk(chunk))
        .cloned()
        .collect::<Vec<_>>();
    units.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id));
    units
}

pub(crate) async fn plan_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
) -> anyhow::Result<StructuredQueryPlanningStage> {
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    let source_truth_version =
        repositories::get_library_source_truth_version(&state.persistence.postgres, library_id)
            .await
            .context("failed to load library source-truth version for query planning")?;
    let planning = derive_query_planning_metadata(&IntentResolutionRequest {
        library_id,
        question: question.to_string(),
        explicit_mode: mode,
        source_truth_version,
    });
    let plan = build_task_query_plan(&QueryPlanTaskInput {
        question: question.to_string(),
        top_k: Some(top_k),
        explicit_mode: Some(mode),
        metadata: Some(planning.clone()),
    })
    .map_err(|failure| anyhow::anyhow!(failure.summary))?;
    let technical_literal_intent = TechnicalLiteralIntent::default();
    let skip_vector_search = should_skip_vector_search(&plan);
    let (question_embedding, hyde_embedding, embedding_usage) = if skip_vector_search {
        tracing::info!(
            stage = "embed",
            exact_literal_technical = true,
            "vector retrieval skipped for exact technical literal query"
        );
        (Vec::new(), None, None)
    } else {
        let embed_result = embed_question(state, library_id, &provider_profile, question).await?;
        let question_embedding = embed_result.embedding.clone();

        let hyde_embedding = if plan.hyde_recommended {
            tracing::info!(
                stage = "hyde",
                hyde_recommended = true,
                "HyDE activated for this query"
            );
            match generate_hyde_passage(state, library_id, question).await {
                Some(passage) => {
                    tracing::debug!(
                        stage = "hyde",
                        passage_len = passage.len(),
                        "HyDE passage generated"
                    );
                    tracing::trace!(stage = "hyde", passage = %passage, "HyDE passage content");
                    match embed_question(state, library_id, &provider_profile, &passage).await {
                        Ok(hyde_result) => {
                            tracing::debug!(stage = "hyde_embed", "HyDE embedding computed");
                            Some(hyde_result.embedding)
                        }
                        Err(error) => {
                            tracing::warn!(
                                stage = "hyde",
                                error = %error,
                                "HyDE embedding failed, falling back to question embedding"
                            );
                            None
                        }
                    }
                }
                None => {
                    tracing::warn!(
                        stage = "hyde",
                        "HyDE passage generation failed or timed out, using raw query embedding"
                    );
                    None
                }
            }
        } else {
            tracing::debug!(
                stage = "hyde",
                hyde_recommended = false,
                "HyDE skipped — not recommended for this query intent"
            );
            None
        };
        (question_embedding, hyde_embedding, Some(embed_result))
    };

    let graph_index = load_graph_index(state, library_id).await?;
    let document_index = load_document_index(state, library_id).await?;
    let candidate_limit = expanded_candidate_limit(
        plan.planned_mode,
        plan.top_k,
        state.retrieval_intelligence.rerank_enabled,
        state.retrieval_intelligence.rerank_candidate_limit,
    )
    .max(technical_literal_candidate_limit(technical_literal_intent, plan.top_k));

    Ok(StructuredQueryPlanningStage {
        provider_profile,
        planning,
        plan,
        technical_literal_intent,
        question_embedding,
        hyde_embedding,
        embedding_usage,
        graph_index,
        document_index,
        candidate_limit,
    })
}

pub(crate) async fn retrieve_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mut planning: StructuredQueryPlanningStage,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
) -> anyhow::Result<StructuredQueryRetrievalStage> {
    let technical_literal_intent =
        effective_technical_literal_intent(question, query_ir, planning.technical_literal_intent);
    planning.technical_literal_intent = technical_literal_intent;
    planning.candidate_limit = planning
        .candidate_limit
        .max(technical_literal_candidate_limit(technical_literal_intent, planning.plan.top_k));

    let plan = &planning.plan;
    let provider_profile = &planning.provider_profile;
    let vector_search_embedding =
        planning.hyde_embedding.as_deref().unwrap_or(&planning.question_embedding);
    let question_embedding = vector_search_embedding;
    let graph_index = &planning.graph_index;
    let document_index = &planning.document_index;
    let candidate_limit = planning.candidate_limit;
    let document_filter_ids = explicit_target_document_ids_from_values(
        question,
        document_index.values().flat_map(|document| {
            [
                document.file_name.as_deref(),
                document.title.as_deref(),
                Some(document.external_key.as_str()),
            ]
            .into_iter()
            .flatten()
            .map(move |value| (document.document_id, value))
        }),
    );
    let locked_target_document_ids =
        (!document_filter_ids.is_empty()).then_some(&document_filter_ids);

    let mut bundle = match plan.planned_mode {
        RuntimeQueryMode::Document => {
            let chunks = retrieve_document_chunks(
                state,
                library_id,
                provider_profile,
                question,
                locked_target_document_ids,
                plan,
                candidate_limit,
                question_embedding,
                document_index,
                query_ir,
            )
            .await?;
            RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks }
        }
        RuntimeQueryMode::Local => {
            retrieve_local_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                query_ir,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?
        }
        RuntimeQueryMode::Global => {
            retrieve_global_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                query_ir,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?
        }
        RuntimeQueryMode::Hybrid => {
            let mut bundle = retrieve_local_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                query_ir,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?;
            bundle.chunks = retrieve_document_chunks(
                state,
                library_id,
                provider_profile,
                question,
                locked_target_document_ids,
                plan,
                candidate_limit,
                question_embedding,
                document_index,
                query_ir,
            )
            .await?;
            bundle
        }
        RuntimeQueryMode::Mix => {
            let mut local = retrieve_local_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                query_ir,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?;
            let global = retrieve_global_bundle(
                state,
                library_id,
                provider_profile,
                plan,
                query_ir,
                candidate_limit,
                question_embedding,
                graph_index,
            )
            .await?;
            local.entities = merge_entities(local.entities, global.entities, candidate_limit);
            local.relationships =
                merge_relationships(local.relationships, global.relationships, candidate_limit);
            local.chunks = retrieve_document_chunks(
                state,
                library_id,
                provider_profile,
                question,
                locked_target_document_ids,
                plan,
                candidate_limit,
                question_embedding,
                document_index,
                query_ir,
            )
            .await?;
            local
        }
    };

    let graph_evidence = load_graph_evidence_chunks_for_bundle(
        state,
        library_id,
        question,
        &bundle.entities,
        &bundle.relationships,
        plan,
        query_ir,
        graph_index,
        document_index,
        &plan.keywords,
    )
    .await?;
    if !graph_evidence.chunks.is_empty() {
        bundle.chunks = merge_graph_evidence_chunks(
            std::mem::take(&mut bundle.chunks),
            graph_evidence.chunks,
            graph_evidence_context_top_k(candidate_limit),
        );
    }
    let stale_chunk_count =
        retain_canonical_document_head_chunks(&mut bundle.chunks, document_index);
    if stale_chunk_count > 0 {
        tracing::info!(
            stage = "retrieval.canonical_head_filter",
            library_id = %library_id,
            stale_chunk_count,
            "removed non-head revision chunks from retrieval bundle"
        );
    }

    Ok(StructuredQueryRetrievalStage {
        planning,
        bundle,
        graph_evidence_context_lines: graph_evidence.context_lines,
    })
}

pub(crate) async fn rerank_structured_query(
    state: &AppState,
    question: &str,
    mut retrieval: StructuredQueryRetrievalStage,
) -> anyhow::Result<StructuredQueryRerankStage> {
    let plan = &retrieval.planning.plan;
    let rerank = match plan.planned_mode {
        RuntimeQueryMode::Hybrid => {
            apply_hybrid_rerank(state, question, plan, &mut retrieval.bundle)
        }
        RuntimeQueryMode::Mix => apply_mix_rerank(state, question, plan, &mut retrieval.bundle),
        _ => derive_rerank_metadata(&crate::services::query::support::RerankRequest {
            question: question.to_string(),
            requested_mode: plan.planned_mode,
            candidate_count: retrieval.bundle.entities.len()
                + retrieval.bundle.relationships.len()
                + retrieval.bundle.chunks.len(),
            enabled: state.retrieval_intelligence.rerank_enabled,
            result_limit: plan.top_k,
        }),
    };

    Ok(StructuredQueryRerankStage { retrieval, rerank })
}

async fn assemble_structured_query(
    state: &AppState,
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    mut rerank: StructuredQueryRerankStage,
    _include_debug: bool,
    focused_document_id: Option<Uuid>,
) -> anyhow::Result<StructuredQueryAssemblyStage> {
    let technical_literal_intent = effective_technical_literal_intent(
        question,
        Some(query_ir),
        rerank.retrieval.planning.technical_literal_intent,
    );
    rerank.retrieval.planning.technical_literal_intent = technical_literal_intent;
    let plan = &rerank.retrieval.planning.plan;
    let bundle = &mut rerank.retrieval.bundle;
    let effective_top_k = source_slice_context_top_k(query_ir, plan.top_k);
    let retrieved_documents = load_retrieved_document_briefs(
        state,
        &bundle.chunks,
        &rerank.retrieval.planning.document_index,
        effective_top_k,
        focused_document_id,
    )
    .await;
    let pagination_requested = question_mentions_pagination(question);
    let literal_focus_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let technical_literal_chunks = select_technical_literal_chunks(
        question,
        query_ir,
        &bundle.chunks,
        technical_literal_intent,
        effective_top_k,
        &literal_focus_keywords,
        pagination_requested,
    );
    let technical_literal_groups =
        collect_technical_literal_groups(question, query_ir, &technical_literal_chunks);
    let technical_literals_text =
        render_exact_technical_literals_section(&technical_literal_groups);
    truncate_bundle(bundle, effective_top_k);

    let grouped_references = group_visible_references_for_query(
        &build_grouped_reference_candidates(
            &bundle.entities,
            &bundle.relationships,
            &bundle.chunks,
            effective_top_k,
        ),
        effective_top_k,
    );
    let effective_context_budget =
        source_slice_context_budget_chars(query_ir, plan.context_budget_chars);
    let graph_evidence_lines = rerank.retrieval.graph_evidence_context_lines.clone();
    let context_text = assemble_bounded_context_for_query(
        query_ir,
        question,
        &bundle.entities,
        &bundle.relationships,
        &bundle.chunks,
        &graph_evidence_lines,
        effective_context_budget,
    );
    let graph_support_count =
        bundle.entities.len() + bundle.relationships.len() + graph_evidence_lines.len();
    let context_assembly = assemble_context_metadata_for_query(
        plan.planned_mode,
        graph_support_count,
        bundle.chunks.len(),
    );

    Ok(StructuredQueryAssemblyStage {
        rerank,
        context_text,
        technical_literals_text,
        technical_literal_chunks,
        retrieved_documents,
        grouped_references,
        context_assembly,
    })
}

fn effective_technical_literal_intent(
    question: &str,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
    fallback: TechnicalLiteralIntent,
) -> TechnicalLiteralIntent {
    let query_ir_intent = query_ir
        .map(|ir| {
            super::technical_literals::detect_technical_literal_intent_from_query_ir(question, ir)
        })
        .unwrap_or_default();
    merge_technical_literal_intent(fallback, query_ir_intent)
}

fn merge_technical_literal_intent(
    left: TechnicalLiteralIntent,
    right: TechnicalLiteralIntent,
) -> TechnicalLiteralIntent {
    TechnicalLiteralIntent {
        wants_urls: left.wants_urls || right.wants_urls,
        wants_prefixes: left.wants_prefixes || right.wants_prefixes,
        wants_paths: left.wants_paths || right.wants_paths,
        wants_methods: left.wants_methods || right.wants_methods,
        wants_parameters: left.wants_parameters || right.wants_parameters,
    }
}

#[cfg(test)]
mod tests {
    use crate::domains::query_ir::{QueryAct, QueryIR, QueryLanguage, QueryScope};
    use uuid::Uuid;

    use super::*;

    fn runtime_chunk(kind: Option<&str>, score: f32) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: 1,
            chunk_kind: kind.map(str::to_string),
            document_label: "records.jsonl".to_string(),
            excerpt: "record".to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(score),
            source_text: "record".to_string(),
        }
    }

    #[test]
    fn chunk_reference_snapshots_exclude_source_units() {
        let profile = runtime_chunk(Some("source_profile"), 4.0);
        let source_unit = runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0);
        let ordinary = runtime_chunk(Some("text"), 2.0);

        let snapshots = build_query_chunk_reference_snapshots(&[
            profile.clone(),
            source_unit,
            ordinary.clone(),
        ]);

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].chunk_id, profile.chunk_id);
        assert_eq!(snapshots[0].rank, 1);
        assert_eq!(snapshots[1].chunk_id, ordinary.chunk_id);
        assert_eq!(snapshots[1].rank, 2);
    }

    #[test]
    fn ordered_source_units_preserve_source_order() {
        let later = RuntimeMatchedChunk {
            chunk_index: 7,
            ..runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0)
        };
        let earlier = RuntimeMatchedChunk {
            chunk_index: 3,
            ..runtime_chunk(Some(SOURCE_UNIT_CHUNK_KIND), 3.0)
        };
        let ordinary = runtime_chunk(Some("text"), 2.0);

        let units = collect_ordered_source_units(&[later.clone(), ordinary, earlier.clone()]);

        assert_eq!(units.len(), 2);
        assert_eq!(units[0].chunk_id, earlier.chunk_id);
        assert_eq!(units[1].chunk_id, later.chunk_id);
    }

    #[test]
    fn query_ir_target_types_expand_technical_literal_selection() {
        let question = "Which commands and settings configure scanning through RareProtocol?";
        let query_ir = QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec![
                "protocol".to_string(),
                "path".to_string(),
                "config_key".to_string(),
            ],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 0.82,
        };
        let target_document_id = Uuid::now_v7();
        let target_chunk_id = Uuid::now_v7();
        let mut chunks = (0..14)
            .map(|index| RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                revision_id: Uuid::now_v7(),
                chunk_index: index,
                chunk_kind: None,
                document_label: format!("noisy-{index}.md"),
                excerpt: "General operations memo without command literals.".to_string(),
                score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
                score: Some(1.0 - (index as f32 * 0.01)),
                source_text: "General operations memo without command literals.".to_string(),
            })
            .collect::<Vec<_>>();
        chunks.push(RuntimeMatchedChunk {
            chunk_id: target_chunk_id,
            document_id: target_document_id,
            revision_id: Uuid::now_v7(),
            chunk_index: 0,
            chunk_kind: None,
            document_label: "rare-protocol-scan-folder.md".to_string(),
            excerpt: "RareProtocol setup: create /srv/scans and set scan_share = writable."
                .to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(0.42),
            source_text: "RareProtocol setup: create /srv/scans and set scan_share = writable."
                .to_string(),
        });
        let focus_keywords = technical_literal_focus_keywords(question, Some(&query_ir));

        let default_selected = select_technical_literal_chunks(
            question,
            &query_ir,
            &chunks,
            TechnicalLiteralIntent::default(),
            8,
            &focus_keywords,
            false,
        );
        assert!(
            !default_selected.iter().any(|chunk| chunk.chunk_id == target_chunk_id),
            "default selection is capped before the later needle chunk"
        );

        let technical_intent = effective_technical_literal_intent(
            question,
            Some(&query_ir),
            TechnicalLiteralIntent::default(),
        );
        assert!(technical_intent.any());

        let expanded_selected = select_technical_literal_chunks(
            question,
            &query_ir,
            &chunks,
            technical_intent,
            8,
            &focus_keywords,
            false,
        );

        assert!(
            expanded_selected.iter().any(|chunk| chunk.chunk_id == target_chunk_id),
            "QueryIR technical target types must keep later exact-literal evidence candidates"
        );
    }

    #[test]
    fn effective_technical_literal_intent_unions_planner_and_query_ir_signals() {
        let query_ir = QueryIR {
            act: QueryAct::ConfigureHow,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["config_key".to_string(), "path".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            confidence: 0.8,
        };

        let intent = effective_technical_literal_intent(
            "Which settings should the client use?",
            Some(&query_ir),
            TechnicalLiteralIntent { wants_urls: true, ..TechnicalLiteralIntent::default() },
        );

        assert!(intent.wants_urls);
        assert!(intent.wants_paths);
        assert!(intent.wants_methods);
        assert!(intent.wants_parameters);
    }
}
