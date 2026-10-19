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
fn chunk_lexical_sql_adds_strict_identifier_frequency_metadata_without_reordering() {
    for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
        assert!(sql.contains("unnest($11::text[])"));
        assert!(sql.contains("frequency.document_count / readable_count.value <= 0.05"));
        assert!(sql.contains("as exact_identifier_rarity"));
        assert!(sql.contains("as exact_identifier_terms"));
        assert!(sql.contains("as rare_exact_identifier_terms"));
        assert!(!sql.contains("order by exact_identifier_rarity"));
        assert!(!sql.contains("score + exact_identifier_rarity"));
        assert_eq!(
            sql.matches("order by text_hit.score desc, text_hit.chunk_id asc").count(),
            2,
            "the text lane keeps one per-document and one global ordering, \
             both on (score desc, chunk_id asc)",
        );
        assert!(sql.contains(
            "row_number() over (partition by document_id order by score desc, chunk_id asc)"
        ));
        let selected = compact_sql(named_parenthesized_bodies(sql, "selected_ids")[0]);
        assert!(selected.contains("order by score desc, chunk_id asc limit $10"));
        assert!(sql.contains("order by selected.score desc, selected.chunk_id asc"));
        assert!(!sql.contains("{TEXT_SEARCH_CONFIG}"));
    }
}

#[test]
fn exact_identifier_terms_follow_token_structure_without_word_lists() {
    assert_eq!(
        exact_identifier_terms(
            "error AlphaFault circuit_breaker_threshold crate::state::Phase @scope/item module.rs node2"
        ),
        vec!["AlphaFault", "circuit_breaker_threshold", "module.rs", "node2",]
    );
    assert!(exact_identifier_terms("error handling types and codes").is_empty());
}

fn assert_identity_score_lane_uses_bound_identity_terms(sql: &str) {
    assert_eq!(sql.matches("cardinality($7::text[]) > 0").count(), 1);
    assert_eq!(sql.matches("from title_identity_docs d").count(), 1);
    assert_eq!(sql.matches("select identity_hit.chunk_id").count(), 1);
    assert_eq!(sql.matches("((1000000.0 - c.chunk_index::double precision)").count(), 1);
}

#[test]
fn derived_probe_cannot_activate_document_identity_score_lane() {
    let query = "sample ledger";
    let query_terms = lexical_query_terms(query);

    assert_eq!(query_terms, vec!["sample".to_string(), "ledger".to_string()]);
    assert!(title_identity_terms(query, &query_terms, LexicalQueryOrigin::DerivedProbe).is_empty());
    for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
        assert_identity_score_lane_uses_bound_identity_terms(sql);
    }
}

#[test]
fn user_query_keeps_document_identity_score_lane_for_direct_title_ask() {
    let query = "sample ledger";
    let query_terms = lexical_query_terms(query);

    assert_eq!(
        title_identity_terms(query, &query_terms, LexicalQueryOrigin::UserQuery),
        query_terms
    );
    for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
        assert_identity_score_lane_uses_bound_identity_terms(sql);
    }
}

/// Fragments every chunk lexical lane must carry to exclude superseded
/// revisions before its local limit.
const CHUNK_LEXICAL_STALE_REVISION_FRAGMENTS: &[&str] = &[
    "join knowledge_structured_revision sr",
    "sr.revision_id = current_revision.revision_id",
    "sr.document_id = d.document_id",
    "sr.workspace_id = d.workspace_id",
    "sr.library_id = d.library_id",
    "sr.preparation_state = 'prepared'",
    "join content_current_revision current_revision",
    "d.deleted_at is null",
];

/// How often each lane-correlation fragment must appear. The two title lanes
/// correlate a hydrated chunk row to its readable head; the text lane ranks
/// text_hits directly, which its own scan has already bounded to readable_docs
/// revisions. The identifier lane correlates through a semi-join now, binding
/// the chunk to that one document's readable head rather than to the set of
/// every readable head in the library. The readable-doc, exact-title and
/// soft-title candidates each filter unreadable/deleted documents before their
/// local limits, so those gates appear three times.
const CHUNK_LEXICAL_LANE_OCCURRENCES: &[(&str, usize)] = &[
    ("join readable_docs title_readable", 2),
    ("title_readable.workspace_id = d.workspace_id", 2),
    ("title_readable.library_id = d.library_id", 2),
    ("current_revision.revision_id as readable_revision_id", 1),
    ("join readable_docs rd", 2),
    ("join readable_docs identifier_doc", 1),
    ("rd.document_id = c.document_id", 2),
    ("rd.workspace_id = c.workspace_id", 2),
    ("rd.library_id = c.library_id", 2),
    ("rd.readable_revision_id = c.revision_id", 2),
    ("c.document_id = identifier_doc.document_id", 1),
    ("c.workspace_id = identifier_doc.workspace_id", 1),
    ("c.revision_id = identifier_doc.readable_revision_id", 1),
    ("join content_current_revision current_revision", 1),
    ("and d.deleted_at is null", 3),
];

