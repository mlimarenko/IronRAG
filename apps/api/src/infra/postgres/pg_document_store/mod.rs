use std::{
    cmp::Reverse,
    collections::{BTreeSet, HashSet},
};

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder, Row, Transaction, postgres::PgRow};
use uuid::Uuid;

use crate::infra::repositories::{catalog_repository, ingest_repository};
use crate::infra::{
    knowledge_plane::{
        DocumentPointersUpdate, DocumentStore, IngestAttemptMutationOutcome,
        IngestRevisionExtractArtifact, IngestRevisionWriteAuthority, RevisionReadinessUpdate,
    },
    knowledge_rows::{
        KNOWLEDGE_CHUNK_VECTOR_KIND, KnowledgeChunkRow, KnowledgeChunkSupportReferenceRow,
        KnowledgeDocumentRow, KnowledgeRevisionRow, KnowledgeStructuredBlockRow,
        KnowledgeStructuredRevisionRow, KnowledgeTechnicalFactRow, LibraryGenerationSignals,
        StructuredRevisionCounts,
    },
};

const KNOWLEDGE_CHUNK_INSERT_BATCH_ROWS: usize = 250;
const KNOWLEDGE_CHUNK_WINDOW_FETCH_LIMIT: usize = 2_000;
const KNOWLEDGE_CHUNK_REVISION_TERM_LIMIT: usize = 24;
const KNOWLEDGE_CHUNK_REVISION_TERM_MAX_CHARS: usize = 128;
const KNOWLEDGE_CHUNK_MULTI_REVISION_MATCH_CANDIDATE_CAP: usize = 256;
const KNOWLEDGE_CHUNK_MULTI_REVISION_STATEMENT_TIMEOUT_MS: u64 = 120_000;
/// Concatenated lower(text) used by the revision-scoped gin_trgm prefilter.
/// `chr(1)` cannot appear in query terms, so a per-column `strpos` hit is
/// always a substring of this expression and the expression cannot invent a
/// cross-column match for those terms.
const MATCHING_TERMS_LOWER_TEXT_SQL: &str = "(lower(c.normalized_text) || chr(1) || lower(c.content_text) || chr(1) || lower(COALESCE(c.window_text, ''::text)))";
const SETUP_STRUCTURED_BLOCK_LANE_LIMIT: usize = 384;
const SETUP_STRUCTURED_BLOCK_STATEMENT_TIMEOUT_MS: u64 = 2_000;
const CODE_PATTERN_ASSIGNMENT_REGEX: &str =
    r"(^|[^[:alnum:]_])[a-z][a-z0-9_.-]{2,160}\s*=\s*-?[0-9]{2,}([,.;]\s*-?[0-9]{2,})*";
const CODE_PATTERN_NUMERIC_MAPPING_REGEX: &str =
    r"(^|[\r\n])\s*-?[0-9]{2,}([.][0-9]{2,})?\s*=\s*[^\r\n]{2,}";
const CODE_PATTERN_SECTION_REGEX: &str = r"(^|[\r\n])\s*\[[^\]\r\n]{2,80}\]";
const SOURCE_UNIT_RELEASE_MARKER_REGEX: &str = r"(^|[^0-9.])[0-9]+\.[0-9]+(\.[0-9]+)?([^0-9.]|$)";
const SOURCE_UNIT_OCCURRED_AT_REGEX: &str = r"occurred_at=([0-9T:+\-]+)";

