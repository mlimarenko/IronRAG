use super::*;

/// Whether a single library restore should refresh `PostgreSQL` planner stats
/// itself, or defer to one ANALYZE pass run by the caller (Workstream R / R1).
///
/// Snapshot tables (`runtime_graph_*`, `knowledge_*`, the per-dim vector
/// shards) are physically shared and grow with every library imported. Running
/// `ANALYZE {table}` after each library — as the single-library path correctly
/// does — re-scans the whole growing table per library, which is O(n²) over a
/// mass workspace import. In [`Self::Deferred`] the per-library restore skips
/// ANALYZE entirely and the workspace driver runs a single ANALYZE over the
/// union of touched tables once the import is done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RestoreStatsMode {
    /// Single-library restore: ANALYZE every touched table once at the end of
    /// this library import so the planner immediately has fresh stats.
    PerLibrary,
    /// Mass / workspace import: skip per-library ANALYZE; the caller runs one
    /// ANALYZE pass after all libraries are restored.
    Deferred,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum TargetRestoreFootprint {
    Empty,
    Populated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RestoreLibraryDataAction {
    Skip,
    ImportIntoEmptyTarget,
    ReplaceTarget,
    RejectPopulatedTarget,
}

pub(super) fn plan_restore_library_data(
    include_kinds: &[IncludeKind],
    overwrite: OverwriteMode,
    target_footprint: TargetRestoreFootprint,
) -> RestoreLibraryDataAction {
    if !include_kinds.contains(&IncludeKind::LibraryData) {
        return RestoreLibraryDataAction::Skip;
    }
    match (overwrite, target_footprint) {
        (OverwriteMode::Reject, TargetRestoreFootprint::Empty) => {
            RestoreLibraryDataAction::ImportIntoEmptyTarget
        }
        (OverwriteMode::Reject, TargetRestoreFootprint::Populated) => {
            RestoreLibraryDataAction::RejectPopulatedTarget
        }
        (OverwriteMode::Replace, _) => RestoreLibraryDataAction::ReplaceTarget,
    }
}

pub(super) struct LibraryRestoreTarget {
    pub(super) workspace_id: Uuid,
    pub(super) library_slug: String,
    pub(super) source_truth_version: i64,
    pub(super) action: RestoreLibraryDataAction,
}

pub(super) async fn prepare_library_restore_target(
    state: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
    manifest: &SnapshotManifest,
    overwrite: OverwriteMode,
    stashed_storage: &mut Option<StashedContentDirectory>,
) -> anyhow::Result<LibraryRestoreTarget> {
    let LockedTargetLibrary { workspace_id, slug, source_truth_version } =
        lock_catalog_library_for_restore(tx, library_id).await?;
    let target_footprint = determine_target_restore_footprint(
        state,
        tx,
        library_id,
        &manifest.include_kinds,
        overwrite,
    )
    .await?;
    let action = plan_restore_library_data(&manifest.include_kinds, overwrite, target_footprint);
    if action == RestoreLibraryDataAction::ReplaceTarget {
        *stashed_storage =
            prepare_replace_library_footprint(state, library_id, Some(workspace_id)).await?;
    }
    Ok(LibraryRestoreTarget { workspace_id, library_slug: slug, source_truth_version, action })
}

pub(super) async fn determine_target_restore_footprint(
    state: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
    include_kinds: &[IncludeKind],
    overwrite: OverwriteMode,
) -> anyhow::Result<TargetRestoreFootprint> {
    let should_check =
        include_kinds.contains(&IncludeKind::LibraryData) && overwrite == OverwriteMode::Reject;
    if should_check && tx_library_has_restore_footprint(state, tx, library_id).await? {
        Ok(TargetRestoreFootprint::Populated)
    } else {
        Ok(TargetRestoreFootprint::Empty)
    }
}

pub(super) async fn run_restore_preflight_before_side_effects<F, Fut>(
    library_id: Uuid,
    action: RestoreLibraryDataAction,
    side_effect: F,
) -> anyhow::Result<()>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    if action == RestoreLibraryDataAction::RejectPopulatedTarget {
        return restore_target_conflict(library_id);
    }
    side_effect().await
}

