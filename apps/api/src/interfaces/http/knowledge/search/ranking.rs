use super::*;

pub(super) fn sort_document_hits_by_relevance(document_hits: &mut [KnowledgeDocumentAccumulator]) {
    document_hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.document.document_id.cmp(&right.document.document_id))
    });
    promote_rare_identifier_declarations(document_hits, |candidate| {
        DocumentIdentifierRankSignals {
            score: candidate.score,
            exact_identifier_rarity: candidate.exact_identifier_rarity,
            declares_query_identifier: candidate.declares_query_identifier,
            declares_rare_exact_identifier: candidate.declares_rare_exact_identifier,
        }
    });
}

pub(super) fn promote_rare_identifier_declarations<T>(
    candidates: &mut [T],
    signals_for: impl Fn(&T) -> DocumentIdentifierRankSignals,
) {
    for initial_index in 1..candidates.len() {
        let mut index = initial_index;
        while index > 0 {
            let higher = signals_for(&candidates[index - 1]);
            let challenger = signals_for(&candidates[index]);
            if !rare_identifier_declaration_may_precede(higher, challenger) {
                break;
            }
            candidates.swap(index - 1, index);
            index -= 1;
        }
    }
}

pub(super) fn rare_identifier_declaration_may_precede(
    higher: DocumentIdentifierRankSignals,
    challenger: DocumentIdentifierRankSignals,
) -> bool {
    higher.score.is_finite()
        && challenger.score.is_finite()
        && higher.score > 0.0
        && challenger.score > 0.0
        && challenger.score <= higher.score
        && !higher.declares_query_identifier
        && challenger.declares_rare_exact_identifier
        && has_strictly_rare_identifier(challenger.exact_identifier_rarity)
}

pub(super) fn has_strictly_rare_identifier(rarity: f64) -> bool {
    rarity.is_finite() && rarity >= DOCUMENT_EXACT_IDENTIFIER_RARITY_MIN
}

pub(super) fn extend_unique_identifier_terms(target: &mut Vec<String>, source: &[String]) {
    for term in source {
        if !target.contains(term) {
            target.push(term.clone());
        }
    }
}

pub(super) fn document_declares_rare_exact_identifier(
    accumulator: &KnowledgeDocumentAccumulator,
) -> bool {
    chunk_hits_declare_rare_exact_identifier(
        accumulator.exact_identifier_rarity,
        &accumulator.rare_exact_identifier_terms,
        &accumulator.chunk_hits,
    )
}

pub(super) fn chunk_hits_declare_identifier(
    identifier_terms: &[String],
    chunk_hits: &[KnowledgeChunkSearchRow],
) -> bool {
    !identifier_terms.is_empty()
        && chunk_hits
            .iter()
            .any(|hit| content_declares_any_identifier(&hit.content_text, identifier_terms))
}

pub(super) fn chunk_hits_declare_rare_exact_identifier(
    rarity: f64,
    rare_identifier_terms: &[String],
    chunk_hits: &[KnowledgeChunkSearchRow],
) -> bool {
    has_strictly_rare_identifier(rarity)
        && chunk_hits_declare_identifier(rare_identifier_terms, chunk_hits)
}

pub(super) fn rescore_document_accumulator(
    accumulator: &mut KnowledgeDocumentAccumulator,
    query_keywords: &[String],
    query_text: &str,
) {
    accumulator.declares_query_identifier =
        chunk_hits_declare_identifier(&accumulator.exact_identifier_terms, &accumulator.chunk_hits);
    accumulator.declares_rare_exact_identifier =
        document_declares_rare_exact_identifier(accumulator);
    accumulator.score = score_document_accumulator(accumulator, query_keywords, query_text);
}

pub(super) fn sanitize_chunk_search_hit(hit: &KnowledgeChunkSearchRow) -> KnowledgeChunkSearchRow {
    let mut sanitized = hit.clone();
    sanitized.content_text = repair_technical_layout_noise(&sanitized.content_text);
    sanitized.normalized_text = repair_technical_layout_noise(&sanitized.normalized_text);
    sanitized.content_text = normalize_search_hit_text(&sanitized.content_text);
    sanitized.normalized_text = normalize_search_hit_text(&sanitized.normalized_text);
    sanitized
}

pub(super) fn normalize_search_hit_text(text: &str) -> String {
    text.to_string()
}

