use sqlx::PgPool;
use uuid::Uuid;

use crate::shared::text_tokens::{literal_wildcard_prefixes, normalized_alnum_token_sequence_by};

use super::types::RuntimeGraphNodeRow;

const RUNTIME_GRAPH_ENTITY_SEARCH_TOKEN_CAP: usize = 8;
const RUNTIME_GRAPH_ENTITY_SEARCH_TOKEN_MIN_CHARS: usize = 3;
/// Searches `runtime_graph_node` by keyword overlap against graph node data.
///
/// Words shorter than 3 characters are ignored to avoid noise. Returns up to
/// `limit` non-document nodes ordered by `support_count` descending. The match
/// surface is deliberately limited to data already attached to the node: label,
/// normalized node type, summary, and extracted aliases.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn search_runtime_graph_nodes_by_query_text(
    pool: &PgPool,
    library_id: Uuid,
    query_text: &str,
    limit: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select
            n.id, n.library_id, n.\u{63}anonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from runtime_graph_node n
         where n.library_id = $1
           and n.node_type <> 'document'
           and exists (
               select 1 from unnest(string_to_array(lower($2), ' ')) as word
               where length(trim(word)) > 2
                 and (
                    lower(n.label) like '%' || trim(word) || '%'
                    or lower(n.node_type) like '%' || trim(word) || '%'
                    or coalesce(lower(n.summary), '') like '%' || trim(word) || '%'
                    or exists (
                        select 1
                        from jsonb_array_elements_text(n.aliases_json) as alias(value)
                        where lower(alias.value) like '%' || trim(word) || '%'
                    )
                 )
           )
         order by n.support_count desc, n.label asc, n.id asc
         limit $3",
    )
    .bind(library_id)
    .bind(query_text)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Searches admitted runtime graph entities for one projection version using
/// label, aliases, and summary text.
///
/// Exact label matches rank above prefix and substring matches; ties break on
/// `support_count` descending so the best-supported entity wins.
///
/// # Errors
/// Returns any `SQLx` error raised during the query.
pub async fn search_admitted_runtime_graph_entities_by_query_text(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    query_text: &str,
    limit: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    let normalized_query = query_text.trim().to_lowercase();
    if normalized_query.is_empty() {
        return Ok(Vec::new());
    }

    let wildcard_prefixes = runtime_graph_entity_wildcard_prefixes(&normalized_query);
    if !wildcard_prefixes.is_empty() {
        let rows = search_admitted_runtime_graph_entities_by_wildcard_prefixes(
            pool,
            library_id,
            projection_version,
            &wildcard_prefixes,
            limit,
        )
        .await?;
        if !rows.is_empty() {
            return Ok(rows);
        }
    }

    let search_terms = runtime_graph_entity_search_terms(&normalized_query);
    let candidate_limit = limit.saturating_mul(6).min(1_000).max(limit);
    let include_term_branch =
        runtime_graph_entity_search_includes_term_branch(&normalized_query, &search_terms);
    let entity_search_sql = runtime_graph_entity_search_sql(include_term_branch);
    let mut rows = sqlx::query_as::<_, RuntimeGraphNodeRow>(sqlx::AssertSqlSafe(entity_search_sql))
        .bind(library_id)
        .bind(projection_version)
        .bind(&normalized_query)
        .bind(&search_terms)
        .bind(limit)
        .bind(candidate_limit)
        .fetch_all(pool)
        .await?;

    let remaining_limit = limit.saturating_sub(rows.len() as i64);
    if remaining_limit <= 0 {
        return Ok(rows);
    }

    let matched_node_ids = rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let mut summary_rows = sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "select
            n.id, n.library_id, n.\u{63}anonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from runtime_graph_node n
         where n.library_id = $1
           and n.projection_version = $2
           and n.node_type <> 'document'
           and not (n.id = any($5::uuid[]))
           and (
                coalesce(lower(n.summary), '') like '%' || $3 || '%'
                or exists (
                    select 1
                    from unnest($4::text[]) as term(value)
                    where coalesce(lower(n.summary), '') like '%' || term.value || '%'
                )
           )
         order by
            case
                when coalesce(lower(n.summary), '') like '%' || $3 || '%' then 0
                else 1
            end asc,
            n.support_count desc,
            n.label asc,
            n.created_at asc
         limit $6",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(&normalized_query)
    .bind(&search_terms)
    .bind(&matched_node_ids)
    .bind(remaining_limit)
    .fetch_all(pool)
    .await?;

    rows.append(&mut summary_rows);
    Ok(rows)
}

