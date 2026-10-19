//! Test-only resolved-binding fixtures shared by the decision-provider suites.
//!
//! The semantic-rerank dispatch tests and the graph entity-typing tests both
//! build a TypeSafe SystemOne binding; this module owns the common shape so the
//! two suites cannot drift apart.

use super::*;

/// Common shape of a TypeSafe SystemOne decision binding. A caller overrides
/// only the binding purpose and the provider profile it needs.
pub(crate) fn systemone_decision_binding_base(base_url: &str) -> ResolvedRuntimeBinding {
    ResolvedRuntimeBinding {
        binding_id: Uuid::now_v7(),
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        binding_purpose: AiBindingPurpose::GraphEntityType,
        provider_catalog_id: Uuid::now_v7(),
        provider_kind: "typesafe".to_string(),
        provider_base_url: Some(base_url.to_string()),
        provider_api_style: "typesafe_system_one".to_string(),
        account_id: Uuid::now_v7(),
        api_key: Some("test-dummy-api-key-not-a-secret".to_string()),
        model_catalog_id: Uuid::now_v7(),
        model_name: "jev-latest".to_string(),
        effective_embedding_dimensions: None,
        system_prompt: None,
        temperature: None,
        top_p: None,
        max_output_tokens_override: None,
        extra_parameters_json: serde_json::json!({
            "_providerProfile": {
                "runtime": {"kind": "typesafe_system_one"},
                "baseUrl": {"allowPrivateNetwork": true},
            }
        }),
    }
}