// (KnowledgeDocumentRow etc.) — safe to simplify to direct column lists
const DOCUMENT_COLUMNS: &str = "document_id, workspace_id, library_id, external_key, file_name, title, NULL::text AS source_uri, NULL::text AS document_hint, document_state, active_revision_id, readable_revision_id, latest_revision_no, parent_document_id, document_role, created_at, updated_at, deleted_at";
const REVISION_COLUMNS: &str = "revision_id, workspace_id, library_id, document_id, revision_number, revision_state, revision_kind, storage_ref, source_uri, document_hint, mime_type, checksum, title, byte_size, normalized_text, text_checksum, image_checksum, text_state, vector_state, graph_state, text_readable_at, vector_ready_at, graph_ready_at, superseded_by_revision_id, created_at";
const CHUNK_COLUMNS: &str = "chunk_id, workspace_id, library_id, document_id, revision_id, chunk_index, chunk_kind, content_text, normalized_text, span_start, span_end, token_count, support_block_ids, section_path, heading_trail, literal_digest, chunk_state, text_generation, vector_generation, quality_score, window_text, occurred_at, occurred_until";
const QUALIFIED_CHUNK_COLUMNS: &str = "c.chunk_id, c.workspace_id, c.library_id, c.document_id, c.revision_id, c.chunk_index, c.chunk_kind, c.content_text, c.normalized_text, c.span_start, c.span_end, c.token_count, c.support_block_ids, c.section_path, c.heading_trail, c.literal_digest, c.chunk_state, c.text_generation, c.vector_generation, c.quality_score, c.window_text, c.occurred_at, c.occurred_until";
const STRUCTURED_REVISION_COLUMNS: &str = "revision_id, workspace_id, library_id, document_id, preparation_state, normalization_profile, source_format, language_code, block_count::int4 AS block_count, chunk_count::int4 AS chunk_count, typed_fact_count::int4 AS typed_fact_count, outline_json, prepared_at, updated_at";
const STRUCTURED_BLOCK_COLUMNS: &str = "block_id, workspace_id, library_id, document_id, revision_id, ordinal, block_kind, text, normalized_text, heading_trail, section_path, page_number, span_start, span_end, parent_block_id, table_coordinates_json, code_language, created_at, updated_at";
const TECHNICAL_FACT_COLUMNS: &str = "fact_id, workspace_id, library_id, document_id, revision_id, fact_kind, \u{63}anonical_value_text, \u{63}anonical_value_exact, \u{63}anonical_value_json, display_value, qualifiers_json, support_block_ids, support_chunk_ids, confidence, extraction_kind, conflict_group_id, created_at, updated_at";

fn setup_structured_blocks_by_revision_sql() -> String {
    format!(
        "WITH sampled AS (
           SELECT b.*, 0::integer AS lane_rank
           FROM knowledge_structured_block b
           WHERE b.revision_id = $1
             AND EXISTS (
               SELECT 1
               FROM knowledge_structured_revision r
               WHERE r.revision_id = b.revision_id
                 AND r.preparation_state = 'prepared'
             )
           ORDER BY b.ordinal ASC, b.block_id ASC
           LIMIT $2
         ), structured AS (
           SELECT b.*, 1::integer AS lane_rank
           FROM knowledge_structured_block b
           WHERE b.revision_id = $1
             AND EXISTS (
               SELECT 1
               FROM knowledge_structured_revision r
               WHERE r.revision_id = b.revision_id
                 AND r.preparation_state = 'prepared'
             )
             AND b.block_kind IN ('table', 'table_row', 'code_block', 'source_unit')
             AND NOT EXISTS (
               SELECT 1
               FROM sampled s
               WHERE s.block_id = b.block_id
             )
           ORDER BY b.ordinal ASC, b.block_id ASC
           LIMIT $3
         ), candidates AS (
           SELECT * FROM sampled
           UNION ALL
           SELECT * FROM structured
         )
         SELECT {STRUCTURED_BLOCK_COLUMNS}
         FROM candidates b
         ORDER BY b.lane_rank ASC, b.ordinal ASC, b.block_id ASC
         LIMIT $4"
    )
}

fn structured_pattern_chunk_search_sql(
    shape_projection: &str,
    minimum_match_count: &str,
    shape_predicate: &str,
    score_expression: &str,
    limit_parameter: &str,
) -> String {
    format!(
        "select {QUALIFIED_CHUNK_COLUMNS}
         from knowledge_chunk c
         join knowledge_document d
           on d.document_id = c.document_id
          and d.library_id = c.library_id
          and exists (select 1 from content_current_revision current_revision
                      where current_revision.document_id = c.document_id
                        and current_revision.revision_id = c.revision_id)
          and d.document_state = 'active'
          and d.deleted_at is null
         join knowledge_structured_revision structured
           on structured.revision_id = c.revision_id
          and structured.workspace_id = c.workspace_id
          and structured.library_id = c.library_id
          and structured.document_id = c.document_id
          and structured.preparation_state = 'prepared'
         cross join lateral (
           select lower(concat_ws(' ', c.normalized_text, c.content_text, c.window_text)) as text
         ) text_parts
         cross join lateral (
           select count(distinct term)::int as matched_count
           from unnest($3::text[]) as term
           where strpos(text_parts.text, term) > 0
         ) matches
         cross join lateral (
           select {shape_projection}
         ) shapes
         where c.library_id = $1
           and c.document_id = any($2::uuid[])
           and c.chunk_state = 'ready'
           and c.chunk_kind is distinct from 'source_profile'
           and not starts_with(c.normalized_text, '[source_profile ')
           and not starts_with(c.content_text, '[source_profile ')
           and matches.matched_count >= {minimum_match_count}
           and {shape_predicate}
         order by ({score_expression}) desc,
         c.revision_id desc,
         c.chunk_index asc,
         c.chunk_id asc
         limit {limit_parameter}"
    )
}

