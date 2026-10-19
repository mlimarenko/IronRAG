use super::*;

/// Maximum number of rows included in a single `PostgreSQL` INSERT statement
/// during restore. 1000 strikes a good balance: large
/// enough to amortize round-trip latency across a ten-thousand-row
/// table, small enough that a single statement's JSONB payload stays
/// under a few MiB and any parser bug only wastes a small slice.
pub(super) const IMPORT_BATCH_ROWS: usize = 1000;
/// Restores a library from a tar.zst archive body. `body` is any
/// `AsyncRead` — typically the request body stream. Rows are flushed
/// to storage in batches as the archive streams in, so memory footprint
/// stays roughly one `PostgreSQL` batch rather than scaling with total archive
/// size.
pub async fn restore_library_archive<R>(
    state: &AppState,
    library_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> Result<SnapshotImportReport, SnapshotImportError>
where
    R: AsyncRead + Unpin + Send,
{
    restore_library_archive_inner(state, library_id, body, overwrite, RestoreStatsMode::PerLibrary)
        .await
        .map_err(|error| {
            // Archive errors can contain restricted filenames or source bytes.
            // Logs carry classified status and scope only, never the raw chain.
            let failure = SnapshotImportError::new(error);
            tracing::error!(
                %library_id,
                failure_code = failure.failure_code(),
                "snapshot import refused",
            );
            failure
        })
}

pub(super) type SnapshotArchiveReader<R> = ZstdDecoder<BufReader<R>>;
pub(super) type SnapshotArchiveEntries<R> = async_tar::Entries<SnapshotArchiveReader<R>>;
pub(super) type SnapshotArchiveEntry<R> = async_tar::Entry<Archive<SnapshotArchiveReader<R>>>;

pub(super) async fn restore_library_archive_inner<R>(
    state: &AppState,
    library_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
    stats_mode: RestoreStatsMode,
) -> anyhow::Result<SnapshotImportReport>
where
    R: AsyncRead + Unpin + Send,
{
    let snapshot_file =
        spool_snapshot_archive(body).await.map_err(classify_snapshot_archive_error)?;
    let (prevalidated_manifest, blob_verification) =
        Box::pin(prevalidate_snapshot_archive(&snapshot_file))
            .await
            .map_err(classify_snapshot_archive_error)?;
    let file = tokio::fs::File::open(&snapshot_file.path)
        .await
        .context("reopen validated snapshot archive")?;
    let decoder = ZstdDecoder::new(BufReader::new(file));
    let archive = Archive::new(decoder);
    let mut entries = archive.entries().context("open tar archive")?;
    let (manifest, manifest_sections, _) = read_library_snapshot_manifest(&mut entries).await?;
    anyhow::ensure!(
        manifest.library_id == prevalidated_manifest.library_id,
        "snapshot changed after prevalidation"
    );
    let mut report = SnapshotImportReport {
        library_id,
        overwrite_mode: overwrite,
        include_kinds: manifest.include_kinds.clone(),
        blobs_verified: blob_verification.verified,
        unverifiable_blobs: blob_verification.unverifiable,
        ..Default::default()
    };
    let mut storage = SnapshotRestoreStorage::default();
    let ctx = LibraryRestoreContext {
        state,
        library_id,
        manifest: &manifest,
        sections: &manifest_sections,
    };
    let restore_result = restore_library_archive_transaction(
        ctx,
        &mut entries,
        overwrite,
        stats_mode,
        &mut report,
        &mut storage,
    )
    .await;
    if restore_result.is_err() && storage.stashed.is_none() {
        remove_uncommitted_snapshot_blobs(state, library_id, &storage.written_blobs).await?;
    }

    restore_stashed_storage_after_restore(
        state,
        library_id,
        storage.stashed.as_ref(),
        &restore_result,
    )
    .await;
    restore_result?;
    Ok(report)
}

pub(super) async fn read_library_snapshot_manifest<R>(
    entries: &mut SnapshotArchiveEntries<R>,
) -> anyhow::Result<(SnapshotManifest, SnapshotManifestSections, u64)>
where
    R: AsyncRead + Unpin,
{
    let Some(entry) = entries.next().await else {
        bail!("snapshot archive missing manifest.json");
    };
    let mut entry = entry.context("read tar entry")?;
    let path = entry.path().context("read tar entry path")?.to_string_lossy().into_owned();
    validate_archive_path(&path)?;
    if path != "manifest.json" {
        bail!("tar entry `{path}` arrived before manifest.json");
    }

    let declared_size = entry.header().size().context("read manifest.json size")?;
    let manifest: SnapshotManifest =
        read_bounded_json_entry(&mut entry, "manifest.json", declared_size).await?;
    validate_snapshot_schema_version(manifest.schema_version, "snapshot")?;
    let manifest_sections = SnapshotManifestSections::from_manifest(&manifest)?;
    Ok((manifest, manifest_sections, declared_size))
}

pub(super) fn validate_snapshot_schema_version(
    schema_version: u32,
    archive_kind: &str,
) -> anyhow::Result<()> {
    if schema_version != SNAPSHOT_SCHEMA_VERSION {
        return Err(SnapshotImportRejection::new(
            SnapshotImportFailureKind::UnsupportedSchemaVersion,
            format!(
                "{archive_kind} schema_version {schema_version} is not supported by this build (supported {SNAPSHOT_SCHEMA_VERSION}). Older archives lack exact visibility-intent closure and cannot be converted. Re-export the library from a deployment running this version, or re-ingest its sources."
            ),
        )
        .into_error());
    }
    Ok(())
}

/// Immutable restore inputs shared by the transaction driver and every per-entry
/// stage: app state, the target library id, the validated manifest and the
/// sections derived from it. Cheap to copy — every field is a reference or a
/// scalar — so each stage takes it by value.
#[derive(Clone, Copy)]
pub(super) struct LibraryRestoreContext<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) manifest: &'a SnapshotManifest,
    pub(super) sections: &'a SnapshotManifestSections,
}

