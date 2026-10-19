use super::*;

type VectorManifestLane = (String, String);
type VectorManifestDecrements = HashMap<VectorManifestLane, i64>;
type DeletedVectorLane = (String, String, i64);
/// Lane-keyed map of the physical relation a staged-cleanup row touched:
/// `(dimensions, vector_kind, relation_name)`, used to reconcile manifest
/// counts after source-less rows are removed.
type TouchedVectorRelation = (u64, String, String);

pub(super) fn ensure_single_manifest_row_updated(rows_affected: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        rows_affected == 1,
        "vector manifest reconciliation updated {rows_affected} rows; expected exactly one prepared lane"
    );
    Ok(())
}

fn vector_plane_data_advisory_lock_name(library_id: Uuid) -> String {
    format!("{VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX}:{library_id}")
}

pub(super) async fn lock_library_vector_plane_data(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    exclusive: bool,
) -> anyhow::Result<()> {
    let lock_name = vector_plane_data_advisory_lock_name(library_id);
    if exclusive {
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(lock_name)
            .execute(&mut **transaction)
            .await
            .context("acquire exclusive library vector-plane data lock")?;
    } else {
        sqlx::query("select pg_advisory_xact_lock_shared(hashtextextended($1::text, 0))")
            .bind(lock_name)
            .execute(&mut **transaction)
            .await
            .context("acquire shared library vector-plane data lock")?;
    }
    Ok(())
}

pub(super) async fn validate_vector_write_fence(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    fence: &VectorWriteFence,
    active_profile_source: Option<&dyn ActiveEmbeddingProfileSource>,
) -> anyhow::Result<i64> {
    anyhow::ensure!(
        fence.expected_source_truth_version > 0,
        "fenced vector write requires a positive source-truth fence"
    );
    let source_truth_version = if fence.advance_source_truth_version {
        sqlx::query_scalar::<_, i64>(
            "select source_truth_version
             from catalog_library
             where id = $1
             for update",
        )
        .bind(library_id)
        .fetch_optional(&mut **transaction)
        .await
    } else {
        sqlx::query_scalar::<_, i64>(
            "select source_truth_version
             from catalog_library
             where id = $1
             for share",
        )
        .bind(library_id)
        .fetch_optional(&mut **transaction)
        .await
    }
    .context("lock fenced vector write source/profile fence")?
    .ok_or_else(|| anyhow::anyhow!("library disappeared during fenced vector write"))?;
    let revision_scoped = if let Some(active_profile_source) = active_profile_source {
        anyhow::ensure!(
            !fence.advance_source_truth_version && fence.ingest_attempt.is_some(),
            "profile-scoped vector writes require non-advancing ingest authority"
        );
        let active_embedding_profile_key = active_profile_source
            .active_embedding_profile_key(library_id)
            .await
            .context("resolve active embedding profile under vector write fence")?;
        validate_vector_embedding_profile_fence(fence, &active_embedding_profile_key)?;
        true
    } else {
        false
    };
    validate_vector_source_truth_fence(fence, source_truth_version, revision_scoped)?;
    if let Some(ingest_attempt) = fence.ingest_attempt {
        let is_authoritative =
            lock_latest_leased_ingest_attempt_authority(transaction, library_id, ingest_attempt)
                .await?;
        validate_ingest_attempt_write_authority(is_authoritative)?;
    }
    Ok(source_truth_version)
}

pub(super) fn validate_vector_source_truth_fence(
    fence: &VectorWriteFence,
    observed_source_truth_version: i64,
    revision_scoped: bool,
) -> anyhow::Result<()> {
    if revision_scoped {
        anyhow::ensure!(
            observed_source_truth_version >= fence.expected_source_truth_version,
            "library source version moved backwards before fenced vector write"
        );
        return Ok(());
    }
    anyhow::ensure!(
        observed_source_truth_version == fence.expected_source_truth_version,
        "library source or embedding profile changed before fenced vector write"
    );
    Ok(())
}

pub(super) fn validate_vector_embedding_profile_fence(
    fence: &VectorWriteFence,
    active_embedding_profile_key: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        fence.embedding_profile_key == active_embedding_profile_key,
        "library embedding profile changed before fenced vector write"
    );
    Ok(())
}

pub(super) fn validate_ingest_attempt_write_authority(
    is_authoritative: bool,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        is_authoritative,
        "ingest attempt authority changed before fenced vector write"
    );
    Ok(())
}

/// Lock and validate the queue authority used by a short vector transaction.
///
/// Required vector-write lock order is library -> job -> attempt. The caller has already
/// locked the library source row; holding the job row in SHARE mode makes
/// lease recovery/finalization wait until the vector transaction commits. No
/// provider I/O runs while these locks are held.
async fn lock_latest_leased_ingest_attempt_authority(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    ingest_attempt: IngestVectorWriteFence,
) -> anyhow::Result<bool> {
    ingest_repository::lock_latest_leased_revision_attempt(
        transaction,
        library_id,
        ingest_attempt.attempt_id,
        ingest_attempt.revision_id,
    )
    .await
    .context("lock latest vector ingest authority")
}

pub(super) async fn finish_vector_write_fence(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    fence: &VectorWriteFence,
    observed_source_truth_version: i64,
) -> anyhow::Result<i64> {
    if !fence.advance_source_truth_version {
        return Ok(observed_source_truth_version);
    }
    advance_vector_source_truth_version(transaction, library_id, observed_source_truth_version)
        .await
}

/// Validate the fence for a destructive vector delete that is scoped to one
/// revision or to an explicit vector-id set.
///
/// Every selector accepted by `delete_chunk_vectors_fenced` is revision-scoped:
/// the caller reads the library source generation before the transaction, and a
/// live connector writing the same library advances that generation for
/// unrelated reasons while the delete is in flight. Exact compare-and-advance is
/// reserved for library-global operations (`purge_empty_library_vector_plane`
/// and `promote_staged_vector_rebuild`); a revision-scoped delete instead needs
/// the same monotonic lower bound as a revision-scoped write. The exclusive
/// library vector-plane lock and the locked library row held here already
/// serialize the mutation against profile promotion, empty-plane purge and
/// shared writes, so accepting an unrelated advance cannot corrupt the vector
/// plane. The generation is only required not to move backwards, and the value
/// read under the lock is what the delete advances.
async fn validate_revision_scoped_vector_delete_fence(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    expected_source_truth_version: i64,
) -> anyhow::Result<i64> {
    let observed_source_truth_version =
        lock_exclusive_vector_mutation_source(transaction, library_id).await?;
    ensure_revision_scoped_vector_delete_source_truth(
        expected_source_truth_version,
        observed_source_truth_version,
    )?;
    Ok(observed_source_truth_version)
}

pub(super) fn ensure_revision_scoped_vector_delete_source_truth(
    expected_source_truth_version: i64,
    observed_source_truth_version: i64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        expected_source_truth_version > 0,
        "vector delete requires a positive source-truth fence"
    );
    anyhow::ensure!(
        observed_source_truth_version >= expected_source_truth_version,
        "library source version moved backwards before destructive vector mutation"
    );
    Ok(())
}

/// Validate the fence for a library-global destructive vector delete.
///
/// A library-global delete replaces the whole vector plane, so the caller's
/// captured generation must still be current: an unrelated library-global
/// change means the library moved and the caller must restart its rebuild.
/// This is the same exact compare-and-advance protocol as empty-plane purge and
/// staged promotion, and its refusal is returned to the caller, never swallowed.
async fn validate_library_global_vector_delete_fence(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    expected_source_truth_version: i64,
) -> anyhow::Result<i64> {
    anyhow::ensure!(
        expected_source_truth_version > 0,
        "vector delete requires a positive source-truth fence"
    );
    let observed_source_truth_version =
        lock_exclusive_vector_mutation_source(transaction, library_id).await?;
    anyhow::ensure!(
        observed_source_truth_version == expected_source_truth_version,
        "library source changed before destructive library-wide vector mutation"
    );
    ensure_no_authoritative_vector_candidates(transaction, library_id).await?;
    Ok(observed_source_truth_version)
}

