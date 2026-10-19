use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeIngestionStatus {
    Queued,
    Processing,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDocumentActivityStatus {
    Queued,
    Active,
    Blocked,
    Retrying,
    Stalled,
    Ready,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProviderFailureClass {
    InternalRequestInvalid,
    UpstreamProtocolFailure,
    UpstreamTimeout,
    UpstreamRejection,
    InvalidModelOutput,
    RecoveredAfterRetry,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RuntimeProviderFailureDetail {
    pub failure_class: RuntimeProviderFailureClass,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<usize>,
    pub chunk_count: Option<usize>,
    pub upstream_status: Option<String>,
    /// The provider's advertised `Retry-After` for a rate-limit rejection, in
    /// seconds. Carried so a worker that only sees this sanitized snapshot can
    /// still defer the job for the provider's own cooldown.
    pub retry_after_seconds: Option<u64>,
    pub elapsed_ms: Option<i64>,
    pub retry_decision: Option<String>,
    pub usage_visible: bool,
}
