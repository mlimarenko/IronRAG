use super::*;

/// A reprocess source resolved for admission, together with the guard that
/// reclaims its freshly-persisted blob if admission never commits. `guard` is
/// `Some` only when this call wrote a new derived blob; the caller must disarm
/// it once a revision references the source.
pub(crate) struct ResolvedReprocessRevisionSource {
    pub(crate) source: ReprocessRevisionSource,
    pub(crate) guard: Option<PersistedRevisionSourceGuard>,
}

/// Identity of a source blob to persist: its storage scope and byte content.
/// Grouped so the persist helpers read as a named input rather than a
/// positional argument bag.
pub(crate) struct InlineFileSourceInput<'a> {
    pub(crate) workspace_id: Uuid,
    pub(crate) library_id: Uuid,
    pub(crate) file_name: &'a str,
    pub(crate) checksum: &'a str,
    pub(crate) file_bytes: &'a [u8],
}

fn rendered_revision_text_source_file_name(revision: &ContentRevision) -> String {
    let fallback = format!("revision-{}.txt", revision.id);
    let base = derive_content_source_file_name(
        revision.source_uri.as_deref(),
        revision.title.as_deref(),
        &fallback,
    );
    let stem = base.rsplit_once('.').map_or(base.as_str(), |(stem, _)| stem).trim();
    let normalized_stem = if stem.is_empty() { "revision" } else { stem };
    format!("{normalized_stem}.txt")
}

fn record_stream_reprocess_file_name(revision: &ContentRevision) -> String {
    let fallback = format!("revision-{}.jsonl", revision.id);
    let base = derive_content_source_file_name(
        revision.source_uri.as_deref(),
        revision.title.as_deref(),
        &fallback,
    );
    let trimmed = base.trim();
    let extension = trimmed.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase());
    if matches!(extension.as_deref(), Some("jsonl" | "ndjson")) {
        return trimmed.to_string();
    }
    let stem = trimmed.rsplit_once('.').map_or(trimmed, |(stem, _)| stem).trim();
    let normalized_stem = if stem.is_empty() { "revision" } else { stem };
    format!("{normalized_stem}.jsonl")
}

