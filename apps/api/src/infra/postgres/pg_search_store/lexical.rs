use super::*;

const TITLE_NGRAM_MIN_TERM_CHARS: usize = 8;
const TITLE_NGRAM_MAX_TERMS: usize = 4;
const TITLE_IDENTITY_MAX_TERMS: usize = 6;
/// Caps the database-facing symbol set for long user input.
const EXACT_IDENTIFIER_TERM_LIMIT: usize = 16;
/// One-character fragments are too ambiguous to justify a corpus scan.
const EXACT_IDENTIFIER_MIN_ALNUM_CHARS: usize = 2;
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
pub(super) const LEXICAL_RELAX_FLOOR: usize = 8;

/// Canonical revision-first title lane. Identity and soft-title retrieval differ
/// only in their bounded document source, score base, and result alias; keeping
/// the shared SQL here prevents the two access paths from drifting.
const TITLE_RAW_LANE_SQL_TEMPLATE: &str = "             title_{LANE}_raw as (
                 select {HIT_ALIAS}.chunk_id, {HIT_ALIAS}.document_id, {HIT_ALIAS}.score
{TITLE_DOCS_SOURCE}
                 join readable_docs rd on rd.document_id = d.document_id
                 cross join lateral (
                    with revision_candidate_ids as materialized (
                        select c.chunk_id
                        from knowledge_chunk c
                        where c.revision_id = rd.readable_revision_id
                    )
                    select c.chunk_id, c.document_id,
                        ({SCORE_BASE}
                            * coalesce(c.quality_score::double precision, 1.0)) as score
                    from revision_candidate_ids candidate
                    cross join lateral (
                       select hydrated_candidate.*
                       from knowledge_chunk hydrated_candidate
                       where hydrated_candidate.chunk_id = candidate.chunk_id
                       limit 1
                    ) c
                    where rd.document_id = c.document_id
                      and rd.workspace_id = c.workspace_id
                      and rd.library_id = c.library_id
                      and rd.readable_revision_id = c.revision_id
                      and c.library_id = $1
                      and c.chunk_state = 'ready'
                      and (($4::timestamptz is null and $5::timestamptz is null)
                           or (c.occurred_at is not null
                               and ($4::timestamptz is null or coalesce(c.occurred_until, c.occurred_at) >= $4)
                               and ($5::timestamptz is null or c.occurred_at <= $5)))
                    order by score desc, c.revision_id desc,
                             c.chunk_index asc, c.chunk_id asc
                     limit 2
                 ) {HIT_ALIAS}
             )";

const TITLE_IDENTITY_DOCS_SOURCE_SQL: &str = "                 from title_identity_docs d";
const TITLE_SOFT_DOCS_SOURCE_SQL: &str = "                 from (
                    select d.document_id
                    from soft_title_docs d
                    where $9::boolean
                 ) d";

fn render_title_raw_lane(
    lane: &str,
    hit_alias: &str,
    title_docs_source: &str,
    score_base: &str,
) -> String {
    TITLE_RAW_LANE_SQL_TEMPLATE
        .replace("{LANE}", lane)
        .replace("{HIT_ALIAS}", hit_alias)
        .replace("{TITLE_DOCS_SOURCE}", title_docs_source)
        .replace("{SCORE_BASE}", score_base)
}

fn render_title_raw_lanes() -> String {
    let identity = render_title_raw_lane(
        "identity",
        "identity_hit",
        TITLE_IDENTITY_DOCS_SOURCE_SQL,
        "(1000000.0 - c.chunk_index::double precision)",
    );
    let soft = render_title_raw_lane(
        "soft",
        "soft_hit",
        TITLE_SOFT_DOCS_SOURCE_SQL,
        "(50.0 - (c.chunk_index::double precision * 0.001))",
    );
    format!("{identity},\n{soft}")
}