fn code_pattern_chunk_search_sql() -> String {
    structured_pattern_chunk_search_sql(
        "text_parts.text ~* $5 as assignment_shape,
         text_parts.text ~* $6 as numeric_mapping_shape,
         text_parts.text ~* $7 as section_shape",
        "$4",
        "shapes.assignment_shape",
        "matches.matched_count * 3000
         + 5000
         + case when shapes.numeric_mapping_shape then 3500 else 0 end
         + case when shapes.section_shape then 800 else 0 end",
        "$8",
    )
}

fn transport_pattern_chunk_search_sql() -> String {
    structured_pattern_chunk_search_sql(
        "strpos(text_parts.text, '=http://') > 0
           or strpos(text_parts.text, '= http://') > 0
           or strpos(text_parts.text, '=https://') > 0
           or strpos(text_parts.text, '= https://') > 0 as url_assignment_shape,
         strpos(text_parts.text, 'port=') > 0
           or strpos(text_parts.text, 'port =') > 0
           or strpos(text_parts.text, '.port') > 0
           or strpos(text_parts.text, '_port') > 0
           or strpos(text_parts.text, '-port') > 0 as port_assignment_shape,
         strpos(text_parts.text, 'data:image') > 0
           or strpos(text_parts.text, '.svg') > 0
           or strpos(text_parts.text, '.png') > 0
           or strpos(text_parts.text, '.jpg') > 0
           or strpos(text_parts.text, '.jpeg') > 0 as media_reference_shape,
         strpos(text_parts.text, '[') > 0 and strpos(text_parts.text, ']') > 0 as section_shape,
         strpos(text_parts.text, '=') > 0 as config_assignment_shape",
        "1",
        "(shapes.url_assignment_shape or shapes.port_assignment_shape)
         and not shapes.media_reference_shape",
        "matches.matched_count * 3000
         + case when shapes.url_assignment_shape then 4500 else 0 end
         + case when shapes.port_assignment_shape then 2200 else 0 end
         + case when shapes.section_shape then 900 else 0 end
         + case when shapes.config_assignment_shape then 600 else 0 end",
        "$4",
    )
}

fn multi_revision_matching_terms_candidate_cap(
    revision_count: usize,
    per_revision_limit: usize,
) -> usize {
    revision_count
        .saturating_mul(per_revision_limit)
        .min(KNOWLEDGE_CHUNK_MULTI_REVISION_MATCH_CANDIDATE_CAP)
}

fn matching_terms_like_patterns(terms: &[String]) -> Vec<String> {
    let selective = terms
        .iter()
        .filter(|term| term.chars().count() >= 3)
        .map(|term| format!("%{term}%"))
        .collect::<Vec<_>>();
    if selective.is_empty() {
        terms.iter().map(|term| format!("%{term}%")).collect()
    } else {
        selective
    }
}

fn matching_terms_trigram_hit_sql() -> String {
    format!(
        "select c.chunk_id
           from knowledge_chunk c
           where c.revision_id = any($1::uuid[])
             and {MATCHING_TERMS_LOWER_TEXT_SQL} like any($5::text[])"
    )
}

