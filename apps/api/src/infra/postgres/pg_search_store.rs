#![allow(clippy::too_many_lines)]

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::domains::query_ir::literal_text_is_identifier_shaped;
use crate::infra::{
    arangodb::search_store::{
        KNOWLEDGE_CHUNK_VECTOR_KIND, KNOWLEDGE_ENTITY_VECTOR_KIND, KnowledgeChunkSearchRow,
        KnowledgeChunkVectorRow, KnowledgeChunkVectorSearchRow, KnowledgeEntitySearchRow,
        KnowledgeEntityVectorRow, KnowledgeEntityVectorSearchRow, KnowledgeRelationSearchRow,
        KnowledgeStructuredBlockSearchRow, KnowledgeTechnicalFactSearchRow,
    },
    knowledge_plane::SearchStore,
};

const TITLE_NGRAM_MIN_TERM_CHARS: usize = 8;
const TITLE_NGRAM_MAX_TERMS: usize = 4;
const TITLE_IDENTITY_MAX_TERMS: usize = 6;
/// Character length each query token is truncated to when building a prefix
/// (`p:*`) tsquery for the lexical relaxation ladder. Language-agnostic: it is
/// a plain `chars().take(N)` cut, no stemmer/dictionary. 5 was eval-tuned on the
/// internal corpus to bridge morphological surface forms (e.g. a verb stem and
/// its noun derivative share a 5-char prefix) without over-matching.
const LEXICAL_PREFIX_LEN: usize = 5;
/// Tokens shorter than this are dropped from the relaxed prefix query: a 2-3
/// char prefix matches far too broadly to preserve precision.
const LEXICAL_PREFIX_MIN_TOKEN_CHARS: usize = 4;
/// The precise pass (exact AND, `websearch_to_tsquery`) is only relaxed when it
/// returns fewer than this many hits. Queries that already retrieve a healthy
/// set are left untouched, so precision is never traded for recall on them.
const LEXICAL_RELAX_FLOOR: usize = 8;
const CHUNK_VECTOR_RELATION_PREFIX: &str = "knowledge_chunk_vector_d";
const ENTITY_VECTOR_RELATION_PREFIX: &str = "knowledge_entity_vector_d";
const PGVECTOR_HNSW_VECTOR_MAX_DIM: u64 = 2000;
const PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES: u64 = 3_000_000_000;
const PG_HNSW_DEFAULT_EF_SEARCH: u64 = 400;
const PG_HNSW_MIN_M: u64 = 8;
const PG_HNSW_MID_M: u64 = 16;
const PG_HNSW_LARGE_M: u64 = 24;

/// Chunk lexical-lane CTE with the FTS constructor abstracted as `{FTS}`. The
/// two rungs of the relaxation ladder share this template verbatim and differ
/// only in which tsquery constructor is substituted — the user query text always
/// binds via `$2`, never interpolated, so no user data enters the SQL string.
const CHUNK_LEXICAL_SQL_TEMPLATE: &str = "with title_identity_docs as (
                 select d.document_id
                 from knowledge_document d
                 where d.library_id = $1
                   and d.document_state = 'active'
                   and cardinality($7::text[]) > 0
                   and not exists (
                       select 1
                       from unnest($7::text[]) term(value)
                       where not (
                           case when term.value ~ '[0-9]'
                                then strpos(
                                    ' ' || lower(coalesce(d.title, '') || ' ' || coalesce(d.file_name, '')) || ' ',
                                    ' ' || term.value || ' '
                                ) > 0
                                else strpos(
                                    lower(coalesce(d.title, '') || ' ' || coalesce(d.file_name, '')),
                                    term.value
                                ) > 0
                           end
                       )
                   )
                 limit 50
             ),
             title_match_docs as (
                 select distinct d.document_id
                 from knowledge_document d
                 where d.library_id = $1
                   and d.document_state = 'active'
                   and (
                       exists (
                           select 1 from unnest($6::text[]) term(value)
                           where strpos(
                               lower(coalesce(d.title, '') || ' ' || coalesce(d.file_name, '')),
                               term.value
                           ) > 0
                       )
                       or exists (
                           select 1 from unnest($8::text[]) term(value)
                           where similarity(coalesce(d.title, ''), term.value) >= 0.40
                              or similarity(coalesce(d.file_name, ''), term.value) >= 0.40
                              or coalesce(d.title, '') % term.value
                              or coalesce(d.file_name, '') % term.value
                       )
                   )
                 limit 50
             ),
             soft_title_docs as (
                 select document_id from title_match_docs
                 except
                 select document_id from title_identity_docs
             ),
             text_raw as (
                 select c.chunk_id, c.document_id, c.workspace_id, c.library_id, c.revision_id,
                    c.content_text, c.normalized_text, c.section_path, c.heading_trail,
                    ts_rank_cd(c.search_tsv, {FTS})::double precision
                        * case
                            when exists (select 1 from title_identity_docs tid where tid.document_id = c.document_id) then 8.0
                            when exists (select 1 from soft_title_docs std where std.document_id = c.document_id) then 2.0
                            when exists (
                                select 1 from unnest($6::text[]) term(value)
                                where strpos(lower(array_to_string(c.heading_trail, ' ')), term.value) > 0
                            ) then 3.0
                            else 1.0
                          end
                        * case
                            when exists (
                                select 1 from unnest($6::text[]) term(value)
                                where strpos(lower(array_to_string(c.section_path, ' ')), term.value) > 0
                            ) then 1.5
                            else 1.0
                          end
                        * coalesce(c.quality_score::double precision, 1.0) as score,
                    c.quality_score
                 from knowledge_chunk c
                 where c.library_id = $1
                   and c.chunk_state = 'ready'
                   and c.search_tsv @@ {FTS}
                   and (($4::timestamptz is null and $5::timestamptz is null)
                        or (c.occurred_at is not null
                            and ($4::timestamptz is null or coalesce(c.occurred_until, c.occurred_at) >= $4)
                            and ($5::timestamptz is null or c.occurred_at <= $5)))
                 order by score desc, c.chunk_id asc
                 limit $3
             ),
             title_identity_raw as (
                 select chunk_id, document_id, workspace_id, library_id, revision_id,
                    content_text, normalized_text, section_path, heading_trail,
                    score,
                    quality_score
                 from (
                    select scored.*,
                        row_number() over (
                            partition by scored.document_id
                            order by scored.score desc, scored.revision_id desc,
                                scored.chunk_index asc, scored.chunk_id asc
                        ) as rn
                    from (
                        select c.*,
                            ((1000000.0 - c.chunk_index::double precision)
                                * coalesce(c.quality_score::double precision, 1.0)) as score
                        from knowledge_chunk c
                        join title_identity_docs d on d.document_id = c.document_id
                        where c.library_id = $1
                          and c.chunk_state = 'ready'
                          and (($4::timestamptz is null and $5::timestamptz is null)
                               or (c.occurred_at is not null
                                   and ($4::timestamptz is null or coalesce(c.occurred_until, c.occurred_at) >= $4)
                                   and ($5::timestamptz is null or c.occurred_at <= $5)))
                    ) scored
                 ) ranked
                 where rn <= 2
             ),
             title_soft_raw as (
                 select chunk_id, document_id, workspace_id, library_id, revision_id,
                    content_text, normalized_text, section_path, heading_trail,
                    score,
                    quality_score
                 from (
                    select scored.*,
                        row_number() over (
                            partition by scored.document_id
                            order by scored.score desc, scored.revision_id desc,
                                scored.chunk_index asc, scored.chunk_id asc
                        ) as rn
                    from (
                        select c.*,
                            ((50.0 - (c.chunk_index::double precision * 0.001))
                                * coalesce(c.quality_score::double precision, 1.0)) as score
                        from knowledge_chunk c
                        join soft_title_docs d on d.document_id = c.document_id
                        where $9::boolean
                          and c.library_id = $1
                          and c.chunk_state = 'ready'
                          and (($4::timestamptz is null and $5::timestamptz is null)
                               or (c.occurred_at is not null
                                   and ($4::timestamptz is null or coalesce(c.occurred_until, c.occurred_at) >= $4)
                                   and ($5::timestamptz is null or c.occurred_at <= $5)))
                    ) scored
                 ) ranked
                 where rn <= 2
             ),
             raw as (
                 select * from text_raw
                 union all select * from title_identity_raw
                 union all select * from title_soft_raw
             ),
             diversified as (
                 select *,
                    row_number() over (partition by document_id order by score desc, chunk_id asc) as per_doc_rank
                 from raw
             )
             select chunk_id, workspace_id, library_id, revision_id, content_text, normalized_text,
                section_path, heading_trail, score, quality_score
             from diversified
             where per_doc_rank <= 2
             order by score desc, chunk_id asc
             limit $10";

/// Pass A (precise): exact AND `websearch_to_tsquery`. Byte-for-byte the
/// original chunk query — the executed SQL is unchanged.
static CHUNK_LEXICAL_SQL_EXACT: LazyLock<String> = LazyLock::new(|| {
    CHUNK_LEXICAL_SQL_TEMPLATE
        .replace("{FTS}", "websearch_to_tsquery('simple', ironrag_unaccent($2))")
});

/// Relaxed passes (B/C): `to_tsquery`, fed a prefix tsquery string via `$2`.
static CHUNK_LEXICAL_SQL_PREFIX: LazyLock<String> = LazyLock::new(|| {
    CHUNK_LEXICAL_SQL_TEMPLATE.replace("{FTS}", "to_tsquery('simple', ironrag_unaccent($2))")
});

#[derive(Clone)]
pub struct PgSearchStore {
    pub pool: PgPool,
}

#[derive(Debug, Clone, FromRow)]
struct PgChunkVectorRow {
    key: String,
    vector_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    dimensions: i32,
    vector_text: String,
    freshness_generation: i64,
    created_at: DateTime<Utc>,
    occurred_at: Option<DateTime<Utc>>,
    occurred_until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, FromRow)]
