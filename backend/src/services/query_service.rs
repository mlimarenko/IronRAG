use chrono::Utc;
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::{
        query::{
            QueryChunkReference, QueryConversation, QueryConversationDetail, QueryExecution,
            QueryExecutionDetail, QueryGraphEdgeReference, QueryGraphNodeReference, QueryTurn,
        },
        query_modes::RuntimeQueryMode,
    },
    infra::repositories::{ai_repository, query_repository},
    interfaces::http::router_support::ApiError,
    services::{
        billing_service::CaptureQueryExecutionBillingCommand,
        query_runtime::{RuntimeQueryRequest, execute_answer_query},
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
    pub chunk_references: Vec<QueryChunkReference>,
    pub graph_node_references: Vec<QueryGraphNodeReference>,
    pub graph_edge_references: Vec<QueryGraphEdgeReference>,
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
        let row = query_repository::create_conversation(
            &state.persistence.postgres,
            &query_repository::NewQueryConversation {
                workspace_id: command.workspace_id,
                library_id: command.library_id,
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

        let execution = query_repository::create_execution(
            &state.persistence.postgres,
            &query_repository::NewQueryExecution {
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

        let runtime_request = RuntimeQueryRequest {
            library_id: conversation.library_id,
            question: content_text.clone(),
            system_prompt: None,
            mode: command.mode,
            top_k: command.top_k.clamp(1, 12),
            include_debug: command.include_debug,
        };

        let runtime_result = match execute_answer_query(state, &runtime_request).await {
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
                return Err(map_runtime_query_error_message(
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

        let chunk_references = query_repository::replace_chunk_references(
            &state.persistence.postgres,
            execution.id,
            &runtime_result
                .structured
                .chunks
                .iter()
                .enumerate()
                .map(|(index, chunk)| query_repository::NewQueryChunkReference {
                    chunk_id: chunk.chunk_id,
                    rank: saturating_rank(index),
                    score: chunk.score.map(f64::from).unwrap_or_default(),
                })
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let graph_node_references = query_repository::replace_graph_node_references(
            &state.persistence.postgres,
            execution.id,
            &runtime_result
                .structured
                .entities
                .iter()
                .enumerate()
                .map(|(index, node)| query_repository::NewQueryGraphNodeReference {
                    node_id: node.node_id,
                    rank: saturating_rank(index),
                    score: node.score.map(f64::from).unwrap_or_default(),
                })
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let graph_edge_references = query_repository::replace_graph_edge_references(
            &state.persistence.postgres,
            execution.id,
            &runtime_result
                .structured
                .relationships
                .iter()
                .enumerate()
                .map(|(index, edge)| query_repository::NewQueryGraphEdgeReference {
                    edge_id: edge.edge_id,
                    rank: saturating_rank(index),
                    score: edge.score.map(f64::from).unwrap_or_default(),
                })
                .collect::<Vec<_>>(),
        )
        .await
        .map_err(|_| ApiError::Internal)?;

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
            chunk_references: chunk_references.into_iter().map(map_chunk_reference_row).collect(),
            graph_node_references: graph_node_references
                .into_iter()
                .map(map_graph_node_reference_row)
                .collect(),
            graph_edge_references: graph_edge_references
                .into_iter()
                .map(map_graph_edge_reference_row)
                .collect(),
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
        let chunk_references = query_repository::list_chunk_references_by_execution(
            &state.persistence.postgres,
            execution.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let graph_node_references = query_repository::list_graph_node_references_by_execution(
            &state.persistence.postgres,
            execution.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let graph_edge_references = query_repository::list_graph_edge_references_by_execution(
            &state.persistence.postgres,
            execution.id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        Ok(QueryExecutionDetail {
            execution: map_execution_row(execution),
            request_turn,
            response_turn,
            chunk_references: chunk_references.into_iter().map(map_chunk_reference_row).collect(),
            graph_node_references: graph_node_references
                .into_iter()
                .map(map_graph_node_reference_row)
                .collect(),
            graph_edge_references: graph_edge_references
                .into_iter()
                .map(map_graph_edge_reference_row)
                .collect(),
        })
    }
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

fn map_chunk_reference_row(row: query_repository::QueryChunkReferenceRow) -> QueryChunkReference {
    QueryChunkReference {
        execution_id: row.execution_id,
        chunk_id: row.chunk_id,
        rank: row.rank,
        score: row.score,
    }
}

fn map_graph_node_reference_row(
    row: query_repository::QueryGraphNodeReferenceRow,
) -> QueryGraphNodeReference {
    QueryGraphNodeReference {
        execution_id: row.execution_id,
        node_id: row.node_id,
        rank: row.rank,
        score: row.score,
    }
}

fn map_graph_edge_reference_row(
    row: query_repository::QueryGraphEdgeReferenceRow,
) -> QueryGraphEdgeReference {
    QueryGraphEdgeReference {
        execution_id: row.execution_id,
        edge_id: row.edge_id,
        rank: row.rank,
        score: row.score,
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

fn map_runtime_query_error_message(
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
