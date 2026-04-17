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

pub(crate) async fn execute_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    mode: RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<RuntimeStructuredQueryResult> {
    let planning_stage =
        run_async_try_op((), |_| plan_structured_query(state, library_id, question, mode, top_k))
            .await?;
    let retrieval_stage = run_async_try_op(planning_stage, |planning_stage| {
        retrieve_structured_query(state, library_id, question, planning_stage)
    })
    .await?;

    let retrieval_stage = {
        if should_skip_crag_retry(&retrieval_stage.planning.plan, &retrieval_stage.bundle.chunks) {
            tracing::info!(
                stage = "crag",
                exact_literal_technical = true,
                chunk_count = retrieval_stage.bundle.chunks.len(),
                "CRAG retry skipped for exact technical literal query with grounded lexical hits"
            );
            retrieval_stage
        } else {
            let confidence = evaluate_retrieval_quality(
                &retrieval_stage.bundle.chunks,
                &retrieval_stage.planning.plan.keywords,
            );
            tracing::info!(
                stage = "crag",
                avg_score = confidence.score,
                is_sufficient = confidence.is_sufficient,
                threshold = %CRAG_CONFIDENCE_THRESHOLD,
                "retrieval quality evaluated"
            );
            if confidence.is_sufficient {
                retrieval_stage
            } else {
                tracing::info!(stage = "crag", original_score = %confidence.score, "retrieval quality below threshold, triggering CRAG retry");
                let mut stage = retrieval_stage;
                let explicit_target_document_ids = explicit_target_document_ids_from_values(
                    question,
                    stage.planning.document_index.values().flat_map(|document| {
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
                let locked_target_document_ids = (!explicit_target_document_ids.is_empty())
                    .then_some(&explicit_target_document_ids);
                if let Some(rewritten) = rewrite_query_for_retry(state, library_id, question).await
                {
                    tracing::debug!(stage = "crag_rewrite", rewritten_query = %rewritten, "CRAG query rewritten");
                    let retry_limit = stage.planning.candidate_limit;
                    let retry_ok = async {
                        let retry_embed = embed_question(
                            state,
                            library_id,
                            &stage.planning.provider_profile,
                            &rewritten,
                        )
                        .await?;
                        retrieve_document_chunks(
                            state,
                            library_id,
                            &stage.planning.provider_profile,
                            &rewritten,
                            locked_target_document_ids,
                            &stage.planning.plan,
                            retry_limit,
                            &retry_embed.embedding,
                            &stage.planning.document_index,
                        )
                        .await
                    }
                    .await;
                    match retry_ok {
                        Ok(retry_chunks) => {
                            let original_chunks = std::mem::take(&mut stage.bundle.chunks);
                            let original_len = original_chunks.len();
                            let retry_len = retry_chunks.len();
                            stage.bundle.chunks =
                                merge_chunks(original_chunks, retry_chunks, retry_limit);
                            tracing::debug!(
                                stage = "crag_merge",
                                original_len,
                                retry_len,
                                merged_len = stage.bundle.chunks.len(),
                                "CRAG retry chunks merged"
                            );
                        }
                        Err(error) => {
                            tracing::warn!(stage = "crag_retry", error = %error, "CRAG retry failed, keeping original chunks");
                        }
                    }
                }
                stage
            }
        }
    };

    let rerank_stage = run_async_try_op(retrieval_stage, |retrieval_stage| {
        rerank_structured_query(state, question, retrieval_stage)
    })
    .await?;
    let assembly_stage = run_async_try_op(rerank_stage, |rerank_stage| {
        assemble_structured_query(state, question, rerank_stage, include_debug)
    })
    .await?;

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

    Ok(RuntimeStructuredQueryResult {
        planned_mode: assembly_stage.rerank.retrieval.planning.plan.planned_mode,
        embedding_usage: assembly_stage.rerank.retrieval.planning.embedding_usage,
        intent_profile: assembly_stage.rerank.retrieval.planning.plan.intent_profile,
        context_text: assembly_stage.context_text,
        technical_literals_text: assembly_stage.technical_literals_text,
        technical_literal_chunks: assembly_stage.technical_literal_chunks,
        diagnostics,
        retrieved_documents: assembly_stage.retrieved_documents,
    })
}

async fn plan_structured_query(
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
    let technical_literal_intent = if plan.intent_profile.exact_literal_technical {
        detect_technical_literal_intent(question)
    } else {
        TechnicalLiteralIntent::default()
    };
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

async fn retrieve_structured_query(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    planning: StructuredQueryPlanningStage,
) -> anyhow::Result<StructuredQueryRetrievalStage> {
    let plan = &planning.plan;
    let provider_profile = &planning.provider_profile;
    let vector_search_embedding =
        planning.hyde_embedding.as_deref().unwrap_or(&planning.question_embedding);
    let question_embedding = vector_search_embedding;
    let graph_index = &planning.graph_index;
    let document_index = &planning.document_index;
    let candidate_limit = planning.candidate_limit;
    let explicit_target_document_ids = explicit_target_document_ids_from_values(
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
        (!explicit_target_document_ids.is_empty()).then_some(&explicit_target_document_ids);

    let bundle = match plan.planned_mode {
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
            )
            .await?;
            local
        }
    };

    Ok(StructuredQueryRetrievalStage { planning, bundle })
}

async fn rerank_structured_query(
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
    mut rerank: StructuredQueryRerankStage,
    _include_debug: bool,
) -> anyhow::Result<StructuredQueryAssemblyStage> {
    let plan = &rerank.retrieval.planning.plan;
    let bundle = &mut rerank.retrieval.bundle;
    let retrieved_documents = load_retrieved_document_briefs(
        state,
        &bundle.chunks,
        &rerank.retrieval.planning.document_index,
        plan.top_k,
    )
    .await;
    let pagination_requested = question_mentions_pagination(question);
    let literal_focus_keywords = technical_literal_focus_keywords(question, None);
    let technical_literal_chunks = select_technical_literal_chunks(
        question,
        &bundle.chunks,
        rerank.retrieval.planning.technical_literal_intent,
        plan.top_k,
        &literal_focus_keywords,
        pagination_requested,
    );
    let technical_literal_groups =
        collect_technical_literal_groups(question, &technical_literal_chunks);
    let technical_literals_text =
        render_exact_technical_literals_section(&technical_literal_groups);
    truncate_bundle(bundle, plan.top_k);

    let grouped_references = group_visible_references_for_query(
        &build_grouped_reference_candidates(
            &bundle.entities,
            &bundle.relationships,
            &bundle.chunks,
            plan.top_k,
        ),
        plan.top_k,
    );
    let context_text = assemble_bounded_context(
        &bundle.entities,
        &bundle.relationships,
        &bundle.chunks,
        plan.context_budget_chars,
    );
    let graph_support_count = bundle.entities.len() + bundle.relationships.len();
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

pub(crate) fn should_skip_crag_retry(
    plan: &crate::services::query::planner::RuntimeQueryPlan,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    plan.intent_profile.exact_literal_technical && !chunks.is_empty()
}
