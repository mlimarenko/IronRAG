use chrono::Utc;
use uuid::Uuid;

use super::bootstrap_support::validate_email;
use super::*;
use crate::domains::iam::ApiToken;

#[test]
fn validates_required_bootstrap_email() {
    assert_eq!(validate_email(" owner@example.com ").expect("valid email"), "owner@example.com");
    assert!(validate_email("owner").is_err());
    assert!(validate_email("owner@localhost").is_err());
}

#[test]
fn iam_commands_and_outcomes_redact_all_plaintext_credentials() {
    let bootstrap = BootstrapSetupCommand {
        login: "owner".into(),
        email: "owner@example.com".into(),
        display_name: None,
        password: "bootstrap-password-regression".into(),
        ai_setup: Some(BootstrapSetupAiCommand {
            provider_kind: "synthetic".into(),
            api_key: Some("provider-key-regression".into()),
            base_url: Some("https://user:url-secret@host.example/?token=query-secret".into()),
        }),
        request_id: "request".into(),
    };
    let login = AuthenticateSessionCommand {
        login: "owner".into(),
        password: "login-password-regression".into(),
        source_key: "203.0.113.7".into(),
    };
    let session = AuthenticateSessionOutcome {
        session_id: Uuid::now_v7(),
        session_secret: "session-secret-regression".into(),
        principal_id: Uuid::now_v7(),
        login: "owner".into(),
        email: "owner@example.com".into(),
        display_name: "Owner".into(),
        expires_at: Utc::now(),
    };
    let token = MintApiTokenOutcome {
        token: "raw-api-token-regression".into(),
        api_token: ApiToken {
            principal_id: Uuid::now_v7(),
            workspace_id: None,
            label: "synthetic".into(),
            token_prefix: "prefix".into(),
            status: "active".into(),
            expires_at: None,
        },
    };

    let debug = format!("{bootstrap:?} {login:?} {session:?} {token:?}");
    for secret in [
        "bootstrap-password-regression",
        "provider-key-regression",
        "url-secret",
        "query-secret",
        "login-password-regression",
        "session-secret-regression",
        "raw-api-token-regression",
    ] {
        assert!(!debug.contains(secret), "Debug exposed {secret}");
    }
}
