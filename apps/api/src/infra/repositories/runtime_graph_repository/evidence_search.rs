use std::collections::BTreeSet;

use sqlx::{Executor, PgPool, Postgres};
use uuid::Uuid;

use crate::shared::text_tokens::normalized_alnum_token_sequence_by;

use super::types::{RuntimeGraphEvidenceRow, RuntimeGraphEvidenceSearchQuery};

pub(super) const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP: usize = 6;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_QUERY_CAP: usize = 8;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_CAP: usize = 16;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS: usize = 4;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_MIN_TOTAL_CHARS: usize = 11;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MIN_TOKENS: usize = 2;
const RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS: usize = 4;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_TOKENS: usize = 20;
const RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_CHARS: usize = 220;
const RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_DOCUMENT_CAP: usize = 16;
const RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_ROW_CAP: i64 = 40_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RuntimeGraphEvidenceSearchShape {
    Unscoped,
    ScopedMaterializedWithText,
    ScopedMaterializedLiteralOnly,
    ScopedPostFiltered,
}

pub(super) fn runtime_graph_evidence_search_shape(
    document_id_count: usize,
    has_text_queries: bool,
    scoped_row_count: i64,
) -> RuntimeGraphEvidenceSearchShape {
    if document_id_count == 0 {
        RuntimeGraphEvidenceSearchShape::Unscoped
    } else if document_id_count > RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_DOCUMENT_CAP
        || scoped_row_count > RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_ROW_CAP
    {
        RuntimeGraphEvidenceSearchShape::ScopedPostFiltered
    } else if has_text_queries {
        RuntimeGraphEvidenceSearchShape::ScopedMaterializedWithText
    } else {
        RuntimeGraphEvidenceSearchShape::ScopedMaterializedLiteralOnly
    }
}

async fn runtime_graph_evidence_scoped_row_count(
    pool: &PgPool,
    library_id: Uuid,
    document_ids: &[Uuid],
) -> Result<i64, sqlx::Error> {
    // The `library_id`/`document_id` scope predicate mirrors
    // `runtime_graph_evidence_document_filter` and must stay in sync with it.
    sqlx::query_scalar::<_, i64>(
        "select count(*) from runtime_graph_evidence
         where library_id = $1 and document_id = any($2::uuid[])",
    )
    .bind(library_id)
    .bind(document_ids)
    .fetch_one(pool)
    .await
}

/// Builds the document-scope clause, or `None` when the caller scoped no
/// documents, so the clause and the `$6` bind cannot drift apart.
///
/// The clause is wrapped in `is true` deliberately: on its own,
/// `document_id = any($6)` is an index condition for
/// `idx_runtime_graph_evidence_library_document`, and the match is then
/// filtered per fetched evidence row instead of served by the GIN index
/// (worst measured call 110.65 s; 20 calls, 951.1 s total; IRG-128). `is true`
/// keeps the match on `idx_runtime_graph_evidence_library_body_text_search` /
/// `idx_runtime_graph_evidence_library_body_literal_trgm` in custom and generic
/// plans and selects the same rows.
pub(super) fn runtime_graph_evidence_document_filter(
    document_ids: &[Uuid],
) -> Option<&'static str> {
    (!document_ids.is_empty()).then_some(" and (evidence.document_id = any($6::uuid[])) is true")
}

pub async fn list_runtime_graph_evidence_by_target_with_executor<'e, E>(
    executor: E,
    library_id: Uuid,
    target_kind: &str,
    target_id: Uuid,
) -> Result<Vec<RuntimeGraphEvidenceRow>, sqlx::Error>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "select id, library_id, target_kind, target_id, document_id, chunk_id, source_file_name,
            page_ref, evidence_text, confidence_score, created_at
         from runtime_graph_evidence
         where library_id = $1 and target_kind = $2 and target_id = $3
         order by created_at desc, id desc",
    )
    .bind(library_id)
    .bind(target_kind)
    .bind(target_id)
    .fetch_all(executor)
    .await
}