struct PgEntityVectorRow {
    key: String,
    vector_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    entity_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    dimensions: i32,
    vector_text: String,
    freshness_generation: i64,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, FromRow)]
struct PgChunkSearchRow {
    chunk_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    content_text: String,
    normalized_text: String,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    score: f64,
    quality_score: Option<f32>,
}

#[derive(Debug, Clone, FromRow)]
struct PgStructuredBlockSearchRow {
    block_id: Uuid,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    ordinal: i32,
    block_kind: String,
    text: String,
    normalized_text: String,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgTechnicalFactSearchRow {
    fact_id: Uuid,
    document_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    revision_id: Uuid,
    fact_kind: String,
    canonical_value_text: String,
    display_value: String,
    exact_match: bool,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgEntitySearchRow {
    entity_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    canonical_label: String,
    entity_type: String,
    summary: Option<String>,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgRelationSearchRow {
    relation_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    predicate: String,
    normalized_assertion: String,
    summary: Option<String>,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgChunkVectorSearchRow {
    vector_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    chunk_id: Uuid,
    revision_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    freshness_generation: i64,
    score: f64,
}

#[derive(Debug, Clone, FromRow)]
struct PgEntityVectorSearchRow {
    vector_id: Uuid,
    workspace_id: Uuid,
    library_id: Uuid,
    entity_id: Uuid,
    embedding_model_key: String,
    vector_kind: String,
    freshness_generation: i64,
    score: f64,
}

impl PgSearchStore {
    async fn ensure_chunk_vector_relation(&self, dim: u64) -> anyhow::Result<String> {
        let relation_name = vector_relation_name(CHUNK_VECTOR_RELATION_PREFIX, dim)?;
        let relation = quote_identifier(&relation_name)?;
        let storage = PgVectorStorage::for_dim(dim);
        let dim = checked_dim_i32(dim)?;
        let embedding_type = storage.column_type(dim);
        sqlx::query(&format!(
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
        ))
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to create chunk vector relation {relation_name}"))?;
        self.ensure_vector_relation_indexes(
            &relation_name,
            "chunk_id",
            Some("revision_id"),
            storage,
            dim,
        )
        .await?;
        Ok(relation_name)
    }

    async fn ensure_entity_vector_relation(&self, dim: u64) -> anyhow::Result<String> {
        let relation_name = vector_relation_name(ENTITY_VECTOR_RELATION_PREFIX, dim)?;
        let relation = quote_identifier(&relation_name)?;
        let storage = PgVectorStorage::for_dim(dim);
        let dim = checked_dim_i32(dim)?;
        let embedding_type = storage.column_type(dim);
        sqlx::query(&format!(
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
        ))
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to create entity vector relation {relation_name}"))?;
        self.ensure_vector_relation_indexes(&relation_name, "entity_id", None, storage, dim)
            .await?;
        Ok(relation_name)
    }

    async fn ensure_vector_relation_indexes(
        &self,
        relation_name: &str,
        id_column: &str,
        extra_column: Option<&str>,
        storage: PgVectorStorage,
        dim: i32,
    ) -> anyhow::Result<()> {
        let relation = quote_identifier(relation_name)?;
        let lane_idx = quote_identifier(&format!("{relation_name}_lane_idx"))?;
        sqlx::query(&format!(
            "create index if not exists {lane_idx}
             on {relation} (library_id, embedding_model_key, vector_kind)"
        ))
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to create lane index on {relation_name}"))?;

        let id_idx = quote_identifier(&format!("{relation_name}_{id_column}_idx"))?;
        sqlx::query(&format!("create index if not exists {id_idx} on {relation} ({id_column})"))
            .execute(&self.pool)
            .await
            .with_context(|| format!("failed to create id index on {relation_name}"))?;

        if let Some(extra_column) = extra_column {
            let extra_idx = quote_identifier(&format!("{relation_name}_{extra_column}_idx"))?;
            sqlx::query(&format!(
                "create index if not exists {extra_idx} on {relation} ({extra_column})"
            ))
            .execute(&self.pool)
            .await
            .with_context(|| format!("failed to create extra index on {relation_name}"))?;
        }
        self.ensure_vector_relation_hnsw_index(relation_name, storage, dim).await?;
        Ok(())
    }

    async fn ensure_vector_relation_hnsw_index(
        &self,
        relation_name: &str,
        storage: PgVectorStorage,
        dim: i32,
    ) -> anyhow::Result<()> {
        let relation = quote_identifier(relation_name)?;
        let hnsw_idx = quote_identifier(&format!("{relation_name}_hnsw"))?;
        let row_count =
            sqlx::query_scalar::<_, i64>(&format!("select count(*)::bigint from {relation}"))
                .fetch_one(&self.pool)
                .await
                .with_context(|| {
                    format!("failed to count rows in {relation_name} for HNSW sizing")
                })?;
        let row_count = u64::try_from(row_count).context("negative vector shard row count")?;
        let params = pg_hnsw_index_params(row_count, dim, storage)?;
        let ops = storage.cosine_ops();
        sqlx::query(&format!(
            "create index if not exists {hnsw_idx}
             on {relation} using hnsw (embedding {ops})
             with (m = {m}, ef_construction = {ef_construction})",
            m = params.m,
            ef_construction = params.ef_construction
        ))
        .execute(&self.pool)
        .await
        .with_context(|| format!("failed to create HNSW index on {relation_name}"))?;
        Ok(())
    }

    async fn upsert_manifest(
        &self,
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
        .execute(&self.pool)
        .await
        .context("failed to upsert vector relation manifest")?;
        Ok(())
    }

    async fn resolve_manifest_relation(
        &self,
        library_id: Uuid,
        dim: u64,
        vector_kind: &str,
        embedding_model_key: &str,
        expected_prefix: &str,
    ) -> anyhow::Result<Option<String>> {
        let relation_name = sqlx::query_scalar::<_, String>(
            "select relation_name
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
        .context("failed to resolve vector relation manifest")?;
        if let Some(relation_name) = &relation_name {
            validate_relation_name(relation_name, expected_prefix)?;
        }
        Ok(relation_name)
    }

    async fn list_vector_relations(&self, expected_prefix: &str) -> anyhow::Result<Vec<String>> {
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

    async fn refresh_manifest_count(
        &self,
        relation_name: &str,
        library_id: Uuid,
        dim: i32,
        vector_kind: &str,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        let relation = quote_identifier(relation_name)?;
        let row_count = sqlx::query_scalar::<_, i64>(&format!(
            "select count(*)::bigint
             from {relation}
             where library_id = $1
               and vector_kind = $2
               and embedding_model_key = $3"
        ))
        .bind(library_id)
        .bind(vector_kind)
        .bind(embedding_model_key)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("failed to count rows in {relation_name}"))?;
        sqlx::query(
            "update knowledge_vector_relation_manifest
             set row_count = $5
             where library_id = $1
               and dim = $2
               and vector_kind = $3
               and embedding_model_key = $4",
        )
        .bind(library_id)
        .bind(dim)
        .bind(vector_kind)
        .bind(embedding_model_key)
        .bind(row_count)
        .execute(&self.pool)
        .await
        .context("failed to refresh manifest row_count")?;
        Ok(())
    }

    async fn upsert_chunk_vector_in_relation(
        &self,
        relation_name: &str,
        row: &KnowledgeChunkVectorRow,
    ) -> anyhow::Result<KnowledgeChunkVectorRow> {
        let relation = quote_identifier(relation_name)?;
        let vector_literal = pgvector_literal(&row.vector)?;
        let storage = PgVectorStorage::for_dim(u64::try_from(row.dimensions)?);
        let cast_type = storage.cast_type();
        let row = sqlx::query_as::<_, PgChunkVectorRow>(&format!(
            "insert into {relation} (
                key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                embedding_model_key, vector_kind, dimensions, embedding,
                freshness_generation, created_at, occurred_at, occurred_until
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::{cast_type}, $11, $12, $13, $14)
             on conflict (key) do update set
                workspace_id = excluded.workspace_id,
                library_id = excluded.library_id,
                chunk_id = excluded.chunk_id,
                revision_id = excluded.revision_id,
                embedding_model_key = excluded.embedding_model_key,
                vector_kind = excluded.vector_kind,
                dimensions = excluded.dimensions,
                embedding = excluded.embedding,
                freshness_generation = excluded.freshness_generation,
                occurred_at = excluded.occurred_at,
                occurred_until = excluded.occurred_until
             returning key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                freshness_generation, created_at, occurred_at, occurred_until"
        ))
        .bind(&row.key)
        .bind(row.vector_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.chunk_id)
        .bind(row.revision_id)
        .bind(&row.embedding_model_key)
        .bind(&row.vector_kind)
        .bind(row.dimensions)
        .bind(vector_literal)
        .bind(row.freshness_generation)
        .bind(row.created_at)
        .bind(row.occurred_at)
        .bind(row.occurred_until)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("failed to upsert chunk vector into {relation_name}"))?;
        chunk_vector_from_pg(row)
    }

    async fn upsert_entity_vector_in_relation(
        &self,
        relation_name: &str,
        row: &KnowledgeEntityVectorRow,
    ) -> anyhow::Result<KnowledgeEntityVectorRow> {
        let relation = quote_identifier(relation_name)?;
        let vector_literal = pgvector_literal(&row.vector)?;
        let storage = PgVectorStorage::for_dim(u64::try_from(row.dimensions)?);
        let cast_type = storage.cast_type();
        let row = sqlx::query_as::<_, PgEntityVectorRow>(&format!(
            "insert into {relation} (
                key, vector_id, workspace_id, library_id, entity_id, embedding_model_key,
                vector_kind, dimensions, embedding, freshness_generation, created_at
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9::{cast_type}, $10, $11)
             on conflict (key) do update set
                workspace_id = excluded.workspace_id,
                library_id = excluded.library_id,
                entity_id = excluded.entity_id,
                embedding_model_key = excluded.embedding_model_key,
                vector_kind = excluded.vector_kind,
                dimensions = excluded.dimensions,
                embedding = excluded.embedding,
                freshness_generation = excluded.freshness_generation
             returning key, vector_id, workspace_id, library_id, entity_id,
                embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                freshness_generation, created_at"
        ))
        .bind(&row.key)
        .bind(row.vector_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.entity_id)
        .bind(&row.embedding_model_key)
        .bind(&row.vector_kind)
        .bind(row.dimensions)
        .bind(vector_literal)
        .bind(row.freshness_generation)
        .bind(row.created_at)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("failed to upsert entity vector into {relation_name}"))?;
        entity_vector_from_pg(row)
    }

    /// Runs a single rung of the chunk lexical ladder.
    ///
    /// `sql` is one of the [`CHUNK_LEXICAL_SQL_EXACT`] / [`CHUNK_LEXICAL_SQL_PREFIX`]
    /// templates (identical except for the FTS constructor literal). `fts_input`
    /// is bound as `$2`: the raw user query for the exact pass, or the prebuilt
    /// prefix tsquery string for a relaxed pass. Every other bound parameter is
    /// identical across passes, so the title-aware scoring is preserved while
    /// only the FTS lane widens.
    #[allow(clippy::too_many_arguments)]
    async fn run_chunk_lexical_pass(
        &self,
        sql: &str,
        library_id: Uuid,
        fts_input: &str,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        query_terms: &[String],
        title_identity_terms: &[String],
        title_ngram_terms: &[String],
        title_soft_raw_enabled: bool,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        let rows = sqlx::query_as::<_, PgChunkSearchRow>(sql)
            .bind(library_id)
            .bind(fts_input)
            .bind(limit.max(1).saturating_mul(4).max(48) as i64)
            .bind(temporal_start)
            .bind(temporal_end)
            .bind(query_terms)
            .bind(title_identity_terms)
            .bind(title_ngram_terms)
            .bind(title_soft_raw_enabled)
            .bind(limit.max(1) as i64)
            .fetch_all(&self.pool)
            .await
            .context("failed to search knowledge chunks")?;
        Ok(rows
            .into_iter()
            .map(|row| KnowledgeChunkSearchRow {
                chunk_id: row.chunk_id,
                workspace_id: row.workspace_id,
                library_id: row.library_id,
                revision_id: row.revision_id,
                content_text: row.content_text,
                normalized_text: row.normalized_text,
                section_path: row.section_path,
                heading_trail: row.heading_trail,
                score: row.score,
                quality_score: row.quality_score,
            })
            .collect())
    }
}

#[async_trait]
impl SearchStore for PgSearchStore {
    async fn ensure_chunk_vector_shard(&self, dim: u64) -> anyhow::Result<()> {
        self.ensure_chunk_vector_relation(dim).await?;
        Ok(())
    }