pub(super) async fn restore_library_archive_transaction<R>(
    ctx: LibraryRestoreContext<'_>,
    entries: &mut SnapshotArchiveEntries<R>,
    overwrite: OverwriteMode,
    stats_mode: RestoreStatsMode,
    report: &mut SnapshotImportReport,
    storage: &mut SnapshotRestoreStorage,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send,
{
    let pool = &ctx.state.persistence.postgres;
    let mut tx = pool.begin().await.context("begin snapshot tx")?;
    content_mutation_repository::acquire_content_library_storage_lock_in_tx(
        &mut tx,
        ctx.library_id,
    )
    .await
    .context("acquire library storage lock before snapshot restore")?;
    let target = prepare_library_restore_target(
        ctx.state,
        &mut tx,
        ctx.library_id,
        ctx.manifest,
        overwrite,
        &mut storage.stashed,
    )
    .await?;
    let row_counts = stream_library_snapshot_entries(
        ctx,
        entries,
        &mut tx,
        &target,
        report,
        &mut storage.written_blobs,
    )
    .await?;
    finalize_library_restore_transaction(
        pool,
        tx,
        ctx.library_id,
        target.source_truth_version,
        stats_mode,
        &row_counts,
    )
    .await?;
    report.postgres_rows_by_table = row_counts.into_iter().collect();
    Ok(())
}

#[derive(Default)]
pub(super) struct SnapshotRestoreStorage {
    stashed: Option<StashedContentDirectory>,
    written_blobs: Vec<(Uuid, String)>,
}

pub(super) struct LibrarySnapshotRestoreState<'a, 'tx> {
    pub(super) ctx: LibraryRestoreContext<'a>,
    pub(super) tx: &'a mut sqlx::Transaction<'tx, sqlx::Postgres>,
    pub(super) pg_batcher: &'a mut PgBatcher,
    pub(super) row_scope: &'a mut SnapshotRowScope,
    pub(super) report: &'a mut SnapshotImportReport,
    pub(super) written_blobs: &'a mut Vec<(Uuid, String)>,
}

pub(super) async fn stream_library_snapshot_entries<R>(
    ctx: LibraryRestoreContext<'_>,
    entries: &mut SnapshotArchiveEntries<R>,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    target: &LibraryRestoreTarget,
    report: &mut SnapshotImportReport,
    written_blobs: &mut Vec<(Uuid, String)>,
) -> anyhow::Result<BTreeMap<String, u64>>
where
    R: AsyncRead + Unpin + Send,
{
    run_restore_preflight_before_side_effects(ctx.library_id, target.action, || async {
        ensure_manifest_vector_shards(ctx.state, ctx.sections)
            .await
            .context("ensure per-dim vector shards declared by snapshot manifest")
    })
    .await?;
    sqlx::query("SET LOCAL session_replication_role = 'replica'")
        .execute(&mut **tx)
        .await
        .context("disable FK checks for snapshot import")?;
    sqlx::query("SET CONSTRAINTS ALL DEFERRED")
        .execute(&mut **tx)
        .await
        .context("defer content archive forward references")?;
    reject_or_clear_target_restore_footprint(tx, ctx.library_id, target.action).await?;

    let mut counts_pg = BTreeMap::new();
    let mut pg_batcher = PgBatcher::new();
    let mut row_scope = SnapshotRowScope::new(
        ctx.manifest.library_id,
        ctx.library_id,
        target.workspace_id,
        Some(target.library_slug.clone()),
    );
    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read tar entry")?;
        let mut restore_state = LibrarySnapshotRestoreState {
            ctx,
            tx,
            pg_batcher: &mut pg_batcher,
            row_scope: &mut row_scope,
            report,
            written_blobs,
        };
        restore_library_snapshot_entry(&mut entry, &mut restore_state).await?;
    }
    finalize_library_snapshot_entries(&mut pg_batcher, &mut counts_pg, tx).await?;
    sqlx::query("SET LOCAL session_replication_role = 'origin'")
        .execute(&mut **tx)
        .await
        .context("enable deferred content closure checks")?;
    sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
        .execute(&mut **tx)
        .await
        .context("prove restored revision admission constraints")?;
    let invalid_representation: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM content_mutation_reference r
         LEFT JOIN content_mutation m ON m.id=r.id
         LEFT JOIN content_mutation_predecessor_audit a ON a.id=r.id
         WHERE r.library_id=$1 AND (
             (m.id IS NULL) = (a.id IS NULL)
             OR (m.id IS NOT NULL AND (m.workspace_id,m.library_id) IS DISTINCT FROM (r.workspace_id,r.library_id))
             OR (a.id IS NOT NULL AND (a.workspace_id,a.library_id) IS DISTINCT FROM (r.workspace_id,r.library_id))))")
        .bind(ctx.library_id).fetch_one(&mut **tx).await.context("attest restored exact registry representations")?;
    anyhow::ensure!(
        !invalid_representation,
        "restored registry representation closure differs from archive"
    );
    let invalid_storage: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM knowledge_revision k
         LEFT JOIN content_revision r ON r.id=k.revision_id AND r.document_id=k.document_id
             AND r.workspace_id=k.workspace_id AND r.library_id=k.library_id
         WHERE k.library_id=$1 AND (r.id IS NULL OR k.storage_ref IS DISTINCT FROM r.storage_key))",
    )
    .bind(ctx.library_id)
    .fetch_one(&mut **tx)
    .await
    .context("attest restored exact knowledge source ownership")?;
    anyhow::ensure!(
        !invalid_storage,
        "restored knowledge source is not its remapped content owner"
    );
    let invalid_published: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM content_document_head h
         JOIN content_document d ON d.id=h.document_id
         WHERE d.library_id=$1 AND d.document_state='active' AND d.deleted_at IS NULL
           AND h.readable_revision_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM content_current_revision current
               WHERE current.document_id=d.id AND current.revision_id=h.readable_revision_id))",
    )
    .bind(ctx.library_id)
    .fetch_one(&mut **tx)
    .await
    .context("attest restored canonical published membership")?;
    anyhow::ensure!(
        !invalid_published,
        "restored snapshot revision admission is not canonical current content"
    );
    // The batcher's rows — the vectors among them — only reach the database
    // when it flushes, so the invariant is attested after that flush and
    // inside the same transaction: a violation rolls the whole restore back.
    attest_restored_chunk_vector_lifecycle(tx, ctx.library_id).await?;
    Ok(counts_pg)
}