pub(super) fn restore_target_conflict(library_id: Uuid) -> anyhow::Result<()> {
    Err(SnapshotImportRejection::new(
        SnapshotImportFailureKind::TargetPopulated,
        format!(
            "target library data conflict: library {library_id} already contains data — pass overwrite=replace to restore over it"
        ),
    )
    .into_error())
}

pub(super) async fn reject_or_clear_target_restore_footprint(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
    action: RestoreLibraryDataAction,
) -> anyhow::Result<()> {
    match action {
        RestoreLibraryDataAction::Skip | RestoreLibraryDataAction::ImportIntoEmptyTarget => Ok(()),
        RestoreLibraryDataAction::RejectPopulatedTarget => restore_target_conflict(library_id),
        RestoreLibraryDataAction::ReplaceTarget => {
            clear_library_postgres_footprint(tx, library_id).await
        }
    }
}

pub(super) async fn prepare_replace_library_footprint(
    state: &AppState,
    library_id: Uuid,
    existing_workspace_id: Option<Uuid>,
) -> anyhow::Result<Option<StashedContentDirectory>> {
    if state.settings.knowledge_plane_backend != "postgres" {
        bail!(
            "unsupported knowledge_plane_backend `{}` for snapshot restore",
            state.settings.knowledge_plane_backend
        );
    }

    // Blob storage is keyed by the existing library workspace. Capture
    // it before the restore writes replacement blobs under the same
    // library identity.
    let stashed_storage = if let Some(workspace_id) = existing_workspace_id {
        state
            .content_storage
            .stash_library_storage(workspace_id, library_id)
            .await
            .context("stash library blobs before restore")?
    } else {
        None
    };

    Ok(stashed_storage)
}