pub(crate) async fn lock_exclusive_vector_mutation_source(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> anyhow::Result<i64> {
    lock_library_vector_plane_data(transaction, library_id, true).await?;
    sqlx::query_scalar::<_, i64>(
        "select source_truth_version
         from catalog_library
         where id = $1
         for update",
    )
    .bind(library_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock destructive vector mutation source fence")?
    .ok_or_else(|| anyhow::anyhow!("library disappeared during destructive vector mutation"))
}

pub(crate) async fn advance_vector_source_truth_version(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    observed_source_truth_version: i64,
) -> anyhow::Result<i64> {
    sqlx::query_scalar::<_, i64>(
        "update catalog_library
         set source_truth_version = greatest(
                coalesce(source_truth_version, 0) + 1,
                (extract(epoch from clock_timestamp()) * 1000000)::bigint
             ),
             updated_at = now()
         where id = $1 and source_truth_version = $2
         returning source_truth_version",
    )
    .bind(library_id)
    .bind(observed_source_truth_version)
    .fetch_optional(&mut **transaction)
    .await
    .context("advance source fence with vector mutation")?
    .ok_or_else(|| anyhow::anyhow!("library source fence changed during vector mutation"))
}

pub(super) fn vector_rebuild_staging_source_truth_version(
    profile_key: &str,
) -> anyhow::Result<i64> {
    let digest = profile_key
        .strip_prefix(VECTOR_REBUILD_STAGING_PROFILE_PREFIX)
        .context("vector rebuild staging profile has an invalid protocol prefix")?;
    anyhow::ensure!(
        digest.len() == VECTOR_REBUILD_STAGING_PROFILE_DIGEST_LEN
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "vector rebuild staging profile has an invalid digest"
    );
    let encoded_version =
        digest.get(..16).context("vector rebuild staging profile has no source epoch")?;
    let source_truth_version = u64::from_str_radix(encoded_version, 16)
        .context("vector rebuild staging profile has an invalid source epoch")?;
    let source_truth_version = i64::try_from(source_truth_version)
        .context("vector rebuild staging source epoch overflowed i64")?;
    anyhow::ensure!(
        source_truth_version > 0,
        "vector rebuild staging source epoch must be positive"
    );
    Ok(source_truth_version)
}

pub(super) fn validate_vector_rebuild_staging_profile_key(profile_key: &str) -> anyhow::Result<()> {
    vector_rebuild_staging_source_truth_version(profile_key).map(|_| ())
}

pub(super) fn validate_vector_rebuild_staging_source_truth_version(
    profile_key: &str,
    observed_source_truth_version: i64,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        vector_rebuild_staging_source_truth_version(profile_key)? == observed_source_truth_version,
        "vector rebuild staging source epoch changed before batch write"
    );
    Ok(())
}

pub(super) fn validate_exact_embedding_profile_key(profile_key: &str) -> anyhow::Result<()> {
    let digest = profile_key
        .strip_prefix(EMBEDDING_PROFILE_PREFIX)
        .context("embedding profile dimension claim has an invalid protocol prefix")?;
    anyhow::ensure!(
        digest.len() == EMBEDDING_PROFILE_DIGEST_LEN
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "embedding profile dimension claim has an invalid digest"
    );
    Ok(())
}

pub(super) fn validate_exact_profile_dimension_claim(
    relation_prefix: &str,
    claims: &[(i32, String)],
) -> anyhow::Result<Option<u64>> {
    match claims {
        [] => Ok(None),
        [(dimensions, relation_name)] => {
            let dimensions = u64::try_from(*dimensions)
                .context("exact-profile vector dimension claim was negative")?;
            checked_dim_i32(dimensions)?;
            validate_relation_name(relation_name, relation_prefix)?;
            anyhow::ensure!(
                relation_name == &vector_relation_name(relation_prefix, dimensions)?,
                "exact-profile vector dimension claim points to a different dimension relation"
            );
            Ok(Some(dimensions))
        }
        _ => {
            anyhow::bail!("exact embedding execution profile has multiple vector dimension claims")
        }
    }
}

pub(super) async fn lock_prepared_manifest_lane_for_share(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    dimensions: u64,
    vector_kind: &str,
    embedding_model_key: &str,
    relation_prefix: &str,
) -> anyhow::Result<String> {
    validate_vector_rebuild_staging_profile_key(embedding_model_key)?;
    let relation_name = sqlx::query_scalar::<_, String>(
        "select relation_name
         from knowledge_vector_relation_manifest
         where library_id = $1
           and dim = $2
           and vector_kind = $3
           and embedding_model_key = $4
           and promoted = false
         for share",
    )
    .bind(library_id)
    .bind(checked_dim_i32(dimensions)?)
    .bind(vector_kind)
    .bind(embedding_model_key)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock prepared vector manifest lane")?
    .ok_or_else(|| anyhow::anyhow!("prepared vector manifest lane is missing or promoted"))?;
    validate_relation_name(&relation_name, relation_prefix)?;
    let expected_relation_name = vector_relation_name(relation_prefix, dimensions)?;
    anyhow::ensure!(
        relation_name == expected_relation_name,
        "prepared vector manifest points to an unexpected dimension relation"
    );
    Ok(relation_name)
}

pub(super) async fn discard_staged_vector_rebuild_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    dimensions: u64,
    staging_embedding_model_key: &str,
) -> anyhow::Result<BTreeSet<TouchedVectorRelation>> {
    validate_vector_rebuild_staging_profile_key(staging_embedding_model_key)?;
    let lanes = sqlx::query_as::<_, (String, String)>(
        "select vector_kind, relation_name
         from knowledge_vector_relation_manifest
         where library_id = $1
           and dim = $2
           and embedding_model_key = $3
           and promoted = false
         order by vector_kind
         for update",
    )
    .bind(library_id)
    .bind(checked_dim_i32(dimensions)?)
    .bind(staging_embedding_model_key)
    .fetch_all(&mut **transaction)
    .await
    .context("list staged vector lanes for cleanup")?;
    let mut touched_relations = BTreeSet::new();
    for (vector_kind, relation_name) in lanes {
        let relation_prefix = match vector_kind.as_str() {
            KNOWLEDGE_CHUNK_VECTOR_KIND => CHUNK_VECTOR_RELATION_PREFIX,
            KNOWLEDGE_ENTITY_VECTOR_KIND => ENTITY_VECTOR_RELATION_PREFIX,
            _ => anyhow::bail!("staged vector manifest has an unsupported vector kind"),
        };
        validate_relation_name(&relation_name, relation_prefix)?;
        let expected_relation_name = vector_relation_name(relation_prefix, dimensions)?;
        anyhow::ensure!(
            relation_name == expected_relation_name,
            "staged vector manifest points to an unexpected dimension relation"
        );
    }
    for (vector_kind, relation_prefix) in [
        (KNOWLEDGE_CHUNK_VECTOR_KIND, CHUNK_VECTOR_RELATION_PREFIX),
        (KNOWLEDGE_ENTITY_VECTOR_KIND, ENTITY_VECTOR_RELATION_PREFIX),
    ] {
        let relation_name = vector_relation_name(relation_prefix, dimensions)?;
        let relation_exists = sqlx::query_scalar::<_, bool>(
            "select to_regclass(format('%I.%I', current_schema(), $1)) is not null",
        )
        .bind(&relation_name)
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("inspect staged cleanup relation {relation_name}"))?;
        if !relation_exists {
            continue;
        }
        let relation = quote_identifier(&relation_name)?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {relation}
             where library_id = $1
               and vector_kind = $2
               and embedding_model_key = $3"
        )))
        .bind(library_id)
        .bind(vector_kind)
        .bind(staging_embedding_model_key)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("delete staged rows from {relation_name}"))?;
        touched_relations.insert((dimensions, vector_kind.to_string(), relation_name));
    }
    sqlx::query(
        "delete from knowledge_vector_relation_manifest
         where library_id = $1
           and dim = $2
           and embedding_model_key = $3
           and promoted = false",
    )
    .bind(library_id)
    .bind(checked_dim_i32(dimensions)?)
    .bind(staging_embedding_model_key)
    .execute(&mut **transaction)
    .await
    .context("delete staged vector manifest lanes")?;
    Ok(touched_relations)
}

pub(super) async fn delete_source_less_vectors_in_touched_relations(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    touched_relations: &BTreeSet<TouchedVectorRelation>,
) -> anyhow::Result<()> {
    for (dimensions, vector_kind, relation_name) in touched_relations {
        let relation_prefix = match vector_kind.as_str() {
            KNOWLEDGE_CHUNK_VECTOR_KIND => CHUNK_VECTOR_RELATION_PREFIX,
            KNOWLEDGE_ENTITY_VECTOR_KIND => ENTITY_VECTOR_RELATION_PREFIX,
            _ => anyhow::bail!("staged vector manifest has an unsupported vector kind"),
        };
        validate_relation_name(relation_name, relation_prefix)?;
        anyhow::ensure!(
            relation_name == &vector_relation_name(relation_prefix, *dimensions)?,
            "staged cleanup relation does not match its manifest dimension"
        );
        let relation = quote_identifier(relation_name)?;
        let source_identity_exists = match vector_kind.as_str() {
            KNOWLEDGE_CHUNK_VECTOR_KIND => {
                "exists (
                    select 1
                    from knowledge_chunk source_chunk
                    where source_chunk.library_id = stored_vector.library_id
                      and source_chunk.chunk_id = stored_vector.chunk_id
                      and source_chunk.revision_id = stored_vector.revision_id
                 )"
            }
            KNOWLEDGE_ENTITY_VECTOR_KIND => {
                "exists (
                    select 1
                    from knowledge_entity source_entity
                    where source_entity.library_id = stored_vector.library_id
                      and source_entity.entity_id = stored_vector.entity_id
                 )"
            }
            _ => unreachable!("vector kind was validated before relation cleanup"),
        };
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {relation} stored_vector
             where stored_vector.library_id = $1
               and stored_vector.vector_kind = $2
               and not ({source_identity_exists})"
        )))
        .bind(library_id)
        .bind(vector_kind)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("delete source-less vector rows from {relation_name}"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update knowledge_vector_relation_manifest manifest
             set row_count = (
                select count(*)::bigint
                from {relation} stored_vector
                where stored_vector.library_id = manifest.library_id
                  and stored_vector.vector_kind = manifest.vector_kind
                  and stored_vector.embedding_model_key = manifest.embedding_model_key
             )
             where manifest.library_id = $1
               and manifest.dim = $2
               and manifest.vector_kind = $3
               and manifest.relation_name = $4"
        )))
        .bind(library_id)
        .bind(checked_dim_i32(*dimensions)?)
        .bind(vector_kind)
        .bind(relation_name)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("refresh vector manifests after cleanup in {relation_name}"))?;
    }
    Ok(())
}

