use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeIngestionStatus {
    Queued,
    Processing,
    Ready,
    ReadyNoGraph,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeProviderFailureClass {
    InternalRequestInvalid,
    UpstreamProtocolFailure,
    UpstreamTimeout,
    UpstreamRejection,
    InvalidModelOutput,
    RecoveredAfterRetry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeProviderFailureDetail {
    pub failure_class: RuntimeProviderFailureClass,
    pub provider_kind: Option<String>,
    pub model_name: Option<String>,
    pub request_shape_key: Option<String>,
    pub request_size_bytes: Option<usize>,
    pub chunk_count: Option<usize>,
    pub upstream_status: Option<String>,
    pub elapsed_ms: Option<i64>,
    pub retry_decision: Option<String>,
    pub usage_visible: bool,
}
