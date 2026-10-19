use super::*;
use crate::shared::provider_http::ProviderHttpError;
use rand::RngExt as _;

#[derive(Debug, Error)]
pub enum ProviderCallError {
    #[error("provider request admission failed: {source}")]
    Admission {
        #[source]
        source: ProviderBudgetError,
    },
    #[error("{context}: {source}")]
    Transport {
        context: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{context}: {source}")]
    ResponseBody {
        context: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{context}: {source}")]
    ResponsePolicy {
        context: String,
        #[source]
        source: PublicHttpUrlError,
    },
    #[error("{context}: {source}")]
    ResponseJson {
        context: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("provider request failed: provider={provider_kind} status={status} detail={detail}")]
    HttpStatus {
        provider_kind: String,
        status: StatusCode,
        retry_after: Option<Duration>,
        detail: String,
        /// The upstream error body carried the formal `insufficient_quota`
        /// protocol code. Extracted structurally BEFORE the body is redacted,
        /// because quota exhaustion must never be retried: unlike a rate
        /// limit, it does not heal on a retry timescale, and hammering the
        /// provider only burns attempt budgets.
        quota_exhausted: bool,
        /// The upstream error body was structurally empty at the response
        /// boundary, measured on the raw bytes BEFORE redaction. It is never
        /// inferred from the redacted `detail` string: an empty body and a
        /// redacted non-empty body both render opaque operator text, so only
        /// this flag may drive the bounded empty-404 retry.
        empty_body: bool,
    },
    #[error("{message}")]
    Protocol { message: String },
    #[error("provider call exceeded its execution deadline: {context}")]
    Timeout { context: String },
}

impl ProviderCallError {
    #[must_use]
    pub const fn admission(source: ProviderBudgetError) -> Self {
        Self::Admission { source }
    }

    #[must_use]
    pub const fn admission_error(&self) -> Option<&ProviderBudgetError> {
        match self {
            Self::Admission { source } => Some(source),
            _ => None,
        }
    }

    #[must_use]
    pub fn transport(context: impl Into<String>, source: reqwest::Error) -> Self {
        Self::Transport { context: context.into(), source }
    }

    #[must_use]
    pub fn response_body(context: impl Into<String>, source: reqwest::Error) -> Self {
        Self::ResponseBody { context: context.into(), source }
    }

    #[must_use]
    pub fn response_policy(context: impl Into<String>, source: PublicHttpUrlError) -> Self {
        Self::ResponsePolicy { context: context.into(), source }
    }

    #[must_use]
    pub fn response_json(context: impl Into<String>, source: reqwest::Error) -> Self {
        Self::ResponseJson { context: context.into(), source }
    }

    #[must_use]
    pub fn json(context: impl Into<String>, source: serde_json::Error) -> Self {
        Self::Json { context: context.into(), source }
    }

    #[must_use]
    pub fn http_status(
        provider_kind: impl Into<String>,
        status: StatusCode,
        headers: &HeaderMap,
        detail: impl Into<String>,
    ) -> Self {
        Self::HttpStatus {
            provider_kind: provider_kind.into(),
            status,
            retry_after: retry_after_from_headers(headers),
            detail: detail.into(),
            quota_exhausted: false,
            // This synthetic constructor carries no raw response body, so
            // emptiness is not proven and the bounded empty-404 retry cannot
            // fire; a real response goes through `provider_http_status_error`.
            empty_body: false,
        }
    }

    #[must_use]
    pub fn protocol(message: impl Into<String>) -> Self {
        Self::Protocol { message: message.into() }
    }

    /// The one mapping of a failed provider target preparation, shared by every
    /// provider call path. A DNS resolution failure stays an upstream-shaped
    /// [`Self::ResponsePolicy`] carrying its typed resolution source, so operator
    /// classification reports an upstream class and the outer whole-call
    /// recovery may retry it. The local [`with_retry`](super::with_retry) loop
    /// does not: `prepare` runs once before it, and its classifier aborts a
    /// resolution response policy. URL, address and origin policy rejections
    /// stay terminal [`Self::Protocol`] errors — they describe a caller-supplied
    /// target that no retry can fix.
    #[must_use]
    pub fn target_preparation(context: &str, error: &ProviderHttpError) -> Self {
        match error.resolution_failure() {
            Some(source) => Self::response_policy(context, source),
            None => Self::protocol(format!("{context}: {error}")),
        }
    }

    /// Canonical constructor for a whole-call execution-deadline timeout. The
    /// context is a caller-owned diagnostic label; it is never interpolated into
    /// [`Self::redacted_detail`], which stays a fixed `timeout` token.
    #[must_use]
    pub fn timeout(context: impl Into<String>) -> Self {
        Self::Timeout { context: context.into() }
    }

    /// `true` when the provider formally reported exhausted billing quota.
    #[must_use]
    pub const fn is_quota_exhausted(&self) -> bool {
        matches!(self, Self::HttpStatus { quota_exhausted: true, .. })
    }

    /// `true` only when the upstream error body was read successfully and was
    /// structurally empty. A body that could not be read (oversize, truncated
    /// or transport failure) is never reported as empty.
    #[must_use]
    pub const fn is_empty_body(&self) -> bool {
        matches!(self, Self::HttpStatus { empty_body: true, .. })
    }

    /// `true` when the provider rejected the call with HTTP 429 — a rate limit
    /// or an exhausted quota. Both are operational and self-healing: the same
    /// request succeeds again once the window resets or the quota is topped up.
    /// This must never be read as a content verdict nor exhaust a job's attempt
    /// budget; the owning worker defers the job instead.
    #[must_use]
    pub fn is_rate_limited(&self) -> bool {
        matches!(self, Self::HttpStatus { status, .. } if status.as_u16() == 429)
    }

    /// The provider's advertised `Retry-After`, when the response carried one.
    #[must_use]
    pub const fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::HttpStatus { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Value-free classification of this error for operator-facing redacted
    /// storage. Every token is derived from the enum shape alone — variant,
    /// status code, provider kind, transport flags — never from `context`,
    /// `detail`, or source error text, which may carry upstream response
    /// bodies.
    #[must_use]
    pub fn redacted_detail(&self) -> String {
        match self {
            Self::Admission { source } => source.redacted_detail().to_string(),
            Self::Transport { source, .. } => transport_redacted_detail("transport", source),
            Self::ResponseBody { source, .. } => transport_redacted_detail("response_body", source),
            Self::ResponsePolicy { .. } => "response_policy".to_string(),
            Self::ResponseJson { source, .. } => transport_redacted_detail("response_json", source),
            Self::Json { .. } => "json_decode".to_string(),
            Self::HttpStatus { provider_kind, status, quota_exhausted, .. } => {
                let mut detail = format!(
                    "provider={} http_status={}",
                    sanitized_provider_kind(provider_kind),
                    status.as_u16()
                );
                if *quota_exhausted {
                    detail.push_str(" quota_exhausted");
                }
                detail
            }
            Self::Protocol { .. } => "protocol".to_string(),
            Self::Timeout { .. } => "timeout".to_string(),
        }
    }
}

/// A provider rate-limit rejection (HTTP 429) that is operational and
/// self-healing, reconstructed at a boundary where the rich
/// [`ProviderCallError`] (with its unclonable `reqwest` sources and upstream
/// body) cannot travel. It is the typed signal carried across that boundary so
/// an ingest worker can defer a job for the provider's cooldown instead of
/// consuming its attempt budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRateLimit {
    pub provider_kind: String,
    pub retry_after: Option<Duration>,
    pub detail: String,
}

impl ProviderRateLimit {
    #[must_use]
    pub fn new(
        provider_kind: impl Into<String>,
        retry_after: Option<Duration>,
        detail: impl Into<String>,
    ) -> Self {
        Self { provider_kind: provider_kind.into(), retry_after, detail: detail.into() }
    }

    /// Reconstructs the operational signal from a whole-call
    /// [`ProviderCallError`], returning `None` for every non-429 error.
    #[must_use]
    pub fn from_error(error: &ProviderCallError) -> Option<Self> {
        match error {
            ProviderCallError::HttpStatus {
                provider_kind, status, retry_after, detail, ..
            } if status.as_u16() == 429 => Some(Self {
                provider_kind: provider_kind.clone(),
                retry_after: *retry_after,
                detail: detail.clone(),
            }),
            _ => None,
        }
    }
}

impl std::fmt::Display for ProviderRateLimit {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "provider rate limited: provider={} status=429 detail={}",
            sanitized_provider_kind(&self.provider_kind),
            self.detail
        )
    }
}

