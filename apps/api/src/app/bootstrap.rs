#![allow(clippy::result_large_err)]

use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
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
    state
        .canonical_services
        .ai_catalog
        .apply_configured_bootstrap_ai_setup(state, catalog.workspace_id, catalog.library_id, None)
        .await
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
    let grants =
        iam_repository::list_grants_by_principal(&state.persistence.postgres, token_principal_id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    let has_admin_grant = grants.into_iter().any(|grant| {
        grant.resource_kind == "system"
            && grant.resource_id.is_nil()
            && grant.permission_kind == "iam_admin"
    });
    if !has_admin_grant {
        iam_repository::create_grant(
            &state.persistence.postgres,
            token_principal_id,
            "system",
            Uuid::nil(),
            "iam_admin",
            Some(principal_id),
            None,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
    }
    info!(
        principal_id = %principal_id,
        api_token_principal_id = %token_principal_id,
        "ensured bootstrap admin api token",
    );
    Ok(())
}

/// # Errors
/// Returns an error when the canonical bootstrap principal, catalog workspace, library, or
/// bootstrap API token cannot be provisioned.
pub async fn ensure_canonical_bootstrap_admin(state: &AppState) -> Result<(), ApiError> {
    let Some(bootstrap_admin) = state.ui_bootstrap_admin.clone() else {
        return Ok(());
    };

    let principal_id = if let Some(existing) =
        iam_repository::get_user_by_login(&state.persistence.postgres, &bootstrap_admin.login)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .or(iam_repository::get_user_by_email(
                &state.persistence.postgres,
                &bootstrap_admin.email,
            )
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?)
    {
        existing.principal_id
    } else {
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
    };

    let catalog = ensure_default_catalog_workspace_and_library(state, principal_id).await?;
    let _ = apply_configured_default_catalog_ai_setup(state, &catalog).await?;
    if let Some(api_token) = bootstrap_admin.api_token.as_deref() {
        ensure_bootstrap_api_token(state, principal_id, api_token).await?;
    }
    Ok(())
}
