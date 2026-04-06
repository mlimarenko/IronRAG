use std::collections::{HashMap, HashSet};

use tracing::error;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::ai::AiBindingPurpose,
    domains::catalog::{
        CatalogLibrary, CatalogLibraryConnector, CatalogLibraryIngestionReadiness,
        CatalogLifecycleState, CatalogWorkspace,
    },
    infra::repositories::{ai_repository, catalog_repository},
    interfaces::http::router_support::{
        ApiError, map_library_create_error, map_workspace_create_error,
    },
    shared::slugs::slugify,
};

const INGEST_REQUIRED_BINDINGS: &[(AiBindingPurpose, &str)] =
    &[(AiBindingPurpose::ExtractGraph, "extract_graph")];

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

#[derive(Debug, Clone, Copy)]
enum CatalogLifecycleError {
    DisabledVocabulary,
    InvalidValue,
}

impl CatalogLifecycleError {
    fn into_request_error(self) -> ApiError {
        match self {
            Self::DisabledVocabulary => {
                ApiError::forbidden_vocabulary("lifecycleState", "disabled", "archived")
            }
            Self::InvalidValue => ApiError::Internal,
        }
    }

    fn into_persisted_error(self) -> ApiError {
        match self {
            Self::DisabledVocabulary | Self::InvalidValue => ApiError::Internal,
        }
    }
}

impl CatalogService {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Lists workspaces visible to the service and optionally filters to one workspace id.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the catalog repository query fails.
    pub async fn list_workspaces(
        &self,
        state: &AppState,
        workspace_filter: Option<Uuid>,
    ) -> Result<Vec<CatalogWorkspace>, ApiError> {
        let rows = catalog_repository::list_workspaces(&state.persistence.postgres)
            .await
            .map_err(|_| ApiError::Internal)?;

        rows.into_iter()
            .filter(|row| workspace_filter.is_none_or(|workspace_id| row.id == workspace_id))
            .map(map_workspace_row)
            .collect()
    }