impl std::error::Error for ProviderRateLimit {}

fn transport_redacted_detail(base: &str, source: &reqwest::Error) -> String {
    if source.is_timeout() {
        format!("{base}_timeout")
    } else if source.is_connect() {
        format!("{base}_connect")
    } else {
        base.to_string()
    }
}

/// Provider kinds are an operator-configured vocabulary, but the value still
/// crosses into a redacted diagnostics column, so it is reduced to a bounded
/// identifier alphabet instead of being trusted verbatim.
fn sanitized_provider_kind(provider_kind: &str) -> String {
    const MAX_PROVIDER_KIND_CHARS: usize = 32;
    let sanitized: String = provider_kind
        .chars()
        .map(|ch| ch.to_ascii_lowercase())
        .filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '-'))
        .take(MAX_PROVIDER_KIND_CHARS)
        .collect();
    if sanitized.is_empty() { "unknown".to_string() } else { sanitized }
}

/// The OpenAI-compatible wire code for exhausted billing quota. A formal
/// protocol literal, not a natural-language marker: it is compared against
/// the structured `error.code` / `error.type` fields only.
const PROVIDER_QUOTA_EXHAUSTED_CODE: &str = "insufficient_quota";

#[must_use]
pub fn provider_http_status_error(
    provider_kind: &str,
    status: StatusCode,
    headers: &HeaderMap,
    body_text: &str,
) -> ProviderCallError {
    let body = serde_json::from_str::<serde_json::Value>(body_text).ok();
    let quota_exhausted = body.as_ref().is_some_and(provider_error_body_reports_quota_exhausted);
    // Structural, value-free emptiness of the RAW upstream body, captured
    // before any redaction. A whitespace-only body is empty; a JSON body
    // (even `{}`) is not. This is the only evidence the bounded empty-404
    // retry in `classify_provider_call_error` trusts.
    let empty_body = body_text.trim().is_empty();
    let detail = body.as_ref().map_or_else(|| body_text.to_string(), ToString::to_string);
    ProviderCallError::HttpStatus {
        provider_kind: provider_kind.to_string(),
        status,
        retry_after: retry_after_from_headers(headers),
        detail: if quota_exhausted {
            "upstream provider quota exhausted; response details were redacted".to_string()
        } else {
            sanitize_provider_error_detail(&detail)
        },
        quota_exhausted,
        empty_body,
    }
}