    async fn ensure_chunk_vector_shard_for_library(
        &self,
        dim: u64,
        _library_id: Uuid,
    ) -> anyhow::Result<()> {
        self.ensure_chunk_vector_relation(dim).await?;
        Ok(())
    }

    async fn ensure_entity_vector_shard(&self, dim: u64) -> anyhow::Result<()> {
        self.ensure_entity_vector_relation(dim).await?;
        Ok(())
    }

    async fn upsert_chunk_vector(
        &self,
        row: &KnowledgeChunkVectorRow,
    ) -> anyhow::Result<KnowledgeChunkVectorRow> {
        let dim = validate_row_vector_dimensions(row.dimensions, &row.vector, "chunk")?;
        let relation_name = self.ensure_chunk_vector_relation(dim).await?;
        self.upsert_manifest(
            row.library_id,
            dim,
            &row.vector_kind,
            &row.embedding_model_key,
            &relation_name,
        )
        .await?;
        let stored = self.upsert_chunk_vector_in_relation(&relation_name, row).await?;
        self.refresh_manifest_count(
            &relation_name,
            row.library_id,
            row.dimensions,
            &row.vector_kind,
            &row.embedding_model_key,
        )
        .await?;
        Ok(stored)
    }

    async fn upsert_chunk_vectors_bulk(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()> {
        let mut groups: BTreeMap<(u64, Uuid, String, String), Vec<&KnowledgeChunkVectorRow>> =
            BTreeMap::new();
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
            self.upsert_manifest(
                library_id,
                dim,
                &vector_kind,
                &embedding_model_key,
                &relation_name,
            )
            .await?;
            for row in rows {
                self.upsert_chunk_vector_in_relation(&relation_name, row).await?;
            }
            self.refresh_manifest_count(
                &relation_name,
                library_id,
                checked_dim_i32(dim)?,
                &vector_kind,
                &embedding_model_key,
            )
            .await?;
        }
        Ok(())
    }

    async fn delete_chunk_vector(
        &self,
        chunk_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<Option<KnowledgeChunkVectorRow>> {
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let row = sqlx::query_as::<_, PgChunkVectorRow>(&format!(
                "delete from {relation}
                 where key in (
                    select key from {relation}
                    where chunk_id = $1
                      and embedding_model_key = $2
                      and freshness_generation = $3
                    order by freshness_generation desc, created_at desc
                    limit 1
                 )
                 returning key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at, occurred_at, occurred_until"
            ))
            .bind(chunk_id)
            .bind(embedding_model_key)
            .bind(freshness_generation)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("failed to delete chunk vector from {relation_name}"))?;
            if let Some(row) = row {
                return chunk_vector_from_pg(row).map(Some);
            }
        }
        Ok(None)
    }

    async fn delete_chunk_vectors_by_revision(&self, revision_id: Uuid) -> anyhow::Result<u64> {
        delete_from_vector_relations(
            &self.pool,
            &self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await?,
            "revision_id = $1",
            revision_id,
        )
        .await
    }

    async fn delete_chunk_vectors_by_library(&self, library_id: Uuid) -> anyhow::Result<u64> {
        let total = delete_from_vector_relations(
            &self.pool,
            &self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await?,
            "library_id = $1",
            library_id,
        )
        .await?;
        reset_manifest_counts(&self.pool, Some(library_id), CHUNK_VECTOR_RELATION_PREFIX).await?;
        Ok(total)
    }

    async fn delete_library_vectors_except_dim(
        &self,
        library_id: Uuid,
        keep_dim: u64,
    ) -> anyhow::Result<u64> {
        let keep_dim = checked_dim_i32(keep_dim)?;
        let chunk_total = delete_library_vectors_except_dim_from_manifest(
            &self.pool,
            library_id,
            keep_dim,
            CHUNK_VECTOR_RELATION_PREFIX,
        )
        .await?;
        let entity_total = delete_library_vectors_except_dim_from_manifest(
            &self.pool,
            library_id,
            keep_dim,
            ENTITY_VECTOR_RELATION_PREFIX,
        )
        .await?;
        chunk_total.checked_add(entity_total).context("deleted vector count overflowed u64")
    }

