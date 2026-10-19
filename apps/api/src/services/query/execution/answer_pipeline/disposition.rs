use super::variant_selection::extract_query_specific_variants;
use super::{HashSet, PreparedAnswerQueryResult, QueryIR, RuntimeMatchedChunk};

/// Post-retrieval routing decision: should the runtime answer the
/// question from the evidence it has, or should it ask the user a
/// short clarifying question first?
///
/// This is a *corpus-conditioned* signal — QueryCompiler sees only
/// the raw NL question, but the retrieval bundle reveals whether
/// the library has one dominant procedure for the asked topic or
/// several competing variants / subsystems that a single-shot
/// answer will inevitably hedge across (the observed "scattered
/// mentions but no full guide" failure mode on short
/// `ConfigureHow` queries). Driven purely by structural signals on
/// the retrieved context — no hardcoded domain words, no library-
/// specific lists.
#[derive(Debug, Clone)]
pub(super) enum AnswerDisposition {
    /// Proceed with single-shot grounded answering; the evidence
    /// has a dominant cluster or the question is specific enough.
    Answer,
    /// Ask a short clarifying question that enumerates the distinct
    /// variants the retrieval bundle found. `variants` are human-
    /// readable labels pulled from retrieved document titles, graph
    /// node labels, or grouped references — whichever are most
    /// naming on the fetched context. This disposition always carries
    /// at least two distinct, query-aligned variants.
    Clarify { variants: Vec<String> },
}

pub(super) fn prepared_with_preflight_context_titles(
    prepared: &PreparedAnswerQueryResult,
    primary_answer_chunks: &[RuntimeMatchedChunk],
) -> PreparedAnswerQueryResult {
    let mut expanded = prepared.clone();
    let mut seen = expanded
        .structured
        .retrieved_context_document_titles
        .iter()
        .map(|title| title.to_lowercase())
        .collect::<HashSet<_>>();
    for chunk in primary_answer_chunks {
        let title = chunk.document_label.trim();
        if title.is_empty() {
            continue;
        }
        if seen.insert(title.to_lowercase()) {
            expanded.structured.retrieved_context_document_titles.push(title.to_string());
        }
    }
    expanded
}

/// Classify whether the runtime should answer from the retrieved
/// evidence or clarify with the user.
///
/// `Clarify` is authorized only by compiled typed QueryIR and confirmed by at
/// least two distinct choices in retrieved evidence. Raw question length,
/// confidence, tokens, and title overlap cannot create clarification intent.
pub(super) fn classify_answer_disposition(
    prepared: &PreparedAnswerQueryResult,
    user_question: &str,
) -> AnswerDisposition {
    classify_answer_disposition_from_evidence(
        user_question,
        &prepared.query_ir,
        &prepared.structured.retrieved_documents,
        &prepared.structured.retrieved_context_document_titles,
        &prepared.structured.diagnostics.grouped_references,
    )
}

#[cfg(test)]
pub(super) fn classify_answer_disposition_from_groups(
    user_question: &str,
    ir: &QueryIR,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    groups: &[crate::domains::query::GroupedReference],
) -> AnswerDisposition {
    classify_answer_disposition_from_evidence(user_question, ir, retrieved_documents, &[], groups)
}

pub(super) fn classify_answer_disposition_from_evidence(
    user_question: &str,
    ir: &QueryIR,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    context_document_titles: &[String],
    groups: &[crate::domains::query::GroupedReference],
) -> AnswerDisposition {
    if !ir.should_request_clarification() {
        return AnswerDisposition::Answer;
    }

    let mut ranked = groups
        .iter()
        .map(|reference| (reference.evidence_count, reference.title.clone()))
        .collect::<Vec<_>>();
    ranked.sort_by_key(|entry| std::cmp::Reverse(entry.0));

    // Evidence is allowed to confirm typed clarification and populate a bounded
    // menu only after the query compiler contract requested it. Fewer than
    // two evidence-derived choices cannot produce an actionable clarification;
    // continue through grounded answering instead of emitting an empty menu.
    let variants = extract_query_specific_variants(
        user_question,
        retrieved_documents,
        context_document_titles,
        &ranked,
    );
    if variants.len() < 2 {
        AnswerDisposition::Answer
    } else {
        AnswerDisposition::Clarify { variants }
    }
}
