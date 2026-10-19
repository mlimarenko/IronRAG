use crate::infra::knowledge_plane::ChunkLexicalQuery;

use super::coverage::{
    EvidenceCoverage, compare_operands_covered_by_context, operand_covered_by_evidence,
};
use super::literal_revision::{
    contains_label_mention, ranked_structural_literal_token_is_eligible,
};
use super::preflight::{
    COMPARE_OPERAND_PROBE_LIMIT, COMPARE_OPERAND_PROBE_MAX_CHUNKS,
    COMPARE_OPERAND_PROBE_MAX_CHUNKS_PER_OPERAND, TECHNICAL_FOCUS_PROBE_HIT_LIMIT,
    TECHNICAL_FOCUS_PROBE_MAX_CHUNKS, TECHNICAL_FOCUS_PROBE_MAX_CHUNKS_PER_TERM,
    TECHNICAL_FOCUS_PROBE_TERM_LIMIT, structural_question_tokens,
};
use super::{
    AppState, HashMap, HashSet, KnowledgeDocumentRow, LexicalQueryOrigin, QueryAct,
    QueryChunkReferenceSnapshot, QueryIR, RuntimeMatchedChunk, Uuid,
    render_targeted_evidence_chunk_section, technical_literal_focus_keywords,
};

/// Shared immutable inputs for the answer-pipeline evidence probes. The
/// technical-focus and partial-compare probes read the same compile/plan
/// outputs, so they travel as one value instead of six loose arguments.
pub(super) struct EvidenceProbeContext<'a> {
    pub(super) state: &'a AppState,
    pub(super) library_id: Uuid,
    pub(super) ir: &'a QueryIR,
    pub(super) question: &'a str,
    pub(super) document_index: &'a HashMap<Uuid, KnowledgeDocumentRow>,
    pub(super) plan_keywords: &'a [String],
}

#[derive(Debug, Clone, Default)]
pub(super) struct CompareContextProbeOutcome {
    pub(super) attempted: bool,
    pub(super) missing_operand_count: usize,
    pub(super) added_chunk_count: usize,
    pub(super) unresolved_operand_count: usize,
}

#[derive(Debug, Clone, Default)]
pub(super) struct TechnicalFocusProbeOutcome {
    pub(super) attempted: bool,
    pub(super) probe_term_count: usize,
    pub(super) missing_term_count: usize,
    pub(super) added_chunk_count: usize,
}

pub(super) async fn augment_technical_focus_context(
    context: &EvidenceProbeContext<'_>,
    answer_context: &mut String,
    structured: &mut super::super::RuntimeStructuredQueryResult,
) -> anyhow::Result<TechnicalFocusProbeOutcome> {
    let probe_terms = collect_technical_focus_probe_terms(context.ir);
    if probe_terms.is_empty() {
        return Ok(TechnicalFocusProbeOutcome::default());
    }
    let missing_terms = probe_terms
        .iter()
        .filter(|term| !contains_label_mention(answer_context, term))
        .cloned()
        .collect::<Vec<_>>();
    let focus_keywords = technical_literal_focus_keywords(context.question, Some(context.ir));
    let existing_chunk_ids = structured
        .chunk_references
        .iter()
        .map(|reference| reference.chunk_id)
        .chain(structured.context_chunks.iter().map(|chunk| chunk.chunk_id))
        .collect::<HashSet<_>>();
    let probe_chunks = probe_missing_technical_focus_terms(
        context,
        &missing_terms,
        &focus_keywords,
        &existing_chunk_ids,
    )
    .await?;
    if !probe_chunks.is_empty() {
        let probe_question = missing_terms.join(" ");
        let literal_inventory =
            render_technical_focus_literal_inventory(&probe_chunks, &focus_keywords);
        append_answer_context_section(answer_context, &literal_inventory);
        let probe_context = render_targeted_evidence_chunk_section(&probe_question, &probe_chunks);
        append_answer_context_section(answer_context, &probe_context);
        append_probe_chunk_references(structured, &probe_chunks);
        append_probe_document_titles(structured, &probe_chunks);
        append_probe_context_chunks(structured, probe_chunks.clone());
    }
    Ok(TechnicalFocusProbeOutcome {
        attempted: true,
        probe_term_count: probe_terms.len(),
        missing_term_count: missing_terms.len(),
        added_chunk_count: probe_chunks.len(),
    })
}

