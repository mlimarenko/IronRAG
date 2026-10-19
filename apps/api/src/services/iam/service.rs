use tracing::warn;
use uuid::Uuid;
use zeroize::{Zeroize as _, Zeroizing};

use crate::{
    app::{bootstrap, state::AppState},
    domains::iam::{
        ApiToken, Grant, GrantResourceKind, Principal, PrincipalKind, WorkspaceMembership,
    },
    infra::repositories::{audit_repository, iam_repository, query_repository, runtime_repository},
    interfaces::http::router_support::ApiError,
    services::ai_catalog_service::{
        ApplyBootstrapProviderBindingBundleCommand, BootstrapAiSetupDescriptor,
    },
    shared::auth_tokens::{hash_api_token, mint_plaintext_api_token, preview_api_token},
};

mod bootstrap_support;
mod mapping;
mod session_support;
#[cfg(test)]
mod tests;

pub(crate) use self::session_support::hash_password;
use self::{
    bootstrap_support::{
        normalize_bootstrap_login, rollback_failed_bootstrap_principal, validate_email,
    },
    mapping::{
        grant_resource_kind_as_str, map_api_token, map_grant, map_principal,
        map_workspace_membership,
    },
    session_support::{
        DUMMY_PASSWORD_HASH, OIDC_ONLY_PASSWORD_SENTINEL_HASH, issue_session_for_user,
        verify_password,
    },
};

#[derive(Clone, Default)]
pub struct IamService;

/// Test-only observability counter: total real invocations of
/// [`session_support::verify_password`] across both the dummy-hash and
/// real-hash branches of [`IamService::authenticate_session`]. It exists so
/// an integration test can prove a login-rate-limit rejection short-circuits
/// before Argon2id ever runs, using an observed call count rather than
/// response timing (which flakes under CI load). It never influences
/// production behavior and does not exist in a production build.
#[cfg(feature = "test-support")]
pub static PASSWORD_VERIFICATION_ATTEMPTS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Current value of [`PASSWORD_VERIFICATION_ATTEMPTS`]. Test-only.
#[cfg(feature = "test-support")]
#[doc(hidden)]
#[must_use]
pub fn password_verification_attempts() -> usize {
    PASSWORD_VERIFICATION_ATTEMPTS.load(std::sync::atomic::Ordering::SeqCst)
}

#[derive(Debug, Clone)]
pub struct BootstrapStatusOutcome {
    pub setup_required: bool,
    pub sso_available: bool,
    pub ai_setup: Option<BootstrapAiSetupDescriptor>,
}

#[derive(Clone)]
pub struct BootstrapSetupAiCommand {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

impl Drop for BootstrapSetupAiCommand {
    fn drop(&mut self) {
        if let Some(api_key) = self.api_key.as_mut() {
            api_key.zeroize();
        }
    }
}

impl std::fmt::Debug for BootstrapSetupAiCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapSetupAiCommand")
            .field("provider_kind", &self.provider_kind)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("base_url", &self.base_url.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

#[derive(Clone)]
pub struct BootstrapSetupCommand {
    pub login: String,
    pub email: String,
    pub display_name: Option<String>,
    pub password: String,
    pub ai_setup: Option<BootstrapSetupAiCommand>,
    pub request_id: String,
}

impl std::fmt::Debug for BootstrapSetupCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BootstrapSetupCommand")
            .field("login", &self.login)
            .field("email", &self.email)
            .field("display_name", &self.display_name)
            .field("password", &"<redacted>")
            .field("ai_setup", &self.ai_setup)
            .field("request_id", &self.request_id)
            .finish()
    }
}

impl Drop for BootstrapSetupCommand {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

#[derive(Debug, Clone)]
pub struct MintApiTokenCommand {
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub grants: Vec<MintApiTokenGrantCommand>,
    pub issued_by_principal_id: Option<Uuid>,
}

#[derive(Clone)]
pub struct MintApiTokenOutcome {
    pub token: String,
    pub api_token: ApiToken,
}

impl std::fmt::Debug for MintApiTokenOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MintApiTokenOutcome")
            .field("token", &"<redacted>")
            .field("api_token", &self.api_token)
            .finish()
    }
}

