use crate::domains::runtime_ingestion::{
    RuntimeProviderFailureClass, RuntimeProviderFailureDetail,
};

#[derive(Debug, Clone)]
pub struct ProviderFailureClassificationService {
    request_size_soft_limit_bytes: usize,
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
    pub fn request_size_soft_limit_bytes(&self) -> usize {
        self.request_size_soft_limit_bytes
    }

    #[must_use]
    pub fn classify_error_message(&self, message: &str) -> Option<RuntimeProviderFailureClass> {
        let normalized = message.to_ascii_lowercase();
        if normalized.contains("failed to serialize provider request body")
            || normalized.contains("serialized provider request body was not valid json")
        {
            Some(RuntimeProviderFailureClass::InternalRequestInvalid)
        } else if normalized.contains("could not parse the json body of your request")
            || normalized.contains("json body of your request")
            || normalized.contains("expects a json payload")
            || (normalized.contains("invalid_request_error")
                && normalized.contains("json payload")
                && normalized.contains("status=400"))
            || normalized.contains("upstream protocol failure")
        {
            Some(RuntimeProviderFailureClass::UpstreamProtocolFailure)
        } else if normalized.contains("invalid_request")
            || normalized.contains("invalid request")
            || normalized.contains("malformed")
            || normalized.contains("context_length")
            || normalized.contains("prompt is too long")
            || normalized.contains("request payload")
        {
            Some(RuntimeProviderFailureClass::InternalRequestInvalid)
        } else if normalized.contains("timeout")
            || normalized.contains("timed out")
            || normalized.contains("deadline exceeded")
            || normalized.contains("connection closed before message completed")
            || normalized.contains("connection reset")
            || normalized.contains("error sending request")
            || normalized.contains("sendrequest")
            || normalized.contains("status=504")
            || normalized.contains("status=408")
        {
            Some(RuntimeProviderFailureClass::UpstreamTimeout)
        } else if normalized.contains("rejected")
            || normalized.contains("refused")
            || normalized.contains("rate limit")
            || normalized.contains("status=429")
            || normalized.contains("status=500")
            || normalized.contains("status=502")
            || normalized.contains("status=503")
            || normalized.contains("status=520")
            || normalized.contains("status=521")
            || normalized.contains("status=522")
            || normalized.contains("status=523")
            || normalized.contains("status=524")
            || normalized.contains("status=529")
        {
            Some(RuntimeProviderFailureClass::UpstreamRejection)
        } else if normalized.contains("invalid json")
            || normalized.contains("schema")
            || normalized.contains("invalid model output")
        {
            Some(RuntimeProviderFailureClass::InvalidModelOutput)
        } else {
            None
        }
    }

    #[must_use]
    pub fn extract_upstream_status(&self, message: &str) -> Option<String> {
        let marker = "status=";
        let start = message.find(marker)?;
        let status = message[start + marker.len()..]
            .chars()
            .take_while(|char| char.is_ascii_alphanumeric())
            .collect::<String>();
        (!status.is_empty()).then_some(status)
    }

    #[must_use]
    pub fn is_retryable_upstream_status(&self, status: &str) -> bool {
        matches!(
            status,
            "408"
                | "409"
                | "425"
                | "429"
                | "500"
                | "502"
                | "503"
                | "504"
                | "520"
                | "521"
                | "522"
                | "523"
                | "524"
                | "529"
        )
    }

