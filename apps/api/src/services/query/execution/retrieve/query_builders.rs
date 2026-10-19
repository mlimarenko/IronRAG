use super::{
    BTreeSet, MAX_GRAPH_EVIDENCE_DB_TEXT_QUERIES, QueryIR, RuntimeMatchedChunk, RuntimeQueryPlan,
    SETUP_FOCUS_DOCUMENT_CHUNK_CAP, Uuid, repositories, request_safe_query,
    strip_leading_question_marker, technical_literal_focus_keyword_segments,
};

/// Hard cap on the number of lexical searches dispatched per query. Every
/// additional query is a full `search_chunks` round-trip; with a ~500 ms p50 per query and a
/// 1000+ document corpus, a 10-query fan-out added 5–8 s of
/// retrieval latency even when every query returned zero hits.
/// Eight is the empirical sweet spot: enough to carry multiple focus
/// segments through the lexical path when vector search might miss, while
/// the bounded four-wide fan-out keeps wall-clock inside the coordinator's
/// budget without allowing one turn to monopolize the pool. Anything above 8 returned diminishing
/// recall for order-of-magnitude more latency.
pub(super) const MAX_LEXICAL_QUERIES: usize = 8;
pub(super) const MAX_CONCURRENT_LEXICAL_QUERIES: usize = 4;
pub(super) const MAX_GRAPH_EVIDENCE_TEXT_QUERIES: usize = 8;

/// Maximum number of chunks from a single document the retriever is
/// allowed to surface in its final hit list. Two chunks (typically one
/// for context + one for the actual answer) gives the answer model
/// enough signal while preserving top-k diversity. Higher caps let a
/// single over-tokenised document drown out every other candidate.
pub(super) const MAX_CHUNKS_PER_DOCUMENT: usize = 2;

/// Caps the number of chunks from any single `document_id` in a
/// retrieval result. Preserves the input order (which reflects the
/// caller's merged score ranking): walks the list, admits each chunk
/// only if its document has fewer than `max_per_doc` chunks already
/// admitted. Keeps all single-document results if one only has < N
/// chunks (no silent drop of legitimate results).
pub(super) fn diversify_chunks_by_document(
    chunks: Vec<RuntimeMatchedChunk>,
    max_per_doc: usize,
    setup_focus_document_ids: &BTreeSet<Uuid>,
) -> Vec<RuntimeMatchedChunk> {
    if max_per_doc == 0 {
        return chunks;
    }
    let mut counts: std::collections::HashMap<Uuid, usize> =
        std::collections::HashMap::with_capacity(chunks.len());
    let mut out = Vec::with_capacity(chunks.len());
    let mut setup_focus_count = 0usize;
    for chunk in chunks {
        let setup_focus_document = setup_focus_document_ids.contains(&chunk.document_id);
        let document_cap = if setup_focus_document {
            SETUP_FOCUS_DOCUMENT_CHUNK_CAP.max(max_per_doc)
        } else {
            max_per_doc
        };
        if setup_focus_document && setup_focus_count >= SETUP_FOCUS_DOCUMENT_CHUNK_CAP {
            continue;
        }
        let count = counts.entry(chunk.document_id).or_insert(0);
        if *count >= document_cap {
            continue;
        }
        *count += 1;
        if setup_focus_document {
            setup_focus_count = setup_focus_count.saturating_add(1);
        }
        out.push(chunk);
    }
    out
}