fn assert_chunk_lexical_gates_stale_revisions(sql: &str) {
    for fragment in CHUNK_LEXICAL_STALE_REVISION_FRAGMENTS {
        assert!(sql.contains(*fragment), "chunk lexical SQL is missing {fragment:?}");
    }
    for (fragment, expected) in CHUNK_LEXICAL_LANE_OCCURRENCES {
        assert_eq!(
            sql.matches(*fragment).count(),
            *expected,
            "chunk lexical SQL occurrence of {fragment:?}",
        );
    }
    assert!(!sql.contains("preparation_state = 'preparing'"));
}

#[test]
fn chunk_lexical_sql_filters_stale_revisions_before_every_lane_limit() {
    for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
        assert_chunk_lexical_gates_stale_revisions(sql);
    }
}

#[test]
fn chunk_lexical_sql_text_ranking_does_not_rejoin_readable_docs() {
    for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
        let body = sql
            .split("text_hits_per_document as (")
            .nth(1)
            .expect("the text ranking CTE is present")
            .split("title_identity_raw as (")
            .next()
            .expect("the text ranking CTE terminates");
        let body = compact_sql(body);
        assert!(body.contains("from text_hits text_hit"));
        assert!(
            !body.contains("join readable_docs"),
            "text_hits is already bounded to readable_docs revisions at the scan, so \
             re-correlating each hit against the unindexed materialized CTE drops nothing",
        );
        assert!(compact_sql(sql).contains(
            "c.revision_id = any ( array(select rd.readable_revision_id from readable_docs rd) )"
        ));
    }
}

/// Gate shared by the structured-block and technical-fact lexical SQL: the
/// readable-document/prepared-revision filters must run before the bounded
/// result set, and no lane may read `preparing` revisions.
fn assert_lexical_readable_revision_gate(sql: &str, fragments: &[&str], limit_clause: &str) {
    for fragment in fragments {
        assert!(sql.contains(*fragment), "lexical SQL is missing {fragment:?}");
    }
    assert!(
        sql.find("sr.preparation_state = 'prepared'").unwrap() < sql.rfind(limit_clause).unwrap(),
        "readable document filters must run before the bounded result set",
    );
    assert!(!sql.contains("preparation_state = 'preparing'"));
}

const STRUCTURED_BLOCK_LEXICAL_FRAGMENTS: &[&str] = &[
    "from knowledge_structured_block b",
    "join knowledge_document d",
    "d.document_id = b.document_id",
    "d.workspace_id = b.workspace_id",
    "d.library_id = b.library_id",
    "current_revision.revision_id = b.revision_id",
    "d.document_state = 'active'",
    "d.deleted_at is null",
    "join knowledge_structured_revision sr",
    "sr.revision_id = b.revision_id",
    "sr.document_id = b.document_id",
    "sr.workspace_id = b.workspace_id",
    "sr.library_id = b.library_id",
    "sr.preparation_state = 'prepared'",
];

#[test]
fn structured_block_lexical_sql_filters_nonreadable_revisions_before_limit() {
    let (exact_sql, prefix_sql) = structured_block_lexical_sql();
    for sql in [&exact_sql, &prefix_sql] {
        assert_lexical_readable_revision_gate(sql, STRUCTURED_BLOCK_LEXICAL_FRAGMENTS, "limit $3");
    }
}

const TECHNICAL_FACT_LEXICAL_FRAGMENTS: &[&str] = &[
    "from knowledge_technical_fact f",
    "f.\u{63}anonical_value_text as normalized_value_text",
    "f.\u{63}anonical_value_exact = $3",
    "join knowledge_document d",
    "d.document_id = f.document_id",
    "d.workspace_id = f.workspace_id",
    "d.library_id = f.library_id",
    "current_revision.revision_id = f.revision_id",
    "d.document_state = 'active'",
    "d.deleted_at is null",
    "join knowledge_structured_revision sr",
    "sr.revision_id = f.revision_id",
    "sr.document_id = f.document_id",
    "sr.workspace_id = f.workspace_id",
    "sr.library_id = f.library_id",
    "sr.preparation_state = 'prepared'",
];