    async fn delete_all_chunk_vectors(&self) -> anyhow::Result<u64> {
        let mut total = 0_u64;
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let result = sqlx::query(&format!("delete from {relation}"))
                .execute(&self.pool)
                .await
                .with_context(|| format!("failed to delete rows from {relation_name}"))?;
            total = total
                .checked_add(result.rows_affected())
                .context("deleted chunk vector count overflowed u64")?;
        }
        reset_manifest_counts(&self.pool, None, CHUNK_VECTOR_RELATION_PREFIX).await?;
        Ok(total)
    }

    async fn list_chunk_vectors_by_chunk(
        &self,
        chunk_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        let mut rows = Vec::new();
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let pg_rows = sqlx::query_as::<_, PgChunkVectorRow>(&format!(
                "select key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at, occurred_at, occurred_until
                 from {relation}
                 where chunk_id = $1
                 order by freshness_generation desc, created_at desc"
            ))
            .bind(chunk_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to list chunk vectors from {relation_name}"))?;
            for row in pg_rows {
                rows.push(chunk_vector_from_pg(row)?);
            }
        }
        rows.sort_by(|left, right| {
            right
                .freshness_generation
                .cmp(&left.freshness_generation)
                .then_with(|| right.created_at.cmp(&left.created_at))
        });
        Ok(rows)
    }

    async fn list_chunk_vectors_by_chunks(
        &self,
        chunk_ids: &[Uuid],
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut rows = Vec::new();
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let pg_rows = sqlx::query_as::<_, PgChunkVectorRow>(&format!(
                "select key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at, occurred_at, occurred_until
                 from {relation}
                 where chunk_id = any($1::uuid[])
                   and embedding_model_key = $2
                   and vector_kind = $3
                 order by chunk_id asc, freshness_generation desc, created_at desc"
            ))
            .bind(chunk_ids)
            .bind(embedding_model_key)
            .bind(vector_kind)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to list chunk vectors from {relation_name}"))?;
            for row in pg_rows {
                rows.push(chunk_vector_from_pg(row)?);
            }
        }
        rows.sort_by(|left, right| {
            left.chunk_id
                .cmp(&right.chunk_id)
                .then_with(|| right.freshness_generation.cmp(&left.freshness_generation))
                .then_with(|| right.created_at.cmp(&left.created_at))
        });
        Ok(rows)
    }

    async fn count_chunk_vectors_by_revision(
        &self,
        revision_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<usize> {
        let mut total = 0_i64;
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let count = sqlx::query_scalar::<_, i64>(&format!(
                "select count(*)::bigint
                 from {relation}
                 where revision_id = $1
                   and embedding_model_key = $2
                   and vector_kind = $3
                   and freshness_generation = $4"
            ))
            .bind(revision_id)
            .bind(embedding_model_key)
            .bind(vector_kind)
            .bind(freshness_generation)
            .fetch_one(&self.pool)
            .await
            .with_context(|| format!("failed to count chunk vectors in {relation_name}"))?;
            total = total.checked_add(count).context("chunk vector count overflowed i64")?;
        }
        usize::try_from(total).context("chunk vector count overflowed usize")
    }

    async fn upsert_entity_vector(
        &self,
        row: &KnowledgeEntityVectorRow,
    ) -> anyhow::Result<KnowledgeEntityVectorRow> {
        let dim = validate_row_vector_dimensions(row.dimensions, &row.vector, "entity")?;
        let relation_name = self.ensure_entity_vector_relation(dim).await?;
        self.upsert_manifest(
            row.library_id,
            dim,
            &row.vector_kind,
            &row.embedding_model_key,
            &relation_name,
        )
        .await?;
        let stored = self.upsert_entity_vector_in_relation(&relation_name, row).await?;
        self.refresh_manifest_count(
            &relation_name,
            row.library_id,
            row.dimensions,
            &row.vector_kind,
            &row.embedding_model_key,
        )
        .await?;
        Ok(stored)
    }

    async fn delete_entity_vector(
        &self,
        entity_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<Option<KnowledgeEntityVectorRow>> {
        for relation_name in self.list_vector_relations(ENTITY_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let row = sqlx::query_as::<_, PgEntityVectorRow>(&format!(
                "delete from {relation}
                 where key in (
                    select key from {relation}
                    where entity_id = $1
                      and embedding_model_key = $2
                      and freshness_generation = $3
                    order by freshness_generation desc, created_at desc
                    limit 1
                 )
                 returning key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at"
            ))
            .bind(entity_id)
            .bind(embedding_model_key)
            .bind(freshness_generation)
            .fetch_optional(&self.pool)
            .await
            .with_context(|| format!("failed to delete entity vector from {relation_name}"))?;
            if let Some(row) = row {
                return entity_vector_from_pg(row).map(Some);
            }
        }
        Ok(None)
    }

    async fn delete_entity_vectors_by_library(&self, library_id: Uuid) -> anyhow::Result<()> {
        delete_from_vector_relations(
            &self.pool,
            &self.list_vector_relations(ENTITY_VECTOR_RELATION_PREFIX).await?,
            "library_id = $1",
            library_id,
        )
        .await?;
        reset_manifest_counts(&self.pool, Some(library_id), ENTITY_VECTOR_RELATION_PREFIX).await?;
        Ok(())
    }

    async fn delete_all_entity_vectors(&self) -> anyhow::Result<u64> {
        let mut total = 0_u64;
        for relation_name in self.list_vector_relations(ENTITY_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let result = sqlx::query(&format!("delete from {relation}"))
                .execute(&self.pool)
                .await
                .with_context(|| format!("failed to delete rows from {relation_name}"))?;
            total = total
                .checked_add(result.rows_affected())
                .context("deleted entity vector count overflowed u64")?;
        }
        reset_manifest_counts(&self.pool, None, ENTITY_VECTOR_RELATION_PREFIX).await?;
        Ok(total)
    }

    async fn list_entity_vectors_by_entity(
        &self,
        entity_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorRow>> {
        let mut rows = Vec::new();
        for relation_name in self.list_vector_relations(ENTITY_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let pg_rows = sqlx::query_as::<_, PgEntityVectorRow>(&format!(
                "select key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at
                 from {relation}
                 where entity_id = $1
                 order by freshness_generation desc, created_at desc
                 limit 1000"
            ))
            .bind(entity_id)
            .fetch_all(&self.pool)
            .await
            .with_context(|| format!("failed to list entity vectors from {relation_name}"))?;
            for row in pg_rows {
                rows.push(entity_vector_from_pg(row)?);
            }
        }
        rows.sort_by(|left, right| {
            right
                .freshness_generation
                .cmp(&left.freshness_generation)
                .then_with(|| right.created_at.cmp(&left.created_at))
        });
        rows.truncate(1000);
        Ok(rows)
    }

    async fn search_chunks(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let query_terms = lexical_query_terms(query);
        let title_ngram_terms = title_ngram_terms(&query_terms);
        let title_identity_terms = title_identity_terms(query, &query_terms);
        let title_soft_raw_enabled = title_soft_raw_enabled(&query_terms);
        // Borrow the FTS-independent scoring params once, so the per-pass closure
        // (called for both Pass A and the relaxed passes) only move-captures
        // copyable references, not the owning vectors.
        let query_terms = query_terms.as_slice();
        let title_ngram_terms = title_ngram_terms.as_slice();
        let title_identity_terms = title_identity_terms.as_slice();
        // Lexical relaxation ladder: precise exact-AND `websearch_to_tsquery`
        // first, relaxed only when sparse. The title-aware scoring params are
        // identical across passes; only the FTS lane (`$2`) widens.
        run_lexical_ladder(
            query,
            CHUNK_LEXICAL_SQL_EXACT.as_str(),
            CHUNK_LEXICAL_SQL_PREFIX.as_str(),
            |row: &KnowledgeChunkSearchRow| row.chunk_id,
            |sql, fts_input| async move {
                self.run_chunk_lexical_pass(
                    &sql,
                    library_id,
                    &fts_input,
                    limit,
                    temporal_start,
                    temporal_end,
                    query_terms,
                    title_identity_terms,
                    title_ngram_terms,
                    title_soft_raw_enabled,
                )
                .await
            },
        )
        .await
    }

    async fn search_structured_blocks(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockSearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        // PARITY-TODO(0.5.0): .omc/research/adapter-review/search.md:12 -
        // replace the single simple FTS lane with analyzer-equivalent EN/RU parity columns or queries.
        let (exact_sql, prefix_sql) = lexical_lane_sql(
            "select block_id, document_id, workspace_id, library_id, revision_id, ordinal,
                block_kind, text, normalized_text, section_path, heading_trail,
                ts_rank_cd(search_tsv, {FTS})::double precision as score
             from knowledge_structured_block
             where library_id = $1
               and search_tsv @@ {FTS}
             order by score desc, revision_id desc, ordinal asc, block_id asc
             limit $3",
        );
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeStructuredBlockSearchRow| row.block_id,
            |sql, fts_input| async move {
                let rows = sqlx::query_as::<_, PgStructuredBlockSearchRow>(&sql)
                    .bind(library_id)
                    .bind(fts_input)
                    .bind(limit.max(1) as i64)
                    .fetch_all(&self.pool)
                    .await
                    .context("failed to search structured blocks")?;
                Ok(rows
                    .into_iter()
                    .map(|row| KnowledgeStructuredBlockSearchRow {
                        block_id: row.block_id,
                        document_id: row.document_id,
                        workspace_id: row.workspace_id,
                        library_id: row.library_id,
                        revision_id: row.revision_id,
                        ordinal: row.ordinal,
                        block_kind: row.block_kind,
                        text: row.text,
                        normalized_text: row.normalized_text,
                        section_path: row.section_path,
                        heading_trail: row.heading_trail,
                        score: row.score,
                    })
                    .collect())
            },
        )
        .await
    }

    async fn search_technical_facts(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactSearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        // PARITY-TODO(0.5.0): .omc/research/adapter-review/search.md:12 -
        // replace the single simple FTS lane with analyzer-equivalent EN/RU parity columns or queries.
        let query_exact = query.split_whitespace().collect::<String>();
        // `$3` (exact canonical value) and `$4` (limit) are FTS-independent, so
        // they bind identically across passes; only the FTS lane (`$2`) widens.
        let (exact_sql, prefix_sql) = lexical_lane_sql(
            "select fact_id, document_id, workspace_id, library_id, revision_id, fact_kind,
                canonical_value_text, display_value,
                (canonical_value_exact = $3) as exact_match,
                (
                    case when canonical_value_exact = $3 then 1000000.0 else 0.0 end
                    + ts_rank_cd(search_tsv, {FTS})::double precision
                ) as score
             from knowledge_technical_fact
             where library_id = $1
               and (
                    canonical_value_exact = $3
                    or search_tsv @@ {FTS}
               )
             order by score desc, fact_id asc
             limit $4",
        );
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeTechnicalFactSearchRow| row.fact_id,
            |sql, fts_input| {
                let query_exact = query_exact.clone();
                async move {
                    let rows = sqlx::query_as::<_, PgTechnicalFactSearchRow>(&sql)
                        .bind(library_id)
                        .bind(fts_input)
                        .bind(query_exact)
                        .bind(limit.max(1) as i64)
                        .fetch_all(&self.pool)
                        .await
                        .context("failed to search technical facts")?;
                    Ok(rows
                        .into_iter()
                        .map(|row| KnowledgeTechnicalFactSearchRow {
                            fact_id: row.fact_id,
                            document_id: row.document_id,
                            workspace_id: row.workspace_id,
                            library_id: row.library_id,
                            revision_id: row.revision_id,
                            fact_kind: row.fact_kind,
                            canonical_value_text: row.canonical_value_text,
                            display_value: row.display_value,
                            exact_match: row.exact_match,
                            score: row.score,
                        })
                        .collect())
                }
            },
        )
        .await
    }

    async fn search_entities(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeEntitySearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        // PARITY-TODO(0.5.0): .omc/research/adapter-review/search.md:12 -
        // replace the single simple FTS lane with analyzer-equivalent EN/RU parity columns or queries.
        let (exact_sql, prefix_sql) = lexical_lane_sql(
            "select e.entity_id, e.workspace_id, e.library_id, e.canonical_label, e.entity_type, e.summary,
                ts_rank_cd(e.search_tsv, {FTS})::double precision as score
             from knowledge_entity e
             where e.library_id = $1
               and e.entity_state = 'active'
               and e.search_tsv @@ {FTS}
             order by score desc, e.entity_id asc
             limit $3",
        );
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeEntitySearchRow| row.entity_id,
            |sql, fts_input| async move {
                let rows = sqlx::query_as::<_, PgEntitySearchRow>(&sql)
                    .bind(library_id)
                    .bind(fts_input)
                    .bind(limit.max(1) as i64)
                    .fetch_all(&self.pool)
                    .await
                    .context("failed to search knowledge entities")?;
                Ok(rows
                    .into_iter()
                    .map(|row| KnowledgeEntitySearchRow {
                        entity_id: row.entity_id,
                        workspace_id: row.workspace_id,
                        library_id: row.library_id,
                        canonical_label: row.canonical_label,
                        entity_type: row.entity_type,
                        summary: row.summary,
                        score: row.score,
                    })
                    .collect())
            },
        )
        .await
    }

    async fn search_relations(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeRelationSearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        // PARITY-TODO(0.5.0): .omc/research/adapter-review/search.md:12 -
        // replace the single simple FTS lane with analyzer-equivalent EN/RU parity columns or queries.
        let (exact_sql, prefix_sql) = lexical_lane_sql(
            "select relation_id, workspace_id, library_id, predicate, normalized_assertion, summary,
                ts_rank_cd(search_tsv, {FTS})::double precision as score
             from knowledge_relation
             where library_id = $1
               and relation_state = 'active'
               and search_tsv @@ {FTS}
             order by score desc, relation_id asc
             limit $3",
        );
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeRelationSearchRow| row.relation_id,
            |sql, fts_input| async move {
                let rows = sqlx::query_as::<_, PgRelationSearchRow>(&sql)
                    .bind(library_id)
                    .bind(fts_input)
                    .bind(limit.max(1) as i64)
                    .fetch_all(&self.pool)
                    .await
                    .context("failed to search knowledge relations")?;
                Ok(rows
                    .into_iter()
                    .map(|row| KnowledgeRelationSearchRow {
                        relation_id: row.relation_id,
                        workspace_id: row.workspace_id,
                        library_id: row.library_id,
                        predicate: row.predicate,
                        normalized_assertion: row.normalized_assertion,
                        summary: row.summary,
                        score: row.score,
                    })
                    .collect())
            },
        )
        .await
    }

    async fn search_chunk_vectors_by_similarity(
        &self,
        dim: u64,
        library_id: Uuid,
        embedding_model_key: &str,
        query_vector: &[f32],
        limit: usize,
        n_probe: Option<u64>,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Vec<KnowledgeChunkVectorSearchRow>> {
        validate_query_vector_dimensions(dim, query_vector, "chunk")?;
        let Some(relation_name) = self
            .resolve_manifest_relation(
                library_id,
                dim,
                KNOWLEDGE_CHUNK_VECTOR_KIND,
                embedding_model_key,
                CHUNK_VECTOR_RELATION_PREFIX,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let relation = quote_identifier(&relation_name)?;
        let query_literal = pgvector_literal(query_vector)?;
        let storage = PgVectorStorage::for_dim(dim);
        let cast_type = storage.cast_type();
        let ef_search = pg_hnsw_ef_search(n_probe);
        let mut tx = self.pool.begin().await?;
        sqlx::query(&format!("set local hnsw.ef_search = {ef_search}")).execute(&mut *tx).await?;
        let rows = sqlx::query_as::<_, PgChunkVectorSearchRow>(&format!(
            "select vector_id, workspace_id, library_id, chunk_id, revision_id,
                embedding_model_key, vector_kind, freshness_generation,
                (1.0 - (embedding <=> $3::{cast_type}))::double precision as score
             from {relation}
             where library_id = $1
               and embedding_model_key = $2
               and vector_kind = $6
               and (($4::timestamptz is null and $5::timestamptz is null)
                    or (occurred_at is not null
                        and ($4::timestamptz is null or coalesce(occurred_until, occurred_at) >= $4)
                        and ($5::timestamptz is null or occurred_at <= $5)))
             order by embedding <=> $3::{cast_type}, chunk_id asc
             limit $7"
        ))
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(query_literal)
        .bind(temporal_start)
        .bind(temporal_end)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(limit.max(1) as i64)
        .fetch_all(&mut *tx)
        .await
        .with_context(|| format!("failed to search chunk vectors in {relation_name}"))?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|row| KnowledgeChunkVectorSearchRow {
                vector_id: row.vector_id,
                workspace_id: row.workspace_id,
                library_id: row.library_id,
                chunk_id: row.chunk_id,
                revision_id: row.revision_id,
                embedding_model_key: row.embedding_model_key,
                vector_kind: row.vector_kind,
                freshness_generation: row.freshness_generation,
                score: row.score,
            })
            .collect())
    }

    async fn search_entity_vectors_by_similarity(
        &self,
        dim: u64,
        library_id: Uuid,
        embedding_model_key: &str,
        query_vector: &[f32],
        limit: usize,
        n_probe: Option<u64>,
    ) -> anyhow::Result<Vec<KnowledgeEntityVectorSearchRow>> {
        validate_query_vector_dimensions(dim, query_vector, "entity")?;
        let Some(relation_name) = self
            .resolve_manifest_relation(
                library_id,
                dim,
                KNOWLEDGE_ENTITY_VECTOR_KIND,
                embedding_model_key,
                ENTITY_VECTOR_RELATION_PREFIX,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let relation = quote_identifier(&relation_name)?;
        let query_literal = pgvector_literal(query_vector)?;
        let storage = PgVectorStorage::for_dim(dim);
        let cast_type = storage.cast_type();
        let ef_search = pg_hnsw_ef_search(n_probe);
        let mut tx = self.pool.begin().await?;
        sqlx::query(&format!("set local hnsw.ef_search = {ef_search}")).execute(&mut *tx).await?;
        let rows = sqlx::query_as::<_, PgEntityVectorSearchRow>(&format!(
            "select vector_id, workspace_id, library_id, entity_id,
                embedding_model_key, vector_kind, freshness_generation,
                (1.0 - (embedding <=> $3::{cast_type}))::double precision as score
             from {relation}
             where library_id = $1
               and embedding_model_key = $2
               and vector_kind = $4
             order by embedding <=> $3::{cast_type}, entity_id asc
             limit $5"
        ))
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(query_literal)
        .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
        .bind(limit.max(1) as i64)
        .fetch_all(&mut *tx)
        .await
        .with_context(|| format!("failed to search entity vectors in {relation_name}"))?;
        tx.commit().await?;
        Ok(rows
            .into_iter()
            .map(|row| KnowledgeEntityVectorSearchRow {
                vector_id: row.vector_id,
                workspace_id: row.workspace_id,
                library_id: row.library_id,
                entity_id: row.entity_id,
                embedding_model_key: row.embedding_model_key,
                vector_kind: row.vector_kind,
                freshness_generation: row.freshness_generation,
                score: row.score,
            })
            .collect())
    }
}

