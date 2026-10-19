use super::*;

#[test]
fn setup_structured_block_candidates_use_indexable_kinds_before_the_limit() {
    let sql = setup_structured_blocks_by_revision_sql();
    let compact = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let structured_filter = compact.find("b.block_kind IN (");
    let structured_limit = compact.find("LIMIT $3");

    assert!(compact.contains("'table', 'table_row', 'code_block', 'source_unit'"));
    assert!(
        compact.contains("NOT EXISTS ( SELECT 1 FROM sampled s WHERE s.block_id = b.block_id )")
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
}

/// Every answer-driving library search joins the chunk row to its current
/// readable document head and prepared structured revision, and carries the
/// state gates that keep superseded and deleted work out of the bounded set.
const ANSWER_DRIVING_LIBRARY_SEARCH_FRAGMENTS: &[&str] = &[
    "from knowledge_chunk c",
    "join knowledge_document d",
    "d.document_id = c.document_id",
    "d.library_id = c.library_id",
    "current_revision.revision_id = c.revision_id",
    "d.document_state = 'active'",
    "d.deleted_at is null",
    "join knowledge_structured_revision structured",
    "structured.revision_id = c.revision_id",
    "structured.workspace_id = c.workspace_id",
    "structured.library_id = c.library_id",
    "structured.document_id = c.document_id",
    "structured.preparation_state = 'prepared'",
    "c.chunk_state = 'ready'",
    "c.chunk_kind is distinct from 'source_profile'",
];

fn assert_answer_driving_library_search_uses_current_heads(sql: &str, limit_clause: &str) {
    for fragment in ANSWER_DRIVING_LIBRARY_SEARCH_FRAGMENTS {
        assert!(sql.contains(*fragment), "library search is missing {fragment:?}");
    }
    assert!(
        sql.find("d.deleted_at is null").unwrap() < sql.rfind(limit_clause).unwrap(),
        "current-document filters must run before the bounded pattern-search result",
    );
}

#[test]
fn code_pattern_search_filters_noncurrent_chunks_before_limit() {
    assert_answer_driving_library_search_uses_current_heads(
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
fn transport_pattern_search_filters_noncurrent_chunks_before_limit() {
    let sql = transport_pattern_chunk_search_sql();
    assert!(sql.starts_with("select c.chunk_id, c.workspace_id, c.library_id"));
    assert_answer_driving_library_search_uses_current_heads(&sql, "limit $4");
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
    let sql = multi_revision_matching_terms_sql();
    let compact_sql = sql.split_whitespace().collect::<Vec<_>>().join(" ");
    let bounded_start = sql.find("bounded as (").unwrap_or(sql.len());
    let global_limit = sql.find("limit $4").unwrap_or(sql.len());
    let hydration_join = sql.rfind("join knowledge_chunk c").unwrap_or(sql.len());

    assert!(compact_sql.contains("order by revision_rank asc, match_score desc, ordinal asc"));
    assert!(bounded_start < global_limit);
    assert!(global_limit < hydration_join);
    assert!(compact_sql.contains("order by bounded.ordinal asc"));
    assert!(compact_sql.contains("structured.preparation_state = 'prepared'"));
}

/// Positive and negative structural invariants of the trigram-prefiltered
/// matching-terms SQL: one revision-scoped concat LIKE scan, no library-wide
/// binds, and a single shared prepared-revision gate.
const MATCHING_TERMS_PREFILTER_REQUIRED: &[&str] = &[
    "like any($5::text[])",
    "c.revision_id = any($1::uuid[])",
    MATCHING_TERMS_LOWER_TEXT_SQL,
    "hits as materialized",
    "join lateral",
    "where c.chunk_id = hits.chunk_id",
    "offset 0",
    "join requested",
    "cross join lateral",
    "where matches.matched_count > 0",
    "(matches.matched_count * 10000 - matches.earliest_pos) as match_score",
    "order by match_score desc, chunk_index asc, chunk_id asc",
];

const MATCHING_TERMS_PREFILTER_FORBIDDEN: &[&str] = &[
    "c.library_id = any($6::uuid[])",
    "$6",
    "from unnest($5::text[]) as pat",
    " union ",
    "search_tsv",
    "to_tsquery",
    "plainto_tsquery",
];

fn compact_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn assert_matching_terms_prefilter_fragments(sql: &str) {
    for fragment in MATCHING_TERMS_PREFILTER_REQUIRED {
        assert!(sql.contains(*fragment), "matching-terms SQL is missing {fragment:?}");
    }
    for forbidden in MATCHING_TERMS_PREFILTER_FORBIDDEN {
        assert!(!sql.contains(*forbidden), "matching-terms SQL must not contain {forbidden:?}");
    }
}

#[test]
fn matching_terms_sql_prefilters_with_lower_like_then_exact_strpos_scores() {
    let sql = multi_revision_matching_terms_sql();
    let compact_sql = compact_sql(&sql);
    assert_matching_terms_prefilter_fragments(&compact_sql);
    assert_eq!(
        compact_sql.matches("like any($5::text[])").count(),
        1,
        "revision-scoped concat LIKE must be one gin_trgm scan, not three global column scans"
    );
    assert_eq!(
        compact_sql.matches("structured.preparation_state = 'prepared'").count(),
        1,
        "trigram LIKE plus strpos scoring must share one prepared-revision gate",
    );
    let like_prefilter = compact_sql.find("like any($5::text[])").unwrap_or(usize::MAX);
    let exact_rescore = compact_sql
        .find("lower(candidates.normalized_text) as normalized_lower")
        .unwrap_or(usize::MAX);
    assert!(like_prefilter < usize::MAX);
    assert!(exact_rescore < usize::MAX);
    assert!(
        like_prefilter < exact_rescore,
        "trigram LIKE must narrow candidates before strpos scoring laterals"
    );
}

#[test]
fn matching_terms_like_predicate_is_revision_scoped_concat_indexable() {
    let hit_sql = matching_terms_trigram_hit_sql();

    assert!(hit_sql.contains("c.revision_id = any($1::uuid[])"));
    assert!(!hit_sql.contains("library_id"));
    assert!(!hit_sql.contains("$6"));
    assert!(hit_sql.contains(MATCHING_TERMS_LOWER_TEXT_SQL));
    assert!(hit_sql.contains("like any($5::text[])"));
    assert_eq!(hit_sql.matches("like any($5::text[])").count(), 1);
    assert!(
        !hit_sql.contains(" or "),
        "OR-of-column LIKEs plus a revision join is not quality-neutral-fast: the planner seq-filters 356k chunks"
    );
    assert!(
        !hit_sql.contains(" union "),
        "three global column GIN scans heap-fetch other libraries; concat+revision_id is the one indexable path"
    );
}

#[test]
fn matching_terms_hits_apply_revision_inside_materialized_cte() {
    let sql = multi_revision_matching_terms_sql();
    assert!(
        revision_predicate_lives_inside_hits(&sql),
        "revision_id must live inside hits so BitmapAnd can use the revision btree"
    );

    let planted_library_hits =
        sql.replace("c.revision_id = any($1::uuid[])", "c.library_id = any($6::uuid[])");
    assert!(
        !revision_predicate_lives_inside_hits(&planted_library_hits),
        "library_id in hits must fail the structural gate: it restores the 18k-heap library GIN"
    );

    let planted_post_hits = format!(
        "with requested as (
           select revision_id, min(ordinal)::integer as ordinal
           from unnest($1::uuid[]) with ordinality as request(revision_id, ordinal)
           group by revision_id
         ),
         hits as materialized (
           select c.chunk_id
           from knowledge_chunk c
           where {MATCHING_TERMS_LOWER_TEXT_SQL} like any($5::text[])
         ),
         candidates as (
           select c.chunk_id
           from hits
           join knowledge_chunk c on c.chunk_id = hits.chunk_id
           join requested on c.revision_id = requested.revision_id
         )
         select 1"
    );
    assert!(
        !revision_predicate_lives_inside_hits(&planted_post_hits),
        "a revision filter after hits must fail the structural gate"
    );
}

#[test]
fn matching_terms_like_patterns_wrap_terms_and_drop_short_tokens_when_selective_terms_exist() {
    // Production LIKE patterns do not escape `%` / `_` / `\`. That pre-exists
    // this increment and must not be "fixed" here: escaping would change the
    // candidate set. Keep the known gap visible next to the wrapper.
    assert_eq!(
        matching_terms_like_patterns(&["nginx".into(), "/".into(), "ab".into()]),
        vec!["%nginx%".to_string()]
    );
    assert_eq!(matching_terms_like_patterns(&["/".into()]), vec!["%/%".to_string()]);
}

#[test]
fn matching_terms_like_prefilter_keeps_strpos_rank_order_and_chunk_set() {
    let corpus = matching_terms_quality_corpus();
    let term_sets = [
        vec!["nginx".to_string()],
        vec!["api_key=secret".to_string()],
        vec!["secret_token".to_string(), "port=443".to_string()],
        vec!["https://cdn.example.com/v1/export?api_key=secret_token".to_string()],
        vec!["key=value".to_string()],
        vec!["example".to_string(), "nginx".to_string()],
    ];

    for terms in term_sets {
        let exact = rank_matching_terms_strpos(&corpus, &terms, 8);
        let prefiltered = rank_matching_terms_like_then_strpos(&corpus, &terms, 8);
        let concat_prefiltered = rank_matching_terms_concat_like_then_strpos(&corpus, &terms, 8);
        assert_eq!(
            exact, prefiltered,
            "LIKE prefilter must keep strpos chunk_id set and rank order for {terms:?}"
        );
        assert_eq!(
            exact, concat_prefiltered,
            "concat LIKE prefilter must keep strpos chunk_id set and rank order for {terms:?}"
        );
    }
}

#[test]
fn simple_tsquery_prefix_prefilter_misses_substring_inside_url_lexeme() {
    let haystack = "See https://cdn.example.com/v1/export?api_key=secret_token&x=1 for export";
    let term = "api_key=secret";

    assert!(
        strpos_term_matches(haystack, haystack, "", term),
        "strpos must find a substring inside a URL/query tail"
    );
    assert!(
        like_term_matches(haystack, haystack, "", term),
        "lower LIKE must stay equivalent to strpos for URL/query tails"
    );
    assert!(
        !simple_tsquery_prefix_matches(haystack, term),
        "this planted case is the quality regression that expanding to_tsquery('simple', prefix:*) caused"
    );
}

#[test]
fn concat_separator_does_not_invent_cross_column_substring_matches() {
    assert!(
        !concat_like_term_matches("foo", "bar", "", "foobar"),
        "chr(1) must keep concat LIKE from matching a term split across columns"
    );
    assert!(!strpos_term_matches("foo", "bar", "", "foobar"));
    assert!(concat_like_term_matches("foobar", "x", "", "foobar"));
    assert!(concat_like_term_matches("x", "foobar", "", "foobar"));
    assert!(concat_like_term_matches("x", "y", "foobar", "foobar"));
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

#[derive(Clone)]
struct MatchingTermsFixture {
    chunk_id: uuid::Uuid,
    chunk_index: i32,
    normalized_text: &'static str,
    content_text: &'static str,
    window_text: &'static str,
}

fn matching_terms_hits_cte_sql(sql: &str) -> String {
    super::materialized_cte_body(sql, "hits")
}

fn revision_predicate_lives_inside_hits(sql: &str) -> bool {
    let hits = matching_terms_hits_cte_sql(sql);
    let compact = hits.split_whitespace().collect::<Vec<_>>().join(" ");
    compact.contains("c.revision_id = any($1::uuid[])") && !compact.contains("library_id")
}

fn matching_terms_quality_corpus() -> Vec<MatchingTermsFixture> {
    vec![
        MatchingTermsFixture {
            chunk_id: uuid::Uuid::from_u128(1),
            chunk_index: 0,
            normalized_text: "nginx listen 80",
            content_text: "nginx listen 80",
            window_text: "",
        },
        MatchingTermsFixture {
            chunk_id: uuid::Uuid::from_u128(2),
            chunk_index: 1,
            normalized_text: "See https://cdn.example.com/v1/export?api_key=secret_token&x=1 for export",
            content_text: "See https://cdn.example.com/v1/export?api_key=secret_token&x=1 for export",
            window_text: "cdn.example.com export",
        },
        MatchingTermsFixture {
            chunk_id: uuid::Uuid::from_u128(3),
            chunk_index: 2,
            normalized_text: "db_url=postgres://app:secret@db/app",
            content_text: "db_url=postgres://app:secret@db/app",
            window_text: "",
        },
        MatchingTermsFixture {
            chunk_id: uuid::Uuid::from_u128(4),
            chunk_index: 3,
            normalized_text: "port=443 in the reverse proxy",
            content_text: "port=443 in the reverse proxy",
            window_text: "tls",
        },
        MatchingTermsFixture {
            chunk_id: uuid::Uuid::from_u128(5),
            chunk_index: 4,
            normalized_text: "unrelated paragraph about orchards",
            content_text: "unrelated paragraph about orchards",
            window_text: "",
        },
        MatchingTermsFixture {
            chunk_id: uuid::Uuid::from_u128(6),
            chunk_index: 5,
            normalized_text: "key=value pair in the env file",
            content_text: "export KEY=value",
            window_text: "env",
        },
    ]
}

fn strpos_1(haystack: &str, term: &str) -> i32 {
    haystack.find(term).map(|index| i32::try_from(index).expect("fit") + 1).unwrap_or(0)
}

fn strpos_term_matches(normalized: &str, content: &str, window: &str, term: &str) -> bool {
    let normalized = normalized.to_lowercase();
    let content = content.to_lowercase();
    let window = window.to_lowercase();
    strpos_1(&normalized, term) > 0 || strpos_1(&content, term) > 0 || strpos_1(&window, term) > 0
}

fn like_escape(term: &str) -> String {
    term.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

/// Whether the escaped pattern matches starting exactly here.
///
/// This is the inner half of the LIKE emulation below. Written inline it was a
/// loop with four escapes from it, and the question it answers — "does the
/// pattern sit at this position" — was buried in index arithmetic.
fn like_matches_at(
    mut haystack: std::iter::Peekable<std::str::Chars<'_>>,
    escaped: &[char],
) -> bool {
    let mut index = 0;
    while index < escaped.len() {
        let pattern = escaped[index];
        if pattern == '\\' {
            index += 1;
            let Some(literal) = escaped.get(index).copied() else {
                return false;
            };
            if haystack.next() != Some(literal) {
                return false;
            }
            index += 1;
            continue;
        }
        // The SQL pattern is always '%' || escaped_term || '%'; a raw '%' never
        // appears unescaped in the middle of our terms.
        if pattern == '%' || haystack.next() != Some(pattern) {
            return false;
        }
        index += 1;
    }
    true
}

fn like_contains(haystack: &str, escaped_term: &str) -> bool {
    let escaped: Vec<char> = escaped_term.chars().collect();
    if escaped.is_empty() {
        return true;
    }
    let mut haystack_chars = haystack.chars().peekable();
    while haystack_chars.peek().is_some() {
        if like_matches_at(haystack_chars.clone(), &escaped) {
            return true;
        }
        haystack_chars.next();
    }
    false
}

#[test]
fn like_contains_matches_postgres_semantics_for_the_patterns_we_build() {
    // This helper is the oracle the matching-terms tests assert against, so a
    // wrong emulation would make those tests agree with the wrong answer. It
    // had no test of its own: removing the unescaped-'%' guard left all
    // thirty-seven green.
    assert!(like_contains("alpha beta", &like_escape("beta")));
    assert!(!like_contains("alpha beta", &like_escape("gamma")));

    // A '%' inside a term is escaped by like_escape and must match literally
    // rather than acting as a wildcard.
    assert!(like_contains("rate 50% today", &like_escape("50%")));
    assert!(!like_contains("rate 50 today", &like_escape("50%")));

    // An unescaped '%' is not something like_escape can produce, and the
    // emulation refuses it rather than treating it as a literal or a wildcard.
    // The haystack has to contain a literal '%' for this to say anything: with
    // any other input both the refusal and a literal match return false, and
    // the assertion passes without touching the branch it means to pin.
    assert!(!like_contains("any%all", "any%all"));
    // The escaped form of the same term does match that haystack, so the case
    // above fails for the reason claimed rather than for want of a match.
    assert!(like_contains("any%all", &like_escape("any%all")));

    // The other two characters SQL LIKE treats specially.
    assert!(like_contains("a_b", &like_escape("a_b")));
    assert!(!like_contains("axb", &like_escape("a_b")));
    assert!(like_contains("back\\slash", &like_escape("back\\slash")));

    // An empty term matches everything, which is what '%%' does.
    assert!(like_contains("anything", &like_escape("")));
}

fn like_term_matches(normalized: &str, content: &str, window: &str, term: &str) -> bool {
    let escaped = like_escape(term);
    let normalized = normalized.to_lowercase();
    let content = content.to_lowercase();
    let window = window.to_lowercase();
    like_contains(&normalized, &escaped)
        || like_contains(&content, &escaped)
        || like_contains(&window, &escaped)
}

fn matching_terms_strpos_score(
    chunk: &MatchingTermsFixture,
    terms: &[String],
) -> Option<(i32, i32, uuid::Uuid)> {
    let normalized = chunk.normalized_text.to_lowercase();
    let content = chunk.content_text.to_lowercase();
    let window = chunk.window_text.to_lowercase();
    let mut matched = 0_i32;
    let mut earliest = i32::MAX;
    for term in terms {
        if !strpos_term_matches(&normalized, &content, &window, term) {
            continue;
        }
        matched += 1;
        let pos = [strpos_1(&normalized, term), strpos_1(&content, term), strpos_1(&window, term)]
            .into_iter()
            .filter(|pos| *pos > 0)
            .min()
            .unwrap_or(i32::MAX);
        earliest = earliest.min(pos);
    }
    (matched > 0).then_some((matched * 10_000 - earliest, chunk.chunk_index, chunk.chunk_id))
}

fn rank_matching_terms_strpos(
    corpus: &[MatchingTermsFixture],
    terms: &[String],
    limit: usize,
) -> Vec<uuid::Uuid> {
    let mut ranked = corpus
        .iter()
        .filter_map(|chunk| matching_terms_strpos_score(chunk, terms))
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)).then_with(|| left.2.cmp(&right.2))
    });
    ranked.into_iter().take(limit).map(|(_, _, chunk_id)| chunk_id).collect()
}