pub(super) async fn clear_library_postgres_footprint(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<()> {
    clear_pg_vector_relations_for_library(tx, library_id).await?;

    // Tables to wipe for this library, in reverse dependency order.
    //
    // Derived rows are cleared explicitly under replica mode. Content deletes
    // enable origin mode: the registry must cascade to its representations and
    // preserve the deferred exclusivity fence. Include unexported derived rows.
    let mut reverse: Vec<&str> = Vec::new();
    for table in POSTGRES_LIBRARY_OWNED_UNEXPORTED_TABLES {
        reverse.push(*table);
    }
    reverse.push("knowledge_vector_relation_manifest");
    for table in POSTGRES_KNOWLEDGE_EDGE_TABLES.iter().rev() {
        reverse.push(*table);
    }
    for table in POSTGRES_KNOWLEDGE_BASE_TABLES.iter().rev() {
        reverse.push(*table);
    }
    for table in POSTGRES_RUNTIME_GRAPH_TABLES.iter().rev() {
        reverse.push(*table);
    }
    for table in POSTGRES_CONTENT_TABLES.iter().rev() {
        if matches!(*table, "content_mutation" | "content_mutation_predecessor_audit") {
            // Registry deletion owns both representations and their cascades.
            continue;
        }
        reverse.push(*table);
    }
    let mut content_constraints_enabled = false;
    for table in reverse {
        if !content_constraints_enabled && POSTGRES_CONTENT_TABLES.contains(&table) {
            sqlx::query("SET LOCAL session_replication_role = 'origin'")
                .execute(&mut **tx)
                .await
                .context("enable registry cleanup cascades")?;
            content_constraints_enabled = true;
        }
        let sql = match table {
            "content_chunk" => "DELETE FROM content_chunk c
                 USING content_revision r
                 WHERE r.id = c.revision_id AND r.library_id = $1"
                .to_string(),
            "content_mutation_item" => "DELETE FROM content_mutation_item i
                 USING content_mutation_reference m
                 WHERE m.id = i.mutation_id AND m.library_id = $1"
                .to_string(),
            "content_visibility_intent" => "DELETE FROM content_visibility_intent v
                 USING content_document d
                 WHERE d.id = v.document_id AND d.library_id = $1"
                .to_string(),
            "content_document_head" => "DELETE FROM content_document_head h
                 USING content_document d
                 WHERE d.id = h.document_id AND d.library_id = $1"
                .to_string(),
            "knowledge_vector_relation_manifest" => {
                "DELETE FROM knowledge_vector_relation_manifest WHERE library_id = $1".to_string()
            }
            _ => format!("DELETE FROM {table} WHERE library_id = $1"),
        };
        sqlx::query(sqlx::AssertSqlSafe(&*sql))
            .bind(library_id)
            .execute(&mut **tx)
            .await
            .with_context(|| format!("clear pg table {table}"))?;
    }
    sqlx::query("SET LOCAL session_replication_role = 'replica'")
        .execute(&mut **tx)
        .await
        .context("resume derived snapshot import after content cleanup")?;
    Ok(())
}

pub(super) async fn clear_pg_vector_relations_for_library(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<()> {
    let mut relations =
        crate::infra::postgres::pg_search_store::list_physical_vector_relations_in_transaction(
            tx,
            PER_DIM_CHUNK_VECTOR_PREFIX,
        )
        .await
        .context("list physical chunk vector relations before snapshot replace")?;
    relations.extend(
        crate::infra::postgres::pg_search_store::list_physical_vector_relations_in_transaction(
            tx,
            PER_DIM_ENTITY_VECTOR_PREFIX,
        )
        .await
        .context("list physical entity vector relations before snapshot replace")?,
    );
    for relation_name in relations {
        validate_snapshot_pg_table_name(&relation_name)?;
        if !is_runtime_vector_relation_name(&relation_name) {
            bail!("vector manifest relation `{relation_name}` is not a vector relation");
        }
        let relation = quote_pg_identifier(&relation_name)?;
        sqlx::query(sqlx::AssertSqlSafe(format!("DELETE FROM {relation} WHERE library_id = $1")))
            .bind(library_id)
            .execute(&mut **tx)
            .await
            .with_context(|| format!("clear vector relation {relation_name}"))?;
    }
    Ok(())
}

pub(super) struct LockedTargetLibrary {
    workspace_id: Uuid,
    slug: String,
    source_truth_version: i64,
}

pub(super) async fn lock_catalog_library_for_restore(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<LockedTargetLibrary> {
    let locked: Option<(Uuid, String, i64)> = sqlx::query_as(
        "SELECT workspace_id, slug, coalesce(source_truth_version, 1)
         FROM catalog_library
         WHERE id = $1
         FOR UPDATE",
    )
    .bind(library_id)
    .fetch_optional(&mut **tx)
    .await
    .context("lock catalog_library before snapshot restore")?;
    let Some((workspace_id, slug, source_truth_version)) = locked else {
        bail!(
            "target library {library_id} does not exist; create/select a library before restoring a snapshot"
        );
    };
    Ok(LockedTargetLibrary { workspace_id, slug, source_truth_version })
}

pub(super) fn minimum_source_truth_version_after_restore(
    locked_target_version: i64,
) -> anyhow::Result<i64> {
    locked_target_version.max(0).checked_add(1).ok_or_else(|| {
        anyhow!("target library source truth version cannot advance beyond {locked_target_version}")
    })
}

pub(super) const fn advance_restored_library_source_truth_version_sql() -> &'static str {
    "with advanced_library as materialized (
        update catalog_library
        set source_truth_version = greatest(
            coalesce(source_truth_version, 0) + 1,
            $2,
            (extract(epoch from clock_timestamp()) * 1000000)::bigint
        )
        where id = $1
        returning source_truth_version
     )
     select source_truth_version
     from advanced_library
     where source_truth_version >= $2"
}

pub(super) async fn advance_restored_library_source_truth_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
    locked_target_version: i64,
) -> anyhow::Result<i64> {
    let minimum_version = minimum_source_truth_version_after_restore(locked_target_version)?;
    sqlx::query_scalar::<_, i64>(advance_restored_library_source_truth_version_sql())
        .bind(library_id)
        .bind(minimum_version)
        .fetch_one(&mut **tx)
        .await
        .context("advance library source truth after snapshot restore")
}