pub(crate) async fn list_physical_vector_relations_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    relation_prefix: &str,
) -> anyhow::Result<BTreeSet<String>> {
    let relation_names = sqlx::query_scalar::<_, String>(
        "select c.relname
         from pg_catalog.pg_class c
         join pg_catalog.pg_namespace n on n.oid = c.relnamespace
         where n.nspname = current_schema()
           and c.relkind = 'r'
           and left(c.relname, char_length($1)) = $1
         order by c.relname",
    )
    .bind(relation_prefix)
    .fetch_all(&mut **transaction)
    .await
    .context("failed to discover physical vector relations")?;
    relation_names
        .into_iter()
        .map(|relation_name| {
            validate_relation_name(&relation_name, relation_prefix)?;
            Ok(relation_name)
        })
        .collect()
}

async fn refresh_library_vector_manifest_counts_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    relation_prefix: &str,
) -> anyhow::Result<()> {
    sqlx::query(
        "update knowledge_vector_relation_manifest
         set row_count = 0
         where library_id = $1
           and relation_name like $2",
    )
    .bind(library_id)
    .bind(format!("{relation_prefix}%"))
    .execute(&mut **transaction)
    .await
    .context("reset vector manifest counts before exact reconciliation")?;
    for relation_name in
        list_physical_vector_relations_in_transaction(transaction, relation_prefix).await?
    {
        let relation = quote_identifier(&relation_name)?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "update knowledge_vector_relation_manifest manifest
             set row_count = (
                select count(*)::bigint
                from {relation} stored_vector
                where stored_vector.library_id = manifest.library_id
                  and stored_vector.vector_kind = manifest.vector_kind
                  and stored_vector.embedding_model_key = manifest.embedding_model_key
             )
             where manifest.library_id = $1
               and manifest.relation_name = $2"
        )))
        .bind(library_id)
        .bind(&relation_name)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("reconcile vector manifests after delete in {relation_name}"))?;
    }
    Ok(())
}

pub(crate) async fn decrement_vector_manifest_counts_in_transaction(
    executor: &mut PgConnection,
    library_id: Uuid,
    relation_name: &str,
    manifest_decrements: VectorManifestDecrements,
) -> anyhow::Result<()> {
    let relation = quote_identifier(relation_name)?;
    for ((embedding_model_key, vector_kind), rows_deleted) in manifest_decrements {
        let remaining_physical_rows = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "select count(*)::bigint
             from {relation}
             where library_id = $1
               and embedding_model_key = $2
               and vector_kind = $3"
        )))
        .bind(library_id)
        .bind(&embedding_model_key)
        .bind(&vector_kind)
        .fetch_one(&mut *executor)
        .await
        .with_context(|| format!("count remaining physical vectors in {relation_name}"))?;
        let manifest_row_count = sqlx::query_scalar::<_, i64>(
            "select row_count
             from knowledge_vector_relation_manifest
             where library_id = $1
               and relation_name = $2
               and embedding_model_key = $3
               and vector_kind = $4
             for update",
        )
        .bind(library_id)
        .bind(relation_name)
        .bind(&embedding_model_key)
        .bind(&vector_kind)
        .fetch_optional(&mut *executor)
        .await
        .with_context(|| {
            format!("lock vector manifest for exact reconciliation in {relation_name}")
        })?;
        let Some(manifest_row_count) = manifest_row_count else {
            continue;
        };
        if !embedding_model_key.starts_with(VECTOR_REBUILD_STAGING_PROFILE_PREFIX) {
            let expected_pre_delete_count = remaining_physical_rows
                .checked_add(rows_deleted)
                .context("physical vector lane count overflowed i64")?;
            anyhow::ensure!(
                manifest_row_count == expected_pre_delete_count,
                "active vector manifest was not exact before deleting from {relation_name} profile={} kind={}: manifest={}, physical={}",
                embedding_model_key,
                vector_kind,
                manifest_row_count,
                expected_pre_delete_count,
            );
        }
        sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = $1
             where library_id = $2
               and relation_name = $3
               and embedding_model_key = $4
               and vector_kind = $5",
        )
        .bind(remaining_physical_rows)
        .bind(library_id)
        .bind(relation_name)
        .bind(&embedding_model_key)
        .bind(&vector_kind)
        .execute(&mut *executor)
        .await
        .with_context(|| format!("set exact vector manifest count for {relation_name}"))?;
    }
    Ok(())
}

fn collect_vector_manifest_decrements(
    deleted_lanes: Vec<DeletedVectorLane>,
    operation: &str,
) -> anyhow::Result<(u64, VectorManifestDecrements)> {
    deleted_lanes.into_iter().try_fold(
        (0_u64, HashMap::new()),
        |(deleted, mut manifest_decrements), (embedding_model_key, vector_kind, lane_count)| {
            anyhow::ensure!(lane_count > 0, "{operation} returned an empty vector lane");
            let lane_count = u64::try_from(lane_count)
                .with_context(|| format!("{operation} count overflowed u64"))?;
            let deleted = deleted
                .checked_add(lane_count)
                .with_context(|| format!("{operation} count overflowed u64"))?;
            manifest_decrements.insert(
                (embedding_model_key, vector_kind),
                i64::try_from(lane_count).context("validated vector lane count overflowed i64")?,
            );
            Ok((deleted, manifest_decrements))
        },
    )
}

async fn delete_projection_chunk_vectors_from_relation(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    relation_name: &str,
    chunk_ids: &[Uuid],
) -> anyhow::Result<u64> {
    let relation = quote_identifier(relation_name)?;
    let deleted_lanes = sqlx::query_as::<_, (String, String, i64)>(sqlx::AssertSqlSafe(format!(
        "with deleted as (
                delete from {relation}
                where library_id = $1
                  and chunk_id = any($2::uuid[])
                returning embedding_model_key, vector_kind
             )
             select embedding_model_key, vector_kind, count(*)::bigint
             from deleted
             group by embedding_model_key, vector_kind"
    )))
    .bind(library_id)
    .bind(chunk_ids)
    .fetch_all(&mut **transaction)
    .await
    .with_context(|| format!("delete projection chunk vectors from {relation_name}"))?;
    let (deleted, manifest_decrements) =
        collect_vector_manifest_decrements(deleted_lanes, "projection chunk vector delete")?;
    decrement_vector_manifest_counts_in_transaction(
        transaction,
        library_id,
        relation_name,
        manifest_decrements,
    )
    .await?;
    Ok(deleted)
}

/// Removes chunk vectors owned by a knowledge projection and decrements the
/// exact manifest lanes in the caller-owned lifecycle transaction.
///
/// The caller must already hold the library lifecycle lock. Physical relations
/// are the deletion authority: relying on the manifest to discover them would
/// preserve precisely the orphan rows this operation is responsible for.
pub(crate) async fn delete_projection_chunk_vectors(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    chunk_ids: &[Uuid],
) -> anyhow::Result<u64> {
    if chunk_ids.is_empty() {
        return Ok(0);
    }

    let mut deleted = 0_u64;
    for relation_name in
        list_physical_vector_relations_in_transaction(transaction, CHUNK_VECTOR_RELATION_PREFIX)
            .await?
    {
        deleted = deleted
            .checked_add(
                delete_projection_chunk_vectors_from_relation(
                    transaction,
                    library_id,
                    &relation_name,
                    chunk_ids,
                )
                .await?,
            )
            .context("projection chunk vector delete count overflowed u64")?;
    }
    Ok(deleted)
}

