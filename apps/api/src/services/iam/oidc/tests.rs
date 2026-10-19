use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use tokio::{net::TcpListener, task::JoinHandle};
use url::Url;

use super::{
    mock_idp::{
        DiscoveryScenario, IdTokenScenario, MOCK_AUTHORIZATION_CODE, MOCK_EMAIL, MOCK_SUBJECT,
        MockOidcProvider, MockScenario, UserInfoScenario,
    },
    *,
};

struct BlackholedRedisProbe {
    url: String,
    task: JoinHandle<()>,
}

impl Drop for BlackholedRedisProbe {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn start_blackholed_redis_probe() -> BlackholedRedisProbe {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("test-only blackholed Redis listener should bind");
    let url = format!(
        "redis://{}/",
        listener
            .local_addr()
            .expect("test-only blackholed Redis listener should expose its address")
    );
    let task = tokio::spawn(async move {
        if let Ok((stream, _address)) = listener.accept().await {
            let _stream = stream;
            std::future::pending::<()>().await;
        }
    });

    BlackholedRedisProbe { url, task }
}

async fn prepare_attempt(
    provider: &MockOidcProvider,
    settings: &OidcSettings,
) -> anyhow::Result<OidcAuthorizationRequest> {
    let authorization = begin_authorization(settings)
        .await
        .map_err(|failure| anyhow::anyhow!("begin OIDC authorization failed: {failure:?}"))?;
    provider.expect_exchange(&authorization.pkce_verifier, &authorization.nonce)?;
    Ok(authorization)
}

#[test]
fn provider_identifier_is_issuer_specific_and_stable() {
    let first = issuer_provider_identifier("https://auth.example.test/realms/main");
    let same = issuer_provider_identifier("https://auth.example.test/realms/main");
    let other = issuer_provider_identifier("https://auth.example.test/realms/other");

    assert_eq!(first, same);
    assert_ne!(first, other);
    assert!(first.starts_with("oidc:"));
}

#[test]
fn generated_login_uses_provider_and_subject_not_email() {
    let provider = issuer_provider_identifier("https://auth.example.test/realms/main");

    assert_eq!(
        external_identity_login(&provider, "subject-1"),
        external_identity_login(&provider, "subject-1")
    );
    assert_ne!(
        external_identity_login(&provider, "subject-1"),
        external_identity_login(&provider, "subject-2")
    );
}

#[test]
fn groups_claim_requires_nonblank_string_arrays_and_exact_required_group() {
    let mut claims = OidcAdditionalClaims::default();
    claims.claims.insert("groups".to_string(), json!(["engineering", "ironrag"]));
    assert_eq!(has_required_group(&claims, "ironrag"), Ok(true));
    assert_eq!(has_required_group(&claims, "admin"), Ok(false));

    claims.claims.insert("groups".to_string(), json!(["ironrag", 7]));
    assert_eq!(has_required_group(&claims, "ironrag"), Err(OidcFailure::Claims));
    claims.claims.insert("groups".to_string(), json!([""]));
    assert_eq!(has_required_group(&claims, "ironrag"), Err(OidcFailure::Claims));
    claims.claims.insert("groups".to_string(), json!("ironrag"));
    assert_eq!(has_required_group(&claims, "ironrag"), Err(OidcFailure::Claims));
}

#[test]
fn attempt_key_does_not_embed_url_visible_state() {
    let state = "browser-visible-state";
    let key = attempt_redis_key(state);

    assert!(key.starts_with("ironrag:oidc:attempt:"));
    assert!(!key.contains(state));
}

#[test]
fn login_admission_key_is_issuer_scoped_without_embedding_the_issuer() {
    let issuer = "https://auth.example.test/realms/ironrag";
    let key = oidc_login_admission_redis_key(issuer);

    assert!(key.starts_with("ironrag:oidc:login-admission:v1:{"));
    assert!(!key.contains(issuer));
    assert_ne!(key, oidc_login_admission_redis_key("https://auth.example.test/realms/other"));
}

#[test]
fn local_login_admission_is_no_queue_and_uses_the_process_limit() {
    let redis = redis::Client::open("redis://127.0.0.1:1/")
        .expect("test-only unreachable Redis URL should parse");
    let admission = OidcLoginAdmission::new(redis);
    let cloned_admission = admission.clone();
    let permits = (0..OIDC_LOGIN_MAX_IN_FLIGHT_PER_PROCESS)
        .map(|_| {
            admission
                .try_acquire_local_for_tests()
                .expect("capacity should be available before saturation")
        })
        .collect::<Vec<_>>();

    assert!(matches!(
        cloned_admission.try_acquire_local_for_tests(),
        Err(OidcLoginAdmissionRejection::LocalSaturated)
    ));
    drop(permits);
    assert!(cloned_admission.try_acquire_local_for_tests().is_ok());
}

#[test]
fn callback_login_admission_is_no_queue_and_shared_across_clones() {
    let redis = redis::Client::open("redis://127.0.0.1:1/")
        .expect("test-only unreachable Redis URL should parse");
    let admission = OidcLoginAdmission::with_local_capacity_for_tests(redis, 1);
    let cloned_admission = admission.clone();
    let permit = admission
        .try_acquire_callback_for_tests()
        .expect("callback capacity should be available before saturation");

    assert!(matches!(
        cloned_admission.try_acquire_callback_for_tests(),
        Err(OidcLoginAdmissionRejection::CallbackSaturated)
    ));
    drop(permit);
    assert!(cloned_admission.try_acquire_callback_for_tests().is_ok());
}

#[tokio::test]
async fn blackholed_redis_admission_is_bounded_and_releases_the_local_permit() {
    let probe = start_blackholed_redis_probe().await;
    let redis = redis::Client::open(probe.url.as_str())
        .expect("test-only blackholed Redis URL should parse");
    let admission = OidcLoginAdmission::with_local_capacity_for_tests(redis, 1);
    let outcome = tokio::time::timeout(
        OIDC_REDIS_OPERATION_TIMEOUT + Duration::from_secs(1),
        admission.try_admit("https://issuer.example.test/oidc-admission-blackhole", "203.0.113.10"),
    )
    .await
    .expect("OIDC admission must fail within its bounded Redis timeout");

    assert_eq!(outcome.err(), Some(OidcLoginAdmissionRejection::RedisUnavailable));
    assert!(admission.try_acquire_local_for_tests().is_ok());
}

#[test]
fn subject_validation_rejects_blank_and_control_values() {
    assert!(is_valid_subject("subject-1"));
    assert!(!is_valid_subject("   "));
    assert!(!is_valid_subject("subject\n1"));
}

#[test]
fn discovered_endpoints_require_https_without_userinfo_in_production() {
    assert_eq!(validate_discovered_endpoint("https://id.example.test/oidc/token"), Ok(()));
    assert_eq!(
        validate_discovered_endpoint("http://id.example.test/oidc/token"),
        Err(OidcFailure::Discovery)
    );
    assert_eq!(
        validate_discovered_endpoint("https://operator@id.example.test/oidc/token"),
        Err(OidcFailure::Discovery)
    );
    assert_eq!(
        validate_discovered_endpoint("https://operator:password@id.example.test/oidc/token"),
        Err(OidcFailure::Discovery)
    );
    assert_eq!(validate_discovered_endpoint("https://"), Err(OidcFailure::Discovery));
}

#[test]
fn test_only_loopback_transport_allows_the_local_mock_idp() {
    assert_eq!(validate_discovered_endpoint("http://127.0.0.1:49999/oidc/token"), Ok(()));
    assert_eq!(
        validate_discovered_endpoint("http://localhost:49999/oidc/token"),
        Err(OidcFailure::Discovery)
    );
}

#[tokio::test]
async fn exchange_uses_discovery_jwks_rs256_userinfo_and_pkce() -> anyhow::Result<()> {
    let provider = MockOidcProvider::start(MockScenario::default()).await?;
    let settings = provider.settings();
    let authorization = prepare_attempt(&provider, &settings).await?;
    let authorization_url = Url::parse(&authorization.authorization_url)?;
    let code_challenge = authorization_url
        .query_pairs()
        .find_map(|(name, value)| (name == "code_challenge").then_some(value.into_owned()))
        .ok_or_else(|| anyhow::anyhow!("authorization request omitted a PKCE challenge"))?;
    let code_challenge_method = authorization_url
        .query_pairs()
        .find_map(|(name, value)| (name == "code_challenge_method").then_some(value.into_owned()))
        .ok_or_else(|| anyhow::anyhow!("authorization request omitted a PKCE challenge method"))?;
    assert_eq!(code_challenge_method, "S256");
    assert_eq!(
        code_challenge,
        URL_SAFE_NO_PAD.encode(Sha256::digest(authorization.pkce_verifier.as_bytes()))
    );

    let identity = exchange_code_for_identity(
        &settings,
        MOCK_AUTHORIZATION_CODE,
        &authorization.pkce_verifier,
        &authorization.nonce,
    )
    .await
    .map_err(|failure| anyhow::anyhow!("OIDC exchange failed: {failure:?}"))?;

    assert_eq!(identity.subject, MOCK_SUBJECT);
    assert_eq!(identity.email, MOCK_EMAIL);
    assert_eq!(identity.display_name, MOCK_EMAIL);
    let captured = provider.captured_token_request()?;
    assert_eq!(captured.code, MOCK_AUTHORIZATION_CODE);
    assert_eq!(captured.pkce_verifier, authorization.pkce_verifier);
    assert!(captured.client_authenticated);
    assert!(provider.metrics().discovery() >= 2);
    assert!(provider.metrics().jwks() >= 2);
    assert_eq!(provider.metrics().token(), 1);
    assert_eq!(provider.metrics().userinfo(), 1);

    Ok(())
}

#[tokio::test]
async fn exchange_rejects_invalid_signed_id_token_claims() -> anyhow::Result<()> {
    let scenarios = [
        IdTokenScenario::InvalidIssuer,
        IdTokenScenario::InvalidSignature,
        IdTokenScenario::InvalidAudience,
        IdTokenScenario::Expired,
        IdTokenScenario::InvalidNonce,
    ];

    for id_token in scenarios {
        let provider =
            MockOidcProvider::start(MockScenario { id_token, ..MockScenario::default() }).await?;
        let settings = provider.settings();
        let authorization = prepare_attempt(&provider, &settings).await?;
        let result = exchange_code_for_identity(
            &settings,
            MOCK_AUTHORIZATION_CODE,
            &authorization.pkce_verifier,
            &authorization.nonce,
        )
        .await;

        assert_eq!(result.err(), Some(OidcFailure::IdTokenVerification), "{id_token:?}");
    }

    Ok(())
}

#[tokio::test]
async fn exchange_rejects_userinfo_subject_email_and_group_violations() -> anyhow::Result<()> {
    let scenarios = [
        (UserInfoScenario::SubjectMismatch, OidcFailure::UserInfo),
        (UserInfoScenario::UnverifiedEmail, OidcFailure::Claims),
        (UserInfoScenario::BlankEmail, OidcFailure::Claims),
        (UserInfoScenario::MissingGroup, OidcFailure::Claims),
        (UserInfoScenario::WrongGroup, OidcFailure::Claims),
        (UserInfoScenario::MalformedGroup, OidcFailure::Claims),
        (UserInfoScenario::ScalarGroup, OidcFailure::Claims),
    ];

    for (userinfo, expected_failure) in scenarios {
        let provider =
            MockOidcProvider::start(MockScenario { userinfo, ..MockScenario::default() }).await?;
        let settings = provider.settings();
        let authorization = prepare_attempt(&provider, &settings).await?;
        let result = exchange_code_for_identity(
            &settings,
            MOCK_AUTHORIZATION_CODE,
            &authorization.pkce_verifier,
            &authorization.nonce,
        )
        .await;

        assert_eq!(result.err(), Some(expected_failure), "{userinfo:?}");
    }

    Ok(())
}

#[tokio::test]
async fn discovery_redirect_is_refused_without_contacting_redirect_target() -> anyhow::Result<()> {
    let provider = MockOidcProvider::start(MockScenario {
        discovery: DiscoveryScenario::Redirect,
        ..MockScenario::default()
    })
    .await?;
    let settings = provider.settings();
    let result = begin_authorization(&settings).await;

    assert_eq!(result.err(), Some(OidcFailure::Discovery));
    assert_eq!(provider.metrics().discovery(), 1);
    assert_eq!(provider.metrics().redirected_discovery_target(), 0);

    Ok(())
}
