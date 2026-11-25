use std::sync::Mutex;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::integrations::llm::{
    ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
    EmbeddingResponse, LlmGateway, VisionRequest, VisionResponse,
};

use super::super::{
    embedding::{dispatch_revision_chunk_embedding_batch, embedding_provider_failure},
    failures::fail_embed_chunks_preserving_partial,
};

#[derive(Default)]
struct RecordingGateway {
    provider_call_ids: Mutex<Vec<Option<String>>>,
    send_owners: Mutex<Vec<Option<String>>>,
}

#[async_trait]
impl LlmGateway for RecordingGateway {
    async fn generate(&self, _: ChatRequest) -> anyhow::Result<ChatResponse> {
        anyhow::bail!("chat is not used by the embedding batch")
    }

    async fn embed(&self, _: EmbeddingRequest) -> anyhow::Result<EmbeddingResponse> {
        anyhow::bail!("single embedding is not used by the embedding batch")
    }

    async fn embed_many(
        &self,
        request: EmbeddingBatchRequest,
    ) -> anyhow::Result<EmbeddingBatchResponse> {
        self.provider_call_ids
            .lock()
            .unwrap()
            .push(crate::observability::current_provider_call_id());
        self.send_owners.lock().unwrap().push(
            crate::shared::provider_send::current_send_attribution()
                .map(|attribution| attribution.owning_execution_kind),
        );
        Ok(EmbeddingBatchResponse {
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
            dimensions: 1,
            embeddings: vec![vec![0.5]; request.inputs.len()],
            usage_json: serde_json::json!({"prompt_tokens": 3}),
        })
    }

    async fn vision_extract(&self, _: VisionRequest) -> anyhow::Result<VisionResponse> {
        anyhow::bail!("vision is not used by the embedding batch")
    }
}

fn request() -> EmbeddingBatchRequest {
    EmbeddingBatchRequest {
        provider_kind: "test".to_string(),
        model_name: "embedding".to_string(),
        inputs: vec!["evidence".to_string()],
        api_key_override: None,
        base_url_override: None,
        extra_parameters_json: serde_json::json!({}),
        dispatch: None,
    }
}

fn embedding_scope() -> crate::shared::provider_send::ProviderSendScope {
    crate::shared::provider_send::ProviderSendScope::new(
        crate::shared::provider_send::ProviderSendAttribution {
            workspace_id: Uuid::from_u128(31),
            library_id: Uuid::from_u128(32),
            owning_execution_kind: "knowledge_search".to_string(),
            owning_execution_id: Uuid::from_u128(33),
            call_kind: "knowledge_search_embedding".to_string(),
            binding_id: None,
            provider_catalog_id: Uuid::from_u128(34),
            model_catalog_id: Uuid::from_u128(35),
            runtime_execution_id: None,
            runtime_task_kind: None,
        },
        Uuid::from_u128(21),
    )
}

#[tokio::test]
async fn batch_dispatch_installs_the_reservation_scope() {
    let gateway = RecordingGateway::default();
    let response = dispatch_revision_chunk_embedding_batch(
        &gateway,
        embedding_scope(),
        request(),
        &CancellationToken::new(),
    )
    .await
    .expect("dispatch succeeds");
    // The transport mock admits nothing, so no send row exists and the trace
    // label stays empty; the attribution itself must be visible.
    assert_eq!(*gateway.provider_call_ids.lock().unwrap(), vec![None]);
    assert_eq!(*gateway.send_owners.lock().unwrap(), vec![Some("knowledge_search".to_string())]);
    assert_eq!(response.usage_json, serde_json::json!({"prompt_tokens": 3}));
}

#[tokio::test]
async fn cancelled_batch_does_not_dispatch() {
    let gateway = RecordingGateway::default();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let result = dispatch_revision_chunk_embedding_batch(
        &gateway,
        embedding_scope(),
        request(),
        &cancellation,
    )
    .await;
    assert!(result.is_err());
    assert!(gateway.provider_call_ids.lock().unwrap().is_empty());
}

#[test]
fn embedding_provider_identity_survives_anyhow_and_partial_vector_policy() {
    use crate::integrations::{provider_budget::ProviderBudgetError, retry::ProviderCallError};
    use crate::services::query::error::QueryServiceError;
    use crate::shared::provider_http::ProviderDispatchRefusal;

    let id = Uuid::from_u128(21);
    let source = anyhow::Error::new(ProviderCallError::admission(ProviderBudgetError::Dispatch(
        ProviderDispatchRefusal::Authority,
    )))
    .context("provider dispatch");
    let wrapped = embedding_provider_failure(source, id).context("batch failure");
    let query_error = QueryServiceError::from(wrapped);
    assert_eq!(query_error.provider_call_id(), Some(id));
    assert_eq!(
        query_error.provider_failure().and_then(|failure| failure.refusal_code()),
        Some("dispatch_authority")
    );
    assert!(query_error.preserves_partial_vectors());
    let result = fail_embed_chunks_preserving_partial::<()>(Uuid::from_u128(22), query_error);
    let error = result.expect_err("partial vector policy returns the original failure");
    assert_eq!(error.provider_call_id(), Some(id));
    assert_eq!(error.kind(), "QueryServiceError::ProviderAdmissionRefused");
}

#[test]
fn non_provider_failures_do_not_acquire_provider_identity() {
    use crate::services::query::error::QueryServiceError;

    let error = embedding_provider_failure(anyhow::anyhow!("unknown failure"), Uuid::from_u128(21));
    let error = QueryServiceError::from(error);
    assert!(matches!(error, QueryServiceError::Internal(_)));
    assert_eq!(error.provider_call_id(), None);
    assert!(!error.preserves_partial_vectors());
}