#[test]
fn technical_fact_lexical_sql_filters_nonreadable_revisions_before_limit() {
    let (exact_sql, prefix_sql) = technical_fact_lexical_sql();
    for sql in [&exact_sql, &prefix_sql] {
        assert_lexical_readable_revision_gate(sql, TECHNICAL_FACT_LEXICAL_FRAGMENTS, "limit $4");
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

fn named_parenthesized_bodies<'a>(sql: &'a str, name: &str) -> Vec<&'a str> {
    let marker = format!("{name} as materialized (");
    let mut bodies = Vec::new();
    let mut search_start = 0_usize;
    while let Some(relative_start) = sql[search_start..].find(&marker) {
        let marker_start = search_start + relative_start;
        let open = marker_start + marker.len() - 1;
        let mut depth = 0_usize;
        let mut close = None;
        for (offset, ch) in sql[open..].char_indices() {
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth = depth.checked_sub(1).expect("lexical CTE parentheses must balance");
                    if depth == 0 {
                        close = Some(open + offset);
                        break;
                    }
                }
                _ => {}
            }
        }
        let close = close.expect("materialized lexical CTE must have a closing parenthesis");
        bodies.push(&sql[open + 1..close]);
        search_start = close + 1;
    }
    bodies
}

fn compact_sql(sql: &str) -> String {
    sql.split_whitespace().collect::<Vec<_>>().join(" ")
}

const REVISION_CANDIDATE_FORBIDDEN_FRAGMENTS: &[&str] = &[
    "library_id",
    "document_id",
    "workspace_id",
    "chunk_state",
    "search_tsv",
    "content_text",
    "normalized_text",
];

/// Occurrences the revision-first lanes must keep: four bounded laterals, two
/// primary-key hydrations of a revision's chunks, and two per-lane `limit 1`
/// probes.
const REVISION_FIRST_LANE_OCCURRENCES: &[(&str, usize)] = &[
    ("cross join lateral (", 4),
    ("where hydrated_candidate.chunk_id = candidate.chunk_id", 2),
    ("select hydrated_candidate.*", 2),
    ("limit 1\n                    ) c", 2),
];

const REVISION_FIRST_LANE_FRAGMENTS: &[&str] = &[
    "selected_ids as materialized (",
    "join knowledge_chunk hydrated on hydrated.chunk_id = selected.chunk_id",
];

fn assert_revision_candidates_are_id_only(sql: &str) {
    let candidates = named_parenthesized_bodies(sql, "revision_candidate_ids");
    assert_eq!(
        candidates.len(),
        2,
        "only the identity-title and soft-title lanes rank a revision's chunks; \
         both FTS lanes drive from the search index instead",
    );
    for candidate in candidates {
        let compact = compact_sql(candidate);
        assert!(
            compact.starts_with("select c.chunk_id from knowledge_chunk c where c.revision_id = ")
        );
        for forbidden in REVISION_CANDIDATE_FORBIDDEN_FRAGMENTS {
            assert!(
                !compact.contains(*forbidden),
                "revision-index producer leaked {forbidden}: {compact}",
            );
        }
    }
}

fn assert_revision_first_lanes_are_bounded(sql: &str) {
    for (fragment, expected) in REVISION_FIRST_LANE_OCCURRENCES {
        assert_eq!(
            sql.matches(*fragment).count(),
            *expected,
            "revision-first lane occurrence of {fragment:?}",
        );
    }
    for fragment in REVISION_FIRST_LANE_FRAGMENTS {
        assert!(sql.contains(*fragment), "revision-first SQL is missing {fragment:?}");
    }
    assert!(!sql.contains("join knowledge_chunk c on c.chunk_id = candidate.chunk_id"));

    let selected = compact_sql(named_parenthesized_bodies(sql, "selected_ids")[0]);
    assert!(!selected.contains("content_text"));
    assert!(!selected.contains("normalized_text"));
    assert!(selected.contains("order by score desc, chunk_id asc limit $10"));
}

#[test]
fn revision_first_lexical_candidates_are_id_only_and_branch_local() {
    for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
        assert_revision_candidates_are_id_only(sql);
        assert_revision_first_lanes_are_bounded(sql);
    }
}

