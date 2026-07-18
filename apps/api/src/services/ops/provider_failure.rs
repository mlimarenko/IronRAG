use reqwest::StatusCode;

use crate::domains::runtime_ingestion::{
    RuntimeProviderFailureClass, RuntimeProviderFailureDetail,
};
use crate::{integrations::retry::ProviderCallError, shared::outbound_http::PublicHttpUrlError};

#[derive(Debug, Clone)]
pub struct ProviderFailureClassificationService {
    request_size_soft_limit_bytes: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ProviderFailureObservation {
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<usize>,
    pub chunk_count: Option<usize>,
    pub elapsed_ms: Option<i64>,
    pub retry_decision: Option<String>,
    pub usage_visible: bool,
}

impl Default for ProviderFailureClassificationService {
    fn default() -> Self {
        Self::new(256 * 1024)
    }
}

impl ProviderFailureClassificationService {
    #[must_use]
    pub fn new(request_size_soft_limit_bytes: usize) -> Self {
        Self { request_size_soft_limit_bytes: request_size_soft_limit_bytes.max(1024) }
    }

    #[must_use]
    pub const fn request_size_soft_limit_bytes(&self) -> usize {
        self.request_size_soft_limit_bytes
    }

    #[must_use]
    pub fn classify_error(&self, error: &anyhow::Error) -> Option<RuntimeProviderFailureClass> {
        error
            .downcast_ref::<ProviderCallError>()
            .and_then(|error| self.classify_provider_call_error(error))
    }

    #[must_use]
    pub fn classify_provider_call_error(
        &self,
        error: &ProviderCallError,
    ) -> Option<RuntimeProviderFailureClass> {
        match error {
            ProviderCallError::Transport { source, .. } => Some(classify_reqwest_failure(source)),
            ProviderCallError::ResponseBody { source, .. }
            | ProviderCallError::ResponseJson { source, .. } => {
                Some(classify_reqwest_failure(source))
            }
            ProviderCallError::Json { .. } => {
                Some(RuntimeProviderFailureClass::UpstreamProtocolFailure)
            }
            ProviderCallError::ResponsePolicy { source, .. } => {
                Some(classify_response_policy_failure(source))
            }
            ProviderCallError::HttpStatus { status, .. } if !status.is_success() => {
                Some(classify_http_failure(*status))
            }
            ProviderCallError::HttpStatus { .. } => None,
            ProviderCallError::Protocol { .. } => {
                Some(RuntimeProviderFailureClass::InternalRequestInvalid)
            }
        }
    }

    #[must_use]
    pub fn upstream_status(&self, error: &anyhow::Error) -> Option<StatusCode> {
        error.downcast_ref::<ProviderCallError>().and_then(provider_call_status)
    }

    #[must_use]
    pub fn is_transient_retryable_error(&self, error: &anyhow::Error) -> bool {
        error.downcast_ref::<ProviderCallError>().is_some_and(is_retryable_provider_call_error)
    }

    #[must_use]
    pub fn classify_failure(
        &self,
        error: &anyhow::Error,
        explicit_failure_class: Option<RuntimeProviderFailureClass>,
        observation: ProviderFailureObservation,
    ) -> Option<RuntimeProviderFailureDetail> {
        let failure_class = if observation
            .request_size_bytes
            .is_some_and(|size| size > self.request_size_soft_limit_bytes)
        {
            RuntimeProviderFailureClass::InternalRequestInvalid
        } else if let Some(explicit_failure_class) = explicit_failure_class {
            explicit_failure_class
        } else {
            self.classify_error(error)?
        };
        Some(self.summarize_with_status(failure_class, observation, self.upstream_status(error)))
    }

    #[must_use]
    pub fn summarize(
        &self,
        failure_class: RuntimeProviderFailureClass,
        observation: ProviderFailureObservation,
    ) -> RuntimeProviderFailureDetail {
        self.summarize_with_status(failure_class, observation, None)
    }