fn rank_matching_terms_like_then_strpos(
    corpus: &[MatchingTermsFixture],
    terms: &[String],
    limit: usize,
) -> Vec<uuid::Uuid> {
    let candidates = corpus
        .iter()
        .filter(|chunk| {
            terms.iter().any(|term| {
                like_term_matches(
                    chunk.normalized_text,
                    chunk.content_text,
                    chunk.window_text,
                    term,
                )
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    rank_matching_terms_strpos(&candidates, terms, limit)
}

fn concat_like_term_matches(normalized: &str, content: &str, window: &str, term: &str) -> bool {
    let concat = format!(
        "{}\u{1}{}\u{1}{}",
        normalized.to_lowercase(),
        content.to_lowercase(),
        window.to_lowercase()
    );
    like_contains(&concat, &like_escape(term))
}

fn rank_matching_terms_concat_like_then_strpos(
    corpus: &[MatchingTermsFixture],
    terms: &[String],
    limit: usize,
) -> Vec<uuid::Uuid> {
    let candidates = corpus
        .iter()
        .filter(|chunk| {
            terms.iter().any(|term| {
                concat_like_term_matches(
                    chunk.normalized_text,
                    chunk.content_text,
                    chunk.window_text,
                    term,
                )
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    rank_matching_terms_strpos(&candidates, terms, limit)
}

fn simple_tsquery_prefix_matches(haystack: &str, term: &str) -> bool {
    const PREFIX_CHARS: usize = 5;
    let prefixes = term
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| token.chars().count() >= 4)
        .map(|token| token.to_lowercase().chars().take(PREFIX_CHARS).collect::<String>())
        .collect::<Vec<_>>();
    if prefixes.is_empty() {
        return false;
    }
    simple_tsvector_lexemes(haystack)
        .iter()
        .any(|lexeme| prefixes.iter().any(|prefix| lexeme.starts_with(prefix.as_str())))
}

fn simple_tsvector_lexemes(haystack: &str) -> Vec<String> {
    haystack
        .split_whitespace()
        .flat_map(|token| {
            if token.contains("://") {
                vec![
                    token
                        .trim_matches(|ch: char| matches!(ch, ',' | '.' | ';' | ')'))
                        .to_lowercase(),
                ]
            } else {
                alphanumeric_lexemes(token)
            }
        })
        .collect()
}

fn alphanumeric_lexemes(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

#[test]
fn revision_windows_drop_invalid_ranges_without_reordering_valid_requests() {
    let revision_a = Uuid::from_u128(1);
    let revision_b = Uuid::from_u128(2);

    let windows =
        normalized_revision_windows(&[(revision_a, 0, 2), (revision_b, 4, 3), (revision_b, 5, 6)]);

    assert_eq!(windows, vec![(revision_a, 0, 2), (revision_b, 5, 6)]);
}
