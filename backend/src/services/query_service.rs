use std::{
    collections::{BTreeSet, HashMap},
    time::Instant,
};

use chrono::Utc;
use serde_json::json;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::catalog::CatalogLifecycleState,
    domains::query::{
        QueryChunkReference, QueryConversation, QueryConversationDetail, QueryExecution,
        QueryExecutionDetail, QueryGraphEdgeReference, QueryGraphNodeReference, QueryTurn,
        RuntimeQueryMode,
    },
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
            document_store::KnowledgeLibraryGenerationRow,
            graph_store::KnowledgeGraphTraversalRow,
        },
        repositories::{ai_repository, query_repository},
    },
    integrations::llm::EmbeddingRequest,
    interfaces::http::router_support::ApiError,
    services::{
        billing_service::CaptureQueryExecutionBillingCommand,
        ops_service::CreateAsyncOperationCommand, query_execution::execute_answer_query,
    },
};

#[derive(Debug, Clone)]
pub struct CreateConversationCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub created_by_principal_id: Option<Uuid>,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExecuteConversationTurnCommand {
    pub conversation_id: Uuid,
    pub author_principal_id: Option<Uuid>,
    pub content_text: String,
    pub mode: RuntimeQueryMode,
    pub top_k: usize,
    pub include_debug: bool,
}

#[derive(Debug, Clone)]
pub struct QueryTurnExecutionResult {
    pub conversation: QueryConversation,
    pub request_turn: QueryTurn,
    pub response_turn: Option<QueryTurn>,
    pub execution: QueryExecution,
    pub context_bundle_id: Uuid,
}

#[derive(Clone, Default)]
pub struct QueryService;

