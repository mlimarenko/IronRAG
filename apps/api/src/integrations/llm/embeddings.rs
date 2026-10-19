use anyhow::{Context, Result, anyhow};
use reqwest::{Method, Url};

use crate::{
    domains::provider_profiles::REASONING_MODE_FIELD,
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
    pub(super) transport_error: &'a str,
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

impl UnifiedGateway {
    pub(super) async fn call_embeddings(
        &self,
        call: EmbeddingsCall<'_>,
    ) -> Result<serde_json::Value> {
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
                let response = Self::send_provider_request(
                    request_builder.json(&Self::embedding_request_body(
                        call.model_name,
                        call.input.clone(),
                        call.extra_parameters_json,
                    )),
                    call.endpoint_url,
                    call.provider_kind,
                    call.operation,
                )
                .await
                .map_err(|source| {
                    ProviderCallError::transport(
                        format!("{}: provider={}", call.transport_error, call.provider_kind),
                        source,
                    )
                })?;

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