/// Lists runtime graph evidence for an ordered set of node / edge targets.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn list_runtime_graph_evidence_by_targets(
    pool: &PgPool,
    library_id: Uuid,
    targets: &[(String, Uuid)],
    limit: i64,
) -> Result<Vec<RuntimeGraphEvidenceRow>, sqlx::Error> {
    if targets.is_empty() || limit <= 0 {
        return Ok(Vec::new());
    }
    let target_kinds = targets.iter().map(|(kind, _)| kind.clone()).collect::<Vec<_>>();
    let target_ids = targets.iter().map(|(_, id)| *id).collect::<Vec<_>>();
    let per_target_limit = (limit as usize).div_ceil(targets.len()).max(1) as i64;

    sqlx::query_as::<_, RuntimeGraphEvidenceRow>(
        "with requested(target_kind, target_id, ordinal) as (
             select target_kind, target_id, ordinal::integer
             from unnest($2::text[], $3::uuid[]) with ordinality
                as request(target_kind, target_id, ordinal)
         ),
         ranked_evidence as (
             select
                evidence.id,
                evidence.library_id,
                evidence.target_kind,
                evidence.target_id,
                evidence.document_id,
                evidence.chunk_id,
                evidence.source_file_name,
                evidence.page_ref,
                evidence.evidence_text,
                evidence.confidence_score,
                evidence.created_at,
                requested.ordinal,
                row_number() over (
                    partition by requested.ordinal
                    order by evidence.created_at desc, evidence.id desc
                ) as target_rank
             from requested
             join runtime_graph_evidence as evidence
               on evidence.library_id = $1
              and evidence.target_kind = requested.target_kind
              and evidence.target_id = requested.target_id
         )
         select id, library_id, target_kind, target_id,
            evidence.document_id, evidence.chunk_id, evidence.source_file_name,
            page_ref, evidence_text, confidence_score, created_at
         from ranked_evidence as evidence
         where target_rank <= $4
         order by ordinal asc, target_rank asc, created_at desc, id desc",
    )
    .bind(library_id)
    .bind(target_kinds)
    .bind(target_ids)
    .bind(per_target_limit)
    .fetch_all(pool)
    .await
}

const RUNTIME_GRAPH_EVIDENCE_SCOPED_LITERAL_LANE: &str = "from scoped as evidence\n                 where lower(evidence.evidence_text) like requested_literal_query.literal_pattern escape '~'";

const RUNTIME_GRAPH_EVIDENCE_SCOPED_TSV_TEXT_LANE: &str =
    "from scoped as evidence\n                 where evidence.tsv @@ requested_text_query.ts_query";

const RUNTIME_GRAPH_EVIDENCE_SCOPED_LITERAL_ONLY_TEXT_LANE: &str = "from scoped as evidence\n                 where to_tsvector('simple'::regconfig, evidence.evidence_text) @@ requested_text_query.ts_query";

// The `library_id`/`document_id` scope predicate mirrors
// `runtime_graph_evidence_document_filter` and must stay in sync with it.
fn runtime_graph_evidence_scoped_cte(with_text_search: bool) -> String {
    let tsv_column = if with_text_search {
        ",\n                to_tsvector('simple'::regconfig, evidence.evidence_text) as tsv"
    } else {
        ""
    };
    format!(
        "scoped as materialized (
             select
                evidence.id,
                evidence.library_id,
                evidence.target_kind,
                evidence.target_id,
                evidence.document_id,
                evidence.chunk_id,
                evidence.source_file_name,
                evidence.page_ref,
                evidence.evidence_text,
                evidence.confidence_score,
                evidence.created_at{tsv_column}
             from runtime_graph_evidence as evidence
             where evidence.library_id = $1
               and btrim(evidence.evidence_text) <> ''
               and evidence.document_id = any($6::uuid[])
         ),"
    )
}

