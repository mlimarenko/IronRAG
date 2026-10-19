use super::*;

pub(crate) const CHUNK_VECTOR_RELATION_PREFIX: &str = "knowledge_chunk_vector_d";
pub(crate) const ENTITY_VECTOR_RELATION_PREFIX: &str = "knowledge_entity_vector_d";
pub(super) const EMBEDDING_PROFILE_PREFIX: &str = "embedding-profile:v1:";
pub(super) const EMBEDDING_PROFILE_DIGEST_LEN: usize = 64;
pub(crate) const VECTOR_RELATION_DDL_ADVISORY_LOCK_KEY: &str = "knowledge.vector_relation.ddl";
/// Maximum manifest rows locked and removed per abandoned-rebuild recovery
/// transaction. Recovery commits each batch and continues until exhausted, so
/// this is a lock/transaction bound rather than a ceiling on recoverable work.
pub(super) const MAX_ABANDONED_VECTOR_REBUILD_MANIFEST_ROWS: usize = 512;
const PGVECTOR_MAX_INDEXED_DIM: u64 = 4_000;
pub(super) const PG_HNSW_DEFAULT_EF_SEARCH: u64 = 400;
// pgvector defines hnsw.ef_search as an integer GUC in the inclusive range
// 1..=1000. Clamp locally so a configuration typo cannot abort every ANN query.
pub(super) const PG_HNSW_MAX_EF_SEARCH: u64 = 1_000;
pub(super) const PG_HNSW_DEFAULT_MAX_SCAN_TUPLES: u64 = 50_000;
pub(super) const PG_HNSW_MAX_SCAN_TUPLES: u64 = 1_000_000;
pub(super) const PG_HNSW_DEFAULT_SCAN_MEM_MULTIPLIER: u64 = 2;
pub(super) const PG_HNSW_MAX_SCAN_MEM_MULTIPLIER: u64 = 64;
pub(super) const PG_HNSW_DEFAULT_EXACT_FALLBACK_MAX_ROWS: u64 = 10_000;
pub(super) const PG_HNSW_MAX_EXACT_FALLBACK_ROWS: u64 = 100_000;

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
        "select v.vector_id, v.workspace_id, v.library_id, c.document_id,
            v.chunk_id, v.revision_id,
            v.embedding_model_key, v.vector_kind, v.freshness_generation,
            (1.0 - (v.embedding <=> $3::{cast_type}))::double precision as score
         from {relation} v
         join knowledge_chunk c
           on c.chunk_id = v.chunk_id
          and c.revision_id = v.revision_id
          and c.workspace_id = v.workspace_id
          and c.library_id = v.library_id
         join knowledge_document d
           on d.document_id = c.document_id
          and d.workspace_id = c.workspace_id
          and d.library_id = v.library_id
          and exists (select 1 from content_current_revision current_revision
                      where current_revision.document_id = d.document_id
                        and current_revision.revision_id = v.revision_id)
          and d.document_state = 'active'
          and d.deleted_at is null
         join knowledge_structured_revision sr
           on sr.revision_id = v.revision_id
          and sr.document_id = c.document_id
          and sr.workspace_id = v.workspace_id
          and sr.library_id = v.library_id
          and sr.preparation_state = 'prepared'
         where v.library_id = $1
           and v.embedding_model_key = $2
           and v.vector_kind = $6
           and c.chunk_state = 'ready'
           and (($4::timestamptz is null and $5::timestamptz is null)
                or (v.occurred_at is not null
                    and ($4::timestamptz is null or coalesce(v.occurred_until, v.occurred_at) >= $4)
                    and ($5::timestamptz is null or v.occurred_at <= $5)))
         order by {order_expression}, v.chunk_id asc
         limit $7"
    )
}

pub(super) fn chunk_vector_similarity_sql(relation: &str, cast_type: &str) -> String {
    chunk_vector_similarity_sql_with_order(relation, cast_type, false)
}

pub(super) fn chunk_vector_exact_similarity_sql(relation: &str, cast_type: &str) -> String {
    chunk_vector_similarity_sql_with_order(relation, cast_type, true)
}