    fn summarize_with_status(
        &self,
        failure_class: RuntimeProviderFailureClass,
        observation: ProviderFailureObservation,
        upstream_status: Option<StatusCode>,
    ) -> RuntimeProviderFailureDetail {
        RuntimeProviderFailureDetail {
            failure_class,
            provider_kind: observation.provider_kind,
            model_name: observation.model_name,
            request_shape_key: observation.request_shape_key,
            request_size_bytes: observation.request_size_bytes,
            chunk_count: observation.chunk_count,
            upstream_status: upstream_status.map(|status| status.as_u16().to_string()),
            elapsed_ms: observation.elapsed_ms,
            retry_decision: observation.retry_decision,
            usage_visible: observation.usage_visible,
        }
    }
}

fn classify_reqwest_failure(error: &reqwest::Error) -> RuntimeProviderFailureClass {
    if error.is_timeout() || error.status().is_some_and(is_timeout_status) {
        RuntimeProviderFailureClass::UpstreamTimeout
    } else if let Some(status) = error.status().filter(|status| !status.is_success()) {
        classify_http_failure(status)
    } else {
        RuntimeProviderFailureClass::UpstreamProtocolFailure
    }
}

const fn classify_response_policy_failure(
    error: &PublicHttpUrlError,
) -> RuntimeProviderFailureClass {
    match error {
        PublicHttpUrlError::ResolveTimedOut => RuntimeProviderFailureClass::UpstreamTimeout,
        PublicHttpUrlError::ResolveFailed
        | PublicHttpUrlError::NoAddresses
        | PublicHttpUrlError::RequestFailed(_)
        | PublicHttpUrlError::BodyReadFailed(_) => {
            RuntimeProviderFailureClass::UpstreamProtocolFailure
        }
        _ => RuntimeProviderFailureClass::InternalRequestInvalid,
    }
}

fn classify_http_failure(status: StatusCode) -> RuntimeProviderFailureClass {
    if is_timeout_status(status) {
        RuntimeProviderFailureClass::UpstreamTimeout
    } else {
        RuntimeProviderFailureClass::UpstreamRejection
    }
}

fn provider_call_status(error: &ProviderCallError) -> Option<StatusCode> {
    match error {
        ProviderCallError::Transport { source, .. }
        | ProviderCallError::ResponseBody { source, .. }
        | ProviderCallError::ResponseJson { source, .. } => source.status(),
        ProviderCallError::HttpStatus { status, .. } => Some(*status),
        ProviderCallError::ResponsePolicy { .. }
        | ProviderCallError::Json { .. }
        | ProviderCallError::Protocol { .. } => None,
    }
}

fn is_retryable_provider_call_error(error: &ProviderCallError) -> bool {
    match error {
        ProviderCallError::Transport { source, .. }
        | ProviderCallError::ResponseBody { source, .. } => {
            source.is_timeout()
                || source.is_connect()
                || source.is_body()
                || source.status().is_some_and(is_retryable_http_status)
        }
        ProviderCallError::ResponsePolicy { source, .. } => {
            source.is_retryable_resolution_failure()
                || matches!(
                    source,
                    PublicHttpUrlError::RequestFailed(_) | PublicHttpUrlError::BodyReadFailed(_)
                )
        }
        ProviderCallError::ResponseJson { source, .. } => {
            source.status().is_none_or(is_retryable_http_status)
        }
        ProviderCallError::Json { .. } => true,
        ProviderCallError::HttpStatus { status, .. } => is_retryable_http_status(*status),
        ProviderCallError::Protocol { .. } => false,
    }
}

fn is_timeout_status(status: StatusCode) -> bool {
    matches!(status, StatusCode::REQUEST_TIMEOUT | StatusCode::GATEWAY_TIMEOUT)
}

fn is_retryable_http_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::CONFLICT
            | StatusCode::TOO_EARLY
            | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

#[cfg(test)]
mod tests {
    use reqwest::{StatusCode, header::HeaderMap};

    use super::*;

    fn observation(request_size_bytes: usize) -> ProviderFailureObservation {
        ProviderFailureObservation {
            provider_kind: Some("provider-alpha".to_string()),
            model_name: Some("model-beta".to_string()),
            request_shape_key: Some("shape-v1".to_string()),
            request_size_bytes: Some(request_size_bytes),
            chunk_count: Some(1),
            elapsed_ms: Some(250),
            retry_decision: Some("terminal".to_string()),
            usage_visible: false,
        }
    }

