use super::*;

// ===========================================================================
// Workspace snapshot — bundles every library archive into one plain tar
// ===========================================================================

/// Env var naming a scratch directory for the per-library temp files the
/// workspace exporter materializes (one library at a time). Falls back to
/// `std::env::temp_dir()` when unset.
pub(super) const SNAPSHOT_SCRATCH_DIR_ENV: &str = "IRONRAG_SNAPSHOT_SCRATCH_DIR";

/// Manifest entry describing one library inside a workspace archive.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkspaceManifestLibrary {
    pub id: Uuid,
    pub slug: String,
    pub display_name: String,
}

/// First entry of a workspace archive (`workspace-manifest.json`). Declares
/// the workspace identity, the include kinds requested for every embedded
/// library archive, and the ordered library list. Reuses the per-library
/// [`SNAPSHOT_SCHEMA_VERSION`] so a single version gate covers both archive
/// shapes.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkspaceSnapshotManifest {
    pub schema_version: u32,
    pub workspace_id: Uuid,
    pub workspace_slug: String,
    pub exported_at: chrono::DateTime<chrono::Utc>,
    pub source_version: String,
    pub include_kinds: Vec<IncludeKind>,
    pub libraries: Vec<WorkspaceManifestLibrary>,
}

/// Terminal inventory for a workspace archive. It commits the exact set of
/// embedded library archives before any restore side effects may begin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkspaceSnapshotInventory {
    pub libraries: Vec<SnapshotEntryDigest>,
}

#[derive(Debug, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct WorkspaceSnapshotSummary {
    #[serde(flatten)]
    pub inventory: WorkspaceSnapshotInventory,
}

/// Per-library entry in a [`WorkspaceSnapshotImportReport`].
#[derive(Debug, Default)]
pub struct WorkspaceLibraryImportReport {
    /// Library id minted on the target stack (NOT the source id).
    pub library_id: Uuid,
    /// Slug actually assigned on the target (may be suffixed `-2`, `-3`… if
    /// the source slug collided with a sibling library).
    pub slug: String,
    pub postgres_rows_by_table: Vec<(String, u64)>,
    pub blobs_restored: u64,
    /// Blobs this library's import proved against `content_revision.checksum`
    /// (IRG-32).
    pub blobs_verified: u64,
    /// Blobs this library's import accepted without proof (IRG-32).
    pub unverifiable_blobs: u64,
}

pub(super) fn validate_workspace_snapshot_inventory(
    manifest: &WorkspaceSnapshotManifest,
    expected: &WorkspaceSnapshotInventory,
    observed: &WorkspaceSnapshotInventory,
) -> anyhow::Result<()> {
    let mut declared = HashSet::new();
    for library in &manifest.libraries {
        anyhow::ensure!(
            declared.insert(library.id),
            "workspace manifest declares a library more than once"
        );
    }
    let mut inventory_paths = HashSet::new();
    for digest in &expected.libraries {
        anyhow::ensure!(
            digest.size > 0
                && digest.sha256.len() == 64
                && digest.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "workspace inventory has an invalid digest for `{}`",
            digest.path
        );
        let id = parse_workspace_library_entry_path(&digest.path)?.ok_or_else(|| {
            anyhow!("workspace inventory has invalid archive path `{}`", digest.path)
        })?;
        anyhow::ensure!(
            declared.contains(&id),
            "workspace inventory contains undeclared library `{id}`"
        );
        anyhow::ensure!(
            inventory_paths.insert(digest.path.as_str()),
            "workspace inventory duplicates `{}`",
            digest.path
        );
    }
    anyhow::ensure!(
        inventory_paths.len() == declared.len(),
        "workspace inventory does not contain every declared library archive"
    );
    anyhow::ensure!(
        expected == observed,
        "workspace summary does not match observed archive inventory"
    );
    Ok(())
}