/// Prepared-revision consistency fence applied *after* a revision-index
/// prefix/suffix. Putting this EXISTS (and therefore `library_id` /
/// `document_id`) in the inner scan lets the planner drive from
/// `knowledge_chunk_library_document_index` and filter 346k rows.
///
/// LIMIT-then-EXISTS equals fetch-all-then-truncate only when the first
/// N rows pass this fence (a consistent prepared revision). Corrupt early
/// chunks that fail the identity join are dropped instead of being skipped
/// so later rows can fill the limit.
const PREPARED_REVISION_CHUNK_GATE_SQL: &str = "exists (
               select 1
               from knowledge_structured_revision r
               where r.revision_id = c.revision_id
                 and r.preparation_state = 'prepared'
                 and r.workspace_id = c.workspace_id
                 and r.library_id = c.library_id
                 and r.document_id = c.document_id
             )";

fn list_head_chunks_by_revision_sql() -> String {
    format!(
        "with head as materialized (
           select {QUALIFIED_CHUNK_COLUMNS}
           from knowledge_chunk c
           where c.revision_id = $1
           order by c.chunk_index asc, c.chunk_id asc
           limit $2
         )
         select {QUALIFIED_CHUNK_COLUMNS}
         from head c
         where {PREPARED_REVISION_CHUNK_GATE_SQL}
         order by c.chunk_index asc, c.chunk_id asc"
    )
}

fn list_tail_chunks_by_revision_sql() -> String {
    format!(
        "with tail as materialized (
           select {QUALIFIED_CHUNK_COLUMNS}
           from knowledge_chunk c
           where c.revision_id = $1
             and c.chunk_state = 'ready'
           order by c.chunk_index desc, c.chunk_id desc
           limit $2
         )
         select {QUALIFIED_CHUNK_COLUMNS}
         from tail c
         where {PREPARED_REVISION_CHUNK_GATE_SQL}
         order by c.chunk_index asc, c.chunk_id asc"
    )
}

fn list_chunks_by_revision_range_sql() -> String {
    format!(
        "with windowed as materialized (
           select {QUALIFIED_CHUNK_COLUMNS}
           from knowledge_chunk c
           where c.revision_id = $1
             and c.chunk_index between $2 and $3
           order by c.chunk_index asc, c.chunk_id asc
           limit $4
         )
         select {QUALIFIED_CHUNK_COLUMNS}
         from windowed c
         where {PREPARED_REVISION_CHUNK_GATE_SQL}
         order by c.chunk_index asc, c.chunk_id asc"
    )
}

fn list_chunks_by_revision_windows_sql() -> String {
    // normalized_windows makes these ranges disjoint. A row beyond the per-window
    // cap already has `$4` earlier union rows, so it cannot enter the global prefix.
    format!(
        "with requested_windows as (
           select min_index, max_index
           from unnest($2::int4[], $3::int4[]) as request(min_index, max_index)
         ),
         windowed as materialized (
           select picked.*
           from requested_windows w
           cross join lateral (
             select {QUALIFIED_CHUNK_COLUMNS}
             from knowledge_chunk c
             where c.revision_id = $1
               and c.chunk_index between w.min_index and w.max_index
             order by c.chunk_index asc, c.chunk_id asc
             limit $4
           ) picked
           order by picked.chunk_index asc, picked.chunk_id asc
           limit $4
         )
         select {QUALIFIED_CHUNK_COLUMNS}
         from windowed c
         where {PREPARED_REVISION_CHUNK_GATE_SQL}
         order by c.chunk_index asc, c.chunk_id asc"
    )
}