pub(super) fn document_search_keywords(query_text: &str) -> Vec<String> {
    crate::services::query::planner::extract_keywords(query_text)
}

pub(super) fn document_chunk_keyword_coverage(
    hit: &KnowledgeChunkSearchRow,
    keywords: &[String],
) -> usize {
    if keywords.is_empty() {
        return 0;
    }
    let haystack = format!("{}\n{}", hit.content_text, hit.normalized_text).to_lowercase();
    keywords.iter().filter(|keyword| haystack.contains(keyword.as_str())).count()
}

pub(super) fn document_keyword_coverage_bonus(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
) -> f64 {
    if chunk_hits.is_empty() || keywords.is_empty() {
        return 0.0;
    }
    let denominator = keywords.len() as f64;
    let best_chunk_coverage = chunk_hits
        .iter()
        .map(|hit| document_chunk_keyword_coverage(hit, keywords))
        .max()
        .unwrap_or_default() as f64;
    let aggregate_haystack = chunk_hits
        .iter()
        .take(3)
        .map(|hit| format!("{}\n{}", hit.content_text, hit.normalized_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let aggregate_coverage =
        keywords.iter().filter(|keyword| aggregate_haystack.contains(keyword.as_str())).count()
            as f64;
    let best_signal = (best_chunk_coverage / denominator).min(1.0);
    let aggregate_signal = (aggregate_coverage / denominator).min(1.0);
    (best_signal * DOCUMENT_KEYWORD_COVERAGE_BONUS_WEIGHT)
        + (aggregate_signal * DOCUMENT_KEYWORD_AGGREGATE_COVERAGE_BONUS_WEIGHT)
}

pub(super) fn document_anchor_match_bonus(
    chunk_hits: &[KnowledgeChunkSearchRow],
    query_text: &str,
) -> f64 {
    if chunk_hits.is_empty() {
        return 0.0;
    }
    let token_anchors = document_search_anchor_tokens(query_text);
    let surface_anchors = document_search_exact_anchor_surfaces(query_text);
    if token_anchors.is_empty() && surface_anchors.is_empty() {
        return 0.0;
    }
    let haystack = chunk_hits
        .iter()
        .map(|hit| format!("{}\n{}", hit.content_text, hit.normalized_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();

    let token_bonus = if token_anchors.is_empty() {
        0.0
    } else {
        let matches =
            token_anchors.iter().filter(|anchor| haystack.contains(anchor.as_str())).count() as f64;
        (matches / token_anchors.len() as f64).min(1.0) * DOCUMENT_ANCHOR_TOKEN_BONUS_WEIGHT
    };
    let surface_bonus = document_anchor_surface_coverage_bonus(&surface_anchors, &haystack);
    (token_bonus + surface_bonus).min(DOCUMENT_ANCHOR_MATCH_BONUS_MAX)
}

pub(super) fn document_anchor_surface_coverage_bonus(
    surface_anchors: &[String],
    haystack: &str,
) -> f64 {
    if surface_anchors.is_empty() {
        return 0.0;
    }
    let total_weight = surface_anchors
        .iter()
        .map(|anchor| document_anchor_surface_bonus_weight(anchor))
        .sum::<f64>();
    if total_weight <= 0.0 {
        return 0.0;
    }
    let matched_weight = surface_anchors
        .iter()
        .filter(|anchor| haystack.contains(anchor.as_str()))
        .map(|anchor| document_anchor_surface_bonus_weight(anchor))
        .sum::<f64>();
    if total_weight > DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT {
        let coverage = (matched_weight / total_weight).min(1.0);
        coverage.powi(DOCUMENT_ANCHOR_MULTI_SURFACE_EXPONENT)
            * DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT
    } else {
        matched_weight.min(DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT)
    }
}

pub(super) fn document_anchor_surface_bonus_weight(anchor: &str) -> f64 {
    if anchor.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@')) {
        DOCUMENT_ANCHOR_SURFACE_STRONG_BONUS_WEIGHT
    } else {
        DOCUMENT_ANCHOR_SURFACE_WEAK_BONUS_WEIGHT
    }
}

pub(super) fn document_identity_bonus(document: &KnowledgeDocumentRow, query_text: &str) -> f64 {
    let query_tokens =
        document_search_anchor_tokens(query_text).into_iter().collect::<HashSet<_>>();
    if query_tokens.is_empty() {
        return 0.0;
    }
    let query_phrases =
        document_search_anchor_phrases(query_text).into_iter().collect::<HashSet<_>>();
    document_identity_values(document)
        .into_iter()
        .map(|value| {
            let normalized_value = value.to_ascii_lowercase();
            let normalized_source_phrase = normalized_alnum_token_sequence_by(
                value,
                |token| token.chars().count() >= 2 || token.chars().any(|ch| ch.is_ascii_digit()),
                Some(64),
            )
            .join(" ");
            let identity_tokens = normalized_alnum_token_sequence_by(
                value,
                |token| token.chars().count() >= 4 || token.chars().any(|ch| ch.is_ascii_digit()),
                Some(32),
            )
            .into_iter()
            .collect::<HashSet<_>>();
            let token_overlap = identity_tokens.intersection(&query_tokens).count() as f64;
            let phrase_overlap = query_phrases
                .iter()
                .filter(|phrase| {
                    normalized_value.contains(phrase.as_str())
                        || normalized_source_phrase.contains(phrase.as_str())
                })
                .count() as f64;
            let exact_source_phrase_overlap = query_phrases
                .iter()
                .filter(|phrase| {
                    phrase.split_whitespace().count() >= 3
                        && normalized_source_phrase.contains(phrase.as_str())
                })
                .count() as f64;
            token_overlap * DOCUMENT_IDENTITY_TOKEN_BONUS_WEIGHT
                + phrase_overlap * DOCUMENT_IDENTITY_PHRASE_BONUS_WEIGHT
                + exact_source_phrase_overlap * DOCUMENT_IDENTITY_EXACT_SOURCE_PHRASE_BONUS_WEIGHT
        })
        .fold(0.0, f64::max)
}

pub(super) fn document_best_chunk_focus_bonus(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
    query_text: &str,
) -> f64 {
    let signals = document_best_chunk_focus_signals(chunk_hits, keywords, query_text);
    signals.anchor.mul_add(
        DOCUMENT_BEST_CHUNK_ANCHOR_COVERAGE_WEIGHT,
        signals.keyword * DOCUMENT_BEST_CHUNK_KEYWORD_COVERAGE_WEIGHT,
    )
}

pub(super) fn document_best_chunk_evidence_coverage(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
    query_text: &str,
) -> f64 {
    let signals = document_best_chunk_focus_signals(chunk_hits, keywords, query_text);
    signals.keyword.max(signals.anchor).clamp(0.0, 1.0)
}

#[derive(Debug, Clone, Copy, Default)]
pub(super) struct DocumentChunkFocusSignals {
    keyword: f64,
    anchor: f64,
}

pub(super) fn document_best_chunk_focus_signals(
    chunk_hits: &[KnowledgeChunkSearchRow],
    keywords: &[String],
    query_text: &str,
) -> DocumentChunkFocusSignals {
    if chunk_hits.is_empty() {
        return DocumentChunkFocusSignals::default();
    }
    let anchor_tokens = document_search_anchor_tokens(query_text);
    let anchor_phrases = document_search_anchor_phrases(query_text);
    let surface_anchors = document_search_exact_anchor_surfaces(query_text);
    chunk_hits
        .iter()
        .map(|hit| {
            let haystack = format!("{}\n{}", hit.content_text, hit.normalized_text).to_lowercase();
            let keyword_signal = if keywords.is_empty() {
                0.0
            } else {
                let matches =
                    keywords.iter().filter(|keyword| haystack.contains(keyword.as_str())).count();
                (matches as f64 / keywords.len() as f64).min(1.0)
            };
            let anchor_denominator =
                anchor_tokens.len() + anchor_phrases.len() + surface_anchors.len();
            let anchor_signal = if anchor_denominator == 0 {
                0.0
            } else {
                let token_matches =
                    anchor_tokens.iter().filter(|token| haystack.contains(token.as_str())).count();
                let phrase_matches = anchor_phrases
                    .iter()
                    .filter(|phrase| haystack.contains(phrase.as_str()))
                    .count();
                let surface_matches = surface_anchors
                    .iter()
                    .filter(|surface| haystack.contains(surface.as_str()))
                    .count();
                ((token_matches + phrase_matches + surface_matches) as f64
                    / anchor_denominator as f64)
                    .min(1.0)
            };
            DocumentChunkFocusSignals { keyword: keyword_signal, anchor: anchor_signal }
        })
        .fold(DocumentChunkFocusSignals::default(), |best, next| {
            if (next.keyword + next.anchor) > (best.keyword + best.anchor) { next } else { best }
        })
}

pub(super) fn document_vector_raw_score_bonus(vector_score: Option<f64>) -> f64 {
    vector_score.unwrap_or_default().clamp(0.0, 1.0) * DOCUMENT_VECTOR_RAW_SCORE_WEIGHT
}

pub(super) fn document_lexical_signal(
    lexical_score: Option<f64>,
    identity_bonus: f64,
    best_chunk_evidence_coverage: f64,
) -> f64 {
    let Some(lexical_score) = lexical_score else {
        return 0.0;
    };
    let base = lexical_score.ln_1p();
    if (DOCUMENT_SOFT_TITLE_SCORE_MIN..=DOCUMENT_SOFT_TITLE_SCORE_MAX).contains(&lexical_score)
        && identity_bonus < DOCUMENT_SOFT_TITLE_IDENTITY_PRESERVE_THRESHOLD
    {
        let evidence_coverage = best_chunk_evidence_coverage.clamp(0.0, 1.0);
        let scale = ((1.0 - DOCUMENT_SOFT_TITLE_LOW_EVIDENCE_MIN_SCALE) * evidence_coverage)
            .mul_add(evidence_coverage, DOCUMENT_SOFT_TITLE_LOW_EVIDENCE_MIN_SCALE);
        return base * scale;
    }
    base
}

pub(super) fn document_identity_values(document: &KnowledgeDocumentRow) -> Vec<&str> {
    [
        document.title.as_deref(),
        document.file_name.as_deref(),
        Some(document.external_key.as_str()),
        document.source_uri.as_deref(),
        document.document_hint.as_deref(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.trim().is_empty())
    .collect()
}

pub(super) fn resolved_evidence_sample_limit(value: Option<usize>) -> usize {
    value.unwrap_or(DEFAULT_EVIDENCE_SAMPLE_LIMIT)
}

pub(super) fn document_search_vector_limits(
    limit: usize,
    chunk_hit_limit_per_document: usize,
) -> (usize, usize) {
    let chunk_limit =
        limit.saturating_mul(chunk_hit_limit_per_document).max(limit.saturating_mul(2)).max(1);
    let entity_limit = limit.max(1);
    (chunk_limit, entity_limit)
}

pub(super) fn expand_document_search_queries(query_text: &str) -> Vec<String> {
    let mut queries = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    let mut push_query = |value: &str| {
        let normalized = value.trim();
        if normalized.is_empty() {
            return;
        }
        let dedupe_key = normalized.to_lowercase();
        if seen.insert(dedupe_key) {
            queries.push(normalized.to_string());
        }
    };

    push_query(query_text);
    for phrase in document_search_anchor_phrases(query_text).into_iter().take(6) {
        push_query(&phrase);
    }

    queries
}

/// Hybrid lexical + vector search over one library's knowledge — the single
/// primary search interface. `POST` carries the request because a free-text
/// query does not fit a URL query string safely or practically (the same
/// rationale as Elasticsearch `_search`-style endpoints); it is still a pure
pub(super) fn score_document_accumulator(
    accumulator: &KnowledgeDocumentAccumulator,
    query_keywords: &[String],
    query_text: &str,
) -> f64 {
    let lexical_rank = accumulator.lexical_rank.unwrap_or(usize::MAX / 2);
    let vector_rank = accumulator.vector_rank.unwrap_or(usize::MAX / 2);
    let vector_signal = accumulator.vector_score.map(f64::ln_1p).unwrap_or_default();
    let provenance_bonus = (accumulator.evidence_samples.len() as f64) / 1000.0;
    let keyword_coverage_bonus =
        document_keyword_coverage_bonus(&accumulator.chunk_hits, query_keywords);
    let anchor_match_bonus = document_anchor_match_bonus(&accumulator.chunk_hits, query_text);
    let best_chunk_focus_bonus =
        document_best_chunk_focus_bonus(&accumulator.chunk_hits, query_keywords, query_text);
    let vector_raw_score_bonus = document_vector_raw_score_bonus(accumulator.vector_score);
    let identity_bonus = document_identity_bonus(&accumulator.document, query_text);
    let best_chunk_evidence_coverage =
        document_best_chunk_evidence_coverage(&accumulator.chunk_hits, query_keywords, query_text);
    let lexical_signal = document_lexical_signal(
        accumulator.lexical_score,
        identity_bonus,
        best_chunk_evidence_coverage,
    );
    let pre_identifier_score = lexical_signal
        + vector_signal
        + (1.0 / (60.0 + lexical_rank as f64))
        + (1.0 / (60.0 + vector_rank as f64))
        + provenance_bonus
        + keyword_coverage_bonus
        + anchor_match_bonus
        + best_chunk_focus_bonus
        + vector_raw_score_bonus
        + identity_bonus;
    pre_identifier_score
        + document_exact_identifier_relevance_bonus(
            pre_identifier_score,
            accumulator.exact_identifier_rarity,
        )
}

pub(super) fn document_exact_identifier_relevance_bonus(pre_uplift_score: f64, rarity: f64) -> f64 {
    if !pre_uplift_score.is_finite()
        || pre_uplift_score <= 0.0
        || !has_strictly_rare_identifier(rarity)
    {
        return 0.0;
    }
    let rarity_weighted_bonus =
        rarity.clamp(0.0, 1.0) * DOCUMENT_EXACT_IDENTIFIER_RELEVANCE_BONUS_MAX;
    let relative_cap = pre_uplift_score * DOCUMENT_EXACT_IDENTIFIER_RELEVANCE_MAX_FRACTION;
    rarity_weighted_bonus.min(relative_cap)
}
pub(super) fn document_search_chunk_relevance(
    query_text: &str,
    hit: &KnowledgeChunkSearchRow,
) -> usize {
    let lowered_text = format!("{} {}", hit.content_text, hit.normalized_text).to_lowercase();
    let keywords = crate::services::query::planner::extract_keywords(query_text);
    let keyword_score = keywords
        .iter()
        .map(|keyword| lowered_text.matches(keyword.as_str()).count())
        .sum::<usize>();
    let anchor_score = document_search_anchor_tokens(query_text)
        .into_iter()
        .filter(|token| lowered_text.contains(token))
        .count();
    let phrase_score = document_search_anchor_phrases(query_text)
        .into_iter()
        .filter(|phrase| lowered_text.contains(phrase))
        .count();
    keyword_score + anchor_score * 50 + phrase_score * 150
}

pub(super) fn document_search_anchor_tokens(query_text: &str) -> Vec<String> {
    normalized_alnum_token_sequence_by(
        query_text,
        |token| token.chars().count() >= 4 || token.chars().any(|ch| ch.is_ascii_digit()),
        Some(32),
    )
}

pub(super) fn document_search_anchor_phrases(query_text: &str) -> Vec<String> {
    let tokens = normalized_alnum_token_sequence_by(
        query_text,
        |token| token.chars().count() >= 2 || token.chars().any(|ch| ch.is_ascii_digit()),
        Some(48),
    );
    let anchors = document_search_anchor_tokens(query_text).into_iter().collect::<HashSet<_>>();
    let mut phrases = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for window_size in 2..=5 {
        for window in tokens.windows(window_size) {
            if !window.iter().any(|token| anchors.contains(token)) {
                continue;
            }
            let phrase = window.join(" ");
            if seen.insert(phrase.clone()) {
                phrases.push(phrase);
            }
        }
    }
    phrases
}

pub(super) fn document_search_exact_anchor_surfaces(query_text: &str) -> Vec<String> {
    let mut anchors = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for raw in query_text.split_whitespace() {
        let candidate = raw
            .trim_matches(|ch: char| {
                !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'))
            })
            .trim();
        if candidate.is_empty() {
            continue;
        }
        let alnum_count = candidate.chars().filter(|ch| ch.is_alphanumeric()).count();
        if alnum_count < 3 {
            continue;
        }
        let has_structural_separator =
            candidate.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'));
        let has_digit = candidate.chars().any(|ch| ch.is_ascii_digit());
        let has_upper_after_first = candidate.chars().skip(1).any(|ch| ch.is_ascii_uppercase());
        let has_lower = candidate.chars().any(|ch| ch.is_ascii_lowercase());
        if !(has_structural_separator || has_digit || (has_upper_after_first && has_lower)) {
            continue;
        }
        let normalized = candidate.to_lowercase();
        if seen.insert(normalized.clone()) {
            anchors.push(normalized);
        }
    }
    anchors
}
