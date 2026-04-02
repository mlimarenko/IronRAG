use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const DEFAULT_WEB_CRAWL_DEPTH: i32 = 3;
pub const DEFAULT_WEB_CRAWL_MAX_PAGES: i32 = 100;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebIngestMode {
    SinglePage,
    RecursiveCrawl,
}

impl WebIngestMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SinglePage => "single_page",
            Self::RecursiveCrawl => "recursive_crawl",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebBoundaryPolicy {
    SameHost,
    AllowExternal,
}

impl WebBoundaryPolicy {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SameHost => "same_host",
            Self::AllowExternal => "allow_external",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebCandidateState {
    Discovered,
    Eligible,
    Duplicate,
    Excluded,
    Blocked,
    Queued,
    Processing,
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
            Self::Processed => "processed",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WebClassificationReason {
    SeedAccepted,
    DuplicateCanonicalUrl,
    OutsideBoundaryPolicy,
    ExceededMaxDepth,
    ExceededMaxPages,
    SystemPage,
    UnsupportedScheme,
    InvalidUrl,
    Inaccessible,
    UnsupportedContent,
    CancelRequested,
}

impl WebClassificationReason {
    pub const ALL: [Self; 11] = [
        Self::SeedAccepted,
        Self::DuplicateCanonicalUrl,
        Self::OutsideBoundaryPolicy,
        Self::ExceededMaxDepth,
        Self::ExceededMaxPages,
        Self::SystemPage,
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
            Self::DuplicateCanonicalUrl => "duplicate_canonical_url",
            Self::OutsideBoundaryPolicy => "outside_boundary_policy",
            Self::ExceededMaxDepth => "exceeded_max_depth",
            Self::ExceededMaxPages => "exceeded_max_pages",
            Self::SystemPage => "system_page",
            Self::UnsupportedScheme => "unsupported_scheme",
            Self::InvalidUrl => "invalid_url",
            Self::Inaccessible => "inaccessible",
            Self::UnsupportedContent => "unsupported_content",
            Self::CancelRequested => "cancel_requested",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

impl Default for WebRunCounts {
    fn default() -> Self {
        Self {
            discovered: 0,
            eligible: 0,
            processed: 0,
            queued: 0,
            processing: 0,
            duplicates: 0,
            excluded: 0,
            blocked: 0,
            failed: 0,
            canceled: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ValidatedWebRunSettings {
    pub mode: String,
    pub boundary_policy: String,
    pub max_depth: i32,
    pub max_pages: i32,
}

pub fn validate_web_run_settings(
    mode: &str,
    boundary_policy: Option<&str>,
    max_depth: Option<i32>,
    max_pages: Option<i32>,
) -> Result<ValidatedWebRunSettings, String> {
    let mode = parse_mode(mode)?;
    let boundary_policy =
        parse_boundary_policy(boundary_policy.unwrap_or(WebBoundaryPolicy::SameHost.as_str()))?;
    let normalized_depth = match mode {
        WebIngestMode::SinglePage => 0,
        WebIngestMode::RecursiveCrawl => max_depth.unwrap_or(DEFAULT_WEB_CRAWL_DEPTH),
    };
    if normalized_depth < 0 {
        return Err("maxDepth must be greater than or equal to 0".to_string());
    }
    let normalized_pages = max_pages.unwrap_or(DEFAULT_WEB_CRAWL_MAX_PAGES);
    if normalized_pages < 1 {
        return Err("maxPages must be greater than or equal to 1".to_string());
    }
    Ok(ValidatedWebRunSettings {
        mode: mode.as_str().to_string(),
        boundary_policy: boundary_policy.as_str().to_string(),
        max_depth: normalized_depth,
        max_pages: normalized_pages,
    })
}

#[must_use]
pub fn derive_terminal_run_state(counts: &WebRunCounts) -> WebRunState {
    let has_non_success =
        counts.failed > 0 || counts.blocked > 0 || counts.excluded > 0 || counts.canceled > 0;
    if counts.processed > 0 && has_non_success {
        WebRunState::CompletedPartial
    } else if (counts.failed > 0 || counts.blocked > 0) && counts.processed == 0 {
        WebRunState::Failed
    } else if counts.canceled > 0 && counts.processed == 0 {
        WebRunState::Canceled
    } else {
        WebRunState::Completed
    }
}

#[must_use]
pub fn now_if_terminal(run_state: &str) -> Option<DateTime<Utc>> {
    match run_state {
        "completed" | "completed_partial" | "failed" | "canceled" => Some(Utc::now()),
        _ => None,
    }
}

fn parse_mode(mode: &str) -> Result<WebIngestMode, String> {
    match mode {
        "single_page" => Ok(WebIngestMode::SinglePage),
        "recursive_crawl" => Ok(WebIngestMode::RecursiveCrawl),
        _ => Err("mode must be one of: single_page, recursive_crawl".to_string()),
    }
}

fn parse_boundary_policy(boundary_policy: &str) -> Result<WebBoundaryPolicy, String> {
    match boundary_policy {
        "same_host" => Ok(WebBoundaryPolicy::SameHost),
        "allow_external" => Ok(WebBoundaryPolicy::AllowExternal),
        _ => Err("boundaryPolicy must be one of: same_host, allow_external".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_WEB_CRAWL_DEPTH, DEFAULT_WEB_CRAWL_MAX_PAGES, WebClassificationReason,
        WebRunCounts, WebRunFailureCode, WebRunState, derive_terminal_run_state,
        validate_web_run_settings,
    };

    #[test]
    fn single_page_forces_zero_depth() {
        let settings =
            validate_web_run_settings("single_page", Some("same_host"), Some(8), Some(12))
                .expect("validate settings");
        assert_eq!(settings.max_depth, 0);
        assert_eq!(settings.max_pages, 12);
    }

    #[test]
    fn recursive_defaults_are_applied() {
        let settings = validate_web_run_settings("recursive_crawl", None, None, None)
            .expect("validate settings");
        assert_eq!(settings.max_depth, DEFAULT_WEB_CRAWL_DEPTH);
        assert_eq!(settings.max_pages, DEFAULT_WEB_CRAWL_MAX_PAGES);
    }

    #[test]
    fn partial_completion_is_derived_from_mixed_counts() {
        let counts = WebRunCounts { processed: 2, failed: 1, ..WebRunCounts::default() };
        assert_eq!(derive_terminal_run_state(&counts), WebRunState::CompletedPartial);
    }

    #[test]
    fn blocked_only_run_is_failed() {
        let counts = WebRunCounts { blocked: 1, ..WebRunCounts::default() };
        assert_eq!(derive_terminal_run_state(&counts), WebRunState::Failed);
    }

    #[test]
    fn failure_codes_are_stable() {
        assert_eq!(
            WebRunFailureCode::ALL.map(WebRunFailureCode::as_str),
            [
                "inaccessible",
                "invalid_url",
                "unsupported_content",
                "web_discovery_failed",
                "web_snapshot_persist_failed",
                "web_snapshot_missing",
                "web_snapshot_missing_final_url",
                "web_snapshot_unavailable",
                "web_capture_materialization_failed",
                "recursive_crawl_failed",
            ]
        );
    }

    #[test]
    fn classification_reason_vocabulary_is_stable() {
        assert_eq!(
            WebClassificationReason::ALL.map(WebClassificationReason::as_str),
            [
                "seed_accepted",
                "duplicate_canonical_url",
                "outside_boundary_policy",
                "exceeded_max_depth",
                "exceeded_max_pages",
                "system_page",
                "unsupported_scheme",
                "invalid_url",
                "inaccessible",
                "unsupported_content",
                "cancel_requested",
            ]
        );
    }
}
