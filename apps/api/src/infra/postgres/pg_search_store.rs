use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{Executor, FromRow, PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::domains::query_ir::literal_text_is_identifier_shaped;
use crate::domains::retrieval::DEFAULT_TEXT_SEARCH_CONFIG;
use crate::infra::postgres::pg_vector_config::{
    PgVectorStorage, pg_hnsw_index_params, read_env_u64,
};
use crate::infra::{
    knowledge_plane::{
        CanonicalIngestVectorWriteFence, CanonicalVectorWriteFence, ChunkVectorProfileInventory,
        SearchStore, VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX,
        VECTOR_REBUILD_STAGING_PROFILE_DIGEST_LEN, VECTOR_REBUILD_STAGING_PROFILE_PREFIX,
        VectorPlaneDeleteOutcome,
    },
    knowledge_rows::{
        KNOWLEDGE_CHUNK_VECTOR_KIND, KNOWLEDGE_ENTITY_VECTOR_KIND, KnowledgeChunkSearchRow,
        KnowledgeChunkVectorRow, KnowledgeChunkVectorSearchRow, KnowledgeEntitySearchRow,
        KnowledgeEntityVectorRow, KnowledgeEntityVectorSearchRow, KnowledgeRelationSearchRow,
        KnowledgeStructuredBlockSearchRow, KnowledgeTechnicalFactSearchRow,
    },
    repositories::ingest_repository,
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
const EMBEDDING_PROFILE_PREFIX: &str = "embedding-profile:v1:";
const EMBEDDING_PROFILE_DIGEST_LEN: usize = 64;
const VECTOR_RELATION_DDL_ADVISORY_LOCK_KEY: &str = "knowledge.vector_relation.ddl";
/// Maximum manifest rows locked and removed per abandoned-rebuild recovery
/// transaction. Recovery commits each batch and continues until exhausted, so
/// this is a lock/transaction bound rather than a ceiling on recoverable work.
const MAX_ABANDONED_VECTOR_REBUILD_MANIFEST_ROWS: usize = 512;
const PGVECTOR_MAX_INDEXED_DIM: u64 = 4_000;
const PG_HNSW_DEFAULT_EF_SEARCH: u64 = 400;
// pgvector defines hnsw.ef_search as an integer GUC in the inclusive range
// 1..=1000. Clamp locally so a configuration typo cannot abort every ANN query.
const PG_HNSW_MAX_EF_SEARCH: u64 = 1_000;
const PG_HNSW_DEFAULT_MAX_SCAN_TUPLES: u64 = 50_000;
const PG_HNSW_MAX_SCAN_TUPLES: u64 = 1_000_000;
const PG_HNSW_DEFAULT_SCAN_MEM_MULTIPLIER: u64 = 2;
const PG_HNSW_MAX_SCAN_MEM_MULTIPLIER: u64 = 64;
const PG_HNSW_DEFAULT_EXACT_FALLBACK_MAX_ROWS: u64 = 10_000;
const PG_HNSW_MAX_EXACT_FALLBACK_ROWS: u64 = 100_000;

/// Chunk lexical-lane CTE with the FTS constructor abstracted as `{FTS}`. The
/// two rungs of the relaxation ladder share this template verbatim and differ
/// only in which tsquery constructor is substituted — the user query text always
/// binds via `$2`, never interpolated, so no user data enters the SQL string.
const CHUNK_LEXICAL_SQL_TEMPLATE: &str = "with readable_docs as (
                 select d.document_id, d.readable_revision_id
                 from knowledge_document d
                 where d.library_id = $1
                   and d.document_state = 'active'
                   and d.readable_revision_id is not null
                   and d.deleted_at is null
             ),
             title_identity_docs as (
                 select d.document_id
                 from knowledge_document d
                 where d.library_id = $1
                   and d.document_state = 'active'
                   and d.readable_revision_id is not null
                   and d.deleted_at is null
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
                   and d.readable_revision_id is not null
                   and d.deleted_at is null
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
                 join readable_docs rd
                   on rd.document_id = c.document_id
                  and rd.readable_revision_id = c.revision_id
                 where c.library_id = $1
                   and c.chunk_state = 'ready'
                   and c.raptor_level is null
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
                        join readable_docs rd
                          on rd.document_id = c.document_id
                         and rd.readable_revision_id = c.revision_id
                        join title_identity_docs d on d.document_id = c.document_id
                        where c.library_id = $1
                          and c.chunk_state = 'ready'
                          and c.raptor_level is null
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
                        join readable_docs rd
                          on rd.document_id = c.document_id
                         and rd.readable_revision_id = c.revision_id
                        join soft_title_docs d on d.document_id = c.document_id
                        where $9::boolean
                          and c.library_id = $1
                          and c.chunk_state = 'ready'
                          and c.raptor_level is null
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

/// Structured source blocks are revision-scoped evidence. Joining the
/// canonical document head in the lexical statement is essential: filtering
/// stale rows after this query would let them consume the bounded top-k and
/// starve the current readable revision.
const STRUCTURED_BLOCK_LEXICAL_SQL_TEMPLATE: &str =
    "select b.block_id, b.document_id, b.workspace_id, b.library_id, b.revision_id, b.ordinal,
        b.block_kind, b.text, b.normalized_text, b.section_path, b.heading_trail,
        ts_rank_cd(b.search_tsv, {FTS})::double precision as score
     from knowledge_structured_block b
     join knowledge_document d
       on d.document_id = b.document_id
      and d.library_id = b.library_id
      and d.readable_revision_id = b.revision_id
      and d.document_state = 'active'
      and d.deleted_at is null
     where b.library_id = $1
       and b.search_tsv @@ {FTS}
     order by score desc, b.revision_id desc, b.ordinal asc, b.block_id asc
     limit $3";

/// Typed facts have the same canonical-head requirement as chunks and
/// structured blocks. The exact-value boost is preserved, but it may only rank
/// facts from the current readable revision of an active document.
const TECHNICAL_FACT_LEXICAL_SQL_TEMPLATE: &str =
    "select f.fact_id, f.document_id, f.workspace_id, f.library_id, f.revision_id, f.fact_kind,
        f.canonical_value_text, f.display_value,
        (f.canonical_value_exact = $3) as exact_match,
        (
            case when f.canonical_value_exact = $3 then 1000000.0 else 0.0 end
            + ts_rank_cd(f.search_tsv, {FTS})::double precision
        ) as score
     from knowledge_technical_fact f
     join knowledge_document d
       on d.document_id = f.document_id
      and d.library_id = f.library_id
      and d.readable_revision_id = f.revision_id
      and d.document_state = 'active'
      and d.deleted_at is null
     where f.library_id = $1
       and (
            f.canonical_value_exact = $3
            or f.search_tsv @@ {FTS}
       )
     order by score desc, f.fact_id asc
     limit $4";

/// Pass A (precise): exact AND `websearch_to_tsquery`, rendered with the
/// historical default text-search config and the shared safety filters.
static CHUNK_LEXICAL_SQL_EXACT: LazyLock<String> =
    LazyLock::new(|| chunk_lexical_sql(DEFAULT_TEXT_SEARCH_CONFIG).0);

/// Relaxed passes (B/C): `to_tsquery`, fed a prefix tsquery string via `$2`.
static CHUNK_LEXICAL_SQL_PREFIX: LazyLock<String> =
    LazyLock::new(|| chunk_lexical_sql(DEFAULT_TEXT_SEARCH_CONFIG).1);

/// Renders the chunk lexical-lane `(exact_sql, prefix_sql)` pair for a given
/// Postgres text-search config name. `text_search_config == "simple"` preserves
/// the historical analyzer semantics.
fn chunk_lexical_sql(text_search_config: &str) -> (String, String) {
    lexical_lane_sql_for_config(CHUNK_LEXICAL_SQL_TEMPLATE, text_search_config)
}

fn structured_block_lexical_sql() -> (String, String) {
    lexical_lane_sql(STRUCTURED_BLOCK_LEXICAL_SQL_TEMPLATE)
}

fn technical_fact_lexical_sql() -> (String, String) {
    lexical_lane_sql(TECHNICAL_FACT_LEXICAL_SQL_TEMPLATE)
}

#[derive(Clone)]
pub struct PgSearchStore {
    pub pool: PgPool,
}

#[derive(Debug, Clone)]
struct PgVectorSearchLane {
    relation_name: String,
    manifest_row_count: u64,
}

#[derive(Debug, Clone, FromRow)]
struct PgChunkVectorRow {
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

fn chunk_vector_similarity_sql_with_order(
    relation: &str,
    cast_type: &str,
    exact_retry: bool,
) -> String {
    let order_expression = if exact_retry {
        format!("(v.embedding <=> $3::{cast_type}) + 0.0")
    } else {
        format!("v.embedding <=> $3::{cast_type}")
    };
    format!(
        "select v.vector_id, v.workspace_id, v.library_id, v.chunk_id, v.revision_id,
            v.embedding_model_key, v.vector_kind, v.freshness_generation,
            (1.0 - (v.embedding <=> $3::{cast_type}))::double precision as score
         from {relation} v
         join knowledge_chunk c
           on c.chunk_id = v.chunk_id
          and c.revision_id = v.revision_id
          and c.library_id = v.library_id
         join knowledge_document d
           on d.document_id = c.document_id
          and d.library_id = v.library_id
          and d.readable_revision_id = v.revision_id
          and d.document_state = 'active'
          and d.deleted_at is null
         where v.library_id = $1
           and v.embedding_model_key = $2
           and v.vector_kind = $6
           and c.chunk_state = 'ready'
           and c.raptor_level is null
           and (($4::timestamptz is null and $5::timestamptz is null)
                or (v.occurred_at is not null
                    and ($4::timestamptz is null or coalesce(v.occurred_until, v.occurred_at) >= $4)
                    and ($5::timestamptz is null or v.occurred_at <= $5)))
         order by {order_expression}, v.chunk_id asc
         limit $7"
    )
}

fn chunk_vector_similarity_sql(relation: &str, cast_type: &str) -> String {
    chunk_vector_similarity_sql_with_order(relation, cast_type, false)
}

fn chunk_vector_exact_similarity_sql(relation: &str, cast_type: &str) -> String {
    chunk_vector_similarity_sql_with_order(relation, cast_type, true)
}

fn entity_vector_similarity_sql_with_order(
    relation: &str,
    cast_type: &str,
    exact_retry: bool,
) -> String {
    let order_expression = if exact_retry {
        format!("(v.embedding <=> $3::{cast_type}) + 0.0")
    } else {
        format!("v.embedding <=> $3::{cast_type}")
    };
    format!(
        "select v.vector_id, v.workspace_id, v.library_id, v.entity_id,
            v.embedding_model_key, v.vector_kind, v.freshness_generation,
            (1.0 - (v.embedding <=> $3::{cast_type}))::double precision as score
         from {relation} v
         join knowledge_entity e
           on e.entity_id = v.entity_id
          and e.library_id = v.library_id
          and e.entity_state = 'active'
          and e.freshness_generation = v.freshness_generation
         where v.library_id = $1
           and v.embedding_model_key = $2
           and v.vector_kind = $4
         order by {order_expression}, v.entity_id asc
         limit $5"
    )
}

fn entity_vector_similarity_sql(relation: &str, cast_type: &str) -> String {
    entity_vector_similarity_sql_with_order(relation, cast_type, false)
}

fn entity_vector_exact_similarity_sql(relation: &str, cast_type: &str) -> String {
    entity_vector_similarity_sql_with_order(relation, cast_type, true)
}

/// Runtime coverage checks must measure only vectors backed by canonical
/// source chunks. Legacy RAPTOR summary rows can remain in the physical vector
/// shards for maintenance/cleanup, but they must not make a revision appear
/// over- or under-embedded.
fn canonical_chunk_vector_count_sql(relation: &str) -> String {
    format!(
        "select count(*)::bigint
         from {relation} v
         join knowledge_chunk c
           on c.chunk_id = v.chunk_id
          and c.revision_id = v.revision_id
          and c.library_id = v.library_id
         where v.revision_id = $1
           and v.embedding_model_key = $2
           and v.vector_kind = $3
           and v.freshness_generation = $4
           and c.chunk_state = 'ready'
           and c.raptor_level is null"
    )
}

/// Count live canonical vectors across all candidate dimension shards in one
/// set-based database call. This avoids trusting manifest `row_count`, which
/// deliberately includes legacy rows used by broad maintenance APIs, without
/// adding one client round trip per historical dimension.
fn canonical_chunk_vector_dimension_counts_sql(
    manifest_rows: &[(i32, String)],
) -> anyhow::Result<Option<String>> {
    let mut branches = Vec::with_capacity(manifest_rows.len());
    for (dim, relation_name) in manifest_rows {
        anyhow::ensure!(*dim > 0, "chunk vector manifest dimension must be positive");
        validate_relation_name(relation_name, CHUNK_VECTOR_RELATION_PREFIX)?;
        let expected_relation = vector_relation_name(
            CHUNK_VECTOR_RELATION_PREFIX,
            u64::try_from(*dim).context("chunk vector manifest dimension overflowed u64")?,
        )?;
        anyhow::ensure!(
            relation_name == &expected_relation,
            "chunk vector manifest dimension {dim} points to unexpected relation {relation_name}"
        );
        let relation = quote_identifier(relation_name)?;
        branches.push(format!(
            "select {dim}::integer as dim, count(distinct v.chunk_id)::bigint as canonical_count
             from {relation} v
             join knowledge_chunk c
               on c.chunk_id = v.chunk_id
              and c.revision_id = v.revision_id
              and c.library_id = v.library_id
             join knowledge_document d
               on d.document_id = c.document_id
              and d.library_id = v.library_id
              and d.readable_revision_id = v.revision_id
              and d.document_state = 'active'
              and d.deleted_at is null
             where v.library_id = $1
               and v.embedding_model_key = $2
               and v.vector_kind = $3
               and c.chunk_state = 'ready'
               and c.raptor_level is null"
        ));
    }
    Ok((!branches.is_empty()).then(|| branches.join(" union all ")))
}

fn rank_canonical_chunk_vector_dimensions(mut counts: Vec<(i32, i64)>) -> anyhow::Result<Vec<u64>> {
    for (dim, count) in &counts {
        anyhow::ensure!(*dim > 0, "chunk vector manifest dimension must be positive");
        anyhow::ensure!(*count >= 0, "canonical chunk vector count must be non-negative");
    }
    counts.retain(|(_, count)| *count > 0);
    counts.sort_by(|(left_dim, left_count), (right_dim, right_count)| {
        right_count.cmp(left_count).then_with(|| right_dim.cmp(left_dim))
    });
    counts
        .into_iter()
        .map(|(dim, _)| {
            u64::try_from(dim).context("chunk vector manifest dimension overflowed u64")
        })
        .collect()
}

fn chunk_vector_profile_inventory(
    canonical_counts: Vec<(i32, i64)>,
) -> anyhow::Result<ChunkVectorProfileInventory> {
    let active_vector_count =
        canonical_counts.iter().try_fold(0_u64, |total, (_, count)| -> anyhow::Result<u64> {
            let count = u64::try_from(*count)
                .context("active canonical chunk vector count was negative")?;
            total.checked_add(count).context("active canonical chunk vector count overflowed")
        })?;
    Ok(ChunkVectorProfileInventory {
        dimensions: rank_canonical_chunk_vector_dimensions(canonical_counts)?,
        active_vector_count,
    })
}

impl PgSearchStore {
    async fn vector_relation_objects_exist(
        &self,
        relation_name: &str,
        id_column: &str,
        extra_column: Option<&str>,
    ) -> anyhow::Result<bool> {
        let required_objects =
            vector_relation_required_objects(relation_name, id_column, extra_column);
        sqlx::query_scalar::<_, bool>(
            "select coalesce(bool_and(
                        to_regclass(format('%I.%I', current_schema(), object_name)) is not null
                    ), false)
             from unnest($1::text[]) required(object_name)",
        )
        .bind(required_objects)
        .fetch_one(&self.pool)
        .await
        .with_context(|| format!("failed to inspect vector relation objects for {relation_name}"))
    }

    async fn vector_relation_objects_exist_in_transaction(
        transaction: &mut Transaction<'_, Postgres>,
        relation_name: &str,
        id_column: &str,
        extra_column: Option<&str>,
    ) -> anyhow::Result<bool> {
        let required_objects =
            vector_relation_required_objects(relation_name, id_column, extra_column);
        sqlx::query_scalar::<_, bool>(
            "select coalesce(bool_and(
                        to_regclass(format('%I.%I', current_schema(), object_name)) is not null
                    ), false)
             from unnest($1::text[]) required(object_name)",
        )
        .bind(required_objects)
        .fetch_one(&mut **transaction)
        .await
        .with_context(|| format!("failed to recheck vector relation objects for {relation_name}"))
    }

    async fn ensure_chunk_vector_relation(&self, dim: u64) -> anyhow::Result<String> {
        let relation_name = vector_relation_name(CHUNK_VECTOR_RELATION_PREFIX, dim)?;
        if self
            .vector_relation_objects_exist(&relation_name, "chunk_id", Some("revision_id"))
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
        if Self::vector_relation_objects_exist_in_transaction(
            &mut transaction,
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
        transaction.commit().await.context("commit chunk vector relation DDL")?;
        Ok(relation_name)
    }

    async fn ensure_entity_vector_relation(&self, dim: u64) -> anyhow::Result<String> {
        let relation_name = vector_relation_name(ENTITY_VECTOR_RELATION_PREFIX, dim)?;
        if self.vector_relation_objects_exist(&relation_name, "entity_id", None).await? {
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
        if Self::vector_relation_objects_exist_in_transaction(
            &mut transaction,
            &relation_name,
            "entity_id",
            None,
        )
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

    async fn upsert_manifest_in_transaction(
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

    async fn resolve_manifest_search_lane(
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

    async fn upsert_chunk_vectors_in_relation_bulk_with_executor<'e, E>(
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

    async fn upsert_entity_vectors_in_relation_bulk_with_executor<'e, E>(
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

    async fn upsert_chunk_vectors_bulk_immediate(
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

    async fn upsert_chunk_vectors_bulk_fenced_immediate(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &CanonicalVectorWriteFence,
    ) -> anyhow::Result<i64> {
        let first = rows.first().context("fenced chunk vector batch must not be empty")?;
        validate_canonical_embedding_profile_key(&fence.embedding_profile_key)?;
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
        let observed_source_truth_version =
            validate_canonical_vector_write_fence(&mut transaction, first.library_id, fence)
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
        let source_truth_version = finish_canonical_vector_write_fence(
            &mut transaction,
            first.library_id,
            fence,
            observed_source_truth_version,
        )
        .await?;
        transaction.commit().await.context("commit fenced chunk vector write")?;
        Ok(source_truth_version)
    }

    async fn upsert_entity_vectors_bulk_fenced_immediate(
        &self,
        rows: &[KnowledgeEntityVectorRow],
        fence: &CanonicalVectorWriteFence,
    ) -> anyhow::Result<i64> {
        let first = rows.first().context("fenced entity vector batch must not be empty")?;
        validate_canonical_embedding_profile_key(&fence.embedding_profile_key)?;
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
            validate_canonical_vector_write_fence(&mut transaction, first.library_id, fence)
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
        let source_truth_version = finish_canonical_vector_write_fence(
            &mut transaction,
            first.library_id,
            fence,
            observed_source_truth_version,
        )
        .await?;
        transaction.commit().await.context("commit fenced entity vector write")?;
        Ok(source_truth_version)
    }

    async fn upsert_entity_vectors_bulk_immediate(
        &self,
        rows: &[KnowledgeEntityVectorRow],
    ) -> anyhow::Result<()> {
        let mut groups: BTreeMap<(u64, Uuid, String, String), Vec<&KnowledgeEntityVectorRow>> =
            BTreeMap::new();
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

    async fn reconcile_manifest_count_for_lane(
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

    /// Runs a single rung of the chunk lexical ladder.
    ///
    /// `sql` is one of the [`CHUNK_LEXICAL_SQL_EXACT`] / [`CHUNK_LEXICAL_SQL_PREFIX`]
    /// templates (identical except for the FTS constructor literal). `fts_input`
    /// is bound as `$2`: the raw user query for the exact pass, or the prebuilt
    /// prefix tsquery string for a relaxed pass. Every other bound parameter is
    /// identical across passes, so the title-aware scoring is preserved while
    /// only the FTS lane widens.
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
        let rows = sqlx::query_as::<_, PgChunkSearchRow>(sqlx::AssertSqlSafe(sql))
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

fn ensure_single_manifest_row_updated(rows_affected: u64) -> anyhow::Result<()> {
    anyhow::ensure!(
        rows_affected == 1,
        "vector manifest reconciliation updated {rows_affected} rows; expected exactly one prepared lane"
    );
    Ok(())
}

fn vector_plane_data_advisory_lock_name(library_id: Uuid) -> String {
    format!("{VECTOR_PLANE_DATA_ADVISORY_LOCK_PREFIX}:{library_id}")
}

async fn lock_library_vector_plane_data(
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

async fn validate_canonical_vector_write_fence(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    fence: &CanonicalVectorWriteFence,
) -> anyhow::Result<i64> {
    anyhow::ensure!(
        fence.expected_source_truth_version > 0,
        "canonical vector write requires a positive source-truth fence"
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
    .context("lock canonical vector write source/profile fence")?
    .ok_or_else(|| anyhow::anyhow!("library disappeared during canonical vector write"))?;
    anyhow::ensure!(
        source_truth_version == fence.expected_source_truth_version,
        "library source or embedding profile changed before canonical vector write"
    );
    if let Some(ingest_attempt) = fence.ingest_attempt {
        anyhow::ensure!(
            lock_latest_leased_ingest_attempt_authority(transaction, library_id, ingest_attempt,)
                .await?,
            "ingest attempt authority changed before canonical vector write"
        );
    }
    Ok(source_truth_version)
}

/// Lock and validate the queue authority used by a short vector transaction.
///
/// Canonical lock order is library -> job -> attempt. The caller has already
/// locked the library source row; holding the job row in SHARE mode makes
/// lease recovery/finalization wait until the vector transaction commits. No
/// provider I/O runs while these locks are held.
async fn lock_latest_leased_ingest_attempt_authority(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    ingest_attempt: CanonicalIngestVectorWriteFence,
) -> anyhow::Result<bool> {
    ingest_repository::lock_latest_leased_revision_attempt(
        transaction,
        library_id,
        ingest_attempt.attempt_id,
        ingest_attempt.revision_id,
    )
    .await
    .context("lock latest canonical vector ingest authority")
}

async fn finish_canonical_vector_write_fence(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    fence: &CanonicalVectorWriteFence,
    observed_source_truth_version: i64,
) -> anyhow::Result<i64> {
    if !fence.advance_source_truth_version {
        return Ok(observed_source_truth_version);
    }
    advance_vector_source_truth_version(transaction, library_id, observed_source_truth_version)
        .await
}

async fn validate_exclusive_vector_delete_fence(
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
        "library source or embedding profile changed before destructive vector mutation"
    );
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

fn validate_vector_rebuild_staging_profile_key(profile_key: &str) -> anyhow::Result<()> {
    let digest = profile_key
        .strip_prefix(VECTOR_REBUILD_STAGING_PROFILE_PREFIX)
        .context("vector rebuild staging profile has an invalid protocol prefix")?;
    anyhow::ensure!(
        digest.len() == VECTOR_REBUILD_STAGING_PROFILE_DIGEST_LEN
            && digest.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "vector rebuild staging profile has an invalid digest"
    );
    Ok(())
}

fn validate_canonical_embedding_profile_key(profile_key: &str) -> anyhow::Result<()> {
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

fn validate_exact_profile_dimension_claim(
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

async fn lock_prepared_manifest_lane_for_share(
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

async fn discard_staged_vector_rebuild_in_transaction(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    dimensions: u64,
    staging_embedding_model_key: &str,
) -> anyhow::Result<BTreeSet<(u64, String, String)>> {
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
        let relation = quote_identifier(&relation_name)?;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {relation}
             where library_id = $1
               and vector_kind = $2
               and embedding_model_key = $3"
        )))
        .bind(library_id)
        .bind(&vector_kind)
        .bind(staging_embedding_model_key)
        .execute(&mut **transaction)
        .await
        .with_context(|| format!("delete staged rows from {relation_name}"))?;
        touched_relations.insert((dimensions, vector_kind, relation_name));
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

async fn delete_source_less_vectors_in_touched_relations(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    touched_relations: &BTreeSet<(u64, String, String)>,
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

async fn list_physical_vector_relations_in_transaction(
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

#[derive(Clone, Copy)]
enum FencedChunkVectorDelete<'a> {
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
        let result = match selector {
            FencedChunkVectorDelete::ExactVectorIds(vector_ids) => {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "delete from {relation}
                     where library_id = $1
                       and vector_id = any($2::uuid[])"
                )))
                .bind(library_id)
                .bind(vector_ids.to_vec())
                .execute(&mut **transaction)
                .await
            }
            FencedChunkVectorDelete::Revision(revision_id) => {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "delete from {relation}
                     where library_id = $1
                       and revision_id = $2"
                )))
                .bind(library_id)
                .bind(revision_id)
                .execute(&mut **transaction)
                .await
            }
        }
        .with_context(|| format!("delete fenced chunk vectors from {relation_name}"))?;
        deleted = deleted
            .checked_add(result.rows_affected())
            .context("fenced chunk vector delete count overflowed u64")?;
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
    refresh_library_vector_manifest_counts_in_transaction(
        transaction,
        library_id,
        CHUNK_VECTOR_RELATION_PREFIX,
    )
    .await?;
    Ok(deleted)
}

async fn delete_chunk_vectors_fenced(
    pool: &PgPool,
    library_id: Uuid,
    selector: FencedChunkVectorDelete<'_>,
    expected_source_truth_version: i64,
    ingest_attempt: Option<CanonicalIngestVectorWriteFence>,
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
            observed == expected_source_truth_version,
            "library source or embedding profile changed before attempt-owned vector cleanup"
        );
        observed
    } else {
        validate_exclusive_vector_delete_fence(
            &mut transaction,
            library_id,
            expected_source_truth_version,
        )
        .await?
    };
    let deleted =
        delete_chunk_vectors_in_transaction(&mut transaction, library_id, selector).await?;
    refresh_library_vector_manifest_counts_in_transaction(
        &mut transaction,
        library_id,
        CHUNK_VECTOR_RELATION_PREFIX,
    )
    .await?;
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

/// Locks the revision and proves exact canonical chunk-vector coverage inside
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
           and chunk_state = 'ready'
           and raptor_level is null",
    )
    .bind(library_id)
    .bind(revision_id)
    .fetch_one(&mut **transaction)
    .await
    .context("count canonical chunks before ingest readiness")?;
    anyhow::ensure!(chunk_count >= 0, "canonical chunk count was negative");

    let vector_count = if chunk_count == 0 {
        0
    } else {
        let profile = embedding_profile_key
            .context("non-empty revision readiness requires an exact embedding profile")?;
        validate_canonical_embedding_profile_key(profile)?;
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
        sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(canonical_chunk_vector_count_sql(
            &relation,
        )))
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
        "ingest readiness coverage mismatch for revision {revision_id}: {chunk_count} canonical chunks, {vector_count} exact-profile vectors"
    );
    Ok(())
}

async fn delete_entity_vectors_by_library_fenced_transaction(
    pool: &PgPool,
    library_id: Uuid,
    expected_source_truth_version: i64,
) -> anyhow::Result<VectorPlaneDeleteOutcome> {
    let mut transaction = pool.begin().await.context("begin fenced entity vector delete")?;
    let observed_source_truth_version = validate_exclusive_vector_delete_fence(
        &mut transaction,
        library_id,
        expected_source_truth_version,
    )
    .await?;
    let mut deleted = 0_u64;
    for relation_name in list_physical_vector_relations_in_transaction(
        &mut transaction,
        ENTITY_VECTOR_RELATION_PREFIX,
    )
    .await?
    {
        let relation = quote_identifier(&relation_name)?;
        let result = sqlx::query(sqlx::AssertSqlSafe(format!(
            "delete from {relation} where library_id = $1"
        )))
        .bind(library_id)
        .execute(&mut *transaction)
        .await
        .with_context(|| format!("delete fenced entity vectors from {relation_name}"))?;
        deleted = deleted
            .checked_add(result.rows_affected())
            .context("fenced entity vector delete count overflowed u64")?;
    }
    refresh_library_vector_manifest_counts_in_transaction(
        &mut transaction,
        library_id,
        ENTITY_VECTOR_RELATION_PREFIX,
    )
    .await?;
    let source_truth_version = advance_vector_source_truth_version(
        &mut transaction,
        library_id,
        observed_source_truth_version,
    )
    .await?;
    transaction.commit().await.context("commit fenced entity vector delete")?;
    Ok(VectorPlaneDeleteOutcome { deleted, source_truth_version })
}

async fn promote_staged_vector_kind(
    transaction: &mut Transaction<'_, Postgres>,
    library_id: Uuid,
    dimensions: u64,
    vector_kind: &str,
    relation_prefix: &str,
    canonical_embedding_model_key: &str,
    staging_embedding_model_key: &str,
    expected_count: u64,
) -> anyhow::Result<()> {
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
            // this snapshot only while their canonical source identity still
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
            .bind(canonical_embedding_model_key)
            .execute(&mut **transaction)
            .await
            .with_context(|| {
                format!("failed to replace canonical vector rows in {relation_name}")
            })?;
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
    .bind(canonical_embedding_model_key)
    .execute(&mut **transaction)
    .await
    .context("failed to promote staged vector rows")?;
    anyhow::ensure!(
        i64::try_from(promoted.rows_affected()).ok() == Some(expected_count),
        "staged vector promotion changed an unexpected row count"
    );
    let canonical_row_count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
        "select count(*)::bigint
         from {target_relation}
         where library_id = $1
           and vector_kind = $2
           and embedding_model_key = $3"
    )))
    .bind(library_id)
    .bind(vector_kind)
    .bind(canonical_embedding_model_key)
    .fetch_one(&mut **transaction)
    .await
    .context("failed to count promoted canonical vector rows")?;

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
    .bind(canonical_embedding_model_key)
    .bind(target_relation_name)
    .bind(canonical_row_count)
    .execute(&mut **transaction)
    .await
    .context("failed to install promoted vector manifest lane")?;
    Ok(())
}