/// Chunk lexical-lane CTE with the FTS constructor abstracted as `{FTS}`. The
/// two rungs of the relaxation ladder share this template verbatim and differ
/// only in which tsquery constructor is substituted — the user query text binds
/// via `$2` and structural identifier terms bind via `$11`; neither is interpolated.
const CHUNK_LEXICAL_SQL_TEMPLATE: &str = "with readable_docs as (
                 select d.document_id, d.workspace_id, d.library_id,
                        current_revision.revision_id as readable_revision_id
                 from knowledge_document d
                 join content_current_revision current_revision
                   on current_revision.document_id = d.document_id
                 join knowledge_structured_revision sr
                   on sr.revision_id = current_revision.revision_id
                  and sr.document_id = d.document_id
                  and sr.workspace_id = d.workspace_id
                  and sr.library_id = d.library_id
                  and sr.preparation_state = 'prepared'
                 where d.library_id = $1
                   and d.document_state = 'active'
                   and d.deleted_at is null
             ),
             readable_document_count as (
                 select count(*)::double precision as value
                 from readable_docs
             ),
             exact_identifier_terms as (
                 select distinct term.value
                 from unnest($11::text[]) term(value)
                 where term.value <> ''
             ),
             -- Document-level presence of each identifier term, asked as a
             -- semi-join so the scan can stop at a document's first matching
             -- chunk. Collecting every matching chunk and de-duplicating
             -- afterwards read chunk bodies for hits that changed nothing:
             -- measured on a 356k-chunk library, that cost 110 MB of shared
             -- buffers per identifier term against 21 MB here, and the whole
             -- statement dropped 78% (26493 -> 5837 buffers) under both
             -- force_custom_plan and force_generic_plan. The result is
             -- identical -- only the distinct document/term pairs are used
             -- downstream, never the chunks that produced them.
             exact_identifier_document_matches as (
                 select identifier_doc.document_id, term.value
                 from exact_identifier_terms term
                 cross join readable_docs identifier_doc
                 where exists (
                     select 1
                     from knowledge_chunk c
                     where c.library_id = $1
                       and c.chunk_state = 'ready'
                       and c.revision_id = identifier_doc.readable_revision_id
                       and c.document_id = identifier_doc.document_id
                       and c.workspace_id = identifier_doc.workspace_id
                       and c.search_tsv @@ plainto_tsquery(
                               '{TEXT_SEARCH_CONFIG}', ironrag_unaccent(term.value)
                           )
                       and strpos(c.content_text, term.value) > 0
                 )
             ),
             exact_identifier_document_frequency as (
                 select value, count(*)::double precision as document_count
                 from exact_identifier_document_matches
                 group by value
             ),
             exact_identifier_document_metadata as (
                 select matches.document_id,
                    (max(1.0 - frequency.document_count / readable_count.value) filter (
                        where frequency.document_count / readable_count.value <= 0.05
                    ))::double precision as rarity,
                    array_agg(matches.value order by matches.value)::text[] as terms,
                    (array_agg(matches.value order by matches.value) filter (
                        where frequency.document_count / readable_count.value <= 0.05
                    ))::text[] as rare_terms
                 from exact_identifier_document_matches matches
                 join exact_identifier_document_frequency frequency
                   on frequency.value = matches.value
                 cross join readable_document_count readable_count
                 where readable_count.value > 0.0
                 group by matches.document_id
             ),
             title_identity_docs as (
                 select d.document_id
                 from knowledge_document d
                 join readable_docs title_readable
                   on title_readable.document_id = d.document_id
                  and title_readable.workspace_id = d.workspace_id
                  and title_readable.library_id = d.library_id
                 where d.library_id = $1
                   and d.document_state = 'active'
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
                 join readable_docs title_readable
                   on title_readable.document_id = d.document_id
                  and title_readable.workspace_id = d.workspace_id
                  and title_readable.library_id = d.library_id
                 where d.library_id = $1
                   and d.document_state = 'active'
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
                 with text_hits as materialized (
                    select c.chunk_id, c.document_id, c.workspace_id, c.library_id, c.revision_id,
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
                            * coalesce(c.quality_score::double precision, 1.0) as score
                    from knowledge_chunk c
                    where c.search_tsv @@ {FTS}
                      and c.revision_id = any (
                          array(select rd.readable_revision_id from readable_docs rd)
                      )
                      and c.library_id = $1
                      and c.chunk_state = 'ready'
                      and (($4::timestamptz is null and $5::timestamptz is null)
                           or (c.occurred_at is not null
                               and ($4::timestamptz is null or coalesce(c.occurred_until, c.occurred_at) >= $4)
                               and ($5::timestamptz is null or c.occurred_at <= $5)))
                 ),
                 -- text_raw's scan already retains only hits whose revision is
                 -- in readable_docs, so the per-hit re-correlation against the
                 -- unindexed materialized CTE dropped no rows and only re-scanned
                 -- it once per hit.
                 text_hits_per_document as (
                    select text_hit.chunk_id, text_hit.document_id, text_hit.score,
                       row_number() over (
                           partition by text_hit.document_id
                           order by text_hit.score desc, text_hit.chunk_id asc
                       ) as per_revision_rank
                    from text_hits text_hit
                 )
                 select text_hit.chunk_id, text_hit.document_id, text_hit.score
                 from text_hits_per_document text_hit
                 where text_hit.per_revision_rank <= $3
                 order by text_hit.score desc, text_hit.chunk_id asc
                 limit $3
             ),
{TITLE_RAW_LANES},
             raw as (
                 select * from text_raw
                 union all select * from title_identity_raw
                 union all select * from title_soft_raw
             ),
             diversified as (
                 select *,
                    row_number() over (partition by document_id order by score desc, chunk_id asc) as per_doc_rank
                 from raw
             ),
             selected_ids as materialized (
                 select chunk_id, document_id, score
                 from diversified
                 where per_doc_rank <= 2
                 order by score desc, chunk_id asc
                 limit $10
             )
             select hydrated.chunk_id, hydrated.workspace_id, hydrated.library_id,
                hydrated.revision_id, hydrated.content_text, hydrated.normalized_text,
                hydrated.section_path, hydrated.heading_trail, selected.score,
                coalesce(identifier_metadata.rarity, 0.0)::double precision
                    as exact_identifier_rarity,
                coalesce(identifier_metadata.terms, array[]::text[])
                    as exact_identifier_terms,
                coalesce(identifier_metadata.rare_terms, array[]::text[])
                    as rare_exact_identifier_terms,
                hydrated.quality_score
             from selected_ids selected
             join knowledge_chunk hydrated on hydrated.chunk_id = selected.chunk_id
             left join exact_identifier_document_metadata identifier_metadata
               on identifier_metadata.document_id = selected.document_id
             order by selected.score desc, selected.chunk_id asc";

