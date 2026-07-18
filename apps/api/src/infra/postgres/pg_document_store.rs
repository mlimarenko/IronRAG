use std::{
    cmp::Reverse,
    collections::{BTreeSet, HashSet},
};

use anyhow::Context;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Row, postgres::PgRow};
use uuid::Uuid;

use crate::infra::{
    knowledge_plane::DocumentStore,
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
const KNOWLEDGE_CHUNK_MULTI_REVISION_INDEX_CANDIDATE_MULTIPLIER: usize = 8;
const KNOWLEDGE_CHUNK_MULTI_REVISION_INDEX_CANDIDATE_CAP: usize = 2_048;
const KNOWLEDGE_CHUNK_MULTI_REVISION_TSQUERY_PREFIX_CHARS: usize = 5;
const KNOWLEDGE_CHUNK_MULTI_REVISION_TSQUERY_MIN_TOKEN_CHARS: usize = 4;
const KNOWLEDGE_CHUNK_MULTI_REVISION_TSQUERY_TERM_CAP: usize = 24;
const KNOWLEDGE_CHUNK_MULTI_REVISION_STATEMENT_TIMEOUT_MS: u64 = 8_000;
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
const CHUNK_COLUMNS: &str = "chunk_id, workspace_id, library_id, document_id, revision_id, chunk_index, chunk_kind, content_text, normalized_text, span_start, span_end, token_count, support_block_ids, section_path, heading_trail, literal_digest, chunk_state, text_generation, vector_generation, quality_score, window_text, raptor_level, occurred_at, occurred_until";
const QUALIFIED_CHUNK_COLUMNS: &str = "c.chunk_id, c.workspace_id, c.library_id, c.document_id, c.revision_id, c.chunk_index, c.chunk_kind, c.content_text, c.normalized_text, c.span_start, c.span_end, c.token_count, c.support_block_ids, c.section_path, c.heading_trail, c.literal_digest, c.chunk_state, c.text_generation, c.vector_generation, c.quality_score, c.window_text, c.raptor_level, c.occurred_at, c.occurred_until";
const STRUCTURED_REVISION_COLUMNS: &str = "revision_id, workspace_id, library_id, document_id, preparation_state, normalization_profile, source_format, language_code, block_count::int4 AS block_count, chunk_count::int4 AS chunk_count, typed_fact_count::int4 AS typed_fact_count, outline_json, prepared_at, updated_at";
const STRUCTURED_BLOCK_COLUMNS: &str = "block_id, workspace_id, library_id, document_id, revision_id, ordinal, block_kind, text, normalized_text, heading_trail, section_path, page_number, span_start, span_end, parent_block_id, table_coordinates_json, code_language, created_at, updated_at";
const TECHNICAL_FACT_COLUMNS: &str = "fact_id, workspace_id, library_id, document_id, revision_id, fact_kind, canonical_value_text, canonical_value_exact, canonical_value_json, display_value, qualifiers_json, support_block_ids, support_chunk_ids, confidence, extraction_kind, conflict_group_id, created_at, updated_at";

fn setup_structured_blocks_by_revision_sql() -> String {
    format!(
        "WITH sampled AS (
           SELECT b.*, 0::integer AS lane_rank
           FROM knowledge_structured_block b
           WHERE b.revision_id = $1
           ORDER BY b.ordinal ASC, b.block_id ASC
           LIMIT $2
         ), structured AS (
           SELECT b.*, 1::integer AS lane_rank
           FROM knowledge_structured_block b
           WHERE b.revision_id = $1
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

fn code_pattern_chunk_search_sql() -> String {
    format!(
        "select {QUALIFIED_CHUNK_COLUMNS}
         from knowledge_chunk c
         join knowledge_document d
           on d.document_id = c.document_id
          and d.library_id = c.library_id
          and d.readable_revision_id = c.revision_id
          and d.document_state = 'active'
          and d.deleted_at is null
         cross join lateral (
           select lower(concat_ws(' ', c.normalized_text, c.content_text, c.window_text)) as text
         ) text_parts
         cross join lateral (
           select count(distinct term)::int as matched_count
           from unnest($3::text[]) as term
           where strpos(text_parts.text, term) > 0
         ) matches
         cross join lateral (
           select
             text_parts.text ~* $5 as assignment_shape,
             text_parts.text ~* $6 as numeric_mapping_shape,
             text_parts.text ~* $7 as section_shape
         ) shapes
         where c.library_id = $1
           and c.document_id = any($2::uuid[])
           and c.chunk_state = 'ready'
           and c.raptor_level is null
           and c.chunk_kind is distinct from 'source_profile'
           and not starts_with(c.normalized_text, '[source_profile ')
           and not starts_with(c.content_text, '[source_profile ')
           and matches.matched_count >= $4
           and shapes.assignment_shape
         order by (
           matches.matched_count * 3000
           + 5000
           + case when shapes.numeric_mapping_shape then 3500 else 0 end
           + case when shapes.section_shape then 800 else 0 end
         ) desc,
         c.revision_id desc,
         c.chunk_index asc,
         c.chunk_id asc
         limit $8"
    )
}

fn transport_pattern_chunk_search_sql() -> String {
    format!(
        "select {QUALIFIED_CHUNK_COLUMNS}
         from knowledge_chunk c
         join knowledge_document d
           on d.document_id = c.document_id
          and d.library_id = c.library_id
          and d.readable_revision_id = c.revision_id
          and d.document_state = 'active'
          and d.deleted_at is null
         cross join lateral (
           select lower(concat_ws(' ', c.normalized_text, c.content_text, c.window_text)) as text
         ) text_parts
         cross join lateral (
           select count(distinct term)::int as matched_count
           from unnest($3::text[]) as term
           where strpos(text_parts.text, term) > 0
         ) matches
         cross join lateral (
           select
             strpos(text_parts.text, '=http://') > 0
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
             strpos(text_parts.text, '=') > 0 as config_assignment_shape
         ) shapes
         where c.library_id = $1
           and c.document_id = any($2::uuid[])
           and c.chunk_state = 'ready'
           and c.raptor_level is null
           and c.chunk_kind is distinct from 'source_profile'
           and not starts_with(c.normalized_text, '[source_profile ')
           and not starts_with(c.content_text, '[source_profile ')
           and matches.matched_count >= 1
           and (shapes.url_assignment_shape or shapes.port_assignment_shape)
           and not shapes.media_reference_shape
         order by (
           matches.matched_count * 3000
           + case when shapes.url_assignment_shape then 4500 else 0 end
           + case when shapes.port_assignment_shape then 2200 else 0 end
           + case when shapes.section_shape then 900 else 0 end
           + case when shapes.config_assignment_shape then 600 else 0 end
         ) desc,
         c.revision_id desc,
         c.chunk_index asc,
         c.chunk_id asc
         limit $4"
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

fn multi_revision_matching_terms_index_candidate_cap(candidate_cap: usize) -> usize {
    candidate_cap
        .saturating_mul(KNOWLEDGE_CHUNK_MULTI_REVISION_INDEX_CANDIDATE_MULTIPLIER)
        .min(KNOWLEDGE_CHUNK_MULTI_REVISION_INDEX_CANDIDATE_CAP)
}

fn multi_revision_matching_terms_index_budgets(
    revision_count: usize,
    per_revision_limit: usize,
    candidate_cap: usize,
) -> (usize, usize) {
    let revision_count = revision_count.max(1);
    let per_revision_limit = per_revision_limit.max(1);
    let per_revision_cap =
        (candidate_cap / revision_count).max(per_revision_limit).min(candidate_cap.max(1));
    let revision_cap = (candidate_cap / per_revision_cap).max(1);
    (per_revision_cap, revision_cap)
}

const fn should_prefilter_multi_revision_matching_terms(
    revision_count: usize,
    per_revision_limit: usize,
    has_safe_tsquery: bool,
) -> bool {
    has_safe_tsquery
        && revision_count.saturating_mul(per_revision_limit)
            > KNOWLEDGE_CHUNK_MULTI_REVISION_MATCH_CANDIDATE_CAP
}

fn multi_revision_matching_terms_tsquery(terms: &[String]) -> Option<String> {
    let mut seen = BTreeSet::new();
    let mut prefixes = Vec::new();
    for token in terms.iter().flat_map(|term| term.split(|ch: char| !ch.is_alphanumeric())) {
        if prefixes.len() >= KNOWLEDGE_CHUNK_MULTI_REVISION_TSQUERY_TERM_CAP {
            break;
        }
        push_multi_revision_tsquery_prefix(token, &mut seen, &mut prefixes)?;
    }
    (!prefixes.is_empty()).then(|| {
        prefixes
            .iter()
            .map(|prefix| format!("'{}':*", prefix.replace('\'', "''")))
            .collect::<Vec<_>>()
            .join(" | ")
    })
}

fn push_multi_revision_tsquery_prefix(
    token: &str,
    seen: &mut BTreeSet<String>,
    prefixes: &mut Vec<String>,
) -> Option<()> {
    if token.is_empty() {
        return Some(());
    }
    let normalized = token.to_lowercase();
    if normalized.chars().count() < KNOWLEDGE_CHUNK_MULTI_REVISION_TSQUERY_MIN_TOKEN_CHARS {
        return None;
    }
    let prefix = normalized
        .chars()
        .take(KNOWLEDGE_CHUNK_MULTI_REVISION_TSQUERY_PREFIX_CHARS)
        .collect::<String>();
    if seen.insert(prefix.clone()) {
        prefixes.push(prefix);
    }
    Some(())
}

fn multi_revision_matching_terms_sql(use_index_prefilter: bool) -> String {
    let indexed_candidates = if use_index_prefilter {
        "requested_pool as materialized (
           select revision_id, ordinal
           from requested
           order by hashtextextended(revision_id::text, 0), ordinal
           limit $8
         ),
         indexed_candidates as materialized (
           select candidate.chunk_id,
             candidate.revision_id,
             candidate.chunk_index,
             requested_pool.ordinal
           from requested_pool
           cross join lateral (
             select c.chunk_id, c.revision_id, c.chunk_index
             from knowledge_chunk c
             where c.revision_id = requested_pool.revision_id
               and c.chunk_state = 'ready'
               and c.raptor_level is null
               and c.chunk_kind is distinct from 'source_profile'
               and not starts_with(c.normalized_text, '[source_profile ')
               and not starts_with(c.content_text, '[source_profile ')
               and c.search_tsv @@ to_tsquery('simple', ironrag_unaccent($5))
             limit $7
           ) candidate
           limit $6
         ),"
    } else {
        ""
    };
    let scored_source = if use_index_prefilter {
        "from indexed_candidates candidate
           join knowledge_chunk c on c.chunk_id = candidate.chunk_id"
    } else {
        "from requested
           join knowledge_chunk c on c.revision_id = requested.revision_id"
    };
    let ordinal = if use_index_prefilter { "candidate.ordinal" } else { "requested.ordinal" };
    format!(
        "with requested as (
           select revision_id, min(ordinal)::integer as ordinal
           from unnest($1::uuid[]) with ordinality as request(revision_id, ordinal)
           group by revision_id
         ),
         {indexed_candidates}
         scored as (
           select c.chunk_id,
             c.revision_id,
             c.chunk_index,
             {ordinal} as ordinal,
             (matches.matched_count * 10000 - matches.earliest_pos) as match_score
           {scored_source}
           cross join lateral (
             select
               lower(c.normalized_text) as normalized_lower,
               lower(c.content_text) as content_lower,
               lower(coalesce(c.window_text, '')) as window_lower
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
           where c.chunk_state = 'ready'
             and c.raptor_level is null
             and c.chunk_kind is distinct from 'source_profile'
             and not starts_with(c.normalized_text, '[source_profile ')
             and not starts_with(c.content_text, '[source_profile ')
             and matches.matched_count > 0
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
            raptor_level: row.try_get("raptor_level")?,
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
            canonical_value_text: row.try_get("canonical_value_text")?,
            canonical_value_exact: row.try_get("canonical_value_exact")?,
            canonical_value_json: row.try_get("canonical_value_json")?,
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

impl PgDocumentStore {
    async fn insert_structured_block(
        &self,
        row: &KnowledgeStructuredBlockRow,
    ) -> anyhow::Result<()> {
        sqlx::query(
            "INSERT INTO knowledge_structured_block (
                block_id, workspace_id, library_id, document_id, revision_id, ordinal, block_kind,
                text, normalized_text, heading_trail, section_path, page_number, span_start,
                span_end, parent_block_id, table_coordinates_json, code_language, created_at,
                updated_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19)",
        )
        .bind(row.block_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.document_id)
        .bind(row.revision_id)
        .bind(row.ordinal)
        .bind(&row.block_kind)
        .bind(&row.text)
        .bind(&row.normalized_text)
        .bind(&row.heading_trail)
        .bind(&row.section_path)
        .bind(row.page_number)
        .bind(row.span_start)
        .bind(row.span_end)
        .bind(row.parent_block_id)
        .bind(row.table_coordinates_json.clone())
        .bind(&row.code_language)
        .bind(row.created_at)
        .bind(row.updated_at)
        .execute(&self.pool)
        .await
        .context("failed to insert structured block")?;
        Ok(())
    }

    async fn insert_technical_fact(
        &self,
        row: &KnowledgeTechnicalFactRow,
    ) -> anyhow::Result<KnowledgeTechnicalFactRow> {
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_technical_fact (
                fact_id, workspace_id, library_id, document_id, revision_id, fact_kind,
                canonical_value_text, canonical_value_exact, canonical_value_json, display_value,
                qualifiers_json, support_block_ids, support_chunk_ids, confidence,
                extraction_kind, conflict_group_id, created_at, updated_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18)
             RETURNING {TECHNICAL_FACT_COLUMNS}"
        )))
        .bind(row.fact_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.document_id)
        .bind(row.revision_id)
        .bind(&row.fact_kind)
        .bind(&row.canonical_value_text)
        .bind(&row.canonical_value_exact)
        .bind(row.canonical_value_json.clone())
        .bind(&row.display_value)
        .bind(row.qualifiers_json.clone())
        .bind(&row.support_block_ids)
        .bind(&row.support_chunk_ids)
        .bind(row.confidence)
        .bind(&row.extraction_kind)
        .bind(&row.conflict_group_id)
        .bind(row.created_at)
        .bind(row.updated_at)
        .fetch_one(&self.pool)
        .await
        .context("failed to insert technical fact")
    }
}

#[async_trait]
impl DocumentStore for PgDocumentStore {
    async fn upsert_document(
        &self,
        row: &KnowledgeDocumentRow,
    ) -> anyhow::Result<KnowledgeDocumentRow> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_document (
                document_id, workspace_id, library_id, external_key, file_name, title,
                document_state, active_revision_id, readable_revision_id, latest_revision_no,
                parent_document_id, document_role,
                created_at, updated_at, deleted_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)
             ON CONFLICT (document_id) DO UPDATE SET
                workspace_id = EXCLUDED.workspace_id,
                library_id = EXCLUDED.library_id,
                external_key = EXCLUDED.external_key,
                file_name = EXCLUDED.file_name,
                title = EXCLUDED.title,
                document_state = EXCLUDED.document_state,
                active_revision_id = EXCLUDED.active_revision_id,
                readable_revision_id = EXCLUDED.readable_revision_id,
                latest_revision_no = EXCLUDED.latest_revision_no,
                parent_document_id = EXCLUDED.parent_document_id,
                document_role = EXCLUDED.document_role,
                updated_at = EXCLUDED.updated_at,
                deleted_at = EXCLUDED.deleted_at
             RETURNING {DOCUMENT_COLUMNS}"
        )))
        .bind(row.document_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(&row.external_key)
        .bind(&row.file_name)
        .bind(&row.title)
        .bind(&row.document_state)
        .bind(row.active_revision_id)
        .bind(row.readable_revision_id)
        .bind(row.latest_revision_no)
        .bind(row.parent_document_id)
        .bind(&row.document_role)
        .bind(row.created_at)
        .bind(row.updated_at)
        .bind(row.deleted_at)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert knowledge document")
    }

    async fn get_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS} FROM knowledge_document WHERE document_id = $1 LIMIT 1"
        )))
        .bind(document_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get knowledge document")
    }

    async fn get_document_by_external_key(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        external_key: &str,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS}
             FROM knowledge_document
             WHERE workspace_id = $1 AND library_id = $2 AND external_key = $3
             LIMIT 1"
        )))
        .bind(workspace_id)
        .bind(library_id)
        .bind(external_key)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get knowledge document by external key")
    }

    async fn list_documents_by_library(
        &self,
        workspace_id: Uuid,
        library_id: Uuid,
        include_deleted: bool,
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS}
             FROM knowledge_document
             WHERE workspace_id = $1
               AND library_id = $2
               AND ($3 OR document_state <> 'deleted')
             ORDER BY updated_at DESC, document_id DESC"
        )))
        .bind(workspace_id)
        .bind(library_id)
        .bind(include_deleted)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge documents by library")
    }

    async fn list_documents_by_ids(
        &self,
        document_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeDocumentRow>> {
        if document_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {DOCUMENT_COLUMNS}
             FROM knowledge_document
             WHERE document_id = ANY($1) AND document_state <> 'deleted'
             ORDER BY updated_at DESC, document_id DESC"
        )))
        .bind(document_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge documents by ids")
    }

    async fn update_document_pointers(
        &self,
        document_id: Uuid,
        document_state: &str,
        active_revision_id: Option<Uuid>,
        readable_revision_id: Option<Uuid>,
        latest_revision_no: Option<i64>,
        title: Option<&str>,
        deleted_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeDocumentRow>> {
        sqlx::query_as::<_, KnowledgeDocumentRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_document SET
                document_state = $2,
                active_revision_id = $3,
                readable_revision_id = $4,
                latest_revision_no = $5,
                title = $6,
                updated_at = now(),
                deleted_at = $7
             WHERE document_id = $1
             RETURNING {DOCUMENT_COLUMNS}"
        )))
        .bind(document_id)
        .bind(document_state)
        .bind(active_revision_id)
        .bind(readable_revision_id)
        .bind(latest_revision_no)
        .bind(title)
        .bind(deleted_at)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge document pointers")
    }

    async fn upsert_revision(
        &self,
        row: &KnowledgeRevisionRow,
    ) -> anyhow::Result<KnowledgeRevisionRow> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_revision (
                revision_id, workspace_id, library_id, document_id, revision_number,
                revision_state, revision_kind, storage_ref, source_uri, document_hint, mime_type,
                checksum, title, byte_size, normalized_text, text_checksum, image_checksum,
                text_state, vector_state, graph_state, text_readable_at, vector_ready_at,
                graph_ready_at, superseded_by_revision_id, created_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25)
             ON CONFLICT (revision_id) DO UPDATE SET
                workspace_id = EXCLUDED.workspace_id,
                library_id = EXCLUDED.library_id,
                document_id = EXCLUDED.document_id,
                revision_number = EXCLUDED.revision_number,
                revision_state = EXCLUDED.revision_state,
                revision_kind = EXCLUDED.revision_kind,
                storage_ref = EXCLUDED.storage_ref,
                source_uri = EXCLUDED.source_uri,
                document_hint = EXCLUDED.document_hint,
                mime_type = EXCLUDED.mime_type,
                checksum = EXCLUDED.checksum,
                title = EXCLUDED.title,
                byte_size = EXCLUDED.byte_size,
                normalized_text = EXCLUDED.normalized_text,
                text_checksum = EXCLUDED.text_checksum,
                image_checksum = EXCLUDED.image_checksum,
                text_state = EXCLUDED.text_state,
                vector_state = EXCLUDED.vector_state,
                graph_state = EXCLUDED.graph_state,
                text_readable_at = EXCLUDED.text_readable_at,
                vector_ready_at = EXCLUDED.vector_ready_at,
                graph_ready_at = EXCLUDED.graph_ready_at,
                superseded_by_revision_id = EXCLUDED.superseded_by_revision_id
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(row.revision_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.document_id)
        .bind(row.revision_number)
        .bind(&row.revision_state)
        .bind(&row.revision_kind)
        .bind(&row.storage_ref)
        .bind(&row.source_uri)
        .bind(&row.document_hint)
        .bind(&row.mime_type)
        .bind(&row.checksum)
        .bind(&row.title)
        .bind(row.byte_size)
        .bind(&row.normalized_text)
        .bind(&row.text_checksum)
        .bind(&row.image_checksum)
        .bind(&row.text_state)
        .bind(&row.vector_state)
        .bind(&row.graph_state)
        .bind(row.text_readable_at)
        .bind(row.vector_ready_at)
        .bind(row.graph_ready_at)
        .bind(row.superseded_by_revision_id)
        .bind(row.created_at)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert knowledge revision")
    }

    async fn update_revision_document_hint(
        &self,
        revision_id: Uuid,
        document_hint: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET document_hint = $2
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(document_hint)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision document hint")
    }

    async fn get_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS} FROM knowledge_revision WHERE revision_id = $1 LIMIT 1"
        )))
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get knowledge revision")
    }

    async fn list_revisions_by_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS}
             FROM knowledge_revision
             WHERE revision_id = ANY($1)"
        )))
        .bind(revision_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge revisions by ids")
    }

    async fn list_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {REVISION_COLUMNS}
             FROM knowledge_revision
             WHERE document_id = $1
             ORDER BY revision_number DESC, revision_id DESC"
        )))
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge revisions by document")
    }

    async fn aggregate_library_generation_signals(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<LibraryGenerationSignals> {
        sqlx::query_as::<_, LibraryGenerationSignals>(
            "SELECT
                COALESCE(MAX(revision_number) FILTER (WHERE text_state = 'text_readable'), 0) AS active_text_generation,
                COALESCE(MAX(revision_number) FILTER (WHERE vector_state = 'ready'), 0) AS active_vector_generation,
                COALESCE(MAX(revision_number) FILTER (WHERE graph_state = 'ready'), 0) AS active_graph_generation,
                COALESCE(bool_or(text_state = 'text_readable'), false) AS has_ready_text,
                COALESCE(bool_or(vector_state = 'ready'), false) AS has_ready_vector,
                COALESCE(bool_or(graph_state = 'ready'), false) AS has_ready_graph,
                MAX(created_at) AS latest_created_at
             FROM knowledge_revision
             WHERE library_id = $1",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to aggregate library generation signals")
    }

    async fn count_vector_ready_revisions_missing_chunk_vectors(
        &self,
        library_id: Uuid,
    ) -> anyhow::Result<i64> {
        let ready_revision_ids = sqlx::query_as::<_, (Uuid,)>(
            "SELECT r.revision_id
             FROM knowledge_revision r
             WHERE r.library_id = $1
               AND r.vector_state = 'ready'
               AND r.superseded_by_revision_id IS NULL
               AND EXISTS (
                 SELECT 1
                 FROM knowledge_chunk c
                 WHERE c.revision_id = r.revision_id
                   AND c.raptor_level IS NULL
                 LIMIT 1
               )",
        )
        .bind(library_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list vector-ready revisions with chunks")?
        .into_iter()
        .map(|(revision_id,)| revision_id)
        .collect::<Vec<_>>();

        if ready_revision_ids.is_empty() {
            return Ok(0);
        }

        let relation_names = sqlx::query_as::<_, (String,)>(
            "SELECT DISTINCT relation_name
             FROM knowledge_vector_relation_manifest
             WHERE library_id = $1 AND vector_kind = $2",
        )
        .bind(library_id)
        .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
        .fetch_all(&self.pool)
        .await
        .context("failed to list chunk vector relations")?
        .into_iter()
        .map(|(relation_name,)| relation_name)
        .collect::<Vec<_>>();

        let mut vector_revision_ids = HashSet::new();
        for relation_name in relation_names {
            let sql = format!(
                "SELECT DISTINCT revision_id FROM {} WHERE library_id = $1 AND revision_id = ANY($2) AND vector_kind = $3",
                quote_relation_name(&relation_name)
            );
            let rows = sqlx::query_as::<_, (Uuid,)>(sqlx::AssertSqlSafe(&*sql))
                .bind(library_id)
                .bind(&ready_revision_ids)
                .bind(KNOWLEDGE_CHUNK_VECTOR_KIND)
                .fetch_all(&self.pool)
                .await
                .with_context(|| format!("failed to count vector inventory in {relation_name}"))?;
            vector_revision_ids.extend(rows.into_iter().map(|(revision_id,)| revision_id));
        }

        Ok(ready_revision_ids
            .iter()
            .filter(|revision_id| !vector_revision_ids.contains(revision_id))
            .count()
            .try_into()
            .unwrap_or(i64::MAX))
    }

    async fn update_revision_readiness(
        &self,
        revision_id: Uuid,
        text_state: &str,
        vector_state: &str,
        graph_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
        vector_ready_at: Option<DateTime<Utc>>,
        graph_ready_at: Option<DateTime<Utc>>,
        superseded_by_revision_id: Option<Uuid>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET
                text_state = $2,
                vector_state = $3,
                graph_state = $4,
                text_readable_at = $5,
                vector_ready_at = $6,
                graph_ready_at = $7,
                superseded_by_revision_id = $8
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(text_state)
        .bind(vector_state)
        .bind(graph_state)
        .bind(text_readable_at)
        .bind(vector_ready_at)
        .bind(graph_ready_at)
        .bind(superseded_by_revision_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision readiness")
    }

    async fn update_revision_text_content(
        &self,
        revision_id: Uuid,
        normalized_text: Option<&str>,
        text_checksum: Option<&str>,
        text_state: &str,
        text_readable_at: Option<DateTime<Utc>>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET
                normalized_text = $2,
                text_checksum = $3,
                text_state = $4,
                text_readable_at = $5
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(normalized_text)
        .bind(text_checksum)
        .bind(text_state)
        .bind(text_readable_at)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision text content")
    }

    async fn update_revision_image_checksum(
        &self,
        revision_id: Uuid,
        image_checksum: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET image_checksum = $2
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(image_checksum)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision image checksum")
    }

    async fn update_revision_storage_ref(
        &self,
        revision_id: Uuid,
        storage_ref: Option<&str>,
    ) -> anyhow::Result<Option<KnowledgeRevisionRow>> {
        sqlx::query_as::<_, KnowledgeRevisionRow>(sqlx::AssertSqlSafe(format!(
            "UPDATE knowledge_revision SET storage_ref = $2
             WHERE revision_id = $1
             RETURNING {REVISION_COLUMNS}"
        )))
        .bind(revision_id)
        .bind(storage_ref)
        .fetch_optional(&self.pool)
        .await
        .context("failed to update knowledge revision storage ref")
    }

    async fn upsert_chunk(&self, row: &KnowledgeChunkRow) -> anyhow::Result<KnowledgeChunkRow> {
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_chunk (
                chunk_id, workspace_id, library_id, document_id, revision_id, chunk_index,
                chunk_kind, content_text, normalized_text, span_start, span_end, token_count,
                support_block_ids, section_path, heading_trail, literal_digest, chunk_state,
                text_generation, vector_generation, quality_score, window_text, raptor_level,
                occurred_at, occurred_until
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24)
             ON CONFLICT (chunk_id) DO UPDATE SET
                chunk_kind = EXCLUDED.chunk_kind,
                content_text = EXCLUDED.content_text,
                normalized_text = EXCLUDED.normalized_text,
                span_start = EXCLUDED.span_start,
                span_end = EXCLUDED.span_end,
                token_count = EXCLUDED.token_count,
                support_block_ids = EXCLUDED.support_block_ids,
                section_path = EXCLUDED.section_path,
                heading_trail = EXCLUDED.heading_trail,
                literal_digest = EXCLUDED.literal_digest,
                chunk_state = EXCLUDED.chunk_state,
                text_generation = EXCLUDED.text_generation,
                vector_generation = EXCLUDED.vector_generation,
                quality_score = EXCLUDED.quality_score,
                window_text = EXCLUDED.window_text,
                raptor_level = EXCLUDED.raptor_level,
                occurred_at = EXCLUDED.occurred_at,
                occurred_until = EXCLUDED.occurred_until
             RETURNING {CHUNK_COLUMNS}"
        )))
        .bind(row.chunk_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.document_id)
        .bind(row.revision_id)
        .bind(row.chunk_index)
        .bind(&row.chunk_kind)
        .bind(&row.content_text)
        .bind(&row.normalized_text)
        .bind(row.span_start)
        .bind(row.span_end)
        .bind(row.token_count)
        .bind(&row.support_block_ids)
        .bind(&row.section_path)
        .bind(&row.heading_trail)
        .bind(&row.literal_digest)
        .bind(&row.chunk_state)
        .bind(row.text_generation)
        .bind(row.vector_generation)
        .bind(row.quality_score)
        .bind(&row.window_text)
        .bind(row.raptor_level)
        .bind(row.occurred_at)
        .bind(row.occurred_until)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert knowledge chunk")
    }

    async fn insert_chunks(
        &self,
        rows: &[KnowledgeChunkRow],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut inserted = Vec::with_capacity(rows.len());
        for chunk in rows.chunks(KNOWLEDGE_CHUNK_INSERT_BATCH_ROWS) {
            for row in chunk {
                inserted.push(self.upsert_chunk(row).await?);
            }
        }
        Ok(inserted)
    }

    async fn list_active_chunks_by_library_page(
        &self,
        library_id: Uuid,
        after: Option<(i32, Uuid)>,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let limit = i64::try_from(limit).context("active chunk page limit overflowed i64")?;
        let (after_chunk_index, after_chunk_id) = after.unzip();
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            active_chunks_by_library_page_sql(),
        ))
        .bind(library_id)
        .bind(after_chunk_index)
        .bind(after_chunk_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .context("failed to list active canonical chunk page by library")
    }

    async fn count_active_chunks_by_library(&self, library_id: Uuid) -> anyhow::Result<u64> {
        let count = sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*)::bigint
             FROM knowledge_chunk c
             JOIN knowledge_document d
               ON d.document_id = c.document_id
              AND d.library_id = c.library_id
              AND d.readable_revision_id = c.revision_id
              AND d.document_state = 'active'
              AND d.deleted_at IS NULL
             WHERE c.library_id = $1
               AND c.chunk_state = 'ready'
               AND c.raptor_level IS NULL",
        )
        .bind(library_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to count active canonical chunks by library")?;
        u64::try_from(count).context("active canonical chunk count overflowed u64")
    }

    async fn list_source_profile_chunks_by_revisions(
        &self,
        library_id: Uuid,
        revision_ids: &[Uuid],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 || revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "WITH ranked_revisions AS (
               SELECT revision_id, min(input_rank) AS input_rank
               FROM unnest($2::uuid[]) WITH ORDINALITY AS r(revision_id, input_rank)
               GROUP BY revision_id
             )
             SELECT {CHUNK_COLUMNS}
             FROM (
               SELECT c.*, r.input_rank
               FROM knowledge_chunk c
               JOIN ranked_revisions r
                 ON r.revision_id = c.revision_id
               WHERE c.library_id = $1
                 AND c.chunk_state = 'ready'
                 AND c.raptor_level IS NULL
                 AND (
                   c.chunk_kind = 'source_profile'
                   OR starts_with(c.normalized_text, '[source_profile ')
                   OR starts_with(c.content_text, '[source_profile ')
                 )
             ) c
             ORDER BY c.input_rank ASC, c.chunk_index ASC, c.chunk_id ASC
             LIMIT $3"
        )))
        .bind(library_id)
        .bind(revision_ids)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to list source profile chunks by revisions")
    }

    async fn list_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk c
             WHERE c.revision_id = $1
               AND c.raptor_level IS NULL
             ORDER BY chunk_index ASC, chunk_id ASC"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision")
    }

    async fn list_head_chunks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk c
             WHERE c.revision_id = $1
               AND c.raptor_level IS NULL
             ORDER BY chunk_index ASC, chunk_id ASC
             LIMIT $2"
        )))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to list head knowledge chunks by revision")
    }

    async fn count_chunks_by_revision(&self, revision_id: Uuid) -> anyhow::Result<i64> {
        sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*)
             FROM knowledge_chunk
             WHERE revision_id = $1
               AND chunk_state = 'ready'
               AND raptor_level IS NULL",
        )
        .bind(revision_id)
        .fetch_one(&self.pool)
        .await
        .map(|(count,)| count)
        .context("failed to count knowledge chunks by revision")
    }

    async fn list_chunks_by_revision_matching_terms(
        &self,
        revision_id: Uuid,
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 || terms.is_empty() {
            return Ok(Vec::new());
        }
        let terms = normalized_revision_chunk_terms(terms);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk c
             CROSS JOIN LATERAL (
               SELECT
                 lower(c.normalized_text) AS normalized_lower,
                 lower(c.content_text) AS content_lower,
                 lower(coalesce(c.window_text, '')) AS window_lower
             ) text_parts
             CROSS JOIN LATERAL (
               SELECT
                 COUNT(DISTINCT term)::int AS matched_count,
                 MIN(LEAST(
                   COALESCE(NULLIF(strpos(text_parts.normalized_lower, term), 0), 2147483647),
                   COALESCE(NULLIF(strpos(text_parts.content_lower, term), 0), 2147483647),
                   COALESCE(NULLIF(strpos(text_parts.window_lower, term), 0), 2147483647)
                 )) AS earliest_pos
               FROM unnest($2::text[]) AS term
               WHERE strpos(text_parts.normalized_lower, term) > 0
                  OR strpos(text_parts.content_lower, term) > 0
                  OR strpos(text_parts.window_lower, term) > 0
             ) matches
             WHERE c.revision_id = $1
               AND c.chunk_state = 'ready'
               AND c.raptor_level IS NULL
               AND c.chunk_kind IS DISTINCT FROM 'source_profile'
               AND NOT starts_with(c.normalized_text, '[source_profile ')
               AND NOT starts_with(c.content_text, '[source_profile ')
               AND matches.matched_count > 0
             ORDER BY (matches.matched_count * 10000 - matches.earliest_pos) DESC,
                      c.chunk_index ASC,
                      c.chunk_id ASC
             LIMIT $3"
        )))
        .bind(revision_id)
        .bind(&terms)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision terms")
    }

    async fn list_chunks_by_revisions_matching_terms(
        &self,
        revision_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if revision_ids.is_empty() || limit == 0 || terms.is_empty() {
            return Ok(Vec::new());
        }
        let terms = normalized_revision_chunk_terms(terms);
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let candidate_cap = multi_revision_matching_terms_candidate_cap(revision_ids.len(), limit);
        let index_candidate_cap = multi_revision_matching_terms_index_candidate_cap(candidate_cap);
        let (index_per_revision_cap, index_revision_cap) =
            multi_revision_matching_terms_index_budgets(
                revision_ids.len(),
                limit,
                index_candidate_cap,
            );
        let index_prefilter = multi_revision_matching_terms_tsquery(&terms);
        let broad_request = revision_ids.len().saturating_mul(limit)
            > KNOWLEDGE_CHUNK_MULTI_REVISION_MATCH_CANDIDATE_CAP;
        let use_index_prefilter = should_prefilter_multi_revision_matching_terms(
            revision_ids.len(),
            limit,
            index_prefilter.is_some(),
        );
        let query = sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            multi_revision_matching_terms_sql(use_index_prefilter),
        ))
        .bind(revision_ids)
        .bind(&terms)
        .bind(limit_i64(limit))
        .bind(limit_i64(candidate_cap));
        let query = match index_prefilter.filter(|_| use_index_prefilter) {
            Some(prefilter) => query
                .bind(prefilter)
                .bind(limit_i64(index_candidate_cap))
                .bind(limit_i64(index_per_revision_cap))
                .bind(limit_i64(index_revision_cap)),
            None => query,
        };
        if broad_request {
            let mut transaction = self
                .pool
                .begin()
                .await
                .context("failed to start bounded multi-revision term search")?;
            sqlx::query("select set_config('statement_timeout', $1, true)")
                .bind(format!("{KNOWLEDGE_CHUNK_MULTI_REVISION_STATEMENT_TIMEOUT_MS}ms"))
                .execute(&mut *transaction)
                .await
                .context("failed to set bounded multi-revision term search timeout")?;
            let rows = query
                .fetch_all(&mut *transaction)
                .await
                .context("failed to list bounded knowledge chunks by revision terms")?;
            transaction
                .commit()
                .await
                .context("failed to finish bounded multi-revision term search")?;
            return Ok(rows);
        }
        query
            .fetch_all(&self.pool)
            .await
            .context("failed to list knowledge chunks by revision terms")
    }

    async fn list_chunks_by_revision_range(
        &self,
        revision_id: Uuid,
        min_chunk_index: i32,
        max_chunk_index: i32,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if max_chunk_index < min_chunk_index {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk c
             WHERE c.revision_id = $1
               AND c.raptor_level IS NULL
               AND c.chunk_index BETWEEN $2 AND $3
             ORDER BY chunk_index ASC, chunk_id ASC
             LIMIT $4"
        )))
        .bind(revision_id)
        .bind(min_chunk_index)
        .bind(max_chunk_index)
        .bind(limit_i64(KNOWLEDGE_CHUNK_WINDOW_FETCH_LIMIT))
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision range")
    }

    async fn list_chunks_by_revision_windows(
        &self,
        revision_id: Uuid,
        windows: &[(i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let normalized_windows = normalized_windows(windows);
        if normalized_windows.is_empty() {
            return Ok(Vec::new());
        }
        let min_indexes = normalized_windows.iter().map(|(min, _)| *min).collect::<Vec<_>>();
        let max_indexes = normalized_windows.iter().map(|(_, max)| *max).collect::<Vec<_>>();
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk c
             WHERE c.revision_id = $1
               AND c.raptor_level IS NULL
               AND EXISTS (
                 SELECT 1
                 FROM unnest($2::int4[], $3::int4[]) AS w(min_index, max_index)
                 WHERE c.chunk_index BETWEEN w.min_index AND w.max_index
               )
             ORDER BY c.chunk_index ASC, c.chunk_id ASC
             LIMIT $4"
        )))
        .bind(revision_id)
        .bind(&min_indexes)
        .bind(&max_indexes)
        .bind(limit_i64(KNOWLEDGE_CHUNK_WINDOW_FETCH_LIMIT))
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision windows")
    }

    async fn list_chunks_by_revisions_windows(
        &self,
        windows: &[(Uuid, i32, i32)],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let windows = normalized_revision_windows(windows);
        if windows.is_empty() {
            return Ok(Vec::new());
        }
        let revision_ids =
            windows.iter().map(|(revision_id, _, _)| *revision_id).collect::<Vec<_>>();
        let min_indexes = windows.iter().map(|(_, min, _)| *min).collect::<Vec<_>>();
        let max_indexes = windows.iter().map(|(_, _, max)| *max).collect::<Vec<_>>();
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "WITH requested_windows AS (
               SELECT revision_id, min_index, max_index, ordinal::integer AS ordinal
               FROM unnest($1::uuid[], $2::int4[], $3::int4[]) WITH ORDINALITY
                    AS request(revision_id, min_index, max_index, ordinal)
             ),
             requested_revisions AS (
               SELECT revision_id, MIN(ordinal) AS ordinal
               FROM requested_windows
               GROUP BY revision_id
             ),
             ranked AS (
               SELECT c.*,
                 requested_revisions.ordinal,
                 row_number() OVER (
                   PARTITION BY c.revision_id
                   ORDER BY c.chunk_index ASC, c.chunk_id ASC
                 ) AS revision_rank
               FROM requested_revisions
               JOIN knowledge_chunk c ON c.revision_id = requested_revisions.revision_id
               WHERE c.raptor_level IS NULL
                 AND EXISTS (
                 SELECT 1
                 FROM requested_windows w
                 WHERE w.revision_id = c.revision_id
                   AND c.chunk_index BETWEEN w.min_index AND w.max_index
               )
             )
             SELECT {CHUNK_COLUMNS}
             FROM ranked c
             WHERE revision_rank <= $4
             ORDER BY ordinal ASC, chunk_index ASC, chunk_id ASC"
        )))
        .bind(&revision_ids)
        .bind(&min_indexes)
        .bind(&max_indexes)
        .bind(limit_i64(KNOWLEDGE_CHUNK_WINDOW_FETCH_LIMIT))
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by revision windows")
    }

    async fn list_tail_chunks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM (
               SELECT *
               FROM knowledge_chunk
               WHERE revision_id = $1
                 AND chunk_state = 'ready'
                 AND raptor_level IS NULL
               ORDER BY chunk_index DESC, chunk_id DESC
               LIMIT $2
             ) c
             ORDER BY chunk_index ASC, chunk_id ASC"
        )))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to list tail knowledge chunks by revision")
    }

    async fn list_head_source_unit_blocks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let temporal_start_iso = temporal_start.map(|value| value.to_rfc3339());
        let temporal_end_iso = temporal_end.map(|value| value.to_rfc3339());
        sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM knowledge_structured_block
             CROSS JOIN LATERAL (
               SELECT CASE
                 WHEN $3::text IS NULL AND $4::text IS NULL THEN NULL
                 ELSE substring(normalized_text FROM $7)
               END AS occurred_iso
             ) temporal
             WHERE revision_id = $1
               AND block_kind = 'source_unit'
               AND (NOT $5 OR normalized_text ~ $6)
               AND (
                 ($3::text IS NULL AND $4::text IS NULL)
                 OR (
                   temporal.occurred_iso IS NOT NULL
                   AND ($3::text IS NULL OR temporal.occurred_iso >= $3)
                   AND ($4::text IS NULL OR temporal.occurred_iso <= $4)
                 )
               )
             ORDER BY ordinal ASC, block_id ASC
             LIMIT $2"
        )))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .bind(temporal_start_iso)
        .bind(temporal_end_iso)
        .bind(release_marker_required)
        .bind(SOURCE_UNIT_RELEASE_MARKER_REGEX)
        .bind(SOURCE_UNIT_OCCURRED_AT_REGEX)
        .fetch_all(&self.pool)
        .await
        .context("failed to list head source-unit structured blocks by revision")
    }

    async fn list_tail_source_unit_blocks_by_revision(
        &self,
        revision_id: Uuid,
        limit: usize,
        temporal_start: Option<DateTime<Utc>>,
        temporal_end: Option<DateTime<Utc>>,
        release_marker_required: bool,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let temporal_start_iso = temporal_start.map(|value| value.to_rfc3339());
        let temporal_end_iso = temporal_end.map(|value| value.to_rfc3339());
        sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM (
               SELECT *
               FROM knowledge_structured_block
               CROSS JOIN LATERAL (
                 SELECT CASE
                   WHEN $3::text IS NULL AND $4::text IS NULL THEN NULL
                   ELSE substring(normalized_text FROM $7)
                 END AS occurred_iso
               ) temporal
               WHERE revision_id = $1
                 AND block_kind = 'source_unit'
                 AND (NOT $5 OR normalized_text ~ $6)
                 AND (
                   ($3::text IS NULL AND $4::text IS NULL)
                   OR (
                     temporal.occurred_iso IS NOT NULL
                     AND ($3::text IS NULL OR temporal.occurred_iso >= $3)
                     AND ($4::text IS NULL OR temporal.occurred_iso <= $4)
                   )
                 )
               ORDER BY ordinal DESC, block_id DESC
               LIMIT $2
             ) block
             ORDER BY ordinal ASC, block_id ASC"
        )))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .bind(temporal_start_iso)
        .bind(temporal_end_iso)
        .bind(release_marker_required)
        .bind(SOURCE_UNIT_RELEASE_MARKER_REGEX)
        .bind(SOURCE_UNIT_OCCURRED_AT_REGEX)
        .fetch_all(&self.pool)
        .await
        .context("failed to list tail source-unit structured blocks by revision")
    }

    async fn get_chunk(&self, chunk_id: Uuid) -> anyhow::Result<Option<KnowledgeChunkRow>> {
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk
             WHERE chunk_id = $1
               AND raptor_level IS NULL
             LIMIT 1"
        )))
        .bind(chunk_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get knowledge chunk by id")
    }

    async fn list_chunks_by_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {CHUNK_COLUMNS}
             FROM knowledge_chunk
             WHERE chunk_id = ANY($1)
               AND raptor_level IS NULL
             ORDER BY chunk_id ASC"
        )))
        .bind(chunk_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list knowledge chunks by ids")
    }

    async fn search_code_pattern_chunks_by_terms(
        &self,
        library_id: Uuid,
        candidate_document_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let candidate_document_ids = candidate_document_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let terms = normalized_search_terms(terms);
        if candidate_document_ids.is_empty() || terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let min_terms = i32::try_from(terms.len().clamp(1, 3)).unwrap_or(i32::MAX);
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(code_pattern_chunk_search_sql()))
            .bind(library_id)
            .bind(&candidate_document_ids)
            .bind(&terms)
            .bind(min_terms)
            .bind(CODE_PATTERN_ASSIGNMENT_REGEX)
            .bind(CODE_PATTERN_NUMERIC_MAPPING_REGEX)
            .bind(CODE_PATTERN_SECTION_REGEX)
            .bind(limit_i64(limit))
            .fetch_all(&self.pool)
            .await
            .context("failed to search code-pattern knowledge chunks")
    }

    async fn search_transport_pattern_chunks_by_terms(
        &self,
        library_id: Uuid,
        candidate_document_ids: &[Uuid],
        terms: &[String],
        limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        let candidate_document_ids = candidate_document_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let terms = normalized_search_terms(terms);
        if candidate_document_ids.is_empty() || terms.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(
            transport_pattern_chunk_search_sql(),
        ))
        .bind(library_id)
        .bind(&candidate_document_ids)
        .bind(&terms)
        .bind(limit_i64(limit))
        .fetch_all(&self.pool)
        .await
        .context("failed to search transport-pattern knowledge chunks")
    }

    async fn delete_chunks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkRow>> {
        sqlx::query_as::<_, KnowledgeChunkRow>(sqlx::AssertSqlSafe(format!(
            "DELETE FROM knowledge_chunk
             WHERE revision_id = $1
             RETURNING {CHUNK_COLUMNS}"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to delete knowledge chunks by revision")
    }

    async fn upsert_structured_revision(
        &self,
        row: &KnowledgeStructuredRevisionRow,
    ) -> anyhow::Result<KnowledgeStructuredRevisionRow> {
        sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
            "INSERT INTO knowledge_structured_revision (
                revision_id, workspace_id, library_id, document_id, preparation_state,
                normalization_profile, source_format, language_code, block_count, chunk_count,
                typed_fact_count, outline_json, prepared_at, updated_at
             ) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)
             ON CONFLICT (revision_id) DO UPDATE SET
                preparation_state = EXCLUDED.preparation_state,
                normalization_profile = EXCLUDED.normalization_profile,
                source_format = EXCLUDED.source_format,
                language_code = EXCLUDED.language_code,
                block_count = EXCLUDED.block_count,
                chunk_count = EXCLUDED.chunk_count,
                typed_fact_count = EXCLUDED.typed_fact_count,
                outline_json = EXCLUDED.outline_json,
                prepared_at = EXCLUDED.prepared_at,
                updated_at = EXCLUDED.updated_at
             RETURNING {STRUCTURED_REVISION_COLUMNS}"
        )))
        .bind(row.revision_id)
        .bind(row.workspace_id)
        .bind(row.library_id)
        .bind(row.document_id)
        .bind(&row.preparation_state)
        .bind(&row.normalization_profile)
        .bind(&row.source_format)
        .bind(&row.language_code)
        .bind(i64::from(row.block_count))
        .bind(i64::from(row.chunk_count))
        .bind(i64::from(row.typed_fact_count))
        .bind(row.outline_json.clone())
        .bind(row.prepared_at)
        .bind(row.updated_at)
        .fetch_one(&self.pool)
        .await
        .context("failed to upsert knowledge structured revision")
    }

    async fn get_structured_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<KnowledgeStructuredRevisionRow>> {
        sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_REVISION_COLUMNS}
             FROM knowledge_structured_revision
             WHERE revision_id = $1
             LIMIT 1"
        )))
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get structured revision")
    }

    async fn get_structured_revision_counts(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Option<StructuredRevisionCounts>> {
        sqlx::query_as::<_, StructuredRevisionCounts>(
            "SELECT block_count::int4 AS block_count, typed_fact_count::int4 AS typed_fact_count
             FROM knowledge_structured_revision
             WHERE revision_id = $1
             LIMIT 1",
        )
        .bind(revision_id)
        .fetch_optional(&self.pool)
        .await
        .context("failed to get structured revision counts")
    }

    async fn list_structured_revisions_by_revision_ids(
        &self,
        revision_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        if revision_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_REVISION_COLUMNS}
             FROM knowledge_structured_revision
             WHERE revision_id = ANY($1)"
        )))
        .bind(revision_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured revisions by revision ids")
    }

    async fn list_structured_revisions_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredRevisionRow>> {
        sqlx::query_as::<_, KnowledgeStructuredRevisionRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_REVISION_COLUMNS}
             FROM knowledge_structured_revision
             WHERE document_id = $1
             ORDER BY prepared_at DESC, revision_id DESC"
        )))
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured revisions by document")
    }

    async fn replace_structured_blocks(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeStructuredBlockRow],
    ) -> anyhow::Result<()> {
        self.delete_structured_blocks_by_revision(revision_id).await?;
        for row in rows {
            self.insert_structured_block(row).await?;
        }
        Ok(())
    }

    async fn list_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM knowledge_structured_block
             WHERE revision_id = $1
             ORDER BY ordinal ASC, block_id ASC"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured blocks by revision")
    }

    async fn list_setup_structured_blocks_by_revision(
        &self,
        revision_id: Uuid,
        sample_limit: usize,
        structured_limit: usize,
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        let sample_limit = sample_limit.min(SETUP_STRUCTURED_BLOCK_LANE_LIMIT);
        let structured_limit = structured_limit.min(SETUP_STRUCTURED_BLOCK_LANE_LIMIT);
        if sample_limit == 0 && structured_limit == 0 {
            return Ok(Vec::new());
        }
        let total_limit = sample_limit.saturating_add(structured_limit);
        let mut transaction = self
            .pool
            .begin()
            .await
            .context("failed to start bounded setup structured block read")?;
        sqlx::query("select set_config('statement_timeout', $1, true)")
            .bind(format!("{SETUP_STRUCTURED_BLOCK_STATEMENT_TIMEOUT_MS}ms"))
            .execute(&mut *transaction)
            .await
            .context("failed to set setup structured block read timeout")?;
        let rows = sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(
            setup_structured_blocks_by_revision_sql(),
        ))
        .bind(revision_id)
        .bind(limit_i64(sample_limit))
        .bind(limit_i64(structured_limit))
        .bind(limit_i64(total_limit))
        .fetch_all(&mut *transaction)
        .await
        .context("failed to list bounded setup structured blocks by revision")?;
        transaction
            .commit()
            .await
            .context("failed to finish bounded setup structured block read")?;
        Ok(rows)
    }

    async fn list_structured_blocks_page_by_revision(
        &self,
        revision_id: Uuid,
        offset: usize,
        limit: usize,
    ) -> anyhow::Result<(Vec<KnowledgeStructuredBlockRow>, usize)> {
        let (total,) = sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM knowledge_structured_block WHERE revision_id = $1",
        )
        .bind(revision_id)
        .fetch_one(&self.pool)
        .await
        .context("failed to count structured blocks by revision")?;
        let page = sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM knowledge_structured_block
             WHERE revision_id = $1
             ORDER BY ordinal ASC, block_id ASC
             LIMIT $2 OFFSET $3"
        )))
        .bind(revision_id)
        .bind(limit_i64(limit))
        .bind(limit_i64(offset))
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured block page by revision")?;
        Ok((page, usize::try_from(total).unwrap_or(usize::MAX)))
    }

    async fn list_chunk_support_references_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeChunkSupportReferenceRow>> {
        sqlx::query_as::<_, KnowledgeChunkSupportReferenceRow>(
            "SELECT chunk_id, support_block_ids
             FROM knowledge_chunk
             WHERE revision_id = $1
               AND raptor_level IS NULL
             ORDER BY chunk_index ASC, chunk_id ASC
             LIMIT 2000",
        )
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list chunk support references by revision")
    }

    async fn list_structured_blocks_by_ids(
        &self,
        block_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeStructuredBlockRow>> {
        if block_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeStructuredBlockRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {STRUCTURED_BLOCK_COLUMNS}
             FROM knowledge_structured_block
             WHERE block_id = ANY($1)
             ORDER BY ordinal ASC, block_id ASC"
        )))
        .bind(block_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list structured blocks by ids")
    }

    async fn delete_structured_blocks_by_revision(&self, revision_id: Uuid) -> anyhow::Result<()> {
        sqlx::query("DELETE FROM knowledge_structured_block WHERE revision_id = $1")
            .bind(revision_id)
            .execute(&self.pool)
            .await
            .context("failed to delete structured blocks by revision")?;
        Ok(())
    }

    async fn replace_technical_facts(
        &self,
        revision_id: Uuid,
        rows: &[KnowledgeTechnicalFactRow],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        self.delete_technical_facts_by_revision(revision_id).await?;
        let mut inserted = Vec::with_capacity(rows.len());
        for row in rows {
            inserted.push(self.insert_technical_fact(row).await?);
        }
        Ok(inserted)
    }

    async fn list_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {TECHNICAL_FACT_COLUMNS}
             FROM knowledge_technical_fact
             WHERE revision_id = $1
             ORDER BY fact_kind ASC, fact_id ASC"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list technical facts by revision")
    }

    async fn count_technical_facts_by_revision(&self, revision_id: Uuid) -> anyhow::Result<i64> {
        sqlx::query_as::<_, (i64,)>(
            "SELECT COUNT(*) FROM knowledge_technical_fact WHERE revision_id = $1",
        )
        .bind(revision_id)
        .fetch_one(&self.pool)
        .await
        .map(|(count,)| count)
        .context("failed to count technical facts by revision")
    }

    async fn list_technical_facts_by_ids(
        &self,
        fact_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        if fact_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {TECHNICAL_FACT_COLUMNS}
             FROM knowledge_technical_fact
             WHERE fact_id = ANY($1)
             ORDER BY fact_kind ASC, fact_id ASC"
        )))
        .bind(fact_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list technical facts by ids")
    }

    async fn list_technical_facts_by_chunk_ids(
        &self,
        chunk_ids: &[Uuid],
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        if chunk_ids.is_empty() {
            return Ok(Vec::new());
        }
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {TECHNICAL_FACT_COLUMNS}
             FROM knowledge_technical_fact
             WHERE support_chunk_ids && $1::uuid[]
             ORDER BY fact_kind ASC, fact_id ASC"
        )))
        .bind(chunk_ids)
        .fetch_all(&self.pool)
        .await
        .context("failed to list technical facts by chunk ids")
    }

    async fn list_technical_facts_by_document(
        &self,
        document_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "SELECT {TECHNICAL_FACT_COLUMNS}
             FROM knowledge_technical_fact
             WHERE document_id = $1
             ORDER BY revision_id DESC, fact_kind ASC, fact_id ASC"
        )))
        .bind(document_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to list technical facts by document")
    }

    async fn delete_technical_facts_by_revision(
        &self,
        revision_id: Uuid,
    ) -> anyhow::Result<Vec<KnowledgeTechnicalFactRow>> {
        sqlx::query_as::<_, KnowledgeTechnicalFactRow>(sqlx::AssertSqlSafe(format!(
            "DELETE FROM knowledge_technical_fact
             WHERE revision_id = $1
             RETURNING {TECHNICAL_FACT_COLUMNS}"
        )))
        .bind(revision_id)
        .fetch_all(&self.pool)
        .await
        .context("failed to delete technical facts by revision")
    }
}