impl Drop for MintApiTokenOutcome {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

#[derive(Clone)]
pub struct RotateApiTokenOutcome {
    pub token: String,
    pub api_token: ApiToken,
    pub revoked_secret_versions: usize,
}

impl std::fmt::Debug for RotateApiTokenOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RotateApiTokenOutcome")
            .field("token", &"<redacted>")
            .field("api_token", &self.api_token)
            .field("revoked_secret_versions", &self.revoked_secret_versions)
            .finish()
    }
}

impl Drop for RotateApiTokenOutcome {
    fn drop(&mut self) {
        self.token.zeroize();
    }
}

#[derive(Clone)]
pub struct AuthenticateSessionCommand {
    pub login: String,
    pub password: String,
    /// Best-effort caller network origin used only as a login-rate-limit
    /// bucket key (IRG-80) -- never persisted, never used for authorization.
    /// See `interfaces::http::iam::session` for how this is derived.
    pub source_key: String,
}

impl std::fmt::Debug for AuthenticateSessionCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticateSessionCommand")
            .field("login", &self.login)
            .field("password", &"<redacted>")
            .field("source_key", &self.source_key)
            .finish()
    }
}

impl Drop for AuthenticateSessionCommand {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

#[derive(Clone)]
pub struct AuthenticateSessionOutcome {
    pub session_id: Uuid,
    pub session_secret: String,
    pub principal_id: Uuid,
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// Result of resolving a verified OIDC identity to a normal browser session.
///
/// `identity_created` is deliberately distinct from session creation: each
/// OIDC sign-in creates a new browser session, while JIT provisioning happens
/// only once for a durable external identity.
pub struct AuthenticateOidcSessionOutcome {
    pub session: AuthenticateSessionOutcome,
    pub identity_created: bool,
}

/// Verified OIDC identity ready for durable JIT resolution. Its provider key
/// is issuer-specific and never derived from a mutable login or email claim.
#[derive(Clone)]
pub struct AuthenticateOidcSessionCommand {
    pub auth_provider_kind: String,
    pub external_subject: String,
    pub login: String,
    pub email: String,
    pub display_name: String,
    pub bootstrap_admin: bool,
}

impl std::fmt::Debug for AuthenticateOidcSessionCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticateOidcSessionCommand")
            .field("auth_provider_kind", &self.auth_provider_kind)
            .field("external_subject", &"<redacted>")
            .field("login", &self.login)
            .field("email", &"<redacted>")
            .field("display_name", &"<redacted>")
            .field("bootstrap_admin", &self.bootstrap_admin)
            .finish()
    }
}

impl std::fmt::Debug for AuthenticateSessionOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticateSessionOutcome")
            .field("session_id", &self.session_id)
            .field("session_secret", &"<redacted>")
            .field("principal_id", &self.principal_id)
            .field("login", &self.login)
            .field("email", &self.email)
            .field("display_name", &self.display_name)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl std::fmt::Debug for AuthenticateOidcSessionOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthenticateOidcSessionOutcome")
            .field("session", &self.session)
            .field("identity_created", &self.identity_created)
            .finish()
    }
}

impl Drop for AuthenticateSessionOutcome {
    fn drop(&mut self) {
        self.session_secret.zeroize();
    }
}

#[derive(Debug, Clone)]
pub struct EffectiveGrantResolution {
    pub principal: Principal,
    pub grants: Vec<Grant>,
    pub workspace_memberships: Vec<WorkspaceMembership>,
}

#[derive(Debug, Clone, Copy)]
pub struct RuntimeExecutionAccessScope {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct MintApiTokenGrantCommand {
    pub resource_kind: GrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone)]
