use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::catalog::{
        CatalogLibrary, CatalogLibraryConnector, CatalogLifecycleState, CatalogWorkspace,
    },
    infra::repositories::catalog_repository,
    interfaces::http::router_support::{
        ApiError, map_library_create_error, map_workspace_create_error,
    },
    shared::slugs::slugify,
};

#[derive(Debug, Clone)]
pub struct CreateWorkspaceCommand {
    pub slug: Option<String>,
    pub display_name: String,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateWorkspaceCommand {
    pub workspace_id: Uuid,
    pub slug: Option<String>,
    pub display_name: String,
    pub lifecycle_state: CatalogLifecycleState,
}

#[derive(Debug, Clone)]
pub struct CreateLibraryCommand {
    pub workspace_id: Uuid,
    pub slug: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateLibraryCommand {
    pub library_id: Uuid,
    pub slug: Option<String>,
    pub display_name: String,
    pub description: Option<String>,
    pub lifecycle_state: CatalogLifecycleState,
}

#[derive(Debug, Clone)]
pub struct CreateConnectorCommand {
    pub workspace_id: Uuid,
    pub library_id: Uuid,
    pub connector_kind: String,
    pub display_name: String,
    pub configuration_json: serde_json::Value,
    pub sync_mode: String,
    pub created_by_principal_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct UpdateConnectorCommand {
    pub connector_id: Uuid,
    pub display_name: String,
    pub configuration_json: serde_json::Value,
    pub sync_mode: String,
    pub last_sync_requested_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Clone, Default)]
pub struct CatalogService;

impl CatalogService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    pub async fn list_workspaces(
        &self,
        state: &AppState,
        workspace_filter: Option<Uuid>,
    ) -> Result<Vec<CatalogWorkspace>, ApiError> {
        let rows = catalog_repository::list_workspaces(&state.persistence.postgres)
            .await
            .map_err(|_| ApiError::Internal)?;

        Ok(rows
            .into_iter()
            .filter(|row| workspace_filter.is_none_or(|workspace_id| row.id == workspace_id))
            .map(map_workspace_row)
            .collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn get_workspace(
        &self,
        state: &AppState,
        workspace_id: Uuid,
    ) -> Result<CatalogWorkspace, ApiError> {
        let row =
            catalog_repository::get_workspace_by_id(&state.persistence.postgres, workspace_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("workspace", workspace_id))?;
        Ok(map_workspace_row(row)?)
    }

    pub async fn create_workspace(
        &self,
        state: &AppState,
        command: CreateWorkspaceCommand,
    ) -> Result<CatalogWorkspace, ApiError> {
        let display_name = normalize_display_name(&command.display_name, "displayName")?;
        let slug = normalize_optional_slug(command.slug.as_deref(), &display_name);
        let row = catalog_repository::create_workspace(
            &state.persistence.postgres,
            &slug,
            &display_name,
            command.created_by_principal_id,
        )
        .await
        .map_err(|error| map_workspace_create_error(error, &slug))?;
        Ok(map_workspace_row(row)?)
    }

    pub async fn update_workspace(
        &self,
        state: &AppState,
        command: UpdateWorkspaceCommand,
    ) -> Result<CatalogWorkspace, ApiError> {
        let display_name = normalize_display_name(&command.display_name, "displayName")?;
        let slug = normalize_optional_slug(command.slug.as_deref(), &display_name);
        let row = catalog_repository::update_workspace(
            &state.persistence.postgres,
            command.workspace_id,
            &slug,
            &display_name,
            lifecycle_state_as_str(&command.lifecycle_state)?,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("workspace", command.workspace_id))?;
        Ok(map_workspace_row(row)?)
    }

    pub async fn list_libraries(
        &self,
        state: &AppState,
        workspace_id: Uuid,
    ) -> Result<Vec<CatalogLibrary>, ApiError> {
        let rows =
            catalog_repository::list_libraries(&state.persistence.postgres, Some(workspace_id))
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_library_row).collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn get_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<CatalogLibrary, ApiError> {
        let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        Ok(map_library_row(row)?)
    }

    pub async fn create_library(
        &self,
        state: &AppState,
        command: CreateLibraryCommand,
    ) -> Result<CatalogLibrary, ApiError> {
        let display_name = normalize_display_name(&command.display_name, "displayName")?;
        let slug = normalize_optional_slug(command.slug.as_deref(), &display_name);
        let description = normalize_optional_text(command.description.as_deref());
        let row = catalog_repository::create_library(
            &state.persistence.postgres,
            command.workspace_id,
            &slug,
            &display_name,
            description.as_deref(),
            command.created_by_principal_id,
        )
        .await
        .map_err(|error| map_library_create_error(error, command.workspace_id, &slug))?;
        Ok(map_library_row(row)?)
    }

    pub async fn update_library(
        &self,
        state: &AppState,
        command: UpdateLibraryCommand,
    ) -> Result<CatalogLibrary, ApiError> {
        let display_name = normalize_display_name(&command.display_name, "displayName")?;
        let slug = normalize_optional_slug(command.slug.as_deref(), &display_name);
        let description = normalize_optional_text(command.description.as_deref());
        let row = catalog_repository::update_library(
            &state.persistence.postgres,
            command.library_id,
            &slug,
            &display_name,
            description.as_deref(),
            lifecycle_state_as_str(&command.lifecycle_state)?,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("library", command.library_id))?;
        Ok(map_library_row(row)?)
    }

    pub async fn list_connectors(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<CatalogLibraryConnector>, ApiError> {
        let rows =
            catalog_repository::list_connectors_by_library(&state.persistence.postgres, library_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_connector_row).collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn get_connector(
        &self,
        state: &AppState,
        connector_id: Uuid,
    ) -> Result<CatalogLibraryConnector, ApiError> {
        let row =
            catalog_repository::get_connector_by_id(&state.persistence.postgres, connector_id)
                .await
                .map_err(|_| ApiError::Internal)?
                .ok_or_else(|| ApiError::resource_not_found("connector", connector_id))?;
        Ok(map_connector_row(row)?)
    }

    pub async fn create_connector(
        &self,
        state: &AppState,
        command: CreateConnectorCommand,
    ) -> Result<CatalogLibraryConnector, ApiError> {
        let display_name = normalize_display_name(&command.display_name, "displayName")?;
        let row = catalog_repository::create_connector(
            &state.persistence.postgres,
            command.workspace_id,
            command.library_id,
            &command.connector_kind,
            &display_name,
            command.configuration_json,
            &command.sync_mode,
            command.created_by_principal_id,
        )
        .await
        .map_err(map_connector_write_error)?;
        Ok(map_connector_row(row)?)
    }

    pub async fn update_connector(
        &self,
        state: &AppState,
        command: UpdateConnectorCommand,
    ) -> Result<CatalogLibraryConnector, ApiError> {
        let display_name = normalize_display_name(&command.display_name, "displayName")?;
        let row = catalog_repository::update_connector(
            &state.persistence.postgres,
            command.connector_id,
            &display_name,
            command.configuration_json,
            &command.sync_mode,
            command.last_sync_requested_at,
        )
        .await
        .map_err(map_connector_write_error)?
        .ok_or_else(|| ApiError::resource_not_found("connector", command.connector_id))?;
        Ok(map_connector_row(row)?)
    }
}

fn normalize_display_name(value: &str, field_name: &'static str) -> Result<String, ApiError> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(ApiError::BadRequest(format!("{field_name} must not be empty")));
    }
    Ok(normalized.to_string())
}

fn normalize_optional_slug(provided_slug: Option<&str>, display_name: &str) -> String {
    provided_slug
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(slugify)
        .unwrap_or_else(|| slugify(display_name))
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToString::to_string)
}

fn parse_lifecycle_state(value: &str) -> Result<CatalogLifecycleState, ApiError> {
    match value {
        "active" => Ok(CatalogLifecycleState::Active),
        "archived" => Ok(CatalogLifecycleState::Archived),
        "disabled" => Err(ApiError::forbidden_vocabulary("lifecycleState", "disabled", "archived")),
        _ => Err(ApiError::Internal),
    }
}

fn lifecycle_state_as_str(value: &CatalogLifecycleState) -> Result<&'static str, ApiError> {
    match value {
        CatalogLifecycleState::Active => Ok("active"),
        CatalogLifecycleState::Disabled => {
            Err(ApiError::forbidden_vocabulary("lifecycleState", "disabled", "archived"))
        }
        CatalogLifecycleState::Archived => Ok("archived"),
    }
}

fn map_workspace_row(
    row: catalog_repository::CatalogWorkspaceRow,
) -> Result<CatalogWorkspace, ApiError> {
    Ok(CatalogWorkspace {
        id: row.id,
        slug: row.slug,
        display_name: row.display_name,
        lifecycle_state: parse_lifecycle_state(&row.lifecycle_state)?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn map_library_row(row: catalog_repository::CatalogLibraryRow) -> Result<CatalogLibrary, ApiError> {
    Ok(CatalogLibrary {
        id: row.id,
        workspace_id: row.workspace_id,
        slug: row.slug,
        display_name: row.display_name,
        description: row.description,
        lifecycle_state: parse_lifecycle_state(&row.lifecycle_state)?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn map_connector_row(
    row: catalog_repository::CatalogLibraryConnectorRow,
) -> Result<CatalogLibraryConnector, ApiError> {
    Ok(CatalogLibraryConnector {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        connector_kind: row.connector_kind,
        display_name: row.display_name,
        configuration_json: row.configuration_json,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn map_connector_write_error(error: sqlx::Error) -> ApiError {
    match error {
        sqlx::Error::Database(database_error) if database_error.is_foreign_key_violation() => {
            ApiError::NotFound("workspace or library not found for connector".to_string())
        }
        sqlx::Error::Database(database_error) if database_error.is_check_violation() => {
            ApiError::BadRequest("connector payload violated catalog constraints".to_string())
        }
        _ => ApiError::Internal,
    }
}
