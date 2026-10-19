use super::*;

impl CatalogService {
    pub(crate) async fn create_workspace_for_provisioning(
        &self,
        transaction: &mut sqlx::PgConnection,
        command: CreateWorkspaceCommand,
    ) -> Result<ProvisioningWorkspaceCreate, ApiError> {
        let display_name = normalize_display_name(&command.display_name, "displayName")?;
        let slug = normalize_optional_slug(command.slug.as_deref(), &display_name);
        let row = catalog_repository::create_workspace_with_transaction(
            transaction,
            &slug,
            &display_name,
            command.created_by_principal_id,
            command.created_by_principal_id.ok_or(ApiError::Internal)?,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        Ok(ProvisioningWorkspaceCreate { row, slug, display_name })
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

    /// Creates a workspace using normalized slug and lifecycle defaults.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, the repository write fails, or the resulting
    /// row cannot be mapped back into the domain type.
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
    /// Returns [`ApiError::Conflict`] while durable webhook work is not drained, or another
    /// [`ApiError`] when the workspace cannot be loaded, stashed, or deleted.
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

        let delete_outcome =
            match catalog_repository::delete_workspace(&state.persistence.postgres, workspace.id)
                .await
            {
                Ok(delete_outcome) => delete_outcome,
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

        match delete_outcome {
            catalog_repository::CatalogWorkspaceDeleteOutcome::Deleted => {}
            catalog_repository::CatalogWorkspaceDeleteOutcome::NotFound => {
                restore_stashed_directory(state, stashed_directory.as_ref()).await;
                return Err(ApiError::resource_not_found("workspace", workspace.id));
            }
            catalog_repository::CatalogWorkspaceDeleteOutcome::Blocked(blockers) => {
                restore_stashed_directory(state, stashed_directory.as_ref()).await;
                return Err(map_catalog_delete_blockers("workspace", blockers));
            }
        }

        purge_stashed_directory(state, stashed_directory.as_ref()).await;
        Ok(workspace)
    }

    /// Admits a workspace deletion as a shared async operation.
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
            .application_services
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
            .application_services
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
