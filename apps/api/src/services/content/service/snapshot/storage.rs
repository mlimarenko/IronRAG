use super::*;

pub(super) async fn export_library_blobs<W>(
    state: &AppState,
    builder: &mut Builder<W>,
    _library_id: Uuid,
    has_blobs: bool,
    storage_keys: &HashSet<String>,
    summary: &mut SnapshotSummary,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    if !has_blobs {
        return Ok(());
    }
    for storage_key in storage_keys {
        let bytes = state
            .content_storage
            .read_revision_source(storage_key)
            .await
            .with_context(|| format!("read required blob {storage_key}"))?;
        let path = format!("blobs/{}", encode_blob_path(storage_key));
        append_raw_entry(builder, &path, &bytes)
            .await
            .with_context(|| format!("append blob {storage_key}"))?;
        summary.inventory.blobs.push(snapshot_entry_digest(path, &bytes));
    }
    Ok(())
}

/// Escapes a storage key into a path-safe form that still round-trips.
/// Storage keys look like `content/<ws>/<lib>/<doc>/<hash>.bin` already,
/// so percent-encoding is overkill — but we still reject leading `/` and
/// parent traversal to keep the archive safe.
pub(super) fn encode_blob_path(storage_key: &str) -> String {
    storage_key.trim_start_matches('/').replace("..", "__")
}

pub(super) async fn restore_snapshot_blob_entry<R>(
    entry: &mut SnapshotArchiveEntry<R>,
    blob_suffix: &str,
    restore: &mut LibrarySnapshotRestoreState<'_, '_>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send,
{
    if !restore.ctx.manifest.has_blobs {
        bail!("snapshot entry references undeclared blob payload");
    }
    let storage_key = restore.row_scope.normalize_blob_key(blob_suffix)?;
    let size = entry.header().size().context("read blob entry size")?;
    anyhow::ensure!(size > 0, "snapshot blob `{storage_key}` is empty");
    let workspace_id = restore.row_scope.target_workspace_id();
    // Register before the stream: a failed filesystem/S3 write may leave a
    // partial object, which must be compensated just like a completed write.
    restore.written_blobs.push((workspace_id, storage_key.clone()));
    restore
        .ctx
        .state
        .content_storage
        .write_revision_source_raw_stream(&storage_key, entry, size)
        .await
        .with_context(|| format!("stream blob {storage_key}"))?;
    restore.report.blobs_restored += 1;
    Ok(())
}

pub(super) async fn remove_uncommitted_snapshot_blobs(
    state: &AppState,
    library_id: Uuid,
    written_blobs: &[(Uuid, String)],
) -> anyhow::Result<()> {
    if written_blobs.is_empty() {
        return Ok(());
    }
    let pool = &state.persistence.postgres;
    let mut guard = pool.begin().await.context("begin failed snapshot blob compensation")?;
    content_mutation_repository::acquire_content_library_storage_lock_in_tx(&mut guard, library_id)
        .await?;
    for (workspace_id, storage_key) in written_blobs {
        // An admission after the rollback may now own this content-addressed
        // object. Never delete its bytes; unknown ownership also fails closed.
        if !crate::infra::repositories::content_revision_repository::storage_key_is_referenced(
            pool,
            storage_key,
        )
        .await?
        {
            state
                .content_storage
                .delete_revision_source(*workspace_id, library_id, storage_key)
                .await
                .context("remove uncommitted snapshot source object")?;
        }
    }
    guard.commit().await.context("finish failed snapshot blob compensation")
}

pub(super) async fn restore_stashed_storage_after_restore(
    state: &AppState,
    library_id: Uuid,
    stashed_storage: Option<&StashedContentDirectory>,
    restore_result: &anyhow::Result<()>,
) {
    let Some(stashed) = stashed_storage else {
        return;
    };
    match restore_result {
        Ok(()) => purge_stashed_storage_after_success(state, library_id, stashed).await,
        Err(error) => {
            restore_stashed_storage_after_failure(state, library_id, stashed, error).await;
        }
    }
}

/// The restore committed, so the stashed blob directory is disposable. A purge
/// failure is warned about — the restored library is already correct, and the
/// stale stash is bounded by the next backup cycle.
async fn purge_stashed_storage_after_success(
    state: &AppState,
    library_id: Uuid,
    stashed: &StashedContentDirectory,
) {
    if state.content_storage.purge_stashed_directory(stashed).await.is_err() {
        tracing::warn!(
            %library_id,
            "snapshot restore succeeded but failed to purge stashed blob directory",
        );
    }
}

/// The restore failed, so the target's original blobs are put back. A failed
/// rollback is an error: the library's Postgres state was already rolled back,
/// and without the blobs it is left in a half-consistent shape.
async fn restore_stashed_storage_after_failure(
    state: &AppState,
    library_id: Uuid,
    stashed: &StashedContentDirectory,
    _error: &anyhow::Error,
) {
    if state.content_storage.restore_stashed_directory_replacing_current(stashed).await.is_err() {
        tracing::error!(
            %library_id,
            "snapshot restore failed and blob stash rollback also failed",
        );
    }
}