pub(crate) fn collect_technical_focus_probe_terms(ir: &QueryIR) -> Vec<String> {
    let mut ranked_terms = Vec::<(usize, String)>::new();
    for literal in &ir.literal_constraints {
        push_technical_focus_probe_term(&mut ranked_terms, &literal.text, 0);
    }
    for entity in &ir.target_entities {
        push_technical_focus_probe_term(&mut ranked_terms, &entity.label, 1);
    }
    if let Some(focus) = &ir.document_focus {
        push_technical_focus_probe_term(&mut ranked_terms, &focus.hint, 2);
    }
    let mut best_rank_by_key = HashMap::<String, (usize, String)>::new();
    for (rank, term) in ranked_terms {
        let trimmed = term.trim();
        if !technical_focus_probe_term_is_eligible(trimmed) {
            continue;
        }
        let key = trimmed.to_lowercase();
        best_rank_by_key
            .entry(key)
            .and_modify(|existing| {
                if rank < existing.0 {
                    *existing = (rank, trimmed.to_string());
                }
            })
            .or_insert_with(|| (rank, trimmed.to_string()));
    }
    let mut terms = best_rank_by_key.into_values().collect::<Vec<_>>();
    terms.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then_with(|| right.1.chars().count().cmp(&left.1.chars().count()))
            .then_with(|| left.1.cmp(&right.1))
    });
    terms.into_iter().map(|(_, term)| term).take(TECHNICAL_FOCUS_PROBE_TERM_LIMIT).collect()
}

pub(super) fn push_technical_focus_probe_term(
    ranked_terms: &mut Vec<(usize, String)>,
    value: &str,
    rank: usize,
) {
    for token in structural_question_tokens(value) {
        ranked_terms.push((rank, token));
    }
}

pub(super) fn technical_focus_probe_term_is_eligible(term: &str) -> bool {
    let char_count = term.chars().count();
    (2..=80).contains(&char_count) && term.chars().any(char::is_alphanumeric)
}

pub(super) async fn probe_missing_technical_focus_terms(
    context: &EvidenceProbeContext<'_>,
    missing_terms: &[String],
    focus_keywords: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let mut score_by_chunk = HashMap::<Uuid, f32>::new();
    for term in missing_terms {
        probe_technical_term_scores(
            context.state,
            context.library_id,
            term,
            existing_chunk_ids,
            &mut score_by_chunk,
        )
        .await?;
    }
    probe_combined_technical_focus_scores(
        context.state,
        context.library_id,
        focus_keywords,
        existing_chunk_ids,
        &mut score_by_chunk,
    )
    .await?;
    map_probe_scores_to_chunks(context, score_by_chunk, TECHNICAL_FOCUS_PROBE_MAX_CHUNKS).await
}

pub(super) async fn probe_technical_term_scores(
    state: &AppState,
    library_id: Uuid,
    term: &str,
    existing_chunk_ids: &HashSet<Uuid>,
    score_by_chunk: &mut HashMap<Uuid, f32>,
) -> anyhow::Result<()> {
    let rows = state
        .search_store
        .search_chunks(ChunkLexicalQuery {
            library_id,
            query: term,
            query_origin: LexicalQueryOrigin::DerivedProbe,
            limit: TECHNICAL_FOCUS_PROBE_HIT_LIMIT,
            temporal_start: None,
            temporal_end: None,
            text_search_config: None,
        })
        .await?;
    for row in rows
        .into_iter()
        .filter(|row| {
            !existing_chunk_ids.contains(&row.chunk_id) && search_row_covers_operand(term, row)
        })
        .take(TECHNICAL_FOCUS_PROBE_MAX_CHUNKS_PER_TERM)
    {
        let score = row.score as f32 + (term.chars().count().min(24) as f32 / 24.0);
        record_probe_score(score_by_chunk, row.chunk_id, score);
    }
    Ok(())
}

