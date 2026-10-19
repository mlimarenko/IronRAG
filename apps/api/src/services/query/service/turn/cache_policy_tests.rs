use super::test_prelude::*;

#[test]
fn result_cache_binding_identity_is_stable_across_rerank_rollout_modes() {
    use crate::domains::query::SemanticRerankMode;

    let expected = &[
        AiBindingPurpose::QueryCompile,
        AiBindingPurpose::QueryRerank,
        AiBindingPurpose::ExtractGraph,
        AiBindingPurpose::QueryAnswer,
    ];
    assert_eq!(query_result_binding_purposes(SemanticRerankMode::Off), expected);
    assert_eq!(query_result_binding_purposes(SemanticRerankMode::Shadow), expected);
    assert_eq!(query_result_binding_purposes(SemanticRerankMode::Active), expected);
    assert!(query_result_cache_enabled_for_semantic_rerank(SemanticRerankMode::Off));
    assert!(
        query_result_cache_enabled_for_semantic_rerank(SemanticRerankMode::Shadow),
        "shadow rollout must not force every repeated answer through the paid query pipeline",
    );
    assert!(query_result_cache_enabled_for_semantic_rerank(SemanticRerankMode::Active));
}
