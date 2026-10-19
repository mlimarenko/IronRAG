use super::*;

impl WebIngestService {
    pub(super) async fn fetch_http_response(
        &self,
        initial_url: &str,
    ) -> Result<reqwest::Response, PublicHttpUrlError> {
        let timeout = Duration::from_secs(self.runtime.request_timeout_seconds);
        let connect_timeout = Duration::from_secs(self.runtime.request_timeout_seconds.min(10));
        match self.fetch_policy {
            WebFetchPolicy::PublicOnly => {
                get_public_http_following_redirects(
                    initial_url,
                    true,
                    self.runtime.max_redirects,
                    timeout,
                    connect_timeout,
                    Some(&self.runtime.user_agent),
                )
                .await
            }
            #[cfg(feature = "test-support")]
            WebFetchPolicy::LoopbackTestOnly => {
                get_loopback_test_http_following_redirects(
                    initial_url,
                    self.runtime.max_redirects,
                    timeout,
                    connect_timeout,
                    Some(&self.runtime.user_agent),
                )
                .await
            }
        }
    }

    /// Re-fetches a web-captured document's source URL and persists the fresh
    /// bytes as a new revision snapshot. Used by the reprocess/retry path so
    /// that "retry" for a web document means "go back to the site and pull
    /// the current version" instead of "re-parse the captured HTML we already
    /// stored". Diff-aware ingest then still kicks in downstream: unchanged
    /// chunks reuse prior extractions, changed chunks get a fresh run.
    ///
    /// Returns the new storage descriptor for the revision row together with a
    /// [`PersistedRevisionSourceGuard`] that owns the per-library content
    /// storage lock and reclaims the fresh blob if the caller aborts before
    /// admission commits. Invalid or policy-rejected sources and permanent
    /// response errors are reported as `BadRequest`; transient transport and
    /// upstream failures are reported as typed service-unavailable errors so
    /// callers can retry safely.
    pub(crate) async fn refetch_document_source(
        &self,
        state: &AppState,
        workspace_id: Uuid,
        library_id: Uuid,
        source_uri: &str,
    ) -> Result<(RefetchedWebDocumentSource, PersistedRevisionSourceGuard), ApiError> {
        let trimmed = source_uri.trim();
        if trimmed.is_empty() {
            return Err(ApiError::BadRequest(
                "web document has no source_uri to re-fetch".to_string(),
            ));
        }
        let response = self.fetch_http_response(trimmed).await.map_err(|error| {
            if error.is_terminal_policy_rejection() {
                ApiError::BadRequest(format!("web source URL is not allowed: {error}"))
            } else {
                ApiError::service_unavailable(
                    "web source is temporarily unavailable",
                    "web_source_unavailable",
                )
            }
        })?;
        let http_status = response.status();
        if !http_status.is_success() {
            if http_status.is_server_error() {
                return Err(ApiError::service_unavailable(
                    "web source is temporarily unavailable",
                    "web_source_unavailable",
                ));
            }
            return Err(ApiError::BadRequest(format!(
                "web source returned status {http_status} on re-fetch"
            )));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let final_url = response.url().as_str().to_string();
        let payload_bytes = read_response_bytes_with_limit(response, MAX_WEB_FETCH_BODY_BYTES)
            .await
            .map_err(|error| match error {
                PublicHttpUrlError::ContentLengthExceeded { .. }
                | PublicHttpUrlError::BodySizeExceeded { .. } => {
                    ApiError::BadRequest(error.to_string())
                }
                _ => ApiError::service_unavailable(
                    "web source response could not be read",
                    "web_source_unavailable",
                ),
            })?;
        let byte_size = i64::try_from(payload_bytes.len()).unwrap_or(i64::MAX);
        let checksum = format!(
            "sha256:{}",
            hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&payload_bytes))
        );
        // Mirror the inline pipeline sites: take the per-library content
        // storage lock and arm the guard *before* the blob write so a
        // concurrent reclaim of the same content-addressed key serializes
        // behind the lock and an abort during or after the write reclaims it.
        let storage_lock = content_mutation_repository::acquire_content_library_storage_lock(
            &state.persistence.postgres,
            library_id,
        )
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
        let storage_key = ContentStorageService::build_web_snapshot_storage_key(
            workspace_id,
            library_id,
            &final_url,
            &checksum,
        );
        let guard = PersistedRevisionSourceGuard::arm(
            state,
            workspace_id,
            library_id,
            storage_key.clone(),
            storage_lock,
        );
        let persisted_storage_key = state
            .content_storage
            .persist_web_snapshot(workspace_id, library_id, &final_url, &checksum, &payload_bytes)
            .await
            .map_err(|error| {
                ApiError::internal_with_log(error, "persist refetched web snapshot")
            })?;
        debug_assert_eq!(
            persisted_storage_key, storage_key,
            "persisted key must match the key armed on the guard",
        );
        Ok((
            RefetchedWebDocumentSource {
                file_name: super::support::source_file_name_from_url(
                    &final_url,
                    content_type.as_deref(),
                ),
                storage_key,
                checksum,
                byte_size,
                mime_type: content_type,
                final_url,
            },
            guard,
        ))
    }
}