pub(super) fn runtime_graph_evidence_search_fragments(
    shape: RuntimeGraphEvidenceSearchShape,
    document_filter: &str,
) -> (String, String, String) {
    match shape {
        RuntimeGraphEvidenceSearchShape::Unscoped
        | RuntimeGraphEvidenceSearchShape::ScopedPostFiltered => (
            String::new(),
            format!(
                "from runtime_graph_evidence as evidence\n                 where evidence.library_id = $1\n                   and btrim(evidence.evidence_text) <> ''\n                   {document_filter}\n                   and to_tsvector('simple'::regconfig, evidence.evidence_text)\n                       @@ requested_text_query.ts_query"
            ),
            format!(
                "from runtime_graph_evidence as evidence\n                 where evidence.library_id = $1\n                   and btrim(evidence.evidence_text) <> ''\n                   {document_filter}\n                   and lower(evidence.evidence_text) like requested_literal_query.literal_pattern escape '~'"
            ),
        ),
        RuntimeGraphEvidenceSearchShape::ScopedMaterializedWithText => (
            runtime_graph_evidence_scoped_cte(true),
            RUNTIME_GRAPH_EVIDENCE_SCOPED_TSV_TEXT_LANE.to_string(),
            RUNTIME_GRAPH_EVIDENCE_SCOPED_LITERAL_LANE.to_string(),
        ),
        RuntimeGraphEvidenceSearchShape::ScopedMaterializedLiteralOnly => (
            runtime_graph_evidence_scoped_cte(false),
            RUNTIME_GRAPH_EVIDENCE_SCOPED_LITERAL_ONLY_TEXT_LANE.to_string(),
            RUNTIME_GRAPH_EVIDENCE_SCOPED_LITERAL_LANE.to_string(),
        ),
    }
}

fn runtime_graph_evidence_text_search_sql(
    scoped_cte: &str,
    text_lane: &str,
    literal_lane: &str,
) -> String {
    format!(
        "with requested_text(search_query, ordinal) as (
             select search_query, ordinal::integer
             from unnest($2::text[]) with ordinality as request(search_query, ordinal)
         ),
         requested_text_query as (
             select
                search_query,
                ordinal,
                to_tsquery('simple', search_query) as ts_query
             from requested_text
         ),
         requested_literal(literal_query, ordinal) as (
             select literal_query, ordinal::integer
             from unnest($3::text[]) with ordinality as request(literal_query, ordinal)
         ),
         requested_literal_query as (
             select
                literal_query,
                ordinal,
                '%' || replace(
                    replace(replace(literal_query, '~', '~~'), '%', '~%'),
                    '_',
                    '~_'
                ) || '%' as literal_pattern
             from requested_literal
         ),
         {scoped_cte}
         text_matched as (
             select
                evidence.id,
                evidence.library_id,
                evidence.target_kind,
                evidence.target_id,
                evidence.document_id,
                evidence.chunk_id,
                evidence.source_file_name,
                evidence.page_ref,
                evidence.evidence_text,
                evidence.confidence_score,
                evidence.created_at,
                evidence.body_key,
                evidence.first_query_ordinal,
                evidence.body_match,
                evidence.literal_match
             from requested_text_query
             cross join lateral (
                 select
                    evidence.id,
                    evidence.library_id,
                    evidence.target_kind,
                    evidence.target_id,
                    evidence.document_id,
                    evidence.chunk_id,
                    evidence.source_file_name,
                    evidence.page_ref,
                    evidence.evidence_text,
                    evidence.confidence_score,
                    evidence.created_at,
                    md5(lower(regexp_replace(btrim(evidence.evidence_text), '[[:space:]]+', ' ', 'g'))) as body_key,
                    requested_text_query.ordinal as first_query_ordinal,
                    true as body_match,
                    false as literal_match
                 {text_lane}
                 order by
                    evidence.confidence_score desc nulls last,
                    evidence.created_at desc,
                    evidence.id desc
                 limit $5
             ) as evidence
         ),
         literal_matched as (
             select
                evidence.id,
                evidence.library_id,
                evidence.target_kind,
                evidence.target_id,
                evidence.document_id,
                evidence.chunk_id,
                evidence.source_file_name,
                evidence.page_ref,
                evidence.evidence_text,
                evidence.confidence_score,
                evidence.created_at,
                evidence.body_key,
                evidence.first_query_ordinal,
                evidence.body_match,
                evidence.literal_match
             from requested_literal_query
             cross join lateral (
                 select
                    evidence.id,
                    evidence.library_id,
                    evidence.target_kind,
                    evidence.target_id,
                    evidence.document_id,
                    evidence.chunk_id,
                    evidence.source_file_name,
                    evidence.page_ref,
                    evidence.evidence_text,
                    evidence.confidence_score,
                    evidence.created_at,
                    md5(lower(regexp_replace(btrim(evidence.evidence_text), '[[:space:]]+', ' ', 'g'))) as body_key,
                    requested_literal_query.ordinal as first_query_ordinal,
                    false as body_match,
                    true as literal_match
                 {literal_lane}
                 order by
                    evidence.confidence_score desc nulls last,
                    evidence.created_at desc,
                    evidence.id desc
                 limit $5
             ) as evidence
         ),
         matched as (
             select distinct on (evidence.id)
                evidence.id,
                evidence.library_id,
                evidence.target_kind,
                evidence.target_id,
                evidence.document_id,
                evidence.chunk_id,
                evidence.source_file_name,
                evidence.page_ref,
                evidence.evidence_text,
                evidence.confidence_score,
                evidence.created_at,
                evidence.body_key,
                evidence.first_query_ordinal,
                evidence.body_match,
                evidence.literal_match
             from (
                 select * from text_matched
                 union all
                 select * from literal_matched
             ) as evidence
             order by
                evidence.id,
                evidence.first_query_ordinal asc,
                evidence.literal_match desc,
                evidence.body_match desc
         ),
         deduped as (
             select distinct on (body_key)
                id,
                library_id,
                target_kind,
                target_id,
                document_id,
                chunk_id,
                source_file_name,
                page_ref,
                evidence_text,
                confidence_score,
                created_at,
                first_query_ordinal,
                body_match,
                literal_match
             from matched
             order by
                body_key,
                first_query_ordinal asc,
                literal_match desc,
                body_match desc,
                confidence_score desc nulls last,
                created_at desc,
                id desc
         )
         select
            id,
            library_id,
            target_kind,
            target_id,
            document_id,
            chunk_id,
            source_file_name,
            page_ref,
            evidence_text,
            confidence_score,
            created_at
         from deduped
         order by
            first_query_ordinal asc,
            literal_match desc,
            body_match desc,
            confidence_score desc nulls last,
            created_at desc,
            id desc
         limit $4"
    )
}