/// Exact one-best-chunk-per-document rescue for document-level retrieval.
///
/// The primary query remains HNSW-backed. If its bounded chunk window contains
/// fewer documents than requested, this query considers only documents absent
/// from that window and returns their exact best chunk. `+ 0.0` deliberately
/// prevents pgvector from treating the window order as another ANN scan.
pub(super) fn chunk_vector_document_rescue_sql(relation: &str, cast_type: &str) -> String {
    format!(
        "with eligible as materialized (
            select v.vector_id, v.workspace_id, v.library_id, c.document_id,
                v.chunk_id, v.revision_id, v.embedding_model_key, v.vector_kind,
                v.freshness_generation,
                ((v.embedding <=> $3::{cast_type}) + 0.0)::double precision as distance
            from {relation} v
            join knowledge_chunk c
              on c.chunk_id = v.chunk_id
             and c.revision_id = v.revision_id
             and c.workspace_id = v.workspace_id
             and c.library_id = v.library_id
            join knowledge_document d
              on d.document_id = c.document_id
             and d.workspace_id = c.workspace_id
             and d.library_id = v.library_id
             and exists (select 1 from content_current_revision current_revision
                         where current_revision.document_id = d.document_id
                           and current_revision.revision_id = v.revision_id)
             and d.document_state = 'active'
             and d.deleted_at is null
            join knowledge_structured_revision sr
              on sr.revision_id = v.revision_id
             and sr.document_id = c.document_id
             and sr.workspace_id = v.workspace_id
             and sr.library_id = v.library_id
             and sr.preparation_state = 'prepared'
            where v.library_id = $1
              and v.embedding_model_key = $2
              and v.vector_kind = $6
              and c.chunk_state = 'ready'
              and not (c.document_id = any($7::uuid[]))
              and (($4::timestamptz is null and $5::timestamptz is null)
                   or (v.occurred_at is not null
                       and ($4::timestamptz is null
                            or coalesce(v.occurred_until, v.occurred_at) >= $4)
                       and ($5::timestamptz is null or v.occurred_at <= $5)))
        ), ranked as (
            select eligible.*,
                row_number() over (
                    partition by document_id
                    order by distance, chunk_id
                ) as document_rank
            from eligible
        )
        select vector_id, workspace_id, library_id, document_id, chunk_id, revision_id,
            embedding_model_key, vector_kind, freshness_generation,
            (1.0 - distance)::double precision as score
        from ranked
        where document_rank = 1
        order by distance, document_id, chunk_id
        limit $8"
    )
}

fn entity_vector_similarity_sql_with_order(
    relation: &str,
    cast_type: &str,
    exact_retry: bool,
) -> String {
    let entity_visibility = current_knowledge_graph_target!(
        "knowledge_evidence_entity_support",
        "e.entity_id",
        "e.library_id"
    );
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
           and {entity_visibility}
         where v.library_id = $1
           and v.embedding_model_key = $2
           and v.vector_kind = $4
         order by {order_expression}, v.entity_id asc
         limit $5"
    )
}

pub(super) fn entity_vector_similarity_sql(relation: &str, cast_type: &str) -> String {
    entity_vector_similarity_sql_with_order(relation, cast_type, false)
}

pub(super) fn entity_vector_exact_similarity_sql(relation: &str, cast_type: &str) -> String {
    entity_vector_similarity_sql_with_order(relation, cast_type, true)
}

/// Runtime coverage checks must measure only vectors backed by active
/// source chunks. Stale vector rows can linger in the physical shards for
/// maintenance/cleanup, but they must not make a revision appear over- or
/// under-embedded.
/// Counts one explicitly requested in-flight revision during embedding
/// publication. This intentionally does not require a prepared/readable head:
/// the caller must prove vector completeness before that revision is published.
pub(super) fn active_chunk_vector_count_sql(relation: &str) -> String {
    format!(
        "select count(*)::bigint
         from {relation} v
         join knowledge_chunk c
           on c.chunk_id = v.chunk_id
          and c.revision_id = v.revision_id
          and c.workspace_id = v.workspace_id
          and c.library_id = v.library_id
         where v.revision_id = $1
           and v.embedding_model_key = $2
           and v.vector_kind = $3
           and v.freshness_generation = $4
           and c.chunk_state = 'ready'"
    )
}