    fn http_error(status: StatusCode, detail: &str) -> anyhow::Error {
        anyhow::Error::new(ProviderCallError::http_status(
            "provider-alpha",
            status,
            &HeaderMap::new(),
            detail,
        ))
        .context("wrapped provider call")
    }

    #[test]
    fn unknown_text_fails_closed_without_retry() {
        let service = ProviderFailureClassificationService::default();
        let error = anyhow::anyhow!(
            "provider rejected request status=503 after timeout with invalid model output"
        );

        assert_eq!(service.classify_error(&error), None);
        assert!(!service.is_transient_retryable_error(&error));
        assert!(service.classify_failure(&error, None, observation(512)).is_none());
    }

    #[test]
    fn wrapped_http_status_controls_classification_not_detail_text() {
        let service = ProviderFailureClassificationService::default();
        let error = http_error(
            StatusCode::TOO_MANY_REQUESTS,
            "timeout invalid model output internal request malformed",
        );
        let detail =
            service.classify_failure(&error, None, observation(512)).expect("typed failure");

        assert_eq!(detail.failure_class, RuntimeProviderFailureClass::UpstreamRejection);
        assert_eq!(detail.upstream_status.as_deref(), Some("429"));
        assert!(service.is_transient_retryable_error(&error));
    }

    #[test]
    fn typed_timeout_status_is_distinct_from_other_rejections() {
        let service = ProviderFailureClassificationService::default();
        let timeout = http_error(StatusCode::GATEWAY_TIMEOUT, "opaque");
        let rejection = http_error(StatusCode::BAD_REQUEST, "timeout");

        assert_eq!(
            service.classify_error(&timeout),
            Some(RuntimeProviderFailureClass::UpstreamTimeout)
        );
        assert_eq!(
            service.classify_error(&rejection),
            Some(RuntimeProviderFailureClass::UpstreamRejection)
        );
        assert!(service.is_transient_retryable_error(&timeout));
        assert!(!service.is_transient_retryable_error(&rejection));
    }

    #[test]
    fn protocol_message_cannot_forge_status_or_retryability() {
        let service = ProviderFailureClassificationService::default();
        let error = anyhow::Error::new(ProviderCallError::protocol(
            "status=503 timeout connection reset upstream rejection",
        ));

        assert_eq!(
            service.classify_error(&error),
            Some(RuntimeProviderFailureClass::InternalRequestInvalid)
        );
        assert_eq!(service.upstream_status(&error), None);
        assert!(!service.is_transient_retryable_error(&error));
    }

    #[test]
    fn typed_response_json_failure_is_retryable_protocol_failure() {
        let service = ProviderFailureClassificationService::default();
        let source = serde_json::Error::io(std::io::Error::other("opaque response"));
        let error = anyhow::Error::new(ProviderCallError::json("opaque", source))
            .context("graph provider boundary");

        assert_eq!(
            service.classify_error(&error),
            Some(RuntimeProviderFailureClass::UpstreamProtocolFailure)
        );
        assert!(service.is_transient_retryable_error(&error));
    }

    #[test]
    fn structural_request_limit_and_explicit_class_are_supported() {
        let service = ProviderFailureClassificationService::new(1_024);
        let unknown = anyhow::anyhow!("opaque");
        let oversized = service
            .classify_failure(&unknown, None, observation(2_048))
            .expect("request limit class");
        let explicit = service
            .classify_failure(
                &unknown,
                Some(RuntimeProviderFailureClass::InvalidModelOutput),
                observation(512),
            )
            .expect("explicit class");

        assert_eq!(oversized.failure_class, RuntimeProviderFailureClass::InternalRequestInvalid);
        assert_eq!(explicit.failure_class, RuntimeProviderFailureClass::InvalidModelOutput);
    }

    #[test]
    fn explicit_summary_preserves_observability_fields() {
        let service = ProviderFailureClassificationService::default();
        let recovered =
            service.summarize(RuntimeProviderFailureClass::RecoveredAfterRetry, observation(4_000));

        assert_eq!(recovered.failure_class, RuntimeProviderFailureClass::RecoveredAfterRetry);
        assert_eq!(recovered.provider_kind.as_deref(), Some("provider-alpha"));
        assert_eq!(recovered.request_size_bytes, Some(4_000));
    }
}