/// Structured source blocks are revision-scoped evidence. Joining the
/// readable document head in the lexical statement is essential: filtering
/// stale rows after this query would let them consume the bounded top-k and
/// starve the current readable revision.
const STRUCTURED_BLOCK_LEXICAL_SQL_TEMPLATE: &str =
    "select b.block_id, b.document_id, b.workspace_id, b.library_id, b.revision_id, b.ordinal,
        b.block_kind, b.text, b.normalized_text, b.section_path, b.heading_trail,
        ts_rank_cd(b.search_tsv, {FTS})::double precision as score
     from knowledge_structured_block b
     join knowledge_document d
       on d.document_id = b.document_id
      and d.workspace_id = b.workspace_id
      and d.library_id = b.library_id
      and exists (select 1 from content_current_revision current_revision
                  where current_revision.document_id = d.document_id
                    and current_revision.revision_id = b.revision_id)
      and d.document_state = 'active'
      and d.deleted_at is null
     join knowledge_structured_revision sr
       on sr.revision_id = b.revision_id
      and sr.document_id = b.document_id
      and sr.workspace_id = b.workspace_id
      and sr.library_id = b.library_id
      and sr.preparation_state = 'prepared'
     where b.library_id = $1
       and b.search_tsv @@ {FTS}
     order by score desc, b.revision_id desc, b.ordinal asc, b.block_id asc
     limit $3";

/// Typed facts have the same readable-head requirement as chunks and
/// structured blocks. The exact-value boost is preserved, but it may only rank
/// facts from the current readable revision of an active document.
const TECHNICAL_FACT_LEXICAL_SQL_TEMPLATE: &str =
    "select f.fact_id, f.document_id, f.workspace_id, f.library_id, f.revision_id, f.fact_kind,
        f.\u{63}anonical_value_text as normalized_value_text, f.display_value,
        (f.\u{63}anonical_value_exact = $3) as exact_match,
        (
            case when f.\u{63}anonical_value_exact = $3 then 1000000.0 else 0.0 end
            + ts_rank_cd(f.search_tsv, {FTS})::double precision
        ) as score
     from knowledge_technical_fact f
     join knowledge_document d
       on d.document_id = f.document_id
      and d.workspace_id = f.workspace_id
      and d.library_id = f.library_id
      and exists (select 1 from content_current_revision current_revision
                  where current_revision.document_id = d.document_id
                    and current_revision.revision_id = f.revision_id)
      and d.document_state = 'active'
      and d.deleted_at is null
     join knowledge_structured_revision sr
       on sr.revision_id = f.revision_id
      and sr.document_id = f.document_id
      and sr.workspace_id = f.workspace_id
      and sr.library_id = f.library_id
      and sr.preparation_state = 'prepared'
     where f.library_id = $1
       and (
            f.\u{63}anonical_value_exact = $3
            or f.search_tsv @@ {FTS}
       )
     order by score desc, f.fact_id asc
     limit $4";