pub(super) async fn probe_combined_technical_focus_scores(
    state: &AppState,
    library_id: Uuid,
    focus_keywords: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
    score_by_chunk: &mut HashMap<Uuid, f32>,
) -> anyhow::Result<()> {
    if focus_keywords.len() < 3 {
        return Ok(());
    }
    let focus_query = focus_keywords
        .iter()
        .take(TECHNICAL_FOCUS_PROBE_TERM_LIMIT)
        .cloned()
        .collect::<Vec<_>>()
        .join(" ");
    let rows = state
        .search_store
        .search_chunks(ChunkLexicalQuery {
            library_id,
            query: &focus_query,
            query_origin: LexicalQueryOrigin::DerivedProbe,
            limit: TECHNICAL_FOCUS_PROBE_HIT_LIMIT,
            temporal_start: None,
            temporal_end: None,
            text_search_config: None,
        })
        .await?;
    let accepted = rows
        .into_iter()
        .filter(|row| {
            !existing_chunk_ids.contains(&row.chunk_id)
                && !score_by_chunk.contains_key(&row.chunk_id)
                && search_row_covers_technical_focus(row, focus_keywords)
        })
        .take(TECHNICAL_FOCUS_PROBE_MAX_CHUNKS_PER_TERM)
        .collect::<Vec<_>>();
    for row in accepted {
        score_by_chunk.insert(row.chunk_id, row.score as f32);
    }
    Ok(())
}

pub(super) fn render_technical_focus_literal_inventory(
    chunks: &[RuntimeMatchedChunk],
    focus_keywords: &[String],
) -> String {
    let focus_keywords = focus_keywords
        .iter()
        .map(|keyword| keyword.trim().to_lowercase())
        .filter(|keyword| keyword.chars().count() >= 2)
        .collect::<Vec<_>>();
    if focus_keywords.is_empty() {
        return String::new();
    }
    let mut seen = HashSet::<String>::new();
    let mut lines = Vec::with_capacity(chunks.len().saturating_add(1));
    lines.push("Exact technical literals".to_string());
    for chunk in chunks {
        let text = format!("{}\n{}", chunk.excerpt, chunk.source_text);
        let literals = extract_focus_aligned_structural_literals(&text, &focus_keywords, &mut seen);
        if literals.is_empty() {
            continue;
        }
        let rendered = literals
            .into_iter()
            .take(8)
            .map(|literal| format!("`{literal}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- {}: {}", chunk.document_label, rendered));
        if seen.len() >= 32 {
            break;
        }
    }
    if lines.len() <= 1 { String::new() } else { lines.join("\n") }
}

pub(super) fn extract_focus_aligned_structural_literals(
    text: &str,
    focus_keywords: &[String],
    seen: &mut HashSet<String>,
) -> Vec<String> {
    structural_question_tokens(text)
        .into_iter()
        .map(|token| trim_structural_literal_token(&token).to_string())
        .filter(|token| technical_focus_literal_token_is_eligible(token))
        .filter(|token| technical_focus_literal_token_matches_focus(token, focus_keywords))
        .filter(|token| seen.insert(token.to_lowercase()))
        .take(16)
        .collect()
}

pub(super) fn trim_structural_literal_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '`' | ':')
    })
}

pub(super) fn technical_focus_literal_token_is_eligible(token: &str) -> bool {
    let char_count = token.chars().count();
    (2..=96).contains(&char_count) && ranked_structural_literal_token_is_eligible(token)
}

