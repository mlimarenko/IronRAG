//! [`LlmGateway`] decorator that enforces the shared per-provider outbound
//! concurrency budget.
//!
//! This is the wiring layer between the pure
//! [`crate::integrations::provider_budget`] registry and the real provider
//! gateway. It wraps any inner [`LlmGateway`] and, around every delegated
//! outbound call, acquires a budget permit for the call's provider identity on
//! the current task-local lane (see
//! [`crate::integrations::provider_budget::current_lane`]). The permit is held
//! for the full duration of the inner call — including its retry/backoff and
//! streaming-response consumption, which are all genuinely in-flight against the
//! provider — and released when the inner future resolves.
//!
//! When the provider is explicitly configured without a cap, the registry returns
//! an unlimited guard and this decorator is a transparent pass-through.

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::integrations::llm::{
    ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
    EmbeddingResponse, LlmGateway, ToolUseRequest, ToolUseResponse, VisionRequest, VisionResponse,
};
use crate::integrations::provider_budget::{
    ProviderBudgetRegistry, ProviderIdentity, current_lane,
};

/// Builds the provider identity key for an outbound call. The base URL is the
/// per-binding resolved endpoint already stored on the runtime binding, so the
/// raw value is consistent across every call site; trimming a trailing slash is
/// the only normalization needed to canonicalize it.
fn provider_identity(provider_kind: &str, base_url_override: Option<&str>) -> ProviderIdentity {
    let base_url = base_url_override.map(str::trim).unwrap_or_default().trim_end_matches('/');
    ProviderIdentity::new(provider_kind, base_url)
}

/// [`LlmGateway`] decorator enforcing the per-provider concurrency budget.
pub struct ConcurrencyLimitedGateway<G: LlmGateway> {
    inner: G,
    registry: Arc<ProviderBudgetRegistry>,
}

impl<G: LlmGateway> ConcurrencyLimitedGateway<G> {
    #[must_use]
    pub const fn new(inner: G, registry: Arc<ProviderBudgetRegistry>) -> Self {
        Self { inner, registry }
    }

    async fn acquire(
        &self,
        provider_kind: &str,
        base_url_override: Option<&str>,
    ) -> Result<crate::integrations::provider_budget::ProviderBudgetGuard> {
        let identity = provider_identity(provider_kind, base_url_override);
        self.registry.acquire(&identity, current_lane()).await.map_err(Into::into)
    }
}

#[async_trait]
impl<G: LlmGateway> LlmGateway for ConcurrencyLimitedGateway<G> {
    async fn generate(&self, request: ChatRequest) -> Result<ChatResponse> {
        let _permit =
            self.acquire(&request.provider_kind, request.base_url_override.as_deref()).await?;
        self.inner.generate(request).await
    }