async fn delete_from_vector_relations(
    pool: &PgPool,
    relation_names: &[String],
    predicate: &str,
    id: Uuid,
) -> anyhow::Result<u64> {
    let mut total = 0_u64;
    for relation_name in relation_names {
        let relation = quote_identifier(relation_name)?;
        let result = sqlx::query(&format!("delete from {relation} where {predicate}"))
            .bind(id)
            .execute(pool)
            .await
            .with_context(|| format!("failed to delete rows from {relation_name}"))?;
        total = total
            .checked_add(result.rows_affected())
            .context("deleted vector count overflowed u64")?;
    }
    Ok(total)
}

async fn delete_library_vectors_except_dim_from_manifest(
    pool: &PgPool,
    library_id: Uuid,
    keep_dim: i32,
    relation_prefix: &str,
) -> anyhow::Result<u64> {
    let relation_names = sqlx::query_scalar::<_, String>(
        "select distinct relation_name
         from knowledge_vector_relation_manifest
         where library_id = $1
           and dim <> $2
           and relation_name like $3
         order by relation_name",
    )
    .bind(library_id)
    .bind(keep_dim)
    .bind(format!("{relation_prefix}%"))
    .fetch_all(pool)
    .await
    .context("failed to list non-target vector relations")?;
    let relation_names = relation_names
        .into_iter()
        .map(|relation_name| {
            validate_relation_name(&relation_name, relation_prefix)?;
            Ok(relation_name)
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let total =
        delete_from_vector_relations(pool, &relation_names, "library_id = $1", library_id).await?;
    sqlx::query(
        "update knowledge_vector_relation_manifest
         set row_count = 0
         where library_id = $1
           and dim <> $2
           and relation_name like $3",
    )
    .bind(library_id)
    .bind(keep_dim)
    .bind(format!("{relation_prefix}%"))
    .execute(pool)
    .await
    .context("failed to reset non-target vector manifest row counts")?;
    Ok(total)
}

async fn reset_manifest_counts(
    pool: &PgPool,
    library_id: Option<Uuid>,
    relation_prefix: &str,
) -> anyhow::Result<()> {
    let mut query = sqlx::query(
        "update knowledge_vector_relation_manifest
         set row_count = 0
         where relation_name like $1
           and ($2::uuid is null or library_id = $2)",
    )
    .bind(format!("{relation_prefix}%"));
    query = query.bind(library_id);
    query.execute(pool).await.context("failed to reset manifest row counts")?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum PgVectorStorage {
    Vector,
    Halfvec,
}

impl PgVectorStorage {
    fn for_dim(dim: u64) -> Self {
        if dim > PGVECTOR_HNSW_VECTOR_MAX_DIM { Self::Halfvec } else { Self::Vector }
    }

    fn column_type(self, dim: i32) -> String {
        match self {
            Self::Vector => format!("vector({dim})"),
            Self::Halfvec => format!("halfvec({dim})"),
        }
    }

    fn cast_type(self) -> &'static str {
        match self {
            Self::Vector => "vector",
            Self::Halfvec => "halfvec",
        }
    }

    fn cosine_ops(self) -> &'static str {
        match self {
            Self::Vector => "vector_cosine_ops",
            Self::Halfvec => "halfvec_cosine_ops",
        }
    }

    fn bytes_per_component(self) -> u64 {
        match self {
            Self::Vector => 4,
            Self::Halfvec => 2,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PgHnswIndexParams {
    m: u64,
    ef_construction: u64,
}

fn pg_hnsw_index_params(
    row_count: u64,
    dim: i32,
    storage: PgVectorStorage,
) -> anyhow::Result<PgHnswIndexParams> {
    let dim = u64::try_from(dim).context("vector dimension must be positive")?;
    let configured_m = read_env_u64("IRONRAG_PG_HNSW_M");
    let configured_ef_construction = read_env_u64("IRONRAG_PG_HNSW_EF_CONSTRUCTION");
    let m = configured_m
        .map(|m| m.clamp(PG_HNSW_MIN_M, PG_HNSW_LARGE_M))
        .unwrap_or_else(|| memory_safe_hnsw_m(row_count, dim, storage));
    let ef_construction = configured_ef_construction.unwrap_or(m.saturating_mul(4)).max(m);
    Ok(PgHnswIndexParams { m, ef_construction })
}

fn memory_safe_hnsw_m(row_count: u64, dim: u64, storage: PgVectorStorage) -> u64 {
    let target = if row_count >= 100_000 {
        PG_HNSW_LARGE_M
    } else if row_count >= 1_000 {
        PG_HNSW_MID_M
    } else {
        PG_HNSW_MIN_M
    };
    let budget = pg_hnsw_build_budget_bytes();
    [target, PG_HNSW_MID_M, PG_HNSW_MIN_M]
        .into_iter()
        .find(|&m| estimated_hnsw_build_bytes(row_count, dim, storage, m) <= budget)
        .unwrap_or(PG_HNSW_MIN_M)
}

fn estimated_hnsw_build_bytes(row_count: u64, dim: u64, storage: PgVectorStorage, m: u64) -> u128 {
    let rows = u128::from(row_count.max(1));
    let vector_bytes = u128::from(dim) * u128::from(storage.bytes_per_component());
    let graph_bytes = u128::from(m) * 16;
    rows * (vector_bytes.saturating_mul(2) + graph_bytes)
}

fn pg_hnsw_build_budget_bytes() -> u128 {
    u128::from(
        read_env_u64("IRONRAG_PG_HNSW_BUILD_BUDGET_BYTES")
            .or_else(|| read_env_u64("IRONRAG_VECTOR_INDEX_TRAINING_BUDGET_BYTES"))
            .unwrap_or(PG_HNSW_DEFAULT_BUILD_BUDGET_BYTES),
    )
}

fn pg_hnsw_ef_search(n_probe: Option<u64>) -> u64 {
    n_probe
        .or_else(|| read_env_u64("IRONRAG_PG_HNSW_EF_SEARCH"))
        .unwrap_or(PG_HNSW_DEFAULT_EF_SEARCH)
        .clamp(1, 10_000)
}

fn read_env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            trimmed.parse::<u64>().ok().filter(|value| *value > 0)
        }
    })
}