async fn search_admitted_runtime_graph_entities_by_wildcard_prefixes(
    pool: &PgPool,
    library_id: Uuid,
    projection_version: i64,
    wildcard_prefixes: &[String],
    limit: i64,
) -> Result<Vec<RuntimeGraphNodeRow>, sqlx::Error> {
    let candidate_limit = limit.saturating_mul(6).min(1_000).max(limit);
    sqlx::query_as::<_, RuntimeGraphNodeRow>(
        "with wildcard_prefixes(value) as (
            select unnest($3::text[])
         ),
         matched_ids as (
            select id, min(match_rank) as match_rank
            from (
                select id, 0::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    join wildcard_prefixes prefix
                      on lower(n.label) like prefix.value || '%' escape '~'
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $5
                ) label_prefix
                union all
                select id, 1::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and exists (
                        select 1
                        from jsonb_array_elements_text(
                            case
                                when jsonb_typeof(n.aliases_json) = 'array'
                                then n.aliases_json
                                else '[]'::jsonb
                            end
                        ) as alias(value)
                        join wildcard_prefixes prefix
                          on lower(alias.value) like prefix.value || '%' escape '~'
                      )
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $5
                ) alias_prefix
            ) matches
            group by id
            order by min(match_rank) asc, id asc
            limit $5
         )
         select
            n.id, n.library_id, n.\u{63}anonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from matched_ids matched
         join runtime_graph_node n on n.id = matched.id
         order by
            matched.match_rank asc,
            n.support_count desc,
            n.label asc,
            n.created_at asc
         limit $4",
    )
    .bind(library_id)
    .bind(projection_version)
    .bind(wildcard_prefixes)
    .bind(limit)
    .bind(candidate_limit)
    .fetch_all(pool)
    .await
}

pub(super) fn runtime_graph_entity_wildcard_prefixes(query_text: &str) -> Vec<String> {
    literal_wildcard_prefixes(query_text, 2)
        .into_iter()
        .map(|prefix| escape_sql_like_literal(&prefix))
        .collect()
}

fn escape_sql_like_literal(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(ch, '~' | '%' | '_') {
            escaped.push('~');
        }
        escaped.push(ch);
    }
    escaped
}

pub(super) fn runtime_graph_entity_search_terms(query_text: &str) -> Vec<String> {
    normalized_alnum_token_sequence_by(
        query_text,
        |token| {
            token.chars().any(char::is_numeric)
                || token.chars().count() >= RUNTIME_GRAPH_ENTITY_SEARCH_TOKEN_MIN_CHARS
        },
        Some(RUNTIME_GRAPH_ENTITY_SEARCH_TOKEN_CAP),
    )
}

/// Whether the rank-6 keyword-term branch can add a row the other branches do
/// not already rank at least as well.
///
/// A single normalized token that equals `$3` makes `$4` exactly `[$3]`, so the
/// rank-6 match set is the rank-4 label-phrase set and every one of its ids is
/// already ranked 4. Dropping the branch removes one full label-index scan per
/// entity search for the common single-token label without changing the
/// returned rows or their order.
pub(super) fn runtime_graph_entity_search_includes_term_branch(
    normalized_query: &str,
    search_terms: &[String],
) -> bool {
    !(search_terms.len() == 1 && search_terms[0].as_str() == normalized_query)
}

/// Builds the admitted-entity keyword search over `runtime_graph_node`.
///
/// The rank-6 branch is included only when
/// [`runtime_graph_entity_search_includes_term_branch`] says it can contribute.
pub(super) fn runtime_graph_entity_search_sql(include_term_branch: bool) -> String {
    let term_branch = if include_term_branch {
        r#"union all
                select id, 6::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    join search_terms term on lower(n.label) like '%' || term.value || '%'
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) label_terms"#
    } else {
        ""
    };
    format!(
        r#"with search_terms(value) as (
            select unnest($4::text[])
         ),
         matched_ids as (
            select id, min(match_rank) as match_rank
            from (
                select id, 0::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and md5(lower(n.label)) = md5($3)
                      and lower(n.label) = $3
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) label_exact
                union all
                select id, 1::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.aliases_json::text) like '%"' || $3 || '"%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) alias_exact
                union all
                select id, 2::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.label) like $3 || '%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) label_prefix
                union all
                select id, 3::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.aliases_json::text) like '%"' || $3 || '%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) alias_prefix
                union all
                select id, 4::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.label) like '%' || $3 || '%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) label_phrase
                union all
                select id, 5::integer as match_rank
                from (
                    select n.id
                    from runtime_graph_node n
                    where n.library_id = $1
                      and n.projection_version = $2
                      and n.node_type <> 'document'
                      and lower(n.aliases_json::text) like '%' || $3 || '%'
                    order by n.support_count desc, n.label asc, n.created_at asc, n.id asc
                    limit $6
                ) alias_phrase
                {term_branch}
            ) matches
            group by id
            order by min(match_rank) asc, id asc
            limit $6
         )
         select
            n.id, n.library_id, n.canonical_key, n.label, n.node_type,
            n.aliases_json, n.summary, n.metadata_json, n.support_count,
            n.projection_version, n.created_at, n.updated_at
         from matched_ids matched
         join runtime_graph_node n on n.id = matched.id
         order by
            matched.match_rank asc,
            n.support_count desc,
            n.label asc,
            n.created_at asc
         limit $5"#
    )
}
