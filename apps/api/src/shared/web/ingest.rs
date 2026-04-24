use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use url::Url;

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebIngestPolicy {
    #[serde(default)]
    pub ignore_patterns: Vec<WebIngestIgnorePattern>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebIngestIgnorePattern {
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIngestIgnoreMatch {
    pub detail: String,
}

#[must_use]
pub fn default_web_ingest_policy() -> WebIngestPolicy {
    WebIngestPolicy {
        ignore_patterns: [
            ("path_prefix", "/aboutconfluencepage.action"),
            ("path_prefix", "/collector/pages.action"),
            ("path_prefix", "/dashboard/configurerssfeed.action"),
            ("path_prefix", "/exportword"),
            ("path_prefix", "/forgotuserpassword.action"),
            ("path_prefix", "/labels/viewlabel.action"),
            ("path_prefix", "/login.action"),
            ("path_prefix", "/pages/diffpages.action"),
            ("path_prefix", "/pages/diffpagesbyversion.action"),
            ("path_prefix", "/pages/listundefinedpages.action"),
            ("path_prefix", "/pages/reorderpages.action"),
            ("path_prefix", "/pages/viewinfo.action"),
            ("path_prefix", "/pages/viewpageattachments.action"),
            ("path_prefix", "/pages/viewpreviousversions.action"),
            ("path_prefix", "/plugins/viewsource/viewpagesrc.action"),
            ("path_prefix", "/spacedirectory/view.action"),
            ("path_prefix", "/spaces/flyingpdf/pdfpageexport.action"),
            ("path_prefix", "/spaces/listattachmentsforspace.action"),
            ("path_prefix", "/spaces/listrssfeeds.action"),
            ("path_prefix", "/spaces/viewspacesummary.action"),
            ("glob", "*/display/~*"),
            ("glob", "*os_destination=*"),
            ("glob", "*permissionviolation=*"),
        ]
        .into_iter()
        .map(|(kind, value)| WebIngestIgnorePattern {
            kind: kind.to_string(),
            value: value.to_string(),
            source: None,
        })
        .collect(),
    }
}

/// Validates and normalizes reusable library-owned web-ingest policy.
///
/// # Errors
///
/// Returns an error when any ignore pattern uses an unknown kind or an invalid value.
pub fn validate_web_ingest_policy(policy: WebIngestPolicy) -> Result<WebIngestPolicy, String> {
    Ok(WebIngestPolicy {
        ignore_patterns: normalize_web_ingest_ignore_patterns(
            policy.ignore_patterns,
            None,
            "webIngestPolicy.ignorePatterns",
        )?,
    })
}

/// Merges library policy with run-local additions into a run snapshot.
///
/// # Errors
///
/// Returns an error when either policy source contains invalid ignore patterns.
pub fn build_web_ingest_run_ignore_patterns(
    library_policy: &WebIngestPolicy,
    extra_ignore_patterns: Vec<WebIngestIgnorePattern>,
) -> Result<Vec<WebIngestIgnorePattern>, String> {
    let mut merged = normalize_web_ingest_ignore_patterns(
        library_policy.ignore_patterns.clone(),
        Some("library"),
        "webIngestPolicy.ignorePatterns",
    )?;
    let mut seen = merged
        .iter()
        .map(|pattern| (pattern.kind.clone(), pattern.value.clone()))
        .collect::<std::collections::BTreeSet<_>>();

    for pattern in normalize_web_ingest_ignore_patterns(
        extra_ignore_patterns,
        Some("run"),
        "extraIgnorePatterns",
    )? {
        if seen.insert((pattern.kind.clone(), pattern.value.clone())) {
            merged.push(pattern);
        }
    }
    Ok(merged)
}

#[must_use]
pub fn match_web_ingest_ignore_pattern(
    url: &str,
    patterns: &[WebIngestIgnorePattern],
) -> Option<WebIngestIgnoreMatch> {
    let parsed = Url::parse(url).ok();
    patterns.iter().find_map(|pattern| {
        let matched = match pattern.kind.as_str() {
            "url_prefix" => url.starts_with(&pattern.value),
            "path_prefix" => parsed
                .as_ref()
                .is_some_and(|parsed_url| parsed_url.path().starts_with(&pattern.value)),
            "glob" => wildcard_matches(&pattern.value, url),
            _ => false,
        };
        matched.then(|| WebIngestIgnoreMatch {
            detail: format!(
                "{}:{}:{}",
                pattern.source.as_deref().unwrap_or("policy"),
                pattern.kind,
                pattern.value
            ),
        })
    })
}

fn normalize_web_ingest_ignore_patterns(
    patterns: Vec<WebIngestIgnorePattern>,
    source: Option<&'static str>,
    field_name: &'static str,
) -> Result<Vec<WebIngestIgnorePattern>, String> {
    let mut normalized = Vec::with_capacity(patterns.len());
    let mut seen = std::collections::BTreeSet::<(String, String)>::new();
    for pattern in patterns {
        let kind = normalize_ignore_pattern_kind(&pattern.kind, field_name)?;
        let value = normalize_ignore_pattern_value(&kind, &pattern.value, field_name)?;
        if seen.insert((kind.clone(), value.clone())) {
            normalized.push(WebIngestIgnorePattern {
                kind,
                value,
                source: source.map(str::to_string),
            });
        }
    }
    Ok(normalized)
}

fn normalize_ignore_pattern_kind(value: &str, field_name: &'static str) -> Result<String, String> {
    let normalized = value.trim();
    match normalized {
        "url_prefix" | "path_prefix" | "glob" => Ok(normalized.to_string()),
        _ => Err(format!("{field_name}.kind must be one of: url_prefix, path_prefix, glob")),
    }
}

fn normalize_ignore_pattern_value(
    kind: &str,
    value: &str,
    field_name: &'static str,
) -> Result<String, String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(format!("{field_name}.value must not be empty"));
    }
    if normalized.len() > 2048 {
        return Err(format!("{field_name}.value must be at most 2048 characters"));
    }
    if kind == "path_prefix" && !normalized.starts_with('/') {
        return Err(format!("{field_name}.value must start with / for path_prefix"));
    }
    if kind == "url_prefix" && Url::parse(normalized).is_err() {
        return Err(format!("{field_name}.value must be an absolute URL for url_prefix"));
    }
    Ok(normalized.to_string())
}