/// Streams one outer archive entry to a temporary file while producing its
/// digest. This avoids trusting tar header sizes and keeps archive bytes out of
/// memory during the validation pass.
pub(super) async fn spool_workspace_library_entry<R>(
    entry: &mut async_tar::Entry<Archive<BufReader<R>>>,
    path: &str,
) -> anyhow::Result<(SpoolSnapshotArchive, SnapshotEntryDigest)>
where
    R: AsyncRead + Unpin,
{
    let temp = tempfile::NamedTempFile::new().context("create embedded snapshot spool file")?;
    let temp_path = temp.into_temp_path();
    let mut output =
        tokio::fs::File::create(&temp_path).await.context("open embedded snapshot spool file")?;
    let mut hasher = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = entry.read(&mut buffer).await.context("read embedded snapshot archive")?;
        if read == 0 {
            break;
        }
        size = size
            .checked_add(read as u64)
            .ok_or_else(|| anyhow!("embedded snapshot is too large"))?;
        anyhow::ensure!(
            size <= MAX_SNAPSHOT_ARCHIVE_BYTES,
            "embedded snapshot archive exceeds maximum size"
        );
        hasher.update(&buffer[..read]);
        tokio::io::AsyncWriteExt::write_all(&mut output, &buffer[..read])
            .await
            .context("spool embedded snapshot archive")?;
    }
    tokio::io::AsyncWriteExt::shutdown(&mut output)
        .await
        .context("finalize embedded snapshot spool file")?;
    Ok((
        SpoolSnapshotArchive { path: temp_path },
        SnapshotEntryDigest {
            path: path.to_string(),
            size,
            sha256: hex::encode(hasher.finalize()),
        },
    ))
}

pub(super) async fn prevalidate_workspace_archive<R>(
    body: R,
) -> anyhow::Result<WorkspaceSnapshotManifest>
where
    R: AsyncRead + Unpin + Send,
{
    let archive = Archive::new(BufReader::new(body));
    let mut entries = archive.entries().context("open workspace archive for prevalidation")?;
    let manifest = read_workspace_snapshot_manifest(&mut entries).await?;
    let mut observed = WorkspaceSnapshotInventory::default();
    let mut summary: Option<WorkspaceSnapshotSummary> = None;
    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read workspace prevalidation entry")?;
        let path = entry
            .path()
            .context("read workspace prevalidation path")?
            .to_string_lossy()
            .into_owned();
        validate_archive_path(&path)?;
        if path == "workspace-summary.json" {
            anyhow::ensure!(
                summary.is_none(),
                "workspace snapshot has duplicate workspace-summary.json"
            );
            let declared_size =
                entry.header().size().context("read workspace-summary.json size")?;
            summary = Some(
                read_bounded_json_entry(&mut entry, "workspace-summary.json", declared_size)
                    .await?,
            );
            continue;
        }
        anyhow::ensure!(
            summary.is_none(),
            "workspace-summary.json must be the terminal archive entry"
        );
        let Some(_) = parse_workspace_library_entry_path(&path)? else {
            bail!("unknown workspace snapshot entry `{path}`");
        };
        let (spool, digest) = Box::pin(spool_workspace_library_entry(&mut entry, &path)).await?;
        // Classify the embedded archive's failure here, at the point it is
        // raised: an embedded blob that contradicts its checksum must carry
        // `ArchiveInvalid` up the chain, exactly like the library door's own
        // prevalidation, rather than collapse into `snapshot_import_failed`.
        let (embedded_manifest, _) = Box::pin(prevalidate_snapshot_archive(&spool))
            .await
            .map_err(classify_snapshot_archive_error)
            .with_context(|| format!("prevalidate embedded library archive `{path}`"))?;
        let expected_id = parse_workspace_library_entry_path(&path)?
            .ok_or_else(|| anyhow!("workspace inventory has invalid archive path `{path}`"))?;
        anyhow::ensure!(
            embedded_manifest.library_id == expected_id,
            "embedded library archive `{path}` declares a different library id"
        );
        observed.libraries.push(digest);
    }
    let summary = summary
        .ok_or_else(|| anyhow!("workspace snapshot missing terminal workspace-summary.json"))?;
    validate_workspace_snapshot_inventory(&manifest, &summary.inventory, &observed)?;
    Ok(manifest)
}