/// Searches runtime graph evidence bodies using the same active evidence table
/// that powers graph support. This complements target-based evidence lookup for
/// rare facts whose text is more discriminative than their node label.
///
/// # Errors
/// Returns any `SQLx` error raised while querying the evidence rows.
pub async fn search_runtime_graph_evidence_by_text(
    pool: &PgPool,
    library_id: Uuid,
    query_texts: &[RuntimeGraphEvidenceSearchQuery],
    document_ids: &[Uuid],
    limit: i64,
) -> Result<Vec<RuntimeGraphEvidenceRow>, sqlx::Error> {
    if query_texts.is_empty() || limit <= 0 {
        return Ok(Vec::new());
    }
    let search_queries = runtime_graph_evidence_text_search_queries(query_texts);
    let literal_queries = runtime_graph_evidence_literal_search_queries(query_texts);
    if search_queries.is_empty() && literal_queries.is_empty() {
        return Ok(Vec::new());
    }
    let per_query_candidate_limit = runtime_graph_evidence_per_query_candidate_limit(
        limit,
        search_queries.len().saturating_add(literal_queries.len()),
    );
    let document_scope = runtime_graph_evidence_document_filter(document_ids);
    let document_filter = document_scope.unwrap_or_default();
    let shape = if document_ids.is_empty() {
        RuntimeGraphEvidenceSearchShape::Unscoped
    } else if document_ids.len() > RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_DOCUMENT_CAP {
        RuntimeGraphEvidenceSearchShape::ScopedPostFiltered
    } else {
        let scoped_row_count =
            runtime_graph_evidence_scoped_row_count(pool, library_id, document_ids).await?;
        runtime_graph_evidence_search_shape(
            document_ids.len(),
            !search_queries.is_empty(),
            scoped_row_count,
        )
    };
    let (scoped_cte, text_lane, literal_lane) =
        runtime_graph_evidence_search_fragments(shape, document_filter);

    let sql = runtime_graph_evidence_text_search_sql(&scoped_cte, &text_lane, &literal_lane);

    let mut query = sqlx::query_as::<_, RuntimeGraphEvidenceRow>(sqlx::AssertSqlSafe(&*sql))
        .bind(library_id)
        .bind(search_queries)
        .bind(literal_queries)
        .bind(limit)
        .bind(per_query_candidate_limit);
    if document_scope.is_some() {
        query = query.bind(document_ids);
    }
    query.fetch_all(pool).await
}