pub(super) async fn lock_vector_source_library(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> anyhow::Result<(Uuid, i64)> {
    sqlx::query_as::<_, (Uuid, i64)>(
        "select workspace_id, source_truth_version
         from catalog_library
         where id = $1
         for share",
    )
    .bind(library_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock vector source library")?
    .ok_or_else(|| anyhow::anyhow!("vector source library disappeared"))
}

async fn lock_deferred_vector_source_library(
    transaction: &mut Transaction<'_, Postgres>,
    workspace_id: Uuid,
    library_id: Uuid,
) -> anyhow::Result<i64> {
    let (observed_workspace_id, source_truth_version) =
        lock_vector_source_library(transaction, library_id).await?;
    anyhow::ensure!(
        observed_workspace_id == workspace_id,
        "deferred vector source library moved to another workspace"
    );
    Ok(source_truth_version)
}

/// Serializes a deferred rebuild batch with lifecycle purge and proves that
/// every source chunk still exists after the catalog-library lock is acquired.
pub(super) async fn lock_and_validate_deferred_chunk_vector_sources(
    transaction: &mut Transaction<'_, Postgres>,
    rows: &[KnowledgeChunkVectorRow],
) -> anyhow::Result<i64> {
    let first = rows.first().context("deferred chunk vector batch must not be empty")?;
    anyhow::ensure!(
        rows.iter().all(|row| {
            row.library_id == first.library_id && row.workspace_id == first.workspace_id
        }),
        "deferred chunk vector batch spans multiple library scopes"
    );
    let source_truth_version =
        lock_deferred_vector_source_library(transaction, first.workspace_id, first.library_id)
            .await?;

    let source_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from unnest($1::uuid[], $2::uuid[]) input(chunk_id, revision_id)
         where exists (
             select 1
             from knowledge_chunk source_chunk
             where source_chunk.workspace_id = $3
               and source_chunk.library_id = $4
               and source_chunk.chunk_id = input.chunk_id
               and source_chunk.revision_id = input.revision_id
         )",
    )
    .bind(rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>())
    .bind(rows.iter().map(|row| row.revision_id).collect::<Vec<_>>())
    .bind(first.workspace_id)
    .bind(first.library_id)
    .fetch_one(&mut **transaction)
    .await
    .context("validate deferred chunk vector source identities")?;
    anyhow::ensure!(
        usize::try_from(source_count).ok() == Some(rows.len()),
        "deferred chunk vector source changed before batch write"
    );
    Ok(source_truth_version)
}

/// Entity rebuilds use the same lifecycle boundary as chunk rebuilds so graph
/// reset or library purge cannot be followed by a stale deferred write.
pub(super) async fn lock_and_validate_deferred_entity_vector_sources(
    transaction: &mut Transaction<'_, Postgres>,
    rows: &[KnowledgeEntityVectorRow],
) -> anyhow::Result<i64> {
    let first = rows.first().context("deferred entity vector batch must not be empty")?;
    anyhow::ensure!(
        rows.iter().all(|row| {
            row.library_id == first.library_id && row.workspace_id == first.workspace_id
        }),
        "deferred entity vector batch spans multiple library scopes"
    );
    let source_truth_version =
        lock_deferred_vector_source_library(transaction, first.workspace_id, first.library_id)
            .await?;

    let source_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from unnest($1::uuid[]) input(entity_id)
         where exists (
             select 1
             from knowledge_entity source_entity
             where source_entity.workspace_id = $2
               and source_entity.library_id = $3
               and source_entity.entity_id = input.entity_id
         )",
    )
    .bind(rows.iter().map(|row| row.entity_id).collect::<Vec<_>>())
    .bind(first.workspace_id)
    .bind(first.library_id)
    .fetch_one(&mut **transaction)
    .await
    .context("validate deferred entity vector source identities")?;
    anyhow::ensure!(
        usize::try_from(source_count).ok() == Some(rows.len()),
        "deferred entity vector source changed before batch write"
    );
    Ok(source_truth_version)
}

#[derive(Clone, Copy)]
pub(super) enum FencedChunkVectorDelete<'a> {
    ExactVectorIds(&'a [Uuid]),
    Revision(Uuid),
}

async fn delete_chunk_vectors_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    selector: FencedChunkVectorDelete<'_>,
) -> anyhow::Result<u64> {
    let mut deleted = 0_u64;
    for relation_name in
        list_physical_vector_relations_in_transaction(transaction, CHUNK_VECTOR_RELATION_PREFIX)
            .await?
    {
        let relation = quote_identifier(&relation_name)?;
        let deleted_lanes = match selector {
            FencedChunkVectorDelete::ExactVectorIds(vector_ids) => {
                sqlx::query_as::<_, (String, String, i64)>(sqlx::AssertSqlSafe(format!(
                    "with deleted as (
                        delete from {relation}
                        where library_id = $1
                          and vector_id = any($2::uuid[])
                        returning embedding_model_key, vector_kind
                     )
                     select embedding_model_key, vector_kind, count(*)::bigint
                     from deleted
                     group by embedding_model_key, vector_kind"
                )))
                .bind(library_id)
                .bind(vector_ids)
                .fetch_all(&mut **transaction)
                .await
            }
            FencedChunkVectorDelete::Revision(revision_id) => {
                sqlx::query_as::<_, (String, String, i64)>(sqlx::AssertSqlSafe(format!(
                    "with deleted as (
                        delete from {relation}
                        where library_id = $1
                          and revision_id = $2
                        returning embedding_model_key, vector_kind
                     )
                     select embedding_model_key, vector_kind, count(*)::bigint
                     from deleted
                     group by embedding_model_key, vector_kind"
                )))
                .bind(library_id)
                .bind(revision_id)
                .fetch_all(&mut **transaction)
                .await
            }
        }
        .with_context(|| format!("delete fenced chunk vectors from {relation_name}"))?;
        let (relation_deleted, manifest_decrements) =
            collect_vector_manifest_decrements(deleted_lanes, "fenced chunk vector delete")?;
        deleted = deleted
            .checked_add(relation_deleted)
            .context("fenced chunk vector delete count overflowed u64")?;
        decrement_vector_manifest_counts_in_transaction(
            transaction,
            library_id,
            &relation_name,
            manifest_decrements,
        )
        .await?;
    }
    Ok(deleted)
}

/// Removes every chunk vector owned by one revision and reconciles its
/// manifests inside a caller-owned lifecycle transaction. The caller must
/// already hold the exclusive library vector/source lock.
pub(crate) async fn delete_ingest_revision_chunk_vectors(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    revision_id: Uuid,
) -> anyhow::Result<u64> {
    let deleted = delete_chunk_vectors_in_transaction(
        transaction,
        library_id,
        FencedChunkVectorDelete::Revision(revision_id),
    )
    .await?;
    Ok(deleted)
}

pub(super) const SUPERSEDED_VECTOR_DELETE_BATCH_SIZE: i64 = 10_000;

/// SQL predicate over the `knowledge_document` alias `document` for a document
/// the system treats as deleted.
///
/// The two tombstone markers are one deletion predicate, not two:
/// `list_documents_by_*` (`pg_document_store`), retrieval, admission and the
/// write-path reconciliation all hide a document when
/// `document_state = 'deleted'` **or** `deleted_at is not null`, and
/// `current_document_collections_exclude_both_half_deleted_shapes` pins both
/// halves as deleted. The sweep reads the same predicate so a document the
/// readers consider deleted is never live for one guard and dead for another.
const DOCUMENT_DELETED_SQL: &str =
    "(document.document_state = 'deleted' or document.deleted_at is not null)";

const AUTHORITATIVE_VECTOR_CANDIDATES_SQL: &str = "select job.library_id, job.workspace_id,
            job.knowledge_document_id as document_id,
            job.knowledge_revision_id as revision_id
     from ingest_job job
     join content_mutation_item mutation_item
       on mutation_item.id = job.mutation_item_id
      and mutation_item.mutation_id = job.mutation_id
      and mutation_item.document_id = job.knowledge_document_id
      and mutation_item.result_revision_id = job.knowledge_revision_id
      and mutation_item.item_state = 'pending'
     join content_mutation mutation
       on mutation.id = mutation_item.mutation_id
      and mutation.workspace_id = job.workspace_id
      and mutation.library_id = job.library_id
      and mutation.mutation_state in ('accepted', 'running')
     join content_document candidate_document
       on candidate_document.id = mutation_item.document_id
      and candidate_document.workspace_id = job.workspace_id
      and candidate_document.library_id = job.library_id
      and candidate_document.document_state = 'active'
      and candidate_document.deleted_at is null
     join lateral (
       select attempt.attempt_state, attempt.finished_at
       from ingest_attempt attempt
       where attempt.job_id = job.id
       order by attempt.attempt_number desc
       limit 1
     ) latest_attempt on true
     where job.job_kind = 'content_mutation'
       and job.queue_state = 'leased'
       and job.completed_at is null
       and latest_attempt.attempt_state = 'leased'
       and latest_attempt.finished_at is null";

/// Call under the exclusive data-plane serializer. The library row also blocks
/// admission/lease transitions, so the candidate inventory stays valid to commit.
pub(super) async fn ensure_no_authoritative_vector_candidates(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> anyhow::Result<()> {
    let library =
        sqlx::query_scalar::<_, Uuid>("select id from catalog_library where id=$1 for update")
            .bind(library_id)
            .fetch_optional(&mut **transaction)
            .await?;
    anyhow::ensure!(library.is_some(), "library disappeared before vector candidate check");
    let quiescent: bool = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "select not exists (select 1 from ({AUTHORITATIVE_VECTOR_CANDIDATES_SQL}) candidate where candidate.library_id=$1)",
    ))).bind(library_id).fetch_one(&mut **transaction).await?;
    anyhow::ensure!(quiescent, "authoritative ingest candidate blocks destructive vector rebuild");
    Ok(())
}

/// Rows the superseded-vector sweep may delete.
///
/// A soft-deleted document keeps its last `readable_revision_id` (the delete
/// writes the tombstone without clearing the readable head), so its live
/// revision is never "distinct from the readable revision" and the head-based
/// branches below keep its vectors forever. [`DOCUMENT_DELETED_SQL`] is
/// therefore an unconditional eligibility branch: retrieval already filters
/// deleted documents out, and the inline delete cleanup may have failed and
/// left the vectors behind. Keeping them waiting for the head branches would
/// leak capacity with no sweeper that can reclaim it.
fn superseded_vector_eligibility_sql() -> String {
    const NULL_HEAD_REPAIR_ELIGIBILITY: &str = crate::infra::repositories::content_document_head_repository::null_head_repair_eligibility!();
    format!(
        "(
    {DOCUMENT_DELETED_SQL}
    or (not exists (
          select 1 from ({AUTHORITATIVE_VECTOR_CANDIDATES_SQL}) candidate
          where candidate.library_id = page.library_id
            and candidate.workspace_id = document.workspace_id
            and candidate.document_id = document.document_id
            and candidate.revision_id = page.revision_id
        )
      and (
        ((document.readable_revision_id is not null
            or document.active_revision_id is not null)
          and page.revision_id is distinct from document.readable_revision_id)
        or ($2 and document.readable_revision_id is null
           and document.active_revision_id is null
           and not exists (
             select 1
              from content_document_head h
              where h.document_id = document.document_id
                and not ({NULL_HEAD_REPAIR_ELIGIBILITY})
            ))
      ))
)"
    )
}