fn limit_i64(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn active_chunks_by_library_page_sql() -> String {
    format!(
        "SELECT {QUALIFIED_CHUNK_COLUMNS}
         FROM knowledge_chunk c
         JOIN knowledge_document d
           ON d.document_id = c.document_id
          AND d.library_id = c.library_id
          AND d.readable_revision_id = c.revision_id
          AND d.document_state = 'active'
          AND d.deleted_at IS NULL
         WHERE c.library_id = $1
           AND c.chunk_state = 'ready'
           AND c.raptor_level IS NULL
           AND (
             $2::integer IS NULL
             OR (c.chunk_index, c.chunk_id) > ($2::integer, $3::uuid)
           )
         ORDER BY c.chunk_index ASC, c.chunk_id ASC
         LIMIT $4"
    )
}

fn normalized_search_terms(terms: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    terms
        .iter()
        .filter_map(|term| {
            let term = term.trim().to_lowercase();
            (!term.is_empty() && seen.insert(term.clone())).then_some(term)
        })
        .collect()
}

fn normalized_revision_chunk_terms(terms: &[String]) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut normalized = terms
        .iter()
        .filter_map(|term| {
            let value = term.trim().to_lowercase();
            if value.is_empty() {
                return None;
            }
            let value =
                value.chars().take(KNOWLEDGE_CHUNK_REVISION_TERM_MAX_CHARS).collect::<String>();
            seen.insert(value.clone()).then_some(value)
        })
        .collect::<Vec<_>>();
    normalized.sort_by_key(|term| Reverse(term.chars().count()));
    normalized.truncate(KNOWLEDGE_CHUNK_REVISION_TERM_LIMIT);
    normalized
}