pub(super) fn runtime_graph_evidence_per_query_candidate_limit(
    limit: i64,
    query_count: usize,
) -> i64 {
    if limit <= 0 || query_count == 0 {
        return 0;
    }
    if query_count <= 1 {
        return limit;
    }
    limit.min(24)
}

pub(super) fn runtime_graph_evidence_text_search_queries(
    query_texts: &[RuntimeGraphEvidenceSearchQuery],
) -> Vec<String> {
    let mut seen_queries = BTreeSet::new();
    let mut token_windows_by_query = Vec::new();
    for query in query_texts {
        let query_text = query.text();
        let tokens = runtime_graph_evidence_text_search_tokens(query_text);
        if !runtime_graph_evidence_text_search_tokens_are_selective(&tokens) {
            continue;
        }
        token_windows_by_query.push(runtime_graph_evidence_text_search_token_windows(&tokens));
    }

    let mut search_queries = Vec::new();
    let mut window_index = 0usize;
    loop {
        let mut saw_window = false;
        for token_windows in &token_windows_by_query {
            let Some(token_window) = token_windows.get(window_index) else {
                continue;
            };
            saw_window = true;
            let search_query = runtime_graph_evidence_text_search_query(token_window);
            if seen_queries.insert(search_query.clone()) {
                search_queries.push(search_query);
                if search_queries.len() >= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP {
                    return search_queries;
                }
            }
        }
        if !saw_window {
            break;
        }
        window_index += 1;
    }
    search_queries
}

pub(super) fn runtime_graph_evidence_literal_search_queries(
    query_texts: &[RuntimeGraphEvidenceSearchQuery],
) -> Vec<String> {
    let mut seen_queries = BTreeSet::new();
    let mut queries = Vec::new();
    for query_text in
        query_texts.iter().filter_map(RuntimeGraphEvidenceSearchQuery::literal_or_formal_text)
    {
        let normalized = query_text.split_whitespace().collect::<Vec<_>>().join(" ");
        if !runtime_graph_evidence_literal_search_query_is_bounded(&normalized) {
            continue;
        }
        let query = normalized.to_lowercase();
        if seen_queries.insert(query.clone()) {
            queries.push(query);
            if queries.len() >= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_QUERY_CAP {
                break;
            }
        }
    }
    queries
}

fn runtime_graph_evidence_literal_search_query_is_bounded(query_text: &str) -> bool {
    let alphanumeric_count = query_text.chars().filter(|ch| ch.is_alphanumeric()).count();
    if alphanumeric_count < 4 {
        return false;
    }
    let char_count = query_text.chars().count();
    let tokens = normalized_alnum_token_sequence_by(
        query_text,
        |token| !token.trim().is_empty(),
        Some(RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_TOKENS + 1),
    );
    !tokens.is_empty()
        && tokens.len() <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_TOKENS
        && char_count <= RUNTIME_GRAPH_EVIDENCE_LITERAL_SEARCH_MAX_CHARS
}

fn runtime_graph_evidence_text_search_query(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| {
            let prefix = runtime_graph_evidence_text_search_token_prefix(token);
            if runtime_graph_evidence_text_search_token_has_numeric(token) {
                format!("'{prefix}'")
            } else {
                format!("'{prefix}':*")
            }
        })
        .collect::<Vec<_>>()
        .join(" & ")
}

