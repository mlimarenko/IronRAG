use std::collections::{HashMap, HashSet};

use tracing::error;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::recognition::LibraryRecognitionPolicy,
    domains::{
        ai::AiBindingPurpose,
        catalog::{
            CatalogLibrary, CatalogLibraryConnector, CatalogLibraryIngestionReadiness,
            CatalogLibraryRuntimeReadiness, CatalogLifecycleState, CatalogWorkspace,
            ChunkingTemplate,
        },
        ops::OpsAsyncOperationStatus,
    },
    infra::repositories::{ai_repository, catalog_repository},
    interfaces::http::router_support::{
        ApiError, map_library_create_error, map_workspace_create_error,
    },
    services::content::service::snapshot::clear_library_arango_footprint,
    services::ops::service::{CreateAsyncOperationCommand, UpdateAsyncOperationCommand},
    shared::slugs::slugify,
    shared::web::ingest::{WebIngestPolicy, validate_web_ingest_policy},
};

const INGEST_REQUIRED_BINDINGS: &[AiBindingPurpose] =
    &[AiBindingPurpose::ExtractGraph, AiBindingPurpose::EmbedChunk];
const RUNTIME_REQUIRED_BINDINGS: &[AiBindingPurpose] = &[
    AiBindingPurpose::ExtractGraph,
    AiBindingPurpose::EmbedChunk,
    AiBindingPurpose::QueryRetrieve,
    AiBindingPurpose::QueryCompile,
    AiBindingPurpose::QueryAnswer,
];

#[derive(Debug, Clone)]
struct CatalogLibraryBindingReadiness {
    ingestion: CatalogLibraryIngestionReadiness,
    runtime: CatalogLibraryRuntimeReadiness,
}

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
    pub extraction_prompt: Option<String>,
    pub lifecycle_state: CatalogLifecycleState,
    pub include_document_hint_in_mcp_answers: bool,
}

#[derive(Debug, Clone)]
pub struct UpdateLibraryWebIngestPolicyCommand {
    pub library_id: Uuid,
    pub web_ingest_policy: WebIngestPolicy,
}

#[derive(Debug, Clone)]
pub struct UpdateLibraryRecognitionPolicyCommand {
    pub library_id: Uuid,
    pub recognition_policy: LibraryRecognitionPolicy,
}

#[derive(Debug, Clone)]
pub struct CatalogDeletionAdmission {
    pub operation_id: Uuid,
    pub workspace_id: Uuid,
    pub library_id: Option<Uuid>,
    pub display_name: String,
    pub should_start_worker: bool,
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        // Capture the library set BEFORE the PG cascade drops the
        // catalog_library rows — once they are gone we have no way of
        // discovering which libraries used to belong to this workspace,
        // and the per-library Arango footprint must still be wiped.
        let libraries_before_delete: Vec<Uuid> =
            catalog_repository::list_libraries(&state.persistence.postgres, Some(workspace.id))
                .await
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
                .into_iter()
                .map(|row| row.id)
                .collect();
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

        // The PG `delete_workspace` cascade only touches PG rows. Every
        // library that lived under this workspace also owns Arango data
        // (knowledge_document / knowledge_chunk / per-dim vector shards
        // plus edges) which has no FK back to PG. Without an explicit
        // sweep these rows become orphans the next sync delete will
        // never see — exactly the leak that left 13k orphan documents
        // on prod before this fix. Fire a sweep per library that lived
        // under the workspace; failures are loud-logged but do not
        // resurrect the workspace row.
        if state.settings.knowledge_plane_backend == "arango" {
            for orphan_library_id in libraries_before_delete {
                if let Err(arango_error) =
                    clear_library_arango_footprint(state, orphan_library_id).await
                {
                    error!(
                        workspace_id = %workspace.id,
                        library_id = %orphan_library_id,
                        error = ?arango_error,
                        "failed to cascade workspace delete into arango library footprint"
                    );
                }
            }
        }