fn normalized_windows(windows: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut windows = windows
        .iter()
        .filter_map(|(min_index, max_index)| {
            (max_index >= min_index).then_some((*min_index, *max_index))
        })
        .collect::<Vec<_>>();
    windows.sort_unstable();

    let mut normalized = Vec::<(i32, i32)>::new();
    for (min_index, max_index) in windows {
        match normalized.last_mut() {
            Some((_, last_max)) if min_index <= last_max.saturating_add(1) => {
                *last_max = (*last_max).max(max_index);
            }
            _ => normalized.push((min_index, max_index)),
        }
    }
    normalized
}

fn normalized_revision_windows(windows: &[(Uuid, i32, i32)]) -> Vec<(Uuid, i32, i32)> {
    windows
        .iter()
        .filter_map(|(revision_id, min_index, max_index)| {
            (max_index >= min_index).then_some((*revision_id, *min_index, *max_index))
        })
        .collect()
}

fn quote_relation_name(relation_name: &str) -> String {
    relation_name.split('.').map(quote_ident).collect::<Vec<_>>().join(".")
}

fn quote_ident(identifier: &str) -> String {
    format!("\"{}\"", identifier.replace('"', "\"\""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_structured_block_candidates_use_indexable_kinds_before_the_limit() {
        let sql = setup_structured_blocks_by_revision_sql();
        let compact = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let structured_filter = compact.find("b.block_kind IN (");
        let structured_limit = compact.find("LIMIT $3");

        assert!(compact.contains("'table', 'table_row', 'code_block', 'source_unit'"));
        assert!(
            compact
                .contains("NOT EXISTS ( SELECT 1 FROM sampled s WHERE s.block_id = b.block_id )")
        );
        assert!(!compact.contains("b.normalized_text"));
        assert!(!compact.contains("LIKE '%"));
        assert!(
            structured_filter
                .is_some_and(|position| structured_limit.is_some_and(|limit| position < limit))
        );
    }

    #[test]
    fn active_chunk_rebuild_page_uses_stable_composite_keyset_order() {
        let sql = active_chunks_by_library_page_sql();
        let compact = sql.split_whitespace().collect::<Vec<_>>().join(" ");

        assert!(compact.contains("$2::integer IS NULL"));
        assert!(compact.contains("(c.chunk_index, c.chunk_id) > ($2::integer, $3::uuid)"));
        assert!(compact.contains("ORDER BY c.chunk_index ASC, c.chunk_id ASC LIMIT $4"));
        assert!(compact.contains("c.chunk_state = 'ready'"));
        assert!(compact.contains("c.raptor_level IS NULL"));
    }

    fn assert_answer_driving_library_search_is_canonical(sql: &str, limit_clause: &str) {
        assert!(sql.contains("from knowledge_chunk c"));
        assert!(sql.contains("join knowledge_document d"));
        assert!(sql.contains("d.document_id = c.document_id"));
        assert!(sql.contains("d.library_id = c.library_id"));
        assert!(sql.contains("d.readable_revision_id = c.revision_id"));
        assert!(sql.contains("d.document_state = 'active'"));
        assert!(sql.contains("d.deleted_at is null"));
        assert!(sql.contains("c.chunk_state = 'ready'"));
        assert!(sql.contains("c.raptor_level is null"));
        assert!(sql.contains("c.chunk_kind is distinct from 'source_profile'"));
        assert!(
            sql.find("d.deleted_at is null").unwrap() < sql.rfind(limit_clause).unwrap(),
            "canonical document filters must run before the bounded pattern-search result",
        );
    }

    #[test]
    fn code_pattern_search_filters_noncanonical_chunks_before_limit() {
        assert_answer_driving_library_search_is_canonical(
            &code_pattern_chunk_search_sql(),
            "limit $8",
        );
    }

    #[test]
    fn code_pattern_search_is_scoped_to_candidate_documents() {
        let sql = code_pattern_chunk_search_sql();

        assert!(sql.starts_with("select c.chunk_id, c.workspace_id, c.library_id"));
        assert!(sql.contains("c.document_id = any($2::uuid[])"));
        assert!(sql.contains("unnest($3::text[])"));
        assert!(sql.contains("matches.matched_count >= $4"));
        assert!(sql.contains("limit $8"));
    }

    #[test]
    fn transport_pattern_search_filters_noncanonical_chunks_before_limit() {
        let sql = transport_pattern_chunk_search_sql();
        assert!(sql.starts_with("select c.chunk_id, c.workspace_id, c.library_id"));
        assert_answer_driving_library_search_is_canonical(&sql, "limit $4");
    }

    #[test]
    fn transport_pattern_search_is_scoped_to_candidate_documents() {
        let sql = transport_pattern_chunk_search_sql();

        assert!(sql.contains("c.document_id = any($2::uuid[])"));
        assert!(sql.contains("unnest($3::text[])"));
        assert!(sql.contains("limit $4"));
    }

    #[test]
    fn multi_revision_term_search_bounds_hydrated_rows_after_diverse_ranking() {
        let sql = multi_revision_matching_terms_sql(false);
        let compact_sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let bounded_start = sql.find("bounded as (").unwrap_or(sql.len());
        let global_limit = sql.find("limit $4").unwrap_or(sql.len());
        let hydration_join = sql.rfind("join knowledge_chunk c").unwrap_or(sql.len());

        assert!(compact_sql.contains("order by revision_rank asc, match_score desc, ordinal asc"));
        assert!(bounded_start < global_limit);
        assert!(global_limit < hydration_join);
        assert!(compact_sql.contains("order by bounded.ordinal asc"));
    }

    #[test]
    fn broad_multi_revision_term_search_prefilters_with_gin_then_exact_scores() {
        let sql = multi_revision_matching_terms_sql(true);
        let compact_sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let indexed_pool =
            compact_sql.find("indexed_candidates as materialized").unwrap_or(usize::MAX);
        let indexed_limit = compact_sql.find("limit $6").unwrap_or(usize::MAX);
        let exact_rescore =
            compact_sql.find("lower(c.normalized_text) as normalized_lower").unwrap_or(usize::MAX);

        assert!(compact_sql.contains("c.search_tsv @@ to_tsquery('simple', ironrag_unaccent($5))"));
        assert!(compact_sql.contains("cross join lateral"));
        assert!(compact_sql.contains("c.revision_id = requested_pool.revision_id"));
        assert!(compact_sql.contains("limit $7"));
        assert!(!compact_sql.contains("order by ts_rank_cd"));
        assert!(indexed_pool < indexed_limit);
        assert!(indexed_limit < exact_rescore);
        assert!(compact_sql.contains("and matches.matched_count > 0"));
        assert!(
            compact_sql
                .contains("(matches.matched_count * 10000 - matches.earliest_pos) as match_score")
        );
    }

    #[test]
    fn small_or_unindexable_multi_revision_requests_keep_exhaustive_semantics() {
        assert!(!should_prefilter_multi_revision_matching_terms(3, 2, true));
        assert!(!should_prefilter_multi_revision_matching_terms(10_000, 2, false));
        assert!(should_prefilter_multi_revision_matching_terms(10_000, 2, true));
        assert!(!multi_revision_matching_terms_sql(false).contains("search_tsv"));
    }

    #[test]
    fn multi_revision_term_prefix_query_is_bounded_and_operator_safe() {
        let terms = ["Orbiting orbital".to_string(), "phase') | !('node".to_string()];
        let query = multi_revision_matching_terms_tsquery(&terms).unwrap_or_default();

        assert_eq!(query, "'orbit':* | 'phase':* | 'node':*");
        assert!(!query.contains('!'));
        assert_eq!(query.matches(":*").count(), 3);

        let many_terms = (0..100).map(|index| format!("p{index:04}suffix")).collect::<Vec<_>>();
        let bounded = multi_revision_matching_terms_tsquery(&many_terms).unwrap_or_default();
        assert_eq!(bounded.matches(":*").count(), KNOWLEDGE_CHUNK_MULTI_REVISION_TSQUERY_TERM_CAP);
    }

    #[test]
    fn mixed_indexable_and_short_terms_keep_exhaustive_semantics() {
        let terms = ["SCS".to_string(), "configuration".to_string()];

        assert!(multi_revision_matching_terms_tsquery(&terms).is_none());
    }

    #[test]
    fn multi_revision_term_prefix_query_keeps_morphological_surface_recall() {
        let query = multi_revision_matching_terms_tsquery(&["configuration".to_string()])
            .unwrap_or_default();

        assert_eq!(query, "'confi':*");
        assert!("configure".starts_with("confi"));
        assert!("configuration".starts_with("confi"));
    }

    #[test]
    fn multi_revision_term_search_candidate_cap_preserves_small_requests() {
        assert_eq!(multi_revision_matching_terms_candidate_cap(3, 2), 6);
        assert_eq!(multi_revision_matching_terms_candidate_cap(1, 64), 64);
    }

    #[test]
    fn multi_revision_term_search_candidate_cap_bounds_broad_requests() {
        assert_eq!(
            multi_revision_matching_terms_candidate_cap(usize::MAX, usize::MAX),
            KNOWLEDGE_CHUNK_MULTI_REVISION_MATCH_CANDIDATE_CAP
        );
        assert_eq!(
            multi_revision_matching_terms_candidate_cap(10_000, 2),
            KNOWLEDGE_CHUNK_MULTI_REVISION_MATCH_CANDIDATE_CAP
        );
    }

    #[test]
    fn multi_revision_index_budget_preserves_revision_diversity_within_cap() {
        assert_eq!(multi_revision_matching_terms_index_budgets(1_000, 2, 2_048), (2, 1_024));
        assert_eq!(multi_revision_matching_terms_index_budgets(100, 2, 2_048), (20, 102));
    }

    #[test]
    fn revision_windows_drop_invalid_ranges_without_reordering_valid_requests() {
        let revision_a = Uuid::from_u128(1);
        let revision_b = Uuid::from_u128(2);

        let windows = normalized_revision_windows(&[
            (revision_a, 0, 2),
            (revision_b, 4, 3),
            (revision_b, 5, 6),
        ]);

        assert_eq!(windows, vec![(revision_a, 0, 2), (revision_b, 5, 6)]);
    }
}
