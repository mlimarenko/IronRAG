use super::{
    DEFAULT_WEB_CRAWL_DEPTH, DEFAULT_WEB_CRAWL_MAX_PAGES, MAX_WEB_CRAWL_DEPTH,
    MAX_WEB_CRAWL_MAX_PAGES, WebBoundaryPolicy, WebClassificationReason, WebIngestPattern,
    WebIngestPolicy, WebIngestUrlFilter, WebRunCounts, WebRunFailureCode, WebRunState,
    classify_web_crawl_filter_exclusion, classify_web_materialization_filter_exclusion,
    confluence_web_ingest_policy, default_web_ingest_policy, derive_terminal_run_state,
    match_web_ingest_pattern, parse_web_boundary_policy, validate_web_boundary_seed_host,
    validate_web_ingest_policy, validate_web_ingest_url_filter, validate_web_run_settings,
};
use crate::shared::web::url_identity::HostClassification;

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
fn generic_web_ingest_policy_has_no_product_specific_url_rules() {
    let policy = default_web_ingest_policy();

    assert!(policy.crawl_filter.allow_patterns.is_empty());
    assert!(policy.crawl_filter.block_patterns.is_empty());
    assert!(policy.materialization_filter.allow_patterns.is_empty());
    assert!(policy.materialization_filter.block_patterns.is_empty());
}

#[test]
fn confluence_url_rules_are_available_only_through_an_explicit_profile() {
    let generic_policy = default_web_ingest_policy();
    let confluence_policy = confluence_web_ingest_policy();
    let login_url = "https://docs.example.test/login.action";

    assert!(
        match_web_ingest_pattern(login_url, &generic_policy.crawl_filter.block_patterns).is_none()
    );
    assert!(
        match_web_ingest_pattern(login_url, &confluence_policy.crawl_filter.block_patterns)
            .is_some()
    );
    assert!(confluence_policy.materialization_filter.block_patterns.is_empty());
}

#[test]
fn subdomain_boundary_policy_uses_supported_runtime_vocabulary() {
    let settings = match validate_web_run_settings(
        "recursive_crawl",
        Some("same_host_and_subdomains"),
        None,
        None,
    ) {
        Ok(settings) => settings,
        Err(error) => panic!("validate settings: {error}"),
    };
    assert_eq!(settings.boundary_policy, "same_host_and_subdomains");
    assert!(
        WebBoundaryPolicy::SameHostAndSubdomains
            .allows_host_classification(&HostClassification::SameHost)
    );
    assert!(
        WebBoundaryPolicy::SameHostAndSubdomains
            .allows_host_classification(&HostClassification::SeedSubdomain)
    );
    assert!(
        !WebBoundaryPolicy::SameHostAndSubdomains
            .allows_host_classification(&HostClassification::External)
    );
    assert!(
        parse_web_boundary_policy("same_host_and_subdomains")
            .expect("boundary policy parse")
            .allows_host_classification(&HostClassification::SeedSubdomain)
    );
}

#[test]
fn subdomain_boundary_rejects_ip_seed_hosts() {
    assert!(
        validate_web_boundary_seed_host("same_host_and_subdomains", "https://192.0.2.10/docs",)
            .is_err()
    );
    assert!(
        validate_web_boundary_seed_host("same_host_and_subdomains", "https://example.com/docs",)
            .is_ok()
    );
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
fn excluded_only_run_is_completed_partial() {
    let counts = WebRunCounts { excluded: 1, ..WebRunCounts::default() };
    assert_eq!(derive_terminal_run_state(&counts), WebRunState::CompletedPartial);
}

#[test]
fn recursive_limits_are_bounded() {
    assert!(
        validate_web_run_settings(
            "recursive_crawl",
            None,
            Some(MAX_WEB_CRAWL_DEPTH + 1),
            Some(DEFAULT_WEB_CRAWL_MAX_PAGES),
        )
        .is_err()
    );
    assert!(
        validate_web_run_settings(
            "recursive_crawl",
            None,
            Some(DEFAULT_WEB_CRAWL_DEPTH),
            Some(MAX_WEB_CRAWL_MAX_PAGES + 1),
        )
        .is_err()
    );
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
            "duplicate_\u{63}anonical_url",
            "duplicate_content",
            "outside_boundary_policy",
            "exceeded_max_depth",
            "exceeded_max_pages",
            "url_filter",
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
        crawl_filter: WebIngestUrlFilter {
            allow_patterns: Vec::new(),
            block_patterns: vec![
                WebIngestPattern {
                    kind: " path_prefix ".to_string(),
                    value: " /labels/viewlabel.action ".to_string(),
                    source: Some("ignored".to_string()),
                },
                WebIngestPattern {
                    kind: "path_prefix".to_string(),
                    value: "/labels/viewlabel.action".to_string(),
                    source: None,
                },
            ],
        },
        materialization_filter: WebIngestUrlFilter {
            allow_patterns: vec![WebIngestPattern {
                kind: "glob".to_string(),
                value: "*/docs/*".to_string(),
                source: Some("ignored".to_string()),
            }],
            block_patterns: Vec::new(),
        },
    })
    .expect("policy should validate");

    assert_eq!(
        policy.crawl_filter.block_patterns,
        vec![WebIngestPattern {
            kind: "path_prefix".to_string(),
            value: "/labels/viewlabel.action".to_string(),
            source: None,
        }]
    );
    assert_eq!(policy.materialization_filter.allow_patterns[0].source, None);
}

