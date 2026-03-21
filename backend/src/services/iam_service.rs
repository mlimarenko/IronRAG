use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use chrono::{Duration, Utc};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::iam::{
        ApiToken, Grant, GrantResourceKind, Principal, PrincipalKind, WorkspaceMembership,
    },
    infra::repositories::{audit_repository, iam_repository},
    interfaces::http::router_support::ApiError,
    shared::auth_tokens::{
        hash_api_token, hash_session_secret, mint_plaintext_api_token,
        mint_plaintext_session_secret, preview_api_token,
    },
};

#[derive(Clone, Default)]
pub struct IamService;

#[derive(Debug, Clone)]
pub struct BootstrapClaimCommand {
    pub bootstrap_secret: String,
    pub email: String,
    pub display_name: String,
    pub password: String,
    pub request_id: String,
}

#[derive(Debug, Clone)]
pub struct BootstrapClaimOutcome {
    pub principal_id: uuid::Uuid,
    pub email: String,
    pub display_name: String,
    pub claimed_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct MintApiTokenCommand {
    pub workspace_id: Option<Uuid>,
    pub label: String,
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub issued_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct MintApiTokenOutcome {
    pub token: String,
    pub api_token: ApiToken,
}

#[derive(Debug, Clone)]
pub struct RotateApiTokenOutcome {
    pub token: String,
    pub api_token: ApiToken,
    pub revoked_secret_versions: usize,
}

#[derive(Debug, Clone)]
pub struct AuthenticateSessionCommand {
    pub email: String,
    pub password: String,
    pub ttl_hours: u64,
}

#[derive(Debug, Clone)]
pub struct AuthenticateSessionOutcome {
    pub session_id: Uuid,
    pub session_secret: String,
    pub principal_id: Uuid,
    pub email: String,
    pub display_name: String,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone)]
pub struct EffectiveGrantResolution {
    pub principal: Principal,
    pub grants: Vec<Grant>,
    pub workspace_memberships: Vec<WorkspaceMembership>,
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

    /// Claims the first canonical administrator explicitly through the IAM surface.
    ///
    /// # Errors
    /// Returns authorization, validation, or persistence errors for bootstrap conflicts.
    pub async fn claim_bootstrap_admin(
        &self,
        state: &AppState,
        command: BootstrapClaimCommand,
    ) -> Result<BootstrapClaimOutcome, ApiError> {
        let bootstrap_settings = state.settings.bootstrap_settings();
        if !bootstrap_settings.bootstrap_claim_enabled {
            return Err(ApiError::forbidden("bootstrap claim is disabled"));
        }

        let configured_secret = bootstrap_settings.bootstrap_token.ok_or_else(|| {
            ApiError::BadRequest("bootstrap claim secret is not configured".into())
        })?;
        if command.bootstrap_secret != configured_secret {
            return Err(ApiError::Unauthorized);
        }

        let email = command.email.trim().to_ascii_lowercase();
        if email.is_empty() || !email.contains('@') {
            return Err(ApiError::BadRequest("email must be a valid address".into()));
        }
        let display_name = command.display_name.trim().to_string();
        if display_name.is_empty() {
            return Err(ApiError::BadRequest("displayName must not be empty".into()));
        }
        let password = command.password.trim().to_string();
        if password.len() < 8 {
            return Err(ApiError::BadRequest("password must be at least 8 characters long".into()));
        }

        let password_hash = hash_password(&password)?;
        let claimed = iam_repository::claim_bootstrap_user(
            &state.persistence.postgres,
            &email,
            &display_name,
            &password_hash,
        )
        .await
        .map_err(|error| {
            warn!(?error, email = %email, "failed to persist canonical bootstrap claim");
            ApiError::Internal
        })?
        .ok_or_else(|| {
            ApiError::bootstrap_already_claimed("bootstrap claim has already been completed")
        })?;

        audit_repository::append_bootstrap_claim_event(
            &state.persistence.postgres,
            claimed.principal_id,
            &command.request_id,
            "Bootstrap administrator claimed",
            &format!(
                "Canonical bootstrap claim created principal {} for {}",
                claimed.principal_id, claimed.email
            ),
        )
        .await
        .map_err(|error| {
            warn!(?error, principal_id = %claimed.principal_id, "failed to append bootstrap audit event");
            ApiError::Internal
        })?;

        Ok(BootstrapClaimOutcome {
            principal_id: claimed.principal_id,
            email: claimed.email,
            display_name: claimed.display_name,
            claimed_at: claimed.claimed_at,
        })
    }