/// SQL predicate for the chunk-vector rows retrieval must keep: the
/// complement of [`superseded_vector_eligibility_sql`] under the sweep's
/// default head policy.
///
/// Snapshot export filters its per-dim chunk-vector sections with exactly
/// this predicate, so a restored archive cannot re-seed the ANN index with
/// vectors the sweep already removed (IRG-67). The surrounding query must
/// alias the vector row as `page` and the owning document as `document`,
/// and bind `$2 = false` — the default policy, which
/// keeps a headless document's vectors rather than deleting them. A vector
/// whose owning document is gone altogether stays selectable here for the
/// same reason the sweep leaves it alone: nothing can prove it removable.
/// A soft-deleted document is the exception: its vectors are not selectable,
/// so an archive cannot restore retrieval state for a deleted document.
/// Lease protection follows the vector row's library, independently of any
/// caller-supplied library filter, so a global inventory has the same protection
/// as a library-scoped sweep.
pub(crate) fn selectable_chunk_vector_rows_sql() -> String {
    format!("(document.document_id is null or not {})", superseded_vector_eligibility_sql())
}

#[derive(Debug, Default)]
pub(crate) struct SupersededChunkVectorSweepReport {
    pub documents_visited: i64,
    pub documents_with_superseded_vectors: i64,
    pub superseded_vectors: i64,
    pub null_head_documents: i64,
    pub null_head_vectors: i64,
    pub work_remaining: bool,
    pub next_cursor_json: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct SupersededVectorSweepCursor {
    pub(super) relation_name: Option<String>,
    pub(super) after_chunk_id: Option<Uuid>,
    pub(super) after_revision_id: Option<Uuid>,
    pub(super) after_embedding_model_key: Option<String>,
    pub(super) after_vector_kind: Option<String>,
    pub(super) after_freshness_generation: Option<i64>,
    pub(super) source_truth_version: Option<i64>,
}

impl SupersededVectorSweepCursor {
    fn is_complete_key(&self) -> bool {
        self.after_chunk_id.is_some()
            && self.after_revision_id.is_some()
            && self.after_embedding_model_key.is_some()
            && self.after_vector_kind.is_some()
            && self.after_freshness_generation.is_some()
    }

    pub(super) fn has_valid_key(&self) -> bool {
        let populated = [
            self.after_chunk_id.is_some(),
            self.after_revision_id.is_some(),
            self.after_embedding_model_key.is_some(),
            self.after_vector_kind.is_some(),
            self.after_freshness_generation.is_some(),
        ];
        self.is_complete_key() || populated.iter().all(|value| !*value)
    }
}

#[derive(Debug, FromRow)]
struct SupersededVectorPageRow {
    document_id: Option<Uuid>,
    null_head: bool,
    projection_coherent: bool,
    chunk_id: Uuid,
    revision_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    freshness_generation: i64,
    deleted: bool,
}

struct SupersededVectorSweepState {
    report: SupersededChunkVectorSweepReport,
    visited_documents: HashSet<Uuid>,
    visited_null_head_documents: HashSet<Uuid>,
    touched_documents: HashSet<Uuid>,
    batches_processed: u32,
    expected_source_truth_version: i64,
    exact_reporting: bool,
}

impl SupersededVectorSweepState {
    fn finish_counts(&mut self) -> anyhow::Result<()> {
        self.report.documents_with_superseded_vectors = i64::try_from(self.touched_documents.len())
            .context("superseded vector document count overflowed i64")?;
        if !self.exact_reporting {
            self.report.documents_visited = i64::try_from(self.visited_documents.len())
                .context("visited vector document count overflowed i64")?;
            self.report.null_head_documents = i64::try_from(self.visited_null_head_documents.len())
                .context("visited null-head document count overflowed i64")?;
        }
        Ok(())
    }

    fn record_preview(&mut self, rows: Vec<(Uuid, bool, i64)>) {
        for (document_id, null_head, row_count) in rows {
            self.touched_documents.insert(document_id);
            if null_head {
                self.report.null_head_vectors += row_count;
            } else {
                self.report.superseded_vectors += row_count;
            }
        }
    }

    fn record_page(&mut self, page_rows: &[SupersededVectorPageRow]) -> anyhow::Result<()> {
        for row in page_rows {
            if let Some(document_id) = row.document_id {
                self.visited_documents.insert(document_id);
                if row.null_head {
                    self.visited_null_head_documents.insert(document_id);
                }
            }
            if !row.deleted {
                continue;
            }
            self.touched_documents
                .insert(row.document_id.context("deleted vector has no source document")?);
            if row.null_head {
                self.report.null_head_vectors += 1;
            } else {
                self.report.superseded_vectors += 1;
            }
        }
        Ok(())
    }
}

enum SupersededVectorBatchResult {
    SourceTruthChanged(i64),
    Committed { page_rows: Vec<SupersededVectorPageRow>, source_truth_version: i64 },
}

async fn initialize_superseded_vector_sweep(
    pool: &PgPool,
    library_id: Uuid,
    exact_reporting: bool,
    cursor_json: Option<&serde_json::Value>,
) -> anyhow::Result<(SupersededVectorSweepState, Vec<String>, SupersededVectorSweepCursor)> {
    let mut report = SupersededChunkVectorSweepReport::default();
    if exact_reporting {
        let (documents_visited, null_head_documents) = sqlx::query_as::<_, (i64, i64)>(
            "select count(*)::bigint,
                    count(*) filter (
                        where readable_revision_id is null
                          and active_revision_id is null
                    )::bigint
             from knowledge_document
             where library_id = $1",
        )
        .bind(library_id)
        .fetch_one(pool)
        .await
        .context("count documents before superseded vector sweep")?;
        report.documents_visited = documents_visited;
        report.null_head_documents = null_head_documents;
    }

    let mut transaction = pool.begin().await.context("begin vector relation discovery")?;
    let source_truth_version =
        lock_exclusive_vector_mutation_source(&mut transaction, library_id).await?;
    let relation_names = list_physical_vector_relations_in_transaction(
        &mut transaction,
        CHUNK_VECTOR_RELATION_PREFIX,
    )
    .await?
    .into_iter()
    .collect::<Vec<_>>();
    transaction.commit().await.context("commit vector relation discovery")?;

    let mut cursor = cursor_json
        .filter(|value| !value.as_object().is_some_and(serde_json::Map::is_empty))
        .map(|value| serde_json::from_value::<SupersededVectorSweepCursor>(value.clone()))
        .transpose()
        .context("decode superseded-vector sweep cursor")?
        .unwrap_or_default();
    if cursor.source_truth_version != Some(source_truth_version)
        || cursor.relation_name.as_ref().is_some_and(|name| !relation_names.contains(name))
        || (cursor.relation_name.is_some() && !cursor.has_valid_key())
    {
        cursor = SupersededVectorSweepCursor::default();
    }

    Ok((
        SupersededVectorSweepState {
            report,
            visited_documents: HashSet::new(),
            visited_null_head_documents: HashSet::new(),
            touched_documents: HashSet::new(),
            batches_processed: 0,
            expected_source_truth_version: source_truth_version,
            exact_reporting,
        },
        relation_names,
        cursor,
    ))
}

async fn preview_superseded_vectors(
    pool: &PgPool,
    library_id: Uuid,
    relation_name: &str,
    relation: &str,
    include_null_head: bool,
) -> anyhow::Result<Vec<(Uuid, bool, i64)>> {
    let mut transaction = pool.begin().await.context("begin superseded vector preview")?;
    lock_exclusive_vector_mutation_source(&mut transaction, library_id).await?;
    ensure_library_head_projection_coherent(&mut transaction, library_id).await?;
    let eligibility_predicate =
        superseded_vector_eligibility_sql().replace("page.", "stored_vector.");
    let rows = sqlx::query_as::<_, (Uuid, bool, i64)>(sqlx::AssertSqlSafe(format!(
        "select chunk.document_id,
                document.readable_revision_id is null
                    and document.active_revision_id is null as null_head,
                count(*)::bigint
         from {relation} stored_vector
         join knowledge_chunk chunk
           on chunk.library_id = stored_vector.library_id
          and chunk.chunk_id = stored_vector.chunk_id
          and chunk.revision_id = stored_vector.revision_id
         join knowledge_document document
           on document.library_id = chunk.library_id
          and document.document_id = chunk.document_id
         where stored_vector.library_id = $1
           and {eligibility_predicate}
         group by chunk.document_id, null_head"
    )))
    .bind(library_id)
    .bind(include_null_head)
    .fetch_all(&mut *transaction)
    .await
    .with_context(|| format!("preview superseded vectors in {relation_name}"))?;
    transaction.commit().await.context("commit superseded vector preview")?;
    Ok(rows)
}

fn superseded_vector_manifest_decrements(
    page_rows: &[SupersededVectorPageRow],
) -> HashMap<(String, String), i64> {
    let mut decrements = HashMap::new();
    for row in page_rows.iter().filter(|row| row.deleted) {
        *decrements
            .entry((row.embedding_model_key.clone(), row.vector_kind.clone()))
            .or_default() += 1;
    }
    decrements
}

/// Per-sweep invariants threaded through relation iteration: the pool, the
/// library being swept, the head policy, and the optional per-run batch budget.
/// Grouping them keeps the per-relation batch loop and the relation executor
/// under the argument-count threshold and stops the four values from being
/// transposed at each call site.
struct SupersededVectorSweepArgs<'a> {
    pool: &'a PgPool,
    library_id: Uuid,
    include_null_head: bool,
    max_batches: Option<u32>,
}

