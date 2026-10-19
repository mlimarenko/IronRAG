use super::*;
use crate::infra::postgres::pg_vector_config::{
    VECTOR_RELATION_LIBRARY_CASCADE_PREDICATE, ensure_vector_relation_library_cascade,
    vector_relation_library_cascade_name,
};

/// Logical vector-manifest lane identity shared by the chunk and entity bulk
/// upserts and every lane-keyed read: `(dim, library_id, vector_kind,
/// embedding_model_key)`. Named once so the two `upsert_*_bulk_immediate`
/// grouping maps cannot drift apart and neither signature carries a raw
/// four-tuple.
type VectorLaneKey = (u64, Uuid, String, String);

impl PgSearchStore {
    async fn vector_relation_objects_exist<'executor, Executor>(
        executor: Executor,
        relation_name: &str,
        id_column: &str,
        extra_column: Option<&str>,
    ) -> anyhow::Result<bool>
    where
        Executor: sqlx::Executor<'executor, Database = Postgres>,
    {
        let required_objects =
            vector_relation_required_objects(relation_name, id_column, extra_column);
        sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!(
            "select coalesce(bool_and(
                        to_regclass(format('%I.%I', current_schema(), object_name)) is not null
                    ), false)
                    and {VECTOR_RELATION_LIBRARY_CASCADE_PREDICATE}
             from unnest($3::text[]) required(object_name)"
        )))
        .bind(vector_relation_library_cascade_name(relation_name))
        .bind(relation_name)
        .bind(required_objects)
        .fetch_one(executor)
        .await
        .with_context(|| format!("failed to inspect vector relation objects for {relation_name}"))
    }

    pub(super) async fn ensure_chunk_vector_relation(&self, dim: u64) -> anyhow::Result<String> {
        let relation_name = vector_relation_name(CHUNK_VECTOR_RELATION_PREFIX, dim)?;
        if Self::vector_relation_objects_exist(
            &self.pool,
            &relation_name,
            "chunk_id",
            Some("revision_id"),
        )
        .await?
        {
            return Ok(relation_name);
        }
        let relation = quote_identifier(&relation_name)?;
        let storage = PgVectorStorage::for_dim(dim);
        let dim = checked_dim_i32(dim)?;
        let embedding_type = storage.column_type(dim);
        let mut transaction = self.pool.begin().await.context("begin chunk vector relation DDL")?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(VECTOR_RELATION_DDL_ADVISORY_LOCK_KEY)
            .execute(&mut *transaction)
            .await
            .context("serialize shared vector relation DDL")?;
        if Self::vector_relation_objects_exist(
            &mut *transaction,
            &relation_name,
            "chunk_id",
            Some("revision_id"),
        )
        .await?
        {
            transaction.commit().await.context("commit chunk vector DDL recheck")?;
            return Ok(relation_name);
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "create table if not exists {relation} (
                key text primary key,
                vector_id uuid not null,
                workspace_id uuid not null,
                library_id uuid not null,
                chunk_id uuid not null,
                revision_id uuid not null,
                embedding_model_key text not null,
                vector_kind text not null,
                dimensions integer not null check (dimensions = {dim}),
                embedding {embedding_type} not null,
                freshness_generation bigint not null,
                created_at timestamptz not null,
                occurred_at timestamptz,
                occurred_until timestamptz
            )"
        )))
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("failed to create chunk vector relation {relation_name}"))?;
        Self::ensure_vector_relation_indexes(
            &mut transaction,
            &relation_name,
            "chunk_id",
            Some("revision_id"),
            storage,
            dim,
        )
        .await?;
        ensure_vector_relation_library_cascade(&mut transaction, &relation_name).await?;
        transaction.commit().await.context("commit chunk vector relation DDL")?;
        Ok(relation_name)
    }

    pub(super) async fn ensure_entity_vector_relation(&self, dim: u64) -> anyhow::Result<String> {
        let relation_name = vector_relation_name(ENTITY_VECTOR_RELATION_PREFIX, dim)?;
        if Self::vector_relation_objects_exist(&self.pool, &relation_name, "entity_id", None)
            .await?
        {
            return Ok(relation_name);
        }
        let relation = quote_identifier(&relation_name)?;
        let storage = PgVectorStorage::for_dim(dim);
        let dim = checked_dim_i32(dim)?;
        let embedding_type = storage.column_type(dim);
        let mut transaction =
            self.pool.begin().await.context("begin entity vector relation DDL")?;
        sqlx::query("select pg_advisory_xact_lock(hashtextextended($1::text, 0))")
            .bind(VECTOR_RELATION_DDL_ADVISORY_LOCK_KEY)
            .execute(&mut *transaction)
            .await
            .context("serialize shared vector relation DDL")?;
        if Self::vector_relation_objects_exist(&mut *transaction, &relation_name, "entity_id", None)
            .await?
        {
            transaction.commit().await.context("commit entity vector DDL recheck")?;
            return Ok(relation_name);
        }
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "create table if not exists {relation} (
                key text primary key,
                vector_id uuid not null,
                workspace_id uuid not null,
                library_id uuid not null,
                entity_id uuid not null,
                embedding_model_key text not null,
                vector_kind text not null,
                dimensions integer not null check (dimensions = {dim}),
                embedding {embedding_type} not null,
                freshness_generation bigint not null,
                created_at timestamptz not null
            )"
        )))
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("failed to create entity vector relation {relation_name}"))?;
        Self::ensure_vector_relation_indexes(
            &mut transaction,
            &relation_name,
            "entity_id",
            None,
            storage,
            dim,
        )
        .await?;
        ensure_vector_relation_library_cascade(&mut transaction, &relation_name).await?;
        transaction.commit().await.context("commit entity vector relation DDL")?;
        Ok(relation_name)
    }

    async fn ensure_vector_relation_indexes(
        transaction: &mut Transaction<'_, Postgres>,
        relation_name: &str,
        id_column: &str,
        extra_column: Option<&str>,
        storage: PgVectorStorage,
        dim: i32,
    ) -> anyhow::Result<()> {
        let relation = quote_identifier(relation_name)?;
        let quoted_id_column = quote_identifier(id_column)?;
        let logical_identity_columns = match extra_column {
            Some(extra_column) => format!(
                "library_id, {quoted_id_column}, {}, embedding_model_key, vector_kind, freshness_generation",
                quote_identifier(extra_column)?,
            ),
            None => format!(
                "library_id, {quoted_id_column}, embedding_model_key, vector_kind, freshness_generation"
            ),
        };
        let logical_key_index_name = format!("{relation_name}_logical_key");
        let logical_key_index_exists = sqlx::query_scalar::<_, bool>(
            "select to_regclass(format('%I.%I', current_schema(), $1)) is not null",
        )
        .bind(&logical_key_index_name)
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("inspect logical key index on {relation_name}"))?;
        if !logical_key_index_exists {
            let has_logical_duplicates =
                sqlx::query_scalar::<_, bool>(sqlx::AssertSqlSafe(format!(
                    "select exists (
                         select 1
                         from {relation}
                         group by {logical_identity_columns}
                         having count(*) > 1
                         limit 1
                     )"
                )))
                .fetch_one(&mut **transaction)
                .await
                .with_context(|| format!("inspect logical vector duplicates in {relation_name}"))?;
            anyhow::ensure!(
                !has_logical_duplicates,
                "cannot install logical vector uniqueness on {relation_name}: duplicate object/profile/generation rows require explicit repair"
            );
            let logical_key_idx = quote_identifier(&logical_key_index_name)?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "create unique index if not exists {logical_key_idx}
                 on {relation} ({logical_identity_columns})"
            )))
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("failed to create logical key index on {relation_name}"))?;
        }

        let lane_idx = quote_identifier(&format!("{relation_name}_lane_idx"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "create index if not exists {lane_idx}
             on {relation} (library_id, embedding_model_key, vector_kind)"
        )))
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("failed to create lane index on {relation_name}"))?;

        let id_idx = quote_identifier(&format!("{relation_name}_{id_column}_idx"))?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "create index if not exists {id_idx} on {relation} ({quoted_id_column})"
        )))
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("failed to create id index on {relation_name}"))?;

        if let Some(extra_column) = extra_column {
            let extra_idx = quote_identifier(&format!("{relation_name}_{extra_column}_idx"))?;
            let quoted_extra_column = quote_identifier(extra_column)?;
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "create index if not exists {extra_idx} on {relation} ({quoted_extra_column})"
            )))
            .execute(&mut **transaction)
            .await
            .with_context(|| format!("failed to create extra index on {relation_name}"))?;
        }
        Self::ensure_vector_relation_hnsw_index(transaction, relation_name, storage, dim).await?;
        Ok(())
    }

    async fn ensure_vector_relation_hnsw_index(
        transaction: &mut Transaction<'_, Postgres>,
        relation_name: &str,
        storage: PgVectorStorage,
        dim: i32,
    ) -> anyhow::Result<()> {
        let relation = quote_identifier(relation_name)?;
        let hnsw_index_name = format!("{relation_name}_hnsw");
        let index_exists = sqlx::query_scalar::<_, bool>("select to_regclass($1) is not null")
            .bind(&hnsw_index_name)
            .fetch_one(&mut **transaction)
            .await
            .with_context(|| format!("failed to inspect HNSW index {hnsw_index_name}"))?;
        if index_exists {
            return Ok(());
        }

        let hnsw_idx = quote_identifier(&hnsw_index_name)?;
        let row_count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "select count(*)::bigint from {relation}"
        )))
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("failed to count rows in {relation_name} for HNSW sizing"))?;
        let row_count = u64::try_from(row_count).context("negative vector shard row count")?;
        let params = pg_hnsw_index_params(row_count, dim, storage)?;
        let ops = storage.cosine_ops();
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "create index if not exists {hnsw_idx}
             on {relation} using hnsw (embedding {ops})
             with (m = {m}, ef_construction = {ef_construction})",
            m = params.m,
            ef_construction = params.ef_construction
        )))
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("failed to create HNSW index on {relation_name}"))?;
        Ok(())
    }

    pub(super) async fn upsert_manifest_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        library_id: Uuid,
        dim: u64,
        vector_kind: &str,
        embedding_model_key: &str,
        relation_name: &str,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "insert into knowledge_vector_relation_manifest (
                library_id, dim, vector_kind, embedding_model_key, relation_name,
                is_default, row_count, promoted
             ) values ($1, $2, $3, $4, $5, true, 0, false)
             on conflict (library_id, dim, vector_kind, embedding_model_key)
             do update set relation_name = excluded.relation_name,
                           is_default = true,
                           promoted = false",
        )
        .bind(library_id)
        .bind(checked_dim_i32(dim)?)
        .bind(vector_kind)
        .bind(embedding_model_key)
        .bind(relation_name)
        .execute(&mut **transaction)
        .await
        .context("failed to upsert vector relation manifest in vector write transaction")?;
        Ok(())
    }

    pub(super) async fn resolve_manifest_search_lane(
        &self,
        library_id: Uuid,
        dim: u64,
        vector_kind: &str,
        embedding_model_key: &str,
        expected_prefix: &str,
    ) -> anyhow::Result<Option<PgVectorSearchLane>> {
        let row = sqlx::query_as::<_, (String, i64)>(
            "select relation_name, row_count
             from knowledge_vector_relation_manifest
             where library_id = $1
               and dim = $2
               and vector_kind = $3
               and embedding_model_key = $4",
        )
        .bind(library_id)
        .bind(checked_dim_i32(dim)?)
        .bind(vector_kind)
        .bind(embedding_model_key)
        .fetch_optional(&self.pool)
        .await
        .context("failed to resolve vector search lane manifest")?;
        let Some((relation_name, row_count)) = row else {
            return Ok(None);
        };
        validate_relation_name(&relation_name, expected_prefix)?;
        let manifest_row_count = u64::try_from(row_count)
            .context("vector search lane manifest row count was negative")?;
        Ok(Some(PgVectorSearchLane { relation_name, manifest_row_count }))
    }

    pub(super) async fn list_vector_relations(
        &self,
        expected_prefix: &str,
    ) -> anyhow::Result<Vec<String>> {
        let rows = sqlx::query_scalar::<_, String>(
            "select distinct relation_name
             from knowledge_vector_relation_manifest
             where relation_name like $1
             order by relation_name",
        )
        .bind(format!("{expected_prefix}%"))
        .fetch_all(&self.pool)
        .await
        .context("failed to list vector relations")?;
        rows.into_iter()
            .map(|relation_name| {
                validate_relation_name(&relation_name, expected_prefix)?;
                Ok(relation_name)
            })
            .collect()
    }

    async fn refresh_manifest_count_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        relation_name: &str,
        library_id: Uuid,
        dim: i32,
        vector_kind: &str,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        let relation = quote_identifier(relation_name)?;
        let row_count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
            "select count(*)::bigint
             from {relation}
             where library_id = $1
               and vector_kind = $2
               and embedding_model_key = $3"
        )))
        .bind(library_id)
        .bind(vector_kind)
        .bind(embedding_model_key)
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("failed to count rows in {relation_name}"))?;
        let updated = sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = $5
             where library_id = $1
               and dim = $2
               and vector_kind = $3
               and embedding_model_key = $4
               and promoted = false",
        )
        .bind(library_id)
        .bind(dim)
        .bind(vector_kind)
        .bind(embedding_model_key)
        .bind(row_count)
        .execute(&mut **transaction)
        .await
        .context("failed to refresh prepared manifest row_count")?;
        ensure_single_manifest_row_updated(updated.rows_affected())?;
        Ok(())
    }

    pub(super) async fn upsert_chunk_vectors_in_relation_bulk_with_executor<'e, E>(
        relation_name: &str,
        rows: &[&KnowledgeChunkVectorRow],
        executor: E,
    ) -> anyhow::Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let Some(first) = rows.first() else {
            return Ok(());
        };
        let relation = quote_identifier(relation_name)?;
        let storage = PgVectorStorage::for_dim(u64::try_from(first.dimensions)?);
        let cast_type = storage.cast_type();
        let vector_literals = rows
            .iter()
            .map(|row| pgvector_literal(&row.vector))
            .collect::<anyhow::Result<Vec<_>>>()?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into {relation} (
                key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                embedding_model_key, vector_kind, dimensions, embedding,
                freshness_generation, created_at, occurred_at, occurred_until
             )
             select input.key, input.vector_id, input.workspace_id, input.library_id,
                    input.chunk_id, input.revision_id, input.embedding_model_key,
                    input.vector_kind, input.dimensions, input.embedding_text::{cast_type},
                    input.freshness_generation, input.created_at,
                    input.occurred_at, input.occurred_until
             from unnest(
                $1::text[], $2::uuid[], $3::uuid[], $4::uuid[], $5::uuid[], $6::uuid[],
                $7::text[], $8::text[], $9::integer[], $10::text[], $11::bigint[],
                $12::timestamptz[], $13::timestamptz[], $14::timestamptz[]
             ) as input(
                key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                embedding_model_key, vector_kind, dimensions, embedding_text,
                freshness_generation, created_at, occurred_at, occurred_until
             )
             on conflict (
                library_id, chunk_id, revision_id, embedding_model_key,
                vector_kind, freshness_generation
             ) do update set
                key = excluded.key,
                vector_id = excluded.vector_id,
                workspace_id = excluded.workspace_id,
                dimensions = excluded.dimensions,
                embedding = excluded.embedding,
                created_at = excluded.created_at,
                occurred_at = excluded.occurred_at,
                occurred_until = excluded.occurred_until"
        )))
        .bind(rows.iter().map(|row| row.vector_id.to_string()).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.vector_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.workspace_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.library_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.revision_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.embedding_model_key.clone()).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.vector_kind.clone()).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.dimensions).collect::<Vec<_>>())
        .bind(vector_literals)
        .bind(rows.iter().map(|row| row.freshness_generation).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.created_at).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.occurred_at).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.occurred_until).collect::<Vec<_>>())
        .execute(executor)
        .await
        .with_context(|| {
            format!("failed to bulk-upsert {} chunk vectors into {relation_name}", rows.len())
        })?;
        Ok(())
    }

    pub(super) async fn upsert_entity_vectors_in_relation_bulk_with_executor<'e, E>(
        relation_name: &str,
        rows: &[&KnowledgeEntityVectorRow],
        executor: E,
    ) -> anyhow::Result<()>
    where
        E: Executor<'e, Database = Postgres>,
    {
        let Some(first) = rows.first() else {
            return Ok(());
        };
        let relation = quote_identifier(relation_name)?;
        let storage = PgVectorStorage::for_dim(u64::try_from(first.dimensions)?);
        let cast_type = storage.cast_type();
        let vector_literals = rows
            .iter()
            .map(|row| pgvector_literal(&row.vector))
            .collect::<anyhow::Result<Vec<_>>>()?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "insert into {relation} (
                key, vector_id, workspace_id, library_id, entity_id, embedding_model_key,
                vector_kind, dimensions, embedding, freshness_generation, created_at
             )
             select input.key, input.vector_id, input.workspace_id, input.library_id,
                    input.entity_id, input.embedding_model_key, input.vector_kind,
                    input.dimensions, input.embedding_text::{cast_type},
                    input.freshness_generation, input.created_at
             from unnest(
                $1::text[], $2::uuid[], $3::uuid[], $4::uuid[], $5::uuid[], $6::text[],
                $7::text[], $8::integer[], $9::text[], $10::bigint[], $11::timestamptz[]
             ) as input(
                key, vector_id, workspace_id, library_id, entity_id, embedding_model_key,
                vector_kind, dimensions, embedding_text, freshness_generation, created_at
             )
             on conflict (
                library_id, entity_id, embedding_model_key,
                vector_kind, freshness_generation
             ) do update set
                key = excluded.key,
                vector_id = excluded.vector_id,
                workspace_id = excluded.workspace_id,
                dimensions = excluded.dimensions,
                embedding = excluded.embedding,
                created_at = excluded.created_at"
        )))
        .bind(rows.iter().map(|row| row.vector_id.to_string()).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.vector_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.workspace_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.library_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.entity_id).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.embedding_model_key.clone()).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.vector_kind.clone()).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.dimensions).collect::<Vec<_>>())
        .bind(vector_literals)
        .bind(rows.iter().map(|row| row.freshness_generation).collect::<Vec<_>>())
        .bind(rows.iter().map(|row| row.created_at).collect::<Vec<_>>())
        .execute(executor)
        .await
        .with_context(|| {
            format!("failed to bulk-upsert {} entity vectors into {relation_name}", rows.len())
        })?;
        Ok(())
    }

    pub(super) async fn upsert_chunk_vectors_bulk_immediate(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()> {
        let mut groups: BTreeMap<VectorLaneKey, Vec<&KnowledgeChunkVectorRow>> = BTreeMap::new();
        for row in rows {
            let dim = validate_row_vector_dimensions(row.dimensions, &row.vector, "chunk")?;
            groups
                .entry((
                    dim,
                    row.library_id,
                    row.vector_kind.clone(),
                    row.embedding_model_key.clone(),
                ))
                .or_default()
                .push(row);
        }
        for ((dim, library_id, vector_kind, embedding_model_key), rows) in groups {
            let relation_name = self.ensure_chunk_vector_relation(dim).await?;
            let mut transaction =
                self.pool.begin().await.context("begin immediate chunk vector write")?;
            lock_library_vector_plane_data(&mut transaction, library_id, false).await?;
            Self::upsert_manifest_in_transaction(
                &mut transaction,
                library_id,
                dim,
                &vector_kind,
                &embedding_model_key,
                &relation_name,
            )
            .await?;
            Self::upsert_chunk_vectors_in_relation_bulk_with_executor(
                &relation_name,
                rows.as_slice(),
                &mut *transaction,
            )
            .await?;
            Self::refresh_manifest_count_in_transaction(
                &mut transaction,
                &relation_name,
                library_id,
                checked_dim_i32(dim)?,
                &vector_kind,
                &embedding_model_key,
            )
            .await?;
            transaction.commit().await.context("commit immediate chunk vector write")?;
        }
        Ok(())
    }

    pub(super) async fn upsert_chunk_vectors_bulk_fenced_immediate(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &VectorWriteFence,
        active_profile_source: Option<&dyn ActiveEmbeddingProfileSource>,
    ) -> anyhow::Result<i64> {
        let first = rows.first().context("fenced chunk vector batch must not be empty")?;
        validate_exact_embedding_profile_key(&fence.embedding_profile_key)?;
        anyhow::ensure!(
            rows.iter().all(|row| row.library_id == first.library_id),
            "fenced chunk vector batch spans multiple libraries"
        );
        anyhow::ensure!(
            rows.iter().all(|row| {
                row.embedding_model_key == fence.embedding_profile_key
                    && row.embedding_model_key == first.embedding_model_key
                    && row.vector_kind == first.vector_kind
                    && row.dimensions == first.dimensions
            }),
            "fenced chunk vector batch spans multiple profiles, kinds, or dimensions"
        );
        let dim = validate_row_vector_dimensions(first.dimensions, &first.vector, "chunk")?;
        for row in &rows[1..] {
            anyhow::ensure!(
                validate_row_vector_dimensions(row.dimensions, &row.vector, "chunk")? == dim,
                "fenced chunk vector batch has mixed dimensions"
            );
        }
        let relation_name = self.ensure_chunk_vector_relation(dim).await?;
        let mut transaction = self.pool.begin().await.context("begin fenced chunk vector write")?;
        lock_library_vector_plane_data(
            &mut transaction,
            first.library_id,
            fence.advance_source_truth_version,
        )
        .await?;
        let observed_source_truth_version = validate_vector_write_fence(
            &mut transaction,
            first.library_id,
            fence,
            active_profile_source,
        )
        .await?;
        Self::upsert_manifest_in_transaction(
            &mut transaction,
            first.library_id,
            dim,
            &first.vector_kind,
            &first.embedding_model_key,
            &relation_name,
        )
        .await?;
        let borrowed = rows.iter().collect::<Vec<_>>();
        Self::upsert_chunk_vectors_in_relation_bulk_with_executor(
            &relation_name,
            borrowed.as_slice(),
            &mut *transaction,
        )
        .await?;
        Self::refresh_manifest_count_in_transaction(
            &mut transaction,
            &relation_name,
            first.library_id,
            checked_dim_i32(dim)?,
            &first.vector_kind,
            &first.embedding_model_key,
        )
        .await?;
        let source_truth_version = finish_vector_write_fence(
            &mut transaction,
            first.library_id,
            fence,
            observed_source_truth_version,
        )
        .await?;
        transaction.commit().await.context("commit fenced chunk vector write")?;
        Ok(source_truth_version)
    }

    pub(super) async fn upsert_entity_vectors_bulk_fenced_immediate(
        &self,
        rows: &[KnowledgeEntityVectorRow],
        fence: &VectorWriteFence,
    ) -> anyhow::Result<i64> {
        let first = rows.first().context("fenced entity vector batch must not be empty")?;
        validate_exact_embedding_profile_key(&fence.embedding_profile_key)?;
        anyhow::ensure!(
            rows.iter().all(|row| row.library_id == first.library_id),
            "fenced entity vector batch spans multiple libraries"
        );
        anyhow::ensure!(
            rows.iter().all(|row| {
                row.embedding_model_key == fence.embedding_profile_key
                    && row.embedding_model_key == first.embedding_model_key
                    && row.vector_kind == first.vector_kind
                    && row.dimensions == first.dimensions
            }),
            "fenced entity vector batch spans multiple profiles, kinds, or dimensions"
        );
        let dim = validate_row_vector_dimensions(first.dimensions, &first.vector, "entity")?;
        for row in &rows[1..] {
            anyhow::ensure!(
                validate_row_vector_dimensions(row.dimensions, &row.vector, "entity")? == dim,
                "fenced entity vector batch has mixed dimensions"
            );
        }
        let relation_name = self.ensure_entity_vector_relation(dim).await?;
        let mut transaction =
            self.pool.begin().await.context("begin fenced entity vector write")?;
        lock_library_vector_plane_data(
            &mut transaction,
            first.library_id,
            fence.advance_source_truth_version,
        )
        .await?;
        let observed_source_truth_version =
            validate_vector_write_fence(&mut transaction, first.library_id, fence, None).await?;
        Self::upsert_manifest_in_transaction(
            &mut transaction,
            first.library_id,
            dim,
            &first.vector_kind,
            &first.embedding_model_key,
            &relation_name,
        )
        .await?;
        let borrowed = rows.iter().collect::<Vec<_>>();
        Self::upsert_entity_vectors_in_relation_bulk_with_executor(
            &relation_name,
            borrowed.as_slice(),
            &mut *transaction,
        )
        .await?;
        Self::refresh_manifest_count_in_transaction(
            &mut transaction,
            &relation_name,
            first.library_id,
            checked_dim_i32(dim)?,
            &first.vector_kind,
            &first.embedding_model_key,
        )
        .await?;
        let source_truth_version = finish_vector_write_fence(
            &mut transaction,
            first.library_id,
            fence,
            observed_source_truth_version,
        )
        .await?;
        transaction.commit().await.context("commit fenced entity vector write")?;
        Ok(source_truth_version)
    }

    pub(super) async fn upsert_entity_vectors_bulk_immediate(
        &self,
        rows: &[KnowledgeEntityVectorRow],
    ) -> anyhow::Result<()> {
        let mut groups: BTreeMap<VectorLaneKey, Vec<&KnowledgeEntityVectorRow>> = BTreeMap::new();
        for row in rows {
            let dim = validate_row_vector_dimensions(row.dimensions, &row.vector, "entity")?;
            groups
                .entry((
                    dim,
                    row.library_id,
                    row.vector_kind.clone(),
                    row.embedding_model_key.clone(),
                ))
                .or_default()
                .push(row);
        }
        for ((dim, library_id, vector_kind, embedding_model_key), rows) in groups {
            let relation_name = self.ensure_entity_vector_relation(dim).await?;
            let mut transaction =
                self.pool.begin().await.context("begin immediate entity vector write")?;
            lock_library_vector_plane_data(&mut transaction, library_id, false).await?;
            Self::upsert_manifest_in_transaction(
                &mut transaction,
                library_id,
                dim,
                &vector_kind,
                &embedding_model_key,
                &relation_name,
            )
            .await?;
            Self::upsert_entity_vectors_in_relation_bulk_with_executor(
                &relation_name,
                rows.as_slice(),
                &mut *transaction,
            )
            .await?;
            Self::refresh_manifest_count_in_transaction(
                &mut transaction,
                &relation_name,
                library_id,
                checked_dim_i32(dim)?,
                &vector_kind,
                &embedding_model_key,
            )
            .await?;
            transaction.commit().await.context("commit immediate entity vector write")?;
        }
        Ok(())
    }

    pub(super) async fn reconcile_manifest_count_for_lane(
        &self,
        library_id: Uuid,
        dimensions: u64,
        vector_kind: &str,
        embedding_model_key: &str,
        relation_prefix: &str,
    ) -> anyhow::Result<()> {
        let mut transaction =
            self.pool.begin().await.context("begin vector manifest reconciliation")?;
        lock_library_vector_plane_data(&mut transaction, library_id, false).await?;
        let relation_name = lock_prepared_manifest_lane_for_share(
            &mut transaction,
            library_id,
            dimensions,
            vector_kind,
            embedding_model_key,
            relation_prefix,
        )
        .await?;
        Self::refresh_manifest_count_in_transaction(
            &mut transaction,
            &relation_name,
            library_id,
            checked_dim_i32(dimensions)?,
            vector_kind,
            embedding_model_key,
        )
        .await?;
        transaction.commit().await.context("commit vector manifest reconciliation")?;
        Ok(())
    }
}
