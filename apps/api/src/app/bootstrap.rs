use argon2::{
    Argon2,
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString, rand_core::OsRng},
};
use tracing::info;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{catalog_repository, iam_repository},
    interfaces::http::router_support::ApiError,
    shared::auth_tokens::{hash_api_token, preview_api_token},
};

fn hash_password(password: &str) -> Result<String, ApiError> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|e| ApiError::internal_with_log(e, "internal"))
}

fn password_hash_matches(password: &str, password_hash: &str) -> Result<bool, ApiError> {
    let parsed = PasswordHash::new(password_hash).map_err(|error| {
        ApiError::internal_with_log(error, "invalid stored bootstrap password hash")
    })?;
    Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}

pub(crate) struct DefaultCatalogBootstrapOutcome {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
}

pub(crate) async fn ensure_default_catalog_workspace_and_library(
    state: &AppState,
    principal_id: Uuid,
) -> Result<DefaultCatalogBootstrapOutcome, ApiError> {
    let workspace = if let Some(existing) =
        catalog_repository::get_workspace_by_slug(&state.persistence.postgres, "default")
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    {
        existing
    } else {
        catalog_repository::create_workspace(
            &state.persistence.postgres,
            "default",
            "Default workspace",
            None,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    };

    let library = if let Some(existing) =
        catalog_repository::list_libraries(&state.persistence.postgres, Some(workspace.id))
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .into_iter()
            .find(|entry| entry.slug == "default-library")
    {
        existing
    } else {
        catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            "default-library",
            "Default library",
            Some("Backstage default library for the primary documents and ask flow"),
            None,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    };

    iam_repository::upsert_workspace_membership(
        &state.persistence.postgres,
        workspace.id,
        principal_id,
        "active",
    )
    .await
    .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

    Ok(DefaultCatalogBootstrapOutcome { workspace_id: workspace.id, library_id: library.id })
}

pub(crate) async fn apply_configured_default_catalog_ai_setup(
    state: &AppState,
    catalog: &DefaultCatalogBootstrapOutcome,
) -> Result<bool, ApiError> {
    // Ensure every credential in IRONRAG_AI_PROVIDER_API_KEYS_JSON_B64 has a matching
    // instance-scope account. Independent of binding selection so newly
    // added provider keys appear in the admin UI on the next restart even
    // if the chosen pipeline provider doesn't cover all purposes.
    state.application_services.ai_catalog.ensure_env_ai_accounts(state).await?;

    // Best-effort: bind the selected workspace/library to the primary
    // bootstrap provider's binding bundle. This requires the provider to
    // cover all five required purposes; optional `extract_text` is included
    // only when explicitly available. The same embedding model serves stored
    // and query vectors. If the provider cannot satisfy that contract (e.g. a
    // multi-provider env where the chosen primary lacks an embedding model),
    // we log and continue so the
    // env-keyed credentials we just created stay visible in the admin UI.
    match state
        .application_services
        .ai_catalog
        .apply_configured_bootstrap_ai_setup(state, catalog.workspace_id, catalog.library_id, None)
        .await
    {
        Ok(applied) => Ok(applied),
        Err(err) => {
            tracing::warn!(
                stage = "bootstrap",
                error = %err,
                "configured bootstrap AI setup skipped — credentials are seeded but bindings need manual configuration",
            );
            Ok(false)
        }
    }
}

async fn ensure_bootstrap_api_token(
    state: &AppState,
    principal_id: Uuid,
    plaintext_token: &str,
) -> Result<(), ApiError> {
    let token_hash = hash_api_token(plaintext_token);
    let token_principal_id = if let Some(existing) =
        iam_repository::find_active_api_token_by_secret_hash(
            &state.persistence.postgres,
            &token_hash,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
    {
        existing.principal_id
    } else {
        let token = iam_repository::create_api_token(
            &state.persistence.postgres,
            None,
            "Bootstrap admin token",
            &preview_api_token(plaintext_token),
            Some(principal_id),
            None,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        iam_repository::create_api_token_secret(
            &state.persistence.postgres,
            token.principal_id,
            &token_hash,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        token.principal_id
    };
    info!(
        principal_id = %principal_id,
        api_token_principal_id = %token_principal_id,
        "ensured bootstrap admin api token",
    );
    Ok(())
}

/// # Errors
/// Returns an error when the bootstrap principal, catalog workspace, library, or
/// bootstrap API token cannot be provisioned.
pub async fn ensure_bootstrap_admin(state: &AppState) -> Result<(), ApiError> {
    let Some(bootstrap_admin) = state.ui_bootstrap_admin.as_ref() else {
        return Ok(());
    };

    let by_login =
        iam_repository::get_user_by_login(&state.persistence.postgres, &bootstrap_admin.login)
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "failed to load bootstrap login")
            })?;
    let by_email =
        iam_repository::get_user_by_email(&state.persistence.postgres, &bootstrap_admin.email)
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "failed to load bootstrap email")
            })?;

    let principal_id = match (by_login, by_email) {
        (None, None) => {
            let password_hash = hash_password(&bootstrap_admin.password)?;
            let claimed = iam_repository::claim_bootstrap_user(
                &state.persistence.postgres,
                &bootstrap_admin.login,
                &bootstrap_admin.email,
                &bootstrap_admin.display_name,
                &password_hash,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            let Some(claimed) = claimed else {
                return Ok(());
            };
            claimed.principal_id
        }
        (Some(login_user), Some(email_user))
            if login_user.principal_id == email_user.principal_id
                && login_user.auth_provider_kind == "password"
                && login_user.login.eq_ignore_ascii_case(&bootstrap_admin.login)
                && login_user.email.eq_ignore_ascii_case(&bootstrap_admin.email) =>
        {
            reconcile_bootstrap_password(state, &login_user, bootstrap_admin).await?;
            login_user.principal_id
        }
        // Any occupied but non-exact identity is a fail-closed operator
        // configuration conflict. In particular, do not attach the break-
        // glass bootstrap details to an OIDC principal based on mutable email.
        _ => {
            return Err(ApiError::Conflict(
                "configured bootstrap administrator identity is already owned by a different account"
                    .to_string(),
            ));
        }
    };

    let catalog = ensure_default_catalog_workspace_and_library(state, principal_id).await?;
    let _ = apply_configured_default_catalog_ai_setup(state, &catalog).await?;
    if let Some(api_token) = bootstrap_admin.api_token.as_deref() {
        ensure_bootstrap_api_token(state, principal_id, api_token).await?;
    }
    Ok(())
}

async fn reconcile_bootstrap_password(
    state: &AppState,
    existing: &iam_repository::IamUserRow,
    bootstrap_admin: &crate::app::config::UiBootstrapAdmin,
) -> Result<(), ApiError> {
    if password_hash_matches(&bootstrap_admin.password, &existing.password_hash)? {
        return Ok(());
    }
    let replacement_hash = hash_password(&bootstrap_admin.password)?;
    let updated = iam_repository::update_bootstrap_password_if_matching_local_user(
        &state.persistence.postgres,
        existing.principal_id,
        &existing.login,
        &existing.email,
        &replacement_hash,
    )
    .await
    .map_err(|error| {
        ApiError::internal_with_log(error, "failed to reconcile bootstrap password")
    })?;
    if !updated {
        return Err(ApiError::Conflict(
            "configured bootstrap administrator identity changed during password reconciliation"
                .to_string(),
        ));
    }
    Ok(())
}
