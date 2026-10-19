use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebIngestPolicy {
    pub crawl_filter: WebIngestUrlFilter,
    pub materialization_filter: WebIngestUrlFilter,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebIngestUrlFilter {
    pub allow_patterns: Vec<WebIngestPattern>,
    pub block_patterns: Vec<WebIngestPattern>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WebIngestPattern {
    pub kind: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIngestPatternMatch {
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebIngestUrlFilterExclusion {
    pub detail: String,
}

#[must_use]
pub fn default_web_ingest_policy() -> WebIngestPolicy {
    WebIngestPolicy {
        crawl_filter: default_web_ingest_crawl_filter(),
        materialization_filter: WebIngestUrlFilter::default(),
    }
}

#[must_use]
pub fn default_web_ingest_crawl_filter() -> WebIngestUrlFilter {
    WebIngestUrlFilter {
        allow_patterns: Vec::new(),
        block_patterns: default_web_ingest_crawl_block_patterns(),
    }
}

/// Returns the domain-neutral blocklist used by generic web ingestion.
///
/// Generic crawl safety is enforced by boundary, depth, and page limits. Product-specific URL
/// rules belong in an explicit library policy or profile.
#[must_use]
pub const fn default_web_ingest_crawl_block_patterns() -> Vec<WebIngestPattern> {
    Vec::new()
}

/// Returns an opt-in policy for crawling Confluence sites.
#[must_use]
pub fn confluence_web_ingest_policy() -> WebIngestPolicy {
    WebIngestPolicy {
        crawl_filter: WebIngestUrlFilter {
            allow_patterns: Vec::new(),
            block_patterns: confluence_web_ingest_crawl_block_patterns(),
        },
        materialization_filter: WebIngestUrlFilter::default(),
    }
}

/// Returns URL rules that suppress Confluence navigation and administrative surfaces.
#[must_use]
pub fn confluence_web_ingest_crawl_block_patterns() -> Vec<WebIngestPattern> {
    [
        ("path_prefix", "/aboutconfluencepage.action"),
        ("path_prefix", "/collector/pages.action"),
        ("path_prefix", "/dashboard/configurerssfeed.action"),
        ("path_prefix", "/exportword"),
        ("path_prefix", "/forgotuserpassword.action"),
        ("path_prefix", "/label/"),
        ("path_prefix", "/labels/"),
        ("path_prefix", "/login.action"),
        ("path_prefix", "/pages/diffpages.action"),
        ("path_prefix", "/pages/diffpagesbyversion.action"),
        ("path_prefix", "/pages/listundefinedpages.action"),
        ("path_prefix", "/pages/reorderpages.action"),
        ("path_prefix", "/pages/viewinfo.action"),
        ("path_prefix", "/pages/viewpageattachments.action"),
        ("path_prefix", "/pages/viewpreviousversions.action"),
        ("path_prefix", "/pages/worddav/"),
        ("path_prefix", "/plugins/recently-updated/"),
        ("path_prefix", "/plugins/viewsource/viewpagesrc.action"),
        ("path_prefix", "/dashboard.action"),
        ("path_prefix", "/spacedirectory/view.action"),
        ("path_prefix", "/spaces/flyingpdf/pdfpageexport.action"),
        ("path_prefix", "/spaces/listattachmentsforspace.action"),
        ("path_prefix", "/spaces/listrssfeeds.action"),
        ("path_prefix", "/spaces/viewspacesummary.action"),
        ("glob", "*/display/~*"),
        ("glob", "*fileExtension=*"),
        ("glob", "*labels=*"),
        ("glob", "*metadataLink=*"),
        ("glob", "*navigatingVersions=*"),
        ("glob", "*openId=*"),
        ("glob", "*originalVersion=*"),
        ("glob", "*os_destination=*"),
        ("glob", "*permissionViolation=*"),
        ("glob", "*permissionviolation=*"),
        ("glob", "*revisedVersion=*"),
        ("glob", "*selectedPageVersions=*"),
        ("glob", "*sortBy=*"),
    ]
    .into_iter()
    .map(|(kind, value)| WebIngestPattern {
        kind: kind.to_string(),
        value: value.to_string(),
        source: None,
    })
    .collect()
}

/// Validates and normalizes reusable library-owned web-ingest policy.
///
/// # Errors
///
/// Returns an error when any URL filter pattern uses an unknown kind or an invalid value.
pub fn validate_web_ingest_policy(policy: WebIngestPolicy) -> Result<WebIngestPolicy, String> {
    Ok(WebIngestPolicy {
        crawl_filter: normalize_web_ingest_url_filter(
            policy.crawl_filter,
            None,
            "webIngestPolicy.crawlFilter",
        )?,
        materialization_filter: normalize_web_ingest_url_filter(
            policy.materialization_filter,
            None,
            "webIngestPolicy.materializationFilter",
        )?,
    })
}

/// Validates and normalizes the full URL filter used by one web-ingest run.
///
/// # Errors
///
/// Returns an error when the filter is incomplete or any pattern is invalid.
pub fn validate_web_ingest_url_filter(
    filter: WebIngestUrlFilter,
    field_name: &'static str,
) -> Result<WebIngestUrlFilter, String> {
    normalize_web_ingest_url_filter(filter, Some("run"), field_name)
}

fn normalize_web_ingest_url_filter(
    filter: WebIngestUrlFilter,
    source: Option<&'static str>,
    field_name: &'static str,
) -> Result<WebIngestUrlFilter, String> {
    let allow_patterns = normalize_web_ingest_patterns(
        filter.allow_patterns,
        source,
        format!("{field_name}.allowPatterns"),
    )?;
    let block_patterns = normalize_web_ingest_patterns(
        filter.block_patterns,
        source,
        format!("{field_name}.blockPatterns"),
    )?;
    Ok(WebIngestUrlFilter { allow_patterns, block_patterns })
}

#[must_use]
pub fn match_web_ingest_pattern(
    url: &str,
    patterns: &[WebIngestPattern],
) -> Option<WebIngestPatternMatch> {
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
        matched.then(|| WebIngestPatternMatch {
            detail: format!(
                "{}:{}:{}",
                pattern.source.as_deref().unwrap_or("policy"),
                pattern.kind,
                pattern.value
            ),
        })
    })
}

#[must_use]
pub fn classify_web_crawl_filter_exclusion(
    url: &str,
    filter: &WebIngestUrlFilter,
) -> Option<WebIngestUrlFilterExclusion> {
    classify_web_url_filter_exclusion("crawl", url, filter)
}

#[must_use]
pub fn classify_web_materialization_filter_exclusion(
    url: &str,
    filter: &WebIngestUrlFilter,
) -> Option<WebIngestUrlFilterExclusion> {
    classify_web_url_filter_exclusion("materialization", url, filter)
}

fn classify_web_url_filter_exclusion(
    stage: &str,
    url: &str,
    filter: &WebIngestUrlFilter,
) -> Option<WebIngestUrlFilterExclusion> {
    if let Some(matched) = match_web_ingest_pattern(url, &filter.block_patterns) {
        return Some(WebIngestUrlFilterExclusion {
            detail: format!("{stage}:blocklist:{}", matched.detail),
        });
    }
    (!filter.allow_patterns.is_empty()
        && match_web_ingest_pattern(url, &filter.allow_patterns).is_none())
    .then(|| WebIngestUrlFilterExclusion { detail: format!("{stage}:allowlist:no_match") })
}

fn normalize_web_ingest_patterns(
    patterns: Vec<WebIngestPattern>,
    source: Option<&'static str>,
    field_name: String,
) -> Result<Vec<WebIngestPattern>, String> {
    let mut normalized = Vec::with_capacity(patterns.len());
    let mut seen = std::collections::BTreeSet::<(String, String)>::new();
    for pattern in patterns {
        let kind = normalize_url_filter_pattern_kind(&pattern.kind, &field_name)?;
        let value = normalize_url_filter_pattern_value(&kind, &pattern.value, &field_name)?;
        if seen.insert((kind.clone(), value.clone())) {
            normalized.push(WebIngestPattern { kind, value, source: source.map(str::to_string) });
        }
    }
    Ok(normalized)
}

fn normalize_url_filter_pattern_kind(value: &str, field_name: &str) -> Result<String, String> {
    let normalized = value.trim();
    match normalized {
        "url_prefix" | "path_prefix" | "glob" => Ok(normalized.to_string()),
        _ => Err(format!("{field_name}.kind must be one of: url_prefix, path_prefix, glob")),
    }
}

fn normalize_url_filter_pattern_value(
    kind: &str,
    value: &str,
    field_name: &str,
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
