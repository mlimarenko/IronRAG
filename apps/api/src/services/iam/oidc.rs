//! Strict server-side OpenID Connect helpers for the browser Authorization
//! Code + PKCE flow. This module deliberately contains no route policy or
//! redirect response construction, so the cryptographic identity proof is
//! independently testable from the HTTP surface.

use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc, time::Duration};

use openidconnect::core::{
    CoreAuthenticationFlow, CoreClient, CoreGenderClaim, CoreProviderMetadata,
};
use openidconnect::reqwest;
use openidconnect::{
    AdditionalClaims, AsyncHttpClient, AuthorizationCode, ClientId, ClientSecret, CsrfToken,
    EndpointNotSet, EndpointSet, HttpClientError, HttpRequest, HttpResponse, IssuerUrl, Nonce,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
    TokenUrl, UserInfoClaims, UserInfoUrl,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use url::Url;

use crate::app::config::OidcSettings;

pub(crate) const OIDC_ATTEMPT_TTL_SECONDS: u64 = 300;
/// Maximum simultaneous OIDC login starts handled by one API process.
pub(crate) const OIDC_LOGIN_MAX_IN_FLIGHT_PER_PROCESS: usize = 4;
/// Maximum simultaneous OIDC callbacks handled by one API process.
pub(crate) const OIDC_CALLBACK_MAX_IN_FLIGHT_PER_PROCESS: usize = 4;
/// Shared Redis token-bucket refill rate for one configured OIDC issuer.
pub(crate) const OIDC_LOGIN_CLUSTER_STARTS_PER_SECOND: u64 = 2;
/// Shared Redis token-bucket capacity for one configured OIDC issuer.
pub(crate) const OIDC_LOGIN_CLUSTER_BURST: u64 = 8;
/// Per-source Redis token-bucket refill rate for one OIDC login origin.
///
/// Below the issuer-wide refill rate on purpose: a single origin can then
/// never spend the shared bucket faster than it refills, so a flood from one
/// address cannot starve every other address of sign-in.
pub(crate) const OIDC_LOGIN_SOURCE_STARTS_PER_SECOND: u64 = 1;
/// Per-source Redis token-bucket capacity for one OIDC login origin.
///
/// A browser sends one start per sign-in click; the second token absorbs an
/// immediate reload. It is deliberately far below `OIDC_LOGIN_CLUSTER_BURST`
/// so one source's burst leaves the shared bucket usable by everyone else.
pub(crate) const OIDC_LOGIN_SOURCE_BURST: u64 = 2;
const MAX_OIDC_CALLBACK_CODE_BYTES: usize = 8_192;
const MAX_OIDC_SUBJECT_BYTES: usize = 1_024;
const OIDC_HTTP_TIMEOUT: Duration = Duration::from_secs(15);
/// Bound for Redis operations that sit on an OIDC admission permit.
pub(crate) const OIDC_REDIS_OPERATION_TIMEOUT: Duration = Duration::from_secs(2);
const OIDC_LOGIN_TOKEN_SCALE: u64 = 1_000_000;
const OIDC_LOGIN_CLUSTER_RATE_UNITS_PER_SECOND: u64 =
    OIDC_LOGIN_CLUSTER_STARTS_PER_SECOND * OIDC_LOGIN_TOKEN_SCALE;
const OIDC_LOGIN_CLUSTER_BURST_UNITS: u64 = OIDC_LOGIN_CLUSTER_BURST * OIDC_LOGIN_TOKEN_SCALE;
const OIDC_LOGIN_BUCKET_TTL_SECONDS: u64 =
    OIDC_LOGIN_CLUSTER_BURST.div_ceil(OIDC_LOGIN_CLUSTER_STARTS_PER_SECOND) + 1;
const OIDC_LOGIN_SOURCE_RATE_UNITS_PER_SECOND: u64 =
    OIDC_LOGIN_SOURCE_STARTS_PER_SECOND * OIDC_LOGIN_TOKEN_SCALE;
const OIDC_LOGIN_SOURCE_BURST_UNITS: u64 = OIDC_LOGIN_SOURCE_BURST * OIDC_LOGIN_TOKEN_SCALE;
const OIDC_LOGIN_SOURCE_BUCKET_TTL_SECONDS: u64 =
    OIDC_LOGIN_SOURCE_BURST.div_ceil(OIDC_LOGIN_SOURCE_STARTS_PER_SECOND) + 1;
/// Atomic two-bucket admission: the caller's own source bucket and the
/// issuer-wide one.
///
/// The source axis is evaluated first and, when it is out of tokens, the
/// cluster bucket is returned untouched with `-2`: the origin that floods
/// spends its own budget rather than the shared one, which is what keeps a
/// single address from turning its own flood into everyone's outage.
/// Returning `0` means the shared bucket is empty -- the aggregate case the
/// source axis cannot see. Any malformed stored state returns `-1` and fails
/// the whole admission closed. Both keys carry the issuer hash tag so a
/// Redis Cluster can evaluate the script in one slot.
const OIDC_LOGIN_BUCKET_ADMISSION_SCRIPT: &str = r#"
local now = redis.call('TIME')
local now_micros = tonumber(now[1]) * 1000000 + tonumber(now[2])
local cluster_rate_units_per_second = tonumber(ARGV[1])
local cluster_burst_units = tonumber(ARGV[2])
local cluster_ttl_seconds = tonumber(ARGV[3])
local source_rate_units_per_second = tonumber(ARGV[4])
local source_burst_units = tonumber(ARGV[5])
local source_ttl_seconds = tonumber(ARGV[6])

-- Returns the token count and the bucket's own effective timestamp, or
-- nil for state that cannot be trusted. The shared `now_micros` is never
-- advanced: with a timestamp from the future, only that bucket clamps its
-- own clock, so evaluating one bucket can never grant the other a refill it
-- did not earn.
local function evaluate_bucket(key, rate_units_per_second, burst_units)
  local tokens_raw = redis.call('HGET', key, 'tokens')
  local updated_raw = redis.call('HGET', key, 'updated_at_micros')
  local tokens
  local updated_at_micros

  if tokens_raw or updated_raw then
    if not tokens_raw or not updated_raw then
      return nil, nil
    end
    tokens = tonumber(tokens_raw)
    updated_at_micros = tonumber(updated_raw)
    if not tokens or not updated_at_micros
      or tokens < 0 or tokens > burst_units
      or tokens ~= math.floor(tokens)
      or updated_at_micros < 0
      or updated_at_micros ~= math.floor(updated_at_micros) then
      return nil, nil
    end
  elseif redis.call('EXISTS', key) == 1 then
    -- A hash that exists without its fields is corrupt, not fresh.
    return nil, nil
  else
    tokens = burst_units
    updated_at_micros = now_micros
  end

  local effective_micros = now_micros
  if effective_micros < updated_at_micros then
    effective_micros = updated_at_micros
  end
  local elapsed_micros = effective_micros - updated_at_micros
  local replenished_units = math.floor(elapsed_micros * rate_units_per_second / 1000000)
  return math.min(burst_units, tokens + replenished_units), effective_micros
end

local cluster_tokens, cluster_effective_micros =
  evaluate_bucket(KEYS[1], cluster_rate_units_per_second, cluster_burst_units)
local source_tokens, source_effective_micros =
  evaluate_bucket(KEYS[2], source_rate_units_per_second, source_burst_units)
if not cluster_tokens or not source_tokens then
  return -1
end

if source_tokens < 1000000 then
  return -2
end
if cluster_tokens < 1000000 then
  return 0
end

redis.call(
  'HSET', KEYS[1],
  'tokens', cluster_tokens - 1000000,
  'updated_at_micros', cluster_effective_micros
)
redis.call('EXPIRE', KEYS[1], cluster_ttl_seconds)
redis.call(
  'HSET', KEYS[2],
  'tokens', source_tokens - 1000000,
  'updated_at_micros', source_effective_micros
)
redis.call('EXPIRE', KEYS[2], source_ttl_seconds)
return 1
"#;

/// Local and cluster-wide admission for expensive OIDC browser work.
///
/// The start and callback semaphores deliberately have no queue: a saturated
/// API worker rejects immediately instead of retaining browser requests while
/// discovery, token exchange, or provisioning is slow. A successful start
/// acquisition is then charged to two Redis buckets whose clock is Redis
/// `TIME`: the caller's own source bucket and the issuer-wide one. Keeping
/// the source budget strictly below the shared budget is what stops one
/// anonymous origin from turning the issuer-wide bucket into an outage for
/// every other origin (IRG-91).
#[derive(Clone)]
pub(crate) struct OidcLoginAdmission {
    redis: redis::Client,
    login_in_flight: Arc<Semaphore>,
    callback_in_flight: Arc<Semaphore>,
}

/// The internal reason for a fail-closed OIDC login admission rejection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OidcLoginAdmissionRejection {
    /// This API process is already handling the configured number of starts.
    LocalSaturated,
    /// This API process is already handling the configured number of callbacks.
    CallbackSaturated,
    /// The issuer-wide Redis bucket has no token left.
    ClusterRateLimited,
    /// This source's own Redis bucket has no token left; the issuer-wide
    /// bucket was returned untouched.
    SourceRateLimited,
    /// Redis could not atomically evaluate a trustworthy bucket decision.
    RedisUnavailable,
}