/// Count live active vectors across all candidate dimension shards in one
/// set-based database call. This avoids trusting manifest `row_count`, which
/// deliberately includes legacy rows used by broad maintenance APIs, without
/// adding one client round trip per historical dimension.
pub(super) fn active_chunk_vector_dimension_counts_sql(
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
            "select {dim}::integer as dim, count(*)::bigint as active_count
             from knowledge_chunk c
             join knowledge_document d
               on d.document_id = c.document_id
              and d.workspace_id = c.workspace_id
              and d.library_id = c.library_id
              and exists (select 1 from content_current_revision current_revision
                          where current_revision.document_id = d.document_id
                            and current_revision.revision_id = c.revision_id)
              and d.document_state = 'active'
              and d.deleted_at is null
             join knowledge_structured_revision sr
               on sr.revision_id = c.revision_id
              and sr.document_id = c.document_id
              and sr.workspace_id = c.workspace_id
              and sr.library_id = c.library_id
              and sr.preparation_state = 'prepared'
             where c.library_id = $1
               and c.chunk_state = 'ready'
               and exists (
                 select 1
                 from {relation} v
                 where v.chunk_id = c.chunk_id
                   and v.revision_id = c.revision_id
                   and v.workspace_id = c.workspace_id
                   and v.library_id = c.library_id
                   and v.embedding_model_key = $2
                   and v.vector_kind = $3
               )"
        ));
    }
    Ok((!branches.is_empty()).then(|| branches.join(" union all ")))
}