/// Proves the current-only vector invariant holds for the rows that actually
/// landed, and makes the manifest tell the truth about them.
///
/// A snapshot exported before the invariant existed can carry vectors the
/// superseded sweep already removed; accepting them would cancel the
/// lifecycle on the target (IRG-67), so any such row aborts the restore
/// while it is still one uncommitted transaction — no success stamp, no
/// half-restored library. The manifest's `row_count` is then recomputed from
/// the restored rows rather than trusted from the archive, because the
/// export filters vectors and a stale count would lie about the shard.
pub(super) async fn attest_restored_chunk_vector_lifecycle(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<()> {
    let relations =
        list_physical_vector_relations_in_transaction(tx, CHUNK_VECTOR_RELATION_PREFIX).await?;
    let predicate = selectable_chunk_vector_rows_sql();
    for relation_name in relations {
        validate_snapshot_pg_table_name(&relation_name)?;
        let relation = quote_pg_identifier(&relation_name)?;
        let joins = "LEFT JOIN knowledge_chunk chunk
                       ON chunk.library_id = page.library_id
                      AND chunk.chunk_id = page.chunk_id
                      AND chunk.revision_id = page.revision_id
                     LEFT JOIN knowledge_document document
                       ON document.library_id = page.library_id
                      AND document.document_id = chunk.document_id";
        let superseded: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
            "SELECT count(*)
             FROM {relation} page
             {joins}
             WHERE page.library_id = $1
               AND NOT {predicate}"
        )))
        .bind(library_id)
        .bind(false)
        .fetch_one(&mut **tx)
        .await
        .with_context(|| format!("count superseded vectors in restored `{relation_name}`"))?;
        anyhow::ensure!(
            superseded == 0,
            format!(
                "restored snapshot carries {superseded} superseded chunk vector row(s) in `{relation_name}`; \
it was exported before the current-only lifecycle existed and is refused (IRG-67). \
Re-export the library from a current deployment, or re-ingest its sources."
            )
        );
        let lanes: Vec<(String, String, i64)> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
            "SELECT page.vector_kind, page.embedding_model_key, count(*)
             FROM {relation} page
             {joins}
             WHERE page.library_id = $1
               AND {predicate}
             GROUP BY page.vector_kind, page.embedding_model_key"
        )))
        .bind(library_id)
        .bind(false)
        .fetch_all(&mut **tx)
        .await
        .with_context(|| format!("count restored vectors per lane in `{relation_name}`"))?;
        sqlx::query(
            "UPDATE knowledge_vector_relation_manifest
             SET row_count = 0
             WHERE library_id = $1 AND relation_name = $2",
        )
        .bind(library_id)
        .bind(&relation_name)
        .execute(&mut **tx)
        .await
        .context("reset restored vector manifest counts")?;
        for (vector_kind, embedding_model_key, row_count) in lanes {
            sqlx::query(
                "UPDATE knowledge_vector_relation_manifest
                 SET row_count = $3
                 WHERE library_id = $1
                   AND relation_name = $2
                   AND vector_kind = $4
                   AND embedding_model_key = $5",
            )
            .bind(library_id)
            .bind(&relation_name)
            .bind(row_count)
            .bind(&vector_kind)
            .bind(&embedding_model_key)
            .execute(&mut **tx)
            .await
            .context("store restored vector manifest count")?;
        }
    }
    Ok(())
}