#[derive(Debug, Default)]
pub struct WorkspaceSnapshotImportReport {
    pub workspace_id: Uuid,
    pub libraries_restored: u64,
    pub overwrite_mode: OverwriteMode,
    pub libraries: Vec<WorkspaceLibraryImportReport>,
    /// Sum of every embedded library's proven blob count (IRG-32).
    pub blobs_verified: u64,
    /// Sum of every embedded library's unverifiable blob count (IRG-32).
    pub unverifiable_blobs: u64,
}

/// Streams a plain (uncompressed) tar bundling every library in `workspace_id`
/// into `writer`. Each embedded `libraries/{library_id}.tar.zst` entry is the
/// EXACT byte stream [`export_library_archive`] produces for that library with
/// the same `include` kinds — already zstd-compressed, hence the OUTER tar is
/// not compressed again.
///
/// To emit a tar entry the size must be known up front, so each library is
/// exported to a temp file first, stat-ed, appended, then deleted before the
/// next library — peak scratch usage is one library archive.
pub async fn export_workspace_archive<W>(
    state: AppState,
    workspace_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> Result<(), ContentServiceError>
where
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    export_workspace_archive_inner(state, workspace_id, include, writer).await.map_err(|error| {
        tracing::error!(
            %workspace_id,
            "workspace snapshot export failed",
        );
        ContentServiceError::from(error)
    })
}

pub(super) async fn export_workspace_archive_inner<W>(
    state: AppState,
    workspace_id: Uuid,
    include: Vec<IncludeKind>,
    writer: W,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync + 'static,
{
    IncludeKind::validate(&include)?;
    let pool = &state.persistence.postgres;

    let workspace_slug: String =
        sqlx::query_scalar("SELECT slug FROM catalog_workspace WHERE id = $1")
            .bind(workspace_id)
            .fetch_optional(pool)
            .await
            .context("load catalog_workspace slug")?
            .ok_or_else(|| anyhow!("workspace {workspace_id} does not exist"))?;

    let library_rows = sqlx::query(
        "SELECT id, slug, display_name FROM catalog_library WHERE workspace_id = $1 ORDER BY display_name",
    )
    .bind(workspace_id)
    .fetch_all(pool)
    .await
    .context("load workspace libraries")?;
    let libraries: Vec<WorkspaceManifestLibrary> = library_rows
        .into_iter()
        .map(|row| -> anyhow::Result<WorkspaceManifestLibrary> {
            Ok(WorkspaceManifestLibrary {
                id: row.try_get("id").context("decode catalog_library id")?,
                slug: row.try_get("slug").context("decode catalog_library slug")?,
                display_name: row
                    .try_get("display_name")
                    .context("decode catalog_library display_name")?,
            })
        })
        .collect::<anyhow::Result<_>>()?;

    // Plain tar over the writer — the inner library archives are already
    // zstd-compressed, so the outer layer stays uncompressed.
    let mut builder = Builder::new(writer);
    builder.mode(async_tar::HeaderMode::Deterministic);

    let inner_result = Box::pin(export_workspace_archive_body(
        &state,
        workspace_id,
        &workspace_slug,
        &include,
        &libraries,
        &mut builder,
    ))
    .await;
    finalize_archive_with_failure_sentinel(
        builder,
        ArchiveFinalizeScope { owner: ArchiveOwner::Workspace, id: workspace_id },
        inner_result,
    )
    .await
}

pub(super) async fn export_workspace_archive_body<W>(
    state: &AppState,
    workspace_id: Uuid,
    workspace_slug: &str,
    include: &[IncludeKind],
    libraries: &[WorkspaceManifestLibrary],
    builder: &mut Builder<W>,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    // 1. workspace-manifest.json — MUST be first so a reader learns the
    //    library set before the embedded archives stream past.
    let manifest = WorkspaceSnapshotManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        workspace_id,
        workspace_slug: workspace_slug.to_string(),
        exported_at: chrono::Utc::now(),
        source_version: env!("CARGO_PKG_VERSION").to_string(),
        include_kinds: include.to_vec(),
        libraries: libraries.to_vec(),
    };
    append_json_entry(builder, "workspace-manifest.json", &manifest).await?;

    // 2. One library archive at a time, materialized to a temp file so the
    //    tar header carries the exact size. Delete the temp before the next
    //    library to bound scratch usage to a single library.
    let scratch_dir = std::env::var_os(SNAPSHOT_SCRATCH_DIR_ENV)
        .map_or_else(std::env::temp_dir, std::path::PathBuf::from);

    let mut inventory = WorkspaceSnapshotInventory::default();
    {
        let mut stage = WorkspaceExportStage {
            state,
            workspace_id,
            include,
            scratch_dir: &scratch_dir,
            builder: &mut *builder,
            inventory: &mut inventory,
        };
        for library in libraries {
            export_workspace_library_entry(&mut stage, library).await?;
        }
    }

    validate_workspace_snapshot_inventory(&manifest, &inventory, &inventory)?;
    append_json_entry(builder, "workspace-summary.json", &WorkspaceSnapshotSummary { inventory })
        .await?;
    Ok(())
}