    pub async fn get_principal(
        &self,
        state: &AppState,
        principal_id: Uuid,
    ) -> Result<Principal, ApiError> {
        let principal =
            iam_repository::get_principal_by_id(&state.persistence.postgres, principal_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("principal", principal_id))?;
        Ok(map_principal(principal)?)
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

        let plaintext = mint_plaintext_api_token();
        let token_row = iam_repository::create_api_token(
            &state.persistence.postgres,
            command.workspace_id,
            label,
            &preview_api_token(&plaintext),
            command.issued_by_principal_id,
            command.expires_at,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        iam_repository::create_api_token_secret(
            &state.persistence.postgres,
            token_row.principal_id,
            &hash_api_token(&plaintext),
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        Ok(MintApiTokenOutcome { token: plaintext, api_token: map_api_token(token_row) })
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
        .map_err(|_| ApiError::Internal)?;
        let token_row =
            iam_repository::revoke_api_token(&state.persistence.postgres, token_principal_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;
        Ok(map_api_token(token_row))
    }

    pub async fn list_api_tokens(
        &self,
        state: &AppState,
        workspace_id: Option<Uuid>,
    ) -> Result<Vec<ApiToken>, ApiError> {
        let rows = iam_repository::list_api_tokens(&state.persistence.postgres, workspace_id)
            .await
            .map_err(|_| ApiError::Internal)?;
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
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("api_token", token_principal_id))?;
        let revoked = iam_repository::revoke_active_api_token_secrets(
            &state.persistence.postgres,
            token_principal_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let plaintext = mint_plaintext_api_token();
        iam_repository::create_api_token_secret(
            &state.persistence.postgres,
            token_principal_id,
            &hash_api_token(&plaintext),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(RotateApiTokenOutcome {
            token: plaintext,
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
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("principal", principal_id))?;
        let grants =
            iam_repository::list_grants_by_principal(&state.persistence.postgres, principal_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        let memberships = iam_repository::list_workspace_memberships_by_principal(
            &state.persistence.postgres,
            principal_id,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

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
        let email = command.email.trim().to_ascii_lowercase();
        if email.is_empty() || !email.contains('@') {
            return Err(ApiError::BadRequest("email must be a valid address".into()));
        }
        if command.password.is_empty() {
            return Err(ApiError::BadRequest("password must not be empty".into()));
        }

        let user = iam_repository::get_user_by_email(&state.persistence.postgres, &email)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or(ApiError::Unauthorized)?;
        verify_password(&command.password, &user.password_hash)?;
        let principal =
            iam_repository::get_principal_by_id(&state.persistence.postgres, user.principal_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or(ApiError::Unauthorized)?;
        if principal.status != "active" {
            return Err(ApiError::Unauthorized);
        }

        let ttl_hours = command.ttl_hours.max(1);
        let expires_at = Utc::now() + Duration::hours(i64::try_from(ttl_hours).unwrap_or(24));
        let session_secret = mint_plaintext_session_secret();
        let session = iam_repository::create_session(
            &state.persistence.postgres,
            user.principal_id,
            &hash_session_secret(&session_secret),
            expires_at,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        Ok(AuthenticateSessionOutcome {
            session_id: session.id,
            session_secret,
            principal_id: user.principal_id,
            email: user.email,
            display_name: user.display_name,
            expires_at: session.expires_at,
        })
    }

    pub async fn revoke_session(&self, state: &AppState, session_id: Uuid) -> Result<(), ApiError> {
        iam_repository::revoke_session(&state.persistence.postgres, session_id)
            .await
            .map_err(|_| ApiError::Internal)?
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
            command.principal_id,
            grant_resource_kind_as_str(&command.resource_kind),
            command.resource_id,
            command.permission_kind.trim(),
            command.granted_by_principal_id,
            command.expires_at,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        Ok(map_grant(row)?)
    }

    pub async fn revoke_grant(&self, state: &AppState, grant_id: Uuid) -> Result<Grant, ApiError> {
        let row = iam_repository::delete_grant(&state.persistence.postgres, grant_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("grant", grant_id))?;
        Ok(map_grant(row)?)
    }
}

fn hash_password(password: &str) -> Result<String, ApiError> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|_| ApiError::Internal)
}

fn verify_password(password: &str, password_hash: &str) -> Result<(), ApiError> {
    let parsed_hash = PasswordHash::new(password_hash).map_err(|_| ApiError::Internal)?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| ApiError::Unauthorized)
}

fn map_principal(row: iam_repository::IamPrincipalRow) -> Result<Principal, ApiError> {
    Ok(Principal {
        id: row.id,
        principal_kind: parse_principal_kind(&row.principal_kind)?,
        display_label: row.display_label,
        status: row.status,
        created_at: row.created_at,
    })
}

fn map_api_token(row: iam_repository::IamApiTokenRow) -> ApiToken {
    ApiToken {
        principal_id: row.principal_id,
        workspace_id: row.workspace_id,
        label: row.label,
        token_prefix: row.token_prefix,
        status: row.status,
        expires_at: row.expires_at,
    }
}

fn map_grant(row: iam_repository::IamGrantRow) -> Result<Grant, ApiError> {
    Ok(Grant {
        id: row.id,
        principal_id: row.principal_id,
        resource_kind: parse_grant_resource_kind(&row.resource_kind)?,
        resource_id: row.resource_id,
        permission_kind: row.permission_kind,
        granted_at: row.granted_at,
    })
}

fn map_workspace_membership(row: iam_repository::IamWorkspaceMembershipRow) -> WorkspaceMembership {
    WorkspaceMembership {
        workspace_id: row.workspace_id,
        principal_id: row.principal_id,
        membership_state: row.membership_state,
        joined_at: row.joined_at,
        ended_at: row.ended_at,
    }
}

fn grant_resource_kind_as_str(value: &GrantResourceKind) -> &'static str {
    match value {
        GrantResourceKind::System => "system",
        GrantResourceKind::Workspace => "workspace",
        GrantResourceKind::Library => "library",
        GrantResourceKind::Document => "document",
        GrantResourceKind::QuerySession => "query_session",
        GrantResourceKind::AsyncOperation => "async_operation",
        GrantResourceKind::Connector => "connector",
        GrantResourceKind::ProviderCredential => "provider_credential",
        GrantResourceKind::LibraryBinding => "library_binding",
    }
}

fn parse_principal_kind(value: &str) -> Result<PrincipalKind, ApiError> {
    match value {
        "user" => Ok(PrincipalKind::User),
        "api_token" => Ok(PrincipalKind::ApiToken),
        "worker" => Ok(PrincipalKind::Worker),
        "bootstrap" => Ok(PrincipalKind::Bootstrap),
        _ => Err(ApiError::Internal),
    }
}

fn parse_grant_resource_kind(value: &str) -> Result<GrantResourceKind, ApiError> {
    match value {
        "system" => Ok(GrantResourceKind::System),
        "workspace" => Ok(GrantResourceKind::Workspace),
        "library" => Ok(GrantResourceKind::Library),
        "document" => Ok(GrantResourceKind::Document),
        "query_session" => Ok(GrantResourceKind::QuerySession),
        "async_operation" => Ok(GrantResourceKind::AsyncOperation),
        "connector" => Ok(GrantResourceKind::Connector),
        "provider_credential" => Ok(GrantResourceKind::ProviderCredential),
        "library_binding" => Ok(GrantResourceKind::LibraryBinding),
        _ => Err(ApiError::Internal),
    }
}