impl ContentService {
    pub async fn resolve_revision_storage_key(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Option<String>, ApiError> {
        let revision = content_revision_repository::get_revision_by_id(
            &state.persistence.postgres,
            revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("revision", revision_id))?;
        if let Some(storage_key) = revision
            .storage_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
        {
            return Ok(Some(storage_key));
        }

        let Some(file_name) = derive_storage_backed_content_file_name(
            &revision.content_source_kind,
            revision.source_uri.as_deref(),
            revision.title.as_deref(),
        ) else {
            return Ok(None);
        };

        let storage_key = ContentStorageService::build_revision_storage_key(
            revision.workspace_id,
            revision.library_id,
            &file_name,
            &revision.checksum,
        );
        let exists = state
            .content_storage
            .has_revision_source(&storage_key)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        if !exists {
            return Ok(None);
        }

        let storage_update = content_revision_repository::update_revision_storage_key(
            &state.persistence.postgres,
            revision_id,
            Some(&storage_key),
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        match storage_update {
            content_revision_repository::UpdateRevisionStorageKeyOutcome::Updated(_) => {}
            content_revision_repository::UpdateRevisionStorageKeyOutcome::RevisionNotFound => {
                return Err(ApiError::resource_not_found("revision", revision_id));
            }
            content_revision_repository::UpdateRevisionStorageKeyOutcome::KnowledgeProjectionNotFound => {
                return Err(ApiError::service_unavailable(
                    "revision projection is converging; retry the storage update shortly",
                    "document_projection_converging",
                ));
            }
        }
        Ok(Some(storage_key))
    }

    pub async fn render_revision_text_source(
        &self,
        state: &AppState,
        revision_id: Uuid,
    ) -> Result<Option<String>, ApiError> {
        let revision = state
            .document_store
            .get_revision(revision_id)
            .await
            .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        Ok(revision.and_then(|row| {
            row.normalized_text
                .map(|text| text.trim_end().to_string())
                .filter(|text| !text.is_empty())
        }))
    }

    pub(crate) async fn resolve_reprocess_revision_source(
        &self,
        state: &AppState,
        revision: &ContentRevision,
    ) -> Result<Option<ResolvedReprocessRevisionSource>, ApiError> {
        let storage_key = if let Some(storage_key) = revision
            .storage_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
        {
            Some(storage_key)
        } else {
            self.resolve_revision_storage_key(state, revision.id).await?
        };
        if let Some(storage_key) = storage_key {
            // The key predates this call (it is already recorded on the
            // revision), so there is no staged blob to reclaim.
            return Ok(Some(ResolvedReprocessRevisionSource {
                source: ReprocessRevisionSource {
                    file_name: crate::infra::repositories::content_revision_repository::get_revision_replacement_identity(
                        &state.persistence.postgres, revision.id
                    ).await.map_err(|error| ApiError::internal_with_log(error, "reprocess immutable source identity"))?
                        .ok_or_else(|| ApiError::BadRequest("revision has no unambiguous immutable replacement identity".to_owned()))?.file_name,
                    checksum: revision.checksum.clone(),
                    mime_type: revision.mime_type.clone(),
                    byte_size: revision.byte_size,
                    title: revision.title.clone(),
                    source_uri: revision.source_uri.clone(),
                    storage_key,
                },
                guard: None,
            }));
        }

        if let Some(resolved) = self.resolve_record_stream_reprocess_source(state, revision).await?
        {
            return Ok(Some(resolved));
        }

        let Some(text_source) = self.render_revision_text_source(state, revision.id).await? else {
            return Ok(None);
        };
        let checksum = format!("sha256:{}", sha256_hex_text(&text_source));
        let file_name = rendered_revision_text_source_file_name(revision);
        let (storage_key, guard) = self
            .persist_inline_file_source_with_storage_lock(
                state,
                InlineFileSourceInput {
                    workspace_id: revision.workspace_id,
                    library_id: revision.library_id,
                    file_name: &file_name,
                    checksum: &checksum,
                    file_bytes: text_source.as_bytes(),
                },
            )
            .await?;

        Ok(Some(ResolvedReprocessRevisionSource {
            source: ReprocessRevisionSource {
                file_name: file_name.clone(),
                checksum,
                mime_type: "text/plain".to_string(),
                byte_size: i64::try_from(text_source.len()).unwrap_or(i64::MAX),
                title: Some(file_name),
                source_uri: Some(format!("derived-text://{}", revision.id)),
                storage_key,
            },
            guard: Some(guard),
        }))
    }

    async fn resolve_record_stream_reprocess_source(
        &self,
        state: &AppState,
        revision: &ContentRevision,
    ) -> Result<Option<ResolvedReprocessRevisionSource>, ApiError> {
        let Some(structured_revision) = state
            .document_store
            .get_structured_revision(revision.id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        else {
            return Ok(None);
        };
        if structured_revision.source_format != RECORD_JSONL_SOURCE_FORMAT {
            return Ok(None);
        }

        let blocks = state
            .document_store
            .list_structured_blocks_by_revision(revision.id)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let mut lines = Vec::<String>::new();
        for block in blocks {
            if block.block_kind != StructuredBlockKind::SourceUnit.as_str() {
                continue;
            }
            let text = block.normalized_text.trim();
            if text.is_empty() {
                continue;
            }
            let payload = serde_json::json!({
                "id": block.block_id.to_string(),
                "kind": StructuredBlockKind::SourceUnit.as_str(),
                "ordinal": block.ordinal,
                "text": text,
            });
            let line = serde_json::to_string(&payload)
                .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
            lines.push(line);
        }
        if lines.is_empty() {
            return Ok(None);
        }

        let payload = format!("{}\n", lines.join("\n"));
        let bytes = payload.as_bytes();
        let checksum = format!("sha256:{}", sha256_hex_text(&payload));
        let file_name = record_stream_reprocess_file_name(revision);
        let (storage_key, guard) = self
            .persist_inline_file_source_with_storage_lock(
                state,
                InlineFileSourceInput {
                    workspace_id: revision.workspace_id,
                    library_id: revision.library_id,
                    file_name: &file_name,
                    checksum: &checksum,
                    file_bytes: bytes,
                },
            )
            .await?;

        Ok(Some(ResolvedReprocessRevisionSource {
            source: ReprocessRevisionSource {
                file_name: file_name.clone(),
                checksum,
                mime_type: "application/x-ndjson".to_string(),
                byte_size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
                title: Some(file_name),
                source_uri: Some(format!("derived-record-stream://{}", revision.id)),
                storage_key,
            },
            guard: Some(guard),
        }))
    }

    pub(crate) async fn persist_inline_file_source(
        &self,
        state: &AppState,
        input: InlineFileSourceInput<'_>,
    ) -> Result<String, ApiError> {
        let InlineFileSourceInput { workspace_id, library_id, file_name, checksum, file_bytes } =
            input;
        state
            .content_storage
            .persist_revision_source(workspace_id, library_id, file_name, checksum, file_bytes)
            .await
            .map_err(|e| ApiError::internal_with_log(e, "internal"))
    }

    pub(crate) async fn persist_inline_file_source_with_storage_lock(
        &self,
        state: &AppState,
        input: InlineFileSourceInput<'_>,
    ) -> Result<(String, PersistedRevisionSourceGuard), ApiError> {
        let InlineFileSourceInput { workspace_id, library_id, file_name, checksum, file_bytes } =
            input;
        let storage_lock = content_mutation_repository::acquire_content_library_storage_lock(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?;
        let storage_key = ContentStorageService::build_revision_storage_key(
            workspace_id,
            library_id,
            file_name,
            checksum,
        );
        // Arm before the write, transferring the library storage lock into the
        // guard: an abort during the write, or before admission commits, leaves
        // the blob unreferenced and the guard reclaims it under that same lock.
        // The caller disarms once admission commits; the lock is released only
        // then (or on armed drop).
        let guard = PersistedRevisionSourceGuard::arm(
            state,
            workspace_id,
            library_id,
            storage_key.clone(),
            storage_lock,
        );
        let persisted_storage_key = self
            .persist_inline_file_source(
                state,
                InlineFileSourceInput { workspace_id, library_id, file_name, checksum, file_bytes },
            )
            .await?;
        debug_assert_eq!(
            persisted_storage_key, storage_key,
            "persisted key must match the key armed on the guard",
        );
        Ok((storage_key, guard))
    }

    pub(in crate::services::content::service) async fn lease_inline_attempt(
        &self,
        state: &AppState,
        context: &InlineMutationContext,
    ) -> Result<crate::domains::ingest::IngestAttempt, ApiError> {
        state
            .application_services
            .ingest
            .lease_attempt(
                state,
                LeaseAttemptCommand {
                    job_id: context.job_id,
                    worker_principal_id: None,
                    lease_token: Some(format!("inline-{}", Uuid::now_v7())),
                    expected_queue_lease_token: None,
                    knowledge_generation_id: None,
                    current_stage: Some(INGEST_STAGE_EXTRACT_CONTENT.to_string()),
                },
            )
            .await
    }

    pub(in crate::services::content::service) fn inline_mutation_context_from_admission(
        &self,
        admission: &ContentMutationAdmission,
    ) -> Result<InlineMutationContext, ApiError> {
        let item = admission.items.first().ok_or(ApiError::Internal)?;
        Ok(InlineMutationContext {
            mutation_id: admission.mutation.id,
            job_id: admission.job_id.ok_or(ApiError::Internal)?,
            item_id: item.id,
            workspace_id: admission.mutation.workspace_id,
            library_id: admission.mutation.library_id,
            document_id: item.document_id.ok_or(ApiError::Internal)?,
            revision_id: item.result_revision_id.ok_or(ApiError::Internal)?,
        })
    }

    /// Loads raw source bytes for source-format append.
    pub(in crate::services::content::service) async fn load_appendable_document_source(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        document_id: Uuid,
        base_revision_id: Uuid,
    ) -> Result<super::super::AppendableDocumentSource, ApiError> {
        let document = crate::infra::repositories::content_document_repository::get_document_by_id(
            &state.persistence.postgres,
            document_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "load append document scope"))?
        .ok_or_else(|| ApiError::resource_not_found("document", document_id))?;
        if document.workspace_id != workspace_id || document.library_id != library_id {
            return Err(ApiError::Conflict(
                "append document does not belong to the requested scope".to_owned(),
            ));
        }
        let base_revision = content_revision_repository::get_revision_by_id(
            &state.persistence.postgres,
            base_revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "internal"))?
        .ok_or_else(|| ApiError::resource_not_found("revision", base_revision_id))?;
        if base_revision.document_id != document_id
            || base_revision.workspace_id != workspace_id
            || base_revision.library_id != library_id
        {
            return Err(ApiError::Conflict(
                "append base does not belong to the requested document scope".to_owned(),
            ));
        }
        let replacement = content_revision_repository::get_revision_replacement_identity(
            &state.persistence.postgres,
            base_revision_id,
        )
        .await
        .map_err(|e| ApiError::internal_with_log(e, "load immutable append base identity"))?
        .ok_or_else(|| {
            ApiError::Conflict(
                "append base has no unambiguous immutable replacement identity".to_owned(),
            )
        })?;
        let storage_key = base_revision.storage_key.as_deref().ok_or_else(|| {
            ApiError::BadRequest(
                "append is not supported for revisions without stored source bytes".to_string(),
            )
        })?;
        if !super::super::is_appendable_text_mime(&replacement.mime_type) {
            return Err(ApiError::BadRequest(format!(
                "append is not supported for mime type {} — only text-like sources can be appended",
                replacement.mime_type
            )));
        }
        let raw_bytes =
            if state.content_storage.has_revision_source(storage_key).await.map_err(|error| {
                ApiError::internal_with_log(error, "failed to probe revision source storage")
            })? {
                state.content_storage.read_revision_source(storage_key).await.map_err(|error| {
                    ApiError::internal_with_log(
                        error,
                        "failed to read revision source from storage",
                    )
                })?
            } else {
                return Err(ApiError::Conflict("append base source bytes are missing".to_owned()));
            };
        if replacement.checksum != format!("sha256:{}", super::super::sha256_hex_bytes(&raw_bytes))
            || usize::try_from(replacement.byte_size).ok() != Some(raw_bytes.len())
        {
            return Err(ApiError::Conflict(
                "append base source bytes disagree with immutable identity".to_owned(),
            ));
        }
        Ok(super::super::AppendableDocumentSource { raw_bytes, replacement })
    }
}
