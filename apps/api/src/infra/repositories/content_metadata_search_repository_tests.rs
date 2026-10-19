use super::metadata_search_terms;

#[test]
fn metadata_search_terms_extracts_filename_token_from_mixed_query() {
    let terms = metadata_search_terms("audit_repository.rs filters events");
    assert!(terms.generic.iter().any(|term| term == "audit_repository.rs"));
    assert!(terms.generic.iter().any(|term| term == "filters"));
    assert!(terms.generic.iter().any(|term| term == "events"));
}

#[test]
fn metadata_search_terms_normalizes_unicode_and_deduplicates() {
    let terms = metadata_search_terms("AUDIT_REPOSITORY.RS CAFÉ café");
    assert!(terms.generic.iter().any(|term| term == "audit_repository.rs"));
    assert_eq!(terms.generic.iter().filter(|term| term.as_str() == "café").count(), 1);
}

#[test]
fn metadata_search_terms_extracts_version_prefix_with_word_context() {
    let terms = metadata_search_terms("\"Version 7.8.\" \"Alpha Suite Administrator Guide\"");
    assert!(terms.version.iter().any(|term| term == "7.8"));
}

#[test]
fn metadata_search_terms_requires_context_for_two_part_numbers() {
    let terms = metadata_search_terms("1.2");
    assert!(terms.version.is_empty());
    let terms = metadata_search_terms("Alpha 1.2");
    assert_eq!(terms.version, vec!["1.2"]);
}