#[async_trait]
impl SearchStore for PgSearchStore {
    async fn ensure_chunk_vector_shard(&self, dim: u64) -> anyhow::Result<()> {
        self.ensure_chunk_vector_relation(dim).await?;
        Ok(())
    }

    async fn ensure_chunk_vector_lane_for_library(
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
        self.upsert_chunk_vectors_bulk_immediate(std::slice::from_ref(row)).await?;
        Ok(row.clone())
    }

    async fn upsert_chunk_vectors_bulk(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()> {
        self.upsert_chunk_vectors_bulk_immediate(rows).await
    }

    async fn upsert_chunk_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeChunkVectorRow],
        fence: &CanonicalVectorWriteFence,
    ) -> anyhow::Result<i64> {
        self.upsert_chunk_vectors_bulk_fenced_immediate(rows, fence).await
    }

    async fn prepare_chunk_vector_rebuild_lane(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        validate_vector_rebuild_staging_profile_key(embedding_model_key)?;
        let relation_name = self.ensure_chunk_vector_relation(dimensions).await?;
        let mut transaction =
            self.pool.begin().await.context("begin chunk rebuild lane preparation")?;
        lock_library_vector_plane_data(&mut transaction, library_id, false).await?;
        Self::upsert_manifest_in_transaction(
            &mut transaction,
            library_id,
            dimensions,
            KNOWLEDGE_CHUNK_VECTOR_KIND,
            embedding_model_key,
            &relation_name,
        )
        .await?;
        transaction.commit().await.context("commit chunk rebuild lane preparation")?;
        Ok(())
    }

    async fn upsert_chunk_vectors_bulk_deferred_manifest(
        &self,
        rows: &[KnowledgeChunkVectorRow],
    ) -> anyhow::Result<()> {
        let Some(expected_relation_name) = deferred_chunk_vector_relation(rows)? else {
            return Ok(());
        };
        let first = rows.first().context("deferred chunk vector batch unexpectedly empty")?;
        let dimensions =
            u64::try_from(first.dimensions).context("chunk vector dimension overflowed u64")?;
        let mut transaction =
            self.pool.begin().await.context("begin deferred chunk vector batch")?;
        lock_library_vector_plane_data(&mut transaction, first.library_id, false).await?;
        let relation_name = lock_prepared_manifest_lane_for_share(
            &mut transaction,
            first.library_id,
            dimensions,
            &first.vector_kind,
            &first.embedding_model_key,
            CHUNK_VECTOR_RELATION_PREFIX,
        )
        .await?;
        anyhow::ensure!(
            relation_name == expected_relation_name,
            "prepared chunk vector manifest relation changed before batch write"
        );
        let borrowed = rows.iter().collect::<Vec<_>>();
        Self::upsert_chunk_vectors_in_relation_bulk_with_executor(
            &relation_name,
            borrowed.as_slice(),
            &mut *transaction,
        )
        .await?;
        transaction.commit().await.context("commit deferred chunk vector batch")?;
        Ok(())
    }

    async fn reconcile_chunk_vector_manifest_count(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.reconcile_manifest_count_for_lane(
            library_id,
            dimensions,
            KNOWLEDGE_CHUNK_VECTOR_KIND,
            embedding_model_key,
            CHUNK_VECTOR_RELATION_PREFIX,
        )
        .await
    }

    async fn delete_chunk_vector(
        &self,
        chunk_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<Option<KnowledgeChunkVectorRow>> {
        for relation_name in self.list_vector_relations(CHUNK_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let row = sqlx::query_as::<_, PgChunkVectorRow>(sqlx::AssertSqlSafe(format!(
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
            )))
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

    async fn delete_chunk_vectors_by_ids_fenced(
        &self,
        library_id: Uuid,
        vector_ids: &[Uuid],
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome> {
        delete_chunk_vectors_fenced(
            &self.pool,
            library_id,
            FencedChunkVectorDelete::ExactVectorIds(vector_ids),
            expected_source_truth_version,
            None,
        )
        .await?
        .context("unconditional exact-ID vector delete unexpectedly lost attempt authority")
    }

    async fn delete_attempt_owned_chunk_vectors_by_ids_fenced(
        &self,
        library_id: Uuid,
        vector_ids: &[Uuid],
        expected_source_truth_version: i64,
        ingest_attempt: CanonicalIngestVectorWriteFence,
    ) -> anyhow::Result<Option<VectorPlaneDeleteOutcome>> {
        delete_chunk_vectors_fenced(
            &self.pool,
            library_id,
            FencedChunkVectorDelete::ExactVectorIds(vector_ids),
            expected_source_truth_version,
            Some(ingest_attempt),
        )
        .await
    }

    async fn delete_chunk_vectors_by_revision_fenced(
        &self,
        library_id: Uuid,
        revision_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome> {
        delete_chunk_vectors_fenced(
            &self.pool,
            library_id,
            FencedChunkVectorDelete::Revision(revision_id),
            expected_source_truth_version,
            None,
        )
        .await?
        .context("revision-wide vector delete unexpectedly lost attempt authority")
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
            let result = sqlx::query(sqlx::AssertSqlSafe(format!("delete from {relation}")))
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
            let pg_rows = sqlx::query_as::<_, PgChunkVectorRow>(sqlx::AssertSqlSafe(format!(
                "select key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at, occurred_at, occurred_until
                 from {relation}
                 where chunk_id = $1
                 order by freshness_generation desc, created_at desc"
            )))
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
            let pg_rows = sqlx::query_as::<_, PgChunkVectorRow>(sqlx::AssertSqlSafe(format!(
                "select key, vector_id, workspace_id, library_id, chunk_id, revision_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at, occurred_at, occurred_until
                 from {relation}
                 where chunk_id = any($1::uuid[])
                   and embedding_model_key = $2
                   and vector_kind = $3
                 order by chunk_id asc, freshness_generation desc, created_at desc"
            )))
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
            let count = sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(
                canonical_chunk_vector_count_sql(&relation),
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

    async fn read_vector_profile_dimension_claim(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<Option<u64>> {
        validate_canonical_embedding_profile_key(embedding_model_key)?;
        let relation_prefix = match vector_kind {
            KNOWLEDGE_CHUNK_VECTOR_KIND => CHUNK_VECTOR_RELATION_PREFIX,
            KNOWLEDGE_ENTITY_VECTOR_KIND => ENTITY_VECTOR_RELATION_PREFIX,
            _ => anyhow::bail!("unsupported vector kind for an exact-profile dimension claim"),
        };
        let claims = sqlx::query_as::<_, (i32, String)>(
            "select dim, relation_name
             from knowledge_vector_relation_manifest
             where library_id = $1
               and embedding_model_key = $2
               and vector_kind = $3
               and embedding_model_key like 'embedding-profile:v1:%'
             order by dim
             limit 2",
        )
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(vector_kind)
        .fetch_all(&self.pool)
        .await
        .context("failed to read exact-profile vector dimension claim")?;

        validate_exact_profile_dimension_claim(relation_prefix, &claims)
    }

    async fn inspect_chunk_vector_profile(
        &self,
        library_id: Uuid,
        embedding_model_key: &str,
        vector_kind: &str,
    ) -> anyhow::Result<ChunkVectorProfileInventory> {
        let manifest_rows = sqlx::query_as::<_, (i32, String)>(
            "select dim, relation_name
             from knowledge_vector_relation_manifest
             where library_id = $1
               and embedding_model_key = $2
               and vector_kind = $3
               and relation_name like $4
             order by dim desc",
        )
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(vector_kind)
        .bind(format!("{CHUNK_VECTOR_RELATION_PREFIX}%"))
        .fetch_all(&self.pool)
        .await
        .context("failed to list chunk vector dimensions from manifest")?;

        let Some(counts_sql) = canonical_chunk_vector_dimension_counts_sql(&manifest_rows)? else {
            return Ok(ChunkVectorProfileInventory {
                dimensions: Vec::new(),
                active_vector_count: 0,
            });
        };
        let canonical_counts = sqlx::query_as::<_, (i32, i64)>(sqlx::AssertSqlSafe(counts_sql))
            .bind(library_id)
            .bind(embedding_model_key)
            .bind(vector_kind)
            .fetch_all(&self.pool)
            .await
            .context("failed to count canonical chunk vectors by dimension")?;
        chunk_vector_profile_inventory(canonical_counts)
    }

    async fn upsert_entity_vector(
        &self,
        row: &KnowledgeEntityVectorRow,
    ) -> anyhow::Result<KnowledgeEntityVectorRow> {
        self.upsert_entity_vectors_bulk_immediate(std::slice::from_ref(row)).await?;
        Ok(row.clone())
    }

    async fn upsert_entity_vectors_bulk_fenced(
        &self,
        rows: &[KnowledgeEntityVectorRow],
        fence: &CanonicalVectorWriteFence,
    ) -> anyhow::Result<i64> {
        self.upsert_entity_vectors_bulk_fenced_immediate(rows, fence).await
    }

    async fn prepare_entity_vector_rebuild_lane(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        validate_vector_rebuild_staging_profile_key(embedding_model_key)?;
        let relation_name = self.ensure_entity_vector_relation(dimensions).await?;
        let mut transaction =
            self.pool.begin().await.context("begin entity rebuild lane preparation")?;
        lock_library_vector_plane_data(&mut transaction, library_id, false).await?;
        Self::upsert_manifest_in_transaction(
            &mut transaction,
            library_id,
            dimensions,
            KNOWLEDGE_ENTITY_VECTOR_KIND,
            embedding_model_key,
            &relation_name,
        )
        .await?;
        transaction.commit().await.context("commit entity rebuild lane preparation")?;
        Ok(())
    }

    async fn upsert_entity_vectors_bulk_deferred_manifest(
        &self,
        rows: &[KnowledgeEntityVectorRow],
    ) -> anyhow::Result<()> {
        let Some(expected_relation_name) = deferred_entity_vector_relation(rows)? else {
            return Ok(());
        };
        let first = rows.first().context("deferred entity vector batch unexpectedly empty")?;
        let dimensions =
            u64::try_from(first.dimensions).context("entity vector dimension overflowed u64")?;
        let mut transaction =
            self.pool.begin().await.context("begin deferred entity vector batch")?;
        lock_library_vector_plane_data(&mut transaction, first.library_id, false).await?;
        let relation_name = lock_prepared_manifest_lane_for_share(
            &mut transaction,
            first.library_id,
            dimensions,
            &first.vector_kind,
            &first.embedding_model_key,
            ENTITY_VECTOR_RELATION_PREFIX,
        )
        .await?;
        anyhow::ensure!(
            relation_name == expected_relation_name,
            "prepared entity vector manifest relation changed before batch write"
        );
        let borrowed = rows.iter().collect::<Vec<_>>();
        Self::upsert_entity_vectors_in_relation_bulk_with_executor(
            &relation_name,
            borrowed.as_slice(),
            &mut *transaction,
        )
        .await?;
        transaction.commit().await.context("commit deferred entity vector batch")?;
        Ok(())
    }

    async fn reconcile_entity_vector_manifest_count(
        &self,
        library_id: Uuid,
        dimensions: u64,
        embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        self.reconcile_manifest_count_for_lane(
            library_id,
            dimensions,
            KNOWLEDGE_ENTITY_VECTOR_KIND,
            embedding_model_key,
            ENTITY_VECTOR_RELATION_PREFIX,
        )
        .await
    }

    async fn purge_empty_library_vector_plane(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<u64> {
        let mut transaction = self.pool.begin().await.context("begin empty vector-plane purge")?;
        // Lock order for cross-process vector-plane mutations is always:
        // per-library data serializer -> AI-config serializer -> library row.
        // Ordinary canonical writes use the shared form of this first lock.
        lock_library_vector_plane_data(&mut transaction, library_id, true).await?;
        sqlx::query(
            "select pg_advisory_xact_lock(
                hashtextextended('ironrag:ai-config-generation', 0)
             )",
        )
        .execute(&mut *transaction)
        .await
        .context("serialize empty vector purge with AI configuration changes")?;
        let observed_source_truth_version = sqlx::query_scalar::<_, i64>(
            "select source_truth_version
             from catalog_library
             where id = $1
             for update",
        )
        .bind(library_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("lock library source fence before empty vector purge")?
        .ok_or_else(|| anyhow::anyhow!("library disappeared during empty vector purge"))?;
        anyhow::ensure!(
            observed_source_truth_version == expected_source_truth_version,
            "library source truth changed before empty vector purge"
        );
        let active_chunk_count = sqlx::query_scalar::<_, i64>(
            "select count(*)::bigint
             from knowledge_chunk c
             join knowledge_document d
               on d.document_id = c.document_id
              and d.library_id = c.library_id
              and d.readable_revision_id = c.revision_id
              and d.document_state = 'active'
              and d.deleted_at is null
             where c.library_id = $1
               and c.chunk_state = 'ready'
               and c.raptor_level is null",
        )
        .bind(library_id)
        .fetch_one(&mut *transaction)
        .await
        .context("recheck canonical chunks before empty vector purge")?;
        anyhow::ensure!(active_chunk_count == 0, "library gained chunks before empty vector purge");

        let mut deleted = 0_u64;
        for relation_prefix in [CHUNK_VECTOR_RELATION_PREFIX, ENTITY_VECTOR_RELATION_PREFIX] {
            for relation_name in
                list_physical_vector_relations_in_transaction(&mut transaction, relation_prefix)
                    .await?
            {
                let relation = quote_identifier(&relation_name)?;
                let result = sqlx::query(sqlx::AssertSqlSafe(format!(
                    "delete from {relation} where library_id = $1"
                )))
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .with_context(|| {
                    format!("delete empty-library vector rows from {relation_name}")
                })?;
                deleted = deleted
                    .checked_add(result.rows_affected())
                    .context("empty-library vector purge count overflowed u64")?;
            }
        }
        let deleted_manifests =
            sqlx::query("delete from knowledge_vector_relation_manifest where library_id = $1")
                .bind(library_id)
                .execute(&mut *transaction)
                .await
                .context("delete empty-library vector manifests")?;
        if deleted > 0 || deleted_manifests.rows_affected() > 0 {
            let advanced = sqlx::query_scalar::<_, i64>(
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
            .bind(expected_source_truth_version)
            .fetch_optional(&mut *transaction)
            .await
            .context("advance source fence with empty vector purge")?;
            anyhow::ensure!(
                advanced.is_some(),
                "library source fence changed before empty vector purge"
            );
        }
        transaction.commit().await.context("commit empty vector-plane purge")?;
        Ok(deleted)
    }

    async fn promote_staged_vector_rebuild(
        &self,
        library_id: Uuid,
        dimensions: u64,
        canonical_embedding_model_key: &str,
        staging_embedding_model_key: &str,
        expected_source_truth_version: i64,
        expected_chunk_count: Option<u64>,
        expected_entity_count: Option<u64>,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            canonical_embedding_model_key != staging_embedding_model_key,
            "canonical and staging embedding profiles must be distinct"
        );
        validate_vector_rebuild_staging_profile_key(staging_embedding_model_key)?;
        anyhow::ensure!(
            expected_chunk_count.is_some() || expected_entity_count.is_some(),
            "staged vector promotion requires at least one lane"
        );
        let mut transaction = self.pool.begin().await.context("begin staged vector promotion")?;
        lock_library_vector_plane_data(&mut transaction, library_id, true).await?;
        // AI-binding trigger transactions acquire this serializer before
        // taking child and catalog-library locks. Promotion must enter the
        // same global order before comparing the source fence; otherwise a
        // binding change can wait behind the library row and commit an
        // obsolete vector profile immediately after promotion.
        sqlx::query(
            "select pg_advisory_xact_lock(
                hashtextextended('ironrag:ai-config-generation', 0)
             )",
        )
        .execute(&mut *transaction)
        .await
        .context("serialize staged promotion with AI configuration changes")?;
        let observed_source_truth_version = sqlx::query_scalar::<_, i64>(
            "select source_truth_version
             from catalog_library
             where id = $1
             for update",
        )
        .bind(library_id)
        .fetch_optional(&mut *transaction)
        .await
        .context("lock library source fence before staged vector promotion")?
        .ok_or_else(|| anyhow::anyhow!("library disappeared during staged vector promotion"))?;
        anyhow::ensure!(
            observed_source_truth_version == expected_source_truth_version,
            "library source truth changed during staged vector rebuild"
        );

        if let Some(expected_count) = expected_chunk_count {
            promote_staged_vector_kind(
                &mut transaction,
                library_id,
                dimensions,
                KNOWLEDGE_CHUNK_VECTOR_KIND,
                CHUNK_VECTOR_RELATION_PREFIX,
                canonical_embedding_model_key,
                staging_embedding_model_key,
                expected_count,
            )
            .await?;
        }
        if let Some(expected_count) = expected_entity_count {
            promote_staged_vector_kind(
                &mut transaction,
                library_id,
                dimensions,
                KNOWLEDGE_ENTITY_VECTOR_KIND,
                ENTITY_VECTOR_RELATION_PREFIX,
                canonical_embedding_model_key,
                staging_embedding_model_key,
                expected_count,
            )
            .await?;
        }
        let advanced = sqlx::query_scalar::<_, i64>(
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
        .bind(expected_source_truth_version)
        .fetch_optional(&mut *transaction)
        .await
        .context("advance library source fence with staged vector promotion")?;
        anyhow::ensure!(advanced.is_some(), "library source fence changed before vector promotion");
        transaction.commit().await.context("commit staged vector promotion")?;
        Ok(())
    }

    async fn discard_staged_vector_rebuild(
        &self,
        library_id: Uuid,
        dimensions: u64,
        staging_embedding_model_key: &str,
    ) -> anyhow::Result<()> {
        let mut transaction = self.pool.begin().await.context("begin staged vector cleanup")?;
        // Cleanup refreshes canonical manifest counts for touched physical
        // relations, so it participates in the same short exclusive data
        // boundary as purge/promotion in addition to the rebuild session lock.
        lock_library_vector_plane_data(&mut transaction, library_id, true).await?;
        let touched_relations = discard_staged_vector_rebuild_in_transaction(
            &mut transaction,
            library_id,
            dimensions,
            staging_embedding_model_key,
        )
        .await?;
        delete_source_less_vectors_in_touched_relations(
            &mut transaction,
            library_id,
            &touched_relations,
        )
        .await?;
        transaction.commit().await.context("commit staged vector cleanup")?;
        Ok(())
    }

    async fn discard_abandoned_staged_vector_rebuilds(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<u64> {
        let limit = i64::try_from(MAX_ABANDONED_VECTOR_REBUILD_MANIFEST_ROWS)
            .context("abandoned staging scan limit overflowed i64")?;
        let mut discarded_profiles = 0_u64;
        loop {
            let mut transaction =
                self.pool.begin().await.context("begin abandoned staging cleanup batch")?;
            lock_library_vector_plane_data(&mut transaction, library_id, true).await?;
            let rows = sqlx::query_as::<_, (i32, String)>(
                "select dim, embedding_model_key
                 from knowledge_vector_relation_manifest
                 where library_id = $1
                   and promoted = false
                   and embedding_model_key like $2
                 order by dim, embedding_model_key, vector_kind
                 limit $3
                 for update",
            )
            .bind(library_id)
            .bind(format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}%"))
            .bind(limit)
            .fetch_all(&mut *transaction)
            .await
            .context("scan abandoned vector rebuild manifest batch")?;
            if rows.is_empty() {
                transaction.commit().await.context("commit empty abandoned staging scan")?;
                break;
            }
            let staging_profiles = rows
                .into_iter()
                .map(|(dimensions, profile_key)| {
                    validate_vector_rebuild_staging_profile_key(&profile_key)?;
                    Ok((
                        u64::try_from(dimensions)
                            .context("abandoned staging dimension was negative")?,
                        profile_key,
                    ))
                })
                .collect::<anyhow::Result<BTreeSet<_>>>()?;
            let mut touched_relations = BTreeSet::new();
            for (dimensions, profile_key) in &staging_profiles {
                touched_relations.extend(
                    discard_staged_vector_rebuild_in_transaction(
                        &mut transaction,
                        library_id,
                        *dimensions,
                        profile_key,
                    )
                    .await?,
                );
            }
            delete_source_less_vectors_in_touched_relations(
                &mut transaction,
                library_id,
                &touched_relations,
            )
            .await?;
            transaction.commit().await.context("commit abandoned staging cleanup batch")?;
            let batch_profiles = u64::try_from(staging_profiles.len())
                .context("abandoned staging batch count overflowed u64")?;
            discarded_profiles = discarded_profiles
                .checked_add(batch_profiles)
                .context("abandoned staging cleanup count overflowed u64")?;
        }
        Ok(discarded_profiles)
    }

    async fn delete_entity_vector(
        &self,
        entity_id: Uuid,
        embedding_model_key: &str,
        freshness_generation: i64,
    ) -> anyhow::Result<Option<KnowledgeEntityVectorRow>> {
        for relation_name in self.list_vector_relations(ENTITY_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let row = sqlx::query_as::<_, PgEntityVectorRow>(sqlx::AssertSqlSafe(format!(
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
            )))
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

    async fn delete_entity_vectors_by_library_fenced(
        &self,
        library_id: Uuid,
        expected_source_truth_version: i64,
    ) -> anyhow::Result<VectorPlaneDeleteOutcome> {
        delete_entity_vectors_by_library_fenced_transaction(
            &self.pool,
            library_id,
            expected_source_truth_version,
        )
        .await
    }

    async fn delete_all_entity_vectors(&self) -> anyhow::Result<u64> {
        let mut total = 0_u64;
        for relation_name in self.list_vector_relations(ENTITY_VECTOR_RELATION_PREFIX).await? {
            let relation = quote_identifier(&relation_name)?;
            let result = sqlx::query(sqlx::AssertSqlSafe(format!("delete from {relation}")))
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
            let pg_rows = sqlx::query_as::<_, PgEntityVectorRow>(sqlx::AssertSqlSafe(format!(
                "select key, vector_id, workspace_id, library_id, entity_id,
                    embedding_model_key, vector_kind, dimensions, embedding::text as vector_text,
                    freshness_generation, created_at
                 from {relation}
                 where entity_id = $1
                 order by freshness_generation desc, created_at desc
                 limit 1000"
            )))
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

    async fn search_chunks_with_config(
        &self,
        library_id: Uuid,
        query: &str,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        text_search_config: &str,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        if query.trim().is_empty() {
            return Ok(Vec::new());
        }
        let (sql_exact, sql_prefix) = chunk_lexical_sql(text_search_config);
        let query_terms = lexical_query_terms(query);
        let title_ngram_terms = title_ngram_terms(&query_terms);
        let title_identity_terms = title_identity_terms(query, &query_terms);
        let title_soft_raw_enabled = title_soft_raw_enabled(&query_terms);
        let query_terms = query_terms.as_slice();
        let title_ngram_terms = title_ngram_terms.as_slice();
        let title_identity_terms = title_identity_terms.as_slice();
        run_lexical_ladder(
            query,
            &sql_exact,
            &sql_prefix,
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
        let (exact_sql, prefix_sql) = structured_block_lexical_sql();
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeStructuredBlockSearchRow| row.block_id,
            |sql, fts_input| async move {
                let rows =
                    sqlx::query_as::<_, PgStructuredBlockSearchRow>(sqlx::AssertSqlSafe(&*sql))
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
        let (exact_sql, prefix_sql) = technical_fact_lexical_sql();
        run_lexical_ladder(
            query,
            &exact_sql,
            &prefix_sql,
            |row: &KnowledgeTechnicalFactSearchRow| row.fact_id,
            |sql, fts_input| {
                let query_exact = query_exact.clone();
                async move {
                    let rows =
                        sqlx::query_as::<_, PgTechnicalFactSearchRow>(sqlx::AssertSqlSafe(&*sql))
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
                let rows = sqlx::query_as::<_, PgEntitySearchRow>(sqlx::AssertSqlSafe(&*sql))
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
                let rows = sqlx::query_as::<_, PgRelationSearchRow>(sqlx::AssertSqlSafe(&*sql))
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
        let Some(search_lane) = self
            .resolve_manifest_search_lane(
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
        let relation_name = search_lane.relation_name;
        let manifest_row_count = search_lane.manifest_row_count;
        let relation = quote_identifier(&relation_name)?;
        let query_literal = pgvector_literal(query_vector)?;
        let storage = PgVectorStorage::for_dim(dim);
        let cast_type = storage.cast_type();
        let search_params = pg_hnsw_search_params(n_probe);
        let query_limit = limit.max(1);
        let query_limit_i64 =
            i64::try_from(query_limit).context("chunk ANN limit overflowed i64")?;
        let mut tx = self.pool.begin().await?;
        configure_hnsw_search(&mut tx, search_params).await?;
        let mut rows = sqlx::query_as::<_, PgChunkVectorSearchRow>(sqlx::AssertSqlSafe(
            chunk_vector_similarity_sql(&relation, cast_type),
        ))
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(&query_literal)
        .bind(temporal_start.as_ref())
        .bind(temporal_end.as_ref())
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .bind(query_limit_i64)
        .fetch_all(&mut *tx)
        .await
        .with_context(|| format!("failed to search chunk vectors in {relation_name}"))?;
        if rows.len() < query_limit
            && u64::try_from(query_limit).is_ok_and(|value| manifest_row_count >= value)
        {
            let approximate_result_count = rows.len();
            let exact_fallback_max_rows = pg_hnsw_exact_fallback_max_rows();
            if should_retry_exact_ann(
                approximate_result_count,
                query_limit,
                manifest_row_count,
                exact_fallback_max_rows,
            ) {
                rows = sqlx::query_as::<_, PgChunkVectorSearchRow>(sqlx::AssertSqlSafe(
                    chunk_vector_exact_similarity_sql(&relation, cast_type),
                ))
                .bind(library_id)
                .bind(embedding_model_key)
                .bind(&query_literal)
                .bind(temporal_start.as_ref())
                .bind(temporal_end.as_ref())
                .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
                .bind(query_limit_i64)
                .fetch_all(&mut *tx)
                .await
                .with_context(|| {
                    format!("failed bounded exact chunk-vector retry in {relation_name}")
                })?;
                tracing::warn!(
                    library_id = %library_id,
                    approximate_result_count,
                    exact_result_count = rows.len(),
                    requested_limit = query_limit,
                    manifest_row_count,
                    ef_search = search_params.ef_search,
                    max_scan_tuples = search_params.max_scan_tuples,
                    scan_mem_multiplier = search_params.scan_mem_multiplier,
                    "filtered HNSW chunk search underfilled and used a bounded exact retry"
                );
            } else {
                tracing::warn!(
                    library_id = %library_id,
                    approximate_result_count,
                    requested_limit = query_limit,
                    manifest_row_count,
                    exact_fallback_max_rows,
                    ef_search = search_params.ef_search,
                    max_scan_tuples = search_params.max_scan_tuples,
                    scan_mem_multiplier = search_params.scan_mem_multiplier,
                    "filtered HNSW chunk search underfilled at the configured scan bound; exact retry skipped"
                );
            }
        }
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
        let Some(search_lane) = self
            .resolve_manifest_search_lane(
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
        let relation_name = search_lane.relation_name;
        let manifest_row_count = search_lane.manifest_row_count;
        let relation = quote_identifier(&relation_name)?;
        let query_literal = pgvector_literal(query_vector)?;
        let storage = PgVectorStorage::for_dim(dim);
        let cast_type = storage.cast_type();
        let search_params = pg_hnsw_search_params(n_probe);
        let query_limit = limit.max(1);
        let query_limit_i64 =
            i64::try_from(query_limit).context("entity ANN limit overflowed i64")?;
        let mut tx = self.pool.begin().await?;
        configure_hnsw_search(&mut tx, search_params).await?;
        let mut rows = sqlx::query_as::<_, PgEntityVectorSearchRow>(sqlx::AssertSqlSafe(
            entity_vector_similarity_sql(&relation, cast_type),
        ))
        .bind(library_id)
        .bind(embedding_model_key)
        .bind(&query_literal)
        .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
        .bind(query_limit_i64)
        .fetch_all(&mut *tx)
        .await
        .with_context(|| format!("failed to search entity vectors in {relation_name}"))?;
        if rows.len() < query_limit
            && u64::try_from(query_limit).is_ok_and(|value| manifest_row_count >= value)
        {
            let approximate_result_count = rows.len();
            let exact_fallback_max_rows = pg_hnsw_exact_fallback_max_rows();
            if should_retry_exact_ann(
                approximate_result_count,
                query_limit,
                manifest_row_count,
                exact_fallback_max_rows,
            ) {
                rows = sqlx::query_as::<_, PgEntityVectorSearchRow>(sqlx::AssertSqlSafe(
                    entity_vector_exact_similarity_sql(&relation, cast_type),
                ))
                .bind(library_id)
                .bind(embedding_model_key)
                .bind(&query_literal)
                .bind(KNOWLEDGE_ENTITY_VECTOR_KIND)
                .bind(query_limit_i64)
                .fetch_all(&mut *tx)
                .await
                .with_context(|| {
                    format!("failed bounded exact entity-vector retry in {relation_name}")
                })?;
                tracing::warn!(
                    library_id = %library_id,
                    approximate_result_count,
                    exact_result_count = rows.len(),
                    requested_limit = query_limit,
                    manifest_row_count,
                    ef_search = search_params.ef_search,
                    max_scan_tuples = search_params.max_scan_tuples,
                    scan_mem_multiplier = search_params.scan_mem_multiplier,
                    "filtered HNSW entity search underfilled and used a bounded exact retry"
                );
            } else {
                tracing::warn!(
                    library_id = %library_id,
                    approximate_result_count,
                    requested_limit = query_limit,
                    manifest_row_count,
                    exact_fallback_max_rows,
                    ef_search = search_params.ef_search,
                    max_scan_tuples = search_params.max_scan_tuples,
                    scan_mem_multiplier = search_params.scan_mem_multiplier,
                    "filtered HNSW entity search underfilled at the configured scan bound; exact retry skipped"
                );
            }
        }
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
        let result =
            sqlx::query(sqlx::AssertSqlSafe(format!("delete from {relation} where {predicate}")))
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PgHnswSearchParams {
    ef_search: u64,
    max_scan_tuples: u64,
    scan_mem_multiplier: u64,
}

const HNSW_SEARCH_CONFIG_SQL: &str = "select
    set_config('hnsw.ef_search', $1, true),
    set_config('hnsw.iterative_scan', 'strict_order', true),
    set_config('hnsw.max_scan_tuples', $2, true),
    set_config('hnsw.scan_mem_multiplier', $3, true)";

fn pg_hnsw_search_params_from_overrides(
    n_probe: Option<u64>,
    configured_ef_search: Option<u64>,
    configured_max_scan_tuples: Option<u64>,
    configured_scan_mem_multiplier: Option<u64>,
) -> PgHnswSearchParams {
    PgHnswSearchParams {
        ef_search: n_probe
            .or(configured_ef_search)
            .unwrap_or(PG_HNSW_DEFAULT_EF_SEARCH)
            .clamp(1, PG_HNSW_MAX_EF_SEARCH),
        max_scan_tuples: configured_max_scan_tuples
            .unwrap_or(PG_HNSW_DEFAULT_MAX_SCAN_TUPLES)
            .clamp(1, PG_HNSW_MAX_SCAN_TUPLES),
        scan_mem_multiplier: configured_scan_mem_multiplier
            .unwrap_or(PG_HNSW_DEFAULT_SCAN_MEM_MULTIPLIER)
            .clamp(1, PG_HNSW_MAX_SCAN_MEM_MULTIPLIER),
    }
}

fn pg_hnsw_search_params(n_probe: Option<u64>) -> PgHnswSearchParams {
    pg_hnsw_search_params_from_overrides(
        n_probe,
        read_env_u64("IRONRAG_PG_HNSW_EF_SEARCH"),
        read_env_u64("IRONRAG_PG_HNSW_MAX_SCAN_TUPLES"),
        read_env_u64("IRONRAG_PG_HNSW_SCAN_MEM_MULTIPLIER"),
    )
}

fn pg_hnsw_exact_fallback_max_rows() -> u64 {
    pg_hnsw_exact_fallback_max_rows_from_override(read_env_u64_allow_zero(
        "IRONRAG_PG_HNSW_EXACT_FALLBACK_MAX_ROWS",
    ))
}

fn pg_hnsw_exact_fallback_max_rows_from_override(configured: Option<u64>) -> u64 {
    configured
        .unwrap_or(PG_HNSW_DEFAULT_EXACT_FALLBACK_MAX_ROWS)
        .min(PG_HNSW_MAX_EXACT_FALLBACK_ROWS)
}

fn should_retry_exact_ann(
    result_count: usize,
    requested_limit: usize,
    manifest_row_count: u64,
    exact_fallback_max_rows: u64,
) -> bool {
    let requested_limit = requested_limit.max(1);
    result_count < requested_limit
        && u64::try_from(requested_limit).is_ok_and(|limit| manifest_row_count >= limit)
        && exact_fallback_max_rows > 0
        && manifest_row_count <= exact_fallback_max_rows
}

async fn configure_hnsw_search(
    transaction: &mut Transaction<'_, Postgres>,
    params: PgHnswSearchParams,
) -> anyhow::Result<()> {
    // pgvector applies ordinary filters after an approximate HNSW scan. During
    // a rebuild, opaque staging rows share the dimension shard with canonical
    // rows, so a fixed candidate list can otherwise be consumed by staging.
    // PostgreSQL 18 is the project baseline and its pgvector image is >= 0.8.1;
    // strict iterative scans (introduced in 0.8.0) continue until the filtered
    // LIMIT is satisfied or pgvector's configured scan bound is reached.
    sqlx::query(HNSW_SEARCH_CONFIG_SQL)
        .bind(params.ef_search.to_string())
        .bind(params.max_scan_tuples.to_string())
        .bind(params.scan_mem_multiplier.to_string())
        .execute(&mut **transaction)
        .await
        .context("failed to configure filtered HNSW search")?;
    Ok(())
}

fn read_env_u64_allow_zero(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|value| value.trim().parse::<u64>().ok())
}

fn checked_dim_i32(dim: u64) -> anyhow::Result<i32> {
    anyhow::ensure!(dim > 0, "vector dimension must be positive");
    anyhow::ensure!(
        dim <= PGVECTOR_MAX_INDEXED_DIM,
        "vector dimension exceeds the indexed storage limit"
    );
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

fn validate_deferred_chunk_manifest_lane(rows: &[KnowledgeChunkVectorRow]) -> anyhow::Result<()> {
    let Some(first) = rows.first() else {
        return Ok(());
    };
    anyhow::ensure!(
        first.vector_kind == KNOWLEDGE_CHUNK_VECTOR_KIND,
        "deferred chunk rebuild requires the canonical chunk vector kind"
    );
    anyhow::ensure!(
        rows.iter().all(|row| {
            row.library_id == first.library_id
                && row.dimensions == first.dimensions
                && row.vector_kind == first.vector_kind
                && row.embedding_model_key == first.embedding_model_key
        }),
        "deferred chunk vector batch spans multiple manifest lanes"
    );
    Ok(())
}

fn validate_deferred_entity_manifest_lane(rows: &[KnowledgeEntityVectorRow]) -> anyhow::Result<()> {
    let Some(first) = rows.first() else {
        return Ok(());
    };
    anyhow::ensure!(
        first.vector_kind == KNOWLEDGE_ENTITY_VECTOR_KIND,
        "deferred entity rebuild requires the canonical entity vector kind"
    );
    anyhow::ensure!(
        rows.iter().all(|row| {
            row.library_id == first.library_id
                && row.dimensions == first.dimensions
                && row.vector_kind == first.vector_kind
                && row.embedding_model_key == first.embedding_model_key
        }),
        "deferred entity vector batch spans multiple manifest lanes"
    );
    Ok(())
}

fn deferred_chunk_vector_relation(
    rows: &[KnowledgeChunkVectorRow],
) -> anyhow::Result<Option<String>> {
    validate_deferred_chunk_manifest_lane(rows)?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    for row in rows {
        validate_row_vector_dimensions(row.dimensions, &row.vector, "chunk")?;
    }
    Ok(Some(vector_relation_name(
        CHUNK_VECTOR_RELATION_PREFIX,
        u64::try_from(first.dimensions).context("chunk vector dimension overflowed u64")?,
    )?))
}

fn deferred_entity_vector_relation(
    rows: &[KnowledgeEntityVectorRow],
) -> anyhow::Result<Option<String>> {
    validate_deferred_entity_manifest_lane(rows)?;
    let Some(first) = rows.first() else {
        return Ok(None);
    };
    for row in rows {
        validate_row_vector_dimensions(row.dimensions, &row.vector, "entity")?;
    }
    Ok(Some(vector_relation_name(
        ENTITY_VECTOR_RELATION_PREFIX,
        u64::try_from(first.dimensions).context("entity vector dimension overflowed u64")?,
    )?))
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

fn vector_relation_required_objects(
    relation_name: &str,
    id_column: &str,
    extra_column: Option<&str>,
) -> Vec<String> {
    let mut objects = vec![
        relation_name.to_string(),
        format!("{relation_name}_logical_key"),
        format!("{relation_name}_lane_idx"),
        format!("{relation_name}_{id_column}_idx"),
        format!("{relation_name}_hnsw"),
    ];
    if let Some(extra_column) = extra_column {
        objects.push(format!("{relation_name}_{extra_column}_idx"));
    }
    objects
}

fn validate_relation_name(relation_name: &str, expected_prefix: &str) -> anyhow::Result<()> {
    let dimension = relation_name
        .strip_prefix(expected_prefix)
        .filter(|suffix| !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit()))
        .context("vector relation does not follow the dimension-shard protocol")?
        .parse::<u64>()
        .context("vector relation dimension overflowed u64")?;
    anyhow::ensure!(
        vector_relation_name(expected_prefix, dimension)? == relation_name,
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

const fn title_soft_raw_enabled(query_terms: &[String]) -> bool {
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
const fn should_relax_lexical(hit_count: usize) -> bool {
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
/// returning `(exact_sql, prefix_sql)`. Both bind the FTS input via `$2`. Uses the
/// historical default text-search config, so output is byte-identical to the
/// original hardcoded SQL.
fn lexical_lane_sql(template: &str) -> (String, String) {
    lexical_lane_sql_for_config(template, DEFAULT_TEXT_SEARCH_CONFIG)
}

/// Like [`lexical_lane_sql`] but renders the lexical lane against an explicit
/// Postgres text-search config name (sourced from a library's retrieval config).
///
/// The config name is sanitized to a conservative identifier shape before it is
/// embedded in the SQL string literal — Postgres `regconfig` names are unquoted
/// identifiers, so they cannot be bound as a `$n` parameter and must be rendered
/// inline. The API boundary additionally rejects names absent from `pg_ts_config`;
/// an unexpected value here falls back to the historical default rather than
/// emitting unsafe SQL.
fn lexical_lane_sql_for_config(template: &str, text_search_config: &str) -> (String, String) {
    let config = sanitize_text_search_config(text_search_config);
    (
        template
            .replace("{FTS}", &format!("websearch_to_tsquery('{config}', ironrag_unaccent($2))")),
        template.replace("{FTS}", &format!("to_tsquery('{config}', ironrag_unaccent($2))")),
    )
}

/// Returns the text-search config name when it is a safe Postgres identifier
/// (ASCII letters, digits, and underscores, not starting with a digit), otherwise
/// the historical default. This is defence-in-depth: write-time validation already
/// rejects names missing from `pg_ts_config`, but the name is rendered into a SQL
/// string literal so it must never carry quote or statement-terminator characters.
fn sanitize_text_search_config(text_search_config: &str) -> &str {
    let is_safe = !text_search_config.is_empty()
        && !text_search_config.starts_with(|ch: char| ch.is_ascii_digit())
        && text_search_config.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    if is_safe { text_search_config } else { DEFAULT_TEXT_SEARCH_CONFIG }
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
    fn chunk_exact_template_keeps_precise_fts_constructor() {
        // Pass A remains the precise query; the relaxed template differs only in
        // the FTS constructor while shared safety filters stay identical.
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

    #[test]
    fn chunk_lexical_sql_excludes_legacy_raptor_rows_from_every_lane() {
        for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
            assert_eq!(
                sql.matches("and c.raptor_level is null").count(),
                3,
                "text, title-identity, and soft-title lanes must all quarantine legacy summaries"
            );
        }
    }

    #[test]
    fn chunk_lexical_sql_filters_stale_revisions_before_every_lane_limit() {
        for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
            assert!(sql.contains("d.readable_revision_id is not null"));
            assert!(sql.contains("d.deleted_at is null"));
            assert_eq!(sql.matches("join readable_docs rd").count(), 3);
            assert_eq!(sql.matches("rd.readable_revision_id = c.revision_id").count(), 3);
            assert_eq!(
                sql.matches("and d.readable_revision_id is not null").count(),
                3,
                "readable-doc, exact-title, and soft-title candidates must filter unreadable documents before their local limits",
            );
            assert_eq!(
                sql.matches("and d.deleted_at is null").count(),
                3,
                "deleted title matches must not consume a bounded title lane before canonical chunk hydration",
            );
        }
    }

    #[test]
    fn structured_block_lexical_sql_filters_noncanonical_revisions_before_limit() {
        let (exact_sql, prefix_sql) = structured_block_lexical_sql();
        for sql in [&exact_sql, &prefix_sql] {
            assert!(sql.contains("from knowledge_structured_block b"));
            assert!(sql.contains("join knowledge_document d"));
            assert!(sql.contains("d.document_id = b.document_id"));
            assert!(sql.contains("d.library_id = b.library_id"));
            assert!(sql.contains("d.readable_revision_id = b.revision_id"));
            assert!(sql.contains("d.document_state = 'active'"));
            assert!(sql.contains("d.deleted_at is null"));
            assert!(
                sql.find("d.deleted_at is null").unwrap() < sql.rfind("limit $3").unwrap(),
                "canonical document filters must run before the bounded result set",
            );
        }
    }

    #[test]
    fn technical_fact_lexical_sql_filters_noncanonical_revisions_before_limit() {
        let (exact_sql, prefix_sql) = technical_fact_lexical_sql();
        for sql in [&exact_sql, &prefix_sql] {
            assert!(sql.contains("from knowledge_technical_fact f"));
            assert!(sql.contains("join knowledge_document d"));
            assert!(sql.contains("d.document_id = f.document_id"));
            assert!(sql.contains("d.library_id = f.library_id"));
            assert!(sql.contains("d.readable_revision_id = f.revision_id"));
            assert!(sql.contains("d.document_state = 'active'"));
            assert!(sql.contains("d.deleted_at is null"));
            assert!(
                sql.find("d.deleted_at is null").unwrap() < sql.rfind("limit $4").unwrap(),
                "canonical document filters must run before the bounded result set",
            );
        }
    }

    #[test]
    fn chunk_vector_sql_excludes_legacy_raptor_rows_before_top_k() {
        let sql = chunk_vector_similarity_sql("\"knowledge_chunk_vector_3\"", "vector(3)");

        assert!(sql.contains("join knowledge_chunk c"));
        assert!(sql.contains("join knowledge_document d"));
        assert!(sql.contains("d.readable_revision_id = v.revision_id"));
        assert!(sql.contains("d.document_state = 'active'"));
        assert!(sql.contains("d.deleted_at is null"));
        assert!(sql.contains("and c.chunk_state = 'ready'"));
        assert!(sql.contains("and c.raptor_level is null"));
        assert!(sql.contains("order by v.embedding <=> $3::vector(3)"));
    }

    #[test]
    fn entity_vector_sql_filters_inactive_entities_before_top_k() {
        let sql = entity_vector_similarity_sql("\"knowledge_entity_vector_d3\"", "vector(3)");

        assert!(sql.contains("from \"knowledge_entity_vector_d3\" v"));
        assert!(sql.contains("join knowledge_entity e"));
        assert!(sql.contains("e.entity_id = v.entity_id"));
        assert!(sql.contains("e.library_id = v.library_id"));
        assert!(sql.contains("e.entity_state = 'active'"));
        assert!(
            sql.find("e.entity_state = 'active'").unwrap() < sql.rfind("limit $5").unwrap(),
            "inactive entity vectors must not consume the bounded ANN result set",
        );
    }

    #[test]
    fn filtered_hnsw_search_parameters_are_explicit_and_bounded() {
        let defaults = pg_hnsw_search_params_from_overrides(None, None, None, None);
        assert_eq!(defaults.ef_search, PG_HNSW_DEFAULT_EF_SEARCH);
        assert_eq!(defaults.max_scan_tuples, PG_HNSW_DEFAULT_MAX_SCAN_TUPLES);
        assert_eq!(defaults.scan_mem_multiplier, PG_HNSW_DEFAULT_SCAN_MEM_MULTIPLIER);

        let bounded = pg_hnsw_search_params_from_overrides(
            Some(u64::MAX),
            Some(7),
            Some(u64::MAX),
            Some(u64::MAX),
        );
        assert_eq!(bounded.ef_search, PG_HNSW_MAX_EF_SEARCH);
        assert_eq!(bounded.max_scan_tuples, PG_HNSW_MAX_SCAN_TUPLES);
        assert_eq!(bounded.scan_mem_multiplier, PG_HNSW_MAX_SCAN_MEM_MULTIPLIER);

        let minimums = pg_hnsw_search_params_from_overrides(None, Some(0), Some(0), Some(0));
        assert_eq!(minimums.ef_search, 1);
        assert_eq!(minimums.max_scan_tuples, 1);
        assert_eq!(minimums.scan_mem_multiplier, 1);

        let configured =
            pg_hnsw_search_params_from_overrides(None, Some(123), Some(45_000), Some(3));
        assert_eq!(configured.ef_search, 123);
        assert_eq!(configured.max_scan_tuples, 45_000);
        assert_eq!(configured.scan_mem_multiplier, 3);

        let request_override =
            pg_hnsw_search_params_from_overrides(Some(321), Some(123), Some(45_000), Some(3));
        assert_eq!(request_override.ef_search, 321);
    }

    #[test]
    fn exact_fallback_threshold_is_explicit_disableable_and_bounded() {
        assert_eq!(
            pg_hnsw_exact_fallback_max_rows_from_override(None),
            PG_HNSW_DEFAULT_EXACT_FALLBACK_MAX_ROWS
        );
        assert_eq!(pg_hnsw_exact_fallback_max_rows_from_override(Some(0)), 0);
        assert_eq!(
            pg_hnsw_exact_fallback_max_rows_from_override(Some(u64::MAX)),
            PG_HNSW_MAX_EXACT_FALLBACK_ROWS
        );
    }

    #[test]
    fn filtered_hnsw_configuration_sets_every_bounded_pgvector_knob() {
        for setting in [
            "hnsw.ef_search",
            "hnsw.iterative_scan",
            "hnsw.max_scan_tuples",
            "hnsw.scan_mem_multiplier",
        ] {
            assert!(HNSW_SEARCH_CONFIG_SQL.contains(setting));
        }
    }

    #[test]
    fn exact_retry_is_limited_to_underfilled_small_manifest_lanes() {
        assert!(should_retry_exact_ann(3, 8, 1_000, 10_000));
        assert!(!should_retry_exact_ann(8, 8, 1_000, 10_000));
        assert!(!should_retry_exact_ann(3, 8, 7, 10_000));
        assert!(!should_retry_exact_ann(3, 8, 10_001, 10_000));
        assert!(!should_retry_exact_ann(3, 8, 1_000, 0));
    }

    #[test]
    fn exact_retry_sql_disables_hnsw_ordering_without_removing_lane_filters() {
        let chunk_sql =
            chunk_vector_exact_similarity_sql("\"knowledge_chunk_vector_d3\"", "vector(3)");
        assert!(chunk_sql.contains("v.embedding_model_key = $2"));
        assert!(chunk_sql.contains("v.vector_kind = $6"));
        assert!(chunk_sql.contains("order by (v.embedding <=> $3::vector(3)) + 0.0"));

        let entity_sql =
            entity_vector_exact_similarity_sql("\"knowledge_entity_vector_d3\"", "vector(3)");
        assert!(entity_sql.contains("v.embedding_model_key = $2"));
        assert!(entity_sql.contains("v.vector_kind = $4"));
        assert!(entity_sql.contains("order by (v.embedding <=> $3::vector(3)) + 0.0"));
    }

    #[test]
    fn chunk_vector_coverage_sql_counts_only_canonical_chunks() {
        let sql = canonical_chunk_vector_count_sql("\"knowledge_chunk_vector_d3\"");

        assert!(sql.contains("from \"knowledge_chunk_vector_d3\" v"));
        assert!(sql.contains("join knowledge_chunk c"));
        assert!(sql.contains("c.chunk_id = v.chunk_id"));
        assert!(sql.contains("c.revision_id = v.revision_id"));
        assert!(sql.contains("c.library_id = v.library_id"));
        assert!(sql.contains("and c.chunk_state = 'ready'"));
        assert!(sql.contains("and c.raptor_level is null"));
    }

    #[test]
    fn chunk_vector_dimension_sql_uses_one_set_query_and_ignores_legacy_only_shards() {
        let sql = canonical_chunk_vector_dimension_counts_sql(&[
            (3, "knowledge_chunk_vector_d3".to_string()),
            (7, "knowledge_chunk_vector_d7".to_string()),
        ])
        .unwrap()
        .unwrap();

        assert!(sql.contains("from \"knowledge_chunk_vector_d3\" v"));
        assert!(sql.contains("from \"knowledge_chunk_vector_d7\" v"));
        assert_eq!(sql.matches("union all").count(), 1);
        assert_eq!(sql.matches("count(distinct v.chunk_id)").count(), 2);
        assert!(sql.contains("join knowledge_chunk c"));
        assert!(sql.contains("c.chunk_id = v.chunk_id"));
        assert!(sql.contains("c.revision_id = v.revision_id"));
        assert!(sql.contains("c.library_id = v.library_id"));
        assert_eq!(sql.matches("and c.chunk_state = 'ready'").count(), 2);
        assert_eq!(sql.matches("and c.raptor_level is null").count(), 2);
        assert_eq!(sql.matches("join knowledge_document d").count(), 2);
        assert_eq!(sql.matches("d.readable_revision_id = v.revision_id").count(), 2);
        assert_eq!(sql.matches("d.document_state = 'active'").count(), 2);
        assert_eq!(sql.matches("d.deleted_at is null").count(), 2);
        assert!(canonical_chunk_vector_dimension_counts_sql(&[]).unwrap().is_none());
    }

    #[test]
    fn chunk_vector_dimension_sql_rejects_manifest_dimension_relation_mismatch() {
        let result = canonical_chunk_vector_dimension_counts_sql(&[(
            1536,
            "knowledge_chunk_vector_d768".to_string(),
        )]);

        assert!(result.is_err());
    }

    #[test]
    fn canonical_chunk_vector_dimensions_drop_empty_and_rank_by_live_rows() {
        let dimensions =
            rank_canonical_chunk_vector_dimensions(vec![(384, 0), (768, 2), (1536, 5), (1024, 5)])
                .unwrap();

        assert_eq!(dimensions, vec![1536, 1024, 768]);
    }

    #[test]
    fn chunk_vector_profile_inventory_returns_ranked_dimensions_and_exact_count() {
        let inventory =
            chunk_vector_profile_inventory(vec![(384, 0), (768, 2), (1536, 5)]).unwrap();

        assert_eq!(inventory.dimensions, vec![1536, 768]);
        assert_eq!(inventory.active_vector_count, 7);
        assert!(chunk_vector_profile_inventory(vec![(768, -1)]).is_err());
    }

    #[test]
    fn empty_profile_inventory_has_no_dimension_or_active_vectors() {
        let inventory = chunk_vector_profile_inventory(vec![(384, 0), (768, 0)]).unwrap();

        assert!(inventory.dimensions.is_empty());
        assert_eq!(inventory.active_vector_count, 0);
    }

    #[test]
    fn manifest_reconciliation_requires_exactly_one_prepared_lane() {
        assert!(ensure_single_manifest_row_updated(0).is_err());
        assert!(ensure_single_manifest_row_updated(1).is_ok());
        assert!(ensure_single_manifest_row_updated(2).is_err());
    }

    #[test]
    fn staging_cleanup_accepts_only_the_opaque_rebuild_key_protocol() {
        let valid =
            format!("{VECTOR_REBUILD_STAGING_PROFILE_PREFIX}{}", "0123456789abcdef".repeat(4));
        assert!(validate_vector_rebuild_staging_profile_key(&valid).is_ok());
        assert!(
            validate_vector_rebuild_staging_profile_key(
                "embedding-profile:v1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .is_err()
        );
        assert!(
            validate_vector_rebuild_staging_profile_key("embedding-rebuild:v1:0123456789abcdef")
                .is_err()
        );
        assert!(
            validate_vector_rebuild_staging_profile_key(
                "embedding-rebuild:v1:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
            )
            .is_err()
        );
    }

    #[test]
    fn dimension_claim_reader_accepts_only_canonical_exact_profile_keys() {
        let valid = format!("{EMBEDDING_PROFILE_PREFIX}{}", "0123456789abcdef".repeat(4));
        assert!(validate_canonical_embedding_profile_key(&valid).is_ok());
        assert!(validate_canonical_embedding_profile_key(&Uuid::now_v7().to_string()).is_err());
        assert!(
            validate_canonical_embedding_profile_key(
                "embedding-rebuild:v1:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
            )
            .is_err()
        );
        assert!(
            validate_canonical_embedding_profile_key(
                "embedding-profile:v1:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"
            )
            .is_err()
        );
    }

    #[test]
    fn dimension_claim_requires_one_positive_storage_safe_matching_relation() {
        assert_eq!(
            validate_exact_profile_dimension_claim(CHUNK_VECTOR_RELATION_PREFIX, &[]).unwrap(),
            None
        );
        assert_eq!(
            validate_exact_profile_dimension_claim(
                CHUNK_VECTOR_RELATION_PREFIX,
                &[(3, "knowledge_chunk_vector_d3".to_string())],
            )
            .unwrap(),
            Some(3)
        );
        assert!(
            validate_exact_profile_dimension_claim(
                CHUNK_VECTOR_RELATION_PREFIX,
                &[(0, "knowledge_chunk_vector_d0".to_string())],
            )
            .is_err()
        );
        assert!(
            validate_exact_profile_dimension_claim(
                CHUNK_VECTOR_RELATION_PREFIX,
                &[(4_001, "knowledge_chunk_vector_d4001".to_string())],
            )
            .is_err()
        );
        assert!(
            validate_exact_profile_dimension_claim(
                CHUNK_VECTOR_RELATION_PREFIX,
                &[(3, "knowledge_chunk_vector_d4".to_string())],
            )
            .is_err()
        );
        assert!(
            validate_exact_profile_dimension_claim(
                CHUNK_VECTOR_RELATION_PREFIX,
                &[
                    (3, "knowledge_chunk_vector_d3".to_string()),
                    (4, "knowledge_chunk_vector_d4".to_string()),
                ],
            )
            .is_err()
        );
    }

    #[test]
    fn deferred_chunk_relation_requires_one_valid_prepared_lane() {
        let library_id = Uuid::now_v7();
        let mut first = deferred_chunk_row(library_id, 3, "profile-a");
        let second = deferred_chunk_row(library_id, 3, "profile-a");

        assert_eq!(
            deferred_chunk_vector_relation(&[first.clone(), second]).unwrap(),
            Some("knowledge_chunk_vector_d3".to_string())
        );
        first.vector.pop();
        assert!(deferred_chunk_vector_relation(&[first]).is_err());
        assert_eq!(deferred_chunk_vector_relation(&[]).unwrap(), None);
    }

    #[test]
    fn deferred_entity_relation_rejects_cross_library_batches() {
        let first = deferred_entity_row(Uuid::now_v7(), 3, "profile-a");
        let second = deferred_entity_row(Uuid::now_v7(), 3, "profile-a");

        assert!(deferred_entity_vector_relation(&[first, second]).is_err());
    }

    fn deferred_chunk_row(
        library_id: Uuid,
        dimensions: i32,
        embedding_model_key: &str,
    ) -> KnowledgeChunkVectorRow {
        KnowledgeChunkVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id,
            chunk_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            embedding_model_key: embedding_model_key.to_string(),
            vector_kind: KNOWLEDGE_CHUNK_VECTOR_KIND.to_string(),
            dimensions,
            vector: vec![0.0; usize::try_from(dimensions).unwrap()],
            freshness_generation: 1,
            created_at: Utc::now(),
            occurred_at: None,
            occurred_until: None,
        }
    }

    fn deferred_entity_row(
        library_id: Uuid,
        dimensions: i32,
        embedding_model_key: &str,
    ) -> KnowledgeEntityVectorRow {
        KnowledgeEntityVectorRow {
            vector_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id,
            entity_id: Uuid::now_v7(),
            embedding_model_key: embedding_model_key.to_string(),
            vector_kind: KNOWLEDGE_ENTITY_VECTOR_KIND.to_string(),
            dimensions,
            vector: vec![0.0; usize::try_from(dimensions).unwrap()],
            freshness_generation: 1,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn chunk_lexical_sql_non_default_config_substitutes_config_name() {
        // Prove that a non-default text_search_config name flows through into
        // the rendered FTS constructors so the lexical lane actually uses the
        // library's declared config rather than the hardcoded 'simple' default.
        let (exact, prefix) = chunk_lexical_sql("custom_lang");
        assert!(
            exact.contains("websearch_to_tsquery('custom_lang', ironrag_unaccent($2))"),
            "exact template must contain websearch_to_tsquery with custom_lang config"
        );
        assert!(
            prefix.contains("to_tsquery('custom_lang', ironrag_unaccent($2))"),
            "prefix template must contain to_tsquery with custom_lang config"
        );
        // Sanity: neither template should fall back to 'simple'.
        assert!(
            !exact.contains("'simple'"),
            "exact template must not contain 'simple' when config is custom_lang"
        );
        assert!(
            !prefix.contains("'simple'"),
            "prefix template must not contain 'simple' when config is custom_lang"
        );
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