#[test]
fn validates_run_url_filter_with_sources() {
    let filter = validate_web_ingest_url_filter(
        WebIngestUrlFilter {
            allow_patterns: Vec::new(),
            block_patterns: vec![WebIngestPattern {
                kind: "glob".to_string(),
                value: "*/print/*".to_string(),
                source: None,
            }],
        },
        "crawlFilter",
    )
    .expect("filter should validate");

    assert_eq!(filter.block_patterns[0].source.as_deref(), Some("run"));
}

#[test]
fn matches_url_filter_patterns_by_url_path_and_glob() {
    let patterns = vec![
        WebIngestPattern {
            kind: "path_prefix".to_string(),
            value: "/labels/viewlabel.action".to_string(),
            source: Some("library".to_string()),
        },
        WebIngestPattern {
            kind: "url_prefix".to_string(),
            value: "https://docs.example.test/archive/".to_string(),
            source: Some("run".to_string()),
        },
        WebIngestPattern {
            kind: "glob".to_string(),
            value: "*permissionviolation=*".to_string(),
            source: Some("library".to_string()),
        },
    ];

    assert_eq!(
        match_web_ingest_pattern(
            "https://docs.example.test/labels/viewlabel.action?key=ABC",
            &patterns,
        )
        .map(|matched| matched.detail),
        Some("library:path_prefix:/labels/viewlabel.action".to_string())
    );
    assert!(
        match_web_ingest_pattern("https://docs.example.test/archive/old-page", &patterns,)
            .is_some()
    );
    assert!(
        match_web_ingest_pattern(
            "https://docs.example.test/login.action?permissionviolation=true",
            &patterns,
        )
        .is_some()
    );
}

#[test]
fn classifies_allowlist_non_matches_as_excluded_after_blocklist() {
    let filter = WebIngestUrlFilter {
        allow_patterns: vec![WebIngestPattern {
            kind: "path_prefix".to_string(),
            value: "/docs/".to_string(),
            source: Some("run".to_string()),
        }],
        block_patterns: vec![WebIngestPattern {
            kind: "path_prefix".to_string(),
            value: "/docs/archive/".to_string(),
            source: Some("run".to_string()),
        }],
    };

    assert!(
        classify_web_materialization_filter_exclusion("https://example.test/docs/a", &filter)
            .is_none()
    );
    assert_eq!(
        classify_web_materialization_filter_exclusion(
            "https://example.test/docs/archive/old",
            &filter
        )
        .map(|excluded| excluded.detail),
        Some("materialization:blocklist:run:path_prefix:/docs/archive/".to_string())
    );
    assert_eq!(
        classify_web_materialization_filter_exclusion("https://example.test/admin", &filter)
            .map(|excluded| excluded.detail),
        Some("materialization:allowlist:no_match".to_string())
    );
}

#[test]
fn crawl_and_materialization_filters_are_independent() {
    let crawl_filter = WebIngestUrlFilter {
        allow_patterns: vec![WebIngestPattern {
            kind: "path_prefix".to_string(),
            value: "/docs/".to_string(),
            source: Some("run".to_string()),
        }],
        block_patterns: vec![WebIngestPattern {
            kind: "path_prefix".to_string(),
            value: "/docs/archive/".to_string(),
            source: Some("run".to_string()),
        }],
    };
    let filter = WebIngestUrlFilter {
        allow_patterns: vec![WebIngestPattern {
            kind: "path_prefix".to_string(),
            value: "/content/page".to_string(),
            source: Some("run".to_string()),
        }],
        block_patterns: Vec::new(),
    };

    assert_eq!(
        classify_web_crawl_filter_exclusion("https://example.test/", &crawl_filter)
            .map(|excluded| excluded.detail),
        Some("crawl:allowlist:no_match".to_string())
    );
    assert_eq!(
        classify_web_crawl_filter_exclusion("https://example.test/docs/archive/x", &crawl_filter)
            .map(|excluded| excluded.detail),
        Some("crawl:blocklist:run:path_prefix:/docs/archive/".to_string())
    );
    assert_eq!(
        classify_web_materialization_filter_exclusion("https://example.test/", &filter)
            .map(|excluded| excluded.detail),
        Some("materialization:allowlist:no_match".to_string())
    );
}