async fn execute_superseded_vector_batch(
    args: &SupersededVectorSweepArgs<'_>,
    relation_name: &str,
    relation: &str,
    after: Option<&SupersededVectorSweepCursor>,
    expected_source_truth_version: i64,
) -> anyhow::Result<SupersededVectorBatchResult> {
    let pool = args.pool;
    let library_id = args.library_id;
    let include_null_head = args.include_null_head;
    let mut transaction = pool.begin().await.context("begin superseded vector batch")?;
    let source_truth_version =
        lock_exclusive_vector_mutation_source(&mut transaction, library_id).await?;
    if source_truth_version != expected_source_truth_version {
        transaction.commit().await.context("commit superseded-vector cursor reset")?;
        return Ok(SupersededVectorBatchResult::SourceTruthChanged(source_truth_version));
    }

    let has_keyset = after.is_some_and(SupersededVectorSweepCursor::is_complete_key);
    let keyset_predicate = if has_keyset {
        "and (
            stored_vector.chunk_id,
            stored_vector.revision_id,
            stored_vector.embedding_model_key,
            stored_vector.vector_kind,
            stored_vector.freshness_generation
         ) > ($4, $5, $6, $7, $8)"
    } else {
        ""
    };
    let eligibility_predicate = superseded_vector_eligibility_sql();
    let delete_query =
        sqlx::query_as::<_, SupersededVectorPageRow>(sqlx::AssertSqlSafe(format!(
            "with page as materialized (
                 select stored_vector.key,
                        stored_vector.library_id,
                        stored_vector.chunk_id,
                        stored_vector.revision_id,
                        stored_vector.embedding_model_key,
                        stored_vector.vector_kind,
                        stored_vector.freshness_generation
                 from {relation} stored_vector
                 where stored_vector.library_id = $1
                   {keyset_predicate}
                 order by stored_vector.chunk_id,
                          stored_vector.revision_id,
                          stored_vector.embedding_model_key,
                          stored_vector.vector_kind,
                          stored_vector.freshness_generation
                 limit $3
                 for update of stored_vector
             ), classified as materialized (
                 select page.key,
                        chunk.document_id,
                        document.document_id is not null
                            and document.readable_revision_id is null
                            and document.active_revision_id is null as null_head,
                        document.document_id is null
                            or {DOCUMENT_DELETED_SQL}
                            or (source_head.document_id is not null
                                and document.readable_revision_id is not distinct from source_head.readable_revision_id
                                and document.active_revision_id is not distinct from source_head.active_revision_id)
                            as projection_coherent,
                        page.chunk_id,
                        page.revision_id,
                        page.embedding_model_key,
                        page.vector_kind,
                        page.freshness_generation,
                        document.document_id is not null
                        and {eligibility_predicate} as eligible
                 from page
                 left join knowledge_chunk chunk
                   on chunk.library_id = $1
                  and chunk.chunk_id = page.chunk_id
                  and chunk.revision_id = page.revision_id
                 left join knowledge_document document
                   on document.library_id = $1
                  and document.document_id = chunk.document_id
                 left join content_document_head source_head
                   on source_head.document_id = document.document_id
             ), deleted as (
                 delete from {relation} stored_vector
                 using classified candidate
                 where candidate.eligible
                   and stored_vector.key = candidate.key
                 returning stored_vector.key
             )
             select candidate.document_id,
                    candidate.null_head,
                    candidate.projection_coherent,
                    candidate.chunk_id,
                    candidate.revision_id,
                    candidate.embedding_model_key,
                    candidate.vector_kind,
                    candidate.freshness_generation,
                    deleted.key is not null as deleted
             from classified candidate
             left join deleted using (key)
             order by candidate.chunk_id,
                      candidate.revision_id,
                      candidate.embedding_model_key,
                      candidate.vector_kind,
                      candidate.freshness_generation"
        )))
        .bind(library_id)
        .bind(include_null_head)
        .bind(SUPERSEDED_VECTOR_DELETE_BATCH_SIZE);
    let delete_query = if let Some(after) = after.filter(|_| has_keyset) {
        delete_query
            .bind(after.after_chunk_id.context("cursor chunk id missing")?)
            .bind(after.after_revision_id.context("cursor revision id missing")?)
            .bind(
                after
                    .after_embedding_model_key
                    .as_deref()
                    .context("cursor embedding profile missing")?,
            )
            .bind(after.after_vector_kind.as_deref().context("cursor kind missing")?)
            .bind(after.after_freshness_generation.context("cursor freshness generation missing")?)
    } else {
        delete_query
    };
    let page_rows = delete_query
        .fetch_all(&mut *transaction)
        .await
        .with_context(|| format!("scan and delete vector page from {relation_name}"))?;
    anyhow::ensure!(
        page_rows.iter().all(|row| row.projection_coherent),
        "source and knowledge document heads diverged in superseded vector page"
    );
    let batch_deleted = i64::try_from(page_rows.iter().filter(|row| row.deleted).count())
        .context("superseded vector batch size overflowed i64")?;
    decrement_vector_manifest_counts_in_transaction(
        &mut transaction,
        library_id,
        relation_name,
        superseded_vector_manifest_decrements(&page_rows),
    )
    .await?;
    let committed_source_truth_version = if batch_deleted > 0 {
        advance_vector_source_truth_version(&mut transaction, library_id, source_truth_version)
            .await?
    } else {
        source_truth_version
    };
    transaction.commit().await.context("commit superseded vector batch")?;
    Ok(SupersededVectorBatchResult::Committed {
        page_rows,
        source_truth_version: committed_source_truth_version,
    })
}

fn cursor_after_superseded_vector_page(
    relation_name: &str,
    page_rows: &[SupersededVectorPageRow],
    source_truth_version: i64,
) -> Option<SupersededVectorSweepCursor> {
    page_rows.last().map(|last_row| SupersededVectorSweepCursor {
        relation_name: Some(relation_name.to_owned()),
        after_chunk_id: Some(last_row.chunk_id),
        after_revision_id: Some(last_row.revision_id),
        after_embedding_model_key: Some(last_row.embedding_model_key.clone()),
        after_vector_kind: Some(last_row.vector_kind.clone()),
        after_freshness_generation: Some(last_row.freshness_generation),
        source_truth_version: Some(source_truth_version),
    })
}

#[derive(Debug, PartialEq)]
pub(super) enum SupersededVectorSweepProgress {
    ContinueRelation,
    RelationComplete,
    BatchBudgetExhausted { work_remaining: bool, next_cursor: Option<SupersededVectorSweepCursor> },
}

/// Where the sweep currently stands within the relation list: the ordered
/// relation names plus the index of the relation being processed. Named so the
/// progress decision and the batch loop share one position value instead of
/// two loose parameters.
pub(super) struct SupersededRelationPosition<'a> {
    pub(super) names: &'a [String],
    pub(super) index: usize,
}

pub(super) fn superseded_vector_sweep_progress(
    max_batches: Option<u32>,
    batches_processed: u32,
    page_scanned: i64,
    position: &SupersededRelationPosition<'_>,
    after: Option<&SupersededVectorSweepCursor>,
    source_truth_version: i64,
) -> anyhow::Result<SupersededVectorSweepProgress> {
    if max_batches.is_some_and(|limit| batches_processed >= limit) {
        let work_remaining = page_scanned == SUPERSEDED_VECTOR_DELETE_BATCH_SIZE
            || position.index + 1 < position.names.len();
        let next_cursor = if !work_remaining {
            None
        } else if page_scanned < SUPERSEDED_VECTOR_DELETE_BATCH_SIZE {
            Some(SupersededVectorSweepCursor {
                relation_name: position.names.get(position.index + 1).cloned(),
                source_truth_version: Some(source_truth_version),
                ..SupersededVectorSweepCursor::default()
            })
        } else {
            Some(after.cloned().context("full batch did not produce a vector cursor")?)
        };
        return Ok(SupersededVectorSweepProgress::BatchBudgetExhausted {
            work_remaining,
            next_cursor,
        });
    }
    if page_scanned < SUPERSEDED_VECTOR_DELETE_BATCH_SIZE {
        return Ok(SupersededVectorSweepProgress::RelationComplete);
    }
    Ok(SupersededVectorSweepProgress::ContinueRelation)
}

