use rust_decimal::Decimal;
use uuid::Uuid;

use crate::{
    app::state::AppState, infra::repositories, integrations::llm::EmbeddingRequest,
    interfaces::http::router_support::ApiError, shared::chunking::split_text_into_chunks,
};

pub struct TextIngestRequest<'a> {
    pub project_id: Uuid,
    pub source_id: Option<Uuid>,
    pub external_key: &'a str,
    pub title: Option<&'a str>,
    pub mime_type: Option<&'a str>,
    pub text: &'a str,
    pub ingest_mode: &'a str,
    pub extra_metadata: serde_json::Value,
}

/// Ingests a plain-text payload into a document and project chunks.
///
/// # Errors
/// Returns [`ApiError::Internal`] when document or chunk persistence fails.
pub async fn ingest_plain_text(
    state: &AppState,
    request: TextIngestRequest<'_>,
) -> Result<(Uuid, usize), ApiError> {
    let document = repositories::create_document(
        &state.persistence.postgres,
        request.project_id,
        request.source_id,
        request.external_key,
        request.title,
        request.mime_type,
        None,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    let chunks = split_text_into_chunks(request.text, 1200);
    let mut chunk_count = 0usize;
    for (idx, content) in chunks.iter().enumerate() {
        repositories::create_chunk(
            &state.persistence.postgres,
            document.id,
            request.project_id,
            i32::try_from(idx).unwrap_or(i32::MAX),
            content,
            Some(i32::try_from(content.split_whitespace().count()).unwrap_or(i32::MAX)),
            serde_json::json!({
                "ingest_mode": request.ingest_mode,
                "extra": request.extra_metadata,
            }),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        chunk_count += 1;
    }

    Ok((document.id, chunk_count))
}

/// Generates embeddings for project chunks and records usage/cost data.
///
/// # Errors
/// Returns [`ApiError::Internal`] when loading chunks, calling the embedding gateway,
/// or persisting embeddings, usage events, or cost ledger rows fails.
pub async fn embed_project_chunks_with_usage(
    state: &AppState,
    project_id: Uuid,
    provider_kind: String,
    model_name: String,
    embedding_model_profile_id: Option<Uuid>,
    limit: i64,
) -> Result<usize, ApiError> {
    let chunks =
        repositories::list_chunks_by_project(&state.persistence.postgres, project_id, limit)
            .await
            .map_err(|_| ApiError::Internal)?;

    let workspace_id = repositories::get_project_by_id(&state.persistence.postgres, project_id)
        .await
        .map_err(|_| ApiError::Internal)?
        .map(|project| project.workspace_id);

    let mut embedded = 0usize;
    for chunk in chunks {
        let embedding = state
            .llm_gateway
            .embed(EmbeddingRequest {
                provider_kind: provider_kind.clone(),
                model_name: model_name.clone(),
                input: chunk.content.clone(),
            })
            .await
            .map_err(|_| ApiError::Internal)?;

        repositories::upsert_chunk_embedding(
            &state.persistence.postgres,
            chunk.id,
            chunk.project_id,
            &embedding.provider_kind,
            &embedding.model_name,
            i32::try_from(embedding.dimensions).unwrap_or(i32::MAX),
            serde_json::to_value(&embedding.embedding).map_err(|_| ApiError::Internal)?,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        let prompt_tokens = embedding
            .usage_json
            .get("prompt_tokens")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());
        let total_tokens = embedding
            .usage_json
            .get("total_tokens")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok());

        let usage_event = repositories::create_usage_event(
            &state.persistence.postgres,
            &repositories::NewUsageEvent {
                workspace_id,
                project_id: Some(project_id),
                provider_account_id: None,
                model_profile_id: embedding_model_profile_id,
                usage_kind: "embedding".to_string(),
                prompt_tokens,
                completion_tokens: None,
                total_tokens,
                raw_usage_json: embedding.usage_json.clone(),
            },
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        let input_price_per_1m = match provider_kind.as_str() {
            "openai" => state.settings.openai_input_price_per_1m,
            "deepseek" => state.settings.deepseek_input_price_per_1m,
            _ => 0.0,
        };
        let estimated_cost =
            f64::from(prompt_tokens.unwrap_or(0)) / 1_000_000.0 * input_price_per_1m;

        repositories::create_cost_ledger(
            &state.persistence.postgres,
            workspace_id,
            Some(project_id),
            usage_event.id,
            &provider_kind,
            &model_name,
            Decimal::from_f64_retain(estimated_cost).unwrap_or(Decimal::ZERO),
            serde_json::json!({"input_price_per_1m": input_price_per_1m}),
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        embedded += 1;
    }

    Ok(embedded)
}