    async fn generate_stream(
        &self,
        request: ChatRequest,
        on_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ChatResponse> {
        let _permit =
            self.acquire(&request.provider_kind, request.base_url_override.as_deref()).await?;
        self.inner.generate_stream(request, on_delta).await
    }

    async fn generate_with_tools(&self, request: ToolUseRequest) -> Result<ToolUseResponse> {
        let _permit =
            self.acquire(&request.provider_kind, request.base_url_override.as_deref()).await?;
        self.inner.generate_with_tools(request).await
    }

    async fn generate_with_tools_stream(
        &self,
        request: ToolUseRequest,
        on_text_delta: &mut (dyn FnMut(String) + Send),
    ) -> Result<ToolUseResponse> {
        let _permit =
            self.acquire(&request.provider_kind, request.base_url_override.as_deref()).await?;
        self.inner.generate_with_tools_stream(request, on_text_delta).await
    }

    async fn embed(&self, request: EmbeddingRequest) -> Result<EmbeddingResponse> {
        let _permit =
            self.acquire(&request.provider_kind, request.base_url_override.as_deref()).await?;
        self.inner.embed(request).await
    }

    async fn embed_many(&self, request: EmbeddingBatchRequest) -> Result<EmbeddingBatchResponse> {
        let _permit =
            self.acquire(&request.provider_kind, request.base_url_override.as_deref()).await?;
        self.inner.embed_many(request).await
    }

    async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse> {
        let _permit =
            self.acquire(&request.provider_kind, request.base_url_override.as_deref()).await?;
        self.inner.vision_extract(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use crate::integrations::provider_budget::{
        ProviderBudgetConfig, ProviderBudgetRegistry, ProviderBudgetRegistryOptions, ProviderLane,
        with_lane,
    };

    /// Inner gateway fake that records peak concurrency for `embed_many` and
    /// sleeps so overlapping calls are observable.
    struct CountingGateway {
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl LlmGateway for CountingGateway {
        async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
            Err(anyhow::anyhow!("generate is unused in these tests"))
        }

        async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
            Err(anyhow::anyhow!("embed is unused in these tests"))
        }

        async fn embed_many(
            &self,
            mut request: EmbeddingBatchRequest,
        ) -> Result<EmbeddingBatchResponse> {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(EmbeddingBatchResponse {
                provider_kind: std::mem::take(&mut request.provider_kind),
                model_name: std::mem::take(&mut request.model_name),
                dimensions: 0,
                embeddings: Vec::new(),
                usage_json: serde_json::json!({}),
            })
        }

        async fn vision_extract(&self, _request: VisionRequest) -> Result<VisionResponse> {
            Err(anyhow::anyhow!("vision_extract is unused in these tests"))
        }
    }

    fn embedding_batch(base_url: &str) -> EmbeddingBatchRequest {
        EmbeddingBatchRequest {
            provider_kind: "alpha".to_string(),
            model_name: "model".to_string(),
            inputs: vec!["x".to_string()],
            api_key_override: None,
            base_url_override: Some(base_url.to_string()),
            extra_parameters_json: serde_json::json!({}),
        }
    }

    #[test]
    fn identity_trims_trailing_slash_so_callsites_match() {
        let a = provider_identity("alpha", Some("https://endpoint.example/v1/"));
        let b = provider_identity("alpha", Some("https://endpoint.example/v1"));
        assert_eq!(a, b);
    }

    #[tokio::test]
    async fn decorator_caps_concurrency_through_the_registry() {
        const CAP: usize = 2;
        const TASKS: usize = 12;
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(
            ProviderBudgetRegistry::uniform(
                ProviderBudgetConfig { max_outbound: CAP, query_reserved: 0 },
                ProviderBudgetRegistryOptions::default(),
            )
            .unwrap(),
        );
        let gateway = Arc::new(ConcurrencyLimitedGateway::new(
            CountingGateway { in_flight: Arc::clone(&in_flight), peak: Arc::clone(&peak) },
            registry,
        ));

        let mut handles = Vec::new();
        for _ in 0..TASKS {
            let gateway = Arc::clone(&gateway);
            handles.push(tokio::spawn(async move {
                with_lane(ProviderLane::Ingest, async move {
                    gateway.embed_many(embedding_batch("https://endpoint.example/v1")).await
                })
                .await
                .unwrap();
            }));
        }
        for handle in handles {
            handle.await.unwrap();
        }

        assert!(
            peak.load(Ordering::SeqCst) <= CAP,
            "decorator must cap embed_many concurrency at {CAP}, saw {}",
            peak.load(Ordering::SeqCst),
        );
    }

    #[tokio::test]
    async fn gateways_use_their_injected_registry_without_global_cross_talk() {
        let in_flight = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let unlimited = Arc::new(
            ProviderBudgetRegistry::uniform(
                ProviderBudgetConfig::unlimited(),
                ProviderBudgetRegistryOptions::default(),
            )
            .unwrap(),
        );
        let gateway =
            ConcurrencyLimitedGateway::new(CountingGateway { in_flight, peak }, unlimited);

        gateway.embed_many(embedding_batch("https://endpoint.example/v1")).await.unwrap();
    }
}