impl OidcLoginAdmission {
    /// Builds the production admission guard with the fixed safe defaults.
    #[must_use]
    pub(crate) fn new(redis: redis::Client) -> Self {
        Self {
            redis,
            login_in_flight: Arc::new(Semaphore::new(OIDC_LOGIN_MAX_IN_FLIGHT_PER_PROCESS)),
            callback_in_flight: Arc::new(Semaphore::new(OIDC_CALLBACK_MAX_IN_FLIGHT_PER_PROCESS)),
        }
    }

    /// Attempts a no-queue local acquisition followed by an atomic
    /// two-bucket Redis charge: the caller's source bucket and the
    /// issuer-wide one. The returned permit must stay alive through provider
    /// discovery and attempt persistence.
    pub(crate) async fn try_admit(
        &self,
        issuer_url: &str,
        source_key: &str,
    ) -> Result<OwnedSemaphorePermit, OidcLoginAdmissionRejection> {
        let permit = self
            .login_in_flight
            .clone()
            .try_acquire_owned()
            .map_err(|_| OidcLoginAdmissionRejection::LocalSaturated)?;
        self.consume_admission_tokens(issuer_url, source_key).await?;
        Ok(permit)
    }

    /// Attempts a no-queue permit for the expensive callback exchange and
    /// provisioning path. A callback state is already issued only after a
    /// successful start admission, so this does not charge the Redis start
    /// bucket a second time.
    pub(crate) fn try_admit_callback(
        &self,
    ) -> Result<OwnedSemaphorePermit, OidcLoginAdmissionRejection> {
        self.callback_in_flight
            .clone()
            .try_acquire_owned()
            .map_err(|_| OidcLoginAdmissionRejection::CallbackSaturated)
    }