pub(super) async fn tx_library_has_restore_footprint(
    _state: &AppState,
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<bool> {
    postgres_library_has_restore_footprint(tx, library_id).await
}

pub(super) async fn postgres_library_has_restore_footprint(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    library_id: Uuid,
) -> anyhow::Result<bool> {
    let mut clauses = Vec::new();
    for table in POSTGRES_CONTENT_TABLES
        .iter()
        .chain(POSTGRES_RUNTIME_GRAPH_TABLES.iter())
        .chain(POSTGRES_KNOWLEDGE_TABLES.iter())
        .copied()
    {
        if let Some(clause) = postgres_restore_footprint_clause(table)? {
            clauses.push(clause);
        }
    }
    if clauses.is_empty() {
        return Ok(false);
    }
    let sql = format!("SELECT {}", clauses.join(" OR "));
    let has_footprint = sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(&*sql))
        .bind(library_id)
        .fetch_one(&mut **tx)
        .await
        .context("check target library snapshot restore footprint")?;
    Ok(has_footprint)
}

pub(super) fn postgres_restore_footprint_clause(table: &str) -> anyhow::Result<Option<String>> {
    let clause = match table {
        "content_chunk" => "EXISTS (SELECT 1 FROM content_chunk c \
             JOIN content_revision r ON r.id = c.revision_id \
             WHERE r.library_id = $1)"
            .to_string(),
        "content_mutation_item" => "EXISTS (SELECT 1 FROM content_mutation_item i \
             JOIN content_mutation_reference m ON m.id = i.mutation_id \
             WHERE m.library_id = $1)"
            .to_string(),
        "content_visibility_intent" => "EXISTS (SELECT 1 FROM content_visibility_intent v \
             JOIN content_document d ON d.id = v.document_id \
             WHERE d.library_id = $1)"
            .to_string(),
        "content_document_head" => "EXISTS (SELECT 1 FROM content_document_head h \
             JOIN content_document d ON d.id = h.document_id \
             WHERE d.library_id = $1)"
            .to_string(),
        direct => {
            let direct = require_known_snapshot_pg_table(direct)?;
            let table = quote_pg_identifier(direct)?;
            format!("EXISTS (SELECT 1 FROM {table} WHERE library_id = $1)")
        }
    };
    Ok(Some(clause))
}

/// Lazy-ensure every per-dim vector shard derived from the snapshot's declared
/// Postgres sections,
/// so the import path can stream rows back in without first running a
/// fresh ingest to materialize the collections. ANN + persistent index
/// parameters come from the authoritative search-store config the target
/// deployment is already running with — they are deployment-side knobs
/// rather than snapshot payload, mirroring how new shards are created
/// on first ingest of an unseen dim.
pub(super) async fn ensure_manifest_vector_shards(
    state: &AppState,
    manifest_sections: &SnapshotManifestSections,
) -> anyhow::Result<()> {
    let search_store = &state.search_store;
    for (relation_name, dim) in &manifest_sections.vector_shards {
        if is_per_dim_chunk_vector_collection_name(relation_name) {
            search_store.ensure_chunk_vector_shard(*dim).await.with_context(|| {
                format!("ensure per-dim chunk vector shard {relation_name} for restore")
            })?;
        } else {
            search_store.ensure_entity_vector_shard(*dim).await.with_context(|| {
                format!("ensure per-dim entity vector shard {relation_name} for restore")
            })?;
        }
    }
    Ok(())
}