pub struct CreateGrantCommand {
    pub principal_id: Uuid,
    pub resource_kind: GrantResourceKind,
    pub resource_id: Uuid,
    pub permission_kind: String,
    pub granted_by_principal_id: Option<Uuid>,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl IamService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn resolve_runtime_execution_access_scope(
        &self,
        state: &AppState,
        runtime_execution: &runtime_repository::RuntimeExecutionRow,
    ) -> Result<RuntimeExecutionAccessScope, ApiError> {
        match runtime_execution.owner_kind {
            crate::domains::agent_runtime::RuntimeExecutionOwnerKind::QueryExecution => {
                let execution = query_repository::get_execution_by_id(
                    &state.persistence.postgres,
                    runtime_execution.owner_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| {
                    ApiError::resource_not_found("query_execution", runtime_execution.owner_id)
                })?;
                Ok(RuntimeExecutionAccessScope {
                    workspace_id: execution.workspace_id,
                    library_id: execution.library_id,
                    document_id: None,
                })
            }
            crate::domains::agent_runtime::RuntimeExecutionOwnerKind::GraphExtractionAttempt => {
                let extraction =
                    crate::infra::repositories::runtime_graph_repository::get_runtime_graph_extraction_record_by_id(
                        &state.persistence.postgres,
                        runtime_execution.owner_id,
                    )
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .ok_or_else(|| {
                        ApiError::resource_not_found(
                            "runtime_graph_extraction",
                            runtime_execution.owner_id,
                        )
                    })?;
                let library = crate::infra::repositories::catalog_repository::get_library_by_id(
                    &state.persistence.postgres,
                    extraction.library_id,
                )
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("library", extraction.library_id))?;
                Ok(RuntimeExecutionAccessScope {
                    workspace_id: library.workspace_id,
                    library_id: library.id,
                    document_id: Some(extraction.document_id),
                })
            }
            crate::domains::agent_runtime::RuntimeExecutionOwnerKind::StructuredPreparation
            | crate::domains::agent_runtime::RuntimeExecutionOwnerKind::TechnicalFactExtraction => {
                let revision = state
                    .document_store
                    .get_revision(runtime_execution.owner_id)
                    .await
                    .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                    .ok_or_else(|| {
                        ApiError::resource_not_found(
                            "knowledge_revision",
                            runtime_execution.owner_id,
                        )
                    })?;
                Ok(RuntimeExecutionAccessScope {
                    workspace_id: revision.workspace_id,
                    library_id: revision.library_id,
                    document_id: Some(revision.document_id),
                })
            }
        }
    }

    pub async fn get_bootstrap_status(
        &self,
        state: &AppState,
    ) -> Result<BootstrapStatusOutcome, ApiError> {
        // Admins specifically, not any active user: an OIDC JIT-provisioned
        // viewer must not permanently close interactive bootstrap for a
        // deployment that never got a configured administrator.
        let active_admins = iam_repository::count_admin_users(&state.persistence.postgres)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let setup_required = active_admins == 0 && !state.bootstrap_admin_configured;
        let ai_setup = if setup_required {
            Some(state.application_services.ai_catalog.describe_bootstrap_ai_setup(state).await?)
        } else {
            None
        };
        Ok(BootstrapStatusOutcome { setup_required, sso_available: state.oidc.is_some(), ai_setup })
    }

    pub async fn setup_bootstrap_admin(
        &self,
        state: &AppState,
        mut command: BootstrapSetupCommand,
    ) -> Result<AuthenticateSessionOutcome, ApiError> {
        if state.bootstrap_admin_configured {
            return Err(ApiError::forbidden(
                "interactive bootstrap setup is disabled when admin credentials are configured via environment",
            ));
        }

        let status = self.get_bootstrap_status(state).await?;
        if !status.setup_required {
            return Err(ApiError::bootstrap_already_claimed(
                "bootstrap setup has already been completed",
            ));
        }
        if let Some(ai_setup) = command.ai_setup.as_ref() {
            state
                .application_services
                .ai_catalog
                .ensure_bootstrap_provider_bundle_available(state, &ai_setup.provider_kind)
                .await?;
        }

        let login = normalize_bootstrap_login(&command.login)?;
        let display_name = command
            .display_name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map_or_else(|| login.clone(), std::string::ToString::to_string);
        let password = Zeroizing::new(command.password.trim().to_string());
        if password.len() < 8 {
            return Err(ApiError::BadRequest("password must be at least 8 characters long".into()));
        }

        let email = validate_email(&command.email)?;
        let password_hash = hash_password(&password)?;
        let claimed = iam_repository::claim_bootstrap_user(
            &state.persistence.postgres,
            &login,
            &email,
            &display_name,
            &password_hash,
        )
        .await
        .map_err(|error| {
            warn!(?error, login = %login, "failed to persist interactive bootstrap setup");
            ApiError::Internal
        })?
        .ok_or_else(|| {
            ApiError::bootstrap_already_claimed("bootstrap setup has already been completed")
        })?;

        let provision_result = async {
            let catalog_bootstrap =
                bootstrap::ensure_default_catalog_workspace_and_library(state, claimed.principal_id)
                    .await?;
            if let Some(mut ai_setup) = command.ai_setup.take() {
                state
                    .application_services
                    .ai_catalog
                    .apply_bootstrap_provider_binding_bundle(
                        state,
                        ApplyBootstrapProviderBindingBundleCommand {
                            provider_kind: std::mem::take(&mut ai_setup.provider_kind),
                            api_key: ai_setup.api_key.take(),
                            base_url: ai_setup.base_url.take(),
                            updated_by_principal_id: None,
                        },
                    )
                    .await?;
            } else if !bootstrap::apply_configured_default_catalog_ai_setup(
                state,
                &catalog_bootstrap,
            )
            .await?
            {
                return Err(ApiError::BadRequest(
                    "bootstrap ai setup payload is required when no bootstrap AI environment configuration is present"
                        .into(),
                ));
            }

            audit_repository::append_bootstrap_claim_event(
                &state.persistence.postgres,
                claimed.principal_id,
                &command.request_id,
                "Bootstrap administrator configured",
                &format!(
                    "Interactive bootstrap setup created principal {} for {}",
                    claimed.principal_id, claimed.login
                ),
            )
            .await
            .map_err(|error| {
                warn!(?error, principal_id = %claimed.principal_id, "failed to append bootstrap setup audit event");
                ApiError::Internal
            })?;
            Ok::<(), ApiError>(())
        }
        .await;
        if let Err(error) = provision_result {
            rollback_failed_bootstrap_principal(state, claimed.principal_id).await;
            return Err(error);
        }

        issue_session_for_user(
            state,
            claimed.principal_id,
            &claimed.login,
            &claimed.email,
            &claimed.display_name,
        )
        .await
    }

    pub async fn get_principal(
        &self,
        state: &AppState,
        principal_id: Uuid,
    ) -> Result<Principal, ApiError> {
        let principal =
            iam_repository::get_principal_by_id(&state.persistence.postgres, principal_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("principal", principal_id))?;
        map_principal(principal)
    }

    pub async fn mint_api_token(
        &self,
        state: &AppState,
        command: MintApiTokenCommand,
    ) -> Result<MintApiTokenOutcome, ApiError> {
        let label = command.label.trim();
        if label.is_empty() {
            return Err(ApiError::BadRequest("label must not be empty".into()));
        }
        for permission_kind in command.grants.iter().map(|grant| &grant.permission_kind) {
            if permission_kind.is_empty() {
                return Err(ApiError::BadRequest("permission kind must not be empty".into()));
            }
        }

        let plaintext = Zeroizing::new(mint_plaintext_api_token());
        let mut transaction = state
            .persistence
            .postgres
            .begin()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let transaction_connection = transaction.as_mut();

        let token_row = iam_repository::create_api_token_with_transaction(
            transaction_connection,
            command.workspace_id,
            label,
            &preview_api_token(&plaintext),
            command.issued_by_principal_id,
            command.expires_at,
        )
        .await
        .map_err(|e| {
            warn!(token_label = %label, workspace_id = ?command.workspace_id, ?e, "failed to create api token");
            ApiError::internal_with_log(e, "internal")
        })?;

        iam_repository::create_api_token_secret_with_transaction(
            transaction_connection,
            token_row.principal_id,
            &hash_api_token(&plaintext),
        )
        .await
        .map_err(|e| {
            warn!(
                token_principal_id = %token_row.principal_id,
                ?e,
                "failed to persist api token secret"
            );
            ApiError::internal_with_log(e, "internal")
        })?;

        for grant in &command.grants {
            iam_repository::create_grant(
                &mut *transaction_connection,
                &iam_repository::NewGrant {
                    principal_id: token_row.principal_id,
                    resource_kind: grant_resource_kind_as_str(&grant.resource_kind),
                    resource_id: grant.resource_id,
                    permission_kind: &grant.permission_kind,
                    granted_by_principal_id: command.issued_by_principal_id,
                    expires_at: grant.expires_at,
                },
            )
            .await
            .map_err(|e| {
                warn!(
                    token_principal_id = %token_row.principal_id,
                    resource_kind = %grant_resource_kind_as_str(&grant.resource_kind),
                    resource_id = %grant.resource_id,
                    permission_kind = %grant.permission_kind,
                    ?e,
                    "failed to create token grant",
                );
                ApiError::internal_with_log(e, "internal")
            })?;
        }

        transaction
            .commit()
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

        Ok(MintApiTokenOutcome {
            token: plaintext.to_string(),
            api_token: map_api_token(token_row),
        })
    }

    pub async fn revoke_api_token(
        &self,
        state: &AppState,
        token_principal_id: Uuid,
    ) -> Result<ApiToken, ApiError> {
        iam_repository::revoke_active_api_token_secrets(
            &state.persistence.postgres,
            token_principal_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let token_row =
            iam_repository::revoke_api_token(&state.persistence.postgres, token_principal_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;
        Ok(map_api_token(token_row))
    }

    pub async fn delete_revoked_api_token(
        &self,
        state: &AppState,
        token_principal_id: Uuid,
    ) -> Result<ApiToken, ApiError> {
        let token_row = iam_repository::get_api_token_by_principal_id(
            &state.persistence.postgres,
            token_principal_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;

        if token_row.status != "revoked" {
            return Err(ApiError::Conflict(
                "api token must be revoked before deletion".to_string(),
            ));
        }

        let deleted_row = iam_repository::delete_revoked_api_token(
            &state.persistence.postgres,
            token_principal_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;
        Ok(map_api_token(deleted_row))
    }

    pub async fn list_api_tokens(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
    ) -> Result<Vec<ApiToken>, ApiError> {
        let rows = iam_repository::list_api_tokens(&state.persistence.postgres, workspace_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(rows.into_iter().map(map_api_token).collect())
    }

    pub async fn rotate_api_token(
        &self,
        state: &AppState,
        token_principal_id: Uuid,
    ) -> Result<RotateApiTokenOutcome, ApiError> {
        let token_row = iam_repository::get_api_token_by_principal_id(
            &state.persistence.postgres,
            token_principal_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;
        let revoked = iam_repository::revoke_active_api_token_secrets(
            &state.persistence.postgres,
            token_principal_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let plaintext = Zeroizing::new(mint_plaintext_api_token());
        iam_repository::create_api_token_secret(
            &state.persistence.postgres,
            token_principal_id,
            &hash_api_token(&plaintext),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        Ok(RotateApiTokenOutcome {
            token: plaintext.to_string(),
            api_token: map_api_token(token_row),
            revoked_secret_versions: revoked.len(),
        })
    }

    pub async fn resolve_effective_grants(
        &self,
        state: &AppState,
        principal_id: Uuid,
    ) -> Result<EffectiveGrantResolution, ApiError> {
        let principal =
            iam_repository::get_principal_by_id(&state.persistence.postgres, principal_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or_else(|| ApiError::resource_not_found("principal", principal_id))?;
        let grants =
            iam_repository::list_grants_by_principal(&state.persistence.postgres, principal_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let memberships = iam_repository::list_workspace_memberships_by_principal(
            &state.persistence.postgres,
            principal_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        Ok(EffectiveGrantResolution {
            principal: map_principal(principal)?,
            grants: grants.into_iter().map(map_grant).collect::<Result<Vec<_>, _>>()?,
            workspace_memberships: memberships.into_iter().map(map_workspace_membership).collect(),
        })
    }

    pub async fn authenticate_session(
        &self,
        state: &AppState,
        command: AuthenticateSessionCommand,
    ) -> Result<AuthenticateSessionOutcome, ApiError> {
        let login = command.login.trim().to_ascii_lowercase();
        if login.is_empty() {
            return Err(ApiError::BadRequest("login must not be empty".into()));
        }
        if command.password.is_empty() {
            return Err(ApiError::BadRequest("password must not be empty".into()));
        }

        // Admission runs before any database lookup or Argon2id call, on the
        // already-normalized login: a rejection here never touches either.
        // See `services::iam::login_rate_limit` for why both axes are
        // checked, why rejection is immediate rather than a wait, and why
        // this never becomes a standing lockout on the account.
        let verification_permit = state
            .login_rate_limiter
            .admit(&login, &command.source_key)
            .await
            .map_err(ApiError::from_login_rate_limit)?;

        let user = match iam_repository::get_user_by_login(&state.persistence.postgres, &login)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        {
            Some(user) => user,
            None => {
                #[cfg(feature = "test-support")]
                PASSWORD_VERIFICATION_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = verify_password(&command.password, DUMMY_PASSWORD_HASH);
                return Err(ApiError::Unauthorized);
            }
        };
        if user.auth_provider_kind != "password" {
            // Keep the unknown-user and OIDC-user branches comparable enough
            // to avoid turning the password endpoint into an identity oracle,
            // while making the provider policy explicit instead of trusting a
            // sentinel hash to happen not to match.
            #[cfg(feature = "test-support")]
            PASSWORD_VERIFICATION_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let _ = verify_password(&command.password, DUMMY_PASSWORD_HASH);
            return Err(ApiError::Unauthorized);
        }
        #[cfg(feature = "test-support")]
        PASSWORD_VERIFICATION_ATTEMPTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        verify_password(&command.password, &user.password_hash)?;
        // Release the concurrency slot as soon as Argon2id is done; the
        // remaining work is plain database I/O, not the resource this
        // admission protects.
        drop(verification_permit);
        let principal =
            iam_repository::get_principal_by_id(&state.persistence.postgres, user.principal_id)
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .ok_or(ApiError::Unauthorized)?;
        if principal.status != "active" {
            return Err(ApiError::Unauthorized);
        }

        issue_session_for_user(
            state,
            user.principal_id,
            &user.login,
            &user.email,
            &user.display_name,
        )
        .await
    }

    /// Resolves a verified OIDC identity to one application user and issues a
    /// normal browser session. OIDC admission group membership is checked
    /// before this method; it never maps an IdP group to an application role.
    pub async fn authenticate_oidc_session(
        &self,
        state: &AppState,
        command: AuthenticateOidcSessionCommand,
    ) -> Result<AuthenticateOidcSessionOutcome, ApiError> {
        let role = if command.bootstrap_admin {
            iam_repository::SystemRole::Admin
        } else {
            iam_repository::SystemRole::Viewer
        };
        let provisioned = iam_repository::provision_oidc_user(
            &state.persistence.postgres,
            &iam_repository::NewOidcUser {
                auth_provider_kind: &command.auth_provider_kind,
                external_subject: &command.external_subject,
                login: &command.login,
                email: &command.email,
                display_name: &command.display_name,
                password_hash: OIDC_ONLY_PASSWORD_SENTINEL_HASH,
                role,
            },
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "failed to provision OIDC user"))?;
        let (user, identity_created) = match provisioned {
            iam_repository::OidcProvisionUserOutcome::Provisioned { user, created } => {
                (user, created)
            }
            // Do not reveal whether a local email or generated login exists,
            // and never use that collision as permission to link identities.
            iam_repository::OidcProvisionUserOutcome::IdentityCollision => {
                return Err(ApiError::Unauthorized);
            }
        };
        let principal =
            iam_repository::get_principal_by_id(&state.persistence.postgres, user.principal_id)
                .await
                .map_err(|error| {
                    ApiError::internal_with_log(error, "failed to load OIDC user principal")
                })?
                .ok_or(ApiError::Unauthorized)?;
        if principal.status != "active" {
            return Err(ApiError::Unauthorized);
        }

        let session = issue_session_for_user(
            state,
            user.principal_id,
            &user.login,
            &user.email,
            &user.display_name,
        )
        .await?;
        Ok(AuthenticateOidcSessionOutcome { session, identity_created })
    }

    pub async fn revoke_session(&self, state: &AppState, session_id: Uuid) -> Result<(), ApiError> {
        iam_repository::revoke_session(&state.persistence.postgres, session_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("session", session_id))?;
        Ok(())
    }

    pub async fn create_grant(
        &self,
        state: &AppState,
        command: CreateGrantCommand,
    ) -> Result<Grant, ApiError> {
        let row = iam_repository::create_grant(
            &state.persistence.postgres,
            &iam_repository::NewGrant {
                principal_id: command.principal_id,
                resource_kind: grant_resource_kind_as_str(&command.resource_kind),
                resource_id: command.resource_id,
                permission_kind: command.permission_kind.trim(),
                granted_by_principal_id: command.granted_by_principal_id,
                expires_at: command.expires_at,
            },
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        map_grant(row)
    }

    pub async fn revoke_grant(&self, state: &AppState, grant_id: Uuid) -> Result<Grant, ApiError> {
        let row = iam_repository::delete_grant(&state.persistence.postgres, grant_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("grant", grant_id))?;
        map_grant(row)
    }
}
