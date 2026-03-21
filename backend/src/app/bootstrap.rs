use argon2::{
    Argon2,
    password_hash::{PasswordHasher, SaltString, rand_core::OsRng},
};
use tracing::info;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    infra::repositories::{self, catalog_repository, iam_repository},
    interfaces::http::router_support::ApiError,
};

fn hash_password(password: &str) -> Result<String, ApiError> {
    Argon2::default()
        .hash_password(password.as_bytes(), &SaltString::generate(&mut OsRng))
        .map(|hash| hash.to_string())
        .map_err(|_| ApiError::Internal)
}

async fn ensure_default_workspace_and_library(
    state: &AppState,
    user_id: Uuid,
    role_label: &str,
) -> Result<(repositories::WorkspaceRow, repositories::ProjectRow), ApiError> {
    let workspace = repositories::find_or_create_default_workspace(&state.persistence.postgres)
        .await
        .map_err(|_| ApiError::Internal)?;

    repositories::ensure_workspace_member(
        &state.persistence.postgres,
        workspace.id,
        user_id,
        role_label,
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    let project =
        repositories::find_or_create_default_project(&state.persistence.postgres, workspace.id)
            .await
            .map_err(|_| ApiError::Internal)?;

    repositories::ensure_project_access_grant(
        &state.persistence.postgres,
        project.id,
        user_id,
        "write",
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok((workspace, project))
}

async fn ensure_default_catalog_workspace_and_library(
    state: &AppState,
    principal_id: Uuid,
) -> Result<(), ApiError> {
    let workspace = if let Some(existing) =
        catalog_repository::get_workspace_by_slug(&state.persistence.postgres, "default")
            .await
            .map_err(|_| ApiError::Internal)?
    {
        existing
    } else {
        catalog_repository::create_workspace(
            &state.persistence.postgres,
            "default",
            "Default workspace",
            Some(principal_id),
        )
        .await
        .map_err(|_| ApiError::Internal)?
    };

    let has_library =
        !catalog_repository::list_libraries(&state.persistence.postgres, Some(workspace.id))
            .await
            .map_err(|_| ApiError::Internal)?
            .is_empty();
    if !has_library {
        catalog_repository::create_library(
            &state.persistence.postgres,
            workspace.id,
            "default-library",
            "Default library",
            Some("Backstage default library for the primary documents and ask flow"),
            Some(principal_id),
        )
        .await
        .map_err(|_| ApiError::Internal)?;
    }

    iam_repository::upsert_workspace_membership(
        &state.persistence.postgres,
        workspace.id,
        principal_id,
        "active",
    )
    .await
    .map_err(|_| ApiError::Internal)?;

    Ok(())
}

pub async fn ensure_bootstrap_admin(state: &AppState) -> Result<(), ApiError> {
    let Some(bootstrap_admin) = state.ui_bootstrap_admin.clone() else {
        return Ok(());
    };

    let user = if let Some(existing) =
        repositories::get_ui_user_by_login(&state.persistence.postgres, &bootstrap_admin.login)
            .await
            .map_err(|_| ApiError::Internal)?
            .or(repositories::get_ui_user_by_email(
                &state.persistence.postgres,
                &bootstrap_admin.email,
            )
            .await
            .map_err(|_| ApiError::Internal)?)
    {
        existing
    } else {
        let should_create = if state.settings.has_explicit_ui_bootstrap_admin() {
            true
        } else if state.settings.environment.eq_ignore_ascii_case("local") {
            true
        } else {
            repositories::count_ui_users(&state.persistence.postgres)
                .await
                .map_err(|_| ApiError::Internal)?
                == 0
        };

        if !should_create {
            return Ok(());
        }

        let password_hash = hash_password(&bootstrap_admin.password)?;
        let created = repositories::create_ui_user(
            &state.persistence.postgres,
            &bootstrap_admin.login,
            &bootstrap_admin.email,
            &bootstrap_admin.display_name,
            "Owner",
            &password_hash,
            &state.ui_runtime.default_locale,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        info!(
            login = %created.login,
            email = %created.email,
            user_id = %created.id,
            explicit = state.settings.has_explicit_ui_bootstrap_admin(),
            "created bootstrap ui admin",
        );
        created
    };

    ensure_default_workspace_and_library(state, user.id, &user.role_label).await?;
    Ok(())
}

pub async fn ensure_canonical_bootstrap_admin(state: &AppState) -> Result<(), ApiError> {
    let Some(bootstrap_admin) = state.ui_bootstrap_admin.clone() else {
        return Ok(());
    };

    let principal_id = if let Some(existing) =
        iam_repository::get_user_by_email(&state.persistence.postgres, &bootstrap_admin.email)
            .await
            .map_err(|_| ApiError::Internal)?
    {
        existing.principal_id
    } else {
        let password_hash = hash_password(&bootstrap_admin.password)?;
        let claimed = iam_repository::claim_bootstrap_user(
            &state.persistence.postgres,
            &bootstrap_admin.email,
            &bootstrap_admin.display_name,
            &password_hash,
        )
        .await
        .map_err(|_| ApiError::Internal)?;
        let Some(claimed) = claimed else {
            return Ok(());
        };
        claimed.principal_id
    };

    ensure_default_catalog_workspace_and_library(state, principal_id).await
}