impl QueryService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn list_conversations(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<QueryConversation>, ApiError> {
        let rows = query_repository::list_conversations_by_library(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_conversation_row).collect())
    }

    pub async fn get_conversation(
        &self,
        state: &AppState,
        conversation_id: Uuid,
    ) -> Result<QueryConversationDetail, ApiError> {
        let conversation =
            query_repository::get_conversation_by_id(&state.persistence.postgres, conversation_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("conversation", conversation_id))?;
        let turns = query_repository::list_turns_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let executions = query_repository::list_executions_by_conversation(
            &state.persistence.postgres,
            conversation.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(QueryConversationDetail {
            conversation: map_conversation_row(conversation),
            turns: turns.into_iter().map(map_turn_row).collect(),
            executions: executions.into_iter().map(map_execution_row).collect(),
        })
    }

    pub async fn create_conversation(
        &self,
        state: &AppState,
        command: CreateConversationCommand,
    ) -> Result<QueryConversation, ApiError> {
        let title = normalize_optional_text(command.title.as_deref());
        let library =
            state.canonical_services.catalog.get_library(state, command.library_id).await?;
        if library.workspace_id != command.workspace_id {
            return Err(ApiError::Conflict(format!(
                "library {} does not belong to workspace {}",
                library.id, command.workspace_id
            )));
        }
        if library.lifecycle_state != CatalogLifecycleState::Active {
            return Err(ApiError::Conflict(format!("library {} is not active", library.id)));
        }
        let row = query_repository::create_conversation(
            &state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: library.workspace_id,
                library_id: library.id,
                created_by_principal_id: command.created_by_principal_id,
                title: title.as_deref(),
                conversation_state: "active",
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_conversation_row(row))
    }

    pub async fn execute_turn(
        &self,
        state: &AppState,
        command: ExecuteConversationTurnCommand,
    ) -> Result<QueryTurnExecutionResult, ApiError> {
        let conversation = query_repository::get_conversation_by_id(
            &state.persistence.postgres,
            command.conversation_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("conversation", command.conversation_id))?;
        if conversation.conversation_state != "active" {
            return Err(ApiError::Conflict(format!(
                "conversation {} is not active",
                conversation.id
            )));
        }
        let library =
            state.canonical_services.catalog.get_library(state, conversation.library_id).await?;
        if library.workspace_id != conversation.workspace_id {
            return Err(ApiError::Conflict(format!(
                "conversation {} has library {} outside workspace {}",
                conversation.id, library.id, conversation.workspace_id
            )));
        }
        if library.lifecycle_state != CatalogLifecycleState::Active {
            return Err(ApiError::Conflict(format!("library {} is not active", library.id)));
        }

        let content_text = normalize_required_text(&command.content_text, "contentText")?;
        let request_turn = query_repository::create_turn(
            &state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: conversation.id,
                turn_kind: "user",
                author_principal_id: command.author_principal_id,
                content_text: &content_text,
                execution_id: None,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        let binding_id = ai_repository::get_active_library_binding_by_purpose(
            &state.persistence.postgres,
            conversation.library_id,
            "query_answer",
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .map(|binding| binding.id);

        let execution_id = Uuid::now_v7();
        let execution_context_bundle_id = Uuid::now_v7();
        let execution = query_repository::create_execution(
            &state.persistence.postgres,
            &query_repository::NewQueryExecution {
                execution_id,
                context_bundle_id: execution_context_bundle_id,
                workspace_id: conversation.workspace_id,
                library_id: conversation.library_id,
                conversation_id: conversation.id,
                request_turn_id: Some(request_turn.id),
                response_turn_id: None,
                binding_id,
                execution_state: "retrieving",
                query_text: &content_text,
                failure_code: None,
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let async_operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: conversation.workspace_id,
                    library_id: conversation.library_id,
                    operation_kind: "query_execution".to_string(),
                    surface_kind: "rest".to_string(),
                    requested_by_principal_id: command.author_principal_id,
                    status: "accepted".to_string(),
                    subject_kind: "query_execution".to_string(),
                    subject_id: Some(execution.id),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;
        let async_operation = state
            .canonical_services
            .ops
            .update_async_operation(
                state,
                crate::services::ops_service::UpdateAsyncOperationCommand {
                    operation_id: async_operation.id,
                    status: "processing".to_string(),
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;

        let top_k = command.top_k.clamp(1, 12);
        let runtime_result = match execute_answer_query(
            state,
            library.id,
            content_text.clone(),
            None,
            command.mode,
            top_k,
            command.include_debug,
        )
        .await
        {
            Ok(result) => result,
            Err(error) => {
                let message = error.to_string();
                let failed = query_repository::update_execution(
                    &state.persistence.postgres,
                    execution.id,
                    &query_repository::UpdateQueryExecution {
                        execution_state: "failed",
                        request_turn_id: Some(request_turn.id),
                        response_turn_id: None,
                        failure_code: Some(truncate_failure_code(&message)),
                        completed_at: Some(Utc::now()),
                    },
                )
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))?;
                let _ = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        crate::services::ops_service::UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: "failed".to_string(),
                            completed_at: Some(Utc::now()),
                            failure_code: Some(truncate_failure_code(&message).to_string()),
                        },
                    )
                    .await;
                return Err(map_query_execution_error_message(
                    failed.id,
                    &failed.query_text,
                    message,
                ));
            }
        };

        match assemble_context_bundle(
            state,
            &conversation,
            execution.id,
            execution_context_bundle_id,
            &content_text,
            command.mode,
            top_k,
            command.include_debug,
            runtime_result.structured.planned_mode,
        )
        .await
        {
            Ok(()) => {}
            Err(error) => {
                let message = format!("failed to assemble knowledge context bundle: {error}");
                let failed = query_repository::update_execution(
                    &state.persistence.postgres,
                    execution.id,
                    &query_repository::UpdateQueryExecution {
                        execution_state: "failed",
                        request_turn_id: Some(request_turn.id),
                        response_turn_id: None,
                        failure_code: Some(truncate_failure_code(&message)),
                        completed_at: Some(Utc::now()),
                    },
                )
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))?;
                let _ = state
                    .canonical_services
                    .ops
                    .update_async_operation(
                        state,
                        crate::services::ops_service::UpdateAsyncOperationCommand {
                            operation_id: async_operation.id,
                            status: "failed".to_string(),
                            completed_at: Some(Utc::now()),
                            failure_code: Some(truncate_failure_code(&message).to_string()),
                        },
                    )
                    .await;
                return Err(map_query_execution_error_message(
                    failed.id,
                    &failed.query_text,
                    message,
                ));
            }
        };

        let response_turn = query_repository::create_turn(
            &state.persistence.postgres,
            &query_repository::NewQueryTurn {
                conversation_id: conversation.id,
                turn_kind: "assistant",
                author_principal_id: None,
                content_text: &runtime_result.answer,
                execution_id: Some(execution.id),
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        let execution = query_repository::update_execution(
            &state.persistence.postgres,
            execution.id,
            &query_repository::UpdateQueryExecution {
                execution_state: "completed",
                request_turn_id: Some(request_turn.id),
                response_turn_id: Some(response_turn.id),
                failure_code: None,
                completed_at: Some(Utc::now()),
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("query_execution", execution.id))?;
        let _ = state
            .canonical_services
            .ops
            .update_async_operation(
                state,
                crate::services::ops_service::UpdateAsyncOperationCommand {
                    operation_id: async_operation.id,
                    status: "ready".to_string(),
                    completed_at: Some(Utc::now()),
                    failure_code: None,
                },
            )
            .await;

        if let Err(error) = state
            .canonical_services
            .billing
            .capture_query_execution(
                state,
                CaptureQueryExecutionBillingCommand {
                    workspace_id: conversation.workspace_id,
                    library_id: conversation.library_id,
                    execution_id: execution.id,
                    binding_id: execution.binding_id,
                    provider_kind: runtime_result.provider.provider_kind.as_str().to_string(),
                    model_name: runtime_result.provider.model_name,
                    usage_json: runtime_result.usage_json,
                },
            )
            .await
        {
            warn!(error = %error, execution_id = %execution.id, "canonical query billing capture failed");
        }

        Ok(QueryTurnExecutionResult {
            conversation: map_conversation_row(conversation),
            request_turn: map_turn_row(request_turn),
            response_turn: Some(map_turn_row(response_turn)),
            execution: map_execution_row(execution),
            context_bundle_id: execution_context_bundle_id,
        })
    }

    pub async fn get_execution(
        &self,
        state: &AppState,
        execution_id: Uuid,
    ) -> Result<QueryExecutionDetail, ApiError> {
        let execution =
            query_repository::get_execution_by_id(&state.persistence.postgres, execution_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("query_execution", execution_id))?;
        let request_turn = match execution.request_turn_id {
            Some(turn_id) => query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .map(map_turn_row),
            None => None,
        };
        let response_turn = match execution.response_turn_id {
            Some(turn_id) => query_repository::get_turn_by_id(&state.persistence.postgres, turn_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .map(map_turn_row),
            None => None,
        };
        let bundle_refs = state
            .arango_context_store
            .get_bundle_reference_set_by_query_execution(execution.id)
            .await
            .map_err(|_| ApiError::Internal)?;

        Ok(QueryExecutionDetail {
            execution: map_execution_row(execution),
            request_turn,
            response_turn,
            chunk_references: bundle_refs.as_ref().map_or_else(Vec::new, map_chunk_references),
            graph_node_references: bundle_refs
                .as_ref()
                .map_or_else(Vec::new, map_entity_references),
            graph_edge_references: bundle_refs
                .as_ref()
                .map_or_else(Vec::new, map_relation_references),
        })
    }
}

#[derive(Debug, Clone)]
struct QueryEmbeddingContext {
    model_catalog_id: Uuid,
    freshness_generation: i64,
    query_vector: Vec<f32>,
}

#[derive(Debug, Clone, Default)]
struct RankedBundleReference {
    rank: i32,
    score: f64,
    reasons: BTreeSet<String>,
}

async fn assemble_context_bundle(
    state: &AppState,
    conversation: &query_repository::QueryConversationRow,
    execution_id: Uuid,
    bundle_id: Uuid,
    query_text: &str,
    requested_mode: RuntimeQueryMode,
    top_k: usize,
    include_debug: bool,
    resolved_mode: RuntimeQueryMode,
) -> Result<(), ApiError> {
    let started_at = Instant::now();
    let candidate_limit = top_k.saturating_mul(3).max(6);

    let lexical_chunk_hits = state
        .arango_search_store
        .search_chunks(conversation.library_id, query_text, candidate_limit)
        .await
        .map_err(|_| ApiError::Internal)?;
    let lexical_entity_hits = state
        .arango_search_store
        .search_entities(conversation.library_id, query_text, candidate_limit)
        .await
        .map_err(|_| ApiError::Internal)?;
    let lexical_relation_hits = state
        .arango_search_store
        .search_relations(conversation.library_id, query_text, candidate_limit)
        .await
        .map_err(|_| ApiError::Internal)?;

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
            .map_err(|_| ApiError::Internal)?
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
            .map_err(|_| ApiError::Internal)?
    } else {
        Vec::new()
    };

    let mut chunk_refs: HashMap<Uuid, RankedBundleReference> = HashMap::new();
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
            .map_err(|_| ApiError::Internal)?;
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
            .map_err(|_| ApiError::Internal)?;
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
            .map_err(|_| ApiError::Internal)?;
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

    let now = Utc::now();
    let generations = state
        .arango_document_store
        .list_library_generations(conversation.library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let generation = generations.first().cloned();
    let freshness_snapshot =
        generation.as_ref().map(freshness_snapshot_json).unwrap_or_else(|| json!({}));
    let retrieval_strategy =
        if embedding_context.is_some() { "hybrid".to_string() } else { "lexical".to_string() };
    let chunk_edges = build_chunk_bundle_edges(bundle_id, &chunk_refs, now);
    let entity_edges = build_entity_bundle_edges(bundle_id, &entity_refs, now);
    let relation_edges = build_relation_bundle_edges(bundle_id, &relation_refs, now);
    let evidence_edges = build_evidence_bundle_edges(bundle_id, &evidence_refs, now);

    let candidate_summary = json!({
        "lexicalChunkHits": lexical_chunk_hits.len(),
        "vectorChunkHits": vector_chunk_hits.len(),
        "lexicalEntityHits": lexical_entity_hits.len(),
        "vectorEntityHits": vector_entity_hits.len(),
        "lexicalRelationHits": lexical_relation_hits.len(),
        "entityNeighborhoodRows": entity_neighborhood_rows,
        "relationTraversalRows": relation_traversal_rows,
        "relationEvidenceRows": relation_evidence_rows,
        "finalChunkReferences": chunk_edges.len(),
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
        freshness_snapshot: freshness_snapshot.clone(),
        candidate_summary: candidate_summary.clone(),
        assembly_diagnostics: assembly_diagnostics.clone(),
        created_at: now,
        updated_at: now,
    };
    state.arango_context_store.upsert_bundle(&bundle_row).await.map_err(|_| ApiError::Internal)?;
    state
        .arango_context_store
        .replace_bundle_chunk_edges(bundle_id, &chunk_edges)
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .arango_context_store
        .replace_bundle_entity_edges(bundle_id, &entity_edges)
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .arango_context_store
        .replace_bundle_relation_edges(bundle_id, &relation_edges)
        .await
        .map_err(|_| ApiError::Internal)?;
    state
        .arango_context_store
        .replace_bundle_evidence_edges(bundle_id, &evidence_edges)
        .await
        .map_err(|_| ApiError::Internal)?;

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
        state.arango_context_store.upsert_trace(&trace).await.map_err(|_| ApiError::Internal)?;
    }

    Ok(())
}

async fn resolve_query_embedding_context(
    state: &AppState,
    library_id: Uuid,
    query_text: &str,
) -> Result<Option<QueryEmbeddingContext>, ApiError> {
    let Some(binding) = ai_repository::get_active_library_binding_by_purpose(
        &state.persistence.postgres,
        library_id,
        "embed_chunk",
    )
    .await
    .map_err(|_| ApiError::Internal)?
    else {
        return Ok(None);
    };

    let provider_credential = state
        .canonical_services
        .ai_catalog
        .get_provider_credential(state, binding.provider_credential_id)
        .await?;
    let model_preset = state
        .canonical_services
        .ai_catalog
        .get_model_preset(state, binding.model_preset_id)
        .await?;
    let providers = state.canonical_services.ai_catalog.list_provider_catalog(state).await?;
    let models = state.canonical_services.ai_catalog.list_model_catalog(state, None).await?;
    let Some(provider_kind) = providers
        .into_iter()
        .find(|provider| provider.id == provider_credential.provider_catalog_id)
        .map(|provider| provider.provider_kind)
    else {
        return Ok(None);
    };
    let Some(model) = models.into_iter().find(|model| model.id == model_preset.model_catalog_id)
    else {
        return Ok(None);
    };
    if model.provider_catalog_id != provider_credential.provider_catalog_id {
        return Ok(None);
    }

    let generations = state
        .arango_document_store
        .list_library_generations(library_id)
        .await
        .map_err(|_| ApiError::Internal)?;
    let Some(generation) = generations.first() else {
        return Ok(None);
    };
    if generation.active_vector_generation <= 0 {
        return Ok(None);
    }

    let embedding = state
        .llm_gateway
        .embed(EmbeddingRequest {
            provider_kind,
            model_name: model.model_name.clone(),
            input: query_text.to_string(),
        })
        .await
        .map_err(|error| {
            ApiError::ProviderFailure(format!("failed to embed query bundle request: {error}"))
        })?;

    Ok(Some(QueryEmbeddingContext {
        model_catalog_id: model.id,
        freshness_generation: generation.active_vector_generation,
        query_vector: embedding.embedding,
    }))
}

fn merge_ranked_reference(
    refs: &mut HashMap<Uuid, RankedBundleReference>,
    target_id: Uuid,
    rank: i32,
    score: f64,
    reason: &str,
) {
    let entry = refs.entry(target_id).or_insert_with(|| RankedBundleReference {
        rank,
        score,
        reasons: BTreeSet::new(),
    });
    entry.rank = entry.rank.min(rank);
    if score > entry.score {
        entry.score = score;
    }
    entry.reasons.insert(reason.to_string());
}

fn top_ranked_ids(refs: &HashMap<Uuid, RankedBundleReference>, limit: usize) -> Vec<Uuid> {
    let mut items: Vec<(Uuid, &RankedBundleReference)> =
        refs.iter().map(|(id, rank)| (*id, rank)).collect();
    items.sort_by(|(left_id, left), (right_id, right)| {
        left.rank
            .cmp(&right.rank)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left_id.cmp(right_id))
    });
    items.into_iter().take(limit).map(|(id, _)| id).collect()
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

fn runtime_mode_label(mode: RuntimeQueryMode) -> &'static str {
    match mode {
        RuntimeQueryMode::Document => "document",
        RuntimeQueryMode::Local => "local",
        RuntimeQueryMode::Global => "global",
        RuntimeQueryMode::Hybrid => "hybrid",
        RuntimeQueryMode::Mix => "mix",
    }
}

fn map_chunk_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryChunkReference> {
    let execution_id = bundle
        .bundle
        .query_execution_id
        .expect("query context bundle must carry query_execution_id");
    bundle
        .chunk_references
        .iter()
        .map(|reference| QueryChunkReference {
            execution_id,
            chunk_id: reference.chunk_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

fn map_entity_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryGraphNodeReference> {
    let execution_id = bundle
        .bundle
        .query_execution_id
        .expect("query context bundle must carry query_execution_id");
    bundle
        .entity_references
        .iter()
        .map(|reference| QueryGraphNodeReference {
            execution_id,
            node_id: reference.entity_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

fn map_relation_references(
    bundle: &KnowledgeContextBundleReferenceSetRow,
) -> Vec<QueryGraphEdgeReference> {
    let execution_id = bundle
        .bundle
        .query_execution_id
        .expect("query context bundle must carry query_execution_id");
    bundle
        .relation_references
        .iter()
        .map(|reference| QueryGraphEdgeReference {
            execution_id,
            edge_id: reference.relation_id,
            rank: reference.rank,
            score: reference.score,
        })
        .collect()
}

fn map_conversation_row(row: query_repository::QueryConversationRow) -> QueryConversation {
    QueryConversation {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        created_by_principal_id: row.created_by_principal_id,
        title: row.title,
        conversation_state: row.conversation_state,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn map_turn_row(row: query_repository::QueryTurnRow) -> QueryTurn {
    QueryTurn {
        id: row.id,
        conversation_id: row.conversation_id,
        turn_index: row.turn_index,
        turn_kind: row.turn_kind,
        author_principal_id: row.author_principal_id,
        content_text: row.content_text,
        execution_id: row.execution_id,
        created_at: row.created_at,
    }
}

fn map_execution_row(row: query_repository::QueryExecutionRow) -> QueryExecution {
    QueryExecution {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        conversation_id: row.conversation_id,
        context_bundle_id: row.context_bundle_id,
        request_turn_id: row.request_turn_id,
        response_turn_id: row.response_turn_id,
        binding_id: row.binding_id,
        execution_state: row.execution_state,
        query_text: row.query_text,
        failure_code: row.failure_code,
        started_at: row.started_at,
        completed_at: row.completed_at,
    }
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToString::to_string)
}

fn normalize_required_text(value: &str, field: &str) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest(format!("{field} is required")));
    }
    Ok(normalized.to_string())
}

fn saturating_rank(index: usize) -> i32 {
    i32::try_from(index.saturating_add(1)).unwrap_or(i32::MAX)
}

fn truncate_failure_code(message: &str) -> &str {
    const LIMIT: usize = 120;
    let truncated = message.trim();
    if truncated.len() <= LIMIT {
        truncated
    } else {
        let cutoff =
            truncated.char_indices().nth(LIMIT).map_or(truncated.len(), |(index, _)| index);
        &truncated[..cutoff]
    }
}

fn map_query_execution_error_message(
    execution_id: Uuid,
    query_text: &str,
    message: String,
) -> ApiError {
    let normalized = message.to_ascii_lowercase();
    if normalized.contains("missing openai api key")
        || normalized.contains("missing deepseek api key")
        || normalized.contains("missing qwen api key")
        || normalized.contains("failed to generate grounded answer")
        || normalized.contains("failed to embed runtime query")
    {
        ApiError::Conflict(format!(
            "query execution {execution_id} for '{query_text}' failed: {message}"
        ))
    } else {
        ApiError::Internal
    }
}