fn wildcard_matches(pattern: &str, candidate: &str) -> bool {
    let pattern = pattern.as_bytes();
    let candidate = candidate.as_bytes();
    let (mut pattern_index, mut candidate_index) = (0usize, 0usize);
    let mut star_index = None;
    let mut star_candidate_index = 0usize;

    while candidate_index < candidate.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == candidate[candidate_index]
                || pattern[pattern_index] == b'?')
        {
            pattern_index += 1;
            candidate_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            pattern_index += 1;
            star_candidate_index = candidate_index;
        } else if let Some(previous_star_index) = star_index {
            pattern_index = previous_star_index + 1;
            star_candidate_index += 1;
            candidate_index = star_candidate_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }

    pattern_index == pattern.len()
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
    /// Two distinct URLs hashed to the same content body. Distinct from
    /// `DuplicateCanonicalUrl` so the observability split stays
    /// meaningful: URL-level dupes are crawler hygiene, content-level
    /// dupes point at a site that serves the same body under many
    /// query/fragment variants.
    DuplicateContent,
    OutsideBoundaryPolicy,
    ExceededMaxDepth,
    ExceededMaxPages,
    IgnorePattern,
    UnsupportedScheme,
    InvalidUrl,
    Inaccessible,
    UnsupportedContent,
    CancelRequested,
}

