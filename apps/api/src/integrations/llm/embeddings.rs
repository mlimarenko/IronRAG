use anyhow::{Context, Result, anyhow};
use reqwest::{Method, Url};
use tracing::Instrument as _;

use crate::{
    domains::provider_profiles::{EmbeddingInputBounds, REASONING_MODE_FIELD},
    integrations::retry::{ProviderCallError, provider_status_error_from_read, with_retry},
    shared::provider_http::PreparedProviderTarget,
};

use super::{
    UnifiedGateway,
    provider::{ResolvedProviderRuntime, apply_provider_auth},
    response::{parse_provider_json_body, read_provider_response_body},
};

pub(super) struct EmbeddingsCall<'a> {
    pub(super) provider_kind: &'a str,
    pub(super) model_name: &'a str,
    pub(super) input: serde_json::Value,
    pub(super) extra_parameters_json: &'a serde_json::Value,
    pub(super) resolved: &'a ResolvedProviderRuntime,
    pub(super) endpoint_url: &'a Url,
    pub(super) target: &'a PreparedProviderTarget,
    pub(super) operation: &'a str,
}

pub(super) fn validate_embedding_inputs(
    provider_kind: &str,
    model_name: &str,
    input: &serde_json::Value,
) -> std::result::Result<(), ProviderCallError> {
    if let Some(bounds) = EmbeddingInputBounds::for_resolved_model(provider_kind, model_name)
        && !bounds.accepts_json(input)
    {
        return Err(ProviderCallError::local_request_rejected());
    }
    Ok(())
}

/// Parameters that are sent to the embedding endpoint in addition to the
/// protocol-owned `model` and `input` fields. Provider-control metadata is
/// consumed locally and therefore cannot define the remote vector space.
pub(crate) fn embedding_request_parameters(
    extra_parameters_json: &serde_json::Value,
) -> serde_json::Value {
    let Some(extra) = extra_parameters_json.as_object() else {
        return serde_json::json!({});
    };
    serde_json::Value::Object(
        extra
            .iter()
            .filter(|(key, _)| {
                !matches!(key.as_str(), "model" | "input")
                    && !key.starts_with("_provider")
                    && key.as_str() != REASONING_MODE_FIELD
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    )
}

/// The span for one logical embeddings call: request preparation, every retry
/// attempt and the response parse.
///
/// Embeddings run in batches during ingest and once per query, so the span is
/// where a trace says which model produced the vectors and how many elements
/// came back. The per-attempt `provider.http.request` span stays its child.
fn embeddings_span(provider_kind: &str, model_name: &str) -> tracing::Span {
    let span = tracing::info_span!(
        "llm.embeddings",
        otel.kind = "client",
        "provider.kind" = provider_kind,
        "provider.model" = model_name,
        "provider.call.id" = tracing::field::Empty,
        "llm.embedding.elements" = tracing::field::Empty,
    );
    if let Some(provider_call_id) = crate::observability::current_provider_call_id() {
        span.record("provider.call.id", provider_call_id);
    }
    span
}

impl UnifiedGateway {
    pub(super) async fn call_embeddings(
        &self,
        call: EmbeddingsCall<'_>,
    ) -> Result<serde_json::Value> {
        let span = embeddings_span(call.provider_kind, call.model_name);
        let result = self.call_embeddings_inner(call).instrument(span.clone()).await;
        if let Ok(body) = &result
            && let Some(elements) = body.get("data").and_then(serde_json::Value::as_array)
        {
            span.record(
                "llm.embedding.elements",
                i64::try_from(elements.len()).unwrap_or(i64::MAX),
            );
        }
        result
    }

    async fn call_embeddings_inner(&self, call: EmbeddingsCall<'_>) -> Result<serde_json::Value> {
        validate_embedding_inputs(call.provider_kind, call.model_name, &call.input)?;
        Ok(with_retry(
            || async {
                let _provider_guard = self
                    .begin_provider_attempt(call.provider_kind, &call.resolved.base_url)
                    .await?;
                let request_builder = Self::prepared_request(
                    call.target,
                    Method::POST,
                    call.endpoint_url,
                    call.provider_kind,
                )?;
                let request_builder = apply_provider_auth(
                    request_builder,
                    call.resolved.runtime.auth_scheme,
                    call.resolved.api_key.as_deref(),
                );
                let dispatch = call.resolved.dispatch.as_ref().ok_or_else(|| {
                    ProviderCallError::protocol("provider dispatch authority is missing")
                })?;
                let response = self
                    .send_provider_request(
                        request_builder.json(&Self::embedding_request_body(
                            call.model_name,
                            call.input.clone(),
                            call.extra_parameters_json,
                        )),
                        dispatch,
                        call.operation,
                    )
                    .await?;

                let (status, headers, body_bytes) =
                    read_provider_response_body(response, call.provider_kind, call.operation)
                        .await?;
                if !status.is_success() {
                    return Err(provider_status_error_from_read(
                        call.provider_kind,
                        status,
                        &headers,
                        body_bytes,
                    ));
                }
                let body_bytes = body_bytes.unwrap_or_default();
                parse_provider_json_body(&body_bytes, call.provider_kind, call.operation)
            },
            self.retry_policy,
        )
        .await?)
    }

    pub(super) fn parse_embedding_vector(value: &serde_json::Value) -> Result<Vec<f32>> {
        let values = value.as_array().context("embedding must be a JSON array")?;
        values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = value
                    .as_f64()
                    .ok_or_else(|| anyhow!("embedding element {index} must be a number"))?;
                if !value.is_finite() || value < f64::from(f32::MIN) || value > f64::from(f32::MAX)
                {
                    return Err(anyhow!("embedding element {index} is not a finite f32 value"));
                }
                Ok(value as f32)
            })
            .collect()
    }

    pub(super) fn embedding_request_body(
        model_name: &str,
        input: serde_json::Value,
        extra_parameters_json: &serde_json::Value,
    ) -> serde_json::Value {
        let mut body = serde_json::Map::new();
        body.insert("model".to_string(), serde_json::Value::String(model_name.to_string()));
        body.insert("input".to_string(), input);

        if let Some(extra) = embedding_request_parameters(extra_parameters_json).as_object() {
            body.extend(extra.clone());
        }

        serde_json::Value::Object(body)
    }
}