fn checked_dim_i32(dim: u64) -> anyhow::Result<i32> {
    anyhow::ensure!(dim > 0, "vector dimension must be positive");
    i32::try_from(dim).context("vector dimension overflowed i32")
}

fn validate_row_vector_dimensions(
    row_dim: i32,
    vector: &[f32],
    label: &str,
) -> anyhow::Result<u64> {
    anyhow::ensure!(!vector.is_empty(), "{label} vector must not be empty");
    anyhow::ensure!(row_dim > 0, "{label} vector dimensions must be positive");
    let vector_dim = u64::try_from(vector.len()).context("vector length overflowed u64")?;
    anyhow::ensure!(
        i32::try_from(vector_dim).ok() == Some(row_dim),
        "{label} vector dimension mismatch: row has {row_dim}, vector has {vector_dim}"
    );
    Ok(vector_dim)
}

fn validate_query_vector_dimensions(dim: u64, vector: &[f32], label: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!vector.is_empty(), "{label} query vector must not be empty");
    let vector_dim = u64::try_from(vector.len()).context("query vector length overflowed u64")?;
    anyhow::ensure!(
        dim == vector_dim,
        "{label} query vector dimension mismatch: requested {dim}, vector has {vector_dim}"
    );
    checked_dim_i32(dim)?;
    Ok(())
}

fn vector_relation_name(prefix: &str, dim: u64) -> anyhow::Result<String> {
    checked_dim_i32(dim)?;
    Ok(format!("{prefix}{dim}"))
}

fn validate_relation_name(relation_name: &str, expected_prefix: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        relation_name.starts_with(expected_prefix),
        "manifest relation {relation_name} does not match expected prefix {expected_prefix}"
    );
    quote_identifier(relation_name)?;
    Ok(())
}

fn quote_identifier(identifier: &str) -> anyhow::Result<String> {
    anyhow::ensure!(!identifier.is_empty(), "empty SQL identifier");
    anyhow::ensure!(
        identifier.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_'),
        "unsafe SQL identifier {identifier}"
    );
    anyhow::ensure!(
        identifier.chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_'),
        "SQL identifier must start with a letter or underscore: {identifier}"
    );
    Ok(format!("\"{}\"", identifier.replace('"', "\"\"")))
}

fn pgvector_literal(vector: &[f32]) -> anyhow::Result<String> {
    anyhow::ensure!(!vector.is_empty(), "vector literal must not be empty");
    let mut out = String::from("[");
    for (idx, value) in vector.iter().enumerate() {
        anyhow::ensure!(value.is_finite(), "vector literal contains non-finite value");
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&value.to_string());
    }
    out.push(']');
    Ok(out)
}

fn parse_pgvector_text(value: &str) -> anyhow::Result<Vec<f32>> {
    let trimmed = value.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    trimmed
        .split(',')
        .map(|part| {
            part.trim()
                .parse::<f32>()
                .with_context(|| format!("failed to parse pgvector component {part:?}"))
        })
        .collect()
}

fn chunk_vector_from_pg(row: PgChunkVectorRow) -> anyhow::Result<KnowledgeChunkVectorRow> {
    Ok(KnowledgeChunkVectorRow {
        key: row.key,
        arango_id: None,
        arango_rev: None,
        vector_id: row.vector_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        chunk_id: row.chunk_id,
        revision_id: row.revision_id,
        embedding_model_key: row.embedding_model_key,
        vector_kind: row.vector_kind,
        dimensions: row.dimensions,
        vector: parse_pgvector_text(&row.vector_text)?,
        freshness_generation: row.freshness_generation,
        created_at: row.created_at,
        occurred_at: row.occurred_at,
        occurred_until: row.occurred_until,
    })
}

fn entity_vector_from_pg(row: PgEntityVectorRow) -> anyhow::Result<KnowledgeEntityVectorRow> {
    Ok(KnowledgeEntityVectorRow {
        key: row.key,
        arango_id: None,
        arango_rev: None,
        vector_id: row.vector_id,
        workspace_id: row.workspace_id,
        library_id: row.library_id,
        entity_id: row.entity_id,
        embedding_model_key: row.embedding_model_key,
        vector_kind: row.vector_kind,
        dimensions: row.dimensions,
        vector: parse_pgvector_text(&row.vector_text)?,
        freshness_generation: row.freshness_generation,
        created_at: row.created_at,
    })
}

/// Raw structural lexical tokens of a query: split on any character that is not
/// alphanumeric, `_`, or `/`, trimmed, non-empty. Single source of truth for the
/// lexical tokenizers — the precise terms, the relaxed prefix lexemes, and the
/// short acronym exact lexemes all derive from this same split rule, so the lanes
/// can never drift apart on tokenization. (`/` is kept inside a token so path-like
/// identifiers such as `v2/payments` survive as one unit.)
fn raw_lexical_tokens(query: &str) -> impl Iterator<Item = &str> {
    query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| !token.is_empty())
}

fn lexical_query_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = BTreeSet::new();
    for token in
        raw_lexical_tokens(query).filter(|token| token.chars().count() >= 3).map(str::to_lowercase)
    {
        if seen.insert(token.clone()) {
            terms.push(token);
        }
    }
    terms
}

fn title_ngram_terms(query_terms: &[String]) -> Vec<String> {
    let mut terms = query_terms
        .iter()
        .filter(|term| term.chars().count() >= TITLE_NGRAM_MIN_TERM_CHARS)
        .cloned()
        .collect::<Vec<_>>();
    terms.sort_by(|left, right| {
        right.chars().count().cmp(&left.chars().count()).then_with(|| left.cmp(right))
    });
    terms.truncate(TITLE_NGRAM_MAX_TERMS);
    terms
}

fn title_identity_terms(query: &str, query_terms: &[String]) -> Vec<String> {
    let numeric_literals = numeric_title_literals(query);
    if !numeric_literals.is_empty() {
        return numeric_literals;
    }
    if query_terms.len() > TITLE_IDENTITY_MAX_TERMS {
        return Vec::new();
    }
    query_terms.to_vec()
}

fn numeric_title_literals(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = BTreeSet::new();
    for token in query
        .split(|ch: char| !ch.is_alphanumeric() && ch != '.' && ch != '-' && ch != '_' && ch != '/')
        .map(|token| {
            token.trim_matches(|ch: char| {
                !ch.is_alphanumeric() && ch != '.' && ch != '-' && ch != '_' && ch != '/'
            })
        })
        .filter(|token| token.chars().count() >= 2)
        .filter(|token| token.chars().any(|ch| ch.is_ascii_digit()))
        .map(str::to_lowercase)
    {
        if seen.insert(token.clone()) {
            terms.push(token);
        }
    }
    terms
}