impl WebClassificationReason {
    pub const ALL: [Self; 12] = [
        Self::SeedAccepted,
        Self::DuplicateCanonicalUrl,
        Self::DuplicateContent,
        Self::OutsideBoundaryPolicy,
        Self::ExceededMaxDepth,
        Self::ExceededMaxPages,
        Self::IgnorePattern,
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
            Self::DuplicateContent => "duplicate_content",
            Self::OutsideBoundaryPolicy => "outside_boundary_policy",
            Self::ExceededMaxDepth => "exceeded_max_depth",
            Self::ExceededMaxPages => "exceeded_max_pages",
            Self::IgnorePattern => "ignore_pattern",
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
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

/// Derives the terminal run state from the observed crawl counters.
#[must_use]
pub const fn derive_terminal_run_state(counts: &WebRunCounts) -> WebRunState {
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

/// Returns the current timestamp only when the run state is terminal.
#[must_use]
pub fn now_if_terminal(run_state: &str) -> Option<DateTime<Utc>> {
    match run_state {
        "completed" | "completed_partial" | "failed" | "canceled" => Some(Utc::now()),
        _ => None,
    }
}

/// Parses the canonical web-ingest mode vocabulary.
///
/// # Errors
///
/// Returns an error when the mode is not one of the canonical values.
/// Parses the canonical web-ingest mode vocabulary.
///
/// # Errors
///
/// Returns an error when the mode is not one of the canonical values.
fn parse_mode(mode: &str) -> Result<WebIngestMode, String> {
    match mode {
        "single_page" => Ok(WebIngestMode::SinglePage),
        "recursive_crawl" => Ok(WebIngestMode::RecursiveCrawl),
        _ => Err("mode must be one of: single_page, recursive_crawl".to_string()),
    }
}

/// Parses the canonical boundary-policy vocabulary.
///
/// # Errors
///
/// Returns an error when the boundary policy is not one of the canonical values.
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
        WebIngestIgnorePattern, WebIngestPolicy, WebRunCounts, WebRunFailureCode, WebRunState,
        build_web_ingest_run_ignore_patterns, derive_terminal_run_state,
        match_web_ingest_ignore_pattern, validate_web_ingest_policy, validate_web_run_settings,
    };

    #[test]
    fn single_page_forces_zero_depth() {
        let settings =
            match validate_web_run_settings("single_page", Some("same_host"), Some(8), Some(12)) {
                Ok(settings) => settings,
                Err(error) => panic!("validate settings: {error}"),
            };
        assert_eq!(settings.max_depth, 0);
        assert_eq!(settings.max_pages, 12);
    }

    #[test]
    fn recursive_defaults_are_applied() {
        let settings = match validate_web_run_settings("recursive_crawl", None, None, None) {
            Ok(settings) => settings,
            Err(error) => panic!("validate settings: {error}"),
        };
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
                "duplicate_content",
                "outside_boundary_policy",
                "exceeded_max_depth",
                "exceeded_max_pages",
                "ignore_pattern",
                "unsupported_scheme",
                "invalid_url",
                "inaccessible",
                "unsupported_content",
                "cancel_requested",
            ]
        );
    }

    #[test]
    fn validates_library_web_ingest_policy() {
        let policy = validate_web_ingest_policy(WebIngestPolicy {
            ignore_patterns: vec![
                WebIngestIgnorePattern {
                    kind: " path_prefix ".to_string(),
                    value: " /labels/viewlabel.action ".to_string(),
                    source: Some("ignored".to_string()),
                },
                WebIngestIgnorePattern {
                    kind: "path_prefix".to_string(),
                    value: "/labels/viewlabel.action".to_string(),
                    source: None,
                },
            ],
        })
        .expect("policy should validate");

        assert_eq!(
            policy.ignore_patterns,
            vec![WebIngestIgnorePattern {
                kind: "path_prefix".to_string(),
                value: "/labels/viewlabel.action".to_string(),
                source: None,
            }]
        );
    }

    #[test]
    fn merges_library_and_run_ignore_patterns_with_sources() {
        let merged = build_web_ingest_run_ignore_patterns(
            &WebIngestPolicy {
                ignore_patterns: vec![WebIngestIgnorePattern {
                    kind: "path_prefix".to_string(),
                    value: "/labels/viewlabel.action".to_string(),
                    source: None,
                }],
            },
            vec![WebIngestIgnorePattern {
                kind: "glob".to_string(),
                value: "*/print/*".to_string(),
                source: None,
            }],
        )
        .expect("patterns should merge");

        assert_eq!(merged[0].source.as_deref(), Some("library"));
        assert_eq!(merged[1].source.as_deref(), Some("run"));
    }

    #[test]
    fn matches_ignore_patterns_by_url_path_and_glob() {
        let patterns = vec![
            WebIngestIgnorePattern {
                kind: "path_prefix".to_string(),
                value: "/labels/viewlabel.action".to_string(),
                source: Some("library".to_string()),
            },
            WebIngestIgnorePattern {
                kind: "url_prefix".to_string(),
                value: "https://docs.example.test/archive/".to_string(),
                source: Some("run".to_string()),
            },
            WebIngestIgnorePattern {
                kind: "glob".to_string(),
                value: "*permissionviolation=*".to_string(),
                source: Some("library".to_string()),
            },
        ];

        assert_eq!(
            match_web_ingest_ignore_pattern(
                "https://docs.example.test/labels/viewlabel.action?key=ABC",
                &patterns,
            )
            .map(|matched| matched.detail),
            Some("library:path_prefix:/labels/viewlabel.action".to_string())
        );
        assert!(
            match_web_ingest_ignore_pattern(
                "https://docs.example.test/archive/old-page",
                &patterns,
            )
            .is_some()
        );
        assert!(
            match_web_ingest_ignore_pattern(
                "https://docs.example.test/login.action?permissionviolation=true",
                &patterns,
            )
            .is_some()
        );
    }
}