    /// Loads a single workspace by id.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository read fails or [`ApiError::NotFound`]
    /// when the workspace does not exist.
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
        map_workspace_row(row)
    }

    /// Creates a workspace using canonical slug and lifecycle defaults.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, the repository write fails, or the resulting
    /// row cannot be mapped back into the canonical domain shape.
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
        map_workspace_row(row)
    }

    /// Updates a workspace display payload and lifecycle state.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, the repository update fails, or the workspace
    /// does not exist.
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
            lifecycle_state_as_str(&command.lifecycle_state)
                .map_err(CatalogLifecycleError::into_request_error)?,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("workspace", command.workspace_id))?;
        map_workspace_row(row)
    }

    /// Deletes a workspace and its stashed storage snapshot.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the workspace cannot be loaded, stashed, or deleted.
    pub async fn delete_workspace(
        &self,
        state: &AppState,
        workspace_id: Uuid,
    ) -> Result<CatalogWorkspace, ApiError> {
        let workspace = self.get_workspace(state, workspace_id).await?;
        let stashed_directory =
            state.content_storage.stash_workspace_storage(workspace.id).await.map_err(
                |storage_error| {
                    error!(
                        workspace_id = %workspace.id,
                        error = ?storage_error,
                        "failed to stash workspace storage before delete"
                    );
                    ApiError::Internal
                },
            )?;

        let rows_affected =
            match catalog_repository::delete_workspace(&state.persistence.postgres, workspace.id)
                .await
            {
                Ok(rows_affected) => rows_affected,
                Err(delete_error) => {
                    restore_stashed_directory(state, stashed_directory.as_ref()).await;
                    error!(
                        workspace_id = %workspace.id,
                        error = ?delete_error,
                        "failed to delete workspace"
                    );
                    return Err(ApiError::Internal);
                }
            };

        if rows_affected == 0 {
            restore_stashed_directory(state, stashed_directory.as_ref()).await;
            return Err(ApiError::resource_not_found("workspace", workspace.id));
        }

        purge_stashed_directory(state, stashed_directory.as_ref()).await;
        Ok(workspace)
    }

    /// Lists libraries for a workspace together with ingestion readiness.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when repository reads or readiness derivation fail.
    pub async fn list_libraries(
        &self,
        state: &AppState,
        workspace_id: Uuid,
    ) -> Result<Vec<CatalogLibrary>, ApiError> {
        let rows =
            catalog_repository::list_libraries(&state.persistence.postgres, Some(workspace_id))
                .await
                .map_err(|_| ApiError::Internal)?;
        let readiness_by_library = self
            .list_library_ingestion_readiness(
                state,
                &rows.iter().map(|row| row.id).collect::<Vec<_>>(),
            )
            .await?;
        rows.into_iter()
            .map(|row| {
                let library_id = row.id;
                map_library_row(
                    row,
                    readiness_by_library
                        .get(&library_id)
                        .cloned()
                        .unwrap_or_else(default_ingestion_readiness),
                )
            })
            .collect()
    }

    /// Loads a single library together with its ingestion readiness.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the repository read fails, readiness derivation fails, or the
    /// library does not exist.
    pub async fn get_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<CatalogLibrary, ApiError> {
        let row = catalog_repository::get_library_by_id(&state.persistence.postgres, library_id)
            .await
            .map_err(|_| ApiError::Internal)?
            .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        let readiness = self.get_library_ingestion_readiness(state, row.id).await?;
        map_library_row(row, readiness)
    }

    /// Creates a library and provisions its runtime AI profile.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, persistence fails, runtime profile creation
    /// fails, or the persisted row cannot be mapped.
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
        state
            .canonical_services
            .ai_catalog
            .ensure_library_runtime_profile(
                state,
                command.workspace_id,
                row.id,
                command.created_by_principal_id,
            )
            .await?;
        let readiness = self.get_library_ingestion_readiness(state, row.id).await?;
        map_library_row(row, readiness)
    }

    /// Updates a library display payload, description, and lifecycle state.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, the repository update fails, readiness
    /// derivation fails, or the library does not exist.
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
            lifecycle_state_as_str(&command.lifecycle_state)
                .map_err(CatalogLifecycleError::into_request_error)?,
        )
        .await
        .map_err(|_| ApiError::Internal)?
        .ok_or_else(|| ApiError::resource_not_found("library", command.library_id))?;
        let readiness = self.get_library_ingestion_readiness(state, row.id).await?;
        map_library_row(row, readiness)
    }

    /// Deletes a library and its stashed storage snapshot.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the library cannot be loaded, stashed, or deleted.
    pub async fn delete_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<CatalogLibrary, ApiError> {
        let library = self.get_library(state, library_id).await?;
        let stashed_directory = state
            .content_storage
            .stash_library_storage(library.workspace_id, library.id)
            .await
            .map_err(|storage_error| {
                error!(
                    workspace_id = %library.workspace_id,
                    library_id = %library.id,
                    error = ?storage_error,
                    "failed to stash library storage before delete"
                );
                ApiError::Internal
            })?;

        let rows_affected =
            match catalog_repository::delete_library(&state.persistence.postgres, library.id).await
            {
                Ok(rows_affected) => rows_affected,
                Err(delete_error) => {
                    restore_stashed_directory(state, stashed_directory.as_ref()).await;
                    error!(
                        workspace_id = %library.workspace_id,
                        library_id = %library.id,
                        error = ?delete_error,
                        "failed to delete library"
                    );
                    return Err(ApiError::Internal);
                }
            };

        if rows_affected == 0 {
            restore_stashed_directory(state, stashed_directory.as_ref()).await;
            return Err(ApiError::resource_not_found("library", library.id));
        }

        purge_stashed_directory(state, stashed_directory.as_ref()).await;
        Ok(library)
    }

    /// Loads ingestion readiness for one library.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when readiness derivation fails.
    pub async fn get_library_ingestion_readiness(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<CatalogLibraryIngestionReadiness, ApiError> {
        Ok(self
            .list_library_ingestion_readiness(state, &[library_id])
            .await?
            .remove(&library_id)
            .unwrap_or_else(default_ingestion_readiness))
    }

    /// Derives ingestion readiness for a set of libraries from active AI bindings.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the AI binding query fails.
    pub async fn list_library_ingestion_readiness(
        &self,
        state: &AppState,
        library_ids: &[Uuid],
    ) -> Result<HashMap<Uuid, CatalogLibraryIngestionReadiness>, ApiError> {
        if library_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let rows = ai_repository::list_active_binding_purposes_for_libraries(
            &state.persistence.postgres,
            library_ids,
        )
        .await
        .map_err(|_| ApiError::Internal)?;

        let mut purposes_by_library = HashMap::<Uuid, HashSet<String>>::new();
        for row in rows {
            purposes_by_library.entry(row.library_id).or_default().insert(row.binding_purpose);
        }

        let mut readiness = HashMap::with_capacity(library_ids.len());
        for library_id in library_ids {
            let present = purposes_by_library.get(library_id);
            let missing_binding_purposes = INGEST_REQUIRED_BINDINGS
                .iter()
                .filter_map(|(purpose, key)| {
                    let has_binding = present.is_some_and(|bindings| bindings.contains(*key));
                    (!has_binding).then_some(*purpose)
                })
                .collect::<Vec<_>>();
            readiness.insert(
                *library_id,
                CatalogLibraryIngestionReadiness {
                    ready: missing_binding_purposes.is_empty(),
                    missing_binding_purposes,
                },
            );
        }

        Ok(readiness)
    }

    /// Lists connectors attached to a library.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the repository query fails.
    pub async fn list_connectors(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<Vec<CatalogLibraryConnector>, ApiError> {
        let rows =
            catalog_repository::list_connectors_by_library(&state.persistence.postgres, library_id)
                .await
                .map_err(|_| ApiError::Internal)?;
        Ok(rows.into_iter().map(map_connector_row).collect())
    }

    /// Loads one connector by id.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the repository read fails or the connector does not exist.
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
        Ok(map_connector_row(row))
    }

    /// Creates a connector for a library.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails or the repository write fails.
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
        Ok(map_connector_row(row))
    }

    /// Updates an existing connector payload.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, the repository update fails, or the connector
    /// does not exist.
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
        Ok(map_connector_row(row))
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
        .map_or_else(|| slugify(display_name), slugify)
}

fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(ToString::to_string)
}

fn parse_lifecycle_state(value: &str) -> Result<CatalogLifecycleState, CatalogLifecycleError> {
    match value {
        "active" => Ok(CatalogLifecycleState::Active),
        "archived" => Ok(CatalogLifecycleState::Archived),
        "disabled" => Err(CatalogLifecycleError::DisabledVocabulary),
        _ => Err(CatalogLifecycleError::InvalidValue),
    }
}

fn lifecycle_state_as_str(
    value: &CatalogLifecycleState,
) -> Result<&'static str, CatalogLifecycleError> {
    match value {
        CatalogLifecycleState::Active => Ok("active"),
        CatalogLifecycleState::Disabled => Err(CatalogLifecycleError::DisabledVocabulary),
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
        lifecycle_state: parse_lifecycle_state(&row.lifecycle_state)
            .map_err(CatalogLifecycleError::into_persisted_error)?,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn default_ingestion_readiness() -> CatalogLibraryIngestionReadiness {
    CatalogLibraryIngestionReadiness {
        ready: false,
        missing_binding_purposes: INGEST_REQUIRED_BINDINGS
            .iter()
            .map(|(purpose, _)| *purpose)
            .collect(),
    }
}

fn map_library_row(
    row: catalog_repository::CatalogLibraryRow,
    ingestion_readiness: CatalogLibraryIngestionReadiness,
) -> Result<CatalogLibrary, ApiError> {
    Ok(CatalogLibrary {
        id: row.id,
        workspace_id: row.workspace_id,
        slug: row.slug,
        display_name: row.display_name,
        description: row.description,
        lifecycle_state: parse_lifecycle_state(&row.lifecycle_state)
            .map_err(CatalogLifecycleError::into_persisted_error)?,
        ingestion_readiness,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn map_connector_row(
    row: catalog_repository::CatalogLibraryConnectorRow,
) -> CatalogLibraryConnector {
    CatalogLibraryConnector {
        id: row.id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        connector_kind: row.connector_kind,
        display_name: row.display_name,
        configuration_json: row.configuration_json,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
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

async fn restore_stashed_directory(
    state: &AppState,
    stashed_directory: Option<&crate::services::content_storage::StashedContentDirectory>,
) {
    if let Some(stashed_directory) = stashed_directory
        && let Err(restore_error) =
            state.content_storage.restore_stashed_directory(stashed_directory).await
    {
        error!(
            original_path = %stashed_directory.original_path().display(),
            stashed_path = %stashed_directory.stashed_path().display(),
            error = ?restore_error,
            "failed to restore stashed content directory"
        );
    }
}

async fn purge_stashed_directory(
    state: &AppState,
    stashed_directory: Option<&crate::services::content_storage::StashedContentDirectory>,
) {
    if let Some(stashed_directory) = stashed_directory
        && let Err(purge_error) =
            state.content_storage.purge_stashed_directory(stashed_directory).await
    {
        error!(
            original_path = %stashed_directory.original_path().display(),
            stashed_path = %stashed_directory.stashed_path().display(),
            error = ?purge_error,
            "failed to purge stashed content directory"
        );
    }
}