/// Builds the HTTP-status error for a non-success response whose error body
/// could NOT be read (oversize `Content-Length`, truncated stream or transport
/// failure). Emptiness is unknown, so `empty_body` stays false and the bounded
/// empty-404 retry never fires; the operator still gets the value-free redacted
/// detail without any raw upstream text or transport message.
#[must_use]
pub fn provider_http_status_error_unread(
    provider_kind: &str,
    status: StatusCode,
    headers: &HeaderMap,
) -> ProviderCallError {
    ProviderCallError::HttpStatus {
        provider_kind: provider_kind.to_string(),
        status,
        retry_after: retry_after_from_headers(headers),
        detail: "upstream provider request failed; response details were redacted".to_string(),
        quota_exhausted: false,
        empty_body: false,
    }
}

/// Builds the typed HTTP-status error from a non-success response body read.
/// The `Option` carries the read outcome: `Some(bytes)` is a successful read
/// (possibly empty, which keeps the structural emptiness signal) while `None`
/// is an unread/oversize/truncated body that is never classified as blank.
#[must_use]
pub fn provider_status_error_from_read(
    provider_kind: &str,
    status: StatusCode,
    headers: &HeaderMap,
    body_bytes: Option<Vec<u8>>,
) -> ProviderCallError {
    match body_bytes {
        Some(bytes) => provider_http_status_error(
            provider_kind,
            status,
            headers,
            &String::from_utf8_lossy(&bytes),
        ),
        None => provider_http_status_error_unread(provider_kind, status, headers),
    }
}