fn list_chunks_by_revisions_windows_sql() -> String {
    // Merge overlaps before the per-revision cap so repeated windows cannot consume
    // rank slots. int8range keeps an inclusive i32::MAX upper bound overflow-safe.
    format!(
        "with requested_windows as (
           select revision_id, min_index, max_index, ordinal::integer as ordinal
           from unnest($1::uuid[], $2::int4[], $3::int4[]) with ordinality
                as request(revision_id, min_index, max_index, ordinal)
         ),
         requested_revisions as (
           select revision_id, min(ordinal) as ordinal
           from requested_windows
           group by revision_id
         ),
         merged_windows as (
           select aggregated.revision_id, merged.chunk_range
           from (
             select revision_id,
                    range_agg(
                      int8range(min_index::bigint, max_index::bigint, '[]')
                    ) as windows
             from requested_windows
             group by revision_id
           ) aggregated
           cross join lateral unnest(aggregated.windows) as merged(chunk_range)
         ),
         windowed as materialized (
           select {QUALIFIED_CHUNK_COLUMNS}, c.ordinal
           from (
             select picked.*,
                    requested_revisions.ordinal,
                    row_number() over (
                      partition by picked.revision_id
                      order by picked.chunk_index asc, picked.chunk_id asc
                    ) as revision_rank
             from merged_windows
             join requested_revisions using (revision_id)
             cross join lateral (
               select {QUALIFIED_CHUNK_COLUMNS}
               from knowledge_chunk c
               where c.revision_id = merged_windows.revision_id
                 and c.chunk_index >= lower(merged_windows.chunk_range)
                 and c.chunk_index < upper(merged_windows.chunk_range)
               order by c.chunk_index asc, c.chunk_id asc
               limit $4
             ) picked
           ) c
           where c.revision_rank <= $4
         )
         select {QUALIFIED_CHUNK_COLUMNS}
         from windowed c
         where {PREPARED_REVISION_CHUNK_GATE_SQL}
         order by c.ordinal asc, c.chunk_index asc, c.chunk_id asc"
    )
}

fn matching_terms_strpos_score_sql() -> &'static str {
    "(matches.matched_count * 10000 - matches.earliest_pos)"
}

fn multi_revision_matching_terms_sql() -> String {
    let hit_sql = matching_terms_trigram_hit_sql();
    let match_score = matching_terms_strpos_score_sql();
    format!(
        "with requested as (
           select revision_id, min(ordinal)::integer as ordinal
           from unnest($1::uuid[]) with ordinality as request(revision_id, ordinal)
           group by revision_id
         ),
         hits as materialized (
           {hit_sql}
         ),
         candidates as (
           select c.chunk_id,
             c.revision_id,
             c.chunk_index,
             requested.ordinal as ordinal,
             c.normalized_text,
             c.content_text,
             c.window_text
           from hits
           join lateral (
             select c.chunk_id,
               c.revision_id,
               c.chunk_index,
               c.normalized_text,
               c.content_text,
               c.window_text,
               c.workspace_id,
               c.library_id,
               c.document_id,
               c.chunk_state,
               c.chunk_kind
             from knowledge_chunk c
             where c.chunk_id = hits.chunk_id
             offset 0
           ) c on true
           join requested
             on c.revision_id = requested.revision_id
           where c.chunk_state = 'ready'
             and exists (
               select 1
               from knowledge_structured_revision structured
               where structured.revision_id = c.revision_id
                 and structured.workspace_id = c.workspace_id
                 and structured.library_id = c.library_id
                 and structured.document_id = c.document_id
                 and structured.preparation_state = 'prepared'
             )
             and c.chunk_kind is distinct from 'source_profile'
             and not starts_with(c.normalized_text, '[source_profile ')
             and not starts_with(c.content_text, '[source_profile ')
           group by c.chunk_id,
                    c.revision_id,
                    c.chunk_index,
                    requested.ordinal,
                    c.normalized_text,
                    c.content_text,
                    c.window_text
         ),
         scored as (
           select candidates.chunk_id,
             candidates.revision_id,
             candidates.chunk_index,
             candidates.ordinal,
             {match_score} as match_score
           from candidates
           cross join lateral (
             select
               lower(candidates.normalized_text) as normalized_lower,
               lower(candidates.content_text) as content_lower,
               lower(coalesce(candidates.window_text, '')) as window_lower
           ) text_parts
           cross join lateral (
             select
               count(distinct term)::int as matched_count,
               min(least(
                 coalesce(nullif(strpos(text_parts.normalized_lower, term), 0), 2147483647),
                 coalesce(nullif(strpos(text_parts.content_lower, term), 0), 2147483647),
                 coalesce(nullif(strpos(text_parts.window_lower, term), 0), 2147483647)
               )) as earliest_pos
             from unnest($2::text[]) as term
             where strpos(text_parts.normalized_lower, term) > 0
                or strpos(text_parts.content_lower, term) > 0
                or strpos(text_parts.window_lower, term) > 0
           ) matches
           where matches.matched_count > 0
         ),
         ranked as (
           select scored.*,
             row_number() over (
               partition by revision_id
               order by match_score desc, chunk_index asc, chunk_id asc
             ) as revision_rank
           from scored
         ),
         bounded as (
           select chunk_id, revision_id, ordinal, match_score, revision_rank, chunk_index
           from ranked
           where revision_rank <= $3
           order by revision_rank asc,
                    match_score desc,
                    ordinal asc,
                    chunk_index asc,
                    chunk_id asc
           limit $4
         )
         select {QUALIFIED_CHUNK_COLUMNS}
         from bounded
         join knowledge_chunk c on c.chunk_id = bounded.chunk_id
         order by bounded.ordinal asc,
                  bounded.match_score desc,
                  bounded.chunk_index asc,
                  bounded.chunk_id asc"
    )
}