fn title_soft_raw_enabled(query_terms: &[String]) -> bool {
    query_terms.len() <= TITLE_IDENTITY_MAX_TERMS
}

/// Derives the prefix lexemes used by the relaxed passes of the lexical ladder.
///
/// Reuses the same structural tokenizer as [`lexical_query_terms`] (so the rule
/// is identical and language-agnostic), then truncates each token to
/// [`LEXICAL_PREFIX_LEN`] characters and drops tokens shorter than
/// [`LEXICAL_PREFIX_MIN_TOKEN_CHARS`]. Truncation is by character, not byte, so
/// multi-byte scripts are never split mid-codepoint. Tokens already at or below
/// the prefix length are kept whole.
fn lexical_prefix_tokens(query: &str) -> Vec<String> {
    lexical_query_terms(query)
        .into_iter()
        .filter(|token| token.chars().count() >= LEXICAL_PREFIX_MIN_TOKEN_CHARS)
        .map(|token| token.chars().take(LEXICAL_PREFIX_LEN).collect::<String>())
        .collect()
}

/// Short, identifier-shaped tokens (acronym-shaped, e.g. an all-caps two-letter
/// abbreviation) that the
/// prefix passes drop because they fall under [`LEXICAL_PREFIX_MIN_TOKEN_CHARS`].
/// Such a token usually carries the query's discriminating signal, so the relaxed
/// passes keep it as an EXACT (non-prefix) lexeme and always AND it into the body.
///
/// "Identifier-shaped" is decided by the existing script-agnostic
/// [`literal_text_is_identifier_shaped`] gate (separators, digits, mixed case, or
/// all-uppercase acronym shape) — never a language-specific keyword list, so a
/// lowercase preposition is rejected while an acronym in any writing system is
/// kept. The raw token is shape-tested BEFORE lowercasing so the uppercase signal
/// survives the test. Only tokens the prefix passes actually drop are considered:
/// `2..LEXICAL_PREFIX_MIN_TOKEN_CHARS` alphanumerics (single chars are noise,
/// longer tokens are already carried as prefixes by [`lexical_prefix_tokens`]).
fn lexical_exact_shape_lexemes(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in raw_lexical_tokens(query) {
        let alnum_count = raw.chars().filter(|ch| ch.is_alphanumeric()).count();
        if !(2..LEXICAL_PREFIX_MIN_TOKEN_CHARS).contains(&alnum_count) {
            continue;
        }
        if !literal_text_is_identifier_shaped(raw) {
            continue;
        }
        let lexeme = raw.to_lowercase();
        if seen.insert(lexeme.clone()) {
            terms.push(lexeme);
        }
    }
    terms
}

/// Single-quotes a tsquery lexeme so [`to_tsquery`] parses arbitrary token
/// content (slashes, digits, punctuation kept by the tokenizer) without a syntax
/// error. An embedded single quote is doubled per SQL/tsquery escaping.
fn quote_tsquery_lexeme(token: &str) -> String {
    format!("'{}':*", token.replace('\'', "''"))
}

/// Like [`quote_tsquery_lexeme`] but WITHOUT the `:*` prefix marker: an exact
/// lexeme match. Used for short acronym-shaped tokens where a prefix would
/// over-match (a 2-char prefix also hits every unrelated word sharing that
/// start).
fn quote_tsquery_lexeme_exact(token: &str) -> String {
    format!("'{}'", token.replace('\'', "''"))
}

/// Joins the relaxed lexemes into a tsquery body with the given operator (`&` for
/// the prefix-AND pass, `|` for the prefix-OR pass). The relaxed prefix group
/// keeps the caller's operator; any short acronym-shaped exact lexemes are ALWAYS
/// required (AND'd) — they carry the query's discriminating signal — so the prefix
/// group is parenthesized to bind tighter than the surrounding AND. When the exact
/// set is empty this is byte-identical to the previous behaviour (a bare
/// operator-joined prefix group). Returns `None` when no usable tokens remain, so
/// the caller skips the pass instead of handing an empty string to `to_tsquery`
/// (which would error).
fn prefix_relaxed_tsquery_with(query: &str, operator: &str) -> Option<String> {
    let prefix_tokens = lexical_prefix_tokens(query);
    let exact_lexemes = lexical_exact_shape_lexemes(query);
    if prefix_tokens.is_empty() && exact_lexemes.is_empty() {
        return None;
    }
    let mut clauses: Vec<String> = Vec::new();
    if !prefix_tokens.is_empty() {
        let group = prefix_tokens
            .iter()
            .map(|token| quote_tsquery_lexeme(token))
            .collect::<Vec<_>>()
            .join(operator);
        clauses.push(if exact_lexemes.is_empty() { group } else { format!("({group})") });
    }
    clauses.extend(exact_lexemes.iter().map(|token| quote_tsquery_lexeme_exact(token)));
    Some(clauses.join(" & "))
}

/// Pass B body: prefix-AND (`p1:* & p2:* & ...`). Bridges morphological surface
/// forms while still requiring every token to be present, so it stays precise.
fn prefix_relaxed_tsquery_and(query: &str) -> Option<String> {
    prefix_relaxed_tsquery_with(query, " & ")
}

/// Pass C body: prefix-OR (`p1:* | p2:* | ...`). The widest, lowest-precision
/// rung; only used when the prefix-AND pass is still sparse.
fn prefix_relaxed_tsquery_or(query: &str) -> Option<String> {
    prefix_relaxed_tsquery_with(query, " | ")
}

/// Whether the precise pass returned too few hits and the ladder should descend
/// to the next, more relaxed pass.
fn should_relax_lexical(hit_count: usize) -> bool {
    hit_count < LEXICAL_RELAX_FLOOR
}

