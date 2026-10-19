use super::coordination::SHORT_GRAPH_WRITE_GUARD_IDLE_TIMEOUT_SQL;
use super::entity_search::{
    runtime_graph_entity_search_includes_term_branch, runtime_graph_entity_search_sql,
    runtime_graph_entity_search_terms, runtime_graph_entity_wildcard_prefixes,
};
use super::evidence_search::{
    RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP, runtime_graph_evidence_document_filter,
    runtime_graph_evidence_literal_search_queries,
    runtime_graph_evidence_per_query_candidate_limit, runtime_graph_evidence_text_search_queries,
    runtime_graph_evidence_text_search_token_prefix, runtime_graph_evidence_text_search_tokens,
};
use super::node_operations::{RUNTIME_GRAPH_LABEL_MAX_BYTES, clamp_runtime_graph_label};
use super::types::RuntimeGraphEvidenceSearchQuery;
use uuid::Uuid;

#[test]
fn short_graph_write_guard_arms_a_one_minute_idle_timeout() {
    // A short graph-artifact write must self-release within one minute when the
    // owning task is stranded between statements, or its library/attempt locks
    // stall every graph-refresh worker. The guard therefore arms a session-local
    // *idle* timeout (never a statement or lock timeout, which would abort an
    // active, still-progressing write) before it takes any authority lock.
    let sql = SHORT_GRAPH_WRITE_GUARD_IDLE_TIMEOUT_SQL.to_ascii_lowercase();
    assert!(sql.starts_with("set local "), "timeout must be session-local, not global");
    assert!(sql.contains("idle_in_transaction_session_timeout"), "must bound idle time");
    assert!(sql.contains("'60s'"), "idle budget must stay one minute");
    assert!(!sql.contains("statement_timeout"), "an active write must not be cut short");
    assert!(!sql.contains("lock_timeout"), "lock acquisition must not be abandoned");
}

fn lexical_evidence_query(value: &str) -> RuntimeGraphEvidenceSearchQuery {
    RuntimeGraphEvidenceSearchQuery::Lexical(value.to_string())
}

fn literal_evidence_query(value: &str) -> RuntimeGraphEvidenceSearchQuery {
    RuntimeGraphEvidenceSearchQuery::LiteralOrFormal(value.to_string())
}

#[test]
fn clamp_runtime_graph_label_leaves_short_labels_untouched() {
    let label = "Alpha Suite payment module";
    assert_eq!(clamp_runtime_graph_label(label), label);
}

#[test]
fn clamp_runtime_graph_label_bounds_oversized_labels_by_bytes() {
    // A ~1.6 KB ASCII "label" — the extraction-noise shape that overflowed
    // the btree index and aborted whole-library restores (R4).
    let oversized = "x".repeat(1600 * 4);
    let clamped = clamp_runtime_graph_label(&oversized);
    assert!(clamped.len() <= RUNTIME_GRAPH_LABEL_MAX_BYTES);
    assert!(oversized.starts_with(clamped));
}

#[test]
fn clamp_runtime_graph_label_never_splits_a_codepoint() {
    // Each codepoint here is multi-byte; the cap must fall back to a char
    // boundary rather than slice mid-codepoint (which would panic / corrupt).
    // Script-agnostic: we assert structural validity, not any language.
    let multibyte = "\u{1F680}".repeat(RUNTIME_GRAPH_LABEL_MAX_BYTES); // 4 bytes each
    let clamped = clamp_runtime_graph_label(&multibyte);
    assert!(clamped.len() <= RUNTIME_GRAPH_LABEL_MAX_BYTES);
    // Re-borrowing as &str proves we landed on a valid UTF-8 boundary.
    assert!(clamped.chars().all(|ch| ch == '\u{1F680}'));
}

#[test]
fn evidence_text_search_tokens_keep_structural_literals_without_language_lists() {
    let tokens = runtime_graph_evidence_text_search_tokens(
        "Open alpha/report://needle?fontSize=12 and alpha.port=9407",
    );

    assert_eq!(
        tokens,
        vec![
            "open".to_string(),
            "alpha".to_string(),
            "report".to_string(),
            "needle".to_string(),
            "fontsize".to_string(),
            "12".to_string(),
            "port".to_string(),
            "9407".to_string(),
        ],
    );
}