pub(super) fn technical_focus_literal_token_matches_focus(
    token: &str,
    focus_keywords: &[String],
) -> bool {
    let lowered = token.to_lowercase();
    focus_keywords.iter().any(|keyword| {
        keyword == &lowered
            || (keyword.chars().count() >= 4 && lowered.contains(keyword))
            || (lowered.chars().count() >= 4 && keyword.contains(&lowered))
            || (keyword.chars().count() < 4
                && split_identifier_subtokens(token).iter().any(|part| part == keyword))
            || split_identifier_subtokens(token)
                .iter()
                .any(|part| part.chars().count() >= 4 && part == keyword)
    })
}

pub(super) fn split_identifier_subtokens(token: &str) -> Vec<String> {
    let mut parts = Vec::<String>::new();
    let mut current = String::new();
    let mut previous_lowercase = false;
    for ch in token.chars() {
        if !ch.is_alphanumeric() {
            if !current.is_empty() {
                parts.push(current.to_lowercase());
                current.clear();
            }
            previous_lowercase = false;
            continue;
        }
        if previous_lowercase && ch.is_uppercase() && !current.is_empty() {
            parts.push(current.to_lowercase());
            current.clear();
        }
        previous_lowercase = ch.is_lowercase();
        current.push(ch);
    }
    if !current.is_empty() {
        parts.push(current.to_lowercase());
    }
    parts
}

pub(super) fn search_row_covers_technical_focus(
    row: &crate::infra::knowledge_rows::KnowledgeChunkSearchRow,
    focus_keywords: &[String],
) -> bool {
    let section = row.section_path.join(" ");
    let heading = row.heading_trail.join(" ");
    let evidence =
        format!("{}\n{}\n{}\n{}", row.content_text, row.normalized_text, section, heading)
            .to_lowercase();
    let evidence_tokens = crate::services::query::text_match::normalized_alnum_tokens(&evidence, 2);
    let overlap = focus_keywords
        .iter()
        .filter(|keyword| {
            let keyword = keyword.trim().to_lowercase();
            !keyword.is_empty()
                && evidence_tokens.iter().any(|token| {
                    crate::services::query::text_match::near_token_match(&keyword, token)
                })
        })
        .count();
    overlap >= focus_keywords.len().min(3).clamp(2, 3)
}

pub(super) async fn augment_partial_compare_context(
    context: &EvidenceProbeContext<'_>,
    answer_context: &mut String,
    structured: &mut super::super::RuntimeStructuredQueryResult,
) -> anyhow::Result<CompareContextProbeOutcome> {
    if !matches!(context.ir.act, QueryAct::Compare) {
        return Ok(CompareContextProbeOutcome::default());
    }
    let EvidenceCoverage::Partial { covered_operands, missing_operands } =
        compare_operands_covered_by_context(context.ir, answer_context)
    else {
        return Ok(CompareContextProbeOutcome::default());
    };
    let mut outcome = CompareContextProbeOutcome {
        attempted: true,
        missing_operand_count: missing_operands.len(),
        added_chunk_count: 0,
        unresolved_operand_count: missing_operands.len(),
    };
    let existing_chunk_ids = HashSet::<Uuid>::new();
    let probe_chunks =
        probe_missing_compare_operands(context, &missing_operands, &existing_chunk_ids).await?;
    if !probe_chunks.is_empty() {
        let probe_question = missing_operands.join(" ");
        let probe_context = render_targeted_evidence_chunk_section(&probe_question, &probe_chunks);
        append_answer_context_section(answer_context, &probe_context);
        append_probe_chunk_references(structured, &probe_chunks);
        append_probe_document_titles(structured, &probe_chunks);
        append_probe_context_chunks(structured, probe_chunks.clone());
        outcome.added_chunk_count = probe_chunks.len();
    }

    match compare_operands_covered_by_context(context.ir, answer_context) {
        EvidenceCoverage::Sufficient => {
            outcome.unresolved_operand_count = 0;
        }
        EvidenceCoverage::Partial { covered_operands, missing_operands } => {
            outcome.unresolved_operand_count = missing_operands.len();
            append_answer_context_section(
                answer_context,
                &render_partial_comparison_coverage(&covered_operands, &missing_operands),
            );
        }
        EvidenceCoverage::Insufficient(_) => {
            append_answer_context_section(
                answer_context,
                &render_partial_comparison_coverage(&covered_operands, &missing_operands),
            );
        }
    }
    Ok(outcome)
}