pub(crate) fn build_lexical_queries(
    question: &str,
    plan: &RuntimeQueryPlan,
    query_ir_focus_queries: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();

    let mut push_query = |value: String, queries: &mut Vec<String>| {
        if queries.len() >= MAX_LEXICAL_QUERIES {
            return;
        }
        let normalized = value.trim().to_string();
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            return;
        }
        queries.push(normalized);
    };

    // Priority 1 — the raw user question. The full-text analyzer already
    // splits it into relevant tokens; this is the
    // highest-signal query and must always go first.
    let retrieval_question = strip_leading_question_marker(question);
    push_query(retrieval_question.to_string(), &mut queries);

    // Priority 2 — IR focus spans. The compiler has already isolated
    // the entities and literals the user cares about, so these short
    // queries rescue rare exact tokens that a broad sentence-level BM25
    // query can drown in common words.
    for focus in query_ir_focus_queries {
        push_query(focus.clone(), &mut queries);
    }

    // Priority 3 — the plan's combined hi + lo keyword phrase.
    push_query(request_safe_query(plan), &mut queries);

    // Priority 4 — for exact-literal technical queries (port numbers,
    // error codes, config keys), push focus segments for narrow recall.
    // The focus-keyword helper is still useful when the compiler did
    // not emit typed literal constraints: it degrades to structural
    // token segments without hard-coded vocabulary.
    if plan.intent_profile.exact_literal_technical {
        for segment in technical_literal_focus_keyword_segments(retrieval_question, query_ir) {
            push_query(segment.join(" "), &mut queries);
        }
    }

    // Priority 5 — plan-derived keyword variants. Hi/lo splits first
    // (they collapse the user's question to normalized nouns),
    // then individual keywords for narrow-tail recall on corpora
    // where the vector space is sparse.
    if !plan.high_level_keywords.is_empty() {
        push_query(plan.high_level_keywords.join(" "), &mut queries);
    }
    if !plan.low_level_keywords.is_empty() {
        push_query(plan.low_level_keywords.join(" "), &mut queries);
    }
    if plan.keywords.len() > 1 {
        push_query(plan.keywords.join(" "), &mut queries);
    }
    for keyword in plan.keywords.iter().take(MAX_LEXICAL_QUERIES) {
        push_query(keyword.clone(), &mut queries);
    }

    queries
}

pub(crate) fn build_graph_evidence_text_queries(
    question: &str,
    plan: &RuntimeQueryPlan,
    query_ir_focus_queries: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();

    let mut push_query = |value: String, queries: &mut Vec<String>| {
        if queries.len() >= MAX_GRAPH_EVIDENCE_TEXT_QUERIES {
            return;
        }
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        queries.push(normalized);
    };

    // Graph evidence text lookup is backed by Postgres full-text/trigram
    // indexes over the activated evidence table. Keep the first compiler
    // focus spans ahead of the broad prose question, but keep the raw
    // question inside the bounded DB-facing budget as the primary recall
    // fallback when the compiler produced narrow focus spans.
    for focus in query_ir_focus_queries.iter().take(3) {
        push_query(focus.clone(), &mut queries);
    }
    let retrieval_question = strip_leading_question_marker(question);
    push_query(retrieval_question.to_string(), &mut queries);
    for focus in query_ir_focus_queries.iter().skip(3) {
        push_query(focus.clone(), &mut queries);
    }

    if plan.intent_profile.exact_literal_technical {
        for segment in technical_literal_focus_keyword_segments(retrieval_question, query_ir) {
            push_query(segment.join(" "), &mut queries);
        }
    }

    push_query(request_safe_query(plan), &mut queries);
    if !plan.high_level_keywords.is_empty() {
        push_query(plan.high_level_keywords.join(" "), &mut queries);
    }
    if !plan.low_level_keywords.is_empty() {
        push_query(plan.low_level_keywords.join(" "), &mut queries);
    }
    if plan.keywords.len() > 1 {
        push_query(plan.keywords.join(" "), &mut queries);
    }
    queries
}

pub(crate) fn graph_evidence_db_text_queries(
    text_queries: &[String],
    query_ir: Option<&QueryIR>,
) -> Vec<repositories::RuntimeGraphEvidenceSearchQuery> {
    let literal_or_formal_keys = query_ir
        .into_iter()
        .flat_map(|query_ir| {
            query_ir
                .literal_constraints
                .iter()
                .map(|literal| literal.text.as_str())
                .chain(query_ir.target_entities.iter().map(|entity| entity.label.as_str()))
                .chain(query_ir.document_focus.iter().map(|focus| focus.hint.as_str()))
        })
        .map(|query| query.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase())
        .collect::<BTreeSet<_>>();

    text_queries
        .iter()
        .take(MAX_GRAPH_EVIDENCE_DB_TEXT_QUERIES)
        .map(|query| {
            let normalized = query.split_whitespace().collect::<Vec<_>>().join(" ");
            if literal_or_formal_keys.contains(&normalized.to_lowercase()) {
                repositories::RuntimeGraphEvidenceSearchQuery::LiteralOrFormal(normalized)
            } else {
                repositories::RuntimeGraphEvidenceSearchQuery::Lexical(normalized)
            }
        })
        .collect()
}