/// Pass A (precise): exact AND `websearch_to_tsquery`, rendered with the
/// historical default text-search config and the shared safety filters.
pub(super) static CHUNK_LEXICAL_SQL_EXACT: LazyLock<String> =
    LazyLock::new(|| chunk_lexical_sql(DEFAULT_TEXT_SEARCH_CONFIG).0);

/// Relaxed passes (B/C): `to_tsquery`, fed a prefix tsquery string via `$2`.
pub(super) static CHUNK_LEXICAL_SQL_PREFIX: LazyLock<String> =
    LazyLock::new(|| chunk_lexical_sql(DEFAULT_TEXT_SEARCH_CONFIG).1);

/// The FTS-independent scoring inputs shared by both rungs of the chunk lexical
/// ladder. Pass A and the relaxed passes bind every field identically and differ
/// only in the FTS lane (`$2`), so they are computed once in
/// [`PgSearchStore::search_chunks_impl`] and moved into the per-pass closure as
/// one named value instead of a long positional argument list.
#[derive(Clone, Copy)]
pub(super) struct ChunkLexicalParams<'a> {
    pub(super) query_terms: &'a [String],
    pub(super) title_identity_terms: &'a [String],
    pub(super) title_ngram_terms: &'a [String],
    pub(super) title_soft_raw_enabled: bool,
    pub(super) exact_identifier_terms: &'a [String],
}

/// Renders the chunk lexical-lane `(exact_sql, prefix_sql)` pair for a given
/// Postgres text-search config name. `text_search_config == "simple"` preserves
/// the historical analyzer semantics.
pub(super) fn chunk_lexical_sql(text_search_config: &str) -> (String, String) {
    let template =
        CHUNK_LEXICAL_SQL_TEMPLATE.replace("{TITLE_RAW_LANES}", &render_title_raw_lanes());
    lexical_lane_sql_for_config(&template, text_search_config)
}

pub(super) fn structured_block_lexical_sql() -> (String, String) {
    lexical_lane_sql(STRUCTURED_BLOCK_LEXICAL_SQL_TEMPLATE)
}

pub(super) fn technical_fact_lexical_sql() -> (String, String) {
    lexical_lane_sql(TECHNICAL_FACT_LEXICAL_SQL_TEMPLATE)
}

impl PgSearchStore {
    /// Runs a single rung of the chunk lexical ladder.
    ///
    /// `sql` is one of the [`CHUNK_LEXICAL_SQL_EXACT`] / [`CHUNK_LEXICAL_SQL_PREFIX`]
    /// templates (identical except for the FTS constructor literal). `fts_input`
    /// is bound as `$2`: the raw user query for the exact pass, or the prebuilt
    /// prefix tsquery string for a relaxed pass. Every other bound parameter is
    /// identical across passes (carried by [`ChunkLexicalParams`]), so the
    /// title-aware scoring is preserved while only the FTS lane widens.
    pub(super) async fn run_chunk_lexical_pass(
        &self,
        sql: &str,
        fts_input: &str,
        scope: &ChunkLexicalQuery<'_>,
        params: &ChunkLexicalParams<'_>,
    ) -> anyhow::Result<Vec<KnowledgeChunkSearchRow>> {
        // The text lanes drive from the search index, and the planner can only
        // see that access path with the tsquery value in hand: a generic plan
        // cannot estimate `search_tsv @@ $2` or the readable-revision array, so
        // it drops the revision bitmap and heap-fetches every match across all
        // revisions (measured: 362k buffers / 635 ms generic vs 11.9k / 32 ms
        // custom on a 250-document library). Same transaction-local pinning as
        // the matching-terms and primary chunk-vector paths.
        let mut transaction =
            self.pool.begin().await.context("failed to start chunk lexical search")?;
        sqlx::query("select set_config('plan_cache_mode', $1, true)")
            .bind("force_custom_plan")
            .execute(&mut *transaction)
            .await
            .context("failed to force a custom chunk lexical plan")?;
        let rows = sqlx::query_as::<_, PgChunkSearchRow>(sqlx::AssertSqlSafe(sql))
            .bind(scope.library_id)
            .bind(fts_input)
            .bind(scope.limit.max(1).saturating_mul(4).max(48) as i64)
            .bind(scope.temporal_start)
            .bind(scope.temporal_end)
            .bind(params.query_terms)
            .bind(params.title_identity_terms)
            .bind(params.title_ngram_terms)
            .bind(params.title_soft_raw_enabled)
            .bind(scope.limit.max(1) as i64)
            .bind(params.exact_identifier_terms)
            .fetch_all(&mut *transaction)
            .await
            .context("failed to search knowledge chunks")?;
        transaction.commit().await.context("failed to finish chunk lexical search")?;
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
                exact_identifier_rarity: row.exact_identifier_rarity,
                exact_identifier_terms: row.exact_identifier_terms,
                rare_exact_identifier_terms: row.rare_exact_identifier_terms,
                quality_score: row.quality_score,
            })
            .collect())
    }
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