pub(super) fn rank_active_chunk_vector_dimensions(
    mut counts: Vec<(i32, i64)>,
) -> anyhow::Result<Vec<u64>> {
    for (dim, count) in &counts {
        anyhow::ensure!(*dim > 0, "chunk vector manifest dimension must be positive");
        anyhow::ensure!(*count >= 0, "active chunk vector count must be non-negative");
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

pub(super) fn chunk_vector_profile_inventory(
    active_counts: Vec<(i32, i64)>,
) -> anyhow::Result<ChunkVectorProfileInventory> {
    let active_vector_count =
        active_counts.iter().try_fold(0_u64, |total, (_, count)| -> anyhow::Result<u64> {
            let count = u64::try_from(*count).context("active chunk vector count was negative")?;
            total.checked_add(count).context("active chunk vector count overflowed")
        })?;
    Ok(ChunkVectorProfileInventory {
        dimensions: rank_active_chunk_vector_dimensions(active_counts)?,
        active_vector_count,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PgHnswSearchParams {
    pub(super) ef_search: u64,
    pub(super) max_scan_tuples: u64,
    pub(super) scan_mem_multiplier: u64,
}

pub(super) const HNSW_SEARCH_CONFIG_SQL: &str = "select
    set_config('hnsw.ef_search', $1, true),
    set_config('hnsw.iterative_scan', 'strict_order', true),
    set_config('hnsw.max_scan_tuples', $2, true),
    set_config('hnsw.scan_mem_multiplier', $3, true)";

const CHUNK_VECTOR_FORCE_CUSTOM_PLAN_SQL: &str =
    "select set_config('plan_cache_mode', 'force_custom_plan', true)";

pub(super) fn pg_hnsw_search_params_from_overrides(
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

pub(super) fn pg_hnsw_search_params(n_probe: Option<u64>) -> PgHnswSearchParams {
    pg_hnsw_search_params_from_overrides(
        n_probe,
        read_env_u64("IRONRAG_PG_HNSW_EF_SEARCH"),
        read_env_u64("IRONRAG_PG_HNSW_MAX_SCAN_TUPLES"),
        read_env_u64("IRONRAG_PG_HNSW_SCAN_MEM_MULTIPLIER"),
    )
}

pub(super) fn pg_hnsw_exact_fallback_max_rows() -> u64 {
    pg_hnsw_exact_fallback_max_rows_from_override(read_env_u64_allow_zero(
        "IRONRAG_PG_HNSW_EXACT_FALLBACK_MAX_ROWS",
    ))
}

pub(super) fn pg_hnsw_exact_fallback_max_rows_from_override(configured: Option<u64>) -> u64 {
    configured
        .unwrap_or(PG_HNSW_DEFAULT_EXACT_FALLBACK_MAX_ROWS)
        .min(PG_HNSW_MAX_EXACT_FALLBACK_ROWS)
}

pub(super) fn ann_search_underfilled(
    result_count: usize,
    requested_limit: usize,
    manifest_row_count: u64,
) -> bool {
    let requested_limit = requested_limit.max(1);
    result_count < requested_limit
        && u64::try_from(requested_limit).is_ok_and(|limit| manifest_row_count >= limit)
}

pub(super) fn should_retry_exact_ann(
    result_count: usize,
    requested_limit: usize,
    manifest_row_count: u64,
    exact_fallback_max_rows: u64,
) -> bool {
    ann_search_underfilled(result_count, requested_limit, manifest_row_count)
        && exact_fallback_max_rows > 0
        && manifest_row_count <= exact_fallback_max_rows
}

pub(super) fn should_run_exact_document_rescue(
    represented_document_count: usize,
    requested_document_limit: usize,
    manifest_row_count: u64,
    exact_fallback_max_rows: u64,
) -> bool {
    represented_document_count < requested_document_limit.max(1)
        && u64::try_from(represented_document_count)
            .is_ok_and(|represented| manifest_row_count > represented)
        && exact_fallback_max_rows > 0
        && manifest_row_count <= exact_fallback_max_rows
}

pub(super) async fn configure_hnsw_search(
    transaction: &mut Transaction<'_, Postgres>,
    params: PgHnswSearchParams,
) -> anyhow::Result<()> {
    // pgvector applies ordinary filters after an approximate HNSW scan. During
    // a rebuild, opaque staging rows share the dimension shard with active
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

/// Keeps the parameter-sensitive primary chunk query on its measured custom
/// plan. This is deliberately separate from [`configure_hnsw_search`]: the
/// latter also serves document-diversity and entity ANN paths whose approximate
/// candidate sets must not inherit a new plan policy without their own proof.
pub(super) async fn force_custom_chunk_vector_plan(
    transaction: &mut Transaction<'_, Postgres>,
) -> anyhow::Result<()> {
    sqlx::query(CHUNK_VECTOR_FORCE_CUSTOM_PLAN_SQL)
        .execute(&mut **transaction)
        .await
        .context("failed to force a custom primary chunk-vector plan")?;
    Ok(())
}

fn read_env_u64_allow_zero(name: &str) -> Option<u64> {
    std::env::var(name).ok().and_then(|value| value.trim().parse::<u64>().ok())
}

pub(super) fn checked_dim_i32(dim: u64) -> anyhow::Result<i32> {
    anyhow::ensure!(dim > 0, "vector dimension must be positive");
    anyhow::ensure!(
        dim <= PGVECTOR_MAX_INDEXED_DIM,
        "vector dimension exceeds the indexed storage limit"
    );
    i32::try_from(dim).context("vector dimension overflowed i32")
}

pub(super) fn validate_row_vector_dimensions(
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
        "deferred chunk rebuild requires the expected chunk vector kind"
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
        "deferred entity rebuild requires the expected entity vector kind"
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

pub(super) fn deferred_chunk_vector_relation(
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

pub(super) fn deferred_entity_vector_relation(
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

pub(super) fn validate_query_vector_dimensions(
    dim: u64,
    vector: &[f32],
    label: &str,
) -> anyhow::Result<()> {
    anyhow::ensure!(!vector.is_empty(), "{label} query vector must not be empty");
    let vector_dim = u64::try_from(vector.len()).context("query vector length overflowed u64")?;
    anyhow::ensure!(
        dim == vector_dim,
        "{label} query vector dimension mismatch: requested {dim}, vector has {vector_dim}"
    );
    checked_dim_i32(dim)?;
    Ok(())
}

pub(super) fn vector_relation_name(prefix: &str, dim: u64) -> anyhow::Result<String> {
    checked_dim_i32(dim)?;
    Ok(format!("{prefix}{dim}"))
}

pub(super) fn vector_relation_required_objects(
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

pub(super) fn validate_relation_name(
    relation_name: &str,
    expected_prefix: &str,
) -> anyhow::Result<()> {
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

pub(super) fn quote_identifier(identifier: &str) -> anyhow::Result<String> {
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

pub(super) fn pgvector_literal(vector: &[f32]) -> anyhow::Result<String> {
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

pub(super) fn chunk_vector_from_pg(
    row: PgChunkVectorRow,
) -> anyhow::Result<KnowledgeChunkVectorRow> {
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

pub(super) fn entity_vector_from_pg(
    row: PgEntityVectorRow,
) -> anyhow::Result<KnowledgeEntityVectorRow> {
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