        purge_stashed_directory(state, stashed_directory.as_ref()).await;
        Ok(workspace)
    }

    /// Admits a workspace deletion as a canonical async operation.
    ///
    /// The foreground HTTP path persists only the operation row. The destructive
    /// storage/database work is executed by [`Self::execute_workspace_deletion`].
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the workspace cannot be loaded or the
    /// operation row cannot be created.
    pub async fn admit_workspace_deletion(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        requested_by_principal_id: Option<Uuid>,
    ) -> Result<CatalogDeletionAdmission, ApiError> {
        let workspace = self.get_workspace(state, workspace_id).await?;
        if let Some(operation) = state
            .canonical_services
            .ops
            .get_latest_async_operation_by_subject(state, "catalog_workspace", workspace.id)
            .await?
            .filter(|operation| operation.operation_kind == "delete_workspace")
            .filter(|operation| catalog_delete_operation_is_active(operation.status))
        {
            return Ok(CatalogDeletionAdmission {
                operation_id: operation.id,
                workspace_id: workspace.id,
                library_id: None,
                display_name: workspace.display_name,
                should_start_worker: false,
            });
        }
        let operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: workspace.id,
                    library_id: None,
                    operation_kind: "delete_workspace".to_string(),
                    surface_kind: "rest".to_string(),
                    requested_by_principal_id,
                    status: "processing".to_string(),
                    subject_kind: "catalog_workspace".to_string(),
                    subject_id: Some(workspace.id),
                    parent_async_operation_id: None,
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;
        Ok(CatalogDeletionAdmission {
            operation_id: operation.id,
            workspace_id: workspace.id,
            library_id: None,
            display_name: workspace.display_name,
            should_start_worker: true,
        })
    }

    /// Executes an admitted workspace deletion and settles its async operation.
    ///
    /// # Errors
    ///
    /// Returns the original delete error after marking the async operation as failed.
    pub async fn execute_workspace_deletion(
        &self,
        state: &AppState,
        operation_id: Uuid,
        workspace_id: Uuid,
    ) -> Result<CatalogWorkspace, ApiError> {
        match self.delete_workspace(state, workspace_id).await {
            Ok(workspace) => {
                settle_catalog_delete_operation(state, operation_id, "ready", None).await;
                Ok(workspace)
            }
            Err(error) => {
                settle_catalog_delete_operation(
                    state,
                    operation_id,
                    "failed",
                    Some("catalog_delete_failed".to_string()),
                )
                .await;
                Err(error)
            }
        }
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let policies_by_library = parse_library_policies(&rows)?;
        let readiness_by_library =
            self.list_library_binding_readiness(state, &policies_by_library).await?;
        rows.into_iter()
            .map(|row| {
                let library_id = row.id;
                let readiness = readiness_by_library
                    .get(&library_id)
                    .cloned()
                    .unwrap_or_else(default_binding_readiness);
                map_library_row(row, readiness.ingestion, readiness.runtime)
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
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
            .ok_or_else(|| ApiError::resource_not_found("library", library_id))?;
        let policy = parse_library_policy(&row)?;
        let readiness = self.get_library_binding_readiness(state, row.id, &policy).await?;
        map_library_row(row, readiness.ingestion, readiness.runtime)
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
        let recognition_policy = state
            .settings
            .default_recognition_policy()
            .to_json()
            .map_err(|_| ApiError::Internal)?;
        let row = catalog_repository::create_library_with_recognition_policy(
            &state.persistence.postgres,
            command.workspace_id,
            &slug,
            &display_name,
            description.as_deref(),
            recognition_policy,
            command.created_by_principal_id,
        )
        .await
        .map_err(|error| map_library_create_error(error, command.workspace_id, &slug))?;
        let policy = parse_library_policy(&row)?;
        let readiness = self.get_library_binding_readiness(state, row.id, &policy).await?;
        map_library_row(row, readiness.ingestion, readiness.runtime)
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
        let extraction_prompt =
            command.extraction_prompt.as_deref().map(str::trim).filter(|value| !value.is_empty());
        let row = catalog_repository::update_library(
            &state.persistence.postgres,
            command.library_id,
            &slug,
            &display_name,
            description.as_deref(),
            extraction_prompt,
            lifecycle_state_as_str(&command.lifecycle_state)
                .map_err(CatalogLifecycleError::into_request_error)?,
            command.include_document_hint_in_mcp_answers,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("library", command.library_id))?;
        let policy = parse_library_policy(&row)?;
        let readiness = self.get_library_binding_readiness(state, row.id, &policy).await?;
        map_library_row(row, readiness.ingestion, readiness.runtime)
    }

    /// Updates the reusable web-ingest policy owned by one library.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, the repository update fails, readiness
    /// derivation fails, or the library does not exist.
    pub async fn update_library_web_ingest_policy(
        &self,
        state: &AppState,
        command: UpdateLibraryWebIngestPolicyCommand,
    ) -> Result<CatalogLibrary, ApiError> {
        let web_ingest_policy =
            validate_web_ingest_policy(command.web_ingest_policy).map_err(ApiError::BadRequest)?;
        let policy_json =
            serde_json::to_value(web_ingest_policy).map_err(|_| ApiError::Internal)?;
        let row = catalog_repository::update_library_web_ingest_policy(
            &state.persistence.postgres,
            command.library_id,
            policy_json,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("library", command.library_id))?;
        let policy = parse_library_policy(&row)?;
        let readiness = self.get_library_binding_readiness(state, row.id, &policy).await?;
        map_library_row(row, readiness.ingestion, readiness.runtime)
    }

    /// Updates the recognition policy owned by one library.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, the repository update fails, readiness
    /// derivation fails, or the library does not exist.
    pub async fn update_library_recognition_policy(
        &self,
        state: &AppState,
        command: UpdateLibraryRecognitionPolicyCommand,
    ) -> Result<CatalogLibrary, ApiError> {
        command.recognition_policy.validate().map_err(ApiError::BadRequest)?;
        let policy_json = command.recognition_policy.to_json().map_err(|_| ApiError::Internal)?;
        let row = catalog_repository::update_library_recognition_policy(
            &state.persistence.postgres,
            command.library_id,
            policy_json,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("library", command.library_id))?;
        let policy = parse_library_policy(&row)?;
        let readiness = self.get_library_binding_readiness(state, row.id, &policy).await?;
        map_library_row(row, readiness.ingestion, readiness.runtime)
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

        // The PG cascade only removes Postgres rows. Knowledge vertex
        // collections, every per-dim vector shard, and the edge
        // collections in Arango have no FK back to PG, so without this
        // explicit sweep they become orphans the next sync delete will
        // never see again. Run the same canonical sweep snapshot-
        // restore-replace uses; failures are loud-logged but do not
        // resurrect the PG row.
        if state.settings.knowledge_plane_backend == "arango" {
            if let Err(arango_error) = clear_library_arango_footprint(state, library.id).await {
                error!(
                    workspace_id = %library.workspace_id,
                    library_id = %library.id,
                    error = ?arango_error,
                    "failed to cascade library delete into arango library footprint"
                );
            }
        }

        purge_stashed_directory(state, stashed_directory.as_ref()).await;
        Ok(library)
    }

    /// Admits a library deletion as a canonical async operation.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when the library cannot be loaded, does not belong
    /// to the path workspace, or the operation row cannot be created.
    pub async fn admit_library_deletion(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        requested_by_principal_id: Option<Uuid>,
    ) -> Result<CatalogDeletionAdmission, ApiError> {
        let library = self.get_library(state, library_id).await?;
        if library.workspace_id != workspace_id {
            return Err(ApiError::resource_not_found("library", library_id));
        }
        if let Some(operation) = state
            .canonical_services
            .ops
            .get_latest_async_operation_by_subject(state, "catalog_library", library.id)
            .await?
            .filter(|operation| operation.operation_kind == "delete_library")
            .filter(|operation| catalog_delete_operation_is_active(operation.status))
        {
            return Ok(CatalogDeletionAdmission {
                operation_id: operation.id,
                workspace_id: library.workspace_id,
                library_id: Some(library.id),
                display_name: library.display_name,
                should_start_worker: false,
            });
        }
        let operation = state
            .canonical_services
            .ops
            .create_async_operation(
                state,
                CreateAsyncOperationCommand {
                    workspace_id: library.workspace_id,
                    library_id: Some(library.id),
                    operation_kind: "delete_library".to_string(),
                    surface_kind: "rest".to_string(),
                    requested_by_principal_id,
                    status: "processing".to_string(),
                    subject_kind: "catalog_library".to_string(),
                    subject_id: Some(library.id),
                    parent_async_operation_id: None,
                    completed_at: None,
                    failure_code: None,
                },
            )
            .await?;
        Ok(CatalogDeletionAdmission {
            operation_id: operation.id,
            workspace_id: library.workspace_id,
            library_id: Some(library.id),
            display_name: library.display_name,
            should_start_worker: true,
        })
    }

    /// Executes an admitted library deletion and settles its async operation.
    ///
    /// # Errors
    ///
    /// Returns the original delete error after marking the async operation as failed.
    pub async fn execute_library_deletion(
        &self,
        state: &AppState,
        operation_id: Uuid,
        library_id: Uuid,
    ) -> Result<CatalogLibrary, ApiError> {
        match self.delete_library(state, library_id).await {
            Ok(library) => {
                settle_catalog_delete_operation(state, operation_id, "ready", None).await;
                Ok(library)
            }
            Err(error) => {
                settle_catalog_delete_operation(
                    state,
                    operation_id,
                    "failed",
                    Some("catalog_delete_failed".to_string()),
                )
                .await;
                Err(error)
            }
        }
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
        recognition_policy: &LibraryRecognitionPolicy,
    ) -> Result<CatalogLibraryIngestionReadiness, ApiError> {
        Ok(self
            .get_library_binding_readiness(state, library_id, recognition_policy)
            .await?
            .ingestion)
    }

    async fn get_library_binding_readiness(
        &self,
        state: &AppState,
        library_id: Uuid,
        recognition_policy: &LibraryRecognitionPolicy,
    ) -> Result<CatalogLibraryBindingReadiness, ApiError> {
        Ok(self
            .list_library_binding_readiness(state, &[(library_id, recognition_policy.clone())])
            .await?
            .remove(&library_id)
            .unwrap_or_else(default_binding_readiness))
    }

    /// Derives ingestion readiness for a set of libraries from active AI bindings.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the AI binding query fails.
    pub async fn list_library_ingestion_readiness(
        &self,
        state: &AppState,
        library_policies: &[(Uuid, LibraryRecognitionPolicy)],
    ) -> Result<HashMap<Uuid, CatalogLibraryIngestionReadiness>, ApiError> {
        Ok(self
            .list_library_binding_readiness(state, library_policies)
            .await?
            .into_iter()
            .map(|(library_id, readiness)| (library_id, readiness.ingestion))
            .collect())
    }

    /// Derives canonical binding readiness for shell/library summaries from active AI bindings.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Internal`] when the AI binding query fails.
    async fn list_library_binding_readiness(
        &self,
        state: &AppState,
        library_policies: &[(Uuid, LibraryRecognitionPolicy)],
    ) -> Result<HashMap<Uuid, CatalogLibraryBindingReadiness>, ApiError> {
        if library_policies.is_empty() {
            return Ok(HashMap::new());
        }
        let library_ids =
            library_policies.iter().map(|(library_id, _)| *library_id).collect::<Vec<_>>();

        let rows = ai_repository::list_effective_binding_purposes_for_libraries(
            &state.persistence.postgres,
            &library_ids,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;

        let mut purposes_by_library = HashMap::<Uuid, HashSet<String>>::new();
        for row in rows {
            purposes_by_library.entry(row.library_id).or_default().insert(row.binding_purpose);
        }

        let mut readiness = HashMap::with_capacity(library_policies.len());
        for (library_id, _recognition_policy) in library_policies {
            let present = purposes_by_library.get(library_id);
            let missing_binding_purposes =
                missing_required_purposes(present, INGEST_REQUIRED_BINDINGS);
            readiness.insert(
                *library_id,
                CatalogLibraryBindingReadiness {
                    ingestion: CatalogLibraryIngestionReadiness {
                        ready: missing_binding_purposes.is_empty(),
                        missing_binding_purposes: missing_binding_purposes.clone(),
                    },
                    runtime: runtime_readiness(present, &missing_binding_purposes),
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
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
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?
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
        missing_binding_purposes: INGEST_REQUIRED_BINDINGS.to_vec(),
    }
}

fn default_binding_readiness() -> CatalogLibraryBindingReadiness {
    CatalogLibraryBindingReadiness {
        ingestion: default_ingestion_readiness(),
        runtime: runtime_readiness(None, INGEST_REQUIRED_BINDINGS),
    }
}

fn runtime_readiness(
    present: Option<&HashSet<String>>,
    ingest_missing: &[AiBindingPurpose],
) -> CatalogLibraryRuntimeReadiness {
    let mut missing_binding_purposes = ingest_missing.to_vec();
    for purpose in missing_required_purposes(present, RUNTIME_REQUIRED_BINDINGS) {
        if !missing_binding_purposes.contains(&purpose) {
            missing_binding_purposes.push(purpose);
        }
    }
    CatalogLibraryRuntimeReadiness { missing_binding_purposes }
}

fn missing_required_purposes(
    present: Option<&HashSet<String>>,
    required: &[AiBindingPurpose],
) -> Vec<AiBindingPurpose> {
    required
        .iter()
        .filter_map(|purpose| {
            let has_binding = present.is_some_and(|bindings| bindings.contains(purpose.as_str()));
            (!has_binding).then_some(*purpose)
        })
        .collect()
}

fn parse_library_policy(
    row: &catalog_repository::CatalogLibraryRow,
) -> Result<LibraryRecognitionPolicy, ApiError> {
    LibraryRecognitionPolicy::from_json(row.recognition_policy.clone()).map_err(|error| {
        ApiError::internal_with_log(anyhow::anyhow!(error), "invalid persisted recognition policy")
    })
}

fn parse_library_policies(
    rows: &[catalog_repository::CatalogLibraryRow],
) -> Result<Vec<(Uuid, LibraryRecognitionPolicy)>, ApiError> {
    rows.iter().map(|row| parse_library_policy(row).map(|policy| (row.id, policy))).collect()
}

fn map_library_row(
    row: catalog_repository::CatalogLibraryRow,
    ingestion_readiness: CatalogLibraryIngestionReadiness,
    runtime_readiness: CatalogLibraryRuntimeReadiness,
) -> Result<CatalogLibrary, ApiError> {
    let recognition_policy = LibraryRecognitionPolicy::from_json(row.recognition_policy)
        .map_err(|_| ApiError::Internal)?;
    Ok(CatalogLibrary {
        id: row.id,
        workspace_id: row.workspace_id,
        slug: row.slug,
        display_name: row.display_name,
        description: row.description,
        extraction_prompt: row.extraction_prompt,
        web_ingest_policy: serde_json::from_value(row.web_ingest_policy)
            .map_err(|_| ApiError::Internal)?,
        recognition_policy,
        lifecycle_state: parse_lifecycle_state(&row.lifecycle_state)
            .map_err(CatalogLifecycleError::into_persisted_error)?,
        include_document_hint_in_mcp_answers: row.include_document_hint_in_mcp_answers,
        chunking_template: ChunkingTemplate::from_db_str(&row.chunking_template),
        runtime_readiness,
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

async fn settle_catalog_delete_operation(
    state: &AppState,
    operation_id: Uuid,
    status: &str,
    failure_code: Option<String>,
) {
    if let Err(update_error) = state
        .canonical_services
        .ops
        .update_async_operation(
            state,
            UpdateAsyncOperationCommand {
                operation_id,
                status: status.to_string(),
                completed_at: Some(chrono::Utc::now()),
                failure_code,
            },
        )
        .await
    {
        error!(
            operation_id = %operation_id,
            error = %update_error,
            "failed to settle catalog deletion async operation"
        );
    }
}

const fn catalog_delete_operation_is_active(status: OpsAsyncOperationStatus) -> bool {
    matches!(status, OpsAsyncOperationStatus::Accepted | OpsAsyncOperationStatus::Processing)
}

async fn restore_stashed_directory(
    state: &AppState,
    stashed_directory: Option<&crate::services::content::storage::StashedContentDirectory>,
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
    stashed_directory: Option<&crate::services::content::storage::StashedContentDirectory>,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn present(purposes: &[AiBindingPurpose]) -> HashSet<String> {
        purposes.iter().map(|purpose| purpose.as_str().to_string()).collect()
    }

    #[test]
    fn runtime_readiness_preserves_distinct_missing_query_purposes() {
        let present = present(&[AiBindingPurpose::ExtractGraph, AiBindingPurpose::EmbedChunk]);
        let ingestion_readiness = CatalogLibraryIngestionReadiness {
            ready: true,
            missing_binding_purposes: missing_required_purposes(
                Some(&present),
                INGEST_REQUIRED_BINDINGS,
            ),
        };

        let readiness =
            runtime_readiness(Some(&present), &ingestion_readiness.missing_binding_purposes);

        assert_eq!(
            readiness.missing_binding_purposes,
            vec![
                AiBindingPurpose::QueryRetrieve,
                AiBindingPurpose::QueryCompile,
                AiBindingPurpose::QueryAnswer
            ]
        );
    }

    #[test]
    fn ingestion_readiness_does_not_block_on_query_purposes() {
        let present = present(&[AiBindingPurpose::ExtractGraph, AiBindingPurpose::EmbedChunk]);
        let missing = missing_required_purposes(Some(&present), INGEST_REQUIRED_BINDINGS);

        assert!(missing.is_empty());
    }

    #[test]
    fn runtime_readiness_preserves_vision_when_ingest_requires_it() {
        let ingestion_readiness = CatalogLibraryIngestionReadiness {
            ready: false,
            missing_binding_purposes: vec![AiBindingPurpose::Vision],
        };

        let readiness = runtime_readiness(None, &ingestion_readiness.missing_binding_purposes);

        assert_eq!(readiness.missing_binding_purposes[0], AiBindingPurpose::Vision);
        assert!(readiness.missing_binding_purposes.contains(&AiBindingPurpose::QueryRetrieve));
        assert!(readiness.missing_binding_purposes.contains(&AiBindingPurpose::QueryCompile));
        assert!(readiness.missing_binding_purposes.contains(&AiBindingPurpose::QueryAnswer));
    }
}