#[test]
fn entity_search_terms_keep_structural_needles_without_language_lists() {
    let terms =
        runtime_graph_entity_search_terms("Alpha/report://needle?fontSize=12 alpha alpha 80");

    assert_eq!(
        terms,
        vec![
            "alpha".to_string(),
            "report".to_string(),
            "needle".to_string(),
            "fontsize".to_string(),
            "12".to_string(),
            "80".to_string(),
        ],
    );
}

#[test]
fn entity_wildcard_prefixes_keep_literal_prefix_semantics() {
    let prefixes = runtime_graph_entity_wildcard_prefixes("show alpha-* and beta_module* entries");

    assert_eq!(prefixes, vec!["alpha-".to_string(), "beta~_module".to_string()]);
}

#[test]
fn entity_search_omits_the_term_branch_only_when_it_cannot_contribute() {
    let single_token = vec!["harness".to_string()];
    assert!(!runtime_graph_entity_search_includes_term_branch("harness", &single_token));

    let multi_token = vec!["agent".to_string(), "smith".to_string()];
    assert!(runtime_graph_entity_search_includes_term_branch("agent-smith", &multi_token));

    // A token that differs from the whole normalized query keeps the branch:
    // tokenization drops trailing punctuation, so only the rank-6 branch would
    // otherwise see the `agent` term inside `agent-`.
    assert!(runtime_graph_entity_search_includes_term_branch("agent-", &["agent".to_string()],));

    let with_term_branch = runtime_graph_entity_search_sql(true);
    let without_term_branch = runtime_graph_entity_search_sql(false);
    assert!(with_term_branch.contains(") label_terms"));
    assert!(!without_term_branch.contains("label_terms"));
    assert!(without_term_branch.contains(") alias_phrase"));
    assert!(without_term_branch.contains("group by id"));
    assert!(without_term_branch.contains("limit $5"));
}

#[test]
fn evidence_text_search_query_uses_selective_suffix_tolerant_windows() {
    let queries = runtime_graph_evidence_text_search_queries(&[
        lexical_evidence_query("Which parameter links Alpha Module to control service?"),
        lexical_evidence_query("Alpha Module"),
        lexical_evidence_query("Alpha"),
        lexical_evidence_query("port 9407"),
        lexical_evidence_query("Which parameter links Alpha Module to control service?"),
    ]);

    assert_eq!(
        queries.first().map(String::as_str),
        Some("'paramet':* & 'modul':* & 'contr':* & 'servi':*"),
    );
    assert_eq!(queries.get(1).map(String::as_str), Some("'port':* & '9407'"));
    assert!(!queries.contains(&"'alph':* & 'modul':*".to_string()));
    assert!(queries.contains(&"'port':* & '9407'".to_string()));
    assert!(!queries.iter().any(|query| {
        query.contains("'which':* & 'paramet':* & 'link':* & 'alph':* & 'modul':* & 'contr':*")
    }));
    assert!(queries.len() <= RUNTIME_GRAPH_EVIDENCE_TEXT_SEARCH_QUERY_CAP);
}

#[test]
fn evidence_literal_search_queries_keep_exact_structural_spans() {
    let queries = runtime_graph_evidence_literal_search_queries(&[
        lexical_evidence_query("Alpha"),
        literal_evidence_query("Mono Sans"),
        literal_evidence_query("alpha/report://needle?fontSize=12"),
        literal_evidence_query(
            "report://detail?out=display&title=Alpha%20Report%20%(shift.num[d])&font=Mono%20Sans&fontSize=12",
        ),
        literal_evidence_query("port 80"),
    ]);

    assert!(!queries.contains(&"alpha".to_string()));
    assert!(queries.contains(&"mono sans".to_string()));
    assert!(queries.contains(&"alpha/report://needle?fontsize=12".to_string()));
    assert!(queries.contains(
            &"report://detail?out=display&title=alpha%20report%20%(shift.num[d])&font=mono%20sans&fontsize=12".to_string()
        ));
    assert!(queries.contains(&"port 80".to_string()));
}