pub(super) async fn restore_library_snapshot_entry<R>(
    entry: &mut SnapshotArchiveEntry<R>,
    restore: &mut LibrarySnapshotRestoreState<'_, '_>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send,
{
    let path = entry.path().context("read tar entry path")?.to_string_lossy().into_owned();
    validate_archive_path(&path)?;
    match path.as_str() {
        "summary.json" => log_snapshot_summary(entry, restore.ctx.library_id).await,
        "EXPORT_FAILED.json" => bail!(
            "snapshot archive is marked as failed export: {}",
            read_snapshot_export_failure(entry).await?
        ),
        "manifest.json" => bail!("tar archive contains a second manifest.json"),
        _ => {
            if let Some(rest) = path.strip_prefix("postgres/") {
                restore_snapshot_postgres_entry(entry, rest, &path, restore).await
            } else if let Some(blob_suffix) = path.strip_prefix("blobs/") {
                restore_snapshot_blob_entry(entry, blob_suffix, restore).await
            } else {
                bail!("unknown tar entry `{path}`")
            }
        }
    }
}

pub(super) async fn log_snapshot_summary<R>(
    entry: &mut SnapshotArchiveEntry<R>,
    library_id: Uuid,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
{
    let declared_size = entry.header().size().context("read summary.json size")?;
    if let Ok(parsed) =
        read_bounded_json_entry::<SnapshotSummary, _>(entry, "summary.json", declared_size).await
    {
        tracing::info!(
            %library_id,
            declared_blob_count = parsed.inventory.blobs.len(),
            declared_postgres_parts = parsed.inventory.postgres_parts.len(),
            "snapshot summary read",
        );
    }
    Ok(())
}

pub(super) async fn read_snapshot_export_failure<R>(
    entry: &mut SnapshotArchiveEntry<R>,
) -> anyhow::Result<String>
where
    R: AsyncRead + Unpin,
{
    let declared_size = entry.header().size().context("read EXPORT_FAILED.json size")?;
    Ok(read_bounded_json_entry::<serde_json::Value, _>(entry, "EXPORT_FAILED.json", declared_size)
        .await
        .ok()
        .and_then(|value| {
            value.get("error").and_then(serde_json::Value::as_str).map(str::to_string)
        })
        .unwrap_or_else(|| "snapshot archive contains EXPORT_FAILED.json".to_string()))
}

pub(super) async fn restore_snapshot_postgres_entry<R>(
    entry: &mut SnapshotArchiveEntry<R>,
    rest: &str,
    path: &str,
    restore: &mut LibrarySnapshotRestoreState<'_, '_>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
{
    let (table_ref, _file) =
        split_section_path(rest).with_context(|| format!("malformed postgres path `{path}`"))?;
    let table = restore.ctx.sections.require_postgres_table(table_ref)?;
    restore.pg_batcher.on_new_section(table, restore.tx).await?;
    read_ndjson_entry_and(entry, &mut |mut row| {
        restore.row_scope.normalize_postgres_row(table, &mut row)?;
        restore.pg_batcher.push(table, row);
        Ok(())
    })
    .await
    .with_context(|| format!("parse ndjson `{path}`"))?;
    restore.pg_batcher.maybe_flush(restore.tx).await
}

pub(super) async fn finalize_library_snapshot_entries(
    pg_batcher: &mut PgBatcher,
    counts_pg: &mut BTreeMap<String, u64>,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) -> anyhow::Result<()> {
    pg_batcher.flush(tx).await?;
    // The report counts what Postgres wrote, not what the archive offered, so a
    // row a conflict clause dropped can never be reported as restored.
    *counts_pg = pg_batcher.take_written_counts();
    Ok(())
}

pub(super) async fn finalize_library_restore_transaction(
    pool: &PgPool,
    mut tx: sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
    locked_source_truth_version: i64,
    stats_mode: RestoreStatsMode,
    counts_pg: &BTreeMap<String, u64>,
) -> anyhow::Result<()> {
    advance_restored_library_source_truth_version(&mut tx, library_id, locked_source_truth_version)
        .await?;
    tx.commit().await.context("commit snapshot tx")?;
    if stats_mode == RestoreStatsMode::PerLibrary
        && let Err(error) = analyze_imported_postgres_tables(pool, counts_pg).await
    {
        tracing::warn!(
            %library_id,
            error = %error,
            "snapshot import postgres stats refresh failed",
        );
    }
    Ok(())
}

pub(super) async fn analyze_imported_postgres_tables(
    pool: &PgPool,
    row_counts: &BTreeMap<String, u64>,
) -> anyhow::Result<()> {
    for (table, row_count) in row_counts {
        if *row_count == 0 {
            continue;
        }
        validate_snapshot_pg_table_name(table)?;
        let table = quote_pg_identifier(table)?;
        let statement = format!("ANALYZE {table}");
        sqlx::query(sqlx::AssertSqlSafe(&*statement))
            .execute(pool)
            .await
            .with_context(|| format!("analyze imported snapshot table `{table}`"))?;
    }
    Ok(())
}