pub(super) fn runtime_graph_evidence_text_search_token_prefix(token: &str) -> String {
    if runtime_graph_evidence_text_search_token_has_numeric(token) {
        return token.to_string();
    }
    let char_count = token.chars().count();
    if char_count <= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS {
        return token.to_string();
    }
    let suffix_budget = if char_count >= 7 { 2 } else { 1 };
    let prefix_len = char_count
        .saturating_sub(suffix_budget)
        .max(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS);
    token.chars().take(prefix_len).collect()
}

fn runtime_graph_evidence_text_search_token_windows(tokens: &[String]) -> Vec<Vec<String>> {
    let mut candidates = Vec::new();
    if tokens.len() <= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS {
        let full_window = tokens.to_vec();
        candidates.push((
            usize::MAX,
            0,
            full_window.clone(),
            runtime_graph_evidence_text_search_window_query(&full_window),
        ));
    } else if let Some(distinctive_window) =
        runtime_graph_evidence_text_search_distinctive_window(tokens)
    {
        candidates.push((
            usize::MAX,
            0,
            distinctive_window.clone(),
            runtime_graph_evidence_text_search_window_query(&distinctive_window),
        ));
    }

    for width in (RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MIN_TOKENS
        ..=RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS)
        .rev()
    {
        if width > tokens.len() {
            continue;
        }
        for start in 0..=tokens.len().saturating_sub(width) {
            let window = tokens[start..start + width].to_vec();
            if !runtime_graph_evidence_text_search_tokens_are_selective(&window) {
                continue;
            }
            let query = runtime_graph_evidence_text_search_window_query(&window);
            candidates.push((
                runtime_graph_evidence_text_search_window_score(&window),
                start,
                window,
                query,
            ));
        }
    }

    candidates.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));

    let mut seen_queries = BTreeSet::new();
    candidates
        .into_iter()
        .filter_map(|(_, _, window, query)| seen_queries.insert(query).then_some(window))
        .collect()
}

fn runtime_graph_evidence_text_search_distinctive_window(tokens: &[String]) -> Option<Vec<String>> {
    let mut indexed_tokens = tokens.iter().enumerate().collect::<Vec<_>>();
    indexed_tokens.sort_by(|left, right| {
        runtime_graph_evidence_text_search_token_score(right.1)
            .cmp(&runtime_graph_evidence_text_search_token_score(left.1))
            .then_with(|| left.0.cmp(&right.0))
    });
    let mut selected = indexed_tokens
        .into_iter()
        .take(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_WINDOW_MAX_TOKENS)
        .collect::<Vec<_>>();
    selected.sort_by_key(|left| left.0);
    let window = selected.into_iter().map(|(_, token)| token.clone()).collect::<Vec<_>>();
    runtime_graph_evidence_text_search_tokens_are_selective(&window).then_some(window)
}

fn runtime_graph_evidence_text_search_window_query(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| runtime_graph_evidence_text_search_token_prefix(token))
        .collect::<Vec<_>>()
        .join("\u{0}")
}

fn runtime_graph_evidence_text_search_window_score(tokens: &[String]) -> usize {
    let token_score = tokens
        .iter()
        .map(|token| runtime_graph_evidence_text_search_token_score(token))
        .sum::<usize>();
    let width_score =
        tokens.len().saturating_mul(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS);
    token_score.saturating_add(width_score)
}

fn runtime_graph_evidence_text_search_token_score(token: &str) -> usize {
    let numeric_bonus = usize::from(runtime_graph_evidence_text_search_token_has_numeric(token))
        * RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_MIN_TOTAL_CHARS;
    token.chars().count().saturating_add(numeric_bonus)
}

pub(super) fn runtime_graph_evidence_text_search_tokens(query_text: &str) -> Vec<String> {
    normalized_alnum_token_sequence_by(
        query_text,
        |token| {
            runtime_graph_evidence_text_search_token_has_numeric(token)
                || token.chars().count() >= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_MIN_CHARS
        },
        Some(RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_TOKEN_CAP),
    )
}