async fn sweep_superseded_relation_batches(
    args: &SupersededVectorSweepArgs<'_>,
    position: &SupersededRelationPosition<'_>,
    relation_name: &str,
    relation: &str,
    after: &mut Option<SupersededVectorSweepCursor>,
    state: &mut SupersededVectorSweepState,
) -> anyhow::Result<bool> {
    loop {
        let batch = execute_superseded_vector_batch(
            args,
            relation_name,
            relation,
            after.as_ref(),
            state.expected_source_truth_version,
        )
        .await?;
        let (page_rows, source_truth_version) = match batch {
            SupersededVectorBatchResult::SourceTruthChanged(source_truth_version) => {
                state.report.work_remaining = true;
                state.report.next_cursor_json = Some(
                    serde_json::to_value(SupersededVectorSweepCursor {
                        source_truth_version: Some(source_truth_version),
                        ..SupersededVectorSweepCursor::default()
                    })
                    .context("encode reset superseded-vector sweep cursor")?,
                );
                state.finish_counts()?;
                return Ok(true);
            }
            SupersededVectorBatchResult::Committed { page_rows, source_truth_version } => {
                (page_rows, source_truth_version)
            }
        };

        let page_scanned =
            i64::try_from(page_rows.len()).context("superseded vector page size overflowed i64")?;
        state.record_page(&page_rows)?;
        state.expected_source_truth_version = source_truth_version;
        *after =
            cursor_after_superseded_vector_page(relation_name, &page_rows, source_truth_version);
        if page_scanned > 0 {
            state.batches_processed = state.batches_processed.saturating_add(1);
        }
        match superseded_vector_sweep_progress(
            args.max_batches,
            state.batches_processed,
            page_scanned,
            position,
            after.as_ref(),
            source_truth_version,
        )? {
            SupersededVectorSweepProgress::ContinueRelation => {}
            SupersededVectorSweepProgress::RelationComplete => return Ok(false),
            SupersededVectorSweepProgress::BatchBudgetExhausted { work_remaining, next_cursor } => {
                state.report.work_remaining = work_remaining;
                state.report.next_cursor_json = next_cursor
                    .map(serde_json::to_value)
                    .transpose()
                    .context("encode superseded-vector sweep cursor")?;
                state.finish_counts()?;
                return Ok(true);
            }
        }
    }
}

/// Removes vectors that cannot be selected by retrieval while preserving the
/// knowledge chunks and authoritative source history that produced them.
///
/// Every batch takes the same exclusive per-library vector/source lock as the
/// other destructive vector mutations. The content and knowledge head mirrors
/// must agree before a relation is touched; projection drift fails closed.
pub(crate) async fn sweep_superseded_chunk_vectors(
    pool: &PgPool,
    library_id: Uuid,
    dry_run: bool,
    include_null_head: bool,
    max_batches: Option<u32>,
    cursor_json: Option<&serde_json::Value>,
) -> anyhow::Result<SupersededChunkVectorSweepReport> {
    let exact_reporting = max_batches.is_none();
    let (mut state, relation_names, cursor) =
        initialize_superseded_vector_sweep(pool, library_id, exact_reporting, cursor_json).await?;
    let args = SupersededVectorSweepArgs { pool, library_id, include_null_head, max_batches };
    let start_relation_index = cursor
        .relation_name
        .as_ref()
        .and_then(|name| relation_names.iter().position(|candidate| candidate == name))
        .unwrap_or(0);

    for (relation_index, relation_name) in
        relation_names.iter().cloned().enumerate().skip(start_relation_index)
    {
        let relation = quote_identifier(&relation_name)?;
        let mut after = if cursor.relation_name.as_deref() == Some(relation_name.as_str()) {
            Some(cursor.clone())
        } else {
            None
        };
        if dry_run {
            let rows = preview_superseded_vectors(
                pool,
                library_id,
                &relation_name,
                &relation,
                include_null_head,
            )
            .await?;
            state.record_preview(rows);
            continue;
        }
        let position = SupersededRelationPosition { names: &relation_names, index: relation_index };
        if sweep_superseded_relation_batches(
            &args,
            &position,
            &relation_name,
            &relation,
            &mut after,
            &mut state,
        )
        .await?
        {
            return Ok(state.report);
        }
    }

    state.finish_counts()?;
    Ok(state.report)
}

/// Refuse a sweep while any live document's knowledge head diverges from its
/// content head: the head-based eligibility decisions would then be untrustworthy.
///
/// A deleted document is exempt, using the same [`DOCUMENT_DELETED_SQL`]
/// predicate as the page classification and the eligibility branch. Retrieval
/// already filters it out, its vectors are eligible through the unconditional
/// deleted branch, and the delete path may legitimately have tombstoned the
/// knowledge row before the content head was rewritten — the very divergence
/// this guard would otherwise turn into a library-wide stall.
async fn ensure_library_head_projection_coherent(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
) -> anyhow::Result<()> {
    let projection_drift = sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!(
        "select exists (
             select 1
             from knowledge_document document
             left join content_document_head source_head
               on source_head.document_id = document.document_id
             where document.library_id = $1
               and not {DOCUMENT_DELETED_SQL}
               and (source_head.document_id is null
                    or document.readable_revision_id is distinct from source_head.readable_revision_id
                    or document.active_revision_id is distinct from source_head.active_revision_id)
             limit 1
         )"
    )))
    .bind(library_id)
    .fetch_one(&mut **transaction)
    .await
    .context("validate source/knowledge heads before superseded vector sweep")?;
    anyhow::ensure!(
        !projection_drift,
        "source and knowledge document heads diverged before superseded vector sweep"
    );
    Ok(())
}

pub(super) async fn delete_chunk_vectors_fenced(
    pool: &PgPool,
    library_id: Uuid,
    selector: FencedChunkVectorDelete<'_>,
    expected_source_truth_version: i64,
    ingest_attempt: Option<IngestVectorWriteFence>,
) -> anyhow::Result<Option<VectorPlaneDeleteOutcome>> {
    if matches!(selector, FencedChunkVectorDelete::ExactVectorIds(ids) if ids.is_empty()) {
        return Ok(Some(VectorPlaneDeleteOutcome {
            deleted: 0,
            source_truth_version: expected_source_truth_version,
        }));
    }
    let advance_on_noop = matches!(selector, FencedChunkVectorDelete::Revision(_));
    let mut transaction = pool.begin().await.context("begin fenced chunk vector delete")?;
    let observed_source_truth_version = if let Some(ingest_attempt) = ingest_attempt {
        anyhow::ensure!(
            expected_source_truth_version > 0,
            "attempt-owned vector cleanup requires a positive source-truth fence"
        );
        let observed = lock_exclusive_vector_mutation_source(&mut transaction, library_id).await?;
        if !lock_latest_leased_ingest_attempt_authority(
            &mut transaction,
            library_id,
            ingest_attempt,
        )
        .await?
        {
            transaction.commit().await.context("commit preserved attempt-owned vector cleanup")?;
            return Ok(None);
        }
        anyhow::ensure!(
            observed >= expected_source_truth_version,
            "library source version moved backwards before attempt-owned vector cleanup"
        );
        observed
    } else {
        validate_revision_scoped_vector_delete_fence(
            &mut transaction,
            library_id,
            expected_source_truth_version,
        )
        .await?
    };
    let deleted =
        delete_chunk_vectors_in_transaction(&mut transaction, library_id, selector).await?;
    let source_truth_version = if deleted > 0 || advance_on_noop {
        advance_vector_source_truth_version(
            &mut transaction,
            library_id,
            observed_source_truth_version,
        )
        .await?
    } else {
        observed_source_truth_version
    };
    transaction.commit().await.context("commit fenced chunk vector delete")?;
    Ok(Some(VectorPlaneDeleteOutcome { deleted, source_truth_version }))
}

/// Locks the revision and proves exact ready chunk-vector coverage inside
/// a caller-owned publication transaction.
pub(crate) async fn validate_ingest_revision_vector_coverage(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    revision_id: Uuid,
    embedding_profile_key: Option<&str>,
) -> anyhow::Result<()> {
    let revision_number = sqlx::query_scalar::<_, i64>(
        "select revision_number
         from knowledge_revision
         where revision_id = $1 and library_id = $2
         for update",
    )
    .bind(revision_id)
    .bind(library_id)
    .fetch_optional(&mut **transaction)
    .await
    .context("lock revision for attempt-fenced readiness")?
    .ok_or_else(|| anyhow::anyhow!("revision disappeared before ingest readiness"))?;
    let chunk_count = sqlx::query_scalar::<_, i64>(
        "select count(*)::bigint
         from knowledge_chunk
         where library_id = $1
           and revision_id = $2
           and chunk_state = 'ready'",
    )
    .bind(library_id)
    .bind(revision_id)
    .fetch_one(&mut **transaction)
    .await
    .context("count ready chunks before ingest readiness")?;
    anyhow::ensure!(chunk_count >= 0, "ready chunk count was negative");

    let vector_count = if chunk_count == 0 {
        0
    } else {
        let profile = embedding_profile_key
            .context("non-empty revision readiness requires an exact embedding profile")?;
        validate_exact_embedding_profile_key(profile)?;
        let lanes = sqlx::query_as::<_, (i32, String)>(
            "select dim, relation_name
             from knowledge_vector_relation_manifest
             where library_id = $1
               and vector_kind = $2
               and embedding_model_key = $3
             order by dim",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(profile)
        .fetch_all(&mut **transaction)
        .await
        .context("resolve exact vector lane before ingest readiness")?;
        anyhow::ensure!(
            lanes.len() == 1,
            "ingest readiness requires exactly one exact-profile vector lane; found {}",
            lanes.len()
        );
        let (dim, relation_name) = &lanes[0];
        anyhow::ensure!(*dim > 0, "ingest readiness vector dimension must be positive");
        validate_relation_name(relation_name, CHUNK_VECTOR_RELATION_PREFIX)?;
        anyhow::ensure!(
            relation_name
                == &vector_relation_name(
                    CHUNK_VECTOR_RELATION_PREFIX,
                    u64::try_from(*dim).context("ingest readiness dimension overflowed u64")?,
                )?,
            "ingest readiness manifest dimension does not match its physical relation"
        );
        let relation = quote_identifier(relation_name)?;
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(active_chunk_vector_count_sql(&relation)))
            .bind(revision_id)
            .bind(profile)
            .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
            .bind(revision_number)
            .fetch_one(&mut **transaction)
            .await
            .context("count exact-profile vectors before ingest readiness")?
    };
    anyhow::ensure!(
        vector_count == chunk_count,
        "ingest readiness coverage mismatch for revision {revision_id}: {chunk_count} ready chunks, {vector_count} exact-profile vectors"
    );
    Ok(())
}

