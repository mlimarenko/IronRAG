use uuid::Uuid;

use super::support::configured_api_key_summary;
use super::{AiScopeKind, CreateAiAccountRequest, UpdateAiAccountRequest};

#[test]
fn account_request_debug_redacts_key_and_untrusted_base_url() {
    let create = CreateAiAccountRequest {
        scope_kind: AiScopeKind::Instance,
        workspace_id: None,
        library_id: None,
        provider_catalog_id: Uuid::now_v7(),
        label: "synthetic".into(),
        api_key: Some("provider-key-regression".into()),
        base_url: Some("https://user:url-secret@host.example/?token=query-secret".into()),
    };
    let update = UpdateAiAccountRequest {
        label: "synthetic".into(),
        api_key: Some("updated-key-regression".into()),
        base_url: Some("https://host.example/?token=updated-query-secret".into()),
        credential_state: "active".into(),
        expected_updated_at: chrono::Utc::now(),
    };

    let debug = format!("{create:?} {update:?}");
    for secret in [
        "provider-key-regression",
        "updated-key-regression",
        "url-secret",
        "query-secret",
        "updated-query-secret",
    ] {
        assert!(!debug.contains(secret), "Debug exposed {secret}");
    }
}

#[test]
fn account_key_summary_never_discloses_key_fragments() {
    assert_eq!(configured_api_key_summary(true), "configured");
    assert_eq!(configured_api_key_summary(false), "not_configured");
}