/// Drives the lexical relaxation ladder for a single-statement lane.
///
/// `run_pass` executes one lane query: given the FTS-substituted SQL and the
/// `$2` input (raw query for the precise pass, prefix tsquery for a relaxed
/// pass), it binds the lane-specific parameters and returns its rows. `key`
/// extracts each row's identity for de-duplication.
///
/// Pass A (precise) always runs. Only when it returns fewer than
/// [`LEXICAL_RELAX_FLOOR`] hits does it descend: Pass B (prefix-AND), then, if
/// still sparse, Pass C (prefix-OR). Relaxed hits are appended below Pass A and
/// de-duplicated, so exact matches always rank first and recall is additive.
async fn run_lexical_ladder<Row, K, F, Fut>(
    query: &str,
    exact_sql: &str,
    prefix_sql: &str,
    key: impl Fn(&Row) -> K,
    run_pass: F,
) -> anyhow::Result<Vec<Row>>
where
    K: Ord,
    F: Fn(String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<Vec<Row>>>,
{
    let mut rows = run_pass(exact_sql.to_owned(), query.to_owned()).await?;
    if should_relax_lexical(rows.len()) {
        for body in [prefix_relaxed_tsquery_and(query), prefix_relaxed_tsquery_or(query)] {
            let Some(prefix_query) = body else { continue };
            let relaxed = run_pass(prefix_sql.to_owned(), prefix_query).await?;
            let mut seen: BTreeSet<K> = rows.iter().map(&key).collect();
            rows.extend(relaxed.into_iter().filter(|row| seen.insert(key(row))));
            if !should_relax_lexical(rows.len()) {
                break;
            }
        }
    }
    Ok(rows)
}

/// Substitutes the `{FTS}` placeholder in a lexical-lane SQL template with the
/// precise (`websearch_to_tsquery`) and relaxed (`to_tsquery`) constructors,
/// returning `(exact_sql, prefix_sql)`. Both bind the FTS input via `$2`.
fn lexical_lane_sql(template: &str) -> (String, String) {
    (
        template.replace("{FTS}", "websearch_to_tsquery('simple', ironrag_unaccent($2))"),
        template.replace("{FTS}", "to_tsquery('simple', ironrag_unaccent($2))"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // All natural-language tokens below are synthetic placeholders. Multi-byte
    // strings exist only to prove the truncation rule respects char boundaries;
    // none encode a real query, dictionary, or stop-word list.

    #[test]
    fn lexical_lane_sql_substitutes_distinct_fts_constructors() {
        let (exact, prefix) = lexical_lane_sql("a {FTS} b {FTS} c");
        assert_eq!(
            exact,
            "a websearch_to_tsquery('simple', ironrag_unaccent($2)) b \
             websearch_to_tsquery('simple', ironrag_unaccent($2)) c"
        );
        assert_eq!(
            prefix,
            "a to_tsquery('simple', ironrag_unaccent($2)) b \
             to_tsquery('simple', ironrag_unaccent($2)) c"
        );
    }

    #[test]
    fn chunk_exact_template_is_byte_identical_to_original() {
        // Pass A must execute the unchanged precise query: the only difference
        // from the relaxed template is the FTS constructor literal.
        assert!(CHUNK_LEXICAL_SQL_EXACT.contains(
            "ts_rank_cd(c.search_tsv, websearch_to_tsquery('simple', ironrag_unaccent($2)))"
        ));
        assert!(
            CHUNK_LEXICAL_SQL_EXACT
                .contains("c.search_tsv @@ websearch_to_tsquery('simple', ironrag_unaccent($2))")
        );
        assert!(!CHUNK_LEXICAL_SQL_EXACT.contains("{FTS}"));
        // The exact template must use only the `websearch_` constructor — no bare
        // `to_tsquery(` (a "(" guards against the `websearch_to_tsquery` substring).
        assert!(!CHUNK_LEXICAL_SQL_EXACT.contains(", to_tsquery("));
        assert!(CHUNK_LEXICAL_SQL_EXACT.contains("@@ websearch_to_tsquery("));
        assert!(
            CHUNK_LEXICAL_SQL_PREFIX
                .contains("c.search_tsv @@ to_tsquery('simple', ironrag_unaccent($2))")
        );
        assert!(!CHUNK_LEXICAL_SQL_PREFIX.contains("websearch_to_tsquery"));
        assert!(!CHUNK_LEXICAL_SQL_PREFIX.contains("{FTS}"));
    }

    // (a) A multi-token query produces the exact prefix-AND query.
    #[test]
    fn prefix_relaxed_and_emits_exact_prefix_and_query() {
        // tokens: "update"(6) "counter"(7) "server"(6) -> all kept, truncated to 5.
        let body = prefix_relaxed_tsquery_and("update counter server").unwrap();
        assert_eq!(body, "'updat':* & 'count':* & 'serve':*");
    }

    // (b) The prefix-relaxed query truncates tokens correctly and drops short
    //     tokens per the rule (keep tokens >= 4 chars, truncate to 5).
    #[test]
    fn prefix_rule_truncates_long_tokens_and_drops_short_ones() {
        // "abc"(3) dropped (< min 4); "abcd"(4)/"abcde"(5) kept whole;
        // "abcdef"(6) truncated to its 5-char prefix.
        assert_eq!(lexical_prefix_tokens("abc"), Vec::<String>::new());
        assert_eq!(lexical_prefix_tokens("abcd"), vec!["abcd".to_string()]);
        assert_eq!(lexical_prefix_tokens("abcde"), vec!["abcde".to_string()]);
        assert_eq!(lexical_prefix_tokens("abcdef"), vec!["abcde".to_string()]);
        // mixed: 3-char "how" dropped, longer tokens truncated, order preserved.
        assert_eq!(
            lexical_prefix_tokens("how update counter"),
            vec!["updat".to_string(), "count".to_string()]
        );
    }

    #[test]
    fn prefix_rule_keeps_token_under_two_chars_dropped() {
        // A 2-char token (would over-match as "xx:*") is dropped entirely.
        assert_eq!(lexical_prefix_tokens("ab update"), vec!["updat".to_string()]);
    }

    #[test]
    fn prefix_truncation_respects_char_boundaries_for_multibyte() {
        // Multi-byte (2 bytes/char) input must truncate by char, never panic on a
        // byte boundary. 8 chars -> first 5 chars (10 bytes), still valid UTF-8.
        let token = "αβγδεζηθ"; // 8 Greek letters, 2 bytes each
        let prefixed = lexical_prefix_tokens(token);
        assert_eq!(prefixed, vec![token.chars().take(5).collect::<String>()]);
        assert_eq!(prefixed[0].chars().count(), 5);
    }

    #[test]
    fn prefix_lexemes_are_single_quoted_for_tsquery_safety() {
        // A token kept by the tokenizer that contains a slash must be quoted so
        // to_tsquery parses it as a literal lexeme, not operator syntax.
        let body = prefix_relaxed_tsquery_and("v2/payments").unwrap();
        assert_eq!(body, "'v2/pa':*");
    }

    #[test]
    fn prefix_relaxed_or_uses_or_operator() {
        let body = prefix_relaxed_tsquery_or("update counter").unwrap();
        assert_eq!(body, "'updat':* | 'count':*");
    }

    #[test]
    fn prefix_relaxed_is_none_when_no_usable_tokens() {
        // All tokens below the prefix minimum -> no relaxed pass is attempted,
        // so to_tsquery('simple','') is never reached.
        assert!(prefix_relaxed_tsquery_and("ab cd e").is_none());
        assert!(prefix_relaxed_tsquery_or("").is_none());
    }

    // (d) A short identifier-shaped (acronym) token is kept as an EXACT lexeme and
    //     AND'd into the prefix-AND body; the prefix group is parenthesized so the
    //     acronym binds as a hard requirement. "AB" is a synthetic acronym.
    #[test]
    fn exact_shape_acronym_is_required_in_prefix_and_body() {
        let body = prefix_relaxed_tsquery_and("update AB server").unwrap();
        assert_eq!(body, "('updat':* & 'serve':*) & 'ab'");
    }

    // (e) The acronym is AND'd even in the OR pass (the prefix group is OR'd
    //     internally but the acronym stays required, so recall widens without
    //     losing the discriminating token).
    #[test]
    fn exact_shape_acronym_is_required_in_prefix_or_body() {
        let body = prefix_relaxed_tsquery_or("update AB server").unwrap();
        assert_eq!(body, "('updat':* | 'serve':*) & 'ab'");
    }

    // (f) Script-agnostic: a non-Latin all-uppercase acronym is admitted the same
    //     way (Greek capitals here — no real natural-language content), then
    //     lowercased to match the `simple` tsvector lexeme.
    #[test]
    fn exact_shape_acronym_is_script_agnostic() {
        assert_eq!(lexical_exact_shape_lexemes("ΑΒ update"), vec!["αβ".to_string()]);
    }

    // (g) Lowercase short tokens carry no identifier signal and are NOT promoted
    //     to exact lexemes, so a stop-word-shaped short token never over-constrains.
    #[test]
    fn exact_shape_skips_lowercase_short_tokens() {
        assert!(lexical_exact_shape_lexemes("ab cd update").is_empty());
    }

    // (h) A query whose only usable token is a short acronym yields a bare exact
    //     lexeme — no empty prefix group, no dangling operator handed to to_tsquery.
    #[test]
    fn exact_shape_only_query_emits_bare_exact_lexeme() {
        assert_eq!(prefix_relaxed_tsquery_and("AB").unwrap(), "'ab'");
        assert_eq!(prefix_relaxed_tsquery_or("AB").unwrap(), "'ab'");
    }

    // (i) Regression guard: with no exact-shape token the body is byte-identical
    //     to the pre-change behaviour (a bare operator-joined prefix group).
    #[test]
    fn exact_shape_empty_is_byte_identical_to_prefix_only() {
        assert_eq!(
            prefix_relaxed_tsquery_and("update counter server").unwrap(),
            "'updat':* & 'count':* & 'serve':*"
        );
        assert_eq!(prefix_relaxed_tsquery_or("update counter").unwrap(), "'updat':* | 'count':*");
    }

    // (j) Length window: 2-3 char acronyms become exact lexemes; >= 4-char tokens
    //     stay in the prefix path (never duplicated as exact); single chars are
    //     noise. Proves the exact lane rescues exactly the tokens prefixes drop.
    #[test]
    fn exact_shape_respects_length_window() {
        assert_eq!(lexical_exact_shape_lexemes("AB"), vec!["ab".to_string()]);
        assert_eq!(lexical_exact_shape_lexemes("ABC"), vec!["abc".to_string()]);
        assert!(lexical_exact_shape_lexemes("ABCD").is_empty());
        assert!(lexical_exact_shape_lexemes("A").is_empty());
    }

    // (k) Separator/digit-bearing short identifiers are rescued via the same shape
    //     gate (separators and digits are structural identifier signals); a
    //     slash-bearing token stays a single unit with the prefix lane (the shape
    //     gate rejects '/'), so both lexical lanes share one tokenizer + decision.
    #[test]
    fn exact_shape_admits_separator_and_digit_identifiers() {
        assert_eq!(lexical_exact_shape_lexemes("A_B update"), vec!["a_b".to_string()]);
        assert_eq!(lexical_exact_shape_lexemes("v2 update"), vec!["v2".to_string()]);
        // '/' is not an identifier-shape separator -> not rescued as an exact lexeme.
        assert!(lexical_exact_shape_lexemes("a/b update").is_empty());
    }

    // (c) The ladder only relaxes when the precise pass is sparse.
    #[test]
    fn should_relax_only_below_floor() {
        assert!(should_relax_lexical(0));
        assert!(should_relax_lexical(LEXICAL_RELAX_FLOOR - 1));
        assert!(!should_relax_lexical(LEXICAL_RELAX_FLOOR));
        assert!(!should_relax_lexical(LEXICAL_RELAX_FLOOR + 1));
    }

    #[tokio::test]
    async fn ladder_skips_relaxation_when_precise_pass_is_full() {
        let calls = std::cell::Cell::new(0_usize);
        let rows: Vec<u32> = run_lexical_ladder(
            "update counter server",
            "EXACT",
            "PREFIX",
            |row: &u32| *row,
            |sql, _fts| {
                calls.set(calls.get() + 1);
                async move {
                    // Pass A returns a full result set (>= floor); Pass B/C must
                    // never run, so they are not modelled here.
                    assert_eq!(sql, "EXACT");
                    Ok((0..LEXICAL_RELAX_FLOOR as u32).collect())
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(calls.get(), 1, "only the precise pass should execute");
        assert_eq!(rows.len(), LEXICAL_RELAX_FLOOR);
    }

    #[tokio::test]
    async fn ladder_relaxes_and_appends_deduped_below_precise_hits() {
        let calls = std::cell::Cell::new(0_usize);
        let rows: Vec<u32> = run_lexical_ladder(
            "update counter server",
            "EXACT",
            "PREFIX",
            |row: &u32| *row,
            |sql, _fts| {
                let n = calls.get();
                calls.set(n + 1);
                async move {
                    if n == 0 {
                        assert_eq!(sql, "EXACT");
                        // Sparse precise result: 2 hits (< floor).
                        Ok(vec![1_u32, 2])
                    } else {
                        assert_eq!(sql, "PREFIX");
                        // Relaxed Pass B: overlaps {1,2} and adds {3,4,...}.
                        Ok((1..=LEXICAL_RELAX_FLOOR as u32 + 2).collect())
                    }
                }
            },
        )
        .await
        .unwrap();
        // Precise hits 1,2 keep their leading positions; relaxed hits appended
        // de-duplicated (no second 1 or 2).
        assert_eq!(&rows[..2], &[1, 2]);
        assert!(rows.iter().filter(|&&r| r == 1).count() == 1);
        assert!(rows.iter().filter(|&&r| r == 2).count() == 1);
        // Pass B alone cleared the floor, so Pass C did not run (2 calls total).
        assert_eq!(calls.get(), 2);
    }
}