/// The workspace archive sink and export scope shared across the per-library
/// loop: app state, the workspace being exported, the include set, the scratch
/// directory for materialized library archives, the outer tar builder and the
/// inventory accumulated so far.
struct WorkspaceExportStage<'a, W>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    state: &'a AppState,
    workspace_id: Uuid,
    include: &'a [IncludeKind],
    scratch_dir: &'a std::path::Path,
    builder: &'a mut Builder<W>,
    inventory: &'a mut WorkspaceSnapshotInventory,
}

/// Exports one manifest library to a bounded temp archive, appends it as a tar
/// entry, and always removes the temp — including when the export or append
/// failed. A failure is returned with the library id attached.
async fn export_workspace_library_entry<W>(
    stage: &mut WorkspaceExportStage<'_, W>,
    library: &WorkspaceManifestLibrary,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let temp_path = stage.scratch_dir.join(format!("ironrag-snapshot-{}.tar.zst", Uuid::now_v7()));
    let export_result =
        Box::pin(export_one_library_to_temp(stage.state, library.id, stage.include, &temp_path))
            .await;
    let append_result = match &export_result {
        Ok(()) => append_library_archive_entry(
            &mut *stage.builder,
            library.id,
            &temp_path,
            &mut *stage.inventory,
        )
        .await
        .with_context(|| format!("append library {} archive entry", library.id)),
        Err(_) => Ok(()),
    };
    // Always clean the temp file before moving on, regardless of outcome.
    if let Err(remove_err) = tokio::fs::remove_file(&temp_path).await
        && remove_err.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(
            workspace_id = %stage.workspace_id,
            library_id = %library.id,
            "workspace snapshot export failed to remove temp library archive",
        );
    }
    export_result.with_context(|| format!("export library {} archive", library.id))?;
    append_result
}

/// Exports one library archive to `temp_path` through the shared
/// [`export_library_archive`], leaving the file ready for stat + tar append.
pub(super) async fn export_one_library_to_temp(
    state: &AppState,
    library_id: Uuid,
    include: &[IncludeKind],
    temp_path: &std::path::Path,
) -> anyhow::Result<()> {
    let file = tokio::fs::File::create(temp_path)
        .await
        .with_context(|| format!("create temp library archive `{}`", temp_path.display()))?;
    Box::pin(export_library_archive(state.clone(), library_id, include.to_vec(), file))
        .await
        .map_err(anyhow::Error::new)?;
    Ok(())
}

