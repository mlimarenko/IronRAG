#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Context;
use uuid::Uuid;

mod answer;
mod context;
mod document_target;
mod embed;
mod hyde_crag;
mod port_answer;
mod rerank;
mod retrieve;
mod technical_literals;
#[cfg(test)]
mod tests;
mod types;
mod verification;

use embed::embed_question;
use hyde_crag::{evaluate_retrieval_quality, generate_hyde_passage, rewrite_query_for_retry};
#[cfg(test)]
use port_answer::question_mentions_port;
#[cfg(test)]
use port_answer::{build_port_and_protocol_answer, build_port_answer};
#[cfg(test)]
use technical_literals::build_exact_technical_literals_section;
#[cfg(test)]
use technical_literals::technical_literal_focus_keyword_segments;
#[cfg(test)]
use verification::{
    RuntimeAnswerVerification, enrich_query_assembly_diagnostics, enrich_query_candidate_summary,
};
use verification::{persist_query_verification, verify_answer_against_canonical_evidence};

use crate::domains::query::QueryVerificationState;
#[cfg(test)]
use crate::domains::query::QueryVerificationWarning;
use technical_literals::{
    TechnicalLiteralIntent, collect_technical_literal_groups, detect_technical_literal_intent,
    question_mentions_pagination, render_exact_technical_literals_section,
    select_document_balanced_chunks, technical_literal_candidate_limit,
    technical_literal_focus_keywords,
};

pub(crate) use answer::*;
pub(crate) use context::*;
pub(crate) use document_target::*;
pub(crate) use rerank::*;
pub(crate) use retrieve::*;
pub(crate) use types::*;

use crate::{
    agent_runtime::{pipeline::try_op::run_async_try_op, request::build_provider_request},
    app::state::AppState,
    domains::{agent_runtime::RuntimeTaskKind, ai::AiBindingPurpose, query::RuntimeQueryMode},
    infra::arangodb::document_store::KnowledgeDocumentRow,
    infra::repositories,
    integrations::llm::ChatRequestSeed,
    services::{
        ingest::runtime::resolve_effective_provider_profile,
        query::planner::{QueryPlanTaskInput, build_task_query_plan},
        query::support::{
            IntentResolutionRequest, derive_query_planning_metadata, derive_rerank_metadata,
        },
    },
    shared::extraction::text_render::repair_technical_layout_noise,
};

/// HyDE passage generation timeout. Increase for slow LLM providers.
const HYDE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
/// CRAG query rewrite timeout.
const CRAG_REWRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// HyDE generation temperature. Lower = more factual, higher = more creative.
const HYDE_TEMPERATURE: f64 = 0.3;
/// CRAG rewrite temperature.
const CRAG_REWRITE_TEMPERATURE: f64 = 0.5;
/// Minimum retrieval quality score (0.0-1.0) to skip CRAG retry.
const CRAG_CONFIDENCE_THRESHOLD: f32 = 0.25;
/// Maximum structured blocks included per answer assembly pass.
const MAX_ANSWER_BLOCKS: usize = 16;
/// Maximum chunks selected per document in balanced chunk selection.
const MAX_CHUNKS_PER_DOCUMENT: usize = 8;
/// Minimum chunks selected per document in balanced chunk selection.
const MIN_CHUNKS_PER_DOCUMENT: usize = 2;
/// Maximum number of table-row chunks loaded directly for a focused aggregate answer.
const MAX_DIRECT_TABLE_ANALYTICS_ROWS: usize = 2_000;

const MAX_CANONICAL_ANSWER_TECHNICAL_FACTS: usize = 24;