#[cfg(test)]
mod input_bound_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::{
        app::config::Settings,
        integrations::llm::{EmbeddingBatchRequest, EmbeddingRequest, LlmGateway},
        shared::provider_http::{
            DispatchRequestEvidence, ProviderDispatchAdmission, ProviderDispatchContext,
            ProviderDispatchPermit, ProviderDispatchRefusal,
        },
    };

    #[derive(Default)]
    struct RefuseAndCountAdmission(AtomicUsize);

    #[async_trait::async_trait]
    impl ProviderDispatchAdmission for RefuseAndCountAdmission {
        async fn admit(
            &self,
            _context: &ProviderDispatchContext,
            _evidence: &DispatchRequestEvidence<'_>,
        ) -> std::result::Result<ProviderDispatchPermit, ProviderDispatchRefusal> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(ProviderDispatchRefusal::Admission)
        }
    }

    #[tokio::test]
    async fn oversized_embedding_inputs_never_reach_resolution_or_admission() {
        let admission = Arc::new(RefuseAndCountAdmission::default());
        let settings = Settings::from_env().expect("valid isolated gateway settings");
        let gateway = UnifiedGateway::from_settings(&settings, admission.clone())
            .expect("valid isolated gateway settings");
        let single = gateway
            .embed(EmbeddingRequest {
                provider_kind: "openai".into(),
                model_name: "text-embedding-3-large".into(),
                input: "🦀".repeat(2049),
                api_key_override: None,
                // Even a missing preflight must not allow this fixture to do DNS or HTTP.
                base_url_override: Some("not a URL".into()),
                extra_parameters_json: serde_json::json!({}),
                dispatch: None,
            })
            .await
            .expect_err("oversized input must be rejected before provider resolution");
        assert!(matches!(
            single.downcast_ref::<ProviderCallError>(),
            Some(ProviderCallError::LocalRequestRejected)
        ));
        let batch = gateway
            .embed_many(EmbeddingBatchRequest {
                provider_kind: "openai".into(),
                model_name: "text-embedding-3-large".into(),
                inputs: vec!["x".repeat(8192); 37],
                api_key_override: None,
                base_url_override: Some("not a URL".into()),
                extra_parameters_json: serde_json::json!({}),
                dispatch: None,
            })
            .await
            .expect_err("oversized request must be rejected before provider resolution");
        assert!(matches!(
            batch.downcast_ref::<ProviderCallError>(),
            Some(ProviderCallError::LocalRequestRejected)
        ));
        assert_eq!(admission.0.load(Ordering::SeqCst), 0);
    }
}