pub(super) async fn probe_missing_compare_operands(
    context: &EvidenceProbeContext<'_>,
    missing_operands: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let mut score_by_chunk = HashMap::<Uuid, f32>::new();
    for operand in missing_operands {
        probe_compare_operand_scores(
            context.state,
            context.library_id,
            operand,
            context.plan_keywords,
            existing_chunk_ids,
            &mut score_by_chunk,
        )
        .await?;
    }
    map_probe_scores_to_chunks(context, score_by_chunk, COMPARE_OPERAND_PROBE_MAX_CHUNKS).await
}

pub(super) async fn probe_compare_operand_scores(
    state: &AppState,
    library_id: Uuid,
    operand: &str,
    plan_keywords: &[String],
    existing_chunk_ids: &HashSet<Uuid>,
    score_by_chunk: &mut HashMap<Uuid, f32>,
) -> anyhow::Result<()> {
    for query in compare_operand_probe_queries(operand, plan_keywords) {
        let rows = state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: &query,
                query_origin: LexicalQueryOrigin::DerivedProbe,
                limit: COMPARE_OPERAND_PROBE_LIMIT,
                temporal_start: None,
                temporal_end: None,
                text_search_config: None,
            })
            .await?;
        let accepted = rows
            .into_iter()
            .filter(|row| {
                !existing_chunk_ids.contains(&row.chunk_id)
                    && search_row_covers_operand(operand, row)
            })
            .take(COMPARE_OPERAND_PROBE_MAX_CHUNKS_PER_OPERAND);
        for row in accepted {
            let score = row.score as f32 + compare_probe_query_specificity_bonus(&query, operand);
            record_probe_score(score_by_chunk, row.chunk_id, score);
        }
    }
    Ok(())
}

pub(super) fn record_probe_score(
    score_by_chunk: &mut HashMap<Uuid, f32>,
    chunk_id: Uuid,
    score: f32,
) {
    score_by_chunk
        .entry(chunk_id)
        .and_modify(|existing| *existing = existing.max(score))
        .or_insert(score);
}

pub(super) async fn map_probe_scores_to_chunks(
    context: &EvidenceProbeContext<'_>,
    score_by_chunk: HashMap<Uuid, f32>,
    limit: usize,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if score_by_chunk.is_empty() {
        return Ok(Vec::new());
    }
    let chunk_ids = score_by_chunk.keys().copied().collect::<Vec<_>>();
    let rows = context.state.document_store.list_chunks_by_ids(&chunk_ids).await?;
    let mut chunks = rows
        .into_iter()
        .filter_map(|row| {
            let score = score_by_chunk.get(&row.chunk_id).copied()?;
            super::super::chunk_support::map_chunk_hit(
                row,
                score,
                context.document_index,
                context.plan_keywords,
            )
        })
        .collect::<Vec<_>>();
    chunks.sort_by(|left, right| {
        right.score.partial_cmp(&left.score).unwrap_or(std::cmp::Ordering::Equal)
    });
    chunks.truncate(limit);
    Ok(chunks)
}