async fn execute_structured_query(
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

    // CRAG: evaluate retrieval quality and retry with rewritten query if needed
    let retrieval_stage = {
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
            let locked_target_document_ids =
                (!explicit_target_document_ids.is_empty()).then_some(&explicit_target_document_ids);
            if let Some(rewritten) = rewrite_query_for_retry(state, library_id, question).await {
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
    let embed_result = embed_question(state, library_id, &provider_profile, question).await?;
    let question_embedding = embed_result.embedding.clone();

    // HyDE: generate a hypothetical passage and embed it for vector search
    let hyde_embedding = if plan.hyde_recommended {
        tracing::info!(stage = "hyde", hyde_recommended = true, "HyDE activated for this query");
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
        embedding_usage: Some(embed_result),
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
    // Use HyDE embedding for vector search when available, raw question embedding otherwise
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
    let literal_focus_keywords = technical_literal_focus_keywords(question);
    let technical_literal_chunks = if rerank.retrieval.planning.technical_literal_intent.any() {
        bundle.chunks.clone()
    } else {
        select_document_balanced_chunks(
            question,
            &bundle.chunks,
            &literal_focus_keywords,
            pagination_requested,
            12,
            3,
        )
        .into_iter()
        .cloned()
        .collect::<Vec<_>>()
    };
    let technical_literal_groups = collect_technical_literal_groups(question, &bundle.chunks);
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

pub(crate) async fn prepare_answer_query(
    state: &AppState,
    library_id: Uuid,
    question: String,
    mode: RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
) -> anyhow::Result<PreparedAnswerQueryResult> {
    let mut structured = run_async_try_op((), |_| {
        execute_structured_query(state, library_id, &question, mode, top_k, include_debug)
    })
    .await?;
    let library_context = match load_query_execution_library_context(state, library_id).await {
        Ok(context) => Some(context),
        Err(error) => {
            tracing::warn!(
                error = %error,
                library_id = %library_id,
                "skipping non-critical query library context enrichment"
            );
            None
        }
    };
    apply_query_execution_warning(
        &mut structured.diagnostics,
        library_context.as_ref().and_then(|context| context.warning.as_ref()),
    );
    apply_query_execution_library_summary(&mut structured.diagnostics, library_context.as_ref());
    let community_matches = search_community_summaries(state, library_id, &question, 3).await;
    let community_context_text = format_community_context(&community_matches);
    let mut answer_context = library_context.as_ref().map_or_else(
        || structured.context_text.clone(),
        |context| {
            assemble_answer_context(
                &context.summary,
                &context.recent_documents,
                &structured.retrieved_documents,
                structured.technical_literals_text.as_deref(),
                &structured.context_text,
            )
        },
    );
    if let Some(community_text) = &community_context_text {
        answer_context = format!("{community_text}\n\n{answer_context}");
    }

    let embedding_usage = structured.embedding_usage.clone();
    Ok(PreparedAnswerQueryResult { structured, answer_context, embedding_usage })
}

pub(crate) async fn generate_answer_query(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    effective_question: &str,
    user_question: &str,
    conversation_history: Option<&str>,
    _system_prompt: Option<String>,
    _prepared: PreparedAnswerQueryResult,
    on_delta: Option<&mut (dyn FnMut(String) + Send)>,
    auth: &crate::interfaces::http::auth::AuthContext,
) -> anyhow::Result<RuntimeAnswerQueryResult> {
    // The in-app assistant is now a vanilla MCP-tool agent loop. The
    // existing prepared/system_prompt arguments are kept for ABI stability
    // but ignored — the loop pulls everything it needs through MCP.
    let _ = (effective_question, execution_id);
    tracing::info!(
        %execution_id,
        %library_id,
        question_len = user_question.len(),
        "assistant agent loop start"
    );
    let result = match crate::services::query::agent_loop::run_assistant_turn(
        state,
        auth,
        library_id,
        &execution_id.to_string(),
        user_question,
        conversation_history,
        on_delta,
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            tracing::error!(
                %execution_id,
                %library_id,
                ?error,
                "assistant agent loop failed"
            );
            return Err(error);
        }
    };
    tracing::info!(
        %execution_id,
        iterations = result.iterations,
        tool_calls = result.tool_calls_total,
        answer_len = result.answer.len(),
        "assistant agent loop done"
    );
    Ok(RuntimeAnswerQueryResult {
        answer: result.answer,
        provider: result.provider,
        usage_json: result.usage_json,
    })
}

async fn generate_answer_stage(
    state: &AppState,
    library_id: Uuid,
    execution_id: Uuid,
    effective_question: &str,
    user_question: &str,
    conversation_history: Option<&str>,
    system_prompt: Option<String>,
    prepared: PreparedAnswerQueryResult,
    on_delta: Option<&mut (dyn FnMut(String) + Send)>,
) -> anyhow::Result<AnswerGenerationStage> {
    let intent_profile = prepared.structured.intent_profile.clone();
    let provider_profile = resolve_effective_provider_profile(state, library_id).await?;
    let document_index = load_document_index(state, library_id).await?;
    let answer_provider = provider_profile
        .selection_for_runtime_task_kind(RuntimeTaskKind::QueryAnswer)
        .cloned()
        .unwrap_or_else(|| provider_profile.answer.clone());
    let missing_explicit_document_answer =
        build_missing_explicit_document_answer(effective_question, &document_index);
    let direct_targeted_table_answer =
        load_direct_targeted_table_answer(state, effective_question, &document_index).await?;
    let canonical_answer_chunks = load_canonical_answer_chunks(
        state,
        execution_id,
        effective_question,
        &prepared.structured.technical_literal_chunks,
        &document_index,
    )
    .await?;
    let canonical_evidence = load_canonical_answer_evidence(state, execution_id).await?;
    let community_matches =
        search_community_summaries(state, library_id, effective_question, 3).await;
    let community_context_text = format_community_context(&community_matches);
    let canonical_answer_context = build_canonical_answer_context(
        effective_question,
        &prepared.structured,
        &canonical_evidence,
        &canonical_answer_chunks,
        &prepared.answer_context,
        community_context_text.as_deref(),
    );
    let (answer, provider, usage_json) = if canonical_answer_context.trim().is_empty() {
        let answer = "No grounded evidence is available in the active library yet.".to_string();
        if let Some(on_delta) = on_delta {
            on_delta(answer.clone());
        }
        (answer, answer_provider.clone(), serde_json::json!({}))
    } else if let Some(answer) = missing_explicit_document_answer
        .or(direct_targeted_table_answer)
        .or_else(|| {
            build_unsupported_capability_answer(
                &prepared.structured.intent_profile,
                effective_question,
                &canonical_answer_chunks,
            )
        })
        .or_else(|| {
            build_deterministic_grounded_answer(
                effective_question,
                &canonical_evidence,
                &canonical_answer_chunks,
            )
        })
    {
        if let Some(on_delta) = on_delta {
            on_delta(answer.clone());
        }
        (
            answer,
            answer_provider.clone(),
            serde_json::json!({
                "deterministic": true,
                "reason": "canonical_deterministic_answer",
            }),
        )
    } else {
        let answer_binding_purpose =
            AiBindingPurpose::for_runtime_task_kind(RuntimeTaskKind::QueryAnswer)
                .ok_or_else(|| anyhow::anyhow!("query answer task kind has no binding purpose"))?;
        let answer_binding = state
            .canonical_services
            .ai_catalog
            .resolve_active_runtime_binding(state, library_id, answer_binding_purpose)
            .await?
            .ok_or_else(|| {
                anyhow::anyhow!("active answer binding is not configured for this library")
            })?;
        let answer_task_spec =
            state.agent_runtime.registry().spec(RuntimeTaskKind::QueryAnswer).ok_or_else(|| {
                anyhow::anyhow!("query answer runtime task spec is not registered")
            })?;
        let request = build_provider_request(
            answer_task_spec,
            ChatRequestSeed {
                provider_kind: answer_binding.provider_kind.clone(),
                model_name: answer_binding.model_name.clone(),
                api_key_override: answer_binding.api_key,
                base_url_override: answer_binding.provider_base_url,
                system_prompt: system_prompt.or(answer_binding.system_prompt),
                temperature: answer_binding.temperature,
                top_p: answer_binding.top_p,
                max_output_tokens_override: answer_binding.max_output_tokens_override,
                extra_parameters_json: answer_binding.extra_parameters_json,
            },
            build_answer_prompt(
                user_question,
                &canonical_answer_context,
                conversation_history,
                None,
            ),
        );
        let response = match on_delta {
            Some(on_delta) => state.llm_gateway.generate_stream(request, on_delta).await,
            None => state.llm_gateway.generate(request).await,
        }
        .context("failed to generate grounded answer")?;
        (
            response.output_text.trim().to_string(),
            crate::domains::provider_profiles::ProviderModelSelection {
                provider_kind: answer_binding.provider_kind.parse().unwrap_or_default(),
                model_name: answer_binding.model_name,
            },
            response.usage_json,
        )
    };

    Ok(AnswerGenerationStage {
        intent_profile,
        canonical_answer_chunks,
        canonical_evidence,
        answer,
        provider,
        usage_json,
        prompt_context: canonical_answer_context,
    })
}

async fn load_direct_targeted_table_answer(
    state: &AppState,
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Option<String>> {
    let row_count = requested_initial_table_row_count(question);
    let inventory_requested = question_asks_table_value_inventory(question);
    if row_count.is_none() && !inventory_requested {
        return Ok(None);
    }
    let targeted_document_ids = explicit_target_document_ids_from_values(
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
    if targeted_document_ids.len() != 1 {
        return Ok(None);
    }

    let document_id = *targeted_document_ids.iter().next().expect("validated single target");
    let Some(document) = document_index.get(&document_id) else {
        return Ok(None);
    };
    let Some(revision_id) = document.readable_revision_id.or(document.active_revision_id) else {
        return Ok(None);
    };

    let plan_keywords = crate::services::query::planner::extract_keywords(question);
    let document_label = document
        .title
        .clone()
        .filter(|value: &String| !value.trim().is_empty())
        .or_else(|| document.file_name.clone())
        .unwrap_or_else(|| document.external_key.clone());
    let row_limit = row_count.unwrap_or(16);
    let initial_rows = state
        .arango_document_store
        .list_structured_blocks_by_revision(revision_id)
        .await
        .context("failed to load structured blocks for direct initial row answer")?
        .into_iter()
        .filter(|block| block.block_kind == "table_row")
        .take(row_limit)
        .enumerate()
        .map(|(ordinal, block)| RuntimeMatchedChunk {
            chunk_id: block.block_id,
            document_id,
            document_label: document_label.clone(),
            excerpt: focused_excerpt_for(&block.normalized_text, &plan_keywords, 280),
            score: Some(10_000.0 - ordinal as f32),
            source_text: repair_technical_layout_noise(&block.normalized_text),
        })
        .collect::<Vec<_>>();
    if let Some(row_count) = row_count
        && initial_rows.len() < row_count
    {
        return Ok(None);
    }

    Ok(build_table_row_grounded_answer(question, &initial_rows))
}

async fn verify_generated_answer(
    state: &AppState,
    execution_id: Uuid,
    question: &str,
    mut generation: AnswerGenerationStage,
) -> anyhow::Result<AnswerVerificationStage> {
    let verification = verify_answer_against_canonical_evidence(
        question,
        &generation.answer,
        &generation.intent_profile,
        &generation.canonical_evidence,
        &generation.canonical_answer_chunks,
        &generation.prompt_context,
    );
    persist_query_verification(state, execution_id, &verification, &generation.canonical_evidence)
        .await?;

    // Grounding guard: only suppress the LLM's output when the model invented
    // facts that the verification step could not back up against the canonical
    // evidence (`unsupported_literal`, `wrong_canonical_target`,
    // `unsupported_canonical_claim`) — these are real hallucinations.
    //
    // We deliberately do NOT suppress on `Conflicting` or on
    // `InsufficientEvidence` triggered by the LLM's own self-disclosure
    // ("no grounded evidence", "exact value is not grounded"). Those are
    // honest, useful answers and replacing them with a generic refusal makes
    // the assistant feel weaker than it actually is.
    let has_hallucinated_literal =
        verification.warnings.iter().any(|warning| warning.code == "unsupported_literal");
    let has_wrong_canonical_target =
        verification.warnings.iter().any(|warning| warning.code == "wrong_canonical_target");
    let has_unsupported_canonical_claim =
        verification.warnings.iter().any(|warning| warning.code == "unsupported_canonical_claim");

    if has_hallucinated_literal || has_wrong_canonical_target || has_unsupported_canonical_claim {
        tracing::warn!(
            %execution_id,
            warnings = verification.warnings.len(),
            "answer suppressed due to hallucinated literals or wrong canonical target"
        );
        generation.answer = "I can't give a confident answer for this question — the most recent \
draft contained values that I couldn't verify against the uploaded documents. Please rephrase \
the question, narrow it to a specific document, or rerun the query."
            .to_string();
    } else if matches!(verification.state, QueryVerificationState::Conflicting) {
        // Conflicting evidence is informational only. Surface it via the
        // returned verification metadata, but keep the LLM's answer text
        // because the model usually picked the most plausible reading.
        tracing::info!(
            %execution_id,
            "answer kept despite conflicting evidence (verification flag only)"
        );
    }

    Ok(AnswerVerificationStage { generation })
}

async fn load_canonical_answer_chunks(
    state: &AppState,
    execution_id: Uuid,
    question: &str,
    fallback_chunks: &[RuntimeMatchedChunk],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let explicit_targeted_document_ids = explicit_target_document_ids_from_values(
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
    let focused_document_id = if explicit_targeted_document_ids.len() == 1 {
        explicit_targeted_document_ids.iter().next().copied()
    } else {
        focused_answer_document_id(question, fallback_chunks)
    };
    let aggregation_summary_chunks = if question_asks_table_aggregation(question)
        && let Some(document_id) = focused_document_id
    {
        let plan_keywords = crate::services::query::planner::extract_keywords(question);
        let targeted_document_ids = BTreeSet::from([document_id]);
        load_table_summary_chunks_for_documents(
            state,
            &document_index,
            &targeted_document_ids,
            32,
            &plan_keywords,
        )
        .await
        .context("failed to load focused table summaries for canonical answer")?
    } else {
        Vec::new()
    };
    let aggregation_row_chunks = if question_asks_table_aggregation(question)
        && let Some(document_id) = focused_document_id
    {
        let plan_keywords = crate::services::query::planner::extract_keywords(question);
        let targeted_document_ids = BTreeSet::from([document_id]);
        load_table_rows_for_documents(
            state,
            &document_index,
            &targeted_document_ids,
            MAX_DIRECT_TABLE_ANALYTICS_ROWS,
            &plan_keywords,
        )
        .await
        .context("failed to load focused table rows for canonical aggregate answer")?
    } else {
        Vec::new()
    };
    let explicit_initial_table_rows = if let Some(row_count) =
        requested_initial_table_row_count(question)
        && let Some(document_id) = focused_document_id
    {
        let plan_keywords = crate::services::query::planner::extract_keywords(question);
        let targeted_document_ids = BTreeSet::from([document_id]);
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            &document_index,
            &targeted_document_ids,
            row_count,
            &plan_keywords,
        )
        .await
        .context("failed to load direct initial table rows for canonical answer")?;
        (initial_rows.len() >= row_count).then_some(initial_rows)
    } else {
        None
    };
    if let Some(mut initial_rows) = explicit_initial_table_rows {
        if !aggregation_summary_chunks.is_empty() {
            let chunk_limit = initial_rows.len().saturating_add(32);
            initial_rows = merge_chunks(initial_rows, aggregation_summary_chunks, chunk_limit);
        }
        initial_rows.sort_by(score_desc_chunks);
        return Ok(initial_rows);
    }

    let Some(bundle_refs) = state
        .arango_context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .with_context(|| {
            format!("failed to load context bundle for canonical answer chunks {execution_id}")
        })?
    else {
        if !aggregation_summary_chunks.is_empty() || !aggregation_row_chunks.is_empty() {
            let mut aggregate_chunks = merge_chunks(
                aggregation_summary_chunks,
                aggregation_row_chunks,
                MAX_DIRECT_TABLE_ANALYTICS_ROWS.saturating_add(32),
            );
            aggregate_chunks.sort_by(score_desc_chunks);
            return Ok(aggregate_chunks);
        }
        return Ok(fallback_chunks.to_vec());
    };
    let chunk_ids =
        bundle_refs.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>();
    if chunk_ids.is_empty() {
        if !aggregation_summary_chunks.is_empty() || !aggregation_row_chunks.is_empty() {
            let mut aggregate_chunks = merge_chunks(
                aggregation_summary_chunks,
                aggregation_row_chunks,
                MAX_DIRECT_TABLE_ANALYTICS_ROWS.saturating_add(32),
            );
            aggregate_chunks.sort_by(score_desc_chunks);
            return Ok(aggregate_chunks);
        }
        return Ok(fallback_chunks.to_vec());
    }
    let plan_keywords = crate::services::query::planner::extract_keywords(question);
    let rows = state
        .arango_document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to load canonical answer chunks")?;
    let mut chunks: Vec<RuntimeMatchedChunk> = rows
        .into_iter()
        .filter_map(|chunk| map_chunk_hit(chunk, 1.0, &document_index, &plan_keywords))
        .collect();
    if question_asks_table_aggregation(question)
        && let Some(document_id) = focused_document_id
    {
        chunks.retain(|chunk| chunk.document_id == document_id);
        chunks = merge_canonical_table_aggregation_chunks(
            chunks,
            aggregation_summary_chunks,
            aggregation_row_chunks,
            MAX_DIRECT_TABLE_ANALYTICS_ROWS.saturating_add(32),
        );
    }
    if chunks.is_empty() {
        if question_asks_table_aggregation(question) && focused_document_id.is_some() {
            return Ok(Vec::new());
        }
        return Ok(fallback_chunks.to_vec());
    }
    if let Some(row_count) = requested_initial_table_row_count(question)
        && let Some(document_id) = focused_document_id
    {
        let targeted_document_ids = BTreeSet::from([document_id]);
        let chunk_limit = chunks.len().max(row_count);
        let initial_rows = load_initial_table_rows_for_documents(
            state,
            &document_index,
            &targeted_document_ids,
            row_count,
            &plan_keywords,
        )
        .await
        .context("failed to load focused initial table rows for canonical answer")?;
        chunks = merge_chunks(chunks, initial_rows, chunk_limit);
    }
    chunks.sort_by(score_desc_chunks);
    Ok(chunks)
}

async fn load_canonical_answer_evidence(
    state: &AppState,
    execution_id: Uuid,
) -> anyhow::Result<CanonicalAnswerEvidence> {
    let Some(bundle_refs) = state
        .arango_context_store
        .get_bundle_reference_set_by_query_execution(execution_id)
        .await
        .with_context(|| {
            format!("failed to load context bundle for answer evidence {execution_id}")
        })?
    else {
        return Ok(CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        });
    };

    let chunk_ids =
        bundle_refs.chunk_references.iter().map(|reference| reference.chunk_id).collect::<Vec<_>>();
    let evidence_rows = state
        .arango_graph_store
        .list_evidence_by_ids(
            &bundle_refs
                .evidence_references
                .iter()
                .map(|reference| reference.evidence_id)
                .collect::<Vec<_>>(),
        )
        .await
        .context("failed to load evidence rows for canonical answer context")?;
    let chunk_rows = state
        .arango_document_store
        .list_chunks_by_ids(&chunk_ids)
        .await
        .context("failed to load chunks for canonical answer context")?;
    let chunk_supported_facts =
        state.arango_document_store.list_technical_facts_by_chunk_ids(&chunk_ids).await.context(
            "failed to load chunk-supported technical facts for canonical answer context",
        )?;
    let mut fact_ids = selected_fact_ids_for_canonical_evidence(
        &bundle_refs.bundle.selected_fact_ids,
        &evidence_rows,
        &chunk_supported_facts,
    );
    for evidence in &evidence_rows {
        if let Some(fact_id) = evidence.fact_id
            && !fact_ids.contains(&fact_id)
            && fact_ids.len() < MAX_CANONICAL_ANSWER_TECHNICAL_FACTS
        {
            fact_ids.push(fact_id);
        }
    }
    let mut technical_facts = state
        .arango_document_store
        .list_technical_facts_by_ids(&fact_ids)
        .await
        .context("failed to load technical facts for canonical answer context")?;
    let mut seen_fact_ids = technical_facts.iter().map(|fact| fact.fact_id).collect::<HashSet<_>>();
    for fact in chunk_supported_facts {
        if fact_ids.contains(&fact.fact_id) && seen_fact_ids.insert(fact.fact_id) {
            technical_facts.push(fact);
        }
    }
    technical_facts.sort_by(|left, right| {
        left.fact_kind.cmp(&right.fact_kind).then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    let mut block_ids =
        evidence_rows.iter().filter_map(|evidence| evidence.block_id).collect::<Vec<_>>();
    for chunk in &chunk_rows {
        for block_id in &chunk.support_block_ids {
            if !block_ids.contains(block_id) {
                block_ids.push(*block_id);
            }
        }
    }
    for fact in &technical_facts {
        for block_id in &fact.support_block_ids {
            if !block_ids.contains(block_id) {
                block_ids.push(*block_id);
            }
        }
    }
    let structured_blocks = state
        .arango_document_store
        .list_structured_blocks_by_ids(&block_ids)
        .await
        .context("failed to load structured blocks for canonical answer context")?;
    Ok(CanonicalAnswerEvidence {
        bundle: Some(bundle_refs.bundle),
        chunk_rows,
        structured_blocks,
        technical_facts,
    })
}

fn selected_fact_ids_for_canonical_evidence(
    selected_fact_ids: &[Uuid],
    evidence_rows: &[crate::infra::arangodb::graph_store::KnowledgeEvidenceRow],
    chunk_supported_facts: &[crate::infra::arangodb::document_store::KnowledgeTechnicalFactRow],
) -> Vec<Uuid> {
    let mut fact_ids = selected_fact_ids.to_vec();
    for evidence in evidence_rows {
        let Some(fact_id) = evidence.fact_id else {
            continue;
        };
        if fact_ids.len() >= MAX_CANONICAL_ANSWER_TECHNICAL_FACTS {
            break;
        }
        if !fact_ids.contains(&fact_id) {
            fact_ids.push(fact_id);
        }
    }
    if fact_ids.is_empty() {
        for fact in chunk_supported_facts {
            if fact_ids.len() >= MAX_CANONICAL_ANSWER_TECHNICAL_FACTS {
                break;
            }
            if !fact_ids.contains(&fact.fact_id) {
                fact_ids.push(fact.fact_id);
            }
        }
    }
    fact_ids.truncate(MAX_CANONICAL_ANSWER_TECHNICAL_FACTS);
    fact_ids
}

async fn search_community_summaries(
    state: &AppState,
    library_id: Uuid,
    question: &str,
    limit: usize,
) -> Vec<(i32, String, String)> {
    let communities = sqlx::query_as::<_, (i32, Option<String>, Vec<String>, i32)>(
        "SELECT community_id, summary, top_entities, node_count
         FROM runtime_graph_community
         WHERE library_id = $1 AND summary IS NOT NULL
         ORDER BY node_count DESC",
    )
    .bind(library_id)
    .fetch_all(&state.persistence.postgres)
    .await
    .unwrap_or_default();

    let question_lower = question.to_ascii_lowercase();
    let question_words: Vec<&str> = question_lower.split_whitespace().collect();

    let mut scored: Vec<_> = communities
        .into_iter()
        .filter_map(|(cid, summary, entities, _)| {
            let summary = summary?;
            let summary_lower = summary.to_ascii_lowercase();
            let entity_text = entities.join(" ").to_ascii_lowercase();

            let score: usize = question_words
                .iter()
                .filter(|w| {
                    w.len() > 2 && (summary_lower.contains(**w) || entity_text.contains(**w))
                })
                .count();

            if score > 0 { Some((score, cid, summary, entities.join(", "))) } else { None }
        })
        .collect();

    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.truncate(limit);

    scored.into_iter().map(|(_, cid, summary, entities)| (cid, summary, entities)).collect()
}

fn format_community_context(matches: &[(i32, String, String)]) -> Option<String> {
    if matches.is_empty() {
        return None;
    }
    let lines: Vec<String> = matches
        .iter()
        .map(|(_, summary, entities)| format!("- {summary} (key entities: {entities})"))
        .collect();
    Some(format!("Knowledge graph communities:\n{}", lines.join("\n")))
}

fn build_canonical_answer_context(
    question: &str,
    structured: &RuntimeStructuredQueryResult,
    evidence: &CanonicalAnswerEvidence,
    canonical_answer_chunks: &[RuntimeMatchedChunk],
    fallback_context: &str,
    community_context: Option<&str>,
) -> String {
    let focused_document_id = focused_answer_document_id(question, canonical_answer_chunks);
    let focused_document_label = focused_document_id.and_then(|document_id| {
        canonical_answer_chunks
            .iter()
            .find(|chunk| chunk.document_id == document_id)
            .map(|chunk| chunk.document_label.clone())
    });
    let filtered_technical_facts = focused_document_id.map_or_else(
        || evidence.technical_facts.clone(),
        |document_id| {
            evidence
                .technical_facts
                .iter()
                .filter(|fact| fact.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let filtered_structured_blocks = focused_document_id.map_or_else(
        || evidence.structured_blocks.clone(),
        |document_id| {
            evidence
                .structured_blocks
                .iter()
                .filter(|block| block.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let filtered_chunks = focused_document_id.map_or_else(
        || canonical_answer_chunks.to_vec(),
        |document_id| {
            canonical_answer_chunks
                .iter()
                .filter(|chunk| chunk.document_id == document_id)
                .cloned()
                .collect::<Vec<_>>()
        },
    );
    let mut sections = Vec::<String>::new();

    if let Some(technical_literals_text) = structured.technical_literals_text.as_deref()
        && !technical_literals_text.trim().is_empty()
    {
        sections.push(technical_literals_text.trim().to_string());
    }

    if let Some(document_label) = focused_document_label.as_deref() {
        sections.push(format!("Focused grounded document\n- {document_label}"));
        sections.push(
            "When a document summary is available in the context, use it to frame the answer."
                .to_string(),
        );
    }

    let table_summary_section = render_table_summary_chunk_section(question, &filtered_chunks);
    let suppress_tabular_detail =
        question_asks_table_aggregation(question) && !table_summary_section.is_empty();
    if !table_summary_section.is_empty() {
        sections.push(table_summary_section);
    }

    if !suppress_tabular_detail {
        let technical_fact_section =
            render_canonical_technical_fact_section(&filtered_technical_facts);
        if !technical_fact_section.is_empty() {
            sections.push(technical_fact_section);
        }
    }

    if let Some(community_text) = community_context {
        if !community_text.is_empty() {
            sections.push(community_text.to_string());
        }
    }

    let prepared_segment_section = render_prepared_segment_section(
        question,
        &filtered_structured_blocks,
        suppress_tabular_detail,
    );
    if !prepared_segment_section.is_empty() {
        sections.push(prepared_segment_section);
    }

    let chunk_section =
        render_canonical_chunk_section(question, &filtered_chunks, suppress_tabular_detail);
    if !chunk_section.is_empty() {
        sections.push(chunk_section);
    }

    if sections.is_empty() {
        return fallback_context.trim().to_string();
    }

    if let Some(bundle) = evidence.bundle.as_ref() {
        sections.insert(
            0,
            format!(
                "Canonical query bundle\n- Strategy: {}\n- Requested mode: {}\n- Resolved mode: {}",
                bundle.bundle_strategy, bundle.requested_mode, bundle.resolved_mode
            ),
        );
    }

    sections.join("\n\n")
}
