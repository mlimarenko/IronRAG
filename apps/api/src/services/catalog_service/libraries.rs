use super::*;

impl CatalogService {
    pub(crate) async fn create_library_for_provisioning(
        &self,
        state: &AppState,
        transaction: &mut sqlx::PgConnection,
        command: CreateLibraryCommand,
    ) -> Result<ProvisioningLibraryCreate, ApiError> {
        let display_name = normalize_display_name(&command.display_name, "displayName")?;
        let slug = normalize_optional_slug(command.slug.as_deref(), &display_name);
        let description = normalize_optional_text(command.description.as_deref());
        let recognition_policy = state
            .settings
            .default_recognition_policy()
            .to_json()
            .map_err(|_| ApiError::Internal)?;
        let row = catalog_repository::create_library_with_recognition_policy_with_transaction(
            transaction,
            catalog_repository::NewCatalogLibrary {
                workspace_id: command.workspace_id,
                slug: &slug,
                display_name: &display_name,
                description: description.as_deref(),
                recognition_policy,
                created_by_principal_id: command.created_by_principal_id,
            },
            command.created_by_principal_id.ok_or(ApiError::Internal)?,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        Ok(ProvisioningLibraryCreate { row, slug, display_name, description })
    }

    /// Lists libraries for a workspace together with ingestion readiness.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when repository reads or readiness derivation fail.
    /// Searches one bounded page of libraries together with readiness data.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when repository reads or readiness derivation fail.
    pub async fn search_library_page(
        &self,
        state: &AppState,
        query: SearchCatalogLibraryPage<'_>,
    ) -> Result<CatalogLibraryPage, ApiError> {
        let page = catalog_repository::search_library_page(
            &state.persistence.postgres,
            catalog_repository::CatalogLibraryPageQuery {
                workspace_id: query.workspace_id,
                search: query.search,
                visible_library_ids: query.visible_library_ids,
                pinned_library_id: query.pinned_library_id,
                offset: query.offset,
                limit: query.limit,
            },
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let policies_by_library = parse_library_policies(&page.items)?;
        let readiness_by_library =
            self.list_library_binding_readiness(state, &policies_by_library).await?;
        let items = page
            .items
            .into_iter()
            .map(|row| {
                let library_id = row.id;
                let readiness = readiness_by_library
                    .get(&library_id)
                    .cloned()
                    .unwrap_or_else(default_binding_readiness);
                map_library_row(row, readiness.ingestion, readiness.runtime)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CatalogLibraryPage { items, total_count: page.total_count })
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
        let (row, _slug) = self.insert_library_row(state, &command).await?;
        let policy = parse_library_policy(&row)?;
        let readiness = self.get_library_binding_readiness(state, row.id, &policy).await?;
        map_library_row(row, readiness.ingestion, readiness.runtime)
    }

    /// Inserts one `catalog_library` row and returns it with its normalized
    /// coordinates. Shared by the public create (which then derives readiness)
    /// and the restore path (which must capture the id before any fallible step).
    async fn insert_library_row(
        &self,
        state: &AppState,
        command: &CreateLibraryCommand,
    ) -> Result<(catalog_repository::CatalogLibraryRow, String), ApiError> {
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
            catalog_repository::NewCatalogLibrary {
                workspace_id: command.workspace_id,
                slug: &slug,
                display_name: &display_name,
                description: description.as_deref(),
                recognition_policy,
                created_by_principal_id: command.created_by_principal_id,
            },
        )
        .await
        .map_err(|error| map_library_create_error(error, command.workspace_id, &slug))?;
        Ok((row, slug))
    }

    /// Creates a library for the workspace-restore path and returns its id and
    /// slug the moment the INSERT commits. Unlike [`Self::create_library`] it
    /// derives no readiness, so no fallible step can sit between the commit and
    /// the caller learning the id — the property the restore's compensation set
    /// depends on.
    pub(crate) async fn create_library_for_restore(
        &self,
        state: &AppState,
        command: &CreateLibraryCommand,
    ) -> Result<(Uuid, String), ApiError> {
        let (row, slug) = self.insert_library_row(state, command).await?;
        Ok((row.id, slug))
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
            catalog_repository::CatalogLibraryUpdate {
                library_id: command.library_id,
                slug: &slug,
                display_name: &display_name,
                description: description.as_deref(),
                extraction_prompt,
                lifecycle_state: lifecycle_state_as_str(&command.lifecycle_state)
                    .map_err(CatalogLifecycleError::into_request_error)?,
                include_document_hint_in_mcp_answers: command.include_document_hint_in_mcp_answers,
            },
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

    /// Updates the declarative retrieval configuration owned by one library.
    ///
    /// The lexical text-search config name is validated against the live
    /// `pg_ts_config` catalog so an operator cannot persist an analyzer the
    /// database does not know about (the lexical SQL would otherwise fail at
    /// query time). Unknown names are rejected with a `BadRequest` the HTTP
    /// layer surfaces as `422`.
    ///
    /// # Errors
    ///
    /// Returns an [`ApiError`] when validation fails, the text-search config is
    /// unknown, the repository update fails, readiness derivation fails, or the
    /// library does not exist.
    pub async fn update_library_retrieval_config(
        &self,
        state: &AppState,
        command: UpdateLibraryRetrievalConfigCommand,
    ) -> Result<CatalogLibrary, ApiError> {
        command.retrieval_config.validate().map_err(ApiError::BadRequest)?;
        self.ensure_text_search_config_exists(
            state,
            &command.retrieval_config.lexical.text_search_config,
        )
        .await?;
        let config_json = command.retrieval_config.to_json().map_err(|_| ApiError::Internal)?;
        let row = catalog_repository::update_library_retrieval_config(
            &state.persistence.postgres,
            command.library_id,
            config_json,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("library", command.library_id))?;
        let policy = parse_library_policy(&row)?;
        let readiness = self.get_library_binding_readiness(state, row.id, &policy).await?;
        map_library_row(row, readiness.ingestion, readiness.runtime)
    }

    /// Rejects a text-search config name that is not present in the Postgres
    /// `pg_ts_config` catalog. The name is bound as a parameter, never
    /// interpolated, so no operator input enters the SQL string.
    async fn ensure_text_search_config_exists(
        &self,
        state: &AppState,
        text_search_config: &str,
    ) -> Result<(), ApiError> {
        let exists = sqlx::query_scalar::<_, bool>(
            "select exists (select 1 from pg_ts_config where cfgname = $1)",
        )
        .bind(text_search_config)
        .fetch_one(&state.persistence.postgres)
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if exists {
            Ok(())
        } else {
            Err(ApiError::BadRequest(format!("unknown text search config: {text_search_config}")))
        }
    }

    /// Deletes a library and its stashed storage snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`ApiError::Conflict`] while durable webhook work is not drained, or another
    /// [`ApiError`] when the library cannot be loaded, stashed, or deleted.
    pub async fn delete_library(
        &self,
        state: &AppState,
        library_id: Uuid,
    ) -> Result<CatalogLibrary, ApiError> {
        let library = self.get_library(state, library_id).await?;
        self.delete_library_at(state, library.workspace_id, library.id, CatalogDeleteFence::Loaded)
            .await?;
        Ok(library)
    }

    /// Deletes a library minted by the workspace-restore compensation path.
    ///
    /// The caller supplies the coordinates the restore captured when it
    /// provisioned the library, so this loads nothing and derives no readiness.
    /// That is the point: compensation runs after the import already failed,
    /// and a readiness source that is unavailable must not be able to strand a
    /// library the import created. Removal is otherwise identical to
    /// [`Self::delete_library`] — stash the storage, delete the catalog row
    /// (the schema's `ON DELETE CASCADE` covers every dependent table), purge
    /// the stash.
    pub(crate) async fn delete_library_for_restore(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
    ) -> Result<(), ApiError> {
        self.delete_library_at(state, workspace_id, library_id, CatalogDeleteFence::Coordinates)
            .await
    }

    /// Stash → durable catalog delete → purge, shared by the operator delete
    /// and the restore compensation. Coordinates come from the caller so the
    /// compensated path never reads the library row (or its derived readiness).
    async fn delete_library_at(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        fence: CatalogDeleteFence,
    ) -> Result<(), ApiError> {
        let stashed_directory =
            state.content_storage.stash_library_storage(workspace_id, library_id).await.map_err(
                |storage_error| {
                    error!(
                        workspace_id = %workspace_id,
                        library_id = %library_id,
                        error = ?storage_error,
                        "failed to stash library storage before delete"
                    );
                    ApiError::Internal
                },
            )?;

        let repository_delete = match fence {
            CatalogDeleteFence::Loaded => {
                catalog_repository::delete_library(&state.persistence.postgres, library_id).await
            }
            CatalogDeleteFence::Coordinates => {
                catalog_repository::delete_library_for_restore(
                    &state.persistence.postgres,
                    library_id,
                    workspace_id,
                )
                .await
            }
        };
        let delete_outcome = match repository_delete {
            Ok(delete_outcome) => delete_outcome,
            Err(delete_error) => {
                restore_stashed_directory(state, stashed_directory.as_ref()).await;
                error!(
                    workspace_id = %workspace_id,
                    library_id = %library_id,
                    error = ?delete_error,
                    "failed to delete library"
                );
                return Err(ApiError::Internal);
            }
        };

        match delete_outcome {
            catalog_repository::CatalogLibraryDeleteOutcome::Deleted => {}
            catalog_repository::CatalogLibraryDeleteOutcome::NotFound => {
                restore_stashed_directory(state, stashed_directory.as_ref()).await;
                return Err(ApiError::resource_not_found("library", library_id));
            }
            catalog_repository::CatalogLibraryDeleteOutcome::Blocked(blockers) => {
                restore_stashed_directory(state, stashed_directory.as_ref()).await;
                return Err(map_catalog_delete_blockers("library", blockers));
            }
        }

        purge_stashed_directory(state, stashed_directory.as_ref()).await;
        Ok(())
    }

    /// Admits a library deletion as a shared async operation.
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
            .application_services
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
            .application_services
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

    /// Derives binding readiness for shell/library summaries from active AI bindings.
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
}

/// Which repository delete the shared removal runs.
///
/// The operator path loads the library first, so its `workspace_id` is the
/// row's own and the delete needs no further fence. The restore compensation
/// only has the coordinates captured at provisioning and must never load the
/// row, so its delete is fenced to that exact `(library_id, workspace_id)` pair.
#[derive(Clone, Copy)]
enum CatalogDeleteFence {
    Loaded,
    Coordinates,
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

pub(super) fn runtime_readiness(
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

pub(super) fn missing_required_purposes(
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
    let retrieval_config =
        RetrievalConfig::from_json(row.retrieval_config).map_err(|_| ApiError::Internal)?;
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
        retrieval_config,
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