pub(crate) async fn delete_entity_vectors_by_library_fenced_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    expected_source_truth_version: i64,
) -> anyhow::Result<VectorPlaneDeleteOutcome> {
    let observed_source_truth_version = validate_library_global_vector_delete_fence(
        transaction,
        library_id,
        expected_source_truth_version,
    )
    .await?;
    let mut deleted = 0_u64;
    for relation_name in
        list_physical_vector_relations_in_transaction(transaction, ENTITY_VECTOR_RELATION_PREFIX)
            .await?
    {
        let relation = quote_identifier(&relation_name)?;
        let result = sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {relation} where library_id = $1"
        )))
        .bind(library_id)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("delete fenced entity vectors from {relation_name}"))?;
        deleted = deleted
            .checked_add(result.rows_affected())
            .context("fenced entity vector delete count overflowed u64")?;
    }
    refresh_library_vector_manifest_counts_in_transaction(
        transaction,
        library_id,
        ENTITY_VECTOR_RELATION_PREFIX,
    )
    .await?;
    let source_truth_version =
        advance_vector_source_truth_version(transaction, library_id, observed_source_truth_version)
            .await?;
    Ok(VectorPlaneDeleteOutcome { deleted, source_truth_version })
}

/// Identifies one staged vector lane being promoted: its logical kind, its
/// physical relation prefix, and the staging/active embedding profile keys.
/// Named so the promotion signature carries one lane identity instead of four
/// loose strings that must agree with each other.
pub(super) struct StagedVectorLane<'a> {
    pub(super) vector_kind: &'a str,
    pub(super) relation_prefix: &'a str,
    pub(super) active_embedding_profile_key: &'a str,
    pub(super) staging_embedding_model_key: &'a str,
}

pub(super) async fn promote_staged_vector_kind(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    dimensions: u64,
    lane: &StagedVectorLane<'_>,
    expected_count: u64,
) -> anyhow::Result<()> {
    let StagedVectorLane {
        vector_kind,
        relation_prefix,
        active_embedding_profile_key,
        staging_embedding_model_key,
    } = *lane;
    let target_relation_name = vector_relation_name(relation_prefix, dimensions)?;
    let staged_relation_name = sqlx::query_scalar::<_, String>(
        "select relation_name
         from knowledge_vector_relation_manifest
         where library_id = $1
           and dim = $2
           and vector_kind = $3
           and embedding_model_key = $4
         for update",
    )
    .bind(library_id)
    .bind(checked_dim_i32(dimensions)?)
    .bind(vector_kind)
    .bind(staging_embedding_model_key)
    .fetch_optional(&mut **transaction)
    .await
    .context("failed to lock staged vector manifest lane")?
    .ok_or_else(|| anyhow::anyhow!("staged vector manifest lane is missing"))?;
    anyhow::ensure!(
        staged_relation_name == target_relation_name,
        "staged vector manifest points to an unexpected dimension relation"
    );

    let target_relation = quote_identifier(&target_relation_name)?;
    let staged_count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        "select count(*)::bigint
         from {target_relation}
         where library_id = $1
           and vector_kind = $2
           and embedding_model_key = $3"
    )))
    .bind(library_id)
    .bind(vector_kind)
    .bind(staging_embedding_model_key)
    .fetch_one(&mut **transaction)
    .await
    .context("failed to count staged vector rows")?;
    let expected_count =
        i64::try_from(expected_count).context("staged vector count overflowed i64")?;
    anyhow::ensure!(
        staged_count == expected_count,
        "staged vector row count mismatch: expected {expected_count}, found {staged_count}"
    );

    let mut old_relations = sqlx::query_scalar::<_, String>(
        "select distinct relation_name
         from knowledge_vector_relation_manifest
         where library_id = $1
           and vector_kind = $2
         order by relation_name",
    )
    .bind(library_id)
    .bind(vector_kind)
    .fetch_all(&mut **transaction)
    .await
    .context("failed to list vector relations replaced by staged promotion")?
    .into_iter()
    .collect::<BTreeSet<_>>();
    old_relations
        .extend(list_physical_vector_relations_in_transaction(transaction, relation_prefix).await?);
    for relation_name in old_relations {
        validate_relation_name(&relation_name, relation_prefix)?;
        let relation = quote_identifier(&relation_name)?;
        if relation_name == target_relation_name {
            let (staged_identity_match, source_identity_exists) = match vector_kind {
                KNOWLEDGE_CHUNK_VECTOR_KIND => (
                    "staged.chunk_id = prior_vector.chunk_id
                     and staged.revision_id = prior_vector.revision_id",
                    "exists (
                        select 1
                        from knowledge_chunk source_chunk
                        where source_chunk.library_id = prior_vector.library_id
                          and source_chunk.chunk_id = prior_vector.chunk_id
                          and source_chunk.revision_id = prior_vector.revision_id
                     )",
                ),
                KNOWLEDGE_ENTITY_VECTOR_KIND => (
                    "staged.entity_id = prior_vector.entity_id",
                    "exists (
                        select 1
                        from knowledge_entity source_entity
                        where source_entity.library_id = prior_vector.library_id
                          and source_entity.entity_id = prior_vector.entity_id
                     )",
                ),
                _ => anyhow::bail!("staged vector promotion has an unsupported vector kind"),
            };
            // Preserve same-profile target-dimension rows that are not part of
            // this snapshot only while their backing source identity still
            // exists (for example a not-yet-readable ingest revision). Every
            // staged identity is replaced; obsolete profiles and source-less
            // physical orphans are purged in the same promotion transaction.
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {relation} prior_vector
                 where prior_vector.library_id = $1
                   and prior_vector.vector_kind = $2
                   and prior_vector.embedding_model_key <> $3
                   and (
                        prior_vector.embedding_model_key <> $4
                        or not ({source_identity_exists})
                        or exists (
                            select 1
                            from {target_relation} staged
                            where staged.library_id = prior_vector.library_id
                              and staged.vector_kind = prior_vector.vector_kind
                              and staged.embedding_model_key = $3
                              and {staged_identity_match}
                        )
                   )"
            )))
            .bind(library_id)
            .bind(vector_kind)
            .bind(staging_embedding_model_key)
            .bind(active_embedding_profile_key)
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("failed to replace active vector rows in {relation_name}"))?;
        } else {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "delete from {relation}
                 where library_id = $1
                   and vector_kind = $2
                   and embedding_model_key <> $3"
            )))
            .bind(library_id)
            .bind(vector_kind)
            .bind(staging_embedding_model_key)
            .execute(&mut **transaction)
            .await
            .with_context(|| {
                format!("failed to remove replaced vector rows from {relation_name}")
            })?;
        }
    }

    let promoted = sqlx::query(sqlx::AssertSqlSafe(format!(
        "update {target_relation}
         set embedding_model_key = $4
         where library_id = $1
           and vector_kind = $2
           and embedding_model_key = $3"
    )))
    .bind(library_id)
    .bind(vector_kind)
    .bind(staging_embedding_model_key)
    .bind(active_embedding_profile_key)
    .execute(&mut **transaction)
    .await
    .context("failed to promote staged vector rows")?;
    anyhow::ensure!(
        i64::try_from(promoted.rows_affected()).ok() == Some(expected_count),
        "staged vector promotion changed an unexpected row count"
    );
    let active_row_count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        "select count(*)::bigint
         from {target_relation}
         where library_id = $1
           and vector_kind = $2
           and embedding_model_key = $3"
    )))
    .bind(library_id)
    .bind(vector_kind)
    .bind(active_embedding_profile_key)
    .fetch_one(&mut **transaction)
    .await
    .context("failed to count promoted active vector rows")?;

    sqlx::query(
        "delete from knowledge_vector_relation_manifest
         where library_id = $1 and vector_kind = $2",
    )
    .bind(library_id)
    .bind(vector_kind)
    .execute(&mut **transaction)
    .await
    .context("failed to retire replaced vector manifest lanes")?;
    sqlx::query(
        "insert into knowledge_vector_relation_manifest (
            library_id, dim, vector_kind, embedding_model_key, relation_name,
            is_default, row_count, promoted
         ) values ($1, $2, $3, $4, $5, true, $6, true)",
    )
    .bind(library_id)
    .bind(checked_dim_i32(dimensions)?)
    .bind(vector_kind)
    .bind(active_embedding_profile_key)
    .bind(target_relation_name)
    .bind(active_row_count)
    .execute(&mut **transaction)
    .await
    .context("failed to install promoted vector manifest lane")?;
    Ok(())
}