/// Stats `temp_path` and appends it as `libraries/{library_id}.tar.zst` with a
/// deterministic regular-file header (mode 0o644, mtime 0).
pub(super) async fn append_library_archive_entry<W>(
    builder: &mut Builder<W>,
    library_id: Uuid,
    temp_path: &std::path::Path,
    inventory: &mut WorkspaceSnapshotInventory,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin + Send + Sync,
{
    let metadata = tokio::fs::metadata(temp_path)
        .await
        .with_context(|| format!("stat temp library archive `{}`", temp_path.display()))?;
    let mut header = Header::new_gnu();
    header.set_size(metadata.len());
    header.set_mode(0o644);
    header.set_mtime(0);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    let path = format!("libraries/{library_id}.tar.zst");
    let bytes = tokio::fs::read(temp_path)
        .await
        .with_context(|| format!("read temp library archive `{}`", temp_path.display()))?;
    anyhow::ensure!(
        bytes.len() as u64 == metadata.len(),
        "temp library archive changed during export"
    );
    inventory.libraries.push(snapshot_entry_digest(path.clone(), &bytes));
    builder
        .append_data(&mut header, path, bytes.as_slice())
        .await
        .with_context(|| format!("append tar entry for library {library_id}"))?;
    Ok(())
}

/// Restores a workspace plain-tar archive into `workspace_id`. For each
/// embedded `libraries/{id}.tar.zst` entry a fresh target library is
/// provisioned in `workspace_id` (so its runtime AI profile is created), then
/// the embedded archive is restored into it via [`restore_library_archive`]
/// in `OverwriteMode::Replace` (the library was just created empty). The
/// caller-supplied `overwrite` is recorded in the report for traceability but
/// each newly minted library is always restored with `Replace`.
pub async fn restore_workspace_archive<R>(
    state: &AppState,
    workspace_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> Result<WorkspaceSnapshotImportReport, SnapshotImportError>
where
    R: AsyncRead + Unpin + Send,
{
    restore_workspace_archive_inner(state, workspace_id, body, overwrite).await.map_err(|error| {
        let failure = SnapshotImportError::new(error);
        tracing::error!(
            %workspace_id,
            failure_code = failure.failure_code(),
            "workspace snapshot import refused",
        );
        failure
    })
}

pub(super) async fn restore_workspace_archive_inner<R>(
    state: &AppState,
    workspace_id: Uuid,
    body: R,
    overwrite: OverwriteMode,
) -> anyhow::Result<WorkspaceSnapshotImportReport>
where
    R: AsyncRead + Unpin + Send,
{
    let workspace_file =
        spool_snapshot_archive(body).await.map_err(classify_snapshot_archive_error)?;
    let (manifest, mut entries) = open_validated_workspace_entries(&workspace_file.path).await?;

    // Index manifest libraries by source id so we can resolve slug +
    // display_name from each `libraries/{id}.tar.zst` path.
    let manifest_by_id: BTreeMap<Uuid, &WorkspaceManifestLibrary> =
        manifest.libraries.iter().map(|library| (library.id, library)).collect();

    let mut report = WorkspaceSnapshotImportReport {
        workspace_id,
        overwrite_mode: overwrite,
        ..Default::default()
    };

    // Libraries this import minted, in creation order. A workspace bundle
    // cannot be one database transaction — each embedded library is restored
    // in its own, and blobs are written outside Postgres entirely — so the
    // all-or-nothing guarantee is kept by unwinding: if any library fails, the
    // ones this import created are deleted again. Without it a failed bundle
    // leaves half a workspace behind, including slug-suffixed duplicates of
    // libraries that were never asked for.
    let mut created_library_ids: Vec<Uuid> = Vec::new();

    // R1: union of PostgreSQL tables touched across all libraries, ANALYZEd
    // once after the import instead of per library.
    let mut deferred_analyze_tables: BTreeMap<String, u64> = BTreeMap::new();

    // Stage 2 — each subsequent entry is a per-library archive. The whole
    // loop shares ONE compensation boundary in this frame: any failure past
    // this point unwinds every library the import created, including ones
    // whose own restore already committed. The loop's former per-exit
    // compensation could only cover the exits that happened to be written.
    run_workspace_restore_with_unwind(
        state,
        workspace_id,
        &mut entries,
        &manifest_by_id,
        &mut WorkspaceRestoreAccumulators {
            report: &mut report,
            created_library_ids: &mut created_library_ids,
            deferred_analyze_tables: &mut deferred_analyze_tables,
        },
    )
    .await?;

    // R1: single end-of-import ANALYZE over the union of touched tables, so the
    // planner gets fresh stats once instead of O(n²) per-library re-scans.
    if let Err(error) =
        analyze_imported_postgres_tables(&state.persistence.postgres, &deferred_analyze_tables)
            .await
    {
        tracing::warn!(
            %workspace_id,
            error = %error,
            "workspace snapshot import deferred postgres stats refresh failed",
        );
    }

    Ok(report)
}

/// Re-opens a spooled workspace archive, replays its manifest and refuses a
/// bundle whose manifest changed between prevalidation and replay.
async fn open_validated_workspace_entries(
    path: &std::path::Path,
) -> anyhow::Result<(WorkspaceSnapshotManifest, async_tar::Entries<BufReader<tokio::fs::File>>)> {
    let manifest = Box::pin(prevalidate_workspace_archive(
        tokio::fs::File::open(path).await.context("reopen workspace snapshot for prevalidation")?,
    ))
    .await
    .map_err(classify_snapshot_archive_error)?;
    let archive = Archive::new(BufReader::new(
        tokio::fs::File::open(path).await.context("reopen validated workspace snapshot archive")?,
    ));
    let mut entries = archive.entries().context("open workspace tar archive")?;
    let replayed_manifest = read_workspace_snapshot_manifest(&mut entries).await?;
    ensure_workspace_snapshot_unchanged(&manifest, &replayed_manifest)?;
    Ok((replayed_manifest, entries))
}

fn ensure_workspace_snapshot_unchanged(
    prevalidated: &WorkspaceSnapshotManifest,
    replayed: &WorkspaceSnapshotManifest,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        prevalidated.workspace_id == replayed.workspace_id
            && prevalidated.libraries.len() == replayed.libraries.len(),
        "workspace snapshot changed after prevalidation"
    );
    Ok(())
}

/// Runs the per-library restore loop under ONE compensation boundary: on any
/// failure it unwinds every library this import created before returning the
/// original error.
async fn run_workspace_restore_with_unwind<R>(
    state: &AppState,
    workspace_id: Uuid,
    entries: &mut async_tar::Entries<R>,
    manifest_by_id: &BTreeMap<Uuid, &WorkspaceManifestLibrary>,
    accumulators: &mut WorkspaceRestoreAccumulators<'_>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send,
{
    let result = restore_workspace_libraries(
        state,
        workspace_id,
        entries,
        manifest_by_id,
        WorkspaceRestoreAccumulators {
            report: &mut *accumulators.report,
            created_library_ids: &mut *accumulators.created_library_ids,
            deferred_analyze_tables: &mut *accumulators.deferred_analyze_tables,
        },
    )
    .await;
    if let Err(error) = result {
        unwind_workspace_import_libraries(
            state,
            workspace_id,
            accumulators.created_library_ids.as_slice(),
        )
        .await;
        return Err(error);
    }
    Ok(())
}

/// Restores every per-library archive entry into `workspace_id`, provisioning a
/// fresh target library per entry and accumulating the deferred ANALYZE union.
///
/// This is the body of the workspace restore loop, extracted so the caller can
/// hold ONE compensation boundary around all of it: a failure anywhere here
/// (including after an earlier library already committed) returns `Err`, and the
/// caller unwinds every library minted across the whole loop.
/// Import-wide accumulators the workspace restore loop fills in.
struct WorkspaceRestoreAccumulators<'a> {
    report: &'a mut WorkspaceSnapshotImportReport,
    created_library_ids: &'a mut Vec<Uuid>,
    deferred_analyze_tables: &'a mut BTreeMap<String, u64>,
}

async fn restore_workspace_libraries<R>(
    state: &AppState,
    workspace_id: Uuid,
    entries: &mut async_tar::Entries<R>,
    manifest_by_id: &BTreeMap<Uuid, &WorkspaceManifestLibrary>,
    accumulators: WorkspaceRestoreAccumulators<'_>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send,
{
    let WorkspaceRestoreAccumulators { report, created_library_ids, deferred_analyze_tables } =
        accumulators;
    while let Some(entry) = entries.next().await {
        let mut entry = entry.context("read workspace tar entry")?;
        let path =
            entry.path().context("read workspace tar entry path")?.to_string_lossy().into_owned();
        validate_archive_path(&path)?;
        if path == "workspace-summary.json" {
            break;
        }
        let Some(source_library_id) = parse_workspace_library_entry_path(&path)? else {
            bail!("unknown workspace snapshot entry `{path}` after prevalidation");
        };
        let source_library = manifest_by_id.get(&source_library_id).ok_or_else(|| {
            anyhow!("workspace archive entry `{path}` is not declared in workspace-manifest.json")
        })?;

        // Provision a fresh target library so its runtime AI profile exists,
        // retrying with `-2`, `-3`… suffixes on slug collision.
        let created = create_target_library(
            state,
            workspace_id,
            &source_library.slug,
            &source_library.display_name,
        )
        .await?;
        created_library_ids.push(created.id);

        // Restore the embedded archive directly from the tar entry stream —
        // it is itself an AsyncRead over the library's tar.zst bytes. Replace
        // mode is correct because the library was just created empty.
        //
        // R1: defer the per-library ANALYZE — the shared snapshot tables grow
        // with every library, so a per-library ANALYZE is O(n²) over a mass
        // import. We run a single ANALYZE over the union of touched tables
        // after the loop instead.
        let library_report = Box::pin(restore_library_archive_inner(
            state,
            created.id,
            &mut entry,
            OverwriteMode::Replace,
            RestoreStatsMode::Deferred,
        ))
        .await
        .with_context(|| {
            format!(
                "restore library `{}` (source {source_library_id}) into workspace {workspace_id}",
                created.slug
            )
        })?;
        for (table, count) in &library_report.postgres_rows_by_table {
            if *count > 0 {
                *deferred_analyze_tables.entry(table.clone()).or_insert(0) += *count;
            }
        }

        report.libraries.push(WorkspaceLibraryImportReport {
            library_id: created.id,
            slug: created.slug,
            postgres_rows_by_table: library_report.postgres_rows_by_table,
            blobs_restored: library_report.blobs_restored,
            blobs_verified: library_report.blobs_verified,
            unverifiable_blobs: library_report.unverifiable_blobs,
        });
        report.blobs_verified += library_report.blobs_verified;
        report.unverifiable_blobs += library_report.unverifiable_blobs;
        report.libraries_restored += 1;
    }
    Ok(())
}

/// Deletes the libraries a failed workspace import created, newest first.
///
/// Best-effort by nature: the bundle already failed, and a library that cannot
/// be removed is reported rather than allowed to mask the original error. The
/// delete takes the restore-only catalog path, which the caller drives from the
/// coordinates captured at provisioning: it never loads the library, so it
/// reads no derived readiness. That keeps compensation working while the
/// readiness source is down — the very condition a failed import may be
/// recovering from. Blob stash, durable-work check and schema cascades are the
/// same as an operator-initiated delete.
pub(super) async fn unwind_workspace_import_libraries(
    state: &AppState,
    workspace_id: Uuid,
    created_library_ids: &[Uuid],
) {
    for library_id in created_library_ids.iter().rev() {
        unwind_one_workspace_library(state, workspace_id, *library_id).await;
    }
}

/// Best-effort delete of one library a failed workspace import created. The
/// outcome is logged, never propagated: the bundle already failed and must not
/// gain a second error.
async fn unwind_one_workspace_library(state: &AppState, workspace_id: Uuid, library_id: Uuid) {
    match state
        .application_services
        .catalog
        .delete_library_for_restore(state, workspace_id, library_id)
        .await
    {
        Ok(_) => tracing::info!(
            %workspace_id,
            %library_id,
            "removed library created by a failed workspace snapshot import",
        ),
        Err(error) => log_workspace_library_unwind_failure(workspace_id, library_id, error),
    }
}

fn log_workspace_library_unwind_failure(
    workspace_id: Uuid,
    library_id: Uuid,
    _error: impl std::fmt::Debug,
) {
    tracing::error!(
        %workspace_id,
        %library_id,
        "could not remove a library created by a failed workspace snapshot import",
    );
}

pub(super) async fn read_workspace_snapshot_manifest<R>(
    entries: &mut async_tar::Entries<R>,
) -> anyhow::Result<WorkspaceSnapshotManifest>
where
    R: AsyncRead + Unpin,
{
    let Some(entry) = entries.next().await else {
        bail!("workspace snapshot archive missing workspace-manifest.json");
    };
    let mut entry = entry.context("read workspace tar entry")?;
    let path =
        entry.path().context("read workspace tar entry path")?.to_string_lossy().into_owned();
    validate_archive_path(&path)?;
    if path != "workspace-manifest.json" {
        bail!("workspace tar entry `{path}` arrived before workspace-manifest.json");
    }
    let declared_size = entry.header().size().context("read workspace-manifest.json size")?;
    let manifest: WorkspaceSnapshotManifest =
        read_bounded_json_entry(&mut entry, "workspace-manifest.json", declared_size).await?;
    validate_workspace_snapshot_schema_version(manifest.schema_version)?;
    Ok(manifest)
}

pub(super) fn validate_workspace_snapshot_schema_version(
    schema_version: u32,
) -> anyhow::Result<()> {
    validate_snapshot_schema_version(schema_version, "workspace snapshot")
}

/// Parses `libraries/{uuid}.tar.zst` and returns the embedded library id.
/// Returns `Ok(None)` for any path that is not a library archive entry.
pub(super) fn parse_workspace_library_entry_path(path: &str) -> anyhow::Result<Option<Uuid>> {
    let Some(rest) = path.strip_prefix("libraries/") else {
        return Ok(None);
    };
    let Some(id_str) = rest.strip_suffix(".tar.zst") else {
        return Ok(None);
    };
    if id_str.contains('/') {
        return Ok(None);
    }
    Uuid::parse_str(id_str)
        .map(Some)
        .with_context(|| format!("parse library id from workspace archive entry `{path}`"))
}

/// A target library minted by the workspace restore path.
pub(super) struct CreatedTargetLibrary {
    id: Uuid,
    slug: String,
}

/// Provisions a fresh library in `workspace_id` through [`CatalogService`] so
/// its runtime AI profile is created, retrying with `-2`, `-3`… slug suffixes
/// when the requested slug collides with a sibling library
/// (`catalog_library_workspace_id_slug_key`).
pub(super) async fn create_target_library(
    state: &AppState,
    workspace_id: Uuid,
    source_slug: &str,
    display_name: &str,
) -> anyhow::Result<CreatedTargetLibrary> {
    const MAX_SLUG_ATTEMPTS: u32 = 50;
    for attempt in 1..=MAX_SLUG_ATTEMPTS {
        let candidate_slug =
            if attempt == 1 { source_slug.to_string() } else { format!("{source_slug}-{attempt}") };
        let command = crate::services::catalog_service::CreateLibraryCommand {
            workspace_id,
            slug: Some(candidate_slug.clone()),
            display_name: display_name.to_string(),
            description: None,
            created_by_principal_id: None,
        };
        match state.application_services.catalog.create_library_for_restore(state, &command).await {
            Ok((id, slug)) => {
                return Ok(CreatedTargetLibrary { id, slug });
            }
            Err(crate::interfaces::http::router_support::ApiError::Conflict(_)) => {
                // Slug collided with a sibling library — try the next suffix.
                continue;
            }
            Err(error) => {
                bail!(
                    "create target library for workspace {workspace_id} (slug `{candidate_slug}`): {error:?}"
                );
            }
        }
    }
    bail!(
        "could not allocate a free slug for workspace {workspace_id} library `{source_slug}` after {MAX_SLUG_ATTEMPTS} attempts"
    )
}