pub(super) fn compare_operand_probe_queries(
    operand: &str,
    plan_keywords: &[String],
) -> Vec<String> {
    let mut queries = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    let mut push = |value: String| {
        let normalized = value.trim();
        if normalized.is_empty() {
            return;
        }
        if seen.insert(normalized.to_lowercase()) {
            queries.push(normalized.to_string());
        }
    };
    push(operand.to_string());
    let focus = plan_keywords
        .iter()
        .map(|keyword| keyword.trim())
        .filter(|keyword| keyword.chars().count() >= 4)
        .take(8)
        .collect::<Vec<_>>()
        .join(" ");
    if !focus.is_empty() {
        push(format!("{operand} {focus}"));
    }
    queries
}

pub(super) fn compare_probe_query_specificity_bonus(query: &str, operand: &str) -> f32 {
    if query.trim().eq_ignore_ascii_case(operand.trim()) { 0.0 } else { 1.0 }
}

pub(super) fn search_row_covers_operand(
    operand: &str,
    row: &crate::infra::knowledge_rows::KnowledgeChunkSearchRow,
) -> bool {
    let section = row.section_path.join(" ");
    let heading = row.heading_trail.join(" ");
    let evidence = [
        row.content_text.as_str(),
        row.normalized_text.as_str(),
        section.as_str(),
        heading.as_str(),
    ];
    operand_covered_by_evidence(operand, &evidence)
}

pub(super) fn append_probe_chunk_references(
    structured: &mut super::super::RuntimeStructuredQueryResult,
    chunks: &[RuntimeMatchedChunk],
) {
    let mut seen = structured
        .chunk_references
        .iter()
        .map(|reference| reference.chunk_id)
        .collect::<HashSet<_>>();
    let mut next_rank =
        structured.chunk_references.iter().map(|reference| reference.rank).max().unwrap_or(0) + 1;
    for chunk in chunks {
        if !seen.insert(chunk.chunk_id) {
            continue;
        }
        structured.chunk_references.push(QueryChunkReferenceSnapshot {
            chunk_id: chunk.chunk_id,
            rank: next_rank,
            score: chunk.score.unwrap_or(0.0) as f64,
        });
        next_rank += 1;
    }
}

pub(super) fn append_probe_document_titles(
    structured: &mut super::super::RuntimeStructuredQueryResult,
    chunks: &[RuntimeMatchedChunk],
) {
    let mut seen = structured
        .retrieved_context_document_titles
        .iter()
        .map(|title| title.to_lowercase())
        .collect::<HashSet<_>>();
    for chunk in chunks {
        let title = chunk.document_label.trim();
        if title.is_empty() {
            continue;
        }
        if seen.insert(title.to_lowercase()) {
            structured.retrieved_context_document_titles.push(title.to_string());
        }
    }
}

pub(super) fn append_probe_context_chunks(
    structured: &mut super::super::RuntimeStructuredQueryResult,
    chunks: Vec<RuntimeMatchedChunk>,
) {
    let mut seen =
        structured.context_chunks.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    for chunk in chunks {
        if seen.insert(chunk.chunk_id) {
            structured.context_chunks.push(chunk);
        }
    }
}

pub(super) fn append_answer_context_section(answer_context: &mut String, section: &str) {
    let section = section.trim();
    if section.is_empty() {
        return;
    }
    if !answer_context.trim().is_empty() {
        answer_context.push_str("\n\n");
    }
    answer_context.push_str(section);
}

pub(super) fn render_partial_comparison_coverage(
    covered_operands: &[String],
    missing_operands: &[String],
) -> String {
    let mut lines = Vec::with_capacity(
        covered_operands.len().saturating_add(missing_operands.len()).saturating_add(1),
    );
    lines.push("COMPARISON_COVERAGE status=partial".to_string());
    for operand in covered_operands {
        lines.push(format!("- covered_operand: {}", operand.trim()));
    }
    for operand in missing_operands {
        lines.push(format!("- uncovered_operand: {}", operand.trim()));
    }
    lines.join("\n")
}
