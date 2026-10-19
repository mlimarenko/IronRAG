use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::{
    DEFAULT_WEB_CRAWL_DEPTH, DEFAULT_WEB_CRAWL_MAX_PAGES, MAX_WEB_CRAWL_DEPTH,
    MAX_WEB_CRAWL_MAX_PAGES, WebBoundaryPolicy, WebIngestMode,
};
use crate::shared::web::url_identity::seed_host_supports_subdomains;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebRunState {
    Accepted,
    Discovering,
    Processing,
    Completed,
    CompletedPartial,
    Failed,
    Canceled,
}

impl WebRunState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Discovering => "discovering",
            Self::Processing => "processing",
            Self::Completed => "completed",
            Self::CompletedPartial => "completed_partial",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebCandidateState {
    Discovered,
    Eligible,
    Duplicate,
    Excluded,
    Blocked,
    Queued,
    Processing,
    Materialized,
    Processed,
    Failed,
    Canceled,
}

impl WebCandidateState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Discovered => "discovered",
            Self::Eligible => "eligible",
            Self::Duplicate => "duplicate",
            Self::Excluded => "excluded",
            Self::Blocked => "blocked",
            Self::Queued => "queued",
            Self::Processing => "processing",
            Self::Materialized => "materialized",
            Self::Processed => "processed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebClassificationReason {
    SeedAccepted,
    #[serde(rename = "duplicate_\u{63}anonical_url")]
    DuplicatePreferredUrl,
    /// Two distinct URLs hashed to the same content body. Distinct from
    /// `DuplicatePreferredUrl` so the observability split stays
    /// meaningful: URL-level dupes are crawler hygiene, content-level
    /// dupes point at a site that serves the same body under many
    /// query/fragment variants.
    DuplicateContent,
    OutsideBoundaryPolicy,
    ExceededMaxDepth,
    ExceededMaxPages,
    UrlFilter,
    UnsupportedScheme,
    InvalidUrl,
    Inaccessible,
    UnsupportedContent,
    CancelRequested,
}