    #[must_use]
    pub fn is_transient_retryable_failure(&self, detail: &RuntimeProviderFailureDetail) -> bool {
        match detail.failure_class {
            RuntimeProviderFailureClass::UpstreamTimeout
            | RuntimeProviderFailureClass::UpstreamProtocolFailure => true,
            RuntimeProviderFailureClass::UpstreamRejection => detail
                .upstream_status
                .as_deref()
                .is_some_and(|status| self.is_retryable_upstream_status(status)),
            _ => false,
        }
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn classify_failure(
        &self,
        provider_kind: &str,
        model_name: &str,
        message: &str,
        request_shape_key: &str,
        request_size_bytes: usize,
        chunk_count: Option<usize>,
        elapsed_ms: Option<i64>,
        retry_decision: Option<String>,
        usage_visible: bool,
    ) -> RuntimeProviderFailureDetail {
        let failure_class = if request_size_bytes > self.request_size_soft_limit_bytes {
            RuntimeProviderFailureClass::InternalRequestInvalid
        } else {
            self.classify_error_message(message)
                .unwrap_or(RuntimeProviderFailureClass::UpstreamRejection)
        };
        self.summarize(
            failure_class,
            Some(provider_kind.to_string()),
            Some(model_name.to_string()),
            Some(request_shape_key.to_string()),
            Some(request_size_bytes),
            chunk_count,
            self.extract_upstream_status(message),
            elapsed_ms,
            retry_decision,
            usage_visible,
        )
    }

    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn summarize(
        &self,
        failure_class: RuntimeProviderFailureClass,
        provider_kind: Option<String>,
        model_name: Option<String>,
        request_shape_key: Option<String>,
        request_size_bytes: Option<usize>,
        chunk_count: Option<usize>,
        upstream_status: Option<String>,
        elapsed_ms: Option<i64>,
        retry_decision: Option<String>,
        usage_visible: bool,
    ) -> RuntimeProviderFailureDetail {
        RuntimeProviderFailureDetail {
            failure_class,
            provider_kind,
            model_name,
            request_shape_key,
            request_size_bytes,
            chunk_count,
            upstream_status,
            elapsed_ms,
            retry_decision,
            usage_visible,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_provider_failure_strings() {
        let service = ProviderFailureClassificationService::default();

        assert_eq!(
            service.classify_error_message("OpenAI invalid_request payload"),
            Some(RuntimeProviderFailureClass::InternalRequestInvalid)
        );
        assert_eq!(
            service.classify_error_message("request timed out upstream"),
            Some(RuntimeProviderFailureClass::UpstreamTimeout)
        );
        assert_eq!(
            service.classify_error_message(
                "graph extraction provider call failed: error sending request for url (https://api.openai.com/v1/chat/completions): client error (SendRequest): connection closed before message completed"
            ),
            Some(RuntimeProviderFailureClass::UpstreamTimeout)
        );
        assert_eq!(
            service.classify_error_message(
                "provider request failed: provider=openai status=400 body={\"error\":{\"message\":\"We could not parse the JSON body of your request. The OpenAI API expects a JSON payload.\"}}"
            ),
            Some(RuntimeProviderFailureClass::UpstreamProtocolFailure)
        );
        assert_eq!(
            service.classify_error_message(
                "provider request failed: provider=openai status=520 body={\"raw_body\":\"error code: 520\"}"
            ),
            Some(RuntimeProviderFailureClass::UpstreamRejection)
        );
    }

    #[test]
    fn classifies_failure_with_upstream_status_and_soft_limit_guard() {
        let service = ProviderFailureClassificationService::new(1_024);
        let detail = service.classify_failure(
            "openai",
            "gpt-5.4-mini",
            "provider request failed: provider=openai status=429 body={}",
            "graph_extract_v3:initial:segments_3:trimmed",
            2_048,
            Some(1),
            Some(1_500),
            Some("terminal_failure".to_string()),
            false,
        );

        assert_eq!(detail.failure_class, RuntimeProviderFailureClass::InternalRequestInvalid);
        assert_eq!(detail.upstream_status.as_deref(), Some("429"));
    }

    #[test]
    fn marks_transient_retryable_failures() {
        let service = ProviderFailureClassificationService::default();
        let timeout = service.summarize(
            RuntimeProviderFailureClass::UpstreamTimeout,
            Some("openai".to_string()),
            Some("gpt-5.4-mini".to_string()),
            Some("shape".to_string()),
            Some(512),
            Some(1),
            None,
            None,
            None,
            false,
        );
        let retryable_rejection = service.summarize(
            RuntimeProviderFailureClass::UpstreamRejection,
            Some("openai".to_string()),
            Some("gpt-5.4-mini".to_string()),
            Some("shape".to_string()),
            Some(512),
            Some(1),
            Some("520".to_string()),
            None,
            None,
            false,
        );
        let terminal_rejection = service.summarize(
            RuntimeProviderFailureClass::UpstreamRejection,
            Some("openai".to_string()),
            Some("gpt-5.4-mini".to_string()),
            Some("shape".to_string()),
            Some(512),
            Some(1),
            Some("401".to_string()),
            None,
            None,
            false,
        );

        assert!(service.is_transient_retryable_failure(&timeout));
        assert!(service.is_transient_retryable_failure(&retryable_rejection));
        assert!(!service.is_transient_retryable_failure(&terminal_rejection));
    }

    #[test]
    fn provider_failure_classes_remain_distinct() {
        let service = ProviderFailureClassificationService::default();

        let timeout = service.classify_failure(
            "openai",
            "gpt-5.4-mini",
            "provider request failed: provider=openai status=504 body={}",
            "graph_extract_v3:initial:segments_3:trimmed",
            32_000,
            Some(1),
            Some(30_000),
            Some("retrying_provider_call".to_string()),
            false,
        );
        let rejection = service.classify_failure(
            "openai",
            "gpt-5.4-mini",
            "provider request failed: provider=openai status=429 body={}",
            "graph_extract_v3:initial:segments_1:full",
            4_000,
            Some(1),
            Some(1_000),
            Some("terminal_failure".to_string()),
            false,
        );
        let invalid_output = service.classify_failure(
            "openai",
            "gpt-5.4-mini",
            "invalid model output: schema mismatch",
            "graph_extract_v3:provider_retry:segments_1:full",
            4_000,
            Some(1),
            Some(800),
            Some("terminal_failure".to_string()),
            true,
        );
        let recovered = service.summarize(
            RuntimeProviderFailureClass::RecoveredAfterRetry,
            Some("openai".to_string()),
            Some("gpt-5.4-mini".to_string()),
            Some("graph_extract_v3:provider_retry:segments_1:full".to_string()),
            Some(4_000),
            Some(1),
            None,
            Some(900),
            Some("recovered_after_retry".to_string()),
            true,
        );

        assert_eq!(timeout.failure_class, RuntimeProviderFailureClass::UpstreamTimeout);
        assert_eq!(timeout.upstream_status.as_deref(), Some("504"));
        assert_eq!(rejection.failure_class, RuntimeProviderFailureClass::UpstreamRejection);
        assert_eq!(rejection.upstream_status.as_deref(), Some("429"));
        assert_eq!(invalid_output.failure_class, RuntimeProviderFailureClass::InvalidModelOutput);
        assert_eq!(recovered.failure_class, RuntimeProviderFailureClass::RecoveredAfterRetry);
    }
}