#[test]
fn evidence_literal_search_queries_require_typed_lane_not_name_casing() {
    let queries = runtime_graph_evidence_literal_search_queries(&[
        RuntimeGraphEvidenceSearchQuery::Lexical("Alpha Module".to_string()),
        RuntimeGraphEvidenceSearchQuery::Lexical("mIxEd CaSiNg".to_string()),
        RuntimeGraphEvidenceSearchQuery::LiteralOrFormal("caseless target".to_string()),
    ]);

    assert_eq!(queries, vec!["caseless target".to_string()]);
}

#[test]
fn evidence_literal_search_queries_ignore_lexical_prose_regardless_of_shape() {
    let queries = runtime_graph_evidence_literal_search_queries(&[
        lexical_evidence_query(
            "Find the configuration paragraph that explains how the terminal connects to the control service, include the source document, and keep this cache marker 2026-05-01.",
        ),
        lexical_evidence_query(
            "Which rare entity describes the escalation recipient, what fields are required in the message, and where is the source mentioned?",
        ),
        lexical_evidence_query("recent project"),
        lexical_evidence_query("meeting notes"),
        literal_evidence_query("Alpha Module"),
    ]);

    assert_eq!(queries, vec!["alpha module".to_string()]);
}

#[test]
fn graph_evidence_text_search_bounds_per_query_candidates_under_fanout() {
    assert_eq!(runtime_graph_evidence_per_query_candidate_limit(72, 0), 0);
    assert_eq!(runtime_graph_evidence_per_query_candidate_limit(72, 1), 72);
    assert_eq!(runtime_graph_evidence_per_query_candidate_limit(72, 2), 24);
    assert_eq!(runtime_graph_evidence_per_query_candidate_limit(12, 4), 12);
}

#[test]
fn evidence_text_search_token_prefix_preserves_numeric_literals() {
    assert_eq!(runtime_graph_evidence_text_search_token_prefix("alpha"), "alph");
    assert_eq!(runtime_graph_evidence_text_search_token_prefix("module"), "modul");
    assert_eq!(runtime_graph_evidence_text_search_token_prefix("alphacases"), "alphacas");
    assert_eq!(runtime_graph_evidence_text_search_token_prefix("9407"), "9407");
    assert_eq!(runtime_graph_evidence_text_search_token_prefix("build42"), "build42");
}

#[test]
fn evidence_text_search_tokens_keep_short_numeric_literals_exact() {
    let tokens = runtime_graph_evidence_text_search_tokens("port 80 status 404 build42");
    let queries = runtime_graph_evidence_text_search_queries(&[lexical_evidence_query("port 80")]);

    assert_eq!(
        tokens,
        vec![
            "port".to_string(),
            "80".to_string(),
            "status".to_string(),
            "404".to_string(),
            "build42".to_string(),
        ],
    );
    assert_eq!(queries, vec!["'port':* & '80'".to_string()]);
}

#[test]
fn evidence_text_search_query_includes_short_needle_windows() {
    let queries = runtime_graph_evidence_text_search_queries(&[lexical_evidence_query(
        "alphacases betagamma deltazeta epsilonkey zetaport thetakey",
    )]);

    assert!(queries.iter().any(|query| query.contains("'deltaze':* & 'epsilonk':*")));
    assert!(!queries.contains(&"'alphacas':* & 'betagam':*".to_string()));
}

#[test]
fn evidence_text_search_query_expands_short_phrases_with_subwindows() {
    let queries = runtime_graph_evidence_text_search_queries(&[lexical_evidence_query(
        "alphacases betagamma deltazeta epsilonkey",
    )]);

    assert_eq!(
        queries.first().map(String::as_str),
        Some("'alphacas':* & 'betagam':* & 'deltaze':* & 'epsilonk':*"),
    );
    assert!(queries.contains(&"'betagam':* & 'deltaze':* & 'epsilonk':*".to_string()));
    assert!(!queries.contains(&"'betagam':* & 'deltaze':*".to_string()));
}

#[test]
fn evidence_document_filter_keeps_the_match_predicate_on_the_gin_index() {
    assert_eq!(runtime_graph_evidence_document_filter(&[]), None);
    assert_eq!(
        runtime_graph_evidence_document_filter(&[Uuid::nil()]),
        Some(" and (evidence.document_id = any($6::uuid[])) is true"),
    );
}