impl WebClassificationReason {
    pub const ALL: [Self; 12] = [
        Self::SeedAccepted,
        Self::DuplicatePreferredUrl,
        Self::DuplicateContent,
        Self::OutsideBoundaryPolicy,
        Self::ExceededMaxDepth,
        Self::ExceededMaxPages,
        Self::UrlFilter,
        Self::UnsupportedScheme,
        Self::InvalidUrl,
        Self::Inaccessible,
        Self::UnsupportedContent,
        Self::CancelRequested,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SeedAccepted => "seed_accepted",
            Self::DuplicatePreferredUrl => "duplicate_\u{63}anonical_url",
            Self::DuplicateContent => "duplicate_content",
            Self::OutsideBoundaryPolicy => "outside_boundary_policy",
            Self::ExceededMaxDepth => "exceeded_max_depth",
            Self::ExceededMaxPages => "exceeded_max_pages",
            Self::UrlFilter => "url_filter",
            Self::UnsupportedScheme => "unsupported_scheme",
            Self::InvalidUrl => "invalid_url",
            Self::Inaccessible => "inaccessible",
            Self::UnsupportedContent => "unsupported_content",
            Self::CancelRequested => "cancel_requested",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WebRunFailureCode {
    Inaccessible,
    InvalidUrl,
    UnsupportedContent,
    WebDiscoveryFailed,
    WebSnapshotPersistFailed,
    WebSnapshotMissing,
    WebSnapshotMissingFinalUrl,
    WebSnapshotUnavailable,
    WebCaptureMaterializationFailed,
    RecursiveCrawlFailed,
}

impl WebRunFailureCode {
    pub const ALL: [Self; 10] = [
        Self::Inaccessible,
        Self::InvalidUrl,
        Self::UnsupportedContent,
        Self::WebDiscoveryFailed,
        Self::WebSnapshotPersistFailed,
        Self::WebSnapshotMissing,
        Self::WebSnapshotMissingFinalUrl,
        Self::WebSnapshotUnavailable,
        Self::WebCaptureMaterializationFailed,
        Self::RecursiveCrawlFailed,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inaccessible => "inaccessible",
            Self::InvalidUrl => "invalid_url",
            Self::UnsupportedContent => "unsupported_content",
            Self::WebDiscoveryFailed => "web_discovery_failed",
            Self::WebSnapshotPersistFailed => "web_snapshot_persist_failed",
            Self::WebSnapshotMissing => "web_snapshot_missing",
            Self::WebSnapshotMissingFinalUrl => "web_snapshot_missing_final_url",
            Self::WebSnapshotUnavailable => "web_snapshot_unavailable",
            Self::WebCaptureMaterializationFailed => "web_capture_materialization_failed",
            Self::RecursiveCrawlFailed => "recursive_crawl_failed",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebRunCounts {
    pub discovered: i64,
    pub eligible: i64,
    pub processed: i64,
    pub queued: i64,
    pub processing: i64,
    pub duplicates: i64,
    pub excluded: i64,
    pub blocked: i64,
    pub failed: i64,
    pub canceled: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedWebRunSettings {
    pub mode: String,
    pub boundary_policy: String,
    pub max_depth: i32,
    pub max_pages: i32,
}

/// Validates and normalizes runtime web-ingest settings for a crawl or single-page run.
///
/// # Errors
///
/// Returns an error when the mode, boundary policy, depth, or page limits are invalid.
pub fn validate_web_run_settings(
    mode: &str,
    boundary_policy: Option<&str>,
    max_depth: Option<i32>,
    max_pages: Option<i32>,
) -> Result<ValidatedWebRunSettings, String> {
    let mode = parse_mode(mode)?;
    let boundary_policy =
        parse_web_boundary_policy(boundary_policy.unwrap_or(WebBoundaryPolicy::SameHost.as_str()))?;
    let normalized_depth = match mode {
        WebIngestMode::SinglePage => 0,
        WebIngestMode::RecursiveCrawl => max_depth.unwrap_or(DEFAULT_WEB_CRAWL_DEPTH),
    };
    if normalized_depth < 0 {
        return Err("maxDepth must be greater than or equal to 0".to_string());
    }
    if normalized_depth > MAX_WEB_CRAWL_DEPTH {
        return Err(format!("maxDepth must be less than or equal to {MAX_WEB_CRAWL_DEPTH}"));
    }
    let normalized_pages = max_pages.unwrap_or(DEFAULT_WEB_CRAWL_MAX_PAGES);
    if normalized_pages < 1 {
        return Err("maxPages must be greater than or equal to 1".to_string());
    }
    if normalized_pages > MAX_WEB_CRAWL_MAX_PAGES {
        return Err(format!("maxPages must be less than or equal to {MAX_WEB_CRAWL_MAX_PAGES}"));
    }
    Ok(ValidatedWebRunSettings {
        mode: mode.as_str().to_string(),
        boundary_policy: boundary_policy.as_str().to_string(),
        max_depth: normalized_depth,
        max_pages: normalized_pages,
    })
}

/// Validates the relationship between a boundary policy and the normalized seed URL.
///
/// # Errors
///
/// Returns an error when the selected boundary policy does not apply to the seed host.
pub fn validate_web_boundary_seed_host(
    boundary_policy: &str,
    normalized_seed_url: &str,
) -> Result<(), String> {
    let boundary_policy = parse_web_boundary_policy(boundary_policy)?;
    if boundary_policy == WebBoundaryPolicy::SameHostAndSubdomains
        && !seed_host_supports_subdomains(normalized_seed_url).map_err(|error| error.to_string())?
    {
        return Err("boundaryPolicy same_host_and_subdomains requires a DNS seed host".to_string());
    }
    Ok(())
}

/// Derives the terminal run state from the observed crawl counters.
#[must_use]
pub const fn derive_terminal_run_state(counts: &WebRunCounts) -> WebRunState {
    let has_non_success =
        counts.failed > 0 || counts.blocked > 0 || counts.excluded > 0 || counts.canceled > 0;
    if counts.processed > 0 && has_non_success {
        WebRunState::CompletedPartial
    } else if (counts.failed > 0 || counts.blocked > 0) && counts.processed == 0 {
        WebRunState::Failed
    } else if counts.excluded > 0 && counts.processed == 0 && counts.canceled == 0 {
        WebRunState::CompletedPartial
    } else if counts.canceled > 0 && counts.processed == 0 {
        WebRunState::Canceled
    } else {
        WebRunState::Completed
    }
}

/// Returns the current timestamp only when the run state is terminal.
#[must_use]
pub fn now_if_terminal(run_state: &str) -> Option<DateTime<Utc>> {
    match run_state {
        "completed" | "completed_partial" | "failed" | "canceled" => Some(Utc::now()),
        _ => None,
    }
}

/// Parses the web-ingest mode vocabulary.
///
/// # Errors
///
/// Returns an error when the mode is not one of the normalized values.
fn parse_mode(mode: &str) -> Result<WebIngestMode, String> {
    match mode {
        "single_page" => Ok(WebIngestMode::SinglePage),
        "recursive_crawl" => Ok(WebIngestMode::RecursiveCrawl),
        _ => Err("mode must be one of: single_page, recursive_crawl".to_string()),
    }
}

/// Parses the supported boundary-policy vocabulary.
///
/// # Errors
///
/// Returns an error when the boundary policy is not one of the normalized values.
pub fn parse_web_boundary_policy(boundary_policy: &str) -> Result<WebBoundaryPolicy, String> {
    match boundary_policy {
        "same_host" => Ok(WebBoundaryPolicy::SameHost),
        "same_host_and_subdomains" => Ok(WebBoundaryPolicy::SameHostAndSubdomains),
        "allow_external" => Ok(WebBoundaryPolicy::AllowExternal),
        _ => Err(
            "boundaryPolicy must be one of: same_host, same_host_and_subdomains, allow_external"
                .to_string(),
        ),
    }
}