/// Defect class: a lane that filters on `search_tsv` but reads
/// `knowledge_chunk` through a per-revision candidate enumeration plus a
/// primary-key hydration lateral. That shape applies the text predicate
/// *after* the row is fetched, so `knowledge_chunk_search_tsv_gin` is
/// invisible to the planner: a 250-document library hydrated ~9.5k chunk rows
/// (~75k buffer touches) per pass to return dozens of matches.
///
/// Both text-matching lanes must therefore reference `search_tsv` only where
/// the scan on `knowledge_chunk` itself can use the index. The title lanes are
/// exempt by construction — they rank a matched document's chunks by position
/// and never constrain `search_tsv`, so they are asserted to keep no text
/// predicate rather than being excluded by name.
#[test]
fn text_matching_lanes_never_filter_search_tsv_after_a_primary_key_hydration() {
    for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
        for lateral in named_parenthesized_bodies(sql, "revision_candidate_ids") {
            assert!(
                !compact_sql(lateral).contains("search_tsv"),
                "revision-enumerating lane must not carry a text predicate",
            );
        }
        for (lane, hydrating) in
            [("text_hits", "hydrated_candidate"), ("text_hits", "revision_candidate_ids")]
        {
            let body = compact_sql(named_parenthesized_bodies(sql, lane)[0]);
            assert!(body.contains("search_tsv @@"), "{lane} must keep its text predicate");
            assert!(
                !body.contains(hydrating),
                "{lane} must not reach knowledge_chunk through {hydrating}",
            );
            assert!(body.contains("from knowledge_chunk c where c.search_tsv @@"));
            // The search-index scan must stay bounded to readable heads. Without
            // this the GIN lane also matches superseded revisions: on a library
            // whose readable heads hold ~9k of ~356k chunks, a high-frequency
            // lexeme scored ~40x the needed rows and cost 10x the pre-change
            // buffers, turning the win into a regression.
            assert!(body.contains(
                "c.revision_id = any ( array(select rd.readable_revision_id from readable_docs rd) )"
            ));
        }
        let identifier = compact_sql(
            sql.split("exact_identifier_document_matches as (")
                .nth(1)
                .expect("identifier lane is present")
                .split("exact_identifier_document_frequency as (")
                .next()
                .expect("identifier lane terminates"),
        );
        assert!(identifier.contains("from knowledge_chunk c where c.library_id = $1"));
        assert!(identifier.contains("c.search_tsv @@"));
        // Bounded to readable heads, now per document rather than by matching
        // against the array of every readable head in the library.
        assert!(identifier.contains("c.revision_id = identifier_doc.readable_revision_id"));
        assert!(
            !identifier.contains("array(select rd.readable_revision_id from readable_docs rd)"),
            "the identifier lane must bind one head per document, not the whole set",
        );
        assert!(!identifier.contains("hydrated_candidate"));
        assert!(!identifier.contains("revision_candidate_ids"));
    }
}

#[test]
fn revision_first_lexical_limits_preserve_original_global_and_per_document_bounds() {
    for sql in [CHUNK_LEXICAL_SQL_EXACT.as_str(), CHUNK_LEXICAL_SQL_PREFIX.as_str()] {
        assert!(sql.contains("exact_identifier_document_matches as ("));
        // One row per (readable document, term). The pre-index shape produced
        // that through a `limit 1` lateral probe per document, then a `distinct`
        // over every matching chunk; the semi-join gets it structurally, which
        // is what lets the scan stop at a document's first matching chunk.
        assert!(sql.contains("select identifier_doc.document_id, term.value"));
        assert!(sql.contains("cross join readable_docs identifier_doc"));
        assert!(
            !sql.contains("select distinct identifier_doc.document_id"),
            "de-duplicating matched chunks re-reads chunk bodies that change no row",
        );
        assert!(sql.contains("text_raw as ("));
        // The per-document bound that the old per-revision lateral expressed as
        // `limit $3` is now a rank filter over the same ordering.
        assert!(sql.contains(
            "partition by text_hit.document_id\n                           order by text_hit.score desc, text_hit.chunk_id asc"
        ));
        assert!(sql.contains("where text_hit.per_revision_rank <= $3"));
        assert!(sql.contains(
            "order by text_hit.score desc, text_hit.chunk_id asc\n                 limit $3"
        ));
        assert_eq!(
            sql.matches(
                "order by score desc, c.revision_id desc,\n                             c.chunk_index asc, c.chunk_id asc\n                     limit 2",
            )
            .count(),
            2,
        );
        assert!(sql.contains("where $9::boolean"));
    }
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

/// A one-token query renders the same tsquery body for both relaxed rungs
/// ("'updat':*" has no operator to differ on), so the second rung would
/// re-issue the first one's statement with identical parameters and the
/// ladder's dedup would then discard every row it returned. Measured on a
/// 10-turn wave: 164 of 340 relaxed executions were such duplicate pairs.
#[tokio::test]
async fn ladder_never_repeats_an_identical_relaxed_body() {
    assert_eq!(prefix_relaxed_tsquery_and("update"), prefix_relaxed_tsquery_or("update"));

    let bodies = std::cell::RefCell::new(Vec::<String>::new());
    let rows: Vec<u32> = run_lexical_ladder(
        "update",
        "EXACT",
        "PREFIX",
        |row: &u32| *row,
        |sql, fts| {
            if sql == "PREFIX" {
                bodies.borrow_mut().push(fts);
            }
            async move { Ok(vec![1_u32]) }
        },
    )
    .await
    .unwrap();

    assert_eq!(bodies.borrow().len(), 1, "the repeated relaxed rung must be skipped");
    assert_eq!(rows, vec![1_u32], "the surviving rows are unchanged");
}