pub(super) fn exact_identifier_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in query.split(|ch: char| ch.is_whitespace() || matches!(ch, ',' | ';')) {
        let candidate =
            raw.trim_matches(is_identifier_wrapper).trim_end_matches(is_sentence_terminator);
        let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
        if alnum_count < EXACT_IDENTIFIER_MIN_ALNUM_CHARS
            || !literal_text_is_identifier_shaped(candidate)
        {
            continue;
        }
        let term = candidate.to_string();
        if seen.insert(term.clone()) {
            terms.push(term);
        }
        if terms.len() >= EXACT_IDENTIFIER_TERM_LIMIT {
            break;
        }
    }
    terms
}

const fn is_identifier_wrapper(ch: char) -> bool {
    matches!(ch, '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '"' | '\'' | '`' | '!' | '?')
}

const fn is_sentence_terminator(ch: char) -> bool {
    matches!(ch, '.' | ':')
}

pub(super) fn lexical_query_terms(query: &str) -> Vec<String> {
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

pub(super) fn title_ngram_terms(query_terms: &[String]) -> Vec<String> {
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

pub(super) fn title_identity_terms(
    query: &str,
    query_terms: &[String],
    query_origin: LexicalQueryOrigin,
) -> Vec<String> {
    if query_origin == LexicalQueryOrigin::DerivedProbe {
        return Vec::new();
    }
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

pub(super) const fn title_soft_raw_enabled(query_terms: &[String]) -> bool {
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
pub(super) fn lexical_prefix_tokens(query: &str) -> Vec<String> {
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
pub(super) fn lexical_exact_shape_lexemes(query: &str) -> Vec<String> {
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
pub(super) fn prefix_relaxed_tsquery_and(query: &str) -> Option<String> {
    prefix_relaxed_tsquery_with(query, " & ")
}

/// Pass C body: prefix-OR (`p1:* | p2:* | ...`). The widest, lowest-precision
/// rung; only used when the prefix-AND pass is still sparse.
pub(super) fn prefix_relaxed_tsquery_or(query: &str) -> Option<String> {
    prefix_relaxed_tsquery_with(query, " | ")
}

/// Whether the precise pass returned too few hits and the ladder should descend
/// to the next, more relaxed pass.
pub(super) const fn should_relax_lexical(hit_count: usize) -> bool {
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
pub(super) async fn run_lexical_ladder<Row, K, F, Fut>(
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
        // A single-token query collapses both relaxed rungs to the same
        // tsquery body ("'tok':*" has no operator to differ on), so the OR
        // rung would re-issue the AND rung's statement verbatim and the
        // dedup below would drop every row it returned. Measured on a
        // 10-turn wave: 164 of 340 relaxed executions were such pairs.
        let mut executed_bodies: BTreeSet<String> = BTreeSet::new();
        for body in [prefix_relaxed_tsquery_and(query), prefix_relaxed_tsquery_or(query)] {
            let Some(prefix_query) = body else { continue };
            if !executed_bodies.insert(prefix_query.clone()) {
                continue;
            }
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
pub(super) fn lexical_lane_sql(template: &str) -> (String, String) {
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
            .replace("{TEXT_SEARCH_CONFIG}", config)
            .replace("{FTS}", &format!("websearch_to_tsquery('{config}', ironrag_unaccent($2))")),
        template
            .replace("{TEXT_SEARCH_CONFIG}", config)
            .replace("{FTS}", &format!("to_tsquery('{config}', ironrag_unaccent($2))")),
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