/// Reads the structured `error.code` / `error.type` fields of an
/// OpenAI-compatible error body. Free-text fields are deliberately ignored.
fn provider_error_body_reports_quota_exhausted(body: &serde_json::Value) -> bool {
    let error = &body["error"];
    [&error["code"], &error["type"]]
        .into_iter()
        .filter_map(serde_json::Value::as_str)
        .any(|code| code == PROVIDER_QUOTA_EXHAUSTED_CODE)
}

#[must_use]
pub fn sanitize_provider_error_detail(message: &str) -> String {
    if message.is_empty() {
        "upstream provider request failed; response body was empty".to_string()
    } else {
        "upstream provider request failed; response details were redacted".to_string()
    }
}

pub(super) const fn abort_retry<E>(_error: &E) -> RetryDecision {
    RetryDecision::Abort
}

pub(super) fn classify_provider_call_error(error: &ProviderCallError) -> RetryDecision {
    match error {
        ProviderCallError::Admission { .. } => RetryDecision::Abort,
        ProviderCallError::Transport { source, .. }
        | ProviderCallError::ResponseBody { source, .. } => {
            if is_retryable_transport_error(source) {
                RetryDecision::Retry
            } else {
                RetryDecision::Abort
            }
        }
        ProviderCallError::ResponsePolicy { source, .. } => {
            if matches!(source, PublicHttpUrlError::BodyReadFailed(_)) {
                RetryDecision::Retry
            } else {
                RetryDecision::Abort
            }
        }
        ProviderCallError::HttpStatus {
            status, retry_after, quota_exhausted, empty_body, ..
        } => {
            if *status == StatusCode::TOO_MANY_REQUESTS {
                record_provider_429();
            }
            classify_http_status(*status, *retry_after, *quota_exhausted, *empty_body)
        }
        ProviderCallError::ResponseJson { .. }
        | ProviderCallError::Json { .. }
        | ProviderCallError::Protocol { .. }
        | ProviderCallError::Timeout { .. } => RetryDecision::Abort,
    }
}

fn classify_http_status(
    status: StatusCode,
    retry_after: Option<Duration>,
    quota_exhausted: bool,
    empty_body: bool,
) -> RetryDecision {
    // Billing quota does not replenish on a retry timescale.
    if quota_exhausted {
        return RetryDecision::Abort;
    }
    match status {
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE => {
            retry_after.map_or(RetryDecision::Retry, RetryDecision::RetryAfter)
        }
        // Only a proven-empty upstream body gets the whole-call single retry.
        StatusCode::NOT_FOUND if empty_body => RetryDecision::RetryOnce,
        status if status.is_server_error() => RetryDecision::Retry,
        _ => RetryDecision::Abort,
    }
}

pub(super) fn retry_delay<E>(policy: &RetryPolicy<E>, attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(31);
    let multiplier = 1_u128 << exponent;
    let millis = policy.initial_backoff.as_millis().saturating_mul(multiplier);
    let capped_millis = millis.min(policy.max_backoff.as_millis()).min(u128::from(u64::MAX));
    let delay = Duration::from_millis(capped_millis as u64);
    if !policy.jitter || delay.is_zero() {
        return delay;
    }

    let max_millis = delay.as_millis().min(u128::from(u64::MAX)) as u64;
    Duration::from_millis(rand::rng().random_range(0..=max_millis))
}

pub(super) fn retry_after_from_headers(headers: &HeaderMap) -> Option<Duration> {
    let value = headers.get(RETRY_AFTER)?.to_str().ok()?.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }

    let retry_at = httpdate::parse_http_date(value).ok()?;
    Some(retry_at.duration_since(std::time::SystemTime::now()).unwrap_or(Duration::ZERO))
}

fn is_retryable_transport_error(error: &reqwest::Error) -> bool {
    error.is_timeout() || error.is_connect() || error.is_body()
}