#[derive(Clone)]
pub struct PgDocumentStore {
    pub pool: PgPool,
}

impl<'r> FromRow<'r, PgRow> for KnowledgeDocumentRow {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            document_id: row.try_get("document_id")?,
            workspace_id: row.try_get("workspace_id")?,
            library_id: row.try_get("library_id")?,
            external_key: row.try_get("external_key")?,
            file_name: row.try_get("file_name")?,
            title: row.try_get("title")?,
            source_uri: row.try_get("source_uri")?,
            document_hint: row.try_get("document_hint")?,
            document_state: row.try_get("document_state")?,
            active_revision_id: row.try_get("active_revision_id")?,
            readable_revision_id: row.try_get("readable_revision_id")?,
            latest_revision_no: row.try_get("latest_revision_no")?,
            parent_document_id: row.try_get("parent_document_id")?,
            document_role: row.try_get("document_role")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
            deleted_at: row.try_get("deleted_at")?,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for KnowledgeRevisionRow {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            revision_id: row.try_get("revision_id")?,
            workspace_id: row.try_get("workspace_id")?,
            library_id: row.try_get("library_id")?,
            document_id: row.try_get("document_id")?,
            revision_number: row.try_get("revision_number")?,
            revision_state: row.try_get("revision_state")?,
            revision_kind: row.try_get("revision_kind")?,
            storage_ref: row.try_get("storage_ref")?,
            source_uri: row.try_get("source_uri")?,
            document_hint: row.try_get("document_hint")?,
            mime_type: row.try_get("mime_type")?,
            checksum: row.try_get("checksum")?,
            title: row.try_get("title")?,
            byte_size: row.try_get("byte_size")?,
            normalized_text: row.try_get("normalized_text")?,
            text_checksum: row.try_get("text_checksum")?,
            image_checksum: row.try_get("image_checksum")?,
            text_state: row.try_get("text_state")?,
            vector_state: row.try_get("vector_state")?,
            graph_state: row.try_get("graph_state")?,
            text_readable_at: row.try_get("text_readable_at")?,
            vector_ready_at: row.try_get("vector_ready_at")?,
            graph_ready_at: row.try_get("graph_ready_at")?,
            superseded_by_revision_id: row.try_get("superseded_by_revision_id")?,
            created_at: row.try_get("created_at")?,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for KnowledgeChunkRow {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            chunk_id: row.try_get("chunk_id")?,
            workspace_id: row.try_get("workspace_id")?,
            library_id: row.try_get("library_id")?,
            document_id: row.try_get("document_id")?,
            revision_id: row.try_get("revision_id")?,
            chunk_index: row.try_get("chunk_index")?,
            chunk_kind: row.try_get("chunk_kind")?,
            content_text: row.try_get("content_text")?,
            normalized_text: row.try_get("normalized_text")?,
            span_start: row.try_get("span_start")?,
            span_end: row.try_get("span_end")?,
            token_count: row.try_get("token_count")?,
            support_block_ids: row.try_get("support_block_ids")?,
            section_path: row.try_get("section_path")?,
            heading_trail: row.try_get("heading_trail")?,
            literal_digest: row.try_get("literal_digest")?,
            chunk_state: row.try_get("chunk_state")?,
            text_generation: row.try_get("text_generation")?,
            vector_generation: row.try_get("vector_generation")?,
            quality_score: row.try_get("quality_score")?,
            window_text: row.try_get("window_text")?,
            occurred_at: row.try_get("occurred_at")?,
            occurred_until: row.try_get("occurred_until")?,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for KnowledgeChunkSupportReferenceRow {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            chunk_id: row.try_get("chunk_id")?,
            support_block_ids: row.try_get("support_block_ids")?,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for KnowledgeStructuredRevisionRow {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            revision_id: row.try_get("revision_id")?,
            workspace_id: row.try_get("workspace_id")?,
            library_id: row.try_get("library_id")?,
            document_id: row.try_get("document_id")?,
            preparation_state: row.try_get("preparation_state")?,
            normalization_profile: row.try_get("normalization_profile")?,
            source_format: row.try_get("source_format")?,
            language_code: row.try_get("language_code")?,
            block_count: row.try_get("block_count")?,
            chunk_count: row.try_get("chunk_count")?,
            typed_fact_count: row.try_get("typed_fact_count")?,
            outline_json: row.try_get("outline_json")?,
            prepared_at: row.try_get("prepared_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for StructuredRevisionCounts {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            block_count: row.try_get("block_count")?,
            typed_fact_count: row.try_get("typed_fact_count")?,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for KnowledgeStructuredBlockRow {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            block_id: row.try_get("block_id")?,
            workspace_id: row.try_get("workspace_id")?,
            library_id: row.try_get("library_id")?,
            document_id: row.try_get("document_id")?,
            revision_id: row.try_get("revision_id")?,
            ordinal: row.try_get("ordinal")?,
            block_kind: row.try_get("block_kind")?,
            text: row.try_get("text")?,
            normalized_text: row.try_get("normalized_text")?,
            heading_trail: row.try_get("heading_trail")?,
            section_path: row.try_get("section_path")?,
            page_number: row.try_get("page_number")?,
            span_start: row.try_get("span_start")?,
            span_end: row.try_get("span_end")?,
            parent_block_id: row.try_get("parent_block_id")?,
            table_coordinates_json: row.try_get("table_coordinates_json")?,
            code_language: row.try_get("code_language")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for KnowledgeTechnicalFactRow {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            fact_id: row.try_get("fact_id")?,
            workspace_id: row.try_get("workspace_id")?,
            library_id: row.try_get("library_id")?,
            document_id: row.try_get("document_id")?,
            revision_id: row.try_get("revision_id")?,
            fact_kind: row.try_get("fact_kind")?,
            normalized_value_text: row.try_get("\u{63}anonical_value_text")?,
            normalized_value_exact: row.try_get("\u{63}anonical_value_exact")?,
            normalized_value_json: row.try_get("\u{63}anonical_value_json")?,
            display_value: row.try_get("display_value")?,
            qualifiers_json: row.try_get("qualifiers_json")?,
            support_block_ids: row.try_get("support_block_ids")?,
            support_chunk_ids: row.try_get("support_chunk_ids")?,
            confidence: row.try_get("confidence")?,
            extraction_kind: row.try_get("extraction_kind")?,
            conflict_group_id: row.try_get("conflict_group_id")?,
            created_at: row.try_get("created_at")?,
            updated_at: row.try_get("updated_at")?,
        })
    }
}

impl<'r> FromRow<'r, PgRow> for LibraryGenerationSignals {
    fn from_row(row: &'r PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            active_text_generation: row.try_get("active_text_generation")?,
            active_vector_generation: row.try_get("active_vector_generation")?,
            active_graph_generation: row.try_get("active_graph_generation")?,
            has_ready_text: row.try_get("has_ready_text")?,
            has_ready_vector: row.try_get("has_ready_vector")?,
            has_ready_graph: row.try_get("has_ready_graph")?,
            latest_created_at: row.try_get("latest_created_at")?,
        })
    }
}

mod document_store;
mod support;

use support::*;

#[cfg(test)]
fn materialized_cte_body(sql: &str, cte_name: &str) -> String {
    let lowered = sql.to_ascii_lowercase();
    let marker = format!("{cte_name} as materialized (");
    let Some(marker_at) = lowered.find(&marker) else {
        return String::new();
    };
    let start = marker_at + marker.len();
    let bytes = sql.as_bytes();
    let mut depth = 1;
    let mut index = start;
    while index < bytes.len() {
        match bytes[index] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return sql[start..index].to_string();
                }
            }
            _ => {}
        }
        index += 1;
    }
    String::new()
}

#[cfg(test)]
mod revision_prefix_tests;
#[cfg(test)]
mod tests;
