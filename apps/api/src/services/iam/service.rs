use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use chrono::{Duration, Utc};
use tracing::warn;
use uuid::Uuid;

use crate::{
    app::{bootstrap, state::AppState},
    domains::iam::{
        ApiToken, Grant, GrantResourceKind, Principal, PrincipalKind, WorkspaceMembership,
    },
    infra::repositories::{audit_repository, iam_repository, query_repository, runtime_repository},
    interfaces::http::router_support::ApiError,
    services::ai_catalog_service::{
        ApplyBootstrapProviderPresetBundleCommand, BootstrapAiSetupDescriptor,
    },
    shared::auth_tokens::{
        hash_api_token, hash_session_secret, mint_plaintext_api_token,
        mint_plaintext_session_secret, preview_api_token,
    },
};

#[derive(Clone, Default)]
pub struct IamService;

#[derive(Debug, Clone)]
pub struct BootstrapStatusOutcome {
    pub setup_required: bool,
    pub ai_setup: Option<BootstrapAiSetupDescriptor>,
}

#[derive(Debug, Clone)]
pub struct BootstrapSetupAiCommand {
    pub provider_kind: String,
    pub api_key: Option<String>,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BootstrapSetupCommand {
    pub login: String,
    pub display_name: Option<String>,
    pub password: String,
    pub ai_setup: Option<BootstrapSetupAiCommand>,
    pub ttl_hours: u64,
    pub request_id: String,
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
    pub login: String,
    pub password: String,
    pub ttl_hours: u64,
}

#[derive(Debug, Clone)]
pub struct AuthenticateSessionOutcome {
    pub session_id: Uuid,
    pub session_secret: String,
    pub principal_id: Uuid,
    pub login: String,
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

#[derive(Debug, Clone, Copy)]
pub struct RuntimeExecutionAccessScope {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub document_id: Option<Uuid>,
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
                .map_err(|_| ApiError::Internal)?
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
                    crate::infra::repositories::get_runtime_graph_extraction_record_by_id(
                        &state.persistence.postgres,
                        runtime_execution.owner_id,
                    )
                    .await
                    .map_err(|_| ApiError::Internal)?
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
                .map_err(|_| ApiError::Internal)?
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
                    .arango_document_store
                    .get_revision(runtime_execution.owner_id)
                    .await
                    .map_err(|_| ApiError::Internal)?
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
        let active_users =
            iam_repository::count_active_user_principals(&state.persistence.postgres)
                .await
                .map_err(|_| ApiError::Internal)?;
        let setup_required = active_users == 0 && !state.settings.has_explicit_ui_bootstrap_admin();
        let ai_setup = if setup_required {
            Some(state.canonical_services.ai_catalog.describe_bootstrap_ai_setup(state).await?)
        } else {
            None
        };
        Ok(BootstrapStatusOutcome { setup_required, ai_setup })
    }

    pub async fn setup_bootstrap_admin(
        &self,
        state: &AppState,
        command: BootstrapSetupCommand,
    ) -> Result<AuthenticateSessionOutcome, ApiError> {
        if state.settings.has_explicit_ui_bootstrap_admin() {
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
                .canonical_services
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
            .map(std::string::ToString::to_string)
            .unwrap_or_else(|| login.clone());
        let password = command.password.trim().to_string();
        if password.len() < 8 {
            return Err(ApiError::BadRequest("password must be at least 8 characters long".into()));
        }

        let email = default_bootstrap_email(&login);
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
            if let Some(ai_setup) = command.ai_setup {
                state
                    .canonical_services
                    .ai_catalog
                    .apply_bootstrap_provider_preset_bundle(
                        state,
                        ApplyBootstrapProviderPresetBundleCommand {
                            provider_kind: ai_setup.provider_kind,
                            api_key: ai_setup.api_key,
                            base_url: ai_setup.base_url,
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
            command.ttl_hours,
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
        let login = command.login.trim().to_ascii_lowercase();
        if login.is_empty() {
            return Err(ApiError::BadRequest("login must not be empty".into()));
        }
        if command.password.is_empty() {
            return Err(ApiError::BadRequest("password must not be empty".into()));
        }

        let user = iam_repository::get_user_by_login(&state.persistence.postgres, &login)
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

        issue_session_for_user(
            state,
            user.principal_id,
            &user.login,
            &user.email,
            &user.display_name,
            command.ttl_hours,
        )
        .await
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

async fn rollback_failed_bootstrap_principal(state: &AppState, principal_id: Uuid) {
    let rollback = async {
        sqlx::query("delete from iam_workspace_membership where principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query("delete from iam_grant where principal_id = $1 or granted_by_principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query("delete from iam_session where principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query(
            "delete from iam_api_token_secret where token_principal_id in (
                select principal_id from iam_api_token where principal_id = $1 or issued_by_principal_id = $1
            )",
        )
        .bind(principal_id)
        .execute(&state.persistence.postgres)
        .await?;
        sqlx::query("delete from iam_api_token where principal_id = $1 or issued_by_principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query("delete from iam_user where principal_id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        sqlx::query("delete from iam_principal where id = $1")
            .bind(principal_id)
            .execute(&state.persistence.postgres)
            .await?;
        Ok::<(), sqlx::Error>(())
    }
    .await;

    if let Err(error) = rollback {
        warn!(
            ?error,
            principal_id = %principal_id,
            "failed to roll back incomplete bootstrap principal",
        );
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

fn normalize_bootstrap_login(value: &str) -> Result<String, ApiError> {
    let login = value.trim().to_ascii_lowercase();
    if login.is_empty() {
        return Err(ApiError::BadRequest("login must not be empty".into()));
    }
    if login.bytes().any(|byte| !matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')) {
        return Err(ApiError::BadRequest(
            "login must contain only lowercase letters, digits, '.', '_' or '-'".into(),
        ));
    }
    Ok(login)
}

fn default_bootstrap_email(login: &str) -> String {
    format!("{login}@rustrag.local")
}

async fn issue_session_for_user(
    state: &AppState,
    principal_id: Uuid,
    login: &str,
    email: &str,
    display_name: &str,
    ttl_hours: u64,
) -> Result<AuthenticateSessionOutcome, ApiError> {
    let expires_at = Utc::now() + Duration::hours(i64::try_from(ttl_hours.max(1)).unwrap_or(24));
    let session_secret = mint_plaintext_session_secret();
    let session = iam_repository::create_session(
        &state.persistence.postgres,
        principal_id,
        &hash_session_secret(&session_secret),
        expires_at,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(AuthenticateSessionOutcome {
        session_id: session.id,
        session_secret,
        principal_id,
        login: login.to_string(),
        email: email.to_string(),
        display_name: display_name.to_string(),
        expires_at: session.expires_at,
    })
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
