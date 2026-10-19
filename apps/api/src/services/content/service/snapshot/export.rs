use super::*;

/// Streams a tar.zst archive into `writer`. The writer is typically the
/// write half of a `tokio::io::duplex` whose read half is attached to an
/// axum response body, so the whole pipeline is back-pressure driven.
pub async fn export_library_archive<W>(
    state: AppState,
    library_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> Result<(), ContentServiceError>
where
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    export_library_archive_inner(state, library_id, include, writer).await.map_err(|error| {
        // Source-owned backup errors may carry restricted filenames. Logs keep
        // scope and status only; the archive failure sentinel remains explicit.
        tracing::error!(
            %library_id,
            "snapshot export failed",
        );
        ContentServiceError::from(error)
    })
}

pub(super) async fn export_library_archive_inner<W>(
    state: AppState,
    library_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    IncludeKind::validate(&include)?;
    let include_set: HashSet<IncludeKind> = include.iter().copied().collect();

    // Run every fallible stage inside an inner async block whose Result
    // we capture, then ALWAYS finalize the tar Builder and the
    // ZstdEncoder before propagating the error. Dropping `Builder`
    // without `into_inner().await` panics inside `async_tar`
    // (`Builder dropped without finalizing`); that panic was reaching
    // the spawned writer task and was masked by an axum response that
    // had already been committed as HTTP 200. The result was a silent
    // truncated archive on the client. With this wrapping the archive
    // is always closed cleanly, and on the failure path we append a
    // sentinel `EXPORT_FAILED.json` entry so a client decompressing
    // the tar sees an explicit failure marker rather than a
    // partially-populated archive that looks complete.
    let zstd = ZstdEncoder::new(writer);
    let mut builder = Builder::new(zstd);
    builder.mode(async_tar::HeaderMode::Deterministic);

    let inner_result =
        export_library_archive_body(&state, library_id, &include, &include_set, &mut builder).await;
    finalize_archive_with_failure_sentinel(
        builder,
        ArchiveFinalizeScope { owner: ArchiveOwner::Library, id: library_id },
        inner_result,
    )
    .await
}

pub(super) async fn export_library_archive_body<W>(
    state: &AppState,
    library_id: Uuid,
    include: &[IncludeKind],
    include_set: &HashSet<IncludeKind>,
    builder: &mut Builder<W>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let pool = &state.persistence.postgres;
    // Freeze the owning lifecycle until every source-owned row and blob is
    // emitted. Acquire the storage lock before the library row, matching import
    // and admission; never gate backup bytes through the public-read view.
    let mut export_guard = pool.begin().await.context("begin snapshot export guard")?;
    content_mutation_repository::acquire_content_library_storage_lock_in_tx(
        &mut export_guard,
        library_id,
    )
    .await?;
    sqlx::query("SELECT id FROM catalog_library WHERE id = $1 FOR SHARE")
        .bind(library_id)
        .fetch_one(&mut *export_guard)
        .await
        .context("lock snapshot source library")?;
    let library_slug = load_library_slug(pool, library_id).await?;
    let export_plan = build_library_export_plan(pool, library_id, include_set).await?;

    // 1. manifest.json — first so readers can learn the shape immediately.
    let manifest = SnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        library_id,
        library_slug,
        exported_at: chrono::Utc::now(),
        source_version: env!("CARGO_PKG_VERSION").to_string(),
        include_kinds: include.to_vec(),
        postgres_tables: export_plan.manifest_postgres_tables.clone(),
        has_blobs: export_plan.has_blobs,
        vector_shards: export_plan.vector_shards,
    };
    append_json_entry(builder, "manifest.json", &manifest).await?;

    let mut summary = SnapshotSummary::default();
    let mut storage_keys = HashSet::new();
    let mut content_closure = SnapshotContentClosure::default();
    let mut stage = ExportStageContext {
        builder,
        pool,
        library_id,
        include_set,
        content_closure: &mut content_closure,
    };
    export_library_postgres_sections(
        &mut stage,
        &export_plan.library_postgres_tables,
        &mut summary,
        &mut storage_keys,
    )
    .await?;
    stage.content_closure.validate(&manifest)?;
    export_library_blobs(
        state,
        &mut *stage.builder,
        library_id,
        export_plan.has_blobs,
        &storage_keys,
        &mut summary,
    )
    .await?;

    // 4. summary.json — last, so it carries the real observed counts and exact
    // inventory. Validate this pure contract before emitting the terminal entry.
    validate_strict_snapshot_inventory(&manifest, &summary.inventory)?;
    validate_terminal_snapshot_summary(&summary.inventory, &summary.inventory, true)?;
    append_json_entry(builder, "summary.json", &summary).await?;
    export_guard.commit().await.context("release snapshot export guard")?;
    Ok(())
}

pub(super) struct LibraryExportPlan {
    manifest_postgres_tables: Vec<String>,
    library_postgres_tables: Vec<String>,
    vector_shards: Vec<VectorShardEntry>,
    has_blobs: bool,
}

pub(super) async fn load_library_slug(pool: &PgPool, library_id: Uuid) -> anyhow::Result<String> {
    let library_row = sqlx::query("SELECT slug FROM catalog_library WHERE id = $1")
        .bind(library_id)
        .fetch_optional(pool)
        .await
        .context("load catalog_library slug")?
        .ok_or_else(|| anyhow!("library {library_id} does not exist"))?;
    library_row.try_get("slug").context("decode catalog_library slug")
}