    async fn consume_admission_tokens(
        &self,
        issuer_url: &str,
        source_key: &str,
    ) -> Result<(), OidcLoginAdmissionRejection> {
        let mut connection = tokio::time::timeout(
            OIDC_REDIS_OPERATION_TIMEOUT,
            self.redis.get_multiplexed_async_connection(),
        )
        .await
        .map_err(|_| OidcLoginAdmissionRejection::RedisUnavailable)?
        .map_err(|_| OidcLoginAdmissionRejection::RedisUnavailable)?;
        let decision: i64 = tokio::time::timeout(
            OIDC_REDIS_OPERATION_TIMEOUT,
            redis::cmd("EVAL")
                .arg(OIDC_LOGIN_BUCKET_ADMISSION_SCRIPT)
                .arg(2)
                .arg(oidc_login_admission_redis_key(issuer_url))
                .arg(oidc_login_admission_source_redis_key(issuer_url, source_key))
                .arg(OIDC_LOGIN_CLUSTER_RATE_UNITS_PER_SECOND)
                .arg(OIDC_LOGIN_CLUSTER_BURST_UNITS)
                .arg(OIDC_LOGIN_BUCKET_TTL_SECONDS)
                .arg(OIDC_LOGIN_SOURCE_RATE_UNITS_PER_SECOND)
                .arg(OIDC_LOGIN_SOURCE_BURST_UNITS)
                .arg(OIDC_LOGIN_SOURCE_BUCKET_TTL_SECONDS)
                .query_async(&mut connection),
        )
        .await
        .map_err(|_| OidcLoginAdmissionRejection::RedisUnavailable)?
        .map_err(|_| OidcLoginAdmissionRejection::RedisUnavailable)?;
        match decision {
            1 => Ok(()),
            0 => Err(OidcLoginAdmissionRejection::ClusterRateLimited),
            -2 => Err(OidcLoginAdmissionRejection::SourceRateLimited),
            _ => Err(OidcLoginAdmissionRejection::RedisUnavailable),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_local_capacity_for_tests(redis: redis::Client, capacity: usize) -> Self {
        Self {
            redis,
            login_in_flight: Arc::new(Semaphore::new(capacity)),
            callback_in_flight: Arc::new(Semaphore::new(capacity)),
        }
    }

    #[cfg(test)]
    pub(crate) fn try_acquire_local_for_tests(
        &self,
    ) -> Result<OwnedSemaphorePermit, OidcLoginAdmissionRejection> {
        self.login_in_flight
            .clone()
            .try_acquire_owned()
            .map_err(|_| OidcLoginAdmissionRejection::LocalSaturated)
    }

    #[cfg(test)]
    pub(crate) fn try_acquire_callback_for_tests(
        &self,
    ) -> Result<OwnedSemaphorePermit, OidcLoginAdmissionRejection> {
        self.callback_in_flight
            .clone()
            .try_acquire_owned()
            .map_err(|_| OidcLoginAdmissionRejection::CallbackSaturated)
    }
}

/// Typed OIDC endpoints after strict discovery.
type OidcCoreClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointSet,
    EndpointSet,
>;

struct DiscoveredOidcClient {
    client: OidcCoreClient,
    http_client: StrictOidcHttpClient,
}

/// A request guard around the OIDC client transport. Discovery is provider
/// supplied, so all URLs it returns must be screened before use rather than
/// trusting the provider metadata as a routing authority.
struct StrictOidcHttpClient {
    inner: reqwest::Client,
}

#[derive(Debug, thiserror::Error)]
enum StrictOidcHttpClientError {
    #[error("OIDC endpoint URL is not permitted")]
    Endpoint,
    #[error("OIDC HTTP request failed")]
    Request(#[source] HttpClientError<reqwest::Error>),
}

impl StrictOidcHttpClient {
    fn new() -> Result<Self, reqwest::Error> {
        let inner = reqwest::ClientBuilder::new()
            // Discovery, JWKS, token and userinfo requests may never follow
            // an arbitrary redirect into another trust boundary.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(OIDC_HTTP_TIMEOUT)
            .build()?;
        Ok(Self { inner })
    }
}

impl<'client> AsyncHttpClient<'client> for StrictOidcHttpClient {
    type Error = StrictOidcHttpClientError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, Self::Error>> + Send + 'client>>;

    fn call(&'client self, request: HttpRequest) -> Self::Future {
        Box::pin(async move {
            validate_discovered_endpoint(request.uri().to_string().as_str())
                .map_err(|_| StrictOidcHttpClientError::Endpoint)?;
            AsyncHttpClient::call(&self.inner, request)
                .await
                .map_err(StrictOidcHttpClientError::Request)
        })
    }
}

/// A redirect target produced for the browser login step.
pub(crate) struct OidcAuthorizationRequest {
    pub authorization_url: String,
    pub state: String,
    pub pkce_verifier: String,
    pub nonce: String,
}

/// One server-side, single-use browser login attempt persisted in Redis.
///
/// The verifier and nonce are never sent back to the browser. The cookie
/// binding is separately random so a leaked URL state cannot complete a
/// login from a different browser.
#[derive(Serialize, Deserialize)]
pub(crate) struct OidcLoginAttempt {
    pub cookie_binding: String,
    pub pkce_verifier: String,
    pub nonce: String,
    pub return_path: String,
}

/// Identity claims that have completed cryptographic verification, userinfo
/// subject matching, verified-email policy, and required-group admission.
pub(crate) struct VerifiedOidcIdentity {
    pub subject: String,
    pub email: String,
    pub display_name: String,
}

/// Intentionally coarse failure classes. OIDC endpoints log only their stage,
/// never provider response data, codes, tokens, or raw IdP error messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OidcFailure {
    Discovery,
    TokenExchange,
    IdTokenVerification,
    UserInfo,
    Claims,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct OidcAdditionalClaims {
    #[serde(flatten)]
    claims: HashMap<String, serde_json::Value>,
}

impl AdditionalClaims for OidcAdditionalClaims {}

/// Starts a strict Authorization Code + PKCE login request.
pub(crate) async fn begin_authorization(
    settings: &OidcSettings,
) -> Result<OidcAuthorizationRequest, OidcFailure> {
    let discovered = discover_client(settings).await?;
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();
    let request = discovered
        .client
        .authorize_url(
            CoreAuthenticationFlow::AuthorizationCode,
            CsrfToken::new_random,
            Nonce::new_random,
        )
        // `openid` is injected by the library. Request standard claims used
        // from userinfo; groups are a configured Keycloak protocol mapper.
        .add_scope(Scope::new("email".to_string()))
        .add_scope(Scope::new("profile".to_string()))
        .set_pkce_challenge(pkce_challenge);
    let (authorization_url, state, nonce) = request.url();

    Ok(OidcAuthorizationRequest {
        authorization_url: authorization_url.to_string(),
        state: state.secret().to_string(),
        pkce_verifier: pkce_verifier.secret().to_string(),
        nonce: nonce.secret().to_string(),
    })
}

/// Exchanges an authorization code and verifies the full identity proof.
///
/// The library verifier validates the JWS signature, issuer, audience,
/// expiration, and nonce. This function intentionally never retries with a
/// relaxed issuer verifier.
pub(crate) async fn exchange_code_for_identity(
    settings: &OidcSettings,
    code: &str,
    pkce_verifier: &str,
    nonce: &str,
) -> Result<VerifiedOidcIdentity, OidcFailure> {
    if code.is_empty() || code.len() > MAX_OIDC_CALLBACK_CODE_BYTES {
        return Err(OidcFailure::TokenExchange);
    }
    let discovered = discover_client(settings).await?;
    let token_response = discovered
        .client
        .exchange_code(AuthorizationCode::new(code.to_string()))
        .set_pkce_verifier(PkceCodeVerifier::new(pkce_verifier.to_string()))
        .request_async(&discovered.http_client)
        .await
        .map_err(|_| OidcFailure::TokenExchange)?;
    let id_token = token_response.id_token().ok_or(OidcFailure::IdTokenVerification)?;
    let verifier = discovered.client.id_token_verifier();
    let claims = id_token
        .claims(&verifier, &Nonce::new(nonce.to_string()))
        .map_err(|_| OidcFailure::IdTokenVerification)?;
    // The verifier above already checks issuer using discovery metadata. Keep
    // this explicit exact assertion as a regression guard against future
    // changes that might weaken the default verifier configuration.
    if claims.issuer().as_str() != settings.issuer_url {
        return Err(OidcFailure::IdTokenVerification);
    }
    let subject = claims.subject().as_str();
    if !is_valid_subject(subject) {
        return Err(OidcFailure::Claims);
    }

    let userinfo: UserInfoClaims<OidcAdditionalClaims, CoreGenderClaim> = discovered
        .client
        .user_info(token_response.access_token().to_owned(), Some(claims.subject().clone()))
        .request_async(&discovered.http_client)
        .await
        .map_err(|_| OidcFailure::UserInfo)?;

    let email = userinfo
        .email()
        .map(|value| value.as_str().trim())
        .filter(|value| !value.is_empty())
        .filter(|_| userinfo.email_verified() == Some(true))
        .map(|value| value.to_ascii_lowercase())
        .ok_or(OidcFailure::Claims)?;
    if !has_required_group(userinfo.additional_claims(), &settings.required_group)? {
        return Err(OidcFailure::Claims);
    }

    Ok(VerifiedOidcIdentity { subject: subject.to_string(), display_name: email.clone(), email })
}

/// An issuer-specific immutable provider key stored in the existing
/// `auth_provider_kind` column. The issuer value itself is not duplicated in
/// per-user rows, while identities from distinct issuers remain separate.
#[must_use]
pub(crate) fn issuer_provider_identifier(issuer_url: &str) -> String {
    format!("oidc:{}", hex::encode(Sha256::digest(issuer_url.as_bytes())))
}

/// Stable local login for an externally authenticated user. It intentionally
/// derives from the immutable provider/subject pair rather than email or a
/// display claim. A collision is still explicitly rejected by the repository.
#[must_use]
pub(crate) fn external_identity_login(provider_identifier: &str, subject: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(provider_identifier.as_bytes());
    digest.update([0]);
    digest.update(subject.as_bytes());
    format!("oidc-{}", hex::encode(digest.finalize()))
}

/// Generates a random, HttpOnly-cookie binding distinct from the URL state.
#[must_use]
pub(crate) fn new_attempt_cookie_binding() -> String {
    CsrfToken::new_random().secret().to_string()
}

/// Redis key deliberately hashes URL-visible state rather than including it
/// verbatim in database/Redis diagnostic tooling.
#[must_use]
pub(crate) fn attempt_redis_key(state: &str) -> String {
    format!("ironrag:oidc:attempt:{}", hex::encode(Sha256::digest(state.as_bytes())))
}

/// Redis key for the issuer-scoped OIDC login start bucket. The raw issuer is
/// intentionally not included because Redis keys are visible in operational
/// diagnostics and support tooling.
#[must_use]
pub(crate) fn oidc_login_admission_redis_key(issuer_url: &str) -> String {
    format!(
        "ironrag:oidc:login-admission:v1:{{{}}}",
        hex::encode(Sha256::digest(issuer_url.as_bytes()))
    )
}

/// Redis key of one source's OIDC start bucket.
///
/// The source identity is hashed into the bucket name while the issuer hash
/// stays the `{...}` hash tag, so this key lands in the same Redis Cluster
/// slot as [`oidc_login_admission_redis_key`] and the two-bucket `EVAL` stays
/// a single-slot script.
pub(crate) fn oidc_login_admission_source_redis_key(issuer_url: &str, source_key: &str) -> String {
    format!(
        "ironrag:oidc:login-admission-source:v1:{}:{{{}}}",
        hex::encode(Sha256::digest(source_key.as_bytes())),
        hex::encode(Sha256::digest(issuer_url.as_bytes()))
    )
}

async fn discover_client(settings: &OidcSettings) -> Result<DiscoveredOidcClient, OidcFailure> {
    let issuer_url =
        IssuerUrl::new(settings.issuer_url.clone()).map_err(|_| OidcFailure::Discovery)?;
    let redirect_url =
        RedirectUrl::new(settings.redirect_url.clone()).map_err(|_| OidcFailure::Discovery)?;
    let http_client = StrictOidcHttpClient::new().map_err(|_| OidcFailure::Discovery)?;
    let provider_metadata = CoreProviderMetadata::discover_async(issuer_url, &http_client)
        .await
        .map_err(|_| OidcFailure::Discovery)?;
    validate_discovered_endpoint(provider_metadata.authorization_endpoint().as_str())?;
    let token_url = provider_metadata.token_endpoint().cloned().ok_or(OidcFailure::Discovery)?;
    validate_discovered_endpoint(token_url.as_str())?;
    let userinfo_url =
        provider_metadata.userinfo_endpoint().cloned().ok_or(OidcFailure::Discovery)?;
    validate_discovered_endpoint(userinfo_url.as_str())?;
    validate_discovered_endpoint(provider_metadata.jwks_uri().as_str())?;
    let client = CoreClient::from_provider_metadata(
        provider_metadata,
        ClientId::new(settings.client_id.clone()),
        Some(ClientSecret::new(settings.client_secret.clone())),
    )
    .set_redirect_uri(redirect_url)
    .set_token_uri(
        TokenUrl::new(token_url.as_str().to_string()).map_err(|_| OidcFailure::Discovery)?,
    )
    .set_user_info_url(
        UserInfoUrl::new(userinfo_url.as_str().to_string()).map_err(|_| OidcFailure::Discovery)?,
    );

    Ok(DiscoveredOidcClient { client, http_client })
}

fn is_valid_subject(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= MAX_OIDC_SUBJECT_BYTES
        && !value.chars().any(char::is_control)
}

fn has_required_group(
    claims: &OidcAdditionalClaims,
    required_group: &str,
) -> Result<bool, OidcFailure> {
    let groups = claims.claims.get("groups").ok_or(OidcFailure::Claims)?;
    match groups {
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| value.as_str().map(str::trim).filter(|value| !value.is_empty()))
            .collect::<Option<Vec<_>>>()
            .map(|groups| groups.into_iter().any(|group| group == required_group))
            .ok_or(OidcFailure::Claims),
        _ => Err(OidcFailure::Claims),
    }
}

fn validate_discovered_endpoint(value: &str) -> Result<(), OidcFailure> {
    let endpoint = Url::parse(value).map_err(|_| OidcFailure::Discovery)?;
    if endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
    {
        return Err(OidcFailure::Discovery);
    }
    if endpoint.scheme() == "https" {
        return Ok(());
    }
    #[cfg(any(test, feature = "test-support"))]
    // The loopback exception exists solely for the in-process mock IdP
    // (`mock_idp`). It is excluded from production builds, where every
    // provider endpoint is unconditionally HTTPS.
    if endpoint.scheme() == "http"
        && endpoint.host().is_some_and(|host| match host {
            url::Host::Ipv4(host) => host.is_loopback(),
            url::Host::Ipv6(host) => host.is_loopback(),
            url::Host::Domain(_) => false,
        })
    {
        return Ok(());
    }

    Err(OidcFailure::Discovery)
}

#[cfg(any(test, feature = "test-support"))]
pub mod mock_idp;

#[cfg(test)]
mod tests;