fn runtime_graph_evidence_text_search_tokens_are_selective(tokens: &[String]) -> bool {
    if tokens.len() < 2 {
        return false;
    }
    if tokens.len() == 2 {
        return tokens
            .iter()
            .any(|token| runtime_graph_evidence_text_search_token_has_numeric(token));
    }
    if tokens.len() >= 3 {
        return true;
    }
    false
}

fn runtime_graph_evidence_text_search_token_has_numeric(token: &str) -> bool {
    token.chars().any(char::is_numeric)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_shape_covers_unscoped_scoped_and_oversized_scope() {
        assert_eq!(
            runtime_graph_evidence_search_shape(0, true, 0),
            RuntimeGraphEvidenceSearchShape::Unscoped
        );
        assert_eq!(
            runtime_graph_evidence_search_shape(
                RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_DOCUMENT_CAP,
                true,
                0
            ),
            RuntimeGraphEvidenceSearchShape::ScopedMaterializedWithText
        );
        assert_eq!(
            runtime_graph_evidence_search_shape(
                RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_DOCUMENT_CAP + 1,
                true,
                0
            ),
            RuntimeGraphEvidenceSearchShape::ScopedPostFiltered
        );
        assert_eq!(
            runtime_graph_evidence_search_shape(1, true, 0),
            RuntimeGraphEvidenceSearchShape::ScopedMaterializedWithText
        );
        assert_eq!(
            runtime_graph_evidence_search_shape(
                12,
                true,
                RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_ROW_CAP + 1
            ),
            RuntimeGraphEvidenceSearchShape::ScopedPostFiltered
        );
        assert_eq!(
            runtime_graph_evidence_search_shape(12, false, 0),
            RuntimeGraphEvidenceSearchShape::ScopedMaterializedLiteralOnly
        );
        assert_eq!(
            runtime_graph_evidence_search_shape(0, true, i64::MAX),
            RuntimeGraphEvidenceSearchShape::Unscoped
        );
        assert_eq!(
            runtime_graph_evidence_search_shape(
                12,
                true,
                RUNTIME_GRAPH_EVIDENCE_SCOPED_MATERIALIZATION_ROW_CAP
            ),
            RuntimeGraphEvidenceSearchShape::ScopedMaterializedWithText
        );
    }

    #[test]
    fn scoped_materialized_sql_bounds_the_document_set_once_with_the_tsv_column() {
        let (scoped_cte, text_lane, literal_lane) = runtime_graph_evidence_search_fragments(
            RuntimeGraphEvidenceSearchShape::ScopedMaterializedWithText,
            "",
        );
        assert!(scoped_cte.contains("scoped as materialized"));
        assert!(scoped_cte.contains("evidence.document_id = any($6::uuid[])"));
        assert!(scoped_cte.contains("as tsv"));
        assert!(scoped_cte.contains(
            "evidence.created_at,\n                to_tsvector('simple'::regconfig, evidence.evidence_text) as tsv"
        ));
        assert!(!scoped_cte.contains("is true"));
        assert_eq!(scoped_cte.matches("$6").count(), 1);
        assert_eq!(text_lane, RUNTIME_GRAPH_EVIDENCE_SCOPED_TSV_TEXT_LANE);
        assert_eq!(literal_lane, RUNTIME_GRAPH_EVIDENCE_SCOPED_LITERAL_LANE);
    }

    #[test]
    fn literal_only_scoped_sql_omits_the_tsv_materialization() {
        let (scoped_cte, text_lane, literal_lane) = runtime_graph_evidence_search_fragments(
            RuntimeGraphEvidenceSearchShape::ScopedMaterializedLiteralOnly,
            "",
        );
        assert!(scoped_cte.contains("scoped as materialized"));
        assert!(!scoped_cte.contains("as tsv"));
        assert_eq!(text_lane, RUNTIME_GRAPH_EVIDENCE_SCOPED_LITERAL_ONLY_TEXT_LANE);
        assert_eq!(literal_lane, RUNTIME_GRAPH_EVIDENCE_SCOPED_LITERAL_LANE);
    }

    #[test]
    fn unscoped_sql_keeps_the_gin_lane_predicates() {
        let (scoped_cte, text_lane, literal_lane) =
            runtime_graph_evidence_search_fragments(RuntimeGraphEvidenceSearchShape::Unscoped, "");
        assert!(scoped_cte.is_empty());
        assert!(text_lane.contains("to_tsvector('simple'::regconfig, evidence.evidence_text)"));
        assert!(text_lane.contains("requested_text_query.ts_query"));
        assert!(literal_lane.contains(
            "lower(evidence.evidence_text) like requested_literal_query.literal_pattern"
        ));
        assert!(!text_lane.contains("is true"));
    }

    #[test]
    fn oversized_scope_keeps_the_post_filtered_gin_lanes() {
        let filter =
            runtime_graph_evidence_document_filter(&[Uuid::nil()]).expect("document scope clause");
        let (scoped_cte, text_lane, _literal_lane) = runtime_graph_evidence_search_fragments(
            RuntimeGraphEvidenceSearchShape::ScopedPostFiltered,
            filter,
        );
        assert!(scoped_cte.is_empty());
        assert!(text_lane.contains("is true"));
    }

    /// One rendered evidence statement together with the bind-placeholder
    /// counts and fragments its shape must (or must not) produce.
    struct RenderedShapeCase<'a> {
        shape: RuntimeGraphEvidenceSearchShape,
        filter: &'a str,
        bind_counts: &'a [(&'a str, usize)],
        required: &'a [&'a str],
        forbidden: &'a [&'a str],
    }

    fn rendered_evidence_statement(shape: RuntimeGraphEvidenceSearchShape, filter: &str) -> String {
        let (scoped_cte, text_lane, literal_lane) =
            runtime_graph_evidence_search_fragments(shape, filter);
        runtime_graph_evidence_text_search_sql(&scoped_cte, &text_lane, &literal_lane)
    }

    fn assert_rendered_evidence_statement(case: RenderedShapeCase<'_>) {
        let sql = rendered_evidence_statement(case.shape, case.filter);
        for (placeholder, count) in case.bind_counts {
            assert_eq!(
                sql.matches(*placeholder).count(),
                *count,
                "bind {placeholder} occurrences in rendered evidence statement",
            );
        }
        for fragment in case.required {
            assert!(sql.contains(*fragment), "rendered evidence statement is missing {fragment:?}");
        }
        for fragment in case.forbidden {
            assert!(
                !sql.contains(*fragment),
                "rendered evidence statement must not contain {fragment:?}",
            );
        }
    }

    #[test]
    fn rendered_statement_matches_each_shape_bind_contract() {
        let canonical_filter =
            runtime_graph_evidence_document_filter(&[Uuid::nil()]).expect("document scope clause");
        let cases = [
            RenderedShapeCase {
                shape: RuntimeGraphEvidenceSearchShape::Unscoped,
                filter: "",
                bind_counts: &[("$1", 2), ("$2", 1), ("$3", 1), ("$4", 1), ("$5", 2), ("$6", 0)],
                required: &[],
                forbidden: &[],
            },
            RenderedShapeCase {
                shape: RuntimeGraphEvidenceSearchShape::ScopedMaterializedWithText,
                filter: "",
                bind_counts: &[("$6", 1), ("$1", 1)],
                required: &[
                    "scoped as materialized",
                    "as tsv",
                    "evidence.tsv @@ requested_text_query.ts_query",
                ],
                forbidden: &["is true"],
            },
            RenderedShapeCase {
                shape: RuntimeGraphEvidenceSearchShape::ScopedMaterializedLiteralOnly,
                filter: "",
                bind_counts: &[("$6", 1)],
                required: &[],
                forbidden: &["as tsv"],
            },
            RenderedShapeCase {
                shape: RuntimeGraphEvidenceSearchShape::ScopedPostFiltered,
                filter: canonical_filter,
                bind_counts: &[("$6", 2), ("$1", 2)],
                required: &["is true"],
                forbidden: &["scoped as materialized"],
            },
        ];
        for case in cases {
            assert_rendered_evidence_statement(case);
        }
    }
}