pub(super) async fn build_library_export_plan(
    pool: &PgPool,
    library_id: Uuid,
    include_set: &HashSet<IncludeKind>,
) -> anyhow::Result<LibraryExportPlan> {
    let include_library_data = include_set.contains(&IncludeKind::LibraryData);
    let mut manifest_postgres_tables = included_export_scope_tables(include_set);
    let mut library_postgres_tables = Vec::new();
    if include_library_data {
        manifest_postgres_tables
            .extend(POSTGRES_LIBRARY_ROOT_TABLES.iter().map(|table| (*table).to_string()));
        library_postgres_tables
            .extend(POSTGRES_CONTENT_TABLES.iter().map(|table| (*table).to_string()));
        library_postgres_tables
            .extend(POSTGRES_RUNTIME_GRAPH_TABLES.iter().map(|table| (*table).to_string()));
        library_postgres_tables
            .extend(POSTGRES_KNOWLEDGE_TABLES.iter().map(|table| (*table).to_string()));
        library_postgres_tables
            .extend(list_pg_vector_relations_for_library(pool, library_id).await?);
        manifest_postgres_tables.extend(library_postgres_tables.iter().cloned());
    }

    Ok(LibraryExportPlan {
        vector_shards: vector_shards_from_tables(&library_postgres_tables)?,
        manifest_postgres_tables,
        library_postgres_tables,
        has_blobs: include_set.contains(&IncludeKind::Blobs),
    })
}

pub(super) fn included_export_scope_tables(include_set: &HashSet<IncludeKind>) -> Vec<String> {
    let mut tables = Vec::new();
    if include_set.contains(&IncludeKind::Workspace) {
        tables.extend(POSTGRES_WORKSPACE_TABLES.iter().map(|table| (*table).to_string()));
    }
    if include_set.contains(&IncludeKind::AiConfig) {
        tables.extend(POSTGRES_AI_CONFIG_TABLES.iter().map(|table| (*table).to_string()));
    }
    tables
}

pub(super) fn vector_shards_from_tables(
    tables: &[String],
) -> anyhow::Result<Vec<VectorShardEntry>> {
    tables
        .iter()
        .filter(|table| is_runtime_vector_relation_name(table))
        .map(|shard| {
            let dim = parse_per_dim_vector_collection_dim(shard).ok_or_else(|| {
                anyhow!("malformed per-dim vector relation `{shard}` discovered during export")
            })?;
            Ok(VectorShardEntry { name: shard.clone(), dim })
        })
        .collect()
}

/// The archive sink and Postgres scope shared by every export stage: the tar
/// builder being appended to, the pool to read, the library being exported and
/// the include set that selects the sections.
pub(super) struct ExportStageContext<'a, W>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    pub(super) builder: &'a mut Builder<W>,
    pub(super) pool: &'a PgPool,
    pub(super) library_id: Uuid,
    pub(super) include_set: &'a HashSet<IncludeKind>,
    pub(super) content_closure: &'a mut SnapshotContentClosure,
}

pub(super) async fn export_library_postgres_sections<W>(
    ctx: &mut ExportStageContext<'_, W>,
    library_postgres_tables: &[String],
    summary: &mut SnapshotSummary,
    storage_keys: &mut HashSet<String>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    export_scope_sections(&mut *ctx, summary).await?;

    let pg_stage_started = std::time::Instant::now();
    for table in library_postgres_tables {
        export_library_postgres_table(ctx, table, storage_keys, summary).await?;
    }
    tracing::info!(
        library_id = %ctx.library_id,
        stage_elapsed_ms = pg_stage_started.elapsed().as_millis() as u64,
        "snapshot export stage postgres done",
    );
    Ok(())
}

/// Writes the workspace-scoped, AI-config-scoped and library-root Postgres
/// sections selected by the export's include set, folding each into `summary`.
async fn export_scope_sections<W>(
    ctx: &mut ExportStageContext<'_, W>,
    summary: &mut SnapshotSummary,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    if ctx.include_set.contains(&IncludeKind::Workspace) {
        let counts = export_pg_workspace_scope(
            ctx.builder,
            ctx.pool,
            ctx.library_id,
            &mut summary.inventory.postgres_parts,
        )
        .await?;
        merge_export_counts(summary, counts);
    }
    if ctx.include_set.contains(&IncludeKind::AiConfig) {
        let counts = export_pg_ai_config_scope(
            ctx.builder,
            ctx.pool,
            ctx.library_id,
            &mut summary.inventory.postgres_parts,
        )
        .await?;
        merge_export_counts(summary, counts);
    }
    if ctx.include_set.contains(&IncludeKind::LibraryData) {
        let count = export_pg_catalog_library(
            ctx.builder,
            ctx.pool,
            ctx.library_id,
            &mut summary.inventory.postgres_parts,
        )
        .await?;
        summary.inventory.postgres_row_counts.insert("catalog_library".to_string(), count);
    }
    Ok(())
}

pub(super) fn merge_export_counts(summary: &mut SnapshotSummary, counts: Vec<(String, u64)>) {
    for (table, count) in counts {
        summary.inventory.postgres_row_counts.insert(table, count);
    }
}

pub(super) async fn export_library_postgres_table<W>(
    ctx: &mut ExportStageContext<'_, W>,
    table: &str,
    storage_keys: &mut HashSet<String>,
    summary: &mut SnapshotSummary,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let table_started = std::time::Instant::now();
    let count = export_pg_table(
        ctx,
        table,
        (table == "content_revision").then_some(storage_keys),
        &mut summary.inventory.postgres_parts,
    )
    .await
    .with_context(|| format!("export postgres `{table}`"))?;
    summary.inventory.postgres_row_counts.insert(table.to_string(), count);
    tracing::info!(
        library_id = %ctx.library_id,
        %table,
        rows = count,
        elapsed_ms = table_started.elapsed().as_millis() as u64,
        "snapshot export stage postgres",
    );
    Ok(())
}
