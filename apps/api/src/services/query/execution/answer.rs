use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::services::query::text_match::{
    add_label_terms_with_acronyms, label_term_sequence, label_terms,
    normalized_alnum_token_sequence, normalized_alnum_tokens, token_sequence_contains_tokens,
    token_sequence_exact_or_contains_tokens,
};
use crate::{
    domains::query_ir::{EntityRole, LiteralKind, QueryAct, QueryIR, QueryLanguage, QueryScope},
    infra::knowledge_rows::{
        KnowledgeDocumentRow, KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow,
    },
    services::query::i18n::{self},
    shared::extraction::{
        record_jsonl::focused_record_unit_excerpt, table_summary::parse_table_column_summary,
        technical_facts::TechnicalFactKind,
    },
};

use super::consolidation::query_has_multi_document_setup_anchors;
use super::endpoint_answer::{
    build_multi_document_endpoint_answer_from_facts, build_single_endpoint_answer_from_facts,
};
pub(crate) use super::focused_document_answer::build_focused_document_answer;
use super::port_answer::{build_port_and_protocol_answer_from_facts, build_port_answer_from_facts};
use super::question_intent::{
    QuestionIntent, canonical_target_type_tag, classify_question_or_ir_intents,
    query_ir_allows_procedure_runbook_target, query_ir_has_setup_configuration_target,
    query_ir_is_unambiguous_versioned_procedure,
};
use super::transport_answer::build_transport_contract_comparison_answer;
use crate::services::query::effective_query::current_question_segment;
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::retrieve::{
    chunk_is_setup_focus_command_path_anchor, command_dense_excerpt_for, excerpt_for,
    focused_excerpt_for,
};
use super::source_context::{salient_source_excerpt_for, structured_literal_excerpt_for};
use super::technical_answer::build_exact_technical_literal_answer;
#[cfg(test)]
use super::technical_literals::technical_chunk_selection_score;
use super::technical_literals::{
    extract_config_assignment_literals, extract_explicit_path_literals, extract_http_methods,
    extract_package_command_literals, extract_parameter_literals, extract_prefix_literals,
    extract_url_literals, select_document_balanced_chunks, technical_literal_focus_keywords,
};
use super::types::*;
use super::{
    build_table_row_grounded_answer, build_table_summary_grounded_answer,
    focus_token_overlap_count, query_ir_document_focus_tokens, question_asks_table_aggregation,
};
use crate::services::query::latest_versions::{
    compare_version_desc, extract_release_context_version, extract_semver_like_version,
    latest_version_family_key, query_requests_latest_versions, requested_latest_version_count,
};

const SOURCE_COVERAGE_MAX_TOTAL_CHUNKS: usize = 32;
const SOURCE_COVERAGE_MAX_CHUNKS_PER_DOCUMENT: usize = 24;
const PREPARED_SEGMENT_EXCERPT_CHARS: usize = 420;
const SOURCE_SLICE_COMPACT_BODY_CHARS: usize = 720;
const SOURCE_SLICE_COMPACT_BODY_LINES: usize = 8;
const PREPARED_STRUCTURAL_BLOCK_CHARS: usize = 4_000;
const EVIDENCE_CHUNK_EXCERPT_CHARS: usize = 560;
const EVIDENCE_CODE_BLOCK_CHARS: usize = 4_000;
const STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS: usize = 4_000;
const UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP: usize = 8;

#[cfg(test)]
pub(crate) fn build_answer_prompt(
    question: &str,
    context_text: &str,
    conversation_history: Option<&str>,
    system_prompt: Option<&str>,
) -> String {
    let instruction = system_prompt
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("You are answering a grounded knowledge-base question.");
    let conversation_history_section = conversation_history
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(String::new, |history| {
            format!(
                "Use the recent conversation history to resolve short follow-up messages, confirmations, pronouns, and ellipsis.\n\
When the latest user message depends on prior turns, continue the same task instead of treating it as a brand-new unrelated request.\n\
\nRecent conversation:\n{}\n\
\n",
                history
            )
        });
    format!(
        "{}\n\
Treat the active library as the primary source of truth and exhaust the provided library context before concluding that information is missing.\n\
Hard output boundary: write only the grounded answer for this turn. Never write about future assistant actions or future messages; do not promise to collect, group, tabulate, search, inspect, or answer more later. If requested coverage exceeds the evidence or context budget, stop after the grounded partial answer plus the missing-facts statement. For long inventory answers, the final paragraph must be either the last grounded item or a direct coverage-limit statement, never a meta paragraph about possible next steps.\n\
The context may include library summary facts, recent document metadata, document excerpts, graph entities, and graph relationships gathered across many documents.\n\
Silently synthesize across the available evidence instead of stopping after the first partial hit.\n\
When Context includes a Table summaries section for a tabular question, treat that section as the authoritative source for aggregate answers such as averages, min/max ranges, and most frequent values.\n\
Do not infer aggregate table answers from individual table rows, technical facts, or neighboring snippets when a Table summaries section is present.\n\
For questions about the latest documents, document inventory, readiness, counts, or pipeline state, answer from library summary and recent document metadata even when chunk excerpts alone are not enough.\n\
Combine metadata, grounded excerpts, and graph references before deciding that the answer is unavailable.\n\
Present the answer directly. Do not narrate the retrieval process and do not mention chunks, internal search steps, the library context, or source document names unless the user explicitly asks for sources, evidence, or document names.\n\
End after the complete grounded answer. Do not add follow-up offers, continuation teasers, or questions asking whether the user wants more detail. If evidence coverage is bounded, state the coverage limit directly instead of offering a next message. For long inventory answers, end on the last grounded item or the coverage-limit statement; do not append a separate invitation or next-step paragraph.\n\
Start with the answer itself, not with preambles like \"in the documents\", \"in the library\", or \"in the available materials\".\n\
Prefer domain-language wording like \"The API uses ...\", \"The system stores ...\", or \"The article names ...\" over wording like \"The materials describe ...\" or \"The library contains ...\".\n\
Only name specific document titles when the question itself asks for titles, recent documents, or sources.\n\
Do not ask the user to upload, resend, or provide more documents unless the active library context is genuinely insufficient after using all provided evidence.\n\
If the answer is still incomplete, give the best grounded partial answer and briefly state which facts are still missing from the active library.\n\
When the library lacks enough information, describe the missing facts or subject area, not a \"missing document\" and not a request to send more files.\n\
Do not suggest uploads or resends unless the user explicitly asks how to improve or extend the library.\n\
Answer in the same language as the question.\n\
When the question clearly targets one article, one document, or one named subject, answer from the single most directly matching grounded document first.\n\
Do not import examples, use cases, lists, or entities from neighboring documents unless the question explicitly asks you to compare or combine multiple documents.\n\
When the user asks for one example or one use case from a specific document, choose an example grounded in that same document.\n\
When the user asks for one example, one use case, or one named item besides an explicitly excluded item from a grounded list, choose a different grounded item from that same list and prefer the next distinct item after the excluded one when the list order is available.\n\
When the user asks for examples across categories joined by \"and\", include grounded representatives from each requested category when they appear in the same grounded document.\n\
When the user asks to describe, classify, or explain each item from a prior literal list, preserve visible coverage of that list. Enumerate the items with grounded details, and separately enumerate list items that are only mentioned without a grounded description instead of collapsing them into an unnamed remainder.\n\
When Context includes a Source title inventory and the user asks a broad inventory, setup, configuration, or ambiguous family question, preserve each visible source title at least once before summarizing. If some listed titles have no grounded details, enumerate those titles separately instead of silently dropping them.\n\
When recent conversation contains a line that begins `literals:`, use it as compact memory of identifier-shaped setting names already surfaced in the chat. For follow-up questions about those settings or previously mentioned items, preserve applicable identifier-shaped names that are also supported by the latest context; do not treat this line as new evidence for paths, URLs, commands, versions, or values.\n\
For multi-role questions that ask which item fits each described role, bind each role to the source entity or document whose evidence directly satisfies that role. Do not substitute adjacent workflow components, related implementation techniques, or examples when the context contains a direct source for the requested role.\n\
When the context includes a library summary, trust those summary counts and readiness facts over individual chunk snippets for totals and overall status.\n\
When Context includes AGGREGATE_PROFILE blocks, treat them as source-level aggregate metadata for counts, time ranges, formats, roles, and unit distribution.\n\
Treat EVIDENCE_CHUNK blocks as sampled excerpts. Do not make whole-source frequency, ranking, or coverage claims from EVIDENCE_CHUNK blocks unless an AGGREGATE_PROFILE block supports the claim.\n\
When Context includes COMPARISON_COVERAGE status=partial, compare only the covered operands and explicitly state which requested operands are not grounded in Context.\n\
When the context includes an Exact technical literals section, treat those literals as the highest-priority grounding for URLs, paths, configuration section names, parameter names, methods, ports, and status codes.\n\
Prefer exact literals extracted from documents over paraphrased graph summaries when both are present.\n\
When Exact technical literals are presented as an inventory and the question asks to explain, describe, configure, or enumerate all items, cover each visible inventory item before summarizing; do not silently drop items from the inventory.\n\
When Context contains directly relevant parameter tables or key/value row blocks, preserve every visible matching row before summarizing; do not reduce a grounded row set to examples unless the user asks for examples.\n\
When answering from Context, do not mention the retrieval machinery or phrases such as \"retrieved context\"; present the grounded facts directly.\n\
When a table block contains both a combined aggregate label and individual parameter rows, report the individual parameter names for parameter questions and do not present the aggregate label as a separate parameter.\n\
When Context includes Retrieved graph evidence or graph-evidence blocks, treat their evidence text as direct source wording. If a graph-evidence block contains delimited row fields, preserve each requested field's own value and do not copy a neighboring field value into it.\n\
When a graph question names or asks for multiple entities, roles, endpoints, archives, artifacts, or relationships, include each grounded requested entity or role value explicitly at least once instead of answering with only the final relation label.\n\
For operational or status-handling questions, cover each distinct grounded outcome or action path visible in Context before saying a next action is unavailable. Include the success condition and any failure, timeout, cancellation, rollback, refund/return, retry, or exception-handling path when that path is present in Context.\n\
When source evidence contains exact labels, headings, table names, field values, quoted phrases, identifiers, or short rare phrases that directly answer the question, copy those source spellings verbatim at least once before adding any paraphrase.\n\
If the answer language differs from a source phrase that directly answers the question, keep that source phrase verbatim and explain around it in the answer language.\n\
For rare graph-evidence phrases, include the shortest complete source phrase or row field value that contains the requested terms; do not substitute synonyms, translated equivalents, or inflected variants for the evidence phrase.\n\
When the question names or implies a source, section, table, or evidence location and Context contains that label, include the exact label with the answer.\n\
For source-, section-, table-, or troubleshooting-specific questions, name the exact source title and nearest available heading, table label, or evidence label before the action or conclusion.\n\
For setup or configuration questions where Context contains both a module setup/package block and later companion files, keep the module's own configuration path from the setup block distinct from adjacent display, check, logging, or integration files. Do not replace the requested module configuration path with a neighboring companion file.\n\
For workflow, list, and procedural answers, direct document excerpts are normative. Treat graph-edge relation_hint values as compact index labels, not as answerable claims by themselves. When a graph edge includes evidence text, answer from that evidence wording and scope; do not turn the hinted target into an unconditional item, document, or requirement unless the evidence itself states that.\n\
When Exact technical literals are grouped by document, keep each literal attached to its document heading and do not mix endpoints, URLs, paths, or methods from different documents unless the question explicitly asks you to compare or combine them.\n\
When Exact technical literals include both Paths and Prefixes, treat Paths as operation endpoints and use Prefixes only for questions that explicitly ask for a base prefix or base URL.\n\
When a grouped document entry also includes a matched excerpt, use that excerpt to decide which literal answers the user's condition inside that document.\n\
When the question asks for URLs, endpoints, paths, configuration section names, parameter names, HTTP methods, ports, status codes, field names, or exact behavioral rules, copy those literals verbatim from Context.\n\
Wrap exact technical literals such as URLs, paths, configuration section names, parameter names, HTTP methods, ports, and status codes in backticks.\n\
Do not normalize, rename, translate, repair, shorten, or expand technical literals from Context.\n\
Do not combine parts from different snippets into a synthetic URL, endpoint, path, or rule.\n\
If a literal does not appear verbatim in Context, do not invent it; state that the exact value is not grounded in the active library.\n\
If nearby snippets describe different examples or operations, answer only from the snippet that directly matches the user's condition and ignore unrelated adjacent error payloads or examples.\n\
For definition questions, preserve concrete enumerations, examples, and listed categories from Context instead of collapsing them into a generic paraphrase.\n\
When context includes a document summary, use it to understand the document's purpose before answering.\n\
When Context includes a short title, report name, validation target, or formats-under-test line for the focused document, answer with that literal directly.\n\
When Context includes SOURCE_SLICE_UNIT blocks, treat them as the runtime's canonical ordered source slice for the question, not as sampled excerpts. For positional source-slice requests, enumerate the matching records visible in those blocks and do not refuse merely because the blocks are a bounded slice. Use one visible answer item per SOURCE_SLICE_UNIT block, in block order, up to requested_count; include the block's document=\"...\" value as that item's source label when present; do not split one block into multiple inventory items or add items absent from those blocks. Treat markdown image syntax, link-only decoration, and heading markers inside the block body as document formatting, not answer content.\n\
\n{}\nContext:\n{}\n\
\nQuestion: {}",
        instruction,
        conversation_history_section,
        context_text,
        question.trim()
    )
}

pub(crate) fn build_deterministic_technical_answer(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    accept_deterministic_technical_candidate(
        build_transport_contract_comparison_answer(question, query_ir, chunks),
        question,
        query_ir,
        evidence,
        chunks,
    )
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_port_and_protocol_answer_from_facts(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_port_answer_from_facts(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_single_endpoint_answer_from_facts(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_multi_document_endpoint_answer_from_facts(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
    .or_else(|| {
        accept_deterministic_technical_candidate(
            build_exact_technical_literal_answer(question, query_ir, evidence, chunks),
            question,
            query_ir,
            evidence,
            chunks,
        )
    })
}

fn accept_deterministic_technical_candidate(
    candidate: Option<String>,
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    candidate.filter(|answer| {
        deterministic_answer_satisfies_required_technical_facets(
            answer, question, query_ir, evidence, chunks,
        )
    })
}

fn deterministic_answer_satisfies_required_technical_facets(
    answer: &str,
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    classify_question_or_ir_intents(question, query_ir).into_iter().all(|intent| {
        !intent_has_grounded_literal_evidence(intent, evidence, chunks)
            || answer_covers_technical_intent(answer, intent, evidence)
    })
}

fn intent_has_grounded_literal_evidence(
    intent: QuestionIntent,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    let fact_match = evidence.technical_facts.iter().any(|fact| {
        fact.fact_kind
            .parse::<TechnicalFactKind>()
            .is_ok_and(|kind| technical_fact_kind_supports_intent(kind, intent))
    });
    if fact_match {
        return true;
    }
    chunks.iter().any(|chunk| text_supports_technical_intent(&chunk.source_text, intent))
}

fn technical_fact_kind_supports_intent(kind: TechnicalFactKind, intent: QuestionIntent) -> bool {
    matches!(
        (kind, intent),
        (TechnicalFactKind::EndpointPath, QuestionIntent::Endpoint)
            | (TechnicalFactKind::Url, QuestionIntent::Endpoint)
            | (TechnicalFactKind::Url, QuestionIntent::BasePrefix)
            | (TechnicalFactKind::HttpMethod, QuestionIntent::HttpMethod)
            | (TechnicalFactKind::Port, QuestionIntent::Port)
            | (TechnicalFactKind::ParameterName, QuestionIntent::Parameter)
            | (TechnicalFactKind::StatusCode, QuestionIntent::ErrorCode)
            | (TechnicalFactKind::ErrorCode, QuestionIntent::ErrorCode)
            | (TechnicalFactKind::Protocol, QuestionIntent::Protocol)
            | (TechnicalFactKind::EnvironmentVariable, QuestionIntent::EnvVar)
            | (TechnicalFactKind::VersionNumber, QuestionIntent::Version)
            | (TechnicalFactKind::ConfigurationKey, QuestionIntent::ConfigKey)
    )
}

fn text_supports_technical_intent(text: &str, intent: QuestionIntent) -> bool {
    match intent {
        QuestionIntent::Endpoint => {
            !extract_explicit_path_literals(text, 1).is_empty()
                || !extract_url_literals(text, 1).is_empty()
        }
        QuestionIntent::BasePrefix => {
            !extract_prefix_literals(text, 1).is_empty()
                || !extract_url_literals(text, 1).is_empty()
        }
        QuestionIntent::HttpMethod => !extract_http_methods(text, 1).is_empty(),
        QuestionIntent::Parameter | QuestionIntent::ConfigKey | QuestionIntent::EnvVar => {
            !extract_parameter_literals(text, 1).is_empty()
        }
        QuestionIntent::Port
        | QuestionIntent::Version
        | QuestionIntent::ErrorCode
        | QuestionIntent::Protocol
        | QuestionIntent::FocusedFormatsUnderTest
        | QuestionIntent::FocusedSecondaryHeading
        | QuestionIntent::FocusedPrimaryHeading => false,
    }
}

fn answer_covers_technical_intent(
    answer: &str,
    intent: QuestionIntent,
    evidence: &CanonicalAnswerEvidence,
) -> bool {
    text_supports_technical_intent(answer, intent)
        || evidence.technical_facts.iter().any(|fact| {
            fact.fact_kind
                .parse::<TechnicalFactKind>()
                .is_ok_and(|kind| technical_fact_kind_supports_intent(kind, intent))
                && answer_contains_fact_value(answer, fact)
        })
}

fn answer_contains_fact_value(answer: &str, fact: &KnowledgeTechnicalFactRow) -> bool {
    let answer = answer.to_lowercase();
    for value in [
        fact.display_value.as_str(),
        fact.canonical_value_exact.as_str(),
        fact.canonical_value_text.as_str(),
    ] {
        let normalized = repair_technical_layout_noise(value).trim().to_lowercase();
        if !normalized.is_empty() && answer.contains(&normalized) {
            return true;
        }
    }
    false
}

pub(crate) fn build_deterministic_grounded_answer(
    question: &str,
    query_ir: &QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let answer = build_table_summary_grounded_answer(question, Some(query_ir), chunks)
        .or_else(|| build_table_row_grounded_answer(question, Some(query_ir), chunks))
        .or_else(|| build_setup_configuration_anchor_answer(question, query_ir, chunks))
        .or_else(|| build_structured_list_grounded_answer(question, query_ir, chunks))
        .or_else(|| build_update_procedure_sequence_answer(question, query_ir, chunks))
        .or_else(|| build_focused_document_answer(question, query_ir, chunks))
        .or_else(|| build_structured_source_unit_inventory_answer(question, query_ir, chunks))
        .or_else(|| build_deterministic_technical_answer(question, query_ir, evidence, chunks))?;
    Some(augment_deterministic_grounded_answer_with_evidence(answer, question, query_ir, chunks))
}

pub(super) fn augment_deterministic_grounded_answer_with_evidence(
    answer: String,
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> String {
    let _ = (question, query_ir, chunks);
    answer
}

fn build_structured_list_grounded_answer(
    _question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if query_ir.source_slice.is_some() || !query_ir_requests_structured_list_answer(query_ir) {
        return None;
    }
    let focus_terms = structured_list_focus_terms(query_ir);
    let mut candidates = chunks
        .iter()
        .filter_map(|chunk| extract_structured_list_candidate(chunk, &focus_terms))
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| right.items.len().cmp(&left.items.len()))
            .then_with(|| left.first_chunk_index.cmp(&right.first_chunk_index))
    });
    let candidate = candidates.into_iter().next()?;
    if candidate.items.len() < 2 {
        return None;
    }
    let mut lines = Vec::with_capacity(candidate.items.len().min(16));
    for (index, item) in candidate.items.into_iter().take(16).enumerate() {
        if candidate.ordered {
            lines.push(format!("{}. {}", index + 1, item));
        } else {
            lines.push(format!("- {item}"));
        }
    }
    Some(lines.join("\n"))
}

#[derive(Debug)]
struct StructuredListCandidate {
    items: Vec<String>,
    ordered: bool,
    score: usize,
    first_chunk_index: i32,
}

fn query_ir_requests_structured_list_answer(query_ir: &QueryIR) -> bool {
    if query_ir_has_typed_table_column_inventory_intent(query_ir) {
        return false;
    }

    let has_structured_focus = query_ir.document_focus.is_some()
        || !query_ir.target_types.is_empty()
        || !query_ir.target_entities.is_empty()
        || !query_ir.literal_constraints.is_empty()
        || query_ir.retrieval_query.as_deref().is_some_and(|query| !query.trim().is_empty());
    match query_ir.act {
        QueryAct::Enumerate => has_structured_focus || query_ir.confidence < 0.6,
        QueryAct::ConfigureHow => has_structured_focus,
        QueryAct::Describe | QueryAct::RetrieveValue => {
            has_structured_focus && query_ir.confidence < 0.6
        }
        _ => false,
    }
}

fn structured_list_focus_terms(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        terms.extend(normalized_alnum_tokens(retrieval_query, 3));
    }
    for target_type in &query_ir.target_types {
        terms.extend(normalized_alnum_tokens(target_type, 3));
    }
    for entity in &query_ir.target_entities {
        terms.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    for literal in &query_ir.literal_constraints {
        terms.extend(normalized_alnum_tokens(&literal.text, 3));
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    terms
}

fn extract_structured_list_candidate(
    chunk: &RuntimeMatchedChunk,
    focus_terms: &BTreeSet<String>,
) -> Option<StructuredListCandidate> {
    let text = repair_technical_layout_noise(&chunk.source_text);
    let lines = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    let mut candidates = Vec::new();
    candidates.extend(extract_marked_list_candidates(&lines, focus_terms, chunk.chunk_index));
    candidates.extend(extract_comment_list_candidates(&lines, focus_terms, chunk.chunk_index));
    candidates.into_iter().max_by(|left, right| {
        left.score
            .cmp(&right.score)
            .then_with(|| left.items.len().cmp(&right.items.len()))
            .then_with(|| right.first_chunk_index.cmp(&left.first_chunk_index))
    })
}

fn extract_marked_list_candidates(
    lines: &[&str],
    focus_terms: &BTreeSet<String>,
    chunk_index: i32,
) -> Vec<StructuredListCandidate> {
    let mut candidates = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        if !line.ends_with(':') {
            continue;
        }
        let heading_score = structured_list_line_focus_score(line, focus_terms);
        if heading_score == 0 && !focus_terms.is_empty() {
            continue;
        }
        let mut items = Vec::new();
        let mut ordered = false;
        for following in lines.iter().skip(line_index + 1) {
            if let Some((item_ordered, item)) = parse_structured_list_item(following) {
                ordered |= item_ordered;
                push_unique_structured_list_item(&mut items, item);
                continue;
            }
            if !items.is_empty() {
                break;
            }
        }
        if items.len() >= 2 {
            let item_score =
                items.iter().map(|item| structured_list_line_focus_score(item, focus_terms)).sum();
            candidates.push(StructuredListCandidate {
                items,
                ordered,
                score: heading_score.saturating_mul(4).saturating_add(item_score),
                first_chunk_index: chunk_index,
            });
        }
    }
    candidates
}

fn extract_comment_list_candidates(
    lines: &[&str],
    focus_terms: &BTreeSet<String>,
    chunk_index: i32,
) -> Vec<StructuredListCandidate> {
    let mut items = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        let Some(comment) = trimmed
            .strip_prefix("# ")
            .or_else(|| trimmed.strip_prefix("// "))
            .or_else(|| trimmed.strip_prefix("* "))
        else {
            continue;
        };
        let item = comment.trim();
        if item.chars().count() < 3 || item.chars().count() > 120 {
            continue;
        }
        push_unique_structured_list_item(&mut items, item.to_string());
    }
    if items.len() < 3 {
        return Vec::new();
    }
    let item_score =
        items.iter().map(|item| structured_list_line_focus_score(item, focus_terms)).sum::<usize>();
    if item_score == 0 {
        return Vec::new();
    }
    vec![StructuredListCandidate {
        items,
        ordered: true,
        score: item_score.saturating_add(4),
        first_chunk_index: chunk_index,
    }]
}

fn parse_structured_list_item(line: &str) -> Option<(bool, String)> {
    let trimmed = line.trim();
    let ordered_body = strip_leading_order_marker(trimmed);
    if ordered_body != trimmed {
        return Some((true, clean_structured_list_item(ordered_body)?));
    }
    let bullet_body = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))?;
    Some((false, clean_structured_list_item(bullet_body)?))
}

fn clean_structured_list_item(item: &str) -> Option<String> {
    let cleaned =
        item.trim().trim_matches('"').trim_matches('`').trim_end_matches(',').trim().to_string();
    if cleaned.chars().count() < 2 || cleaned.chars().count() > 180 {
        return None;
    }
    Some(cleaned)
}

fn push_unique_structured_list_item(items: &mut Vec<String>, item: String) {
    let key = structured_list_item_identity(&item);
    if key.is_empty() || items.iter().any(|existing| structured_list_item_identity(existing) == key)
    {
        return;
    }
    items.push(item);
}

fn structured_list_item_identity(item: &str) -> String {
    normalized_alnum_tokens(item, 2).into_iter().collect::<Vec<_>>().join(" ")
}

fn structured_list_line_focus_score(line: &str, focus_terms: &BTreeSet<String>) -> usize {
    if focus_terms.is_empty() {
        return 0;
    }
    let tokens = normalized_alnum_tokens(line, 3).into_iter().collect::<BTreeSet<_>>();
    focus_terms
        .iter()
        .filter(|term| {
            tokens.contains(term.as_str()) || line.to_lowercase().contains(term.as_str())
        })
        .count()
}

#[derive(Debug, Clone, Copy)]
struct DeterministicAnswerLabels {
    variants: &'static str,
    source: &'static str,
    package: &'static str,
    reconfigure: &'static str,
    path: &'static str,
    section: &'static str,
    parameter: &'static str,
    parameter_details: &'static str,
    update_sequence: &'static str,
}

fn deterministic_answer_labels(question: &str, query_ir: &QueryIR) -> DeterministicAnswerLabels {
    let labels =
        i18n::deterministic_answer_labels(deterministic_answer_language(question, query_ir));
    DeterministicAnswerLabels {
        variants: labels.variants,
        source: labels.source,
        package: labels.package,
        reconfigure: labels.reconfigure,
        path: labels.path,
        section: labels.section,
        parameter: labels.parameter,
        parameter_details: labels.parameter_details,
        update_sequence: labels.update_sequence,
    }
}

fn deterministic_answer_language(question: &str, query_ir: &QueryIR) -> QueryLanguage {
    let _ = question;
    match query_ir.language {
        QueryLanguage::Auto => QueryLanguage::En,
        language => language,
    }
}

#[derive(Debug, Clone)]
pub(super) struct SetupConfigurationAnchorCandidate {
    answer: String,
    is_multi_variant: bool,
    has_parameter_details: bool,
    has_actionable_anchor: bool,
}

impl SetupConfigurationAnchorCandidate {
    pub(super) fn into_answer(self) -> String {
        self.answer
    }

    pub(super) fn is_multi_variant(&self) -> bool {
        self.is_multi_variant
    }

    pub(super) fn has_parameter_details(&self) -> bool {
        self.has_parameter_details
    }

    pub(super) fn should_use_as_direct_answer(
        &self,
        query_ir: &QueryIR,
        chunks: &[RuntimeMatchedChunk],
    ) -> bool {
        self.should_use_as_preflight_answer(query_ir, chunks)
            || (query_ir_has_setup_configuration_target(query_ir) && self.has_actionable_anchor)
    }

    pub(super) fn should_use_as_preflight_answer(
        &self,
        query_ir: &QueryIR,
        chunks: &[RuntimeMatchedChunk],
    ) -> bool {
        query_has_multi_document_setup_anchors(query_ir, chunks)
            || (query_ir.document_focus.is_none() && self.is_multi_variant())
            || self.has_parameter_details()
    }
}

pub(super) fn build_setup_configuration_anchor_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_setup_configuration_anchor_candidate(question, query_ir, chunks)
        .map(SetupConfigurationAnchorCandidate::into_answer)
}

pub(super) fn build_setup_configuration_anchor_candidate(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<SetupConfigurationAnchorCandidate> {
    const SETUP_CONFIGURATION_RENDER_VARIANT_LIMIT: usize = 8;

    if !query_ir_requests_setup_configuration_answer(question, query_ir) {
        return None;
    }
    let variants = setup_configuration_variants_from_chunks(question, query_ir, chunks);
    if variants.is_empty() {
        return None;
    }
    let labels = deterministic_answer_labels(question, query_ir);
    let is_multi_variant = setup_configuration_has_distinct_variants(&variants);
    let has_parameter_details = variants.iter().any(|variant| !variant.parameter_rows.is_empty());
    let has_actionable_anchor = variants.iter().any(|variant| {
        !variant.packages.is_empty()
            || !variant.reconfigure_packages.is_empty()
            || !variant.paths.is_empty()
    });
    let mut lines = Vec::new();
    if is_multi_variant {
        lines.push(format!("**{}:**", labels.variants));
        lines.push(String::new());
    }

    for variant in variants.iter().take(SETUP_CONFIGURATION_RENDER_VARIANT_LIMIT) {
        lines.push(format!("**{}:** **{}**", labels.source, variant.source));
        if !variant.packages.is_empty() {
            lines.push(format!("- **{}:** `{}`", labels.package, variant.packages.join("`, `")));
        }
        if !variant.reconfigure_packages.is_empty() {
            let commands = variant.reconfigure_packages.join("`, `");
            lines.push(format!("- **{}:** `{commands}`", labels.reconfigure));
        }
        if !variant.paths.is_empty() {
            lines.push(format!("- **{}:** `{}`", labels.path, variant.paths.join("`, `")));
        }
        if !variant.sections.is_empty() {
            let sections = variant
                .sections
                .iter()
                .map(|section| format!("[{section}]"))
                .collect::<Vec<_>>()
                .join("`, `");
            lines.push(format!("- **{}:** `{sections}`", labels.section));
        }
        if !variant.parameters.is_empty() {
            lines.push(format!(
                "- **{}:** `{}`",
                labels.parameter,
                variant.parameters.join("`, `")
            ));
        }
        if !variant.parameter_rows.is_empty() {
            lines.push(format!("- **{}:**", labels.parameter_details));
            for row in &variant.parameter_rows {
                lines.push(format!("  - `{}` — {}", row.name, row.render_details(labels)));
            }
        }
        lines.push(String::new());
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    Some(SetupConfigurationAnchorCandidate {
        answer: lines.join("\n"),
        is_multi_variant,
        has_parameter_details,
        has_actionable_anchor,
    })
}

fn query_ir_requests_setup_configuration_answer(question: &str, query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow) || query_ir.source_slice.is_some() {
        return false;
    }
    if query_ir_has_relational_focus(query_ir) {
        return false;
    }
    if query_ir_is_unambiguous_versioned_procedure(query_ir) {
        return false;
    }
    if query_ir_requests_service_port_answer(query_ir) {
        return false;
    }
    if query_ir_has_setup_configuration_target(query_ir) || query_ir.document_focus.is_some() {
        return true;
    }
    if !query_ir_has_subject_focus_identity(query_ir) {
        return false;
    }
    !question_requests_lifecycle_update_procedure(question, query_ir)
}

fn question_requests_lifecycle_update_procedure(question: &str, query_ir: &QueryIR) -> bool {
    if query_ir_is_unambiguous_versioned_procedure(query_ir) {
        return true;
    }
    if query_ir.target_types.iter().any(|target_type| {
        let normalized = target_type.trim().to_lowercase();
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "version" | "release" | "changelog"
        ) || matches!(normalized.as_str(), "version" | "release" | "changelog")
    }) {
        return true;
    }
    if query_ir.source_slice.as_ref().is_some_and(|slice| {
        matches!(slice.filter, crate::domains::query_ir::SourceSliceFilter::ReleaseMarker)
    }) {
        return true;
    }
    if extract_semver_like_version(question).is_some()
        || extract_release_context_version(question).is_some()
        || query_ir.retrieval_query.as_deref().is_some_and(|query| {
            extract_semver_like_version(query).is_some()
                || extract_release_context_version(query).is_some()
        })
    {
        return true;
    }
    update_procedure_focus_model(question, query_ir)
        .procedure_terms
        .iter()
        .any(|term| lifecycle_update_action_term(term))
}

fn lifecycle_update_action_term(term: &str) -> bool {
    let term = term.trim().to_lowercase();
    matches!(
        term.as_str(),
        "update"
            | "updates"
            | "updated"
            | "updating"
            | "upgrade"
            | "upgrades"
            | "upgraded"
            | "upgrading"
            | "version"
            | "versions"
            | "release"
            | "releases"
            | "релиз"
            | "релизы"
            | "релизов"
            | "версия"
            | "версии"
            | "версий"
    ) || term.starts_with("обнов")
}

fn query_ir_has_relational_focus(query_ir: &QueryIR) -> bool {
    query_ir.target_entities.iter().any(|entity| !matches!(entity.role, EntityRole::Subject))
}

fn query_ir_has_subject_focus_identity(query_ir: &QueryIR) -> bool {
    query_ir
        .target_entities
        .iter()
        .any(|entity| matches!(entity.role, EntityRole::Subject) && !entity.label.trim().is_empty())
}

fn query_ir_requests_service_port_answer(query_ir: &QueryIR) -> bool {
    query_ir
        .target_types
        .iter()
        .map(|target_type| canonical_target_type_tag(target_type))
        .any(|tag| matches!(tag.as_str(), "connection" | "port" | "protocol" | "service"))
}

#[derive(Debug, Clone)]
struct SetupConfigurationVariant {
    source: String,
    score: usize,
    focus_score: usize,
    label_focus_score: usize,
    packages: Vec<String>,
    reconfigure_packages: Vec<String>,
    paths: Vec<String>,
    sections: Vec<String>,
    parameters: Vec<String>,
    parameter_rows: Vec<SetupConfigurationParameterRow>,
}

#[derive(Debug, Clone)]
struct SetupConfigurationParameterRow {
    name: String,
    details: Vec<(String, String)>,
}

impl SetupConfigurationParameterRow {
    fn render_details(&self, _labels: DeterministicAnswerLabels) -> String {
        self.details
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn simple_default_assignment(&self) -> Option<String> {
        let default = self
            .details
            .iter()
            .find_map(|(key, value)| {
                (setup_configuration_literal_key(key) == "default").then_some(value.as_str())
            })?
            .trim();
        if !setup_configuration_default_value_is_assignment_scalar(default) {
            return None;
        }
        Some(format!("{} = {default}", self.name))
    }
}

fn setup_configuration_variants_from_chunks(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<SetupConfigurationVariant> {
    const SETUP_CONFIGURATION_PARAMETER_LITERAL_LIMIT: usize = 24;
    const SETUP_CONFIGURATION_PARAMETER_ROW_LIMIT: usize = 16;

    let focus_tokens = setup_configuration_focus_tokens(question, query_ir);
    let subject_label_sequences = setup_configuration_subject_label_sequences(query_ir);
    let subject_tokens = setup_configuration_subject_tokens(query_ir);
    let mut variants = BTreeMap::<Uuid, SetupConfigurationVariant>::new();
    for chunk in chunks {
        let text =
            repair_technical_layout_noise(&format!("{}\n{}", chunk.source_text, chunk.excerpt));
        let packages = extract_package_command_literals(&text, 4);
        let paths = extract_configuration_path_literals(&text, 4);
        let sections = extract_configuration_section_literals(&text, 4);
        let reconfigure_packages = extract_setup_configuration_command_literals(&text, 6);
        let parameter_rows = extract_setup_configuration_parameter_rows(
            &text,
            SETUP_CONFIGURATION_PARAMETER_ROW_LIMIT,
        );
        let parameters = filter_setup_configuration_parameters(
            extract_config_assignment_literals(&text, SETUP_CONFIGURATION_PARAMETER_LITERAL_LIMIT)
                .into_iter()
                .chain(parameter_rows.iter().filter_map(|row| row.simple_default_assignment()))
                .chain(extract_parameter_literals(
                    &text,
                    SETUP_CONFIGURATION_PARAMETER_LITERAL_LIMIT,
                ))
                .chain(parameter_rows.iter().map(|row| row.name.clone()))
                .collect(),
            &packages,
            &reconfigure_packages,
            &paths,
            &sections,
        );
        let focus_score = setup_configuration_focus_score(&focus_tokens, chunk, &text);
        let label_focus_score =
            setup_configuration_label_focus_score(&subject_label_sequences, chunk);
        if packages.is_empty()
            && reconfigure_packages.is_empty()
            && paths.is_empty()
            && sections.is_empty()
            && parameters.is_empty()
        {
            continue;
        }
        if !focus_tokens.is_empty() && focus_score == 0 && label_focus_score == 0 {
            continue;
        }
        let score = setup_configuration_variant_score(
            &packages,
            &reconfigure_packages,
            &paths,
            &sections,
            &parameters,
            &parameter_rows,
            chunk,
        );
        let parameter_only_chunk = packages.is_empty()
            && reconfigure_packages.is_empty()
            && paths.is_empty()
            && sections.is_empty();
        if score < 16 && !parameter_only_chunk {
            continue;
        }
        let entry =
            variants.entry(chunk.document_id).or_insert_with(|| SetupConfigurationVariant {
                source: chunk.document_label.clone(),
                score: 0,
                focus_score: 0,
                label_focus_score: 0,
                packages: Vec::new(),
                reconfigure_packages: Vec::new(),
                paths: Vec::new(),
                sections: Vec::new(),
                parameters: Vec::new(),
                parameter_rows: Vec::new(),
            });
        entry.score = entry.score.saturating_add(score);
        entry.focus_score = entry.focus_score.saturating_add(focus_score);
        entry.label_focus_score = entry.label_focus_score.max(label_focus_score);
        push_unique_values(&mut entry.packages, packages, 4);
        push_unique_values(&mut entry.reconfigure_packages, reconfigure_packages, 4);
        push_unique_values(&mut entry.paths, paths, 4);
        push_unique_values(&mut entry.sections, sections, 4);
        push_unique_values(
            &mut entry.parameters,
            parameters,
            SETUP_CONFIGURATION_PARAMETER_LITERAL_LIMIT,
        );
        push_unique_parameter_rows(
            &mut entry.parameter_rows,
            parameter_rows,
            SETUP_CONFIGURATION_PARAMETER_ROW_LIMIT,
        );
    }
    let mut variants =
        variants.into_values().filter(setup_configuration_variant_has_anchor).collect::<Vec<_>>();
    let has_label_focus = !subject_label_sequences.is_empty()
        && variants.iter().any(|variant| variant.label_focus_score > 0);
    if has_label_focus {
        variants.retain(|variant| variant.label_focus_score > 0);
    } else if variants.iter().any(|variant| variant.focus_score > 0) {
        variants.retain(|variant| variant.focus_score > 0);
    }
    variants.sort_by(|left, right| {
        if has_label_focus {
            right
                .label_focus_score
                .cmp(&left.label_focus_score)
                .then_with(|| right.focus_score.cmp(&left.focus_score))
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| left.source.cmp(&right.source))
        } else if focus_tokens.is_empty() {
            right.score.cmp(&left.score).then_with(|| left.source.cmp(&right.source))
        } else {
            right
                .focus_score
                .cmp(&left.focus_score)
                .then_with(|| right.score.cmp(&left.score))
                .then_with(|| left.source.cmp(&right.source))
        }
    });
    let diversity_terms = if subject_tokens.is_empty() { &focus_tokens } else { &subject_tokens };
    diversify_setup_configuration_variants(variants, diversity_terms)
}

fn setup_configuration_variant_has_anchor(variant: &SetupConfigurationVariant) -> bool {
    !variant.packages.is_empty()
        || !variant.reconfigure_packages.is_empty()
        || !variant.paths.is_empty()
        || (!variant.sections.is_empty() && !variant.parameters.is_empty())
        || (!variant.sections.is_empty() && !variant.parameter_rows.is_empty())
}

fn setup_configuration_has_distinct_variants(variants: &[SetupConfigurationVariant]) -> bool {
    if variants.len() <= 1 {
        return false;
    }
    let anchor_sets =
        variants.iter().map(setup_configuration_variant_anchor_keys).collect::<Vec<_>>();
    for left_index in 0..anchor_sets.len() {
        for right_index in (left_index + 1)..anchor_sets.len() {
            if anchor_sets[left_index]
                .iter()
                .any(|anchor| !anchor.is_empty() && anchor_sets[right_index].contains(anchor))
            {
                return false;
            }
        }
    }
    true
}

fn setup_configuration_variant_anchor_keys(
    variant: &SetupConfigurationVariant,
) -> BTreeSet<String> {
    variant
        .packages
        .iter()
        .chain(&variant.reconfigure_packages)
        .chain(&variant.paths)
        .map(|value| setup_configuration_literal_key(value))
        .filter(|value| !value.is_empty())
        .collect()
}

fn diversify_setup_configuration_variants(
    variants: Vec<SetupConfigurationVariant>,
    query_terms: &BTreeSet<String>,
) -> Vec<SetupConfigurationVariant> {
    if variants.len() <= 2 {
        return variants;
    }
    let family_model = setup_configuration_variant_family_model(&variants, query_terms);
    let mut selected = Vec::with_capacity(variants.len());
    let mut selected_families = BTreeSet::<String>::new();
    for variant in &variants {
        let family = setup_configuration_variant_family(variant, query_terms, &family_model);
        if selected_families.insert(family) {
            selected.push(variant.clone());
        }
    }
    for variant in variants {
        if selected.iter().any(|selected| selected.source == variant.source) {
            continue;
        }
        selected.push(variant);
    }
    selected
}

#[derive(Default)]
struct SetupConfigurationVariantFamilyModel {
    variant_count: usize,
    token_frequency: BTreeMap<String, usize>,
}

fn setup_configuration_variant_family_model(
    variants: &[SetupConfigurationVariant],
    query_terms: &BTreeSet<String>,
) -> SetupConfigurationVariantFamilyModel {
    let mut token_frequency = BTreeMap::<String, usize>::new();
    let mut variant_count = 0usize;
    for variant in variants {
        let tokens = setup_configuration_variant_family_tokens(&variant.source, query_terms);
        if tokens.is_empty() {
            continue;
        }
        variant_count = variant_count.saturating_add(1);
        for token in tokens {
            *token_frequency.entry(token).or_default() += 1;
        }
    }
    SetupConfigurationVariantFamilyModel { variant_count, token_frequency }
}

fn setup_configuration_variant_family(
    variant: &SetupConfigurationVariant,
    query_terms: &BTreeSet<String>,
    family_model: &SetupConfigurationVariantFamilyModel,
) -> String {
    let tokens = setup_configuration_variant_family_tokens(&variant.source, query_terms);
    if let Some(token) = tokens.iter().find(|token| {
        family_model
            .token_frequency
            .get(*token)
            .is_some_and(|frequency| *frequency < family_model.variant_count)
    }) {
        return token.clone();
    }
    if !tokens.is_empty() {
        return tokens.join(" ");
    }
    variant.source.to_lowercase()
}

fn setup_configuration_variant_family_tokens(
    source: &str,
    query_terms: &BTreeSet<String>,
) -> Vec<String> {
    normalized_alnum_tokens(source, 3)
        .into_iter()
        .filter(|token| !query_terms.contains(token))
        .collect()
}

fn setup_configuration_focus_tokens(question: &str, query_ir: &QueryIR) -> BTreeSet<String> {
    let current_question =
        crate::services::query::effective_query::current_question_segment(question);
    let mut tokens = normalized_alnum_tokens(current_question, 3);
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    for entity in &query_ir.target_entities {
        tokens.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    tokens
}

fn setup_configuration_subject_tokens(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    for entity in &query_ir.target_entities {
        tokens.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    tokens
}

fn setup_configuration_subject_label_sequences(query_ir: &QueryIR) -> Vec<Vec<String>> {
    let mut sequences = Vec::new();
    let mut seen = BTreeSet::<Vec<String>>::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_setup_configuration_subject_label_sequence(
            &mut sequences,
            &mut seen,
            &document_focus.hint,
        );
    }
    for entity in &query_ir.target_entities {
        push_setup_configuration_subject_label_sequence(&mut sequences, &mut seen, &entity.label);
    }
    sequences
}

fn push_setup_configuration_subject_label_sequence(
    sequences: &mut Vec<Vec<String>>,
    seen: &mut BTreeSet<Vec<String>>,
    label: &str,
) {
    let sequence = normalized_alnum_token_sequence(label, 2);
    if !sequence.is_empty() && seen.insert(sequence.clone()) {
        sequences.push(sequence);
    }
}

fn setup_configuration_focus_score(
    focus_tokens: &BTreeSet<String>,
    chunk: &RuntimeMatchedChunk,
    text: &str,
) -> usize {
    if focus_tokens.is_empty() {
        return 0;
    }
    let mut available = normalized_alnum_tokens(&chunk.document_label, 3);
    available.extend(normalized_alnum_tokens(text, 3));
    focus_tokens.intersection(&available).count()
}

fn setup_configuration_label_focus_score(
    subject_label_sequences: &[Vec<String>],
    chunk: &RuntimeMatchedChunk,
) -> usize {
    if subject_label_sequences.is_empty() {
        return 0;
    }
    let label_sequence = normalized_alnum_token_sequence(&chunk.document_label, 2);
    subject_label_sequences
        .iter()
        .filter(|subject_sequence| {
            token_sequence_exact_or_contains_tokens(&label_sequence, subject_sequence)
        })
        .map(Vec::len)
        .sum()
}

fn extract_configuration_path_literals(text: &str, limit: usize) -> Vec<String> {
    extract_explicit_path_literals(text, limit.saturating_mul(2).max(4))
        .into_iter()
        .filter(|path| {
            let lowered = path.to_ascii_lowercase();
            [".conf", ".ini", ".cfg", ".properties", ".yaml", ".yml", ".toml", ".json"]
                .iter()
                .any(|extension| lowered.ends_with(extension))
        })
        .take(limit)
        .collect()
}

fn extract_configuration_section_literals(text: &str, limit: usize) -> Vec<String> {
    let mut sections = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let mut rest = line;
        while let Some(start) = rest.find('[') {
            rest = &rest[start + 1..];
            let Some(end) = rest.find(']') else {
                break;
            };
            let section = rest[..end].trim();
            rest = &rest[end + 1..];
            if !configuration_section_literal_is_plausible(section) {
                continue;
            }
            let value = section.to_string();
            if seen.insert(value.to_lowercase()) {
                sections.push(value);
                if sections.len() >= limit {
                    return sections;
                }
            }
        }
    }
    sections
}

fn configuration_section_literal_is_plausible(section: &str) -> bool {
    let section = section.trim();
    if section.is_empty() {
        return false;
    }
    let mut has_alpha = false;
    let mut has_separator = false;
    let mut char_count = 0usize;
    for ch in section.chars() {
        char_count += 1;
        if ch.is_alphabetic() {
            has_alpha = true;
        } else if matches!(ch, '_' | '-' | '.') {
            has_separator = true;
        } else if !ch.is_alphanumeric() {
            return false;
        }
    }
    has_alpha && (has_separator || char_count >= 3)
}

fn extract_setup_configuration_command_literals(text: &str, limit: usize) -> Vec<String> {
    let mut commands = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines().flat_map(split_dense_procedure_line) {
        for command in setup_configuration_command_literal_candidates(&line) {
            let key = setup_configuration_literal_key(&command);
            if seen.insert(key) {
                commands.push(command);
                if commands.len() >= limit {
                    return commands;
                }
            }
        }
    }
    commands
}

fn setup_configuration_command_literal_candidates(line: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let command = strip_leading_order_marker(line).trim().to_string();
    if setup_configuration_command_literal_is_usable(&command) {
        candidates.push(command);
    }
    for embedded in setup_configuration_embedded_command_literals(line) {
        if setup_configuration_command_literal_is_usable(&embedded) {
            candidates.push(embedded);
        }
    }
    candidates
}

fn setup_configuration_embedded_command_literals(line: &str) -> Vec<String> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 3 {
        return Vec::new();
    }
    let mut commands = Vec::new();
    for start in 1..tokens.len() {
        if !setup_configuration_embedded_command_start_is_plausible(tokens[start]) {
            continue;
        }
        let candidate = tokens[start..].join(" ");
        if candidate.split_whitespace().count() < 2 || !line_has_command_signal(&candidate) {
            continue;
        }
        for segment in split_dense_procedure_line(&candidate) {
            let segment = strip_leading_order_marker(&segment)
                .trim()
                .trim_end_matches(['.', ',', ';'])
                .trim()
                .to_string();
            if setup_configuration_command_literal_is_usable(&segment) {
                commands.push(segment);
            }
        }
        if !commands.is_empty() {
            break;
        }
    }
    commands
}

fn setup_configuration_embedded_command_start_is_plausible(token: &str) -> bool {
    let normalized = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
    matches!(normalized.as_str(), "sudo" | "su" | "doas")
        || token_is_command_start(&normalized)
        || token_is_local_script_command_start(&normalized)
}

fn setup_configuration_command_literal_is_usable(command: &str) -> bool {
    !command.is_empty()
        && !setup_configuration_command_is_table_row(command)
        && !setup_configuration_command_is_commented_fragment(command)
        && !setup_configuration_command_is_standalone_path(command)
        && !setup_configuration_command_is_standalone_assignment(command)
        && setup_configuration_command_has_invocable_head(command)
        && line_has_command_signal(command)
}

fn setup_configuration_command_has_invocable_head(command: &str) -> bool {
    let mut tokens = strip_leading_order_marker(command)
        .split_whitespace()
        .map(|token| {
            trim_command_boundary_token_decorations(token)
                .trim_matches(|ch| matches!(ch, '[' | ']'))
                .to_ascii_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    while tokens
        .first()
        .is_some_and(|token| matches!(token.as_str(), "sudo" | "su" | "env" | "command"))
    {
        tokens.remove(0);
    }
    let Some(head) = tokens.first() else {
        return false;
    };
    if !setup_configuration_command_head_is_ascii_invocable(head) {
        return false;
    }
    if matches!(head.as_str(), "sudo" | "su" | "doas") || token_is_local_script_command_start(head)
    {
        return true;
    }
    setup_configuration_command_has_near_structural_argument(&tokens, token_is_command_start(head))
}

fn setup_configuration_command_has_near_structural_argument(
    tokens: &[String],
    executable_shaped_head: bool,
) -> bool {
    if tokens.get(1).is_some_and(|token| command_token_is_structural_argument(token)) {
        return true;
    }
    executable_shaped_head
        && tokens.get(2).is_some_and(|token| command_token_is_structural_argument(token))
}

fn setup_configuration_command_head_is_ascii_invocable(head: &str) -> bool {
    let head = trim_command_boundary_token_decorations(head);
    !head.is_empty()
        && !head.starts_with('-')
        && !head.contains("://")
        && head.chars().any(|ch| ch.is_ascii_alphabetic())
        && head.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-' | '+' | '/' | '\\')
        })
}

fn setup_configuration_command_is_table_row(command: &str) -> bool {
    command.matches('|').count() >= 2
}

fn setup_configuration_command_is_commented_fragment(command: &str) -> bool {
    let trimmed = command.trim_start();
    matches!(trimmed.chars().next(), Some(';' | '#')) && trimmed.contains(['=', ':', '['])
}

fn setup_configuration_command_is_standalone_path(command: &str) -> bool {
    let trimmed = command.trim().trim_matches('`').trim();
    if trimmed.split_whitespace().count() != 1 {
        return false;
    }
    extract_configuration_path_literals(trimmed, 1).into_iter().any(|path| path == trimmed)
}

fn setup_configuration_command_is_standalone_assignment(command: &str) -> bool {
    let Some((left, right)) = command.split_once('=').or_else(|| command.split_once(':')) else {
        return false;
    };
    let left = left.trim().trim_matches(|ch| matches!(ch, '"' | '\'' | '`'));
    let right = right.trim();
    if left.is_empty() || right.is_empty() || left.split_whitespace().count() != 1 {
        return false;
    }
    left.chars().any(char::is_alphabetic)
        && left.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn filter_setup_configuration_parameters(
    parameters: Vec<String>,
    packages: &[String],
    reconfigure_packages: &[String],
    paths: &[String],
    sections: &[String],
) -> Vec<String> {
    let blocked = packages
        .iter()
        .chain(reconfigure_packages)
        .chain(paths)
        .chain(sections)
        .map(|value| setup_configuration_literal_key(value))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    let command_heads = reconfigure_packages
        .iter()
        .filter_map(|value| setup_configuration_command_head_key(value))
        .collect::<BTreeSet<_>>();
    parameters
        .into_iter()
        .filter(|parameter| {
            let key = setup_configuration_literal_key(parameter);
            !key.is_empty() && !blocked.contains(&key) && !command_heads.contains(&key)
        })
        .collect()
}

fn setup_configuration_command_head_key(command: &str) -> Option<String> {
    let mut tokens = strip_leading_order_marker(command)
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    '`' | '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | ';' | ','
                )
            })
        })
        .filter(|token| !token.is_empty());
    let mut head = tokens.next()?;
    while matches!(head, "sudo" | "su") {
        head = tokens.next()?;
    }
    let key = setup_configuration_literal_key(head);
    (!key.is_empty()).then_some(key)
}

fn extract_setup_configuration_parameter_rows(
    text: &str,
    limit: usize,
) -> Vec<SetupConfigurationParameterRow> {
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    for line in text.lines() {
        let Some(row) = parse_setup_configuration_parameter_row(line)
            .or_else(|| parse_setup_configuration_plain_parameter_row(line))
        else {
            continue;
        };
        if seen.insert(setup_configuration_literal_key(&row.name)) {
            rows.push(row);
            if rows.len() >= limit {
                break;
            }
        }
    }
    rows
}

fn parse_setup_configuration_parameter_row(line: &str) -> Option<SetupConfigurationParameterRow> {
    if !line.contains('|') || !line.contains(':') {
        return None;
    }
    let cells = line
        .split('|')
        .filter_map(|cell| {
            let (raw_key, raw_value) = cell.split_once(':')?;
            let key = setup_configuration_table_key(raw_key);
            let value = clean_setup_configuration_row_value(raw_value);
            (!key.is_empty() && !value.is_empty()).then_some((key, value))
        })
        .collect::<Vec<_>>();
    if cells.len() < 2 {
        return None;
    }
    let name_index = cells
        .iter()
        .position(|(_, value)| setup_configuration_row_value_is_parameter_identifier(value))
        .or_else(|| {
            cells
                .iter()
                .position(|(_, value)| setup_configuration_row_value_can_name_parameter(value))
        })?;
    let name = cells[name_index].1.clone();
    let details = cells
        .into_iter()
        .enumerate()
        .filter_map(|(index, (key, value))| {
            (index != name_index && !setup_configuration_literal_key(&value).is_empty())
                .then_some((key, value))
        })
        .take(4)
        .collect::<Vec<_>>();
    let key = setup_configuration_literal_key(&name);
    if key.is_empty() || details.is_empty() {
        return None;
    }
    Some(SetupConfigurationParameterRow { name, details })
}

fn parse_setup_configuration_plain_parameter_row(
    line: &str,
) -> Option<SetupConfigurationParameterRow> {
    if line.matches('|').count() < 3 || line.contains(':') {
        return None;
    }
    let cells = line
        .split('|')
        .map(clean_setup_configuration_row_value)
        .filter(|cell| !cell.is_empty())
        .collect::<Vec<_>>();
    if cells.len() < 4 || cells.iter().all(|cell| cell.chars().all(|ch| matches!(ch, '-'))) {
        return None;
    }
    let name = cells.first()?.clone();
    if !setup_configuration_row_value_is_parameter_identifier(&name) {
        return None;
    }
    let default = cells.last()?.clone();
    if !setup_configuration_default_value_is_assignment_scalar(&default) {
        return None;
    }
    Some(SetupConfigurationParameterRow { name, details: vec![("Default".to_string(), default)] })
}

fn setup_configuration_default_value_is_assignment_scalar(value: &str) -> bool {
    let trimmed = value.trim().trim_matches('`').trim();
    if trimmed.is_empty() || trimmed.chars().count() > 80 || trimmed.contains("://") {
        return false;
    }
    if matches!(trimmed, "true" | "false") {
        return true;
    }
    let numeric = trimmed.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_'));
    numeric && trimmed.chars().any(|ch| ch.is_ascii_digit())
}

fn setup_configuration_row_value_can_name_parameter(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.chars().count() < 2 || trimmed.chars().count() > 120 {
        return false;
    }
    if trimmed.split_whitespace().count() > 8 {
        return false;
    }
    trimmed.chars().any(|ch| ch.is_alphabetic() || ch == '_' || ch == '.' || ch == '-')
}

fn setup_configuration_row_value_is_parameter_identifier(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.chars().count() < 2
        || trimmed.chars().count() > 80
        || trimmed.chars().any(char::is_whitespace)
        || !trimmed.chars().any(char::is_alphabetic)
    {
        return false;
    }
    trimmed.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

fn setup_configuration_table_key(value: &str) -> String {
    value.trim().trim_matches(|ch: char| !ch.is_alphanumeric() && ch != ' ').to_string()
}

fn clean_setup_configuration_row_value(value: &str) -> String {
    value.trim().trim_matches('|').trim().trim_matches('`').trim().to_string()
}

fn setup_configuration_literal_key(value: &str) -> String {
    normalized_alnum_tokens(value, 1).into_iter().collect::<Vec<_>>().join(" ")
}

fn setup_configuration_variant_score(
    packages: &[String],
    reconfigure_packages: &[String],
    paths: &[String],
    sections: &[String],
    parameters: &[String],
    parameter_rows: &[SetupConfigurationParameterRow],
    chunk: &RuntimeMatchedChunk,
) -> usize {
    packages
        .len()
        .saturating_mul(24)
        .saturating_add(reconfigure_packages.len().saturating_mul(18))
        .saturating_add(paths.len().saturating_mul(24))
        .saturating_add(sections.len().saturating_mul(16))
        .saturating_add(parameters.len().saturating_mul(3))
        .saturating_add(parameter_rows.len().saturating_mul(8))
        .saturating_add((chunk_is_setup_focus_command_path_anchor(chunk) as usize) * 32)
}

fn push_unique_values(target: &mut Vec<String>, values: Vec<String>, limit: usize) {
    let mut seen = target.iter().map(|value| value.to_lowercase()).collect::<BTreeSet<_>>();
    for value in values {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if seen.insert(trimmed.to_lowercase()) {
            target.push(trimmed.to_string());
            if target.len() >= limit {
                break;
            }
        }
    }
}

fn push_unique_parameter_rows(
    target: &mut Vec<SetupConfigurationParameterRow>,
    values: Vec<SetupConfigurationParameterRow>,
    limit: usize,
) {
    let mut seen = target
        .iter()
        .map(|row| setup_configuration_literal_key(&row.name))
        .collect::<BTreeSet<_>>();
    for row in values {
        let key = setup_configuration_literal_key(&row.name);
        if key.is_empty() || !seen.insert(key) {
            continue;
        }
        target.push(row);
        if target.len() >= limit {
            break;
        }
    }
}

pub(super) fn build_update_procedure_sequence_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe) {
        return None;
    }
    let focus_model = update_procedure_focus_model(question, query_ir);
    if !question_requests_update_procedure_answer(query_ir, &focus_model) {
        return None;
    }
    let selection = update_procedure_steps_from_chunks(&focus_model, chunks)?;
    if selection.steps.len() < 2 {
        return None;
    }
    let labels = deterministic_answer_labels(question, query_ir);

    let mut lines = vec![
        format!("**{}:** **{}**", labels.source, selection.source),
        String::new(),
        format!("**{}:**", labels.update_sequence),
    ];
    for (index, step) in selection.steps.iter().enumerate() {
        lines.push(render_update_procedure_step(index + 1, step));
    }
    Some(lines.join("\n"))
}

fn render_update_procedure_step(index: usize, step: &str) -> String {
    let stripped = strip_leading_order_marker(step).trim();
    if line_has_command_signal(stripped) {
        let command = stripped.trim_end_matches(['.', ',', ';']).trim();
        format!("{index}. `{command}`")
    } else {
        format!("{index}. {stripped}")
    }
}

fn question_requests_update_procedure_answer(
    query_ir: &QueryIR,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    let allows_retrieved_single_document =
        query_ir_allows_retrieved_single_document_procedure_sequence(query_ir);
    let allows_raw_retrieved_single_document =
        query_ir_allows_raw_retrieved_single_document_procedure_sequence(query_ir, focus_model);
    let has_typed_procedure_action = query_ir_has_typed_procedure_action(query_ir, focus_model);
    query_ir.source_slice.is_none()
        && (query_ir_has_explicit_procedure_focus(query_ir)
            || allows_retrieved_single_document
            || allows_raw_retrieved_single_document)
        && ((has_typed_procedure_action && !query_ir_has_setup_configuration_target(query_ir))
            || query_ir_allows_procedure_runbook_target(query_ir)
            || allows_retrieved_single_document
            || allows_raw_retrieved_single_document)
}

fn query_ir_has_explicit_procedure_focus(query_ir: &QueryIR) -> bool {
    !query_ir.target_entities.is_empty() || query_ir.document_focus.is_some()
}

fn query_ir_has_typed_procedure_action(
    query_ir: &QueryIR,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    if focus_model.procedure_terms.is_empty()
        || !query_ir_has_explicit_procedure_focus(query_ir)
        || query_ir_has_setup_configuration_target(query_ir)
    {
        return false;
    }
    let mut has_procedure = false;
    let mut has_concept = false;
    for target_type in &query_ir.target_types {
        match canonical_target_type_tag(target_type).as_str() {
            "procedure" => has_procedure = true,
            "concept" => has_concept = true,
            _ => {}
        }
    }
    has_procedure && !has_concept
}

fn query_ir_allows_retrieved_single_document_procedure_sequence(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument)
        || query_ir.retrieval_query.as_deref().map(str::trim).is_none_or(str::is_empty)
        || query_ir.needs_clarification.is_some()
        || query_ir_has_setup_configuration_target(query_ir)
    {
        return false;
    }
    let mut has_procedure = false;
    let mut has_concept = false;
    let mut has_document_or_revision_signal = false;
    for target_type in &query_ir.target_types {
        match canonical_target_type_tag(target_type).as_str() {
            "procedure" => has_procedure = true,
            "concept" => has_concept = true,
            "artifact" | "document" | "entity" | "primary_heading" | "secondary_heading"
            | "version" | "release" => {
                has_document_or_revision_signal = true;
            }
            _ => {}
        }
    }
    let has_explicit_subject_focus =
        query_ir.document_focus.is_some() || !query_ir.target_entities.is_empty();
    let has_literal_focus = !query_ir.literal_constraints.is_empty();
    has_procedure
        && ((!has_concept && has_document_or_revision_signal)
            || (has_concept && (has_explicit_subject_focus || has_literal_focus)))
}

fn query_ir_allows_raw_retrieved_single_document_procedure_sequence(
    query_ir: &QueryIR,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        && query_ir.source_slice.is_none()
        && query_ir.needs_clarification.is_none()
        && query_ir.comparison.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.document_focus.is_none()
        && query_ir.literal_constraints.is_empty()
        && !query_ir_has_setup_configuration_target(query_ir)
        && focus_model.query_terms.len() >= 2
        && !focus_model.procedure_terms.is_empty()
        && !focus_model.target_identity_sequences.is_empty()
}

struct UpdateProcedureFocusModel {
    query_terms: BTreeSet<String>,
    subject_terms: BTreeSet<String>,
    subject_acronym_terms: BTreeSet<String>,
    procedure_terms: BTreeSet<String>,
    target_identity_sequences: Vec<UpdateProcedureIdentitySequence>,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct UpdateProcedureIdentitySequence {
    tokens: Vec<String>,
    priority: usize,
}

fn update_procedure_focus_model(question: &str, query_ir: &QueryIR) -> UpdateProcedureFocusModel {
    let current_question =
        crate::services::query::effective_query::current_question_segment(question);
    let mut query_terms =
        normalized_alnum_tokens(current_question, 3).into_iter().collect::<BTreeSet<_>>();
    let mut action_candidate_terms =
        normalized_alnum_tokens(current_question, PROCEDURE_ACTION_TOKEN_MIN_CHARS)
            .into_iter()
            .collect::<BTreeSet<_>>();
    let mut subject_terms = BTreeSet::<String>::new();
    let mut subject_acronym_terms = BTreeSet::<String>::new();
    for entity in &query_ir.target_entities {
        add_label_terms_with_acronyms(
            &mut subject_terms,
            &mut subject_acronym_terms,
            &entity.label,
            2,
        );
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        add_label_terms_with_acronyms(
            &mut subject_terms,
            &mut subject_acronym_terms,
            &document_focus.hint,
            2,
        );
    }
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        let current_retrieval_query =
            crate::services::query::effective_query::current_question_segment(retrieval_query);
        query_terms.extend(normalized_alnum_tokens(current_retrieval_query, 3));
        action_candidate_terms.extend(normalized_alnum_tokens(
            current_retrieval_query,
            PROCEDURE_ACTION_TOKEN_MIN_CHARS,
        ));
    }
    query_terms.extend(subject_terms.iter().cloned());
    query_terms.extend(subject_acronym_terms.iter().cloned());
    let target_identity_sequences = update_procedure_target_identity_token_sequences(
        current_question,
        query_ir.retrieval_query.as_deref(),
        query_ir,
    );
    let mut identity_exclusion_terms = BTreeSet::<String>::new();
    let mut identity_exclusion_acronym_terms = BTreeSet::<String>::new();
    for identity in &target_identity_sequences {
        let has_structured_identity = identity.priority > 10;
        let has_distinctive_raw_surface =
            update_procedure_raw_identity_sequence_has_distinctive_surface(
                current_question,
                &identity.tokens,
            ) || query_ir.retrieval_query.as_deref().is_some_and(|retrieval_query| {
                update_procedure_raw_identity_sequence_has_distinctive_surface(
                    retrieval_query,
                    &identity.tokens,
                )
            });
        if has_structured_identity || has_distinctive_raw_surface {
            add_label_terms_with_acronyms(
                &mut identity_exclusion_terms,
                &mut identity_exclusion_acronym_terms,
                &identity.tokens.join(" "),
                2,
            );
        }
        if has_structured_identity {
            add_label_terms_with_acronyms(
                &mut subject_terms,
                &mut subject_acronym_terms,
                &identity.tokens.join(" "),
                2,
            );
        }
    }
    let mut all_subject_terms = subject_terms.clone();
    all_subject_terms.extend(subject_acronym_terms.iter().cloned());
    all_subject_terms.extend(identity_exclusion_terms);
    all_subject_terms.extend(identity_exclusion_acronym_terms);
    let procedure_terms = action_candidate_terms
        .iter()
        .filter(|term| {
            !all_subject_terms
                .iter()
                .any(|subject_term| procedure_terms_match(term.as_str(), subject_term.as_str()))
        })
        .cloned()
        .collect::<BTreeSet<_>>();
    UpdateProcedureFocusModel {
        query_terms,
        subject_terms,
        subject_acronym_terms,
        procedure_terms,
        target_identity_sequences,
    }
}

fn update_procedure_target_identity_token_sequences(
    question: &str,
    retrieval_query: Option<&str>,
    query_ir: &QueryIR,
) -> Vec<UpdateProcedureIdentitySequence> {
    let mut seen = BTreeSet::<Vec<String>>::new();
    let mut sequences = Vec::new();
    for entity in &query_ir.target_entities {
        push_update_procedure_target_identity_sequence(
            &mut sequences,
            &mut seen,
            &entity.label,
            30,
        );
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_update_procedure_target_identity_sequence(
            &mut sequences,
            &mut seen,
            &document_focus.hint,
            20,
        );
    }
    for literal in &query_ir.literal_constraints {
        if matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other) {
            push_update_procedure_target_identity_sequence(
                &mut sequences,
                &mut seen,
                &literal.text,
                50,
            );
        }
    }
    if sequences.is_empty() {
        if let Some(retrieval_query) = retrieval_query {
            for sequence in update_procedure_raw_target_identity_token_sequences(retrieval_query) {
                push_update_procedure_target_identity_sequence_tokens(
                    &mut sequences,
                    &mut seen,
                    sequence,
                    10,
                );
            }
        }
        for sequence in update_procedure_raw_target_identity_token_sequences(question) {
            push_update_procedure_target_identity_sequence_tokens(
                &mut sequences,
                &mut seen,
                sequence,
                10,
            );
        }
    }
    sequences
}

fn update_procedure_raw_identity_sequence_has_distinctive_surface(
    text: &str,
    sequence: &[String],
) -> bool {
    if sequence.is_empty() {
        return false;
    }
    let surface_tokens = text
        .split_whitespace()
        .filter_map(|token| {
            let normalized = normalized_alnum_token_sequence(token, 1);
            (!normalized.is_empty()).then(|| {
                let distinctive = token.chars().any(|ch| {
                    ch.is_uppercase()
                        || ch.is_ascii_digit()
                        || matches!(ch, '-' | '_' | '.' | '/' | '\\')
                });
                (normalized, distinctive)
            })
        })
        .collect::<Vec<_>>();
    for start in 0..surface_tokens.len() {
        let mut tokens = Vec::<String>::new();
        let mut surface_count = 0usize;
        let mut distinctive_surface_count = 0usize;
        for (normalized, token_is_distinctive) in surface_tokens.iter().skip(start) {
            tokens.extend(normalized.iter().cloned());
            surface_count = surface_count.saturating_add(1);
            if *token_is_distinctive {
                distinctive_surface_count = distinctive_surface_count.saturating_add(1);
            }
            if tokens.len() > sequence.len() {
                break;
            }
            if tokens == sequence {
                return distinctive_surface_count > 0
                    && ((surface_count == 1 && sequence.len() >= 2)
                        || distinctive_surface_count == surface_count);
            }
        }
    }
    false
}

fn push_update_procedure_target_identity_sequence(
    sequences: &mut Vec<UpdateProcedureIdentitySequence>,
    seen: &mut BTreeSet<Vec<String>>,
    label: &str,
    priority: usize,
) {
    let mut sequence = normalized_alnum_token_sequence(label, 1);
    if sequence.len() < 2 {
        sequence = label_term_sequence(label, 1);
    }
    if sequence.len() < 2 {
        return;
    }
    if !seen.insert(sequence.clone()) {
        if let Some(existing) = sequences.iter_mut().find(|existing| existing.tokens == sequence) {
            existing.priority = existing.priority.max(priority);
        }
        return;
    }
    sequences.push(UpdateProcedureIdentitySequence { tokens: sequence, priority });
}

fn push_update_procedure_target_identity_sequence_tokens(
    sequences: &mut Vec<UpdateProcedureIdentitySequence>,
    seen: &mut BTreeSet<Vec<String>>,
    sequence: Vec<String>,
    priority: usize,
) {
    if !update_procedure_target_identity_sequence_is_usable(&sequence) {
        return;
    }
    if !seen.insert(sequence.clone()) {
        if let Some(existing) = sequences.iter_mut().find(|existing| existing.tokens == sequence) {
            existing.priority = existing.priority.max(priority);
        }
        return;
    }
    sequences.push(UpdateProcedureIdentitySequence { tokens: sequence, priority });
}

fn update_procedure_raw_target_identity_token_sequences(question: &str) -> Vec<Vec<String>> {
    let current = current_question_segment(question);
    let tokens = normalized_alnum_token_sequence(current, 1);
    if tokens.is_empty() {
        return Vec::new();
    }
    let mut seen = BTreeSet::<Vec<String>>::new();
    let mut sequences = Vec::<Vec<String>>::new();
    let max_window = tokens.len().min(6);
    for window_len in (2..=max_window).rev() {
        for window in tokens.windows(window_len) {
            let sequence = window.to_vec();
            if update_procedure_target_identity_sequence_is_usable(&sequence)
                && seen.insert(sequence.clone())
            {
                sequences.push(sequence);
            }
        }
    }
    sequences
}

fn update_procedure_target_identity_sequence_is_usable(sequence: &[String]) -> bool {
    sequence.len() >= 2
        && sequence.iter().map(|token| token.chars().count()).sum::<usize>() >= 7
        && sequence.iter().any(|token| token.chars().count() >= 3)
}

fn update_procedure_text_target_identity_priority(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    if focus_model.target_identity_sequences.is_empty() {
        return 0;
    }
    let text_sequence = normalized_alnum_token_sequence(text, 1);
    focus_model
        .target_identity_sequences
        .iter()
        .filter(|target_sequence| {
            token_sequence_contains_tokens(&text_sequence, &target_sequence.tokens)
                || update_procedure_fuzzy_token_sequence_contains_tokens(
                    &text_sequence,
                    &target_sequence.tokens,
                )
        })
        .map(|target_sequence| target_sequence.priority)
        .max()
        .unwrap_or_default()
}

const PROCEDURE_ACTION_TOKEN_MIN_CHARS: usize = 5;
const PROCEDURE_TARGET_IDENTITY_PREFIX_MIN_CHARS: usize = 5;
const UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_CHARS: usize = 480;
const UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_LINES: usize = 6;

fn update_procedure_text_has_bound_target_identity_runbook(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    if focus_model.target_identity_sequences.is_empty() || focus_model.procedure_terms.is_empty() {
        return false;
    }
    update_procedure_bound_target_identity_windows(text).into_iter().any(|window| {
        if update_procedure_text_target_identity_priority(&window, focus_model) == 0 {
            return false;
        }
        let tokens = normalized_alnum_tokens(&window, 2).into_iter().collect::<BTreeSet<_>>();
        procedure_term_overlap_score(&focus_model.procedure_terms, &tokens) > 0
            && update_procedure_window_has_bound_target_action_line(&window, focus_model)
            && (window.lines().any(line_has_command_signal)
                || !extract_explicit_path_literals(&window, 2).is_empty()
                || !extract_package_command_literals(&window, 2).is_empty())
    })
}

fn update_procedure_window_has_bound_target_action_line(
    window: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    let lines = window.lines().collect::<Vec<_>>();
    if lines.iter().any(|line| {
        if update_procedure_text_target_identity_priority(line, focus_model) == 0 {
            return false;
        }
        let tokens = normalized_alnum_tokens(line, 2).into_iter().collect::<BTreeSet<_>>();
        procedure_term_overlap_score(&focus_model.procedure_terms, &tokens) > 0
    }) {
        return true;
    }
    lines.windows(2).any(|pair| {
        let [left, right] = pair else {
            return false;
        };
        if update_procedure_line_has_sentence_boundary(left) {
            return false;
        }
        let merged = format!("{left} {right}");
        if update_procedure_text_target_identity_priority(&merged, focus_model) == 0 {
            return false;
        }
        let tokens = normalized_alnum_tokens(&merged, 2).into_iter().collect::<BTreeSet<_>>();
        procedure_term_overlap_score(&focus_model.procedure_terms, &tokens) > 0
    })
}

fn update_procedure_bound_target_identity_windows(text: &str) -> Vec<String> {
    let lines = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    let mut seen = BTreeSet::<String>::new();
    let mut windows = Vec::new();

    if lines.len() > 1 {
        for start in 0..lines.len() {
            let window = lines
                .iter()
                .skip(start)
                .take(UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_LINES)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if window.chars().count() <= UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_CHARS {
                push_update_procedure_bound_target_identity_window(&mut windows, &mut seen, window);
            }
        }
    } else if let Some(line) = lines.first() {
        if !update_procedure_line_has_sentence_boundary(line) {
            push_update_procedure_bound_target_identity_window(
                &mut windows,
                &mut seen,
                (*line).to_string(),
            );
        }
    }

    for line in &lines {
        for clause in update_procedure_sentence_clauses(line) {
            push_update_procedure_bound_target_identity_window(&mut windows, &mut seen, clause);
        }
    }
    windows
}

fn push_update_procedure_bound_target_identity_window(
    windows: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    window: String,
) {
    let normalized = window.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || !seen.insert(normalized.clone()) {
        return;
    }
    windows.push(window.trim().to_string());
}

fn update_procedure_sentence_clauses(text: &str) -> Vec<String> {
    let mut clauses = Vec::new();
    let mut start = 0usize;
    for (index, ch) in text.char_indices() {
        if !update_procedure_is_sentence_boundary(text, index, ch) {
            continue;
        }
        let clause = text[start..index].trim();
        if !clause.is_empty() {
            clauses.push(clause.to_string());
        }
        start = index.saturating_add(ch.len_utf8());
    }
    let clause = text[start..].trim();
    if !clause.is_empty() {
        clauses.push(clause.to_string());
    }
    clauses
}

fn update_procedure_line_has_sentence_boundary(text: &str) -> bool {
    text.char_indices().any(|(index, ch)| update_procedure_is_sentence_boundary(text, index, ch))
}

fn update_procedure_is_sentence_boundary(text: &str, index: usize, ch: char) -> bool {
    match ch {
        ';' | '!' | '?' => true,
        '.' => {
            let prev = text[..index].chars().next_back();
            let next_index = index.saturating_add(ch.len_utf8());
            let next = text[next_index..].chars().next();
            if prev.is_some_and(|prev| prev.is_ascii_digit())
                && next.is_some_and(|next| next.is_whitespace())
            {
                return false;
            }
            if next.is_some_and(|next| !next.is_whitespace()) {
                return false;
            }
            true
        }
        _ => false,
    }
}

fn update_procedure_fuzzy_token_sequence_contains_tokens(
    haystack_tokens: &[String],
    needle_tokens: &[String],
) -> bool {
    if needle_tokens.is_empty() || haystack_tokens.len() < needle_tokens.len() {
        return false;
    }
    haystack_tokens.windows(needle_tokens.len()).any(|window| {
        window.iter().zip(needle_tokens).all(|(haystack_token, needle_token)| {
            update_procedure_identity_tokens_match(haystack_token, needle_token)
        })
    })
}

fn update_procedure_identity_tokens_match(left: &str, right: &str) -> bool {
    left == right
        || (left.chars().count() >= PROCEDURE_TARGET_IDENTITY_PREFIX_MIN_CHARS
            && right.chars().count() >= PROCEDURE_TARGET_IDENTITY_PREFIX_MIN_CHARS
            && shared_procedure_prefix_len(left, right)
                >= PROCEDURE_TARGET_IDENTITY_PREFIX_MIN_CHARS)
}

#[derive(Debug, Clone)]
struct UpdateProcedureSelection {
    source: String,
    steps: Vec<String>,
    anchors: Vec<String>,
}

#[derive(Debug, Clone)]
struct UpdateProcedureExtract {
    block_index: usize,
    score: usize,
    steps: Vec<String>,
    block_text: String,
    action_text: String,
    command_count: usize,
    action_command_score: usize,
    script_artifact_family_score: usize,
    preparatory_command_score: usize,
    focus_aligned_command_score: usize,
    unfocused_command_score: usize,
    has_setup_script_signature: bool,
    is_focus_projection: bool,
}

#[derive(Debug, Clone)]
struct UpdateProcedureCandidate {
    exact_target_identity: bool,
    label_target_identity: bool,
    target_identity_priority: usize,
    target_identity_focus_score: usize,
    score: usize,
    command_count: usize,
    focused_structural_score: usize,
    selection: UpdateProcedureSelection,
}

fn update_procedure_steps_from_chunks(
    focus_model: &UpdateProcedureFocusModel,
    chunks: &[RuntimeMatchedChunk],
) -> Option<UpdateProcedureSelection> {
    let mut candidates = Vec::<UpdateProcedureCandidate>::new();
    for chunk in chunks {
        let label_target_identity_priority =
            update_procedure_text_target_identity_priority(&chunk.document_label, focus_model);
        let text = repair_technical_layout_noise(&update_procedure_chunk_text(chunk, &focus_model));
        let raw_extracts = update_procedure_extracts_from_text(&text, focus_model);
        let mut extracts = raw_extracts
            .iter()
            .filter(|&extract| {
                update_procedure_selection_matches_action(
                    extract,
                    focus_model,
                    &chunk.document_label,
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        if let Some(aggregate) = update_procedure_focus_aligned_command_aggregate(
            &raw_extracts,
            focus_model,
            &chunk.document_label,
        ) {
            extracts.push(aggregate);
        }
        let extracts = extracts.into_iter().collect::<Vec<_>>();
        let has_richer_extract = extracts.iter().any(|extract| extract.steps.len() >= 4);
        for extract in extracts {
            if has_richer_extract && extract.steps.len() < 4 {
                continue;
            }
            let label_focus_score =
                update_procedure_text_focus_score(&chunk.document_label, focus_model);
            let block_focus_score =
                update_procedure_text_focus_score(&extract.block_text, focus_model);
            let focused_structural_score =
                update_procedure_focused_structural_score(&extract.block_text, focus_model);
            let raw_block_target_identity_priority =
                update_procedure_text_target_identity_priority(&extract.block_text, focus_model);
            let block_target_identity_is_bound = raw_block_target_identity_priority > 0
                && update_procedure_text_has_bound_target_identity_runbook(
                    &extract.block_text,
                    focus_model,
                );
            if label_target_identity_priority == 0
                && raw_block_target_identity_priority > 0
                && !block_target_identity_is_bound
                && extract.command_count >= 2
            {
                continue;
            }
            let block_target_identity_priority =
                if block_target_identity_is_bound || extract.command_count < 2 {
                    raw_block_target_identity_priority
                } else {
                    0
                };
            let target_identity_priority =
                label_target_identity_priority.max(block_target_identity_priority);
            let exact_target_identity = target_identity_priority > 0;
            let target_identity_focus_score = if label_target_identity_priority > 0 {
                label_focus_score
            } else if block_target_identity_priority > 0 {
                block_focus_score
            } else {
                0
            };
            let exact_target_identity_bonus = if exact_target_identity
                && extract.steps.len() >= 4
                && extract.command_count >= 2
            {
                32768usize
            } else {
                8192usize
            };
            let unfocused_command_penalty =
                if extract.focus_aligned_command_score > 0 { 4096 } else { 256 };
            let score = extract
                .score
                .saturating_add(
                    usize::from(extract.is_focus_projection && extract.command_count >= 2)
                        .saturating_mul(65_536),
                )
                .saturating_add(
                    usize::from(exact_target_identity).saturating_mul(exact_target_identity_bonus),
                )
                .saturating_add(label_focus_score.saturating_mul(96))
                .saturating_add(target_identity_focus_score.saturating_mul(1024))
                .saturating_add(focused_structural_score.saturating_mul(160))
                .saturating_add(extract.steps.len().saturating_mul(24))
                .saturating_add(extract.command_count.saturating_mul(512))
                .saturating_add(update_procedure_command_candidate_bonus(extract.command_count))
                .saturating_add(extract.action_command_score.saturating_mul(2048))
                .saturating_add(
                    usize::from(
                        extract.script_artifact_family_score > 0 && extract.command_count >= 2,
                    )
                    .saturating_mul(32_768),
                )
                .saturating_add(extract.script_artifact_family_score.saturating_mul(4096))
                .saturating_add(extract.preparatory_command_score.saturating_mul(512))
                .saturating_add(extract.focus_aligned_command_score.saturating_mul(4096))
                .saturating_sub(
                    extract.unfocused_command_score.saturating_mul(unfocused_command_penalty),
                );
            let steps = update_procedure_steps_with_adjacent_same_head_preparation(
                extract.steps,
                &extract.block_text,
            );
            let anchors = update_procedure_evidence_anchors(&steps, &extract.block_text, 8);
            candidates.push(UpdateProcedureCandidate {
                exact_target_identity,
                label_target_identity: label_target_identity_priority > 0,
                target_identity_priority,
                target_identity_focus_score,
                score,
                command_count: extract.command_count,
                focused_structural_score,
                selection: UpdateProcedureSelection {
                    source: chunk.document_label.clone(),
                    steps,
                    anchors,
                },
            });
        }
    }
    let preferred_target_identity_priority = candidates
        .iter()
        .filter(|candidate| {
            candidate.exact_target_identity
                && update_procedure_candidate_has_target_identity_preference(candidate)
        })
        .map(|candidate| candidate.target_identity_priority)
        .max()
        .unwrap_or_default();
    if preferred_target_identity_priority > 0 {
        candidates.retain(|candidate| {
            candidate.target_identity_priority == preferred_target_identity_priority
                && update_procedure_candidate_has_target_identity_preference(candidate)
        });
    }
    let has_label_target_identity = candidates.iter().any(|candidate| {
        candidate.label_target_identity
            && update_procedure_candidate_has_target_identity_preference(candidate)
    });
    if has_label_target_identity {
        candidates.retain(|candidate| {
            candidate.label_target_identity
                && update_procedure_candidate_has_target_identity_preference(candidate)
        });
    }
    let has_actionable_command_candidate =
        candidates.iter().any(update_procedure_candidate_has_actionable_command_preference);
    if has_actionable_command_candidate {
        candidates.retain(update_procedure_candidate_has_actionable_command_preference);
    }

    candidates
        .into_iter()
        .max_by(|left, right| {
            left.score
                .cmp(&right.score)
                .then_with(|| left.exact_target_identity.cmp(&right.exact_target_identity))
                .then_with(|| left.target_identity_priority.cmp(&right.target_identity_priority))
                .then_with(|| {
                    left.target_identity_focus_score.cmp(&right.target_identity_focus_score)
                })
                .then_with(|| left.focused_structural_score.cmp(&right.focused_structural_score))
                .then_with(|| left.selection.steps.len().cmp(&right.selection.steps.len()))
                .then_with(|| left.command_count.cmp(&right.command_count))
                .then_with(|| right.selection.source.cmp(&left.selection.source))
                .then_with(|| right.selection.anchors.cmp(&left.selection.anchors))
        })
        .map(|candidate| candidate.selection)
}

fn update_procedure_steps_with_adjacent_same_head_preparation(
    steps: Vec<String>,
    block_text: &str,
) -> Vec<String> {
    let Some(first_step) = steps.first() else {
        return steps;
    };
    let first_step_key = update_procedure_step_key(first_step);
    let block_lines = update_procedure_block_structural_lines(block_text);
    let Some(first_index) =
        block_lines.iter().position(|line| update_procedure_step_key(line) == first_step_key)
    else {
        if let Some(previous) =
            update_procedure_previous_same_head_command_from_dense_text(first_step, block_text)
        {
            return update_procedure_prepend_step_if_absent(steps, previous);
        }
        return steps;
    };
    if first_index == 0 {
        if let Some(previous) =
            update_procedure_previous_same_head_command_from_dense_text(first_step, block_text)
        {
            return update_procedure_prepend_step_if_absent(steps, previous);
        }
        return steps;
    }
    let previous = &block_lines[first_index - 1];
    if !line_has_command_signal(previous)
        || !update_procedure_command_heads_match(previous, first_step)
        || steps
            .iter()
            .any(|step| update_procedure_step_key(step) == update_procedure_step_key(previous))
    {
        return steps;
    }
    update_procedure_prepend_step_if_absent(steps, update_procedure_normalized_step(previous))
}

fn update_procedure_prepend_step_if_absent(steps: Vec<String>, previous: String) -> Vec<String> {
    if steps
        .iter()
        .any(|step| update_procedure_step_key(step) == update_procedure_step_key(&previous))
    {
        return steps;
    }
    let mut augmented = Vec::with_capacity(steps.len().saturating_add(1));
    augmented.push(previous);
    augmented.extend(steps);
    augmented
}

fn update_procedure_previous_same_head_command_from_dense_text(
    first_step: &str,
    block_text: &str,
) -> Option<String> {
    let first_key = update_procedure_step_key(first_step);
    let head = update_procedure_command_head(first_step)?;
    let normalized = block_text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = normalized.to_lowercase();
    let first_pos = lower.find(&first_key)?;
    let prefix = normalized.get(..first_pos)?.trim();
    if prefix.is_empty() {
        return None;
    }
    let tokens = prefix.split_whitespace().collect::<Vec<_>>();
    for index in (0..tokens.len()).rev() {
        let token = trim_command_boundary_token_decorations(tokens[index]).to_ascii_lowercase();
        if token != head {
            continue;
        }
        let mut start = index;
        if start > 0 {
            let previous =
                trim_command_boundary_token_decorations(tokens[start - 1]).to_ascii_lowercase();
            if matches!(previous.as_str(), "sudo" | "su") {
                start -= 1;
            }
            if start > 0 {
                let previous =
                    trim_command_boundary_token_decorations(tokens[start - 1]).to_ascii_lowercase();
                if previous == "sudo"
                    && trim_command_boundary_token_decorations(tokens[start]) == "su"
                {
                    start -= 1;
                }
            }
        }
        let candidate = update_procedure_normalized_step(&tokens[start..].join(" "));
        if !candidate.is_empty()
            && candidate != first_key
            && line_has_command_signal(&candidate)
            && update_procedure_command_heads_match(&candidate, first_step)
        {
            return Some(candidate);
        }
    }
    None
}

fn update_procedure_block_structural_lines(block_text: &str) -> Vec<String> {
    update_procedure_line_blocks(block_text)
        .into_iter()
        .flatten()
        .filter(|line| line.has_command || line.has_order_marker || line.has_version)
        .map(|line| line.text)
        .collect()
}

fn update_procedure_step_key(step: &str) -> String {
    update_procedure_normalized_step(step).to_lowercase()
}

fn update_procedure_normalized_step(step: &str) -> String {
    strip_leading_order_marker(step)
        .trim()
        .trim_end_matches(['.', ',', ';'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn update_procedure_candidate_has_target_identity_preference(
    candidate: &UpdateProcedureCandidate,
) -> bool {
    candidate.command_count >= 2
        || candidate.selection.steps.len() >= 3
        || candidate.focused_structural_score > 0
}

fn update_procedure_candidate_has_actionable_command_preference(
    candidate: &UpdateProcedureCandidate,
) -> bool {
    candidate.command_count >= 2
        && (candidate.exact_target_identity
            || candidate.label_target_identity
            || candidate.target_identity_focus_score > 0
            || candidate.focused_structural_score > 0)
}

fn update_procedure_focus_aligned_command_aggregate(
    extracts: &[UpdateProcedureExtract],
    focus_model: &UpdateProcedureFocusModel,
    document_label: &str,
) -> Option<UpdateProcedureExtract> {
    let label_focus_score = update_procedure_text_focus_score(document_label, focus_model);
    let body_focus_score = extracts
        .iter()
        .map(|extract| update_procedure_text_focus_score(&extract.block_text, focus_model))
        .max()
        .unwrap_or_default();
    let aggregate_focus_score = label_focus_score.max(body_focus_score);
    if aggregate_focus_score == 0
        || !extracts.iter().any(|extract| {
            update_procedure_selection_matches_action(extract, focus_model, document_label)
        })
    {
        return None;
    }
    let action_match_indexes = extracts
        .iter()
        .filter(|extract| {
            extract.action_command_score > 0
                || update_procedure_selection_matches_action(extract, focus_model, document_label)
        })
        .map(|extract| extract.block_index)
        .collect::<Vec<_>>();
    let last_action_match_index = action_match_indexes.iter().copied().max()?;
    let focus_aligned_range = extracts
        .iter()
        .filter(|extract| extract.focus_aligned_command_score > 0)
        .map(|extract| extract.block_index)
        .fold(None::<(usize, usize)>, |range, index| match range {
            Some((start, end)) => Some((start.min(index), end.max(index))),
            None => Some((index, index)),
        });
    let body_target_action_range = extracts
        .iter()
        .filter(|extract| {
            update_procedure_text_target_identity_priority(&extract.block_text, focus_model) > 0
        })
        .map(|extract| extract.block_index)
        .min()
        .and_then(|target_start| {
            let action_end = action_match_indexes.iter().copied().max()?;
            (target_start < action_end).then_some((target_start, action_end))
        });
    let (aligned_range, selection_start_index, allow_unfocused_inside_range) =
        if let Some(range) = focus_aligned_range.filter(|range| range.0 < range.1) {
            (range, last_action_match_index, false)
        } else if let Some(range) = body_target_action_range {
            (range, range.0, true)
        } else {
            return None;
        };
    let mut selected = extracts
        .iter()
        .filter(|extract| {
            extract.block_index >= aligned_range.0.saturating_sub(4)
                && extract.block_index >= selection_start_index
                && extract.block_index <= aligned_range.1
                && (extract.focus_aligned_command_score > 0
                    || extract.action_command_score > 0
                    || extract.preparatory_command_score > 0
                    || extract.command_count > 0)
                && (allow_unfocused_inside_range
                    || extract.unfocused_command_score == 0
                    || extract.block_index >= last_action_match_index)
                && (!extract.has_setup_script_signature
                    || update_procedure_selection_matches_action(
                        extract,
                        focus_model,
                        document_label,
                    ))
        })
        .collect::<Vec<_>>();
    let preparatory_indexes = selected
        .iter()
        .filter(|extract| extract.preparatory_command_score > 0)
        .map(|extract| extract.block_index)
        .collect::<Vec<_>>();
    if preparatory_indexes.len() > 2
        && let Some(keep_from) = preparatory_indexes.get(preparatory_indexes.len() - 2)
    {
        selected.retain(|extract| {
            extract.action_command_score > 0
                || extract.focus_aligned_command_score > 0
                || extract.block_index >= *keep_from
        });
    }
    selected.sort_by_key(|extract| extract.block_index);
    if selected.len() < 2 {
        return None;
    }

    let mut seen = std::collections::HashSet::new();
    let mut steps = Vec::<String>::new();
    for extract in &selected {
        for step in &extract.steps {
            if seen.insert(step.to_lowercase()) {
                steps.push(step.clone());
                if steps.len() >= 16 {
                    break;
                }
            }
        }
        if steps.len() >= 16 {
            break;
        }
    }
    if steps.iter().filter(|step| update_procedure_step_is_structural(step)).count() < 2 {
        return None;
    }

    let score =
        selected.iter().fold(aggregate_focus_score.saturating_mul(256), |score, extract| {
            score.saturating_add(extract.score).saturating_add(
                update_procedure_capped_command_score(extract.focus_aligned_command_score)
                    .saturating_mul(4096),
            )
        });
    Some(UpdateProcedureExtract {
        block_index: selected.first().map(|extract| extract.block_index).unwrap_or_default(),
        score,
        steps,
        block_text: selected
            .iter()
            .map(|extract| extract.block_text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        action_text: selected
            .iter()
            .map(|extract| extract.action_text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        command_count: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.command_count).sum(),
        ),
        action_command_score: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.action_command_score).sum(),
        ),
        script_artifact_family_score: selected
            .iter()
            .map(|extract| extract.script_artifact_family_score)
            .max()
            .unwrap_or_default(),
        preparatory_command_score: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.preparatory_command_score).sum(),
        ),
        focus_aligned_command_score: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.focus_aligned_command_score).sum(),
        ),
        unfocused_command_score: 0,
        has_setup_script_signature: false,
        is_focus_projection: false,
    })
}

fn update_procedure_command_candidate_bonus(command_count: usize) -> usize {
    if command_count > 0 { 4096 } else { 0 }
}

fn update_procedure_capped_command_score(score: usize) -> usize {
    score.min(UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

fn update_procedure_chunk_text(
    chunk: &RuntimeMatchedChunk,
    focus_model: &UpdateProcedureFocusModel,
) -> String {
    let excerpt = chunk.excerpt.trim();
    let source_text = chunk.source_text.trim();
    if source_text.is_empty() || excerpt == source_text {
        return excerpt.to_string();
    }
    if excerpt.is_empty() {
        return update_procedure_focused_source_text(source_text, focus_model)
            .unwrap_or_else(|| source_text.to_string());
    }
    if let Some(focused_source) = update_procedure_focused_source_text(source_text, focus_model)
        && !focused_source.trim().is_empty()
    {
        return focused_source;
    }
    excerpt.to_string()
}

fn update_procedure_focused_source_text(
    source_text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> Option<String> {
    const UPDATE_PROCEDURE_SOURCE_VIEW_CHARS: usize = 4_000;

    let lines =
        source_text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    let mut selected = BTreeSet::<usize>::new();
    for (index, line) in lines.iter().enumerate() {
        let target_priority = update_procedure_text_target_identity_priority(line, focus_model);
        let focus_score = update_procedure_text_focus_score(line, focus_model);
        if target_priority == 0 && focus_score == 0 {
            continue;
        }
        selected.insert(index);
        if index > 0
            && (line_has_order_marker(lines[index - 1])
                || line_has_command_signal(lines[index - 1]))
        {
            selected.insert(index - 1);
        }
        for lookahead in 1..=2 {
            let next_index = index + lookahead;
            let Some(next_line) = lines.get(next_index) else {
                break;
            };
            if line_has_order_marker(next_line)
                || line_has_command_signal(next_line)
                || update_procedure_text_target_identity_priority(next_line, focus_model) > 0
                || update_procedure_text_focus_score(next_line, focus_model) > 0
            {
                selected.insert(next_index);
            } else {
                break;
            }
        }
    }
    if selected.is_empty() {
        return None;
    }
    let mut focused = String::new();
    let mut previous_index = None::<usize>;
    for index in selected {
        let line = lines[index];
        if !focused.is_empty() {
            if previous_index.is_some_and(|previous| index > previous + 1) {
                focused.push_str("\n...\n");
            } else {
                focused.push('\n');
            }
        }
        focused.push_str(line);
        previous_index = Some(index);
        if focused.chars().count() >= UPDATE_PROCEDURE_SOURCE_VIEW_CHARS {
            return Some(excerpt_for(&focused, UPDATE_PROCEDURE_SOURCE_VIEW_CHARS));
        }
    }
    (!focused.trim().is_empty()).then_some(focused)
}

fn update_procedure_evidence_anchors(
    steps: &[String],
    source_text: &str,
    limit: usize,
) -> Vec<String> {
    let step_text = steps.join("\n");
    let mut anchors = Vec::new();
    push_update_procedure_evidence_anchors_from_text(&step_text, &mut anchors, limit);
    if anchors.len() < 2 {
        push_update_procedure_evidence_anchors_from_text(source_text, &mut anchors, limit);
    }
    anchors
}

fn push_update_procedure_evidence_anchors_from_text(
    text: &str,
    anchors: &mut Vec<String>,
    limit: usize,
) {
    let mut values = Vec::new();
    values.extend(extract_package_command_literals(text, limit));
    values.extend(extract_explicit_path_literals(text, limit));
    values.extend(extract_configuration_section_literals(text, limit));
    values.extend(extract_parameter_literals(text, limit));
    push_unique_values(anchors, values, limit);
}

fn update_procedure_extracts_from_text(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> Vec<UpdateProcedureExtract> {
    let blocks = update_procedure_line_blocks(text);
    let mut extracts = Vec::<UpdateProcedureExtract>::new();
    for (block_index, block) in blocks.into_iter().enumerate() {
        if !update_procedure_block_is_qualified(&block) {
            continue;
        }
        if let Some(extract) = update_procedure_extract_from_block(block_index, &block, focus_model)
        {
            let focus_projection = update_procedure_focus_aligned_command_projection_from_block(
                block_index,
                &block,
                focus_model,
                &extract,
            );
            if let Some(action_extract) = update_procedure_action_command_projection_from_block(
                block_index,
                &block,
                focus_model,
                &extract,
            ) {
                extracts.push(action_extract);
            }
            if let Some(maintenance_extract) = focus_projection {
                extracts.push(maintenance_extract);
            }
            if let Some(tail_extract) = update_procedure_command_tail_projection_from_block(
                block_index,
                &block,
                focus_model,
                &extract,
            ) {
                extracts.push(tail_extract);
            }
            extracts.push(extract);
        }
    }
    extracts.sort_by(|left, right| {
        right.score.cmp(&left.score).then_with(|| right.steps.len().cmp(&left.steps.len()))
    });
    extracts
}

fn update_procedure_extract_from_block(
    block_index: usize,
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> Option<UpdateProcedureExtract> {
    let score = update_procedure_block_score(block, focus_model);
    let mut seen = std::collections::HashSet::new();
    let mut structural_steps_seen = 0usize;
    let mut steps = Vec::new();
    for line in block {
        let is_structural = line.has_version || line.has_order_marker || line.has_command;
        let include = if is_structural {
            structural_steps_seen = structural_steps_seen.saturating_add(1);
            true
        } else {
            structural_steps_seen >= 2 && line.text.chars().count() <= 160
        };
        if !include {
            continue;
        }
        if seen.insert(line.text.to_lowercase()) {
            steps.push(line.text.clone());
        }
        if steps.len() >= 16 {
            break;
        }
    }
    if steps.iter().filter(|step| update_procedure_step_is_structural(step)).count() < 2 {
        return None;
    }
    let command_count =
        update_procedure_capped_command_score(block.iter().filter(|line| line.has_command).count());
    let action_command_score = update_procedure_capped_command_score(
        update_procedure_action_command_score(block, focus_model),
    );
    let script_artifact_family_score = update_procedure_script_artifact_family_score(block);
    let preparatory_command_score = update_procedure_capped_command_score(
        update_procedure_preparatory_command_score(block, focus_model),
    );
    let focus_aligned_command_score = update_procedure_capped_command_score(
        update_procedure_focus_aligned_command_score(block, focus_model),
    );
    let unfocused_command_score = update_procedure_capped_command_score(
        update_procedure_unfocused_command_score(block, focus_model),
    );
    let block_text = block.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
    let action_text = update_procedure_block_action_match_text(block);
    let has_setup_script_signature =
        update_procedure_block_has_setup_script_signature(block, &focus_model.procedure_terms);
    Some(UpdateProcedureExtract {
        block_index,
        score,
        steps,
        block_text,
        action_text,
        command_count,
        action_command_score,
        script_artifact_family_score,
        preparatory_command_score,
        focus_aligned_command_score,
        unfocused_command_score,
        has_setup_script_signature,
        is_focus_projection: false,
    })
}

fn update_procedure_action_command_projection_from_block(
    block_index: usize,
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
    source_extract: &UpdateProcedureExtract,
) -> Option<UpdateProcedureExtract> {
    if source_extract.action_command_score == 0 || source_extract.command_count < 2 {
        return None;
    }
    if source_extract.action_command_score >= 2 || source_extract.script_artifact_family_score >= 2
    {
        return None;
    }
    if update_procedure_action_artifact_token_count(block, focus_model) >= 2 {
        return None;
    }
    let last_action_index = block
        .iter()
        .enumerate()
        .filter(|(_, line)| line.has_command)
        .filter_map(|(index, line)| {
            let command = strip_leading_order_marker(&line.text);
            let action_text = command_action_match_text(command);
            let action_tokens =
                normalized_alnum_tokens(&action_text, 2).into_iter().collect::<BTreeSet<_>>();
            (procedure_term_overlap_score(&focus_model.procedure_terms, &action_tokens) > 0)
                .then_some(index)
        })
        .last()?;
    if source_extract.focus_aligned_command_score > 0
        && let Some(first_focus_aligned_index) = block.iter().position(|line| {
            line.has_command
                && update_procedure_command_focus_aligned_score(
                    strip_leading_order_marker(&line.text),
                    focus_model,
                ) > 0
        })
        && last_action_index < first_focus_aligned_index
    {
        return None;
    }
    let has_prior_non_privilege_command = block
        .iter()
        .take(last_action_index)
        .any(|line| line.has_command && !update_procedure_line_is_privilege_prefix(&line.text));
    let projection_start_index = if has_prior_non_privilege_command {
        update_procedure_action_projection_start_index(block, last_action_index)
    } else {
        0
    };
    let mut seen = std::collections::HashSet::new();
    let mut steps = Vec::<String>::new();
    for line in block.iter().skip(projection_start_index).filter(|line| {
        line.has_command || (!has_prior_non_privilege_command && line.has_order_marker)
    }) {
        if seen.insert(line.text.to_lowercase()) {
            steps.push(line.text.clone());
            if steps.len() >= 16 {
                break;
            }
        }
    }
    if steps.len() < 2
        || steps.iter().filter(|step| update_procedure_step_is_structural(step)).count() < 2
    {
        return None;
    }
    let block_text = source_extract.block_text.clone();
    let action_text = steps
        .iter()
        .map(|step| command_action_match_text(strip_leading_order_marker(step)))
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let action_command_score =
        update_procedure_capped_command_score(update_procedure_action_command_score(
            &steps
                .iter()
                .map(|step| UpdateProcedureLine {
                    text: step.clone(),
                    has_order_marker: line_has_order_marker(step),
                    has_version: update_procedure_line_has_version(step),
                    has_command: line_has_command_signal(step),
                })
                .collect::<Vec<_>>(),
            focus_model,
        ));
    let focus_aligned_command_score = update_procedure_capped_command_score(
        steps
            .iter()
            .map(|step| update_procedure_command_focus_aligned_score(step, focus_model))
            .sum(),
    );
    let command_count = update_procedure_capped_command_score(steps.len());
    Some(UpdateProcedureExtract {
        block_index,
        score: source_extract
            .score
            .saturating_add(action_command_score.saturating_mul(8192))
            .saturating_add(command_count.saturating_mul(1024))
            .saturating_add(focus_aligned_command_score.saturating_mul(2048)),
        command_count,
        steps,
        block_text,
        action_text,
        action_command_score,
        script_artifact_family_score: source_extract.script_artifact_family_score,
        preparatory_command_score: usize::from(projection_start_index < last_action_index),
        focus_aligned_command_score,
        unfocused_command_score: 0,
        has_setup_script_signature: source_extract.has_setup_script_signature,
        is_focus_projection: false,
    })
}

fn update_procedure_action_projection_start_index(
    block: &[UpdateProcedureLine],
    last_action_index: usize,
) -> usize {
    let mut start_index = last_action_index;
    if start_index > 0
        && block[start_index - 1].has_command
        && update_procedure_line_is_privilege_prefix(&block[start_index - 1].text)
    {
        start_index -= 1;
    }
    let mut preparatory_commands = 0usize;
    while start_index > 0 && preparatory_commands < 2 {
        let previous_index = start_index - 1;
        let previous = &block[previous_index];
        if !previous.has_command {
            break;
        }
        start_index = previous_index;
        if !update_procedure_line_is_privilege_prefix(&previous.text) {
            preparatory_commands = preparatory_commands.saturating_add(1);
        }
    }
    start_index
}

fn update_procedure_focus_aligned_command_projection_from_block(
    block_index: usize,
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
    source_extract: &UpdateProcedureExtract,
) -> Option<UpdateProcedureExtract> {
    if source_extract.focus_aligned_command_score == 0
        || (source_extract.unfocused_command_score == 0
            && source_extract.preparatory_command_score == 0)
    {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let mut steps = Vec::<String>::new();
    let mut preparatory_command_score = 0usize;
    let mut focus_aligned_command_score = 0usize;
    let mut pending_preparatory = Vec::<&UpdateProcedureLine>::new();
    for line in block.iter().filter(|line| line.has_command) {
        let command = strip_leading_order_marker(&line.text);
        let aligned_score = update_procedure_command_focus_aligned_score(command, focus_model);
        if aligned_score == 0 {
            pending_preparatory.push(line);
            if pending_preparatory.len() > 3 {
                pending_preparatory.remove(0);
            }
            continue;
        }
        for preparatory in pending_preparatory.drain(..).rev().take(2).rev() {
            if seen.insert(preparatory.text.to_lowercase()) {
                preparatory_command_score = update_procedure_capped_command_score(
                    preparatory_command_score.saturating_add(1),
                );
                steps.push(preparatory.text.clone());
            }
        }
        focus_aligned_command_score = update_procedure_capped_command_score(
            focus_aligned_command_score.saturating_add(aligned_score),
        );
        if seen.insert(line.text.to_lowercase()) {
            steps.push(line.text.clone());
            if steps.len() >= 16 {
                break;
            }
        }
    }
    if focus_aligned_command_score == 0 || steps.len() < 2 {
        return None;
    }
    if steps.iter().filter(|step| update_procedure_step_is_structural(step)).count() < 2 {
        return None;
    }
    let block_text = source_extract.block_text.clone();
    let action_text = steps
        .iter()
        .map(|step| {
            if line_has_command_signal(step) {
                command_action_match_text(strip_leading_order_marker(step))
            } else {
                prose_step_action_match_text(strip_leading_order_marker(step))
            }
        })
        .filter(|text| !text.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let focused_structural_score =
        update_procedure_focused_structural_score(&block_text, focus_model);
    let score = source_extract
        .score
        .saturating_add(focused_structural_score.saturating_mul(160))
        .saturating_add(
            update_procedure_capped_command_score(preparatory_command_score).saturating_mul(2048),
        )
        .saturating_add(
            update_procedure_capped_command_score(focus_aligned_command_score).saturating_mul(8192),
        )
        .saturating_add(update_procedure_command_candidate_bonus(steps.len()));
    Some(UpdateProcedureExtract {
        block_index,
        score,
        command_count: update_procedure_capped_command_score(steps.len()),
        steps,
        block_text,
        action_text,
        action_command_score: preparatory_command_score,
        script_artifact_family_score: 0,
        preparatory_command_score,
        focus_aligned_command_score,
        unfocused_command_score: 0,
        has_setup_script_signature: source_extract.has_setup_script_signature,
        is_focus_projection: true,
    })
}

fn update_procedure_command_tail_projection_from_block(
    block_index: usize,
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
    source_extract: &UpdateProcedureExtract,
) -> Option<UpdateProcedureExtract> {
    if source_extract.command_count < 5 {
        return None;
    }
    let command_indices = block
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.has_command.then_some(index))
        .collect::<Vec<_>>();
    if command_indices.len() < 5 {
        return None;
    }
    let first_target_command_offset = command_indices.iter().position(|index| {
        update_procedure_text_target_identity_priority(&block[*index].text, focus_model) > 0
    });
    if first_target_command_offset.is_none()
        && !source_extract.block_text.lines().any(update_procedure_line_has_version)
    {
        return None;
    }
    let mut start_offset = first_target_command_offset
        .map(|offset| offset.saturating_sub(3))
        .unwrap_or_else(|| command_indices.len().saturating_sub(4));
    if start_offset > 0 {
        let previous_index = command_indices[start_offset - 1];
        let first_index = command_indices[start_offset];
        if update_procedure_command_heads_match(
            &block[previous_index].text,
            &block[first_index].text,
        ) {
            start_offset -= 1;
        }
    }
    let projection_block = command_indices
        .iter()
        .skip(start_offset)
        .take(8)
        .map(|index| block[*index].clone())
        .collect::<Vec<_>>();
    if projection_block.len() < 4 {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let steps = projection_block
        .iter()
        .filter(|line| seen.insert(line.text.to_lowercase()))
        .map(|line| line.text.clone())
        .collect::<Vec<_>>();
    if steps.len() < 4 {
        return None;
    }
    let block_text = steps.join("\n");
    let action_text = update_procedure_projection_action_text(source_extract, &projection_block);
    let command_count = update_procedure_capped_command_score(projection_block.len());
    let action_command_score = update_procedure_capped_command_score(
        update_procedure_action_command_score(&projection_block, focus_model),
    );
    let script_artifact_family_score =
        update_procedure_script_artifact_family_score(&projection_block);
    let preparatory_command_score =
        usize::from(first_target_command_offset.is_some_and(|offset| start_offset < offset));
    let focus_aligned_command_score = update_procedure_capped_command_score(
        update_procedure_focus_aligned_command_score(&projection_block, focus_model),
    );
    let unfocused_command_score = update_procedure_capped_command_score(
        update_procedure_unfocused_command_score(&projection_block, focus_model),
    );
    let target_identity_priority =
        update_procedure_text_target_identity_priority(&block_text, focus_model);
    let score = source_extract
        .score
        .saturating_add(command_count.saturating_mul(2048))
        .saturating_add(action_command_score.saturating_mul(8192))
        .saturating_add(script_artifact_family_score.saturating_mul(1024))
        .saturating_add(focus_aligned_command_score.saturating_mul(4096))
        .saturating_add(target_identity_priority.saturating_mul(2048))
        .saturating_sub(unfocused_command_score.saturating_mul(512));
    Some(UpdateProcedureExtract {
        block_index,
        score,
        steps,
        block_text,
        action_text,
        command_count,
        action_command_score,
        script_artifact_family_score,
        preparatory_command_score,
        focus_aligned_command_score,
        unfocused_command_score,
        has_setup_script_signature: source_extract.has_setup_script_signature,
        is_focus_projection: true,
    })
}

fn update_procedure_projection_action_text(
    source_extract: &UpdateProcedureExtract,
    projection_block: &[UpdateProcedureLine],
) -> String {
    let projection_action_text = update_procedure_block_action_match_text(projection_block);
    if source_extract.action_text.trim().is_empty() {
        return projection_action_text;
    }
    if projection_action_text.trim().is_empty() {
        return source_extract.action_text.clone();
    }
    format!("{}\n{}", source_extract.action_text, projection_action_text)
}

fn update_procedure_block_is_qualified(block: &[UpdateProcedureLine]) -> bool {
    let version_count = block.iter().filter(|line| line.has_version).count();
    let ordered_count = block.iter().filter(|line| line.has_order_marker).count();
    let command_count = block.iter().filter(|line| line.has_command).count();
    ordered_count >= 2
        || command_count >= 2
        || (ordered_count >= 1 && command_count >= 1)
        || (ordered_count >= 1 && version_count >= 1)
}

fn update_procedure_line_is_privilege_prefix(line: &str) -> bool {
    let tokens = line
        .split_whitespace()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    matches!(tokens.as_slice(), [value] if value == "sudo" || value == "su")
        || matches!(tokens.as_slice(), [left, right] if left == "sudo" && right == "su")
}

fn update_procedure_block_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let focus_text = block.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
    let focus_score = update_procedure_text_focus_score(&focus_text, focus_model);
    let version_count = block.iter().filter(|line| line.has_version).count();
    let ordered_count = block.iter().filter(|line| line.has_order_marker).count();
    let command_count = block.iter().filter(|line| line.has_command).count();
    focus_score
        .saturating_mul(128)
        .saturating_add(command_count.saturating_mul(32))
        .saturating_add(ordered_count.saturating_mul(24))
        .saturating_add(version_count.saturating_mul(8))
        .saturating_add(block.len().min(16))
}

fn update_procedure_text_focus_score(text: &str, focus_model: &UpdateProcedureFocusModel) -> usize {
    let tokens = label_terms(text, 2);
    if tokens.is_empty() {
        return 0;
    }
    let subject_overlap = procedure_term_overlap_score(&focus_model.subject_terms, &tokens);
    let subject_acronym_overlap = focus_model.subject_acronym_terms.intersection(&tokens).count();
    let procedure_overlap = procedure_term_overlap_score(&focus_model.procedure_terms, &tokens);
    let query_overlap = procedure_term_overlap_score(&focus_model.query_terms, &tokens);
    subject_overlap
        .saturating_mul(16)
        .saturating_add(subject_acronym_overlap.saturating_mul(12))
        .saturating_add(procedure_overlap.saturating_mul(4))
        .saturating_add(query_overlap)
}

fn update_procedure_focused_structural_score(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    text.lines()
        .filter_map(|line| {
            let is_structural =
                update_procedure_line_has_version(line) || line_has_command_signal(line);
            if !is_structural {
                return None;
            }
            let focus_score = update_procedure_text_focus_score(line, focus_model);
            (focus_score > 0).then_some(focus_score)
        })
        .sum::<usize>()
}

fn update_procedure_selection_matches_action(
    extract: &UpdateProcedureExtract,
    focus_model: &UpdateProcedureFocusModel,
    document_label: &str,
) -> bool {
    if focus_model.procedure_terms.is_empty() {
        return true;
    }
    let action_tokens =
        normalized_alnum_tokens(&extract.action_text, 2).into_iter().collect::<BTreeSet<_>>();
    if procedure_term_overlap_score(&focus_model.procedure_terms, &action_tokens) > 0 {
        if extract.has_setup_script_signature
            && extract.action_command_score == 0
            && extract.script_artifact_family_score == 0
            && !update_procedure_setup_signature_is_action_bound(
                extract,
                focus_model,
                document_label,
            )
        {
            return false;
        }
        return true;
    }
    if extract.has_setup_script_signature {
        return false;
    }
    if extract.is_focus_projection
        && update_procedure_focused_structural_score(&extract.block_text, focus_model) > 0
    {
        return true;
    }
    if extract.command_count > 0 {
        return false;
    }
    let label_tokens =
        normalized_alnum_tokens(document_label, 2).into_iter().collect::<BTreeSet<_>>();
    procedure_term_overlap_score(&focus_model.procedure_terms, &label_tokens) > 0
}

fn update_procedure_setup_signature_is_action_bound(
    extract: &UpdateProcedureExtract,
    focus_model: &UpdateProcedureFocusModel,
    document_label: &str,
) -> bool {
    if !extract.has_setup_script_signature {
        return true;
    }
    let has_target_identity =
        update_procedure_text_target_identity_priority(&extract.block_text, focus_model) > 0
            || update_procedure_text_target_identity_priority(document_label, focus_model) > 0;
    if !has_target_identity {
        return false;
    }
    let label_tokens =
        normalized_alnum_tokens(document_label, 2).into_iter().collect::<BTreeSet<_>>();
    let label_target_identity =
        update_procedure_text_target_identity_priority(document_label, focus_model) > 0;
    let has_label_action_binding = label_target_identity
        && procedure_term_overlap_score(&focus_model.procedure_terms, &label_tokens) > 0;
    let has_command_action_binding =
        extract.action_command_score > 0 || extract.script_artifact_family_score > 0;
    let has_action_binding = has_label_action_binding || has_command_action_binding;
    has_action_binding
        && (extract.command_count > 0
            || update_procedure_focused_structural_score(&extract.block_text, focus_model) > 0)
}

fn update_procedure_block_action_match_text(block: &[UpdateProcedureLine]) -> String {
    let mut lines = Vec::<String>::new();
    let mut structural_seen = false;
    for line in block {
        let is_structural = line.has_version || line.has_order_marker || line.has_command;
        if !is_structural && structural_seen {
            continue;
        }
        let trimmed = strip_leading_order_marker(&line.text).trim();
        if trimmed.is_empty() {
            continue;
        }
        if line.has_command {
            let command_text = command_action_match_text(trimmed);
            if !command_text.trim().is_empty() {
                lines.push(command_text);
            }
        } else {
            let prose_text = prose_step_action_match_text(trimmed);
            if !prose_text.trim().is_empty() {
                lines.push(prose_text);
            }
        }
        if is_structural {
            structural_seen = true;
        }
    }
    lines.join("\n")
}

fn update_procedure_action_command_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    block
        .iter()
        .filter(|line| line.has_command)
        .map(|line| {
            let command = strip_leading_order_marker(&line.text);
            let action_text = command_action_match_text(command);
            let action_tokens =
                normalized_alnum_tokens(&action_text, 2).into_iter().collect::<BTreeSet<_>>();
            procedure_term_overlap_score(&focus_model.procedure_terms, &action_tokens)
        })
        .sum()
}

fn update_procedure_script_artifact_family_score(block: &[UpdateProcedureLine]) -> usize {
    let mut stems = BTreeSet::<String>::new();
    for line in block {
        if !line.has_command {
            continue;
        }
        let command = strip_leading_order_marker(&line.text);
        for token in command_token_values(command) {
            let Some(file_name) = command_token_file_artifact_name(&token) else {
                continue;
            };
            if let Some(stem) = script_artifact_family_stem(file_name) {
                stems.insert(stem);
            }
        }
    }
    if stems.len() < 2 {
        return 0;
    }
    let stems = stems.into_iter().collect::<Vec<_>>();
    let mut best = 0usize;
    for (index, left) in stems.iter().enumerate() {
        for right in stems.iter().skip(index + 1) {
            best = best.max(shared_script_artifact_family_prefix_len(left, right));
        }
    }
    best
}

fn update_procedure_action_artifact_token_count(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let mut artifacts = BTreeSet::<String>::new();
    for line in block {
        if !line.has_command {
            continue;
        }
        let command = strip_leading_order_marker(&line.text);
        for token in command_token_values(command) {
            let Some(file_name) = command_token_file_artifact_name(&token) else {
                continue;
            };
            let artifact_tokens =
                normalized_alnum_tokens(file_name, 3).into_iter().collect::<BTreeSet<_>>();
            if procedure_term_overlap_score(&focus_model.procedure_terms, &artifact_tokens) > 0 {
                artifacts.insert(token);
            }
        }
    }
    artifacts.len()
}

fn update_procedure_preparatory_command_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let first_aligned = block.iter().position(|line| {
        line.has_command
            && update_procedure_command_focus_aligned_score(
                strip_leading_order_marker(&line.text),
                focus_model,
            ) > 0
    });
    let Some(first_aligned) = first_aligned else {
        return 0;
    };
    block.iter().take(first_aligned).filter(|line| line.has_command).count()
}

fn update_procedure_focus_aligned_command_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    block
        .iter()
        .filter(|line| line.has_command)
        .map(|line| {
            update_procedure_command_focus_aligned_score(
                strip_leading_order_marker(&line.text),
                focus_model,
            )
        })
        .sum()
}

fn update_procedure_unfocused_command_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let Some(first_aligned) = block.iter().position(|line| {
        line.has_command
            && update_procedure_command_focus_aligned_score(
                strip_leading_order_marker(&line.text),
                focus_model,
            ) > 0
    }) else {
        return block.iter().filter(|line| line.has_command).count();
    };
    block
        .iter()
        .skip(first_aligned.saturating_add(1))
        .filter(|line| line.has_command)
        .filter(|line| {
            update_procedure_command_focus_aligned_score(
                strip_leading_order_marker(&line.text),
                focus_model,
            ) == 0
        })
        .count()
}

fn update_procedure_command_focus_aligned_score(
    command: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let tokens = label_terms(command, 2);
    if tokens.is_empty() {
        return 0;
    }
    let subject_overlap = focus_model.subject_terms.intersection(&tokens).count();
    let subject_acronym_overlap = focus_model.subject_acronym_terms.intersection(&tokens).count();
    let procedure_overlap = procedure_term_overlap_score(&focus_model.procedure_terms, &tokens);
    let query_overlap = focus_model.query_terms.intersection(&tokens).count();
    let target_score =
        subject_overlap.saturating_mul(8).saturating_add(subject_acronym_overlap.saturating_mul(6));
    if target_score == 0 {
        return 0;
    }
    target_score
        .saturating_add(procedure_overlap.saturating_mul(2))
        .saturating_add(query_overlap.min(2))
}

const SCRIPT_ARTIFACT_FAMILY_PREFIX_MIN_CHARS: usize = 4;

fn script_artifact_family_stem(file_name: &str) -> Option<String> {
    let stem = file_name
        .split(['?', '#'])
        .next()?
        .trim()
        .strip_suffix(".sh")
        .unwrap_or(file_name)
        .chars()
        .flat_map(char::to_lowercase)
        .collect::<String>();
    stem.chars().any(char::is_alphanumeric).then_some(stem)
}

fn shared_script_artifact_family_prefix_len(left: &str, right: &str) -> usize {
    let shared = shared_procedure_prefix_len(left, right);
    if shared < SCRIPT_ARTIFACT_FAMILY_PREFIX_MIN_CHARS {
        return 0;
    }
    let left_next = left.chars().nth(shared);
    let right_next = right.chars().nth(shared);
    if left_next.is_none_or(is_script_artifact_family_boundary)
        || right_next.is_none_or(is_script_artifact_family_boundary)
    {
        shared
    } else {
        0
    }
}

fn is_script_artifact_family_boundary(ch: char) -> bool {
    matches!(ch, '-' | '_' | '.') || ch.is_ascii_digit()
}

fn prose_step_action_match_text(line: &str) -> String {
    let without_quoted_titles = strip_quoted_prose_spans(line);
    leading_action_sentence(&without_quoted_titles)
}

fn strip_quoted_prose_spans(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut quote_stack = Vec::<char>::new();
    for ch in text.chars() {
        if let Some(expected_close) = quote_stack.last().copied() {
            if ch == expected_close {
                quote_stack.pop();
                result.push(' ');
            }
            continue;
        }
        let close = match ch {
            '"' => Some('"'),
            '«' => Some('»'),
            '“' => Some('”'),
            _ => None,
        };
        if let Some(close) = close {
            quote_stack.push(close);
            result.push(' ');
        } else {
            result.push(ch);
        }
    }
    result.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn leading_action_sentence(text: &str) -> String {
    let trimmed = text.trim();
    let mut previous = None::<char>;
    for (index, ch) in trimmed.char_indices() {
        if matches!(ch, '.' | '!' | '?') && previous.is_some_and(|prev| !prev.is_ascii_digit()) {
            let end = index + ch.len_utf8();
            return trimmed[..end].trim().to_string();
        }
        previous = Some(ch);
    }
    trimmed.to_string()
}

fn command_action_match_text(command_line: &str) -> String {
    command_line.split_whitespace().filter_map(command_action_token).collect::<Vec<_>>().join(" ")
}

fn update_procedure_block_has_setup_script_signature(
    block: &[UpdateProcedureLine],
    procedure_terms: &BTreeSet<String>,
) -> bool {
    let mut has_external_artifact_materialization = false;
    let mut has_local_artifact_preparation = false;
    let mut has_local_artifact = false;
    let mut has_local_artifact_run = false;
    let mut has_action_specific_command = false;
    for line in block {
        if !line.has_command {
            continue;
        }
        let command = strip_leading_order_marker(&line.text);
        let command_tokens = command_token_values(command);
        if command_tokens.is_empty() {
            continue;
        }
        let action_tokens = normalized_alnum_tokens(&command_action_match_text(command), 2)
            .into_iter()
            .collect::<BTreeSet<_>>();
        if procedure_term_overlap_score(procedure_terms, &action_tokens) > 0 {
            has_action_specific_command = true;
        }
        if command_tokens_have_external_artifact_materialization(&command_tokens) {
            has_external_artifact_materialization = true;
        }
        let line_has_local_artifact =
            command_tokens.iter().any(|token| token_has_local_command_artifact(token));
        if line_has_local_artifact {
            has_local_artifact = true;
        }
        if line_has_local_artifact
            && command_tokens.iter().any(|token| command_token_has_preparation_signal(token))
        {
            has_local_artifact_preparation = true;
        }
        if command_tokens.first().is_some_and(|token| token_is_local_command_artifact_start(token))
        {
            has_local_artifact_run = true;
        }
    }
    has_external_artifact_materialization
        && (has_local_artifact_run || (has_local_artifact_preparation && has_local_artifact))
        && !has_action_specific_command
}

fn command_tokens_have_external_artifact_materialization(tokens: &[String]) -> bool {
    let has_external_artifact = tokens.iter().any(|token| token.contains("://"));
    let has_local_artifact =
        tokens.iter().skip(1).any(|token| token_has_local_command_artifact(token));
    has_external_artifact && has_local_artifact
}

fn token_has_local_command_artifact(token: &str) -> bool {
    let normalized = trim_command_token_decorations(token);
    if normalized.starts_with('-') || normalized.starts_with('+') || normalized.contains("://") {
        return false;
    }
    token_is_local_command_artifact_start(normalized)
        || command_token_file_artifact_name(normalized).is_some()
}

fn token_is_local_command_artifact_start(token: &str) -> bool {
    (token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with('/')
        || token.contains('/'))
        && !token.contains("://")
        && token.chars().any(|ch| ch.is_alphanumeric())
}

fn command_token_file_artifact_name(token: &str) -> Option<&str> {
    let file_name =
        token.rsplit('/').next()?.split(['?', '#']).next()?.trim_end_matches(|ch: char| {
            command_token_char_is_invisible_format(ch) || ch.is_ascii_punctuation()
        });
    let has_extension = file_name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| (2..=12).contains(&extension.len()));
    let has_structural_name = file_name.contains('-')
        || file_name.contains('_')
        || file_name.chars().any(|ch| ch.is_ascii_digit());
    (has_extension || has_structural_name).then_some(file_name)
}

fn command_token_has_preparation_signal(token: &str) -> bool {
    token.starts_with('+')
        || token.contains("+x")
        || token.chars().all(|ch| ch.is_ascii_digit())
        || token.starts_with('-')
}

fn trim_command_token_decorations(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ',' | ':')
            || command_token_char_is_invisible_format(ch)
    })
}

fn trim_command_boundary_token_decorations(token: &str) -> &str {
    trim_command_token_decorations(token).trim_end_matches(|ch: char| matches!(ch, '.' | ',' | ';'))
}

fn command_token_char_is_invisible_format(ch: char) -> bool {
    matches!(
        ch,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
    )
}

fn command_token_values(command_line: &str) -> Vec<String> {
    let mut tokens = command_line
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| {
                    matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ',' | ':')
                        || command_token_char_is_invisible_format(ch)
                })
                .to_ascii_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    while tokens.first().is_some_and(|token| matches!(token.as_str(), "sudo" | "su")) {
        tokens.remove(0);
    }
    tokens
}

fn update_procedure_command_head(command_line: &str) -> Option<String> {
    command_token_values(strip_leading_order_marker(command_line)).into_iter().next()
}

fn update_procedure_command_heads_match(left: &str, right: &str) -> bool {
    let Some(left_head) = update_procedure_command_head(left) else {
        return false;
    };
    let Some(right_head) = update_procedure_command_head(right) else {
        return false;
    };
    left_head == right_head
}

fn command_action_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|ch: char| {
        ch.is_ascii_punctuation() && ch != '/' && ch != '-' && ch != '_' && ch != '.'
    });
    if trimmed.is_empty() || trimmed.starts_with('-') {
        return None;
    }
    if let Some(scheme_index) = trimmed.find("://") {
        let after_scheme = &trimmed[scheme_index + 3..];
        return after_scheme.find('/').and_then(|path_index| {
            let path = after_scheme[path_index..].trim_matches(|ch: char| {
                ch.is_ascii_punctuation() && ch != '/' && ch != '-' && ch != '_' && ch != '.'
            });
            (!path.is_empty()).then(|| path.to_string())
        });
    }
    Some(trimmed.to_string())
}

fn procedure_term_overlap_score(
    expected: &BTreeSet<String>,
    available: &BTreeSet<String>,
) -> usize {
    expected
        .iter()
        .filter(|term| {
            available
                .iter()
                .any(|candidate| procedure_terms_match(term.as_str(), candidate.as_str()))
        })
        .count()
}

fn procedure_terms_match(left: &str, right: &str) -> bool {
    left == right || shared_procedure_prefix_len(left, right) >= PROCEDURE_TERM_PREFIX_MIN_CHARS
}

const PROCEDURE_TERM_PREFIX_MIN_CHARS: usize = 5;

fn shared_procedure_prefix_len(left: &str, right: &str) -> usize {
    let mut count = 0usize;
    for (left_ch, right_ch) in left.chars().zip(right.chars()) {
        if left_ch != right_ch {
            break;
        }
        count += 1;
    }
    count
}

fn update_procedure_step_is_structural(step: &str) -> bool {
    line_has_order_marker(step)
        || update_procedure_line_has_version(step)
        || line_has_command_signal(step)
}

#[derive(Debug, Clone)]
struct UpdateProcedureLine {
    text: String,
    has_order_marker: bool,
    has_version: bool,
    has_command: bool,
}

fn update_procedure_line_blocks(text: &str) -> Vec<Vec<UpdateProcedureLine>> {
    let mut blocks = Vec::<Vec<UpdateProcedureLine>>::new();
    let mut current = Vec::<UpdateProcedureLine>::new();
    for raw_line in text.lines() {
        let expanded_lines = split_dense_procedure_line(raw_line);
        let expanded_lines = if expanded_lines.is_empty() {
            vec![raw_line.trim().to_string()]
        } else {
            expanded_lines
        };
        for trimmed in expanded_lines {
            let trimmed = trimmed.trim();
            if trimmed.is_empty() {
                if !current.is_empty() {
                    blocks.push(std::mem::take(&mut current));
                }
                continue;
            }
            let has_order_marker = line_has_order_marker(trimmed);
            let line = trimmed.trim_matches(['-', '*', '•', ' ']).trim();
            if line.chars().count() < 8 && !line_has_command_signal(line) {
                continue;
            }
            let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
            let has_version = !line_looks_data_record(&normalized)
                && update_procedure_line_has_version(&normalized);
            let has_command = line_has_command_signal(&normalized);
            if update_procedure_line_is_section_heading(
                &normalized,
                has_order_marker,
                has_version,
                has_command,
            ) && update_procedure_block_has_setup_script_signature(&current, &BTreeSet::new())
            {
                blocks.push(std::mem::take(&mut current));
            }
            current.push(UpdateProcedureLine {
                has_version,
                has_order_marker,
                has_command,
                text: normalized,
            });
        }
    }
    if !current.is_empty() {
        blocks.push(current);
    }
    blocks
}

fn update_procedure_line_is_section_heading(
    line: &str,
    has_order_marker: bool,
    has_version: bool,
    has_command: bool,
) -> bool {
    !has_order_marker
        && !has_version
        && !has_command
        && line.ends_with(':')
        && line.chars().count() <= 120
        && line.split_whitespace().count() <= 14
}

fn split_dense_procedure_line(line: &str) -> Vec<String> {
    let mut segments = Vec::<String>::new();
    let mut current = Vec::<String>::new();
    let tokens =
        line.split_whitespace().flat_map(split_joined_local_script_token).collect::<Vec<_>>();
    for (token_index, token) in tokens.iter().enumerate() {
        let next_token = tokens.get(token_index + 1).copied();
        if matches!(current.as_slice(), [value] if value == "sudo") {
            if let Some(command_suffix) = joined_privilege_command_suffix(token) {
                current.push("su".to_string());
                segments.push(current.join(" "));
                current.clear();
                current.push(command_suffix);
                continue;
            }
        }
        let is_command_start = token_is_inline_command_boundary_start(token, next_token, &current);
        let is_order_start = token_is_inline_order_marker(token);
        let current_is_privilege_prefix = matches!(current.as_slice(), [value] if value == "sudo");
        if matches!(current.as_slice(), [left, right] if left == "sudo" && right == "su")
            && is_command_start
        {
            segments.push(current.join(" "));
            current.clear();
        }
        let starts_new_prose_after_command =
            current_starts_with_command(&current) && token_starts_prose_after_command(token);
        if (is_command_start || is_order_start || starts_new_prose_after_command)
            && !current.is_empty()
            && !current_is_privilege_prefix
        {
            segments.push(current.join(" "));
            current.clear();
        }
        current.push(token.to_string());
    }
    if !current.is_empty() {
        segments.push(current.join(" "));
    }
    segments
        .into_iter()
        .map(|segment| segment.trim_matches(['-', '*', '•', ' ']).trim().to_string())
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn split_joined_local_script_token(token: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for (index, _) in token.char_indices().skip(1) {
        let rest = &token[index..];
        if (rest.starts_with("/tmp/") || rest.starts_with("./"))
            && token_has_local_command_artifact(&token[start..index])
        {
            segments.push(&token[start..index]);
            start = index;
        };
    }
    if start == 0 {
        return vec![token];
    }
    segments.push(&token[start..]);
    segments.into_iter().filter(|segment| !segment.is_empty()).collect()
}

fn token_is_inline_command_boundary_start(
    token: &str,
    next_token: Option<&str>,
    current: &[String],
) -> bool {
    let cleaned = token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';'));
    let normalized = trim_command_boundary_token_decorations(cleaned).to_ascii_lowercase();
    if matches!(normalized.as_str(), "sudo" | "doas") {
        return true;
    }
    if !current.is_empty()
        && !current_ends_with_inline_command_delimiter(current)
        && !token_is_local_script_command_start(cleaned)
        && !matches!(current, [left, right] if left == "sudo" && right == "su")
        && current_is_plain_invocation_with_structural_argument(current, &normalized)
    {
        return false;
    }
    if token_is_local_script_command_start(cleaned) {
        if cleaned.starts_with("./") {
            return current.is_empty()
                || current_ends_with_inline_command_delimiter(current)
                || current.first().is_some_and(|previous| previous == "cd")
                || current
                    .last()
                    .is_some_and(|previous| token_is_local_script_command_start(previous));
        }
        return current.is_empty()
            || current_ends_with_inline_command_delimiter(current)
            || current
                .last()
                .is_some_and(|previous| token_is_local_script_command_start(previous));
    }
    if current_ends_with_inline_command_delimiter(current)
        && command_token_is_invocable_head(&normalized)
    {
        return true;
    }
    if matches!(current, [left, right] if left == "sudo" && right == "su")
        && token_is_command_start(cleaned)
    {
        return true;
    }
    if current_starts_with_command(current)
        && current_command_has_external_materialization(current)
        && command_token_is_invocable_head(&normalized)
    {
        return true;
    }
    if current_command_prepares_local_artifact(current)
        && token_has_local_command_artifact(&normalized)
    {
        return true;
    }
    if current_command_expects_structural_value(current)
        && token_has_local_command_artifact(&normalized)
    {
        return false;
    }
    if !current.is_empty()
        && command_token_has_executable_name_shape(&normalized)
        && next_token
            .map(|token| {
                let next = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
                command_token_is_structural_argument(&next)
                    || command_token_has_executable_name_shape(&next)
            })
            .unwrap_or(false)
    {
        return true;
    }
    if !current.is_empty() && !current_ends_with_inline_command_delimiter(current) {
        return false;
    }
    token_is_command_start(cleaned)
}

fn current_is_plain_invocation_with_structural_argument(current: &[String], token: &str) -> bool {
    let Some(head) = current
        .first()
        .map(|value| trim_command_boundary_token_decorations(value).to_ascii_lowercase())
    else {
        return false;
    };
    !token_is_command_start(&head)
        && command_token_is_invocable_head(&head)
        && command_token_is_structural_argument(token)
}

fn joined_privilege_command_suffix(token: &str) -> Option<String> {
    let cleaned = trim_command_token_decorations(token).to_ascii_lowercase();
    let suffix = cleaned.strip_prefix("su")?;
    (!suffix.is_empty()
        && command_token_is_invocable_head(suffix)
        && command_token_has_executable_name_shape(suffix))
    .then(|| suffix.to_string())
}

fn current_ends_with_inline_command_delimiter(tokens: &[String]) -> bool {
    tokens.last().is_some_and(|token| {
        token
            .trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ','))
            .ends_with(':')
    })
}

fn current_starts_with_command(tokens: &[String]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    if token_is_command_start(first) {
        return true;
    }
    let normalized = tokens
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    if command_tokens_have_structural_shape(&normalized) {
        return true;
    }
    if first != "sudo" {
        return false;
    }
    if tokens.get(1).is_some_and(|token| token_is_command_start(token)) {
        return true;
    }
    tokens.get(1).is_some_and(|token| token == "su")
        && tokens.get(2).is_some_and(|token| token_is_command_start(token))
}

fn token_starts_prose_after_command(token: &str) -> bool {
    let cleaned =
        token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ',' | ':'));
    let Some(first) = cleaned.chars().next() else {
        return false;
    };
    let char_count = cleaned.chars().count();
    first.is_uppercase()
        && (cleaned.chars().skip(1).any(char::is_lowercase)
            || (char_count == 1 && !first.is_ascii()))
        && !cleaned.contains('=')
        && !cleaned.contains('/')
        && !cleaned.contains('.')
        && !cleaned.starts_with('-')
}

fn token_is_inline_order_marker(token: &str) -> bool {
    let token = token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';'));
    let mut chars = token.chars().peekable();
    let mut digit_count = 0usize;
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        digit_count = digit_count.saturating_add(1);
        chars.next();
    }
    digit_count > 0 && chars.peek().is_some_and(|ch| matches!(ch, '.' | ')')) && {
        chars.next();
        chars.next().is_none()
    }
}

fn token_is_command_start(token: &str) -> bool {
    let token = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
    if token.starts_with('-')
        || token.contains("://")
        || !token.chars().any(|ch| ch.is_alphabetic())
    {
        return false;
    }
    token_is_local_script_command_start(&token) || command_token_has_executable_name_shape(&token)
}

fn token_is_local_script_command_start(token: &str) -> bool {
    token.starts_with("/tmp/") || token.starts_with("./") || token.starts_with("../")
}

fn command_token_has_executable_name_shape(token: &str) -> bool {
    let token = trim_command_boundary_token_decorations(token);
    if token.starts_with('-')
        || !token.chars().any(|ch| ch.is_alphabetic())
        || token.contains("://")
    {
        return false;
    }
    let has_ascii_alpha = token.chars().any(|ch| ch.is_ascii_alphabetic());
    command_token_is_path_like(token)
        || (token.contains('-') && has_ascii_alpha)
        || token.contains('_')
        || token.contains('.')
        || token.chars().any(|ch| ch.is_ascii_digit())
}

fn command_token_is_path_like(token: &str) -> bool {
    (token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with('/')
        || token.contains('/'))
        && !token.contains("://")
        && token.chars().any(|ch| ch.is_alphanumeric())
}

fn command_token_is_structural_argument(token: &str) -> bool {
    let token = trim_command_boundary_token_decorations(token);
    token.starts_with('-')
        || token.starts_with('+')
        || token.contains('=')
        || token.contains("://")
        || command_token_is_path_like(token)
        || command_token_has_executable_name_shape(token)
}

fn command_token_is_invocable_head(token: &str) -> bool {
    let token = trim_command_boundary_token_decorations(token);
    !token.is_empty()
        && !token.starts_with('-')
        && !token.contains("://")
        && !token.contains('=')
        && token.chars().any(|ch| ch.is_alphabetic())
        && token
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-' | '+' | '/' | '\\'))
}

fn command_token_is_subcommand_word(token: &str) -> bool {
    let len = token.chars().count();
    (2..=32).contains(&len)
        && !token.starts_with('-')
        && token.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_'))
}

fn command_tokens_have_structural_shape(tokens: &[String]) -> bool {
    let Some(head) = tokens.first() else {
        return false;
    };
    if token_is_command_start(head) {
        return true;
    }
    if !command_token_is_invocable_head(head) {
        return false;
    }
    if !head.chars().all(|ch| ch.is_ascii_lowercase() || matches!(ch, '-' | '_' | '.')) {
        return false;
    }
    let Some(first_arg) = tokens.get(1) else {
        return false;
    };
    if command_token_is_structural_argument(first_arg) {
        return true;
    }
    if tokens.len() >= 3
        && command_token_is_subcommand_word(first_arg)
        && tokens.get(2).is_some_and(|token| command_token_is_subcommand_word(token))
    {
        return true;
    }
    command_token_is_subcommand_word(first_arg)
        && tokens.iter().skip(1).take(6).any(|token| command_token_is_structural_argument(token))
}

fn current_command_has_external_materialization(tokens: &[String]) -> bool {
    let normalized = tokens
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    if !normalized.iter().any(|token| token.contains("://")) {
        return false;
    }
    normalized.iter().skip(1).any(|token| token_has_local_command_artifact(token))
}

fn current_command_prepares_local_artifact(tokens: &[String]) -> bool {
    let normalized = tokens
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    normalized.iter().any(|token| command_token_has_preparation_signal(token))
        && normalized.iter().skip(1).any(|token| token_has_local_command_artifact(token))
}

fn current_command_expects_structural_value(tokens: &[String]) -> bool {
    tokens.last().is_some_and(|token| {
        let token = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
        token.starts_with('-')
            || token.starts_with('+')
            || token.contains("+")
            || token.ends_with('=')
    })
}

fn line_has_command_signal(line: &str) -> bool {
    let trimmed = strip_leading_order_marker(line).trim();
    if command_line_starts_like_sentence(trimmed) {
        return false;
    }
    let mut tokens = trimmed
        .split_whitespace()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    if matches!(tokens.as_slice(), [value] if value == "sudo" || value == "su") {
        return true;
    }
    if matches!(tokens.as_slice(), [left, right] if left == "sudo" && right == "su") {
        return true;
    }
    while tokens.first().is_some_and(|token| matches!(token.as_str(), "sudo" | "su")) {
        tokens.remove(0);
    }
    command_tokens_have_structural_shape(&tokens)
}

fn command_line_starts_like_sentence(line: &str) -> bool {
    let Some(first_token) = line.split_whitespace().next() else {
        return false;
    };
    let cleaned = trim_command_boundary_token_decorations(first_token);
    let Some(first_char) = cleaned.chars().next() else {
        return false;
    };
    first_char.is_uppercase()
        && cleaned.chars().skip(1).any(char::is_lowercase)
        && !cleaned.contains(['/', '\\', '.', '_', '-'])
        && !cleaned.contains("://")
}

fn line_has_order_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with(['-', '*', '•']) {
        return true;
    }
    let mut chars = trimmed.chars().peekable();
    let mut digit_count = 0usize;
    while chars.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        digit_count = digit_count.saturating_add(1);
        chars.next();
    }
    digit_count > 0 && chars.peek().is_some_and(|ch| matches!(ch, '.' | ')'))
}

fn update_procedure_line_has_version(line: &str) -> bool {
    extract_semver_like_version(line).is_some()
        || line.split_whitespace().any(version_token_has_structured_separator)
}

fn version_token_has_structured_separator(token: &str) -> bool {
    let cleaned = token
        .trim_matches(|ch: char| !(ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_')));
    if cleaned.is_empty()
        || !cleaned.chars().any(|ch| ch.is_ascii_digit())
        || !cleaned.chars().any(|ch| matches!(ch, '-' | '_'))
    {
        return false;
    }
    let numeric_groups = cleaned
        .split(|ch| matches!(ch, '.' | '-' | '_'))
        .filter(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
        .count();
    numeric_groups >= 2
}

fn line_looks_data_record(line: &str) -> bool {
    let trimmed = strip_leading_order_marker(line).trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.starts_with('|') || trimmed.starts_with('{') {
        return true;
    }
    if trimmed.starts_with('[') && trimmed.ends_with(']') && trimmed.contains(',') {
        return true;
    }
    let colon_index = trimmed.find(':');
    let equals_index = trimmed.find('=');
    let separator_index = match (colon_index, equals_index) {
        (Some(colon), Some(equals)) => Some(colon.min(equals)),
        (Some(colon), None) => Some(colon),
        (None, Some(equals)) => Some(equals),
        (None, None) => None,
    };
    let Some(separator_index) = separator_index else {
        return false;
    };
    let (left, right_with_separator) = trimmed.split_at(separator_index);
    let right = right_with_separator[1..].trim();
    let left = left.trim().trim_matches('"');
    if left.is_empty() || right.is_empty() || left.split_whitespace().count() > 3 {
        return false;
    }
    let right_value = right.trim_matches(|ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | ';' | ')' | ']')
    });
    !right_value.is_empty()
        && right_value.chars().any(|ch| ch.is_ascii_digit())
        && right_value.chars().any(|ch| matches!(ch, '.' | '-' | '_'))
        && right_value.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '_' | '+'))
}

fn strip_leading_order_marker(line: &str) -> &str {
    let trimmed = line.trim();
    let without_digits = trimmed.trim_start_matches(|ch: char| ch.is_ascii_digit());
    if without_digits.len() == trimmed.len() {
        return trimmed;
    }
    without_digits
        .strip_prefix('.')
        .or_else(|| without_digits.strip_prefix(')'))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(trimmed)
}

pub(super) fn build_structured_source_unit_inventory_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !query_ir_allows_structured_source_unit_inventory(query_ir) {
        return None;
    }
    let fields = collect_source_unit_fields_for_question(chunks, question, query_ir);
    if fields.is_empty() {
        return None;
    }
    build_structured_source_unit_overlap_inventory_answer(question, query_ir, &fields)
}

fn query_ir_allows_structured_source_unit_inventory(query_ir: &QueryIR) -> bool {
    if query_ir_has_typed_table_column_inventory_intent(query_ir) {
        return false;
    }

    if query_requests_latest_versions(query_ir)
        || query_ir.source_slice.as_ref().is_some_and(|slice| {
            matches!(slice.filter, crate::domains::query_ir::SourceSliceFilter::ReleaseMarker)
        })
    {
        return false;
    }

    if query_ir.source_slice.is_some()
        || !query_ir.literal_constraints.is_empty()
        || query_ir.comparison.is_some()
    {
        return true;
    }

    if matches!(query_ir.act, QueryAct::Enumerate | QueryAct::RetrieveValue)
        && !query_ir.target_types.is_empty()
    {
        return true;
    }

    if !matches!(query_ir.act, QueryAct::Compare | QueryAct::Describe | QueryAct::ConfigureHow) {
        return false;
    }

    query_ir.target_types.iter().map(|target_type| canonical_target_type_tag(target_type)).any(
        |target_type| {
            matches!(
                target_type.as_str(),
                "attribute"
                    | "base_url"
                    | "config_key"
                    | "connection"
                    | "credential"
                    | "endpoint"
                    | "entry"
                    | "env_var"
                    | "error_code"
                    | "event"
                    | "field"
                    | "flag"
                    | "group"
                    | "item"
                    | "parameter"
                    | "port"
                    | "record"
                    | "service"
                    | "state"
                    | "status"
                    | "table_row"
                    | "table_summary"
                    | "url"
                    | "value"
            )
        },
    )
}

fn query_ir_has_typed_table_column_inventory_intent(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some()
        || !matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
    {
        return false;
    }
    let target_types = query_ir
        .target_types
        .iter()
        .map(|target_type| canonical_target_type_tag(target_type))
        .collect::<HashSet<_>>();
    target_types.contains("table_row") && target_types.contains("table_summary")
}

#[derive(Debug, Clone)]
struct SourceUnitField {
    document_id: Uuid,
    document_label: String,
    path: String,
    value: String,
}

fn collect_source_unit_fields_for_question(
    chunks: &[RuntimeMatchedChunk],
    question: &str,
    query_ir: &QueryIR,
) -> Vec<SourceUnitField> {
    collect_source_unit_fields_with_document_filter(
        chunks,
        source_unit_inventory_primary_document_ids(chunks, question, query_ir).as_ref(),
    )
}

fn collect_source_unit_fields_with_document_filter(
    chunks: &[RuntimeMatchedChunk],
    document_filter: Option<&HashSet<Uuid>>,
) -> Vec<SourceUnitField> {
    let mut fields = Vec::<SourceUnitField>::new();
    let mut seen = HashSet::<String>::new();
    for chunk in chunks.iter().filter(|chunk| is_structured_source_unit_runtime_chunk(chunk)) {
        if document_filter.is_some_and(|document_ids| !document_ids.contains(&chunk.document_id)) {
            continue;
        }
        let parsed = parse_source_unit_text(&chunk.source_text);
        let mut body = parsed.body.trim();
        if let Some((_, rest)) = body.split_once("fields:") {
            body = rest.trim();
        }
        for part in body.split(';').map(str::trim).filter(|part| !part.is_empty()) {
            let Some((path, value)) = part.split_once('=') else {
                continue;
            };
            let path = path.trim();
            let value = value.trim();
            if path.is_empty() || value.is_empty() {
                continue;
            }
            let key = format!("{}\n{}\n{}", chunk.document_label, path, value).to_lowercase();
            if !seen.insert(key) {
                continue;
            }
            fields.push(SourceUnitField {
                document_id: chunk.document_id,
                document_label: chunk.document_label.trim().to_string(),
                path: path.to_string(),
                value: value.to_string(),
            });
        }
    }
    fields
}

fn source_unit_inventory_primary_document_ids(
    chunks: &[RuntimeMatchedChunk],
    question: &str,
    query_ir: &QueryIR,
) -> Option<HashSet<Uuid>> {
    let focus_terms = structured_source_unit_inventory_focus_terms(question, query_ir);
    if focus_terms.is_empty() {
        return None;
    }
    let ordinary_best_score = chunks
        .iter()
        .filter(|chunk| {
            !is_structured_source_unit_runtime_chunk(chunk)
                && !is_source_profile_runtime_chunk(chunk)
        })
        .filter(|chunk| source_unit_ordinary_chunk_focus_score(chunk, &focus_terms) > 0)
        .map(|chunk| score_value(chunk.score))
        .filter(|score| score.is_finite())
        .max_by(|left, right| left.total_cmp(right));
    if let Some(best_score) = ordinary_best_score {
        let document_ids = chunks
            .iter()
            .filter(|chunk| {
                !is_structured_source_unit_runtime_chunk(chunk)
                    && !is_source_profile_runtime_chunk(chunk)
            })
            .filter(|chunk| source_unit_ordinary_chunk_focus_score(chunk, &focus_terms) > 0)
            .filter(|chunk| (score_value(chunk.score) - best_score).abs() <= f32::EPSILON)
            .map(|chunk| chunk.document_id)
            .collect::<HashSet<_>>();
        if !document_ids.is_empty() {
            return Some(document_ids);
        }
    }

    None
}

fn source_unit_ordinary_chunk_focus_score(
    chunk: &RuntimeMatchedChunk,
    focus_terms: &BTreeSet<String>,
) -> usize {
    let text = format!("{}\n{}\n{}", chunk.document_label, chunk.excerpt, chunk.source_text)
        .to_lowercase();
    focus_terms
        .iter()
        .filter(|term| term.chars().count() >= 3)
        .filter(|term| text.contains(term.as_str()))
        .count()
}

fn build_structured_source_unit_overlap_inventory_answer(
    question: &str,
    query_ir: &QueryIR,
    fields: &[SourceUnitField],
) -> Option<String> {
    let focus = StructuredSourceUnitFocus::new(question, query_ir, fields);
    if focus.is_empty() {
        return None;
    }
    let focused_fields;
    let fields =
        if let Some(document_ids) = source_unit_inventory_focused_document_ids(fields, &focus) {
            focused_fields = fields
                .iter()
                .filter(|field| document_ids.contains(&field.document_id))
                .cloned()
                .collect::<Vec<_>>();
            if focused_fields.is_empty() {
                return None;
            }
            focused_fields.as_slice()
        } else {
            fields
        };
    let focus = StructuredSourceUnitFocus::new(question, query_ir, fields);
    if focus.is_empty() {
        return None;
    }
    let mut selected = fields
        .iter()
        .filter_map(|field| {
            let score = structured_source_unit_field_overlap_score(field, &focus)
                .max(structured_source_unit_compact_root_inventory_score(field, &focus));
            (score > 0).then_some((score, field))
        })
        .collect::<Vec<_>>();
    expand_structured_source_unit_sibling_fields(fields, &mut selected, &focus);
    if structured_source_unit_inventory_too_broad(&selected, &focus) {
        return None;
    }
    selected.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.document_label.cmp(&right.document_label))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.value.cmp(&right.value))
    });
    let mut seen = HashSet::<String>::new();
    let lines = selected
        .into_iter()
        .filter_map(|(_, field)| {
            let rendered = format!(
                "`{}`: `{}={}`",
                field.document_label,
                field.path.trim(),
                field.value.trim()
            );
            seen.insert(rendered.to_lowercase()).then_some(rendered)
        })
        .take(16)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    if !structured_source_unit_inventory_matches_specific_terms(&lines, &focus) {
        return None;
    }
    Some(lines.join("\n"))
}

#[derive(Debug, Clone, Copy, Default)]
struct SourceUnitDocumentFocusScore {
    specific: usize,
    focused: usize,
    total: usize,
}

fn source_unit_inventory_focused_document_ids(
    fields: &[SourceUnitField],
    focus: &StructuredSourceUnitFocus,
) -> Option<HashSet<Uuid>> {
    if fields.iter().map(|field| field.document_id).collect::<HashSet<_>>().len() <= 1
        || fields
            .iter()
            .map(|field| field.document_label.trim().to_lowercase())
            .collect::<HashSet<_>>()
            .len()
            <= 1
    {
        return None;
    }

    let mut by_document = HashMap::<Uuid, SourceUnitDocumentFocusScore>::new();
    for field in fields {
        let score = source_unit_document_focus_score(field, focus);
        by_document
            .entry(field.document_id)
            .and_modify(|entry| {
                entry.specific = entry.specific.saturating_add(score.specific);
                entry.focused = entry.focused.saturating_add(score.focused);
                entry.total = entry.total.saturating_add(score.total);
            })
            .or_insert(score);
    }
    if by_document.is_empty() {
        return None;
    }

    let best_specific = by_document.values().map(|score| score.specific).max().unwrap_or_default();
    if best_specific > 0 {
        let selected = by_document
            .into_iter()
            .filter_map(|(document_id, score)| {
                (score.specific == best_specific).then_some(document_id)
            })
            .collect::<HashSet<_>>();
        return (!selected.is_empty()).then_some(selected);
    }

    let mut ranked = by_document.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .focused
            .cmp(&left.1.focused)
            .then_with(|| right.1.total.cmp(&left.1.total))
            .then_with(|| left.0.cmp(&right.0))
    });
    let (best_document_id, best_score) = ranked.first().copied()?;
    if best_score.focused == 0 {
        return None;
    }
    let runner = ranked.get(1).map(|(_, score)| *score).unwrap_or_default();
    if best_score.focused >= runner.focused.saturating_add(4)
        || best_score.focused.saturating_mul(2)
            >= runner.focused.saturating_mul(3).saturating_add(1)
    {
        return Some(HashSet::from([best_document_id]));
    }

    None
}

fn source_unit_document_focus_score(
    field: &SourceUnitField,
    focus: &StructuredSourceUnitFocus,
) -> SourceUnitDocumentFocusScore {
    let field_terms = structured_source_unit_field_terms(field);
    let label_terms = label_terms_with_simple_variants(&field.document_label, 2);
    let field_text =
        format!("{}\n{}\n{}", field.document_label, field.path, field.value).to_lowercase();
    let specific = focus
        .specific_terms
        .iter()
        .filter(|term| {
            let term = term.as_str();
            field_text.contains(term) || field_terms.contains(term) || label_terms.contains(term)
        })
        .count();
    let focused = field_terms.intersection(&focus.scoring_terms).count()
        + label_terms.intersection(&focus.scoring_terms).count();
    let total = field_terms.intersection(&focus.terms).count()
        + label_terms.intersection(&focus.terms).count();
    SourceUnitDocumentFocusScore { specific, focused, total }
}

fn structured_source_unit_inventory_matches_specific_terms(
    lines: &[String],
    focus: &StructuredSourceUnitFocus,
) -> bool {
    let haystack = lines.join("\n").to_lowercase();
    if !focus.required_surface_terms.is_empty() {
        return focus.required_surface_terms.iter().any(|term| haystack.contains(term.as_str()));
    }
    if focus.specific_terms.is_empty() {
        return true;
    }
    focus.specific_terms.iter().any(|term| haystack.contains(term.as_str()))
}

fn structured_source_unit_inventory_too_broad(
    selected: &[(usize, &SourceUnitField)],
    focus: &StructuredSourceUnitFocus,
) -> bool {
    if selected.len() <= 12 {
        return false;
    }
    let direct_count = selected
        .iter()
        .filter(|(_, field)| structured_source_unit_field_overlap_score(field, focus) > 0)
        .count();
    direct_count.saturating_mul(2) < selected.len()
}

fn expand_structured_source_unit_sibling_fields<'a>(
    fields: &'a [SourceUnitField],
    selected: &mut Vec<(usize, &'a SourceUnitField)>,
    focus: &StructuredSourceUnitFocus,
) {
    if selected.is_empty() {
        return;
    }
    let primary_fields = selected.iter().map(|(score, field)| (*score, *field)).collect::<Vec<_>>();
    let mut selected_index_by_identity = selected
        .iter()
        .enumerate()
        .map(|(index, (_, field))| (source_unit_field_identity(field), index))
        .collect::<HashMap<_, _>>();

    for field in fields {
        let identity = source_unit_field_identity(field);
        let score = primary_fields
            .iter()
            .filter_map(|(primary_score, primary)| {
                structured_source_unit_sibling_field_score(field, primary, *primary_score, focus)
            })
            .max()
            .unwrap_or(0);
        if let Some(index) = selected_index_by_identity.get(&identity).copied() {
            selected[index].0 = selected[index].0.max(score);
        } else if score > 0 {
            selected_index_by_identity.insert(identity, selected.len());
            selected.push((score, field));
        }
    }
}

fn structured_source_unit_sibling_field_score(
    field: &SourceUnitField,
    primary: &SourceUnitField,
    primary_score: usize,
    focus: &StructuredSourceUnitFocus,
) -> Option<usize> {
    if field.document_label != primary.document_label
        || field.path == primary.path && field.value == primary.value
    {
        return None;
    }

    let shared_prefix = source_unit_path_common_prefix_len(&field.path, &primary.path);
    let direct_overlap = structured_source_unit_field_overlap_score(field, focus);
    if shared_prefix >= 1
        && primary_score >= 3
        && source_unit_field_root_term(field).as_deref().is_some_and(|root| {
            source_unit_field_root_term(primary).as_deref() == Some(root)
                && focus.compact_root_direct_inventory(root)
        })
    {
        let value_specificity = structured_source_unit_value_specificity_score(&field.value);
        return Some(
            primary_score.max(4)
                + 4
                + direct_overlap.min(6)
                + value_specificity.min(3)
                + source_unit_path_specificity_score(&field.path).min(3),
        );
    }
    if shared_prefix < 3 {
        return None;
    }

    let value_specificity = structured_source_unit_value_specificity_score(&field.value);
    if shared_prefix < 4 && direct_overlap == 0 {
        return None;
    }

    Some(
        shared_prefix.min(8)
            + direct_overlap.min(12)
            + value_specificity.min(10)
            + source_unit_path_specificity_score(&field.path).min(4),
    )
}

fn source_unit_field_identity(field: &SourceUnitField) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        field.document_id,
        field.document_label.trim(),
        field.path.trim(),
        field.value.trim()
    )
    .to_lowercase()
}

fn source_unit_path_common_prefix_len(left: &str, right: &str) -> usize {
    source_unit_path_segments(left)
        .into_iter()
        .zip(source_unit_path_segments(right))
        .take_while(|(left, right)| left == right)
        .count()
}

fn source_unit_path_segments(path: &str) -> Vec<String> {
    path.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect()
}

fn source_unit_path_specificity_score(path: &str) -> usize {
    let segment_count = source_unit_path_segments(path).len();
    segment_count.saturating_sub(2)
}

fn structured_source_unit_value_specificity_score(value: &str) -> usize {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let token_score = label_terms_with_simple_variants(trimmed, 2).len().min(5);
    let syntax_score =
        if trimmed.chars().any(|ch| matches!(ch, '.' | '-' | '_' | '/' | ':' | '@' | '#')) {
            3
        } else {
            0
        };
    let digit_score = usize::from(trimmed.chars().any(|ch| ch.is_ascii_digit())) * 2;
    let mixed_case_score = usize::from(
        trimmed.chars().any(|ch| ch.is_ascii_uppercase())
            && trimmed.chars().any(|ch| ch.is_ascii_lowercase()),
    );
    let length_score = match trimmed.chars().count() {
        0..=3 => 0,
        4..=11 => 1,
        _ => 3,
    };
    token_score + syntax_score + digit_score + mixed_case_score + length_score
}

fn structured_source_unit_inventory_focus_terms(
    question: &str,
    query_ir: &QueryIR,
) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    extend_label_terms_with_simple_variants(&mut terms, &current_question_segment(question), 3);
    for target_type in &query_ir.target_types {
        extend_label_terms_with_simple_variants(&mut terms, target_type, 2);
    }
    for entity in &query_ir.target_entities {
        extend_label_terms_with_simple_variants(&mut terms, &entity.label, 2);
    }
    for literal in &query_ir.literal_constraints {
        extend_label_terms_with_simple_variants(&mut terms, &literal.text, 2);
    }
    if let Some(comparison) = query_ir.comparison.as_ref() {
        if let Some(a) = comparison.a.as_deref() {
            extend_label_terms_with_simple_variants(&mut terms, a, 2);
        }
        if let Some(b) = comparison.b.as_deref() {
            extend_label_terms_with_simple_variants(&mut terms, b, 2);
        }
        extend_label_terms_with_simple_variants(&mut terms, &comparison.dimension, 2);
    }
    terms
}

#[derive(Debug, Clone)]
struct StructuredSourceUnitFocus {
    terms: BTreeSet<String>,
    scoring_terms: BTreeSet<String>,
    root_terms: BTreeSet<String>,
    specific_terms: BTreeSet<String>,
    required_surface_terms: BTreeSet<String>,
    root_field_counts: HashMap<String, usize>,
}

impl StructuredSourceUnitFocus {
    fn new(question: &str, query_ir: &QueryIR, fields: &[SourceUnitField]) -> Self {
        let terms = structured_source_unit_inventory_focus_terms(question, query_ir);
        if terms.is_empty() {
            return Self {
                terms,
                scoring_terms: BTreeSet::new(),
                root_terms: BTreeSet::new(),
                specific_terms: BTreeSet::new(),
                required_surface_terms: BTreeSet::new(),
                root_field_counts: HashMap::new(),
            };
        }

        let broad_limit =
            if fields.len() <= 8 { fields.len() } else { (fields.len() / 4).clamp(3, 8) };
        let mut field_counts = HashMap::<String, usize>::new();
        let mut root_field_counts = HashMap::<String, usize>::new();
        for field in fields {
            if let Some(root) = source_unit_field_root_term(field) {
                *root_field_counts.entry(root).or_insert(0) += 1;
            }
            let field_terms = structured_source_unit_field_terms(field);
            for term in terms.intersection(&field_terms) {
                *field_counts.entry(term.clone()).or_insert(0) += 1;
            }
        }

        let root_terms = fields
            .iter()
            .filter_map(source_unit_field_root_term)
            .flat_map(|segment| label_terms_with_simple_variants(&segment, 2))
            .filter(|term| terms.contains(term))
            .collect::<BTreeSet<_>>();

        let scoring_terms = terms
            .iter()
            .filter(|term| !root_terms.contains(*term))
            .filter(|term| field_counts.get(*term).copied().unwrap_or(0) <= broad_limit)
            .cloned()
            .collect::<BTreeSet<_>>();
        let specific_terms = structured_source_unit_specific_terms(question, query_ir)
            .into_iter()
            .filter(|term| !root_terms.contains(term))
            .collect();
        let required_surface_terms =
            structured_source_unit_required_surface_terms(question, query_ir);
        Self {
            terms,
            scoring_terms,
            root_terms,
            specific_terms,
            required_surface_terms,
            root_field_counts,
        }
    }

    fn is_empty(&self) -> bool {
        self.terms.is_empty() || self.scoring_terms.is_empty()
    }

    fn compact_root_direct_inventory(&self, root: &str) -> bool {
        self.root_terms.contains(root)
            && self.root_field_counts.get(root).copied().unwrap_or(0) <= 8
    }
}

fn structured_source_unit_specific_terms(question: &str, query_ir: &QueryIR) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    for term in distinctive_surface_terms(question) {
        terms.insert(term);
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        extend_label_terms_with_simple_variants(&mut terms, &document_focus.hint, 3);
    }
    for entity in &query_ir.target_entities {
        extend_label_terms_with_simple_variants(&mut terms, &entity.label, 3);
    }
    for literal in &query_ir.literal_constraints {
        extend_label_terms_with_simple_variants(&mut terms, &literal.text, 2);
    }
    if let Some(comparison) = query_ir.comparison.as_ref() {
        if let Some(a) = comparison.a.as_deref() {
            extend_label_terms_with_simple_variants(&mut terms, a, 3);
        }
        if let Some(b) = comparison.b.as_deref() {
            extend_label_terms_with_simple_variants(&mut terms, b, 3);
        }
    }
    terms
}

fn structured_source_unit_required_surface_terms(
    question: &str,
    query_ir: &QueryIR,
) -> BTreeSet<String> {
    let mut terms = distinctive_surface_terms(question);
    for literal in &query_ir.literal_constraints {
        terms.extend(distinctive_surface_terms(&literal.text));
    }
    terms
}

fn distinctive_surface_terms(text: &str) -> BTreeSet<String> {
    text.split_whitespace()
        .filter_map(|raw| {
            let candidate = raw
                .trim_matches(|ch: char| {
                    !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'))
                })
                .trim();
            if candidate.chars().filter(|ch| ch.is_alphanumeric()).count() < 3 {
                return None;
            }
            let has_structural_separator =
                candidate.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'));
            let has_digit = candidate.chars().any(|ch| ch.is_ascii_digit());
            let has_upper_after_first = candidate.chars().skip(1).any(|ch| ch.is_ascii_uppercase());
            let has_lower = candidate.chars().any(|ch| ch.is_ascii_lowercase());
            if !(has_structural_separator || has_digit || (has_upper_after_first && has_lower)) {
                return None;
            }
            Some(candidate.to_lowercase())
        })
        .collect()
}

fn structured_source_unit_field_terms(field: &SourceUnitField) -> BTreeSet<String> {
    label_terms_with_simple_variants(&field.path, 2)
        .into_iter()
        .chain(label_terms_with_simple_variants(&field.value, 2))
        .collect()
}

fn source_unit_field_root_term(field: &SourceUnitField) -> Option<String> {
    source_unit_path_segments(&field.path).into_iter().next()
}

fn structured_source_unit_field_overlap_score(
    field: &SourceUnitField,
    focus: &StructuredSourceUnitFocus,
) -> usize {
    let path_terms = label_terms_with_simple_variants(&field.path, 2);
    let value_terms = label_terms_with_simple_variants(&field.value, 2);
    let path_score = path_terms.intersection(&focus.scoring_terms).count() * 4;
    let value_score = value_terms.intersection(&focus.scoring_terms).count() * 3;
    path_score + value_score
}

fn structured_source_unit_compact_root_inventory_score(
    field: &SourceUnitField,
    focus: &StructuredSourceUnitFocus,
) -> usize {
    let Some(root) = source_unit_field_root_term(field) else {
        return 0;
    };
    if !focus.compact_root_direct_inventory(&root) {
        return 0;
    }
    2 + structured_source_unit_value_specificity_score(&field.value).min(3)
        + source_unit_path_specificity_score(&field.path).min(3)
}

fn label_terms_with_simple_variants(text: &str, min_token_chars: usize) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    extend_label_terms_with_simple_variants(&mut terms, text, min_token_chars);
    terms
}

fn extend_label_terms_with_simple_variants(
    terms: &mut BTreeSet<String>,
    text: &str,
    min_token_chars: usize,
) {
    for term in label_terms(text, min_token_chars) {
        insert_term_with_simple_variants(terms, term, min_token_chars);
    }
}

fn insert_term_with_simple_variants(
    terms: &mut BTreeSet<String>,
    term: String,
    min_token_chars: usize,
) {
    if term.chars().count() < min_token_chars {
        return;
    }
    terms.insert(term.clone());

    for suffix in ["ies", "es", "s"] {
        let Some(stem) = term.strip_suffix(suffix) else {
            continue;
        };
        let variant = if suffix == "ies" { format!("{stem}y") } else { stem.to_string() };
        if variant.chars().count() >= min_token_chars {
            terms.insert(variant);
        }
    }
}

#[cfg(test)]
fn build_structured_source_unit_field_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !structured_source_unit_field_answer_allowed(query_ir) {
        return None;
    }
    let focus_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    if focus_keywords.is_empty() {
        return None;
    }
    let mut candidates = chunks
        .iter()
        .filter(|chunk| is_structured_source_unit_runtime_chunk(chunk))
        .filter_map(|chunk| {
            let score = technical_chunk_selection_score(
                &format!("{}\n{}", chunk.excerpt, chunk.source_text),
                &focus_keywords,
                false,
            );
            (score > 0).then_some((score, chunk))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
            .then_with(|| left.document_label.cmp(&right.document_label))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });

    let mut lines = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for (_, chunk) in candidates.into_iter().take(4) {
        let excerpt = focused_record_unit_excerpt(&chunk.source_text, &focus_keywords, 1_200)
            .or_else(|| focused_record_unit_excerpt(&chunk.excerpt, &focus_keywords, 1_200))
            .unwrap_or_else(|| focused_excerpt_for(&chunk.source_text, &focus_keywords, 1_200));
        let excerpt = excerpt.trim();
        if excerpt.is_empty() {
            continue;
        }
        let key = format!("{}\n{}", chunk.document_label, excerpt).to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        lines.push(format!("`{}`: {}", chunk.document_label.trim(), excerpt));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
fn structured_source_unit_field_answer_allowed(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !matches!(query_ir.act, QueryAct::RetrieveValue)
        || query_ir.target_entities.len() > 1
    {
        return false;
    }
    query_ir.is_exact_literal_technical()
        || (query_ir.target_entities.len() <= 1
            && query_ir.literal_constraints.iter().any(|literal| {
                matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Path)
                    && literal.text.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':'))
            }))
}

pub(crate) fn build_exact_version_change_summary_answer(
    query_ir: &QueryIR,
    context_chunks: &[RuntimeMatchedChunk],
    graph_evidence_context_lines: &[String],
) -> Option<String> {
    if query_ir.source_slice.is_some()
        || !matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
    {
        return None;
    }
    if !query_ir
        .target_types
        .iter()
        .any(|target_type| matches!(target_type.as_str(), "version" | "release" | "changelog"))
    {
        return None;
    }
    let version_literals = query_ir
        .literal_constraints
        .iter()
        .filter(|literal| literal.kind == LiteralKind::Version)
        .map(|literal| literal.text.trim())
        .filter(|literal| !literal.is_empty())
        .collect::<Vec<_>>();
    if version_literals.is_empty() {
        return None;
    }

    exact_version_change_candidate_from_graph_lines(&version_literals, graph_evidence_context_lines)
        .or_else(|| exact_version_change_candidate_from_chunks(&version_literals, context_chunks))
        .and_then(render_exact_version_change_candidate)
}

#[derive(Debug)]
struct ExactVersionChangeCandidate {
    title: String,
    bullets: Vec<String>,
}

fn exact_version_change_candidate_from_graph_lines(
    version_literals: &[&str],
    graph_evidence_context_lines: &[String],
) -> Option<ExactVersionChangeCandidate> {
    graph_evidence_context_lines
        .iter()
        .filter_map(|line| {
            let (_header, body) = line.split_once('\n')?;
            if !contains_any_exact_version_literal(body, version_literals) {
                return None;
            }
            let title = body
                .lines()
                .map(str::trim)
                .find(|candidate| {
                    !candidate.is_empty()
                        && contains_any_exact_version_literal(candidate, version_literals)
                })
                .or_else(|| body.lines().map(str::trim).find(|candidate| !candidate.is_empty()))?
                .to_string();
            let bullets = collect_change_bullets(body);
            (bullets.len() >= 2).then_some(ExactVersionChangeCandidate { title, bullets })
        })
        .max_by(|left, right| {
            left.bullets
                .len()
                .cmp(&right.bullets.len())
                .then_with(|| right.title.len().cmp(&left.title.len()))
        })
}

fn exact_version_change_candidate_from_chunks(
    version_literals: &[&str],
    context_chunks: &[RuntimeMatchedChunk],
) -> Option<ExactVersionChangeCandidate> {
    let mut chunks = context_chunks
        .iter()
        .filter(|chunk| {
            contains_any_exact_version_literal(&chunk.document_label, version_literals)
                || contains_any_exact_version_literal(&chunk.source_text, version_literals)
        })
        .collect::<Vec<_>>();
    if chunks.is_empty() {
        return None;
    }
    chunks.sort_by_key(|chunk| {
        (chunk.document_id, chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id)
    });

    let mut candidates = Vec::new();
    for document_chunks in chunks.chunk_by(|left, right| left.document_id == right.document_id) {
        let Some(first) = document_chunks.first() else {
            continue;
        };
        let mut bullets = Vec::new();
        let mut seen = HashSet::new();
        for chunk in document_chunks {
            for bullet in collect_change_bullets(&chunk.source_text) {
                if seen.insert(bullet.clone()) {
                    bullets.push(bullet);
                }
            }
        }
        if bullets.len() >= 2 {
            candidates
                .push(ExactVersionChangeCandidate { title: first.document_label.clone(), bullets });
        }
    }

    candidates.into_iter().max_by(|left, right| left.bullets.len().cmp(&right.bullets.len()))
}

fn render_exact_version_change_candidate(candidate: ExactVersionChangeCandidate) -> Option<String> {
    if candidate.bullets.is_empty() {
        return None;
    }
    let mut lines = vec![format!("**{}**", candidate.title.trim()), String::new()];
    for bullet in candidate.bullets.into_iter().take(16) {
        lines.push(format!("- {bullet}"));
    }
    Some(lines.join("\n"))
}

fn contains_any_exact_version_literal(text: &str, version_literals: &[&str]) -> bool {
    version_literals.iter().any(|literal| contains_exact_version_literal(text, literal))
}

fn contains_exact_version_literal(text: &str, literal: &str) -> bool {
    text.match_indices(literal).any(|(start, _)| {
        let end = start + literal.len();
        let before = text[..start].chars().next_back();
        let after = text[end..].chars().next();
        !before.is_some_and(is_version_literal_boundary_blocker)
            && !after.is_some_and(is_version_literal_boundary_blocker)
    })
}

fn is_version_literal_boundary_blocker(ch: char) -> bool {
    ch.is_alphanumeric() || matches!(ch, '.' | '-')
}

fn collect_change_bullets(text: &str) -> Vec<String> {
    let mut bullets = Vec::new();
    let mut seen = HashSet::new();
    for line in text.lines().map(str::trim) {
        let bullet = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .or_else(|| line.strip_prefix("• "))
            .map(str::trim);
        let Some(bullet) = bullet else {
            continue;
        };
        if bullet.is_empty() || bullet.starts_with("![") {
            continue;
        }
        let normalized = bullet.to_lowercase();
        if seen.insert(normalized) {
            bullets.push(bullet.to_string());
        }
    }
    bullets
}

pub(crate) struct OrderedSourceSliceAnswer {
    pub(crate) answer: String,
    pub(crate) unit_count: usize,
    pub(crate) used_context_fallback: bool,
    /// Chunk ids of the exact units the deterministic answer was built from
    /// (post filtering, dedup, dominant-family retention and truncation).
    /// Downstream probes must scope to these, not the wider context set.
    pub(crate) unit_chunk_ids: Vec<Uuid>,
}

pub(crate) fn build_ordered_source_slice_answer(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
    context_chunks: &[RuntimeMatchedChunk],
) -> Option<OrderedSourceSliceAnswer> {
    let used_context_fallback = source_units.is_empty();
    let units = source_slice_answer_units(query_ir, source_units, context_chunks);
    let answer = build_ordered_source_units_answer(query_ir, &units)?;
    let unit_chunk_ids = units.iter().map(|unit| unit.chunk_id).collect();
    Some(OrderedSourceSliceAnswer {
        answer,
        unit_count: units.len(),
        used_context_fallback,
        unit_chunk_ids,
    })
}

fn source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
    context_chunks: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    let explicit_latest_version_inventory = query_requests_latest_versions(query_ir);
    let inferred_latest_version_inventory = !explicit_latest_version_inventory
        && context_supports_latest_version_inventory(query_ir, context_chunks);
    let source_units_latest_version_inventory = !source_units.is_empty()
        && context_supports_latest_version_inventory(query_ir, source_units);
    let latest_version_inventory = explicit_latest_version_inventory
        || inferred_latest_version_inventory
        || source_units_latest_version_inventory;
    if query_ir.source_slice.is_none() && !latest_version_inventory {
        return Vec::new();
    }
    if !source_units.is_empty() {
        let mut units = source_units.to_vec();
        sort_source_slice_answer_units(query_ir, &mut units);
        let requested_count = if latest_version_inventory {
            latest_source_slice_requested_count(query_ir)
        } else {
            super::source_slice_requested_count(query_ir).unwrap_or(units.len())
        };
        if latest_version_inventory {
            dedupe_latest_source_slice_answer_units(query_ir, &mut units);
            retain_dominant_latest_version_family(&mut units, requested_count);
        }
        if requested_count > 0 && units.len() > requested_count {
            units.truncate(requested_count);
        }
        return units;
    }
    if !latest_version_inventory {
        return Vec::new();
    }

    let requested_count = latest_source_slice_requested_count(query_ir);
    let mut units = context_chunks
        .iter()
        .filter(|chunk| !is_source_profile_runtime_chunk(chunk))
        .filter(|chunk| {
            if explicit_latest_version_inventory {
                chunk_supports_explicit_latest_version_inventory(chunk)
            } else {
                latest_source_slice_answer_unit_version(chunk).is_some()
            }
        })
        .filter(|chunk| source_slice_answer_unit_evidence(query_ir, chunk).is_some())
        .cloned()
        .collect::<Vec<_>>();
    sort_source_slice_answer_units(query_ir, &mut units);

    dedupe_latest_source_slice_answer_units(query_ir, &mut units);
    retain_dominant_latest_version_family(&mut units, requested_count);
    if requested_count > 0 && units.len() > requested_count {
        units.truncate(requested_count);
    }
    units
}

fn sort_source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    units: &mut [RuntimeMatchedChunk],
) {
    if query_requests_latest_versions(query_ir)
        || context_supports_latest_version_inventory(query_ir, units)
    {
        units.sort_by(|left, right| latest_source_slice_answer_unit_order(query_ir, left, right));
    } else {
        units
            .sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id));
    }
}

pub(crate) fn context_supports_latest_version_inventory(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    if query_ir_is_unambiguous_versioned_procedure(query_ir)
        || query_ir_allows_procedure_runbook_target(query_ir)
    {
        return false;
    }
    if !query_ir_allows_latest_version_context_fallback(query_ir) {
        return false;
    }

    let mut family_documents = HashMap::<String, HashSet<Uuid>>::new();
    let mut family_chunk_counts = HashMap::<String, usize>::new();
    for chunk in chunks.iter().filter(|chunk| !is_source_profile_runtime_chunk(chunk)) {
        if latest_source_slice_answer_unit_version(chunk).is_none() {
            continue;
        }
        let family_key = latest_version_family_key(&chunk.document_label);
        family_documents.entry(family_key.clone()).or_default().insert(chunk.document_id);
        *family_chunk_counts.entry(family_key).or_default() += 1;
    }

    let mut candidates = family_documents
        .iter()
        .map(|(family_key, document_ids)| {
            (
                family_key.clone(),
                document_ids.len(),
                family_chunk_counts.get(family_key).copied().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| right.2.cmp(&left.2)).then_with(|| left.0.cmp(&right.0))
    });
    let Some((_family_key, document_count, chunk_count)) = candidates.first() else {
        return false;
    };
    let runner_up_document_count = candidates.get(1).map(|candidate| candidate.1).unwrap_or(0);
    *document_count >= 2 && *document_count > runner_up_document_count && *chunk_count >= 2
}

fn query_ir_allows_latest_version_context_fallback(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.35
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::Enumerate | QueryAct::Meta)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_entities.is_empty()
        && query_ir.comparison.is_none()
        && query_ir.temporal_constraints.is_empty()
        && !query_ir
            .literal_constraints
            .iter()
            .any(|literal| matches!(literal.kind, LiteralKind::Version))
        && query_ir
            .literal_constraints
            .iter()
            .all(|literal| matches!(literal.kind, LiteralKind::NumericCode))
        && query_ir.target_types.iter().all(|target_type| {
            matches!(
                canonical_target_type_tag(target_type).as_str(),
                "concept" | "document" | "release" | "version" | "changelog"
            )
        })
}

fn latest_source_slice_answer_unit_order(
    query_ir: &crate::domains::query_ir::QueryIR,
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    match (
        matches!(left.score_kind, RuntimeChunkScoreKind::LatestVersion),
        matches!(right.score_kind, RuntimeChunkScoreKind::LatestVersion),
    ) {
        (true, true) => {
            let score_order = score_value(right.score).total_cmp(&score_value(left.score));
            if !score_order.is_eq() {
                return score_order;
            }
        }
        (true, false) => return std::cmp::Ordering::Less,
        (false, true) => return std::cmp::Ordering::Greater,
        (false, false) => {}
    }

    match (
        source_slice_answer_unit_version(query_ir, left),
        source_slice_answer_unit_version(query_ir, right),
    ) {
        (Some(left_version), Some(right_version)) => {
            let version_order = compare_version_desc(&left_version, &right_version);
            if !version_order.is_eq() {
                return version_order;
            }
        }
        (Some(_), None) => return std::cmp::Ordering::Less,
        (None, Some(_)) => return std::cmp::Ordering::Greater,
        (None, None) => {}
    }

    score_value(right.score)
        .total_cmp(&score_value(left.score))
        .then_with(|| left.chunk_index.cmp(&right.chunk_index))
        .then_with(|| left.document_label.cmp(&right.document_label))
        .then_with(|| left.chunk_id.cmp(&right.chunk_id))
}

fn latest_source_slice_answer_unit_version(chunk: &RuntimeMatchedChunk) -> Option<Vec<u32>> {
    extract_release_context_version(&chunk.document_label)
}

fn source_slice_answer_unit_version(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<Vec<u32>> {
    source_slice_answer_unit_evidence(query_ir, chunk).map(|evidence| evidence.version)
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct LatestSourceSliceEvidence {
    family_key: String,
    version: Vec<u32>,
}

fn source_slice_answer_unit_evidence(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<LatestSourceSliceEvidence> {
    if query_requests_latest_versions(query_ir) {
        return explicit_latest_source_slice_answer_unit_version(chunk).map(|version| {
            LatestSourceSliceEvidence {
                family_key: latest_version_family_key(&chunk.document_label),
                version,
            }
        });
    }
    latest_source_slice_answer_unit_version(chunk).map(|version| LatestSourceSliceEvidence {
        family_key: latest_version_family_key(&chunk.document_label),
        version,
    })
}

fn explicit_latest_source_slice_answer_unit_version(
    chunk: &RuntimeMatchedChunk,
) -> Option<Vec<u32>> {
    if !latest_source_slice_chunk_has_release_change_marker(chunk) {
        return None;
    }
    latest_source_slice_answer_unit_version(chunk)
        .or_else(|| extract_release_context_version(&chunk.source_text))
        .or_else(|| extract_release_context_version(&chunk.excerpt))
}

fn chunk_supports_explicit_latest_version_inventory(chunk: &RuntimeMatchedChunk) -> bool {
    match chunk.score_kind {
        RuntimeChunkScoreKind::LatestVersion => true,
        RuntimeChunkScoreKind::DocumentIdentity => {
            latest_source_slice_answer_unit_version(chunk).is_some()
        }
        RuntimeChunkScoreKind::QueryIrFocus | RuntimeChunkScoreKind::SourceContext => {
            explicit_latest_source_slice_answer_unit_version(chunk).is_some()
        }
        RuntimeChunkScoreKind::Relevance => {
            latest_source_slice_answer_unit_version(chunk).is_some()
        }
        _ => false,
    }
}

fn latest_source_slice_chunk_has_release_change_marker(chunk: &RuntimeMatchedChunk) -> bool {
    [chunk.document_label.as_str(), chunk.source_text.as_str(), chunk.excerpt.as_str()]
        .into_iter()
        .any(latest_source_slice_text_has_release_change_marker)
}

fn latest_source_slice_text_has_release_change_marker(text: &str) -> bool {
    text.lines().any(latest_source_slice_line_has_release_change_marker)
}

fn latest_source_slice_line_has_release_change_marker(line: &str) -> bool {
    let lowered = line.to_lowercase();
    let has_release_word = [
        "release",
        "version",
        "build",
        "changelog",
        "change",
        "релиз",
        "версия",
        "сборка",
        "изменен",
        "изменения",
    ]
    .iter()
    .any(|marker| lowered.contains(marker));
    has_release_word && extract_release_context_version(line).is_some()
}

fn dedupe_latest_source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    units: &mut Vec<RuntimeMatchedChunk>,
) {
    if query_requests_latest_versions(query_ir) {
        let mut seen_versions = HashSet::<LatestSourceSliceEvidence>::new();
        let mut seen_revisions = HashSet::<Uuid>::new();
        units.retain(|unit| {
            if let Some(evidence) = source_slice_answer_unit_evidence(query_ir, &unit) {
                return seen_versions.insert(evidence);
            }
            seen_revisions.insert(unit.revision_id)
        });
        return;
    }

    let mut seen_revisions = HashSet::<Uuid>::new();
    units.retain(|unit| seen_revisions.insert(unit.revision_id));
}

fn retain_dominant_latest_version_family(
    units: &mut Vec<RuntimeMatchedChunk>,
    requested_count: usize,
) {
    if requested_count <= 1 || units.len() <= requested_count {
        return;
    }
    let family_sizes = units.iter().fold(HashMap::<String, usize>::new(), |mut acc, unit| {
        *acc.entry(latest_version_family_key(&unit.document_label)).or_default() += 1;
        acc
    });
    let mut counts = family_sizes.values().copied().collect::<Vec<_>>();
    counts.sort_unstable_by(|left, right| right.cmp(left));
    let Some((family_key, family_count)) = family_sizes
        .iter()
        .max_by(|left, right| left.1.cmp(right.1).then_with(|| left.0.cmp(right.0)))
        .map(|(family_key, count)| (family_key.clone(), *count))
    else {
        return;
    };
    let runner_up = counts.get(1).copied().unwrap_or(0);
    if family_count >= requested_count && family_count > runner_up {
        units.retain(|unit| latest_version_family_key(&unit.document_label) == family_key);
    }
}

fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(f32::NEG_INFINITY)
}

fn build_ordered_source_units_answer(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
) -> Option<String> {
    let latest_version_inventory = query_requests_latest_versions(query_ir)
        || context_supports_latest_version_inventory(query_ir, source_units);
    if query_ir.source_slice.is_none() && !latest_version_inventory {
        return None;
    }
    if source_units.is_empty() {
        return None;
    }

    let mut units = source_units.to_vec();
    sort_source_slice_answer_units(query_ir, &mut units);
    let requested_count = if query_requests_latest_versions(query_ir) {
        latest_source_slice_requested_count(query_ir)
    } else {
        super::source_slice_requested_count(query_ir).unwrap_or(units.len())
    };
    if requested_count > 0 && units.len() > requested_count {
        units.truncate(requested_count);
    }
    let document_labels = units
        .iter()
        .map(|unit| unit.document_label.trim())
        .filter(|label| !label.is_empty())
        .collect::<std::collections::BTreeSet<_>>();

    let mut lines = Vec::<String>::new();
    if document_labels.len() == 1 {
        let label = document_labels.iter().next().copied().unwrap_or("source");
        lines.push(format!("`{}` - {}/{}", label, units.len(), requested_count));
    } else {
        lines.push(format!("{}/{}", units.len(), requested_count));
    }
    lines.push(String::new());

    let include_document_label = document_labels.len() > 1;
    for (index, unit) in units.iter().enumerate() {
        let parsed = parse_source_unit_text(&unit.source_text);
        let mut heading_parts = Vec::<String>::new();
        if include_document_label {
            heading_parts.push(format!("source=`{}`", unit.document_label.trim()));
        }
        if let Some(heading) = latest_inventory_source_unit_heading(
            latest_version_inventory,
            include_document_label,
            unit,
        ) {
            heading_parts.push(format!("**{}**", heading));
        }
        if let Some(timestamp) = parsed.field("occurred_at") {
            heading_parts.push(format!("**{}**", timestamp));
        }
        if let Some(actor) = parsed
            .field("actor_label")
            .or_else(|| parsed.field("actor_id"))
            .or_else(|| parsed.field("actor_role"))
        {
            heading_parts.push(format!("`{}`", actor));
        } else if let Some(unit_id) = parsed.field("unit_id") {
            heading_parts.push(format!("`unit_id={}`", unit_id));
        }
        if heading_parts.is_empty() {
            heading_parts.push(format!("`ordinal={}`", unit.chunk_index));
        }
        lines.push(format!("{}. {}", index + 1, heading_parts.join(" - ")));

        let body = source_slice_unit_body_for_answer(latest_version_inventory, &parsed);
        if !body.is_empty() {
            lines.push(indent_source_unit_body(&body));
        }
    }

    Some(lines.join("\n"))
}

fn latest_inventory_source_unit_heading(
    latest_version_inventory: bool,
    include_document_label: bool,
    unit: &RuntimeMatchedChunk,
) -> Option<String> {
    if !latest_version_inventory || include_document_label {
        return None;
    }
    let label = compact_source_slice_inventory_line(&unit.document_label)?;
    extract_semver_like_version(&label)?;
    Some(excerpt_for(&label, 160))
}

fn source_slice_unit_body_for_answer(
    latest_version_inventory: bool,
    parsed: &ParsedSourceUnitText,
) -> String {
    let body = parsed.body.trim();
    if latest_version_inventory {
        compact_source_slice_inventory_body(&source_slice_inventory_body_source(parsed))
    } else {
        body.trim().to_string()
    }
}

fn source_slice_inventory_body_source(parsed: &ParsedSourceUnitText) -> String {
    let body = parsed.body.trim();
    let include_header = parsed
        .header
        .as_deref()
        .is_some_and(|header| extract_semver_like_version(header).is_some() || body.is_empty());
    let mut lines = Vec::<String>::new();
    if include_header
        && let Some(header) =
            parsed.header.as_deref().map(str::trim).filter(|header| !header.is_empty())
    {
        lines.push(format!("[{header}]"));
    }
    if !body.is_empty() {
        lines.push(body.to_string());
    }
    if lines.is_empty() { body.to_string() } else { lines.join("\n") }
}

fn compact_source_slice_inventory_body(body: &str) -> String {
    let mut lines = Vec::<String>::new();
    let mut used_chars = 0usize;
    for line in
        body.lines().filter_map(compact_source_slice_inventory_line).filter(|line| !line.is_empty())
    {
        let line_chars = line.chars().count();
        if lines.is_empty() && line_chars > SOURCE_SLICE_COMPACT_BODY_CHARS {
            lines.push(excerpt_for(&line, SOURCE_SLICE_COMPACT_BODY_CHARS));
            break;
        }
        let projected = used_chars.saturating_add(line_chars).saturating_add(1);
        if (!lines.is_empty() && projected > SOURCE_SLICE_COMPACT_BODY_CHARS)
            || lines.len() >= SOURCE_SLICE_COMPACT_BODY_LINES
        {
            break;
        }
        used_chars = projected;
        lines.push(line);
    }
    if lines.is_empty() {
        excerpt_for(body, SOURCE_SLICE_COMPACT_BODY_CHARS)
    } else {
        lines.join("\n")
    }
}

fn latest_source_slice_requested_count(query_ir: &crate::domains::query_ir::QueryIR) -> usize {
    super::source_slice_requested_count(query_ir)
        .unwrap_or_else(|| requested_latest_version_count(query_ir))
}

fn compact_source_slice_inventory_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty()
        || is_markdown_image_line(trimmed)
        || is_markdown_link_only_line(trimmed)
        || is_markdown_horizontal_rule(trimmed)
    {
        return None;
    }
    let without_heading = trimmed.trim_start_matches('#').trim_start();
    if without_heading.is_empty() { None } else { Some(without_heading.to_string()) }
}

fn is_markdown_image_line(line: &str) -> bool {
    line.starts_with("![") && line.contains("](") && line.ends_with(')')
}

fn is_markdown_link_only_line(line: &str) -> bool {
    line.starts_with('[') && line.contains("](") && line.ends_with(')')
}

fn is_markdown_horizontal_rule(line: &str) -> bool {
    let marker_count = line.chars().filter(|ch| matches!(ch, '-' | '_' | '*')).count();
    marker_count >= 3 && marker_count == line.chars().count()
}

pub(crate) fn build_missing_explicit_document_answer(
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<String> {
    let explicit_literals = super::explicit_document_reference_literals(question);
    if explicit_literals.is_empty() {
        return None;
    }

    for document_label in explicit_literals {
        let is_present = super::explicit_document_reference_literal_is_present(
            &document_label,
            document_index.values().flat_map(|document| {
                [
                    document.file_name.as_deref(),
                    document.title.as_deref(),
                    Some(document.external_key.as_str()),
                ]
                .into_iter()
                .flatten()
            }),
        );
        if !is_present {
            return Some(format!(
                "Document `{document_label}` is not present in the active library."
            ));
        }
    }

    None
}

pub(crate) fn render_canonical_technical_fact_section(
    facts: &[KnowledgeTechnicalFactRow],
) -> String {
    if facts.is_empty() {
        return String::new();
    }
    let mut lines = Vec::<String>::new();
    for fact in facts.iter().take(24) {
        let qualifiers = serde_json::from_value::<
            Vec<crate::shared::extraction::technical_facts::TechnicalFactQualifier>,
        >(fact.qualifiers_json.clone())
        .unwrap_or_default();
        let qualifier_suffix = if qualifiers.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                qualifiers
                    .iter()
                    .map(|qualifier| format!("{}={}", qualifier.key, qualifier.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        lines.push(format!("- {}: `{}`{}", fact.fact_kind, fact.display_value, qualifier_suffix));
    }
    format!("Technical facts\n{}", lines.join("\n"))
}

pub(crate) fn render_prepared_segment_section(
    question: &str,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
    blocks: &[KnowledgeStructuredBlockRow],
    suppress_tabular_detail: bool,
) -> String {
    if suppress_tabular_detail {
        return String::new();
    }
    if blocks.is_empty() {
        return String::new();
    }
    let ranked_blocks = rank_prepared_segments_for_answer(question, query_ir, blocks);
    let focus_keywords =
        prepared_segment_answer_focus_tokens(question, query_ir).into_iter().collect::<Vec<_>>();
    let mut lines = Vec::<String>::new();
    for block in ranked_blocks.into_iter().take(super::MAX_ANSWER_BLOCKS) {
        let label = if block.heading_trail.is_empty() {
            block.block_kind.clone()
        } else {
            format!("{} > {}", block.block_kind, block.heading_trail.join(" > "))
        };
        let excerpt = prepared_segment_answer_text(block, &focus_keywords);
        if block.block_kind == "code_block" {
            lines.push(format!(
                "- {} (coverage={}):\n```text\n{}\n```",
                label,
                prepared_segment_answer_coverage(block, PREPARED_STRUCTURAL_BLOCK_CHARS),
                excerpt
            ));
        } else {
            lines.push(format!("- {}: {}", label, excerpt));
        }
    }
    format!("Prepared segments\n{}", lines.join("\n"))
}

fn prepared_segment_answer_text(
    block: &KnowledgeStructuredBlockRow,
    focus_keywords: &[String],
) -> String {
    let repaired = repair_technical_layout_noise(&block.normalized_text);
    let max_chars = if prepared_segment_is_structural(block) {
        PREPARED_STRUCTURAL_BLOCK_CHARS
    } else {
        PREPARED_SEGMENT_EXCERPT_CHARS
    };
    if repaired.trim().chars().count() <= max_chars {
        return repaired.trim().to_string();
    }
    let focused = focused_excerpt_for(&repaired, focus_keywords, max_chars);
    if !focused.trim().is_empty() {
        return focused;
    }
    excerpt_for(&repaired, max_chars)
}

fn prepared_segment_is_structural(block: &KnowledgeStructuredBlockRow) -> bool {
    matches!(block.block_kind.as_str(), "code_block" | "table_row" | "list_item")
}

fn prepared_segment_answer_coverage(
    block: &KnowledgeStructuredBlockRow,
    max_chars: usize,
) -> &'static str {
    if repair_technical_layout_noise(&block.normalized_text).trim().chars().count() <= max_chars {
        "full"
    } else {
        "excerpt"
    }
}

fn rank_prepared_segments_for_answer<'a>(
    question: &str,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
    blocks: &'a [KnowledgeStructuredBlockRow],
) -> Vec<&'a KnowledgeStructuredBlockRow> {
    let focus_tokens = prepared_segment_answer_focus_tokens(question, query_ir);
    if focus_tokens.is_empty() {
        return blocks.iter().collect();
    }

    let token_frequencies = prepared_segment_answer_token_frequencies(blocks);
    let candidate_count = blocks.len().max(1);
    let mut ranked = blocks
        .iter()
        .map(|block| {
            let score = prepared_segment_answer_score(
                block,
                &focus_tokens,
                &token_frequencies,
                candidate_count,
            );
            (block, score)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left, left_score), (right, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
            .then_with(|| left.block_id.cmp(&right.block_id))
    });
    ranked.into_iter().map(|(block, _)| block).collect()
}

fn prepared_segment_answer_focus_tokens(
    question: &str,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
) -> BTreeSet<String> {
    let mut tokens = normalized_alnum_tokens(question, 3);
    let Some(query_ir) = query_ir else {
        return tokens;
    };
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    for entity in &query_ir.target_entities {
        tokens.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    for literal in &query_ir.literal_constraints {
        tokens.extend(normalized_alnum_tokens(&literal.text, 3));
    }
    tokens
}

fn prepared_segment_answer_token_frequencies(
    blocks: &[KnowledgeStructuredBlockRow],
) -> HashMap<String, usize> {
    let mut frequencies = HashMap::<String, usize>::new();
    for block in blocks {
        for token in prepared_segment_answer_block_tokens(block) {
            *frequencies.entry(token).or_default() += 1;
        }
    }
    frequencies
}

fn prepared_segment_answer_score(
    block: &KnowledgeStructuredBlockRow,
    focus_tokens: &BTreeSet<String>,
    token_frequencies: &HashMap<String, usize>,
    candidate_count: usize,
) -> usize {
    let heading_tokens = prepared_segment_answer_heading_tokens(block);
    let body_tokens = normalized_alnum_tokens(&block.normalized_text, 3);
    let heading_score = prepared_segment_answer_overlap_score(
        focus_tokens,
        &heading_tokens,
        token_frequencies,
        candidate_count,
    ) * 8;
    let body_score = prepared_segment_answer_overlap_score(
        focus_tokens,
        &body_tokens,
        token_frequencies,
        candidate_count,
    );
    heading_score + body_score
}

fn prepared_segment_answer_overlap_score(
    focus_tokens: &BTreeSet<String>,
    block_tokens: &BTreeSet<String>,
    token_frequencies: &HashMap<String, usize>,
    candidate_count: usize,
) -> usize {
    focus_tokens
        .iter()
        .filter(|token| block_tokens.contains(*token))
        .map(|token| {
            let frequency = token_frequencies.get(token).copied().unwrap_or(candidate_count);
            candidate_count.saturating_sub(frequency).saturating_add(1)
        })
        .sum()
}

fn prepared_segment_answer_block_tokens(block: &KnowledgeStructuredBlockRow) -> BTreeSet<String> {
    let mut tokens = prepared_segment_answer_heading_tokens(block);
    tokens.extend(normalized_alnum_tokens(&block.normalized_text, 3));
    tokens
}

fn prepared_segment_answer_heading_tokens(block: &KnowledgeStructuredBlockRow) -> BTreeSet<String> {
    let mut heading_text = String::new();
    if !block.heading_trail.is_empty() {
        heading_text.push_str(&block.heading_trail.join(" "));
        heading_text.push(' ');
    }
    if !block.section_path.is_empty() {
        heading_text.push_str(&block.section_path.join(" "));
    }
    normalized_alnum_tokens(&heading_text, 3)
}

pub(crate) fn render_canonical_chunk_section(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
    suppress_tabular_detail: bool,
) -> String {
    if suppress_tabular_detail && question_asks_table_aggregation(question, Some(query_ir)) {
        return String::new();
    }
    if chunks.is_empty() {
        return String::new();
    }
    let filtered_chunks = chunks
        .iter()
        .filter(|chunk| parse_table_column_summary(&chunk.source_text).is_none())
        .cloned()
        .collect::<Vec<_>>();
    if filtered_chunks.is_empty() {
        return String::new();
    }
    if query_ir.requests_source_slice_context()
        && let Some(section) = render_ordered_source_slice_unit_section(query_ir, &filtered_chunks)
    {
        return section;
    }
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let pagination_requested = false;
    let (max_total_chunks, max_chunks_per_document) = if query_ir.requests_source_coverage_context()
        || query_ir_needs_expanded_setup_evidence(question, query_ir, &filtered_chunks)
    {
        (SOURCE_COVERAGE_MAX_TOTAL_CHUNKS, SOURCE_COVERAGE_MAX_CHUNKS_PER_DOCUMENT)
    } else {
        (super::MAX_CHUNKS_PER_DOCUMENT, super::MIN_CHUNKS_PER_DOCUMENT)
    };
    let mut selected = select_document_balanced_chunks(
        question,
        Some(query_ir),
        &filtered_chunks,
        &question_keywords,
        pagination_requested,
        max_total_chunks,
        max_chunks_per_document,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    if selected.is_empty() {
        selected = filtered_chunks.iter().take(8).cloned().collect();
    }
    if query_ir.requests_source_coverage_context() {
        let mut seen_chunk_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
        let mut source_profile_chunks = chunks
            .iter()
            .filter(|chunk| is_source_profile_runtime_chunk(chunk))
            .filter(|chunk| {
                if seen_chunk_ids.contains(&chunk.chunk_id) {
                    false
                } else {
                    seen_chunk_ids.push(chunk.chunk_id);
                    true
                }
            })
            .cloned()
            .collect::<Vec<_>>();
        if !source_profile_chunks.is_empty() {
            source_profile_chunks.extend(selected);
            selected = source_profile_chunks;
        }
    }
    let question_keywords = crate::services::query::planner::extract_keywords(question);
    // For a confident single-document configure/how-to query, render the
    // focused document's setup anchor (the chunk carrying both a command-object
    // literal and a configuration path) in full, ahead of the sampled excerpts.
    // The score-balanced selection above can otherwise drop or window-truncate
    // the "what to install" line in favour of denser parameter-table chunks.
    let setup_install_anchor = focused_setup_install_anchor(question, query_ir, &filtered_chunks);
    let anchor_chunk_id = setup_install_anchor.map(|chunk| chunk.chunk_id);
    let (source_profile_chunks, content_chunks): (Vec<_>, Vec<_>) = selected
        .iter()
        .filter(|chunk| Some(chunk.chunk_id) != anchor_chunk_id)
        .partition(|chunk| is_source_profile_runtime_chunk(chunk));
    let mut sections = Vec::<String>::new();
    if let Some(anchor) = setup_install_anchor {
        sections.push(format!(
            "Setup install anchor (scope=document; coverage=full; document=\"{}\")\n{}",
            context_label(&anchor.document_label),
            anchor.source_text.trim()
        ));
    }
    if !source_profile_chunks.is_empty() {
        let lines = source_profile_chunks
            .iter()
            .map(|chunk| {
                format!(
                    "- [AGGREGATE_PROFILE scope=document coverage=full document=\"{}\"] {}",
                    context_label(&chunk.document_label),
                    source_profile_excerpt(chunk)
                )
            })
            .collect::<Vec<_>>();
        sections.push(format!(
            "AGGREGATE_PROFILE blocks (scope=document; coverage=full)\n{}",
            lines.join("\n")
        ));
    }
    let lines = render_evidence_chunk_lines(&content_chunks, &question_keywords, "sampled");
    if !lines.is_empty() {
        sections.push(format!(
            "EVIDENCE_CHUNK blocks (scope=excerpt; coverage=sampled)\n{}",
            lines.join("\n")
        ));
    }
    sections.join("\n\n")
}

fn focused_setup_install_anchor<'a>(
    question: &str,
    query_ir: &QueryIR,
    chunks: &'a [RuntimeMatchedChunk],
) -> Option<&'a RuntimeMatchedChunk> {
    let focus_tokens = if matches!(query_ir.act, QueryAct::ConfigureHow)
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && query_ir.document_focus.is_some()
    {
        query_ir_document_focus_tokens(query_ir)?
    } else if query_ir_allows_setup_anchor_fallback(query_ir) {
        let tokens = technical_literal_focus_keywords(question, Some(query_ir))
            .into_iter()
            .collect::<BTreeSet<_>>();
        if tokens.is_empty() {
            return None;
        }
        tokens
    } else {
        return None;
    };
    chunks
        .iter()
        .filter(|chunk| chunk_is_setup_focus_command_path_anchor(chunk))
        .filter(|chunk| {
            let label_tokens = normalized_alnum_tokens(&chunk.document_label, 3);
            focus_token_overlap_count(&focus_tokens, &label_tokens) > 0
        })
        .max_by(|left, right| left.score.unwrap_or(0.0).total_cmp(&right.score.unwrap_or(0.0)))
}

fn query_ir_allows_setup_anchor_fallback(query_ir: &QueryIR) -> bool {
    query_ir.confidence <= 0.3
        && matches!(query_ir.act, QueryAct::Describe | QueryAct::ConfigureHow)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir.target_types.is_empty()
        && query_ir.target_entities.is_empty()
        && query_ir.literal_constraints.is_empty()
}

fn query_ir_needs_expanded_setup_evidence(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    if !chunks.iter().any(|chunk| {
        matches!(
            chunk.score_kind,
            RuntimeChunkScoreKind::DocumentIdentity
                | RuntimeChunkScoreKind::LatestVersion
                | RuntimeChunkScoreKind::SourceContext
        )
    }) {
        return false;
    }
    if matches!(query_ir.act, crate::domains::query_ir::QueryAct::ConfigureHow) {
        return true;
    }
    if query_ir_needs_expanded_short_technical_evidence(question, query_ir, chunks) {
        return true;
    }
    query_ir.target_types.iter().any(|target_type| {
        matches!(
            canonical_target_type_tag(target_type).as_str(),
            "configuration_file" | "config_key" | "parameter" | "package"
        )
    })
}

fn query_ir_needs_expanded_short_technical_evidence(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    query_ir_allows_setup_anchor_fallback(query_ir)
        && technical_literal_focus_keywords(question, Some(query_ir))
            .iter()
            .any(|keyword| keyword.chars().count() < 4)
        && chunks.iter().any(|chunk| {
            chunk.score_kind == RuntimeChunkScoreKind::SourceContext
                && !extract_parameter_literals(
                    &format!("{}\n{}", chunk.excerpt, chunk.source_text),
                    2,
                )
                .is_empty()
        })
}

fn render_ordered_source_slice_unit_section(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let slice = query_ir.source_slice.as_ref()?;
    let mut source_profile_chunks =
        chunks.iter().filter(|chunk| is_source_profile_runtime_chunk(chunk)).collect::<Vec<_>>();
    let mut content_chunks =
        chunks.iter().filter(|chunk| !is_source_profile_runtime_chunk(chunk)).collect::<Vec<_>>();
    if content_chunks.iter().any(|chunk| super::source_context::is_source_unit_runtime_chunk(chunk))
    {
        content_chunks.retain(|chunk| super::source_context::is_source_unit_runtime_chunk(chunk));
    }
    if content_chunks.is_empty() {
        return None;
    }
    source_profile_chunks.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index));
    content_chunks.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index));
    let requested_count = super::source_slice_requested_count(query_ir).unwrap_or_default();
    let mut lines = Vec::<String>::new();
    lines.push(format!(
        "SOURCE_SLICE blocks (scope=ordered_source; coverage=ordered; direction={}; requested_count={}; returned_unit_count={})",
        source_slice_direction_label(slice.direction),
        requested_count,
        content_chunks.len()
    ));
    for chunk in source_profile_chunks {
        lines.push(format!(
            "- [SOURCE_PROFILE document=\"{}\"] {}",
            context_label(&chunk.document_label),
            source_profile_excerpt(chunk)
        ));
    }
    for chunk in content_chunks {
        let text = chunk_text_for_source_slice(chunk);
        lines.push(format!(
            "- [SOURCE_SLICE_UNIT direction={} requested_count={} document=\"{}\" ordinal={} coverage=ordered] {}",
            source_slice_direction_label(slice.direction),
            requested_count,
            context_label(&chunk.document_label),
            chunk.chunk_index,
            text
        ));
    }
    Some(lines.join("\n"))
}

fn chunk_text_for_source_slice(chunk: &RuntimeMatchedChunk) -> String {
    let source = chunk.source_text.trim();
    if !source.is_empty() {
        return source.to_string();
    }
    chunk.excerpt.trim().to_string()
}

pub(crate) fn render_targeted_evidence_chunk_section(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> String {
    if chunks.is_empty() {
        return String::new();
    }
    let question_keywords = crate::services::query::planner::extract_keywords(question);
    let chunk_refs = chunks.iter().collect::<Vec<_>>();
    let lines = render_evidence_chunk_lines(&chunk_refs, &question_keywords, "targeted");
    if lines.is_empty() {
        String::new()
    } else {
        format!("EVIDENCE_CHUNK blocks (scope=excerpt; coverage=targeted)\n{}", lines.join("\n"))
    }
}

fn render_evidence_chunk_lines(
    chunks: &[&RuntimeMatchedChunk],
    question_keywords: &[String],
    coverage: &str,
) -> Vec<String> {
    chunks
        .iter()
        .map(|chunk| {
            let (scope, excerpt) = evidence_chunk_scope_and_excerpt(chunk, question_keywords);
            format!(
                "- [EVIDENCE_CHUNK scope={} coverage={} document=\"{}\" chunk_index={}] {}",
                scope,
                coverage,
                context_label(&chunk.document_label),
                chunk.chunk_index,
                excerpt
            )
        })
        .collect()
}

fn evidence_chunk_scope_and_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> (&'static str, String) {
    if chunk.score_kind == RuntimeChunkScoreKind::GraphEvidence {
        let source_text = chunk.source_text.trim();
        if !source_text.is_empty() {
            let excerpt = if source_text.chars().count() <= STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS {
                source_text.to_string()
            } else {
                focused_excerpt_for(
                    source_text,
                    question_keywords,
                    STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS,
                )
            };
            let excerpt = if excerpt.trim().is_empty() {
                excerpt_for(source_text, STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS)
            } else {
                excerpt
            };
            return ("graph_evidence", excerpt);
        }
    }

    if is_structured_source_unit_runtime_chunk(chunk) {
        let source_text = chunk.source_text.trim();
        if source_text.chars().count() <= STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS {
            return ("source_unit", source_text.to_string());
        }
        let excerpt = focused_record_unit_excerpt(
            source_text,
            question_keywords,
            STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS,
        )
        .unwrap_or_else(|| {
            focused_excerpt_for(
                source_text,
                question_keywords,
                STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS,
            )
        });
        let excerpt = if excerpt.trim().is_empty() {
            excerpt_for(source_text, STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS)
        } else {
            excerpt
        };
        return ("source_unit", excerpt);
    }

    if chunk.chunk_kind.as_deref() == Some("code_block") {
        let source_text = repair_technical_layout_noise(&chunk.source_text);
        let excerpt = if source_text.chars().count() <= EVIDENCE_CODE_BLOCK_CHARS {
            source_text
        } else {
            structured_literal_excerpt_for(
                &source_text,
                question_keywords,
                EVIDENCE_CODE_BLOCK_CHARS,
            )
            .unwrap_or_else(|| excerpt_for(&source_text, EVIDENCE_CODE_BLOCK_CHARS))
        };
        return ("code_block", excerpt);
    }

    if let Some(excerpt) =
        salient_source_excerpt_for(&chunk.source_text, question_keywords, EVIDENCE_CODE_BLOCK_CHARS)
    {
        return ("salient_excerpt", excerpt);
    }

    if let Some(excerpt) = structured_literal_excerpt_for(
        &chunk.source_text,
        question_keywords,
        EVIDENCE_CODE_BLOCK_CHARS,
    ) {
        return ("structured_excerpt", excerpt);
    }

    if let Some(excerpt) = command_dense_excerpt_for(&chunk.source_text, EVIDENCE_CODE_BLOCK_CHARS)
    {
        return ("code_block", excerpt);
    }

    let excerpt =
        focused_excerpt_for(&chunk.source_text, question_keywords, EVIDENCE_CHUNK_EXCERPT_CHARS);
    let excerpt = if excerpt.trim().is_empty() {
        excerpt_for(&chunk.source_text, EVIDENCE_CHUNK_EXCERPT_CHARS)
    } else {
        excerpt
    };
    ("excerpt", excerpt)
}

fn is_structured_source_unit_runtime_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    chunk.chunk_kind.as_deref() == Some(super::SOURCE_UNIT_CHUNK_KIND)
}

fn is_source_profile_runtime_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    super::source_profile::is_source_profile_runtime_chunk(chunk)
}

fn source_profile_excerpt(chunk: &RuntimeMatchedChunk) -> String {
    chunk
        .source_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| chunk.source_text.trim())
        .to_string()
}

fn source_slice_direction_label(
    direction: crate::domains::query_ir::SourceSliceDirection,
) -> &'static str {
    match direction {
        crate::domains::query_ir::SourceSliceDirection::Head => "head",
        crate::domains::query_ir::SourceSliceDirection::Tail => "tail",
        crate::domains::query_ir::SourceSliceDirection::All => "all",
    }
}

fn context_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace('"', "\\\"")
}

#[derive(Debug, Default)]
struct ParsedSourceUnitText {
    header: Option<String>,
    fields: HashMap<String, String>,
    body: String,
}

impl ParsedSourceUnitText {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str).filter(|value| !value.trim().is_empty())
    }
}

fn parse_source_unit_text(text: &str) -> ParsedSourceUnitText {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix('[') else {
        return ParsedSourceUnitText {
            header: None,
            fields: HashMap::new(),
            body: trimmed.to_string(),
        };
    };
    let Some((header, body)) = rest.split_once(']') else {
        return ParsedSourceUnitText {
            header: None,
            fields: HashMap::new(),
            body: trimmed.to_string(),
        };
    };
    let fields = header
        .split_whitespace()
        .filter_map(|token| {
            let (key, value) = token.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect::<HashMap<_, _>>();
    ParsedSourceUnitText {
        header: Some(header.trim().to_string()),
        fields,
        body: body.trim().to_string(),
    }
}

fn indent_source_unit_body(body: &str) -> String {
    body.lines().map(|line| format!("   {}", line)).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod source_unit_answer_tests {
    use uuid::Uuid;

    use crate::domains::query_ir::LiteralSpan;

    use super::*;

    fn source_slice_ir(count: u16) -> crate::domains::query_ir::QueryIR {
        crate::domains::query_ir::QueryIR {
            act: crate::domains::query_ir::QueryAct::Enumerate,
            scope: crate::domains::query_ir::QueryScope::SingleDocument,
            language: crate::domains::query_ir::QueryLanguage::Auto,
            target_types: vec!["record".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: Some(crate::domains::query_ir::SourceSliceSpec {
                direction: crate::domains::query_ir::SourceSliceDirection::Tail,
                count: Some(count),
                filter: crate::domains::query_ir::SourceSliceFilter::None,
            }),
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    fn latest_source_slice_ir(count: u16) -> crate::domains::query_ir::QueryIR {
        let mut ir = source_slice_ir(count);
        ir.act = crate::domains::query_ir::QueryAct::Describe;
        ir.scope = crate::domains::query_ir::QueryScope::LibraryMeta;
        ir.target_types = vec!["release".to_string()];
        if let Some(slice) = ir.source_slice.as_mut() {
            slice.filter = crate::domains::query_ir::SourceSliceFilter::ReleaseMarker;
        }
        ir
    }

    fn low_confidence_concept_ir() -> crate::domains::query_ir::QueryIR {
        crate::domains::query_ir::QueryIR {
            act: crate::domains::query_ir::QueryAct::Describe,
            scope: crate::domains::query_ir::QueryScope::LibraryMeta,
            language: crate::domains::query_ir::QueryLanguage::Auto,
            target_types: vec!["concept".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.25,
        }
    }

    fn exact_version_ir(version: &str) -> crate::domains::query_ir::QueryIR {
        crate::domains::query_ir::QueryIR {
            act: crate::domains::query_ir::QueryAct::Describe,
            scope: crate::domains::query_ir::QueryScope::SingleDocument,
            language: crate::domains::query_ir::QueryLanguage::Auto,
            target_types: vec!["version".to_string()],
            target_entities: Vec::new(),
            literal_constraints: vec![crate::domains::query_ir::LiteralSpan {
                text: version.to_string(),
                kind: crate::domains::query_ir::LiteralKind::Version,
            }],
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    fn configure_how_focus_ir(focus: &str) -> crate::domains::query_ir::QueryIR {
        crate::domains::query_ir::QueryIR {
            act: crate::domains::query_ir::QueryAct::ConfigureHow,
            scope: crate::domains::query_ir::QueryScope::SingleDocument,
            language: crate::domains::query_ir::QueryLanguage::Auto,
            target_types: vec!["procedure".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: Some(crate::domains::query_ir::DocumentHint {
                hint: focus.to_string(),
            }),
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.95,
        }
    }

    fn configure_update_focus_ir(focus: &str) -> crate::domains::query_ir::QueryIR {
        let mut query_ir = configure_how_focus_ir(focus);
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: focus.to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        query_ir
    }

    #[test]
    fn render_canonical_chunk_section_surfaces_setup_install_anchor_in_full() {
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\nsample-runner --install sample-link\nSettings are defined in the file /opt/alpha/connector/connector.conf",
        );
        anchor.document_label = "Sample Subject admin guide".to_string();
        anchor.score = Some(1.0);
        let mut dense_filler = evidence_chunk(
            2,
            Some("paragraph"),
            "Parameters: staticWidgetId credentialToken connectorPrimaryId currency widgetLifetime",
        );
        dense_filler.document_label = "Sample Subject admin guide".to_string();
        dense_filler.score = Some(9_999.0);
        let chunks = vec![dense_filler, anchor];

        let section = render_canonical_chunk_section(
            "how to install and configure Sample Subject",
            &configure_how_focus_ir("Sample Subject"),
            &chunks,
            false,
        );

        assert!(section.contains("Setup install anchor"), "anchor section must be rendered");
        assert!(
            section.contains("sample-runner --install sample-link"),
            "install command must be present verbatim"
        );
    }

    #[test]
    fn render_canonical_chunk_section_skips_anchor_without_document_focus() {
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "sample-runner --install sample-link\nfile /opt/alpha/connector/connector.conf",
        );
        anchor.document_label = "Sample Subject admin guide".to_string();
        let chunks = vec![anchor];
        let mut query_ir = configure_how_focus_ir("Sample Subject");
        query_ir.document_focus = None;

        let section = render_canonical_chunk_section(
            "how to install and configure Sample Subject",
            &query_ir,
            &chunks,
            false,
        );

        assert!(!section.contains("Setup install anchor"));
    }

    #[test]
    fn render_canonical_chunk_section_surfaces_setup_anchor_for_low_confidence_fallback_ir() {
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\nsample-runner --install sample-link\nSettings are defined in the file /opt/alpha/connector/connector.conf",
        );
        anchor.document_label = "Sample Subject admin guide".to_string();
        anchor.score = Some(1.0);
        let mut query_ir = configure_how_focus_ir("Sample Subject");
        query_ir.act = QueryAct::Describe;
        query_ir.confidence = 0.25;
        query_ir.target_types.clear();
        query_ir.document_focus = None;

        let section =
            render_canonical_chunk_section("configure Sample Subject", &query_ir, &[anchor], false);

        assert!(section.contains("Setup install anchor"), "fallback anchor must be rendered");
        assert!(
            section.contains("sample-runner --install sample-link"),
            "install command must remain in the prompt context"
        );
    }

    #[test]
    fn render_canonical_chunk_section_expands_low_confidence_short_technical_rows() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = configure_how_focus_ir("Subject Alpha setup");
        query_ir.act = QueryAct::Describe;
        query_ir.confidence = 0.25;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;

        let mut chunks = vec![
            source_context_chunk(document_id, revision_id, 1, "QX alphaFlag = true"),
            source_context_chunk(document_id, revision_id, 2, "QX betaFlag = true"),
            source_context_chunk(document_id, revision_id, 3, "QX gammaFlag = true"),
            source_context_chunk(document_id, revision_id, 4, "QX deltaFlag = true"),
            source_context_chunk(document_id, revision_id, 5, "QX epsilonFlag = true"),
            source_context_chunk(document_id, revision_id, 6, "QX zetaFlag = true"),
            source_context_chunk(document_id, revision_id, 7, "QX etaFlag = true"),
            source_context_chunk(document_id, revision_id, 8, "QX thetaFlag = true"),
            source_context_chunk(document_id, revision_id, 9, "QX iotaFlag = true"),
            source_context_chunk(document_id, revision_id, 10, "QX kappaFlag = true"),
            source_context_chunk(document_id, revision_id, 11, "QX lambdaFlag = true"),
            source_context_chunk(document_id, revision_id, 12, "QX visibleMode = true"),
        ];
        for (rank, chunk) in chunks.iter_mut().enumerate() {
            chunk.document_label = "Subject Alpha setup".to_string();
            chunk.score = Some(100.0 - rank as f32);
        }

        let section = render_canonical_chunk_section("QX settings", &query_ir, &chunks, false);

        assert!(
            section.contains("visibleMode"),
            "low-confidence short-token structured rows below the old per-document cap must remain visible"
        );
    }

    #[test]
    fn render_canonical_chunk_section_keeps_late_setup_code_blocks() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = configure_how_focus_ir("Subject Alpha setup");
        query_ir.act = QueryAct::Describe;
        query_ir.confidence = 0.25;
        query_ir.target_types.clear();
        query_ir.document_focus = None;

        let mut chunks = (1..=21)
            .map(|index| {
                source_context_chunk(
                    document_id,
                    revision_id,
                    index,
                    &format!("QX tableFlag{index} | boolean | true"),
                )
            })
            .collect::<Vec<_>>();
        let mut late_example = source_context_chunk(
            document_id,
            revision_id,
            22,
            "[UI.Component]\nvisibleMode = true",
        );
        late_example.chunk_kind = Some("code_block".to_string());
        chunks.push(late_example);

        let section = render_canonical_chunk_section("QX settings", &query_ir, &chunks, false);

        assert!(
            section.contains("visibleMode = true"),
            "late code-block assignments retained by source-context retrieval must also be rendered for the answer model"
        );
    }

    #[test]
    fn render_canonical_chunk_section_keeps_late_structured_literals_in_plain_chunks() {
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            &format!(
                "{}\nlateStructuralKey = enabled\n/opt/sample/late.conf",
                (0..80)
                    .map(|index| format!(
                        "background narrative line {index} about ordinary setup context"
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        );
        chunk.document_label = "Sample Subject guide".to_string();
        chunk.score = Some(1.0);

        let section = render_canonical_chunk_section(
            "how to configure Sample Subject",
            &configure_how_focus_ir("Sample Subject"),
            &[chunk],
            false,
        );

        assert!(section.contains("lateStructuralKey = enabled"), "{section}");
        assert!(section.contains("/opt/sample/late.conf"), "{section}");
    }

    #[test]
    fn render_canonical_chunk_section_keeps_late_salient_source_lines_in_plain_chunks() {
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            &format!(
                "{}\n2026-02-03: Theta marker changed from state K to state L.",
                (0..80)
                    .map(|index| format!(
                        "background narrative line {index} about ordinary context"
                    ))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        );
        chunk.document_label = "Sample Subject notes".to_string();
        chunk.score = Some(1.0);
        let mut query_ir = configure_how_focus_ir("Sample Subject");
        query_ir.act = QueryAct::Describe;

        let section = render_canonical_chunk_section(
            "describe recent entries for Sample Subject",
            &query_ir,
            &[chunk],
            false,
        );

        assert!(section.contains("Theta marker changed from state K to state L"), "{section}");
    }

    #[test]
    fn deterministic_answer_keeps_salient_source_lines_out_of_visible_answer() {
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "2026-02-03: Theta marker changed from state K to state L.",
        );
        chunk.document_label = "Sample Subject notes".to_string();
        chunk.score = Some(1.0);
        let mut query_ir = configure_how_focus_ir("Sample Subject");
        query_ir.act = QueryAct::Describe;

        let answer = augment_deterministic_grounded_answer_with_evidence(
            "Sample Subject has recent recorded changes.".to_string(),
            "describe recent entries for Sample Subject",
            &query_ir,
            &[chunk],
        );

        assert!(!answer.contains("Theta marker changed from state K to state L"), "{answer}");
    }

    #[test]
    fn deterministic_answer_keeps_focus_only_source_lines_out_of_visible_answer() {
        let mut chunk = evidence_chunk(1, Some("paragraph"), "Theta marker reached stable status");
        chunk.document_label = "Sample Subject notes".to_string();
        chunk.score = Some(1.0);
        let mut query_ir = configure_how_focus_ir("Sample Subject");
        query_ir.act = QueryAct::Describe;

        let answer = augment_deterministic_grounded_answer_with_evidence(
            "Sample Subject has notes.".to_string(),
            "describe Theta marker",
            &query_ir,
            &[chunk],
        );

        assert!(!answer.contains("Theta marker reached stable status"), "{answer}");
    }

    #[test]
    fn update_procedure_answer_augmentation_keeps_non_step_evidence_out_of_visible_answer() {
        let procedure_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update procedure:\n1. Update Sample Target record to revision 1.\n2. Update Sample Target record to revision 2.",
        );
        let evidence_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "2026-02-03: Sample Target Theta marker changed from state K to state L.",
        );
        let mut query_ir = configure_update_focus_ir("Sample Target");
        query_ir.target_types.push("version".to_string());
        let raw_answer = build_update_procedure_sequence_answer(
            "how to update Sample Target version?",
            &query_ir,
            std::slice::from_ref(&procedure_chunk),
        )
        .expect("update procedure answer");
        assert!(!raw_answer.contains("Theta marker changed"), "{raw_answer}");

        let answer = augment_deterministic_grounded_answer_with_evidence(
            raw_answer,
            "how to update Sample Target?",
            &query_ir,
            &[procedure_chunk, evidence_chunk],
        );

        assert!(!answer.contains("Theta marker changed from state K to state L"), "{answer}");
    }

    #[test]
    fn update_procedure_answer_augmentation_does_not_append_selected_source_evidence() {
        let mut procedure_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update procedure:\n1. Update Sample Target record to revision 1.\n2. Update Sample Target record to revision 2.",
        );
        procedure_chunk.document_label = "Sample Target maintenance guide".to_string();
        let mut selected_source_evidence =
            evidence_chunk(2, Some("paragraph"), "2026-02-03: Sample Target audit.marker=state-l.");
        selected_source_evidence.document_label = "Sample Target maintenance guide".to_string();
        let mut same_document_sibling =
            evidence_chunk(4, Some("paragraph"), "2026-02-03: Other Subject audit.marker=state-y.");
        same_document_sibling.document_label = "Sample Target maintenance guide".to_string();
        let mut sibling_evidence =
            evidence_chunk(3, Some("paragraph"), "2026-02-03: Other Subject audit.marker=state-x.");
        sibling_evidence.document_label = "Other Subject maintenance guide".to_string();
        let mut query_ir = configure_update_focus_ir("Sample Target");
        query_ir.target_types.push("version".to_string());
        let raw_answer = build_update_procedure_sequence_answer(
            "how to update Sample Target version?",
            &query_ir,
            &[procedure_chunk.clone()],
        )
        .expect("update procedure answer");
        assert!(raw_answer.contains("Sample Target maintenance guide"), "{raw_answer}");

        let answer = augment_deterministic_grounded_answer_with_evidence(
            raw_answer,
            "how to update Sample Target version?",
            &query_ir,
            &[procedure_chunk, sibling_evidence, same_document_sibling, selected_source_evidence],
        );

        assert!(!answer.contains("audit.marker=state-l"), "{answer}");
        assert!(!answer.contains("audit.marker=state-x"), "{answer}");
        assert!(!answer.contains("audit.marker=state-y"), "{answer}");
    }

    #[test]
    fn update_procedure_answer_augmentation_rejects_unanswered_structural_line() {
        let mut procedure_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update procedure:\n\
             1. Run sample-runner --refresh.\n\
             2. Run sample-runner --apply.",
        );
        procedure_chunk.document_label = "Sample Target maintenance guide".to_string();
        let mut stale_structural_line = evidence_chunk(
            2,
            Some("paragraph"),
            "Move the environment baseline to version 20.04 before continuing.",
        );
        stale_structural_line.document_label = procedure_chunk.document_label.clone();
        let mut query_ir = configure_update_focus_ir("Sample Target");
        query_ir.target_types.push("version".to_string());
        let raw_answer = build_update_procedure_sequence_answer(
            "how to update Sample Target version?",
            &query_ir,
            &[procedure_chunk.clone()],
        )
        .expect("update procedure answer");

        let answer = augment_deterministic_grounded_answer_with_evidence(
            raw_answer,
            "how to update Sample Target version?",
            &query_ir,
            &[procedure_chunk, stale_structural_line],
        );

        assert!(!answer.contains("20.04"), "{answer}");
        assert!(!answer.contains("environment baseline"), "{answer}");
    }

    #[test]
    fn update_procedure_answer_augmentation_rejects_mixed_unanswered_structural_anchors() {
        let mut procedure_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update procedure:\n\
             1. Run sample-runner --refresh.\n\
             2. Run sample-runner --apply.",
        );
        procedure_chunk.document_label = "Sample Target maintenance guide".to_string();
        let mut stale_structural_line = evidence_chunk(
            2,
            Some("paragraph"),
            "For Sample Target, run sample-runner --refresh; if the host baseline is older, run sample-release-upgrade to version 20.04.",
        );
        stale_structural_line.document_label = procedure_chunk.document_label.clone();
        let mut query_ir = configure_update_focus_ir("Sample Target");
        query_ir.target_types.push("version".to_string());
        let raw_answer = build_update_procedure_sequence_answer(
            "how to update Sample Target version?",
            &query_ir,
            &[procedure_chunk.clone()],
        )
        .expect("update procedure answer");

        let answer = augment_deterministic_grounded_answer_with_evidence(
            raw_answer,
            "how to update Sample Target version?",
            &query_ir,
            &[procedure_chunk, stale_structural_line],
        );

        assert!(!answer.contains("sample-release-upgrade"), "{answer}");
        assert!(!answer.contains("20.04"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_command_runbook_over_prose_context() {
        let mut prose = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target version procedure:\n\
             1. Move Sample Target controller to version 1.2.0.\n\
             2. Edit /opt/sample/target.conf.\n\
             3. Review the compatibility checklist.\n\
             4. Restart dependent workflows after validation.",
        );
        prose.document_label = "Sample Target compatibility guide".to_string();
        let mut commands = evidence_chunk(
            2,
            Some("code_block"),
            "Sample Target update procedure:\n\
             1. sudo su\n\
             2. sample-transfer https://example.invalid/sample/update.sh -o /tmp/sample-runner.sh\n\
             3. sample-prepare +x /tmp/sample-runner.sh\n\
             4. /tmp/sample-runner.sh",
        );
        commands.document_label = "Lifecycle maintenance guide".to_string();
        let mut query_ir = configure_update_focus_ir("Sample Target");
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &query_ir,
            &[prose, commands],
        )
        .expect("update procedure answer");

        assert!(answer.contains("Lifecycle maintenance guide"), "{answer}");
        assert!(answer.contains("`sample-transfer"), "{answer}");
        assert!(answer.contains("`/tmp/sample-runner.sh`"), "{answer}");
        assert!(!answer.contains("compatibility guide"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_location_only_body_target_identity() {
        let mut distractor = evidence_chunk(
            1,
            Some("paragraph"),
            "Companion Unit can run on the same node as Sample Target.\n\
             Companion Unit update procedure:\n\
             1. sample-get refresh\n\
             2. sample-get upgrade\n\
             3. sample-configure companion-unit\n\
             4. sudo service companion-unit restart",
        );
        distractor.document_label = "Companion Unit update guide".to_string();
        let mut runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Target update procedure:\n\
             1. sudo su\n\
             2. sample-transfer https://example.invalid/sample/update.sh -o /tmp/sample-runner.sh\n\
             3. sample-prepare +x /tmp/sample-runner.sh\n\
             4. /tmp/sample-runner.sh",
        );
        runbook.document_label = "Installation and maintenance".to_string();
        let mut query_ir = configure_update_focus_ir("Sample Target");
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &query_ir,
            &[distractor, runbook],
        )
        .expect("update procedure answer");

        assert!(answer.contains("Installation and maintenance"), "{answer}");
        assert!(answer.contains("`/tmp/sample-runner.sh`"), "{answer}");
        assert!(!answer.contains("Companion Unit update guide"), "{answer}");
        assert!(!answer.contains("sample-configure companion-unit"), "{answer}");
    }

    #[test]
    fn update_procedure_chunk_text_uses_focused_view_over_dense_source() {
        let mut query_ir = configure_update_focus_ir("Sample Target");
        query_ir.retrieval_query = Some("update Sample Target".to_string());
        let focus_model = update_procedure_focus_model("how to update Sample Target?", &query_ir);
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update procedure:\n1. Update Sample Target record to revision 1.\n2. Update Sample Target record to revision 2.",
        );
        chunk.source_text = concat!(
            "Sample Target update procedure:\n",
            "1. Update Sample Target record to revision 1.\n",
            "2. Update Sample Target record to revision 2.\n",
            "Other procedure:\n",
            "- Rotate Other Subject marker.\n",
            "- Remove Other Subject marker."
        )
        .to_string();

        let source_view = update_procedure_chunk_text(&chunk, &focus_model);

        assert!(source_view.contains("Update Sample Target record"), "{source_view}");
        assert!(!source_view.contains("Other Subject"), "{source_view}");
    }

    #[test]
    fn structured_source_unit_field_answer_preserves_late_matching_fields() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::RetrieveValue;
        query_ir.scope = QueryScope::SingleDocument;
        query_ir.literal_constraints = vec![LiteralSpan {
            kind: LiteralKind::Identifier,
            text: "service.deep.port".to_string(),
        }];
        let wide_unit = format!(
            "[unit_ordinal=0] fields: {}; service.deep.port=8443; service.deep.protocol=https",
            (0..140)
                .map(|index| format!("early_{index:02}=value-{index}"))
                .collect::<Vec<_>>()
                .join("; ")
        );
        let answer = build_structured_source_unit_field_answer(
            "Which service deep port is configured?",
            &query_ir,
            &[source_unit(0, &wide_unit)],
        )
        .expect("structured source-unit answer");

        assert!(answer.contains("service.deep.port=8443"));
        assert!(!answer.contains("early_00=value-0"));
    }

    #[test]
    fn structured_source_unit_field_answer_skips_broad_enumerations() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.scope = QueryScope::SingleDocument;
        query_ir.act = QueryAct::Enumerate;
        query_ir.literal_constraints = vec![LiteralSpan {
            kind: LiteralKind::Identifier,
            text: "services.api.ports".to_string(),
        }];

        let answer = build_structured_source_unit_field_answer(
            "What services are defined and what ports does each service use?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: services.alpha.ports.0=8001:8001; services.gamma.image=sample-store:16",
            )],
        );

        assert!(answer.is_none());
    }

    #[test]
    fn structured_source_unit_inventory_answer_lists_service_ports() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec!["service".to_string(), "port".to_string()];
        let mut chunk = source_unit(
            0,
            "[unit_ordinal=0] fields: services.alpha-edge.environment.ALPHA_ADMIN_LISTEN=0.0.0.0:8001; services.beta-worker.expose=3001; services.gamma-store.ports=${GAMMA_STORE_PORT:-5432}:5432",
        );
        chunk.document_label = "sample_manifest.yaml".to_string();

        let answer = build_structured_source_unit_inventory_answer(
            "What services are defined and what ports does each service use?",
            &query_ir,
            &[chunk],
        )
        .expect("service port inventory answer");

        assert!(answer.contains("alpha-edge"));
        assert!(answer.contains("8001"));
        assert!(answer.contains("beta-worker"));
        assert!(answer.contains("3001"));
        assert!(answer.contains("5432"));
    }

    #[test]
    fn structured_source_unit_inventory_answer_selects_relevant_group_fields() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec!["group".to_string(), "flag".to_string()];
        let answer = build_structured_source_unit_inventory_answer(
            "Which groups are defined and which flags are active?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: groups.alpha.mode=primary; groups.beta.mode=secondary; groups.gamma.flag=true; components.worker.groups=alpha, beta",
            )],
        )
        .expect("generic group inventory answer");

        assert!(answer.contains("alpha"));
        assert!(answer.contains("beta"));
        assert!(answer.contains("gamma"));
        assert!(answer.contains("flag=true"));
    }

    #[test]
    fn structured_source_unit_inventory_answer_abstains_for_typed_table_column_inventory() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec!["table_row".to_string(), "table_summary".to_string()];

        assert!(
            build_structured_source_unit_inventory_answer(
                "Which columns are in the accounts table?",
                &query_ir,
                &[source_unit(
                    0,
                    "[unit_ordinal=0] fields: routes.accounts.columns=Application route handlers; routes.accounts.description=Handler list",
                )],
            )
            .is_none()
        );
    }

    #[test]
    fn structured_source_unit_inventory_answer_abstains_for_latest_release_slice() {
        let query_ir = latest_source_slice_ir(2);

        assert!(
            build_structured_source_unit_inventory_answer(
                "latest release records",
                &query_ir,
                &[source_unit(
                    0,
                    "[unit_ordinal=0] fields: releases.alpha.version=1.0.2; releases.alpha.change=neutral-evidence",
                )],
            )
            .is_none()
        );
    }

    #[test]
    fn structured_source_unit_inventory_answer_selects_relevant_status_fields() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["parameter".to_string(), "error_code".to_string()];
        let answer = build_structured_source_unit_inventory_answer(
            "Which reason codes are valid for quantity adjustments and what status is returned for insufficient quantity?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: records.item.adjust.description=Creates a quantity adjustment record. Positive quantities increase balance (receiving), negative quantities decrease balance (damage, loss, correction).; records.item.adjust.request.schema.properties.reason_code.enum=receiving, return, damage, loss, correction, transfer_in, transfer_out; records.item.adjust.responses.422.description=Insufficient quantity for negative adjustment",
            )],
        )
        .expect("generic status inventory answer");

        assert!(answer.contains("receiving"));
        assert!(answer.contains("damage"));
        assert!(answer.contains("correction"));
        assert!(answer.contains("422"));
    }

    #[test]
    fn structured_source_unit_inventory_answer_selects_relevant_event_and_secret_fields() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["event".to_string(), "credential".to_string()];
        let answer = build_structured_source_unit_inventory_answer(
            "Which events are supported and how are event payloads authenticated?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: callbacks.create.description=Registers a URL to receive event notifications. Supported events: alpha.low, alpha.adjusted, beta.created. Event payloads are signed with HMAC-SHA256 using the returned secret.; callbacks.create.response.schema.properties.secret.description=Secret used for HMAC-SHA256 signatures",
            )],
        )
        .expect("generic event inventory answer");

        assert!(answer.contains("alpha.low"));
        assert!(answer.contains("alpha.adjusted"));
        assert!(answer.contains("beta.created"));
        assert!(answer.contains("HMAC-SHA256"));
    }

    #[test]
    fn structured_source_unit_inventory_answer_expands_relevant_sibling_fields() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["event".to_string(), "credential".to_string()];
        let answer = build_structured_source_unit_inventory_answer(
            "Which events are supported and how are event payloads authenticated?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: resources.alpha.create.responses.201.schema.properties.events.type=array; resources.alpha.create.responses.201.schema.properties.events.items.enum.0=alpha.low; resources.alpha.create.responses.201.schema.properties.events.items.enum.1=alpha.adjusted; resources.alpha.create.responses.201.schema.properties.id.format=uuid; resources.alpha.create.responses.201.schema.properties.secret.description=HMAC-SHA256 signing secret shown once; resources.alpha.create.responses.201.schema.properties.secret.type=string; resources.alpha.create.summary=Register event receiver",
            )],
        )
        .expect("sibling-expanded structured inventory answer");

        assert!(answer.contains("alpha.low"));
        assert!(answer.contains("alpha.adjusted"));
        assert!(answer.contains("HMAC-SHA256"));
    }

    #[test]
    fn structured_source_unit_inventory_answer_keeps_specific_sibling_fields_with_broad_distractors()
     {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["event".to_string(), "credential".to_string()];
        let answer = build_structured_source_unit_inventory_answer(
            "What callback events does the inventory API support, and how are callback payloads authenticated?",
            &query_ir,
            &[
                source_unit(
                    0,
                    "[unit_ordinal=0] fields: info.description=REST API for managing inventory, records, orders, and supplier relationships with real-time synchronization; paths._records.get.description=Returns records with filtering by category, status, location, and range; paths._records.post.description=Creates a new record and updates inventory counters; components.schemas.Record.properties.status.enum=active, discontinued, draft",
                ),
                source_unit(
                    1,
                    "[unit_ordinal=0] fields: paths._callbacks.post.requestBody.required=true; paths._callbacks.post.responses.201.content.application_json.schema.properties.created_at.format=date-time; paths._callbacks.post.responses.201.content.application_json.schema.properties.created_at.type=string; paths._callbacks.post.responses.201.content.application_json.schema.properties.events.type=array; paths._callbacks.post.responses.201.content.application_json.schema.properties.id.format=uuid; paths._callbacks.post.responses.201.content.application_json.schema.properties.id.type=string; paths._callbacks.post.responses.201.content.application_json.schema.properties.secret.description=HMAC-SHA256 signing secret shown once; paths._callbacks.post.responses.201.content.application_json.schema.properties.secret.type=string; paths._callbacks.post.responses.201.description=Callback registered; paths._callbacks.post.summary=Register a callback endpoint; paths._callbacks.post.tags=Callbacks",
                ),
            ],
        )
        .expect("specific sibling fields should survive broad distractors");

        assert!(answer.contains("events.type=array"), "{answer}");
        assert!(answer.contains("HMAC-SHA256"), "{answer}");
    }

    #[test]
    fn structured_source_unit_inventory_answer_expands_compact_root_groups_ahead_of_subject_noise()
    {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["group".to_string(), "state".to_string()];
        let answer = build_structured_source_unit_inventory_answer(
            "Which groups are defined, which are active, and what arrangement is described?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: groups.alpha.mode=primary; groups.beta.active=true; groups.gamma.mode=standby; items.runner.description=Group-aware runner component; items.worker.description=Arranges jobs across active targets",
            )],
        )
        .expect("compact root inventory answer");

        let alpha_index = answer.find("groups.alpha.mode=primary").unwrap_or(usize::MAX);
        let gamma_index = answer.find("groups.gamma.mode=standby").unwrap_or(usize::MAX);
        let item_index = answer.find("items.runner.description").unwrap_or(usize::MAX);
        assert!(alpha_index < item_index, "{answer}");
        assert!(gamma_index < item_index, "{answer}");
        assert!(answer.contains("groups.beta.active=true"), "{answer}");
    }

    #[test]
    fn structured_source_unit_inventory_answer_does_not_expand_large_root_as_direct_match() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec!["item".to_string(), "value".to_string()];
        let distractors = (0..10)
            .map(|index| format!("items.extra{index}.description=Supplemental note {index}"))
            .collect::<Vec<_>>()
            .join("; ");
        let answer = build_structured_source_unit_inventory_answer(
            "Which item values are listed?",
            &query_ir,
            &[source_unit(
                0,
                &format!(
                    "[unit_ordinal=0] fields: items.alpha.values=10, 20; items.beta.values=30, 40; {distractors}"
                ),
            )],
        )
        .expect("direct value inventory answer");

        assert!(answer.contains("items.alpha.values=10, 20"), "{answer}");
        assert!(answer.contains("items.beta.values=30, 40"), "{answer}");
        assert!(!answer.contains("items.extra0.description"), "{answer}");
    }

    #[test]
    fn structured_source_unit_inventory_answer_ignores_off_document_source_units_when_ordinary_chunk_wins()
     {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["group".to_string(), "state".to_string()];
        let target_document_id = Uuid::now_v7();
        let mut ordinary = evidence_chunk(
            0,
            Some("paragraph"),
            "Target document describes the requested groups in prose.",
        );
        ordinary.document_id = target_document_id;
        ordinary.document_label = "target.txt".to_string();
        ordinary.score = Some(25.0);
        let mut off_document = source_unit(
            1,
            "[unit_ordinal=0] fields: groups.alpha.mode=primary; groups.beta.active=true",
        );
        off_document.document_label = "other.json".to_string();
        off_document.score = Some(10.0);

        assert!(
            build_structured_source_unit_inventory_answer(
                "Which groups are defined and active?",
                &query_ir,
                &[ordinary, off_document],
            )
            .is_none()
        );
    }

    #[test]
    fn structured_source_unit_inventory_answer_requires_distinctive_surface_anchor_when_present() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["service".to_string(), "connection".to_string()];

        assert!(
            build_structured_source_unit_inventory_answer(
                "Which services use BETA_QUEUE_URL and what is the connection value?",
                &query_ir,
                &[source_unit(
                    0,
                    "[unit_ordinal=0] fields: services.alpha.environment.DATABASE_URL=alpha-db; services.alpha.environment.CACHE_URL=alpha-cache",
                )],
            )
            .is_none()
        );

        let answer = build_structured_source_unit_inventory_answer(
            "Which services use BETA_QUEUE_URL and what is the connection value?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: services.beta.environment.BETA_QUEUE_URL=beta-queue; services.beta.environment.CACHE_URL=beta-cache",
            )],
        )
        .expect("exact surface anchor should allow source-unit answer");

        assert!(answer.contains("BETA_QUEUE_URL"), "{answer}");
        assert!(answer.contains("beta-queue"), "{answer}");
    }

    #[test]
    fn structured_source_unit_inventory_answer_uses_document_with_exact_field_anchor() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec!["entry".to_string(), "value".to_string()];
        query_ir.literal_constraints =
            vec![LiteralSpan { kind: LiteralKind::Identifier, text: "AlphaToken".to_string() }];
        let focused_document_id = Uuid::now_v7();
        let distractor_document_id = Uuid::now_v7();
        let mut focused = source_unit(
            0,
            "[unit_ordinal=0] fields: entries.sender.marker=AlphaToken; entries.sender.value=alphatoken://{account}/{item}; entries.sender.mode=active",
        );
        focused.document_id = focused_document_id;
        focused.document_label = "focused-records.jsonl".to_string();
        let mut distractor = source_unit(
            1,
            "[unit_ordinal=0] fields: entries.worker.value=beta://worker; entries.monitor.value=gamma://monitor; entries.worker.mode=active",
        );
        distractor.document_id = distractor_document_id;
        distractor.document_label = "nearby-records.jsonl".to_string();
        distractor.score = Some(1_000_000.0);

        let answer = build_structured_source_unit_inventory_answer(
            "Which entries reference AlphaToken and what value format is used?",
            &query_ir,
            &[distractor, focused],
        )
        .expect("focused source-unit inventory answer");

        assert!(answer.contains("focused-records.jsonl"), "{answer}");
        assert!(answer.contains("AlphaToken"), "{answer}");
        assert!(answer.contains("alphatoken://{account}/{item}"), "{answer}");
        assert!(!answer.contains("nearby-records.jsonl"), "{answer}");
        assert!(!answer.contains("beta://worker"), "{answer}");
    }

    #[test]
    fn structured_source_unit_inventory_answer_keeps_noisy_same_item_siblings_below_direct_fields()
    {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec!["value".to_string()];
        let answer = build_structured_source_unit_inventory_answer(
            "Which values are exposed for each item?",
            &query_ir,
            &[source_unit(
                0,
                "[unit_ordinal=0] fields: items.alpha.values=10, 20; items.alpha.description=Detailed Alpha component with long identifier alpha-component-2026-prod; items.beta.values=30, 40; items.beta.description=Detailed Beta component with long identifier beta-component-2026-prod",
            )],
        )
        .expect("direct value inventory answer");

        let values_index = answer.find("items.alpha.values=10, 20").unwrap_or(usize::MAX);
        let description_index = answer.find("items.alpha.description").unwrap_or(usize::MAX);
        assert!(values_index < description_index, "{answer}");
        assert!(answer.contains("items.beta.values=30, 40"), "{answer}");
    }

    #[test]
    fn structured_source_unit_inventory_answer_upgrades_weak_direct_sibling_matches() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["event".to_string()];
        let distractors = (0..24)
            .map(|index| format!("channels.alpha.metadata.field{index}=metadata-{index}"))
            .collect::<Vec<_>>()
            .join("; ");
        let answer = build_structured_source_unit_inventory_answer(
            "Which channel events are supported and how are channel payloads authenticated?",
            &query_ir,
            &[source_unit(
                0,
                &format!(
                    "[unit_ordinal=0] fields: {distractors}; channels.alpha.create.schema.properties.events.type=array; channels.alpha.create.schema.properties.events.items.enum=alpha.low, alpha.adjusted; channels.alpha.create.schema.properties.secret.description=HMAC-SHA256 signing secret"
                ),
            )],
        )
        .expect("sibling-expanded structured inventory answer");

        assert!(answer.contains("alpha.low"), "{answer}");
        assert!(answer.contains("alpha.adjusted"), "{answer}");
        assert!(answer.contains("HMAC-SHA256"), "{answer}");
    }

    #[test]
    fn structured_source_unit_inventory_answer_abstains_for_broad_concept_explanation() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["protocol".to_string(), "concept".to_string()];
        let document_id = Uuid::now_v7();
        let mut ordinary = evidence_chunk(
            0,
            Some("paragraph"),
            "Protocol X version 2 improves multiplexing, header compression, and binary framing over version 1.",
        );
        ordinary.document_id = document_id;
        ordinary.document_label = "protocol-x-notes.md".to_string();
        let mut structured = source_unit(
            1,
            "[unit_ordinal=0] fields: services.alpha.endpoint=https://example.invalid:9443; services.alpha.timeout=30; services.beta.enabled=true",
        );
        structured.document_id = Uuid::now_v7();
        structured.document_label = "neighboring-config.yaml".to_string();

        let answer = build_structured_source_unit_inventory_answer(
            "What are the main improvements of Protocol X version 2 over version 1?",
            &query_ir,
            &[ordinary, structured],
        );

        assert!(
            answer.is_none(),
            "broad concept/protocol questions should use ordinary evidence synthesis instead of structured field inventory: {answer:?}"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_extracts_steps_from_evidence() {
        let mut distractor_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Environment host refresh note. sudo platform-refresh",
        );
        distractor_chunk.document_label = "Environment host maintenance guide".to_string();
        let mut product_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Subject update procedure:\n1. Update Sample Control Object to version 1.2.3 or higher.\n2. Update the secondary record to version 2.\n3. Update Alpha subject artifact to version 1.8.0 or higher.\n4. Update Alpha subject artifact to version 2.0.0 or higher.\nSkipping steps is forbidden.",
        );
        product_chunk.document_label = "Sample Subject upgrade guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[distractor_chunk, product_chunk],
        )
        .expect("update procedure answer");

        assert!(answer.contains("Sample Control Object"));
        assert!(answer.contains("1.2.3"));
        assert!(answer.contains("1.8.0"));
        assert!(answer.contains("2.0.0"));
        assert!(answer.contains("Skipping steps is forbidden"));
        assert!(!answer.contains("platform-refresh"));
    }

    #[test]
    fn update_procedure_sequence_answer_projects_focused_tail_from_mixed_block() {
        let mut mixed_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Console update procedure:\n\
             1. sample-platform-refresh --all\n\
             2. sample-platform-upgrade --major\n\
             3. sample-platform-transition --commit\n\
             4. repeat sample-platform-transition --commit if the platform reports pending work\n\
             5. sample-runner --refresh\n\
             6. sample-runner --upgrade sample-console-rest\n\
             7. sample-configure sample-console-rest\n\
             8. sudo service sample-console-rest restart",
        );
        mixed_chunk.document_label = "Sample Console update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Console?",
            &configure_update_focus_ir("Sample Console"),
            &[mixed_chunk],
        )
        .expect("focused tail procedure answer");

        assert!(answer.contains("sample-runner --refresh"), "{answer}");
        assert!(answer.contains("sample-runner --upgrade sample-console-rest"), "{answer}");
        assert!(answer.contains("sample-configure sample-console-rest"), "{answer}");
        assert!(answer.contains("sudo service sample-console-rest restart"), "{answer}");
        assert!(!answer.contains("sample-platform-transition"), "{answer}");
    }

    #[test]
    fn update_procedure_tail_projection_keeps_adjacent_same_head_preparation_command() {
        let query_ir = configure_update_focus_ir("Sample Console");
        let focus_model = update_procedure_focus_model("how to update Sample Console?", &query_ir);
        let text = "Sample Console update from 1.0.0 to 2.0.0:\n\
             1. platform-transition --prepare\n\
             2. sample-get refresh\n\
             3. sample-get upgrade\n\
             4. sample-configure connector-rest\n\
             5. sudo service connector-rest restart\n\
             6. sample-check connector-rest";
        let blocks = update_procedure_line_blocks(text);
        let block = blocks.first().expect("procedure block");
        let source_extract =
            update_procedure_extract_from_block(0, block, &focus_model).expect("source extract");
        let tail_extract = update_procedure_command_tail_projection_from_block(
            0,
            block,
            &focus_model,
            &source_extract,
        )
        .expect("tail projection");

        let steps = tail_extract.steps.join("\n");
        assert!(steps.contains("sample-get refresh"), "{steps}");
        assert!(steps.contains("sample-get upgrade"), "{steps}");
        assert!(steps.contains("sample-configure connector-rest"), "{steps}");
        assert!(!steps.contains("platform-transition"), "{steps}");
    }

    #[test]
    fn update_procedure_selection_prepends_adjacent_same_head_preparation_command() {
        let steps =
            vec!["sample-get upgrade".to_string(), "sample-configure alpha-service".to_string()];
        let block_text = "Sample Target update procedure:\n\
             1. platform-transition --prepare\n\
             2. sample-get refresh\n\
             3. sample-get upgrade\n\
             4. sample-configure alpha-service";

        let augmented =
            update_procedure_steps_with_adjacent_same_head_preparation(steps, block_text);

        assert_eq!(augmented[0], "sample-get refresh");
        assert_eq!(augmented[1], "sample-get upgrade");
        assert!(!augmented.iter().any(|step| step.contains("platform-transition")));
    }

    #[test]
    fn update_procedure_selection_prepends_same_head_preparation_from_dense_line() {
        let steps =
            vec!["sample-get upgrade".to_string(), "sample-configure alpha-service".to_string()];
        let block_text = "Sample Target update procedure: sample-get refresh sample-get upgrade\n\
             sample-configure alpha-service";

        let augmented =
            update_procedure_steps_with_adjacent_same_head_preparation(steps, block_text);

        assert_eq!(augmented[0], "sample-get refresh");
        assert_eq!(augmented[1], "sample-get upgrade");
        assert!(augmented.iter().any(|step| step == "sample-configure alpha-service"));
    }

    #[test]
    fn structured_list_answer_extracts_supported_bullets_from_evidence() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.retrieval_query =
            Some("supported alpha beta gamma item types vector mode".to_string());
        let chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Overview\n\nSupported Items:\n- Alpha + Beta mode\n- Gamma mode\n- Delta lightweight mode\n\nMetrics:\n- Noise\n- Distractor",
        );

        let answer = build_structured_list_grounded_answer(
            "What item types does the component support, and which use vector mode?",
            &query_ir,
            &[chunk],
        )
        .expect("supported item list answer");

        assert!(answer.contains("Alpha + Beta mode"), "{answer}");
        assert!(answer.contains("Gamma mode"), "{answer}");
        assert!(answer.contains("Delta lightweight mode"), "{answer}");
        assert!(!answer.contains("Noise"), "{answer}");
    }

    #[test]
    fn structured_list_answer_uses_typed_ir_without_question_keywords() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "DeltaProcessor".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        query_ir.retrieval_query = Some("DeltaProcessor alpha beta gamma".to_string());
        let chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "DeltaProcessor:\n- Alpha branch\n- Beta branch\n- Gamma branch",
        );

        let answer = build_structured_list_grounded_answer("Δelta?", &query_ir, &[chunk])
            .expect("list answer");

        assert!(answer.contains("Alpha branch"), "{answer}");
        assert!(answer.contains("Beta branch"), "{answer}");
        assert!(answer.contains("Gamma branch"), "{answer}");
    }

    #[test]
    fn structured_list_answer_abstains_for_typed_table_column_inventory() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::Enumerate;
        query_ir.target_types = vec!["table_row".to_string(), "table_summary".to_string()];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "accounts".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        query_ir.retrieval_query = Some("Which columns are in the accounts table?".to_string());
        let chunk = evidence_chunk(
            1,
            Some("code_block"),
            "// Application route handlers\n// Accounts list endpoint\n// Handler queries records",
        );

        assert!(
            build_structured_list_grounded_answer(
                "Which columns are in the accounts table?",
                &query_ir,
                &[chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn structured_list_answer_extracts_ordered_steps_from_evidence() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.retrieval_query =
            Some("processor steps lowercase html url email whitespace".to_string());
        let chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "class TextProcessor:\n    \"\"\"Applies the following transformations in order:\n    1. Lowercase conversion\n    2. HTML tag removal\n    3. URL replacement\n    4. Email replacement\n    5. Whitespace normalization\n    \"\"\"",
        );

        let answer = build_structured_list_grounded_answer(
            "What processing steps does TextProcessor apply, and in what order?",
            &query_ir,
            &[chunk],
        )
        .expect("ordered list answer");

        assert!(answer.contains("1. Lowercase conversion"), "{answer}");
        assert!(answer.contains("2. HTML tag removal"), "{answer}");
        assert!(answer.contains("3. URL replacement"), "{answer}");
        assert!(answer.contains("4. Email replacement"), "{answer}");
        assert!(answer.contains("5. Whitespace normalization"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_allows_single_document_procedure_ir_without_entity() {
        let mut query_ir = configure_how_focus_ir("Sample Target");
        query_ir.document_focus = None;
        query_ir.target_entities.clear();
        query_ir.target_types = vec!["procedure".to_string(), "release".to_string()];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        let mut product_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Alpha subject update procedure:\n\
             1. Run sudo /opt/alpha/install_update.sh.\n\
             2. Restart the Alpha subject service.\n\
             3. Validate the package version.",
        );
        product_chunk.document_label = "Alpha subject update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &query_ir,
            &[product_chunk],
        )
        .expect("single-document procedure answer");

        assert!(answer.contains("install_update.sh"));
        assert!(answer.contains("Restart the Alpha subject service"));
        assert!(answer.contains("Validate the package version"));
    }

    #[test]
    fn update_procedure_sequence_answer_keeps_concept_only_ir_on_synthesis_path() {
        let mut query_ir = configure_how_focus_ir("Sample Target");
        query_ir.document_focus = None;
        query_ir.target_entities.clear();
        query_ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        query_ir.retrieval_query = Some("Sample Target".to_string());
        let mut product_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Alpha subject update procedure:\n\
             1. Run sudo /opt/alpha/install_update.sh.\n\
             2. Restart the Alpha subject service.",
        );
        product_chunk.document_label = "Alpha subject update guide".to_string();

        assert!(
            build_update_procedure_sequence_answer("Sample Target", &query_ir, &[product_chunk])
                .is_none()
        );
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_focused_concept_procedure_ir() {
        let mut query_ir = configure_how_focus_ir("Sample Target");
        query_ir.document_focus = None;
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Target".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        query_ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        query_ir.retrieval_query = Some("how to update Sample Target?".to_string());
        let mut product_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Target update procedure:\n\
             1. Run sudo sample-target-update --apply.\n\
             2. Restart the Sample Target service.\n\
             3. Validate the reported version.",
        );
        product_chunk.document_label = "Sample Target maintenance guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &query_ir,
            &[product_chunk],
        )
        .expect("focused concept/procedure IR should use structural procedure evidence");

        assert!(answer.contains("sample-target-update"), "{answer}");
        assert!(answer.contains("Restart the Sample Target service"), "{answer}");
        assert!(answer.contains("Validate the reported version"), "{answer}");
    }

    #[test]
    fn deterministic_answer_labels_follow_explicit_query_language() {
        let english = deterministic_answer_labels(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
        );
        assert_eq!(english.update_sequence, "Steps");
        assert_eq!(english.parameter_details, "Parameter details");

        let mut russian_ir = configure_update_focus_ir("S1");
        russian_ir.language = crate::domains::query_ir::QueryLanguage::Ru;
        let russian = deterministic_answer_labels("how to update S1?", &russian_ir);
        assert_eq!(
            russian.parameter_details,
            i18n::RU_DETERMINISTIC_ANSWER_LABELS.parameter_details
        );

        let auto_question =
            deterministic_answer_labels("placeholder S1?", &configure_update_focus_ir("S1"));
        assert_eq!(auto_question.update_sequence, "Steps");
    }

    #[test]
    fn update_procedure_sequence_answer_ignores_numeric_data_records() {
        let mut data_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "{\"metricA\": 14.31}\nmetricB = 82.10\n| metricC | 9.20 |\nmetricD: 3.40",
        );
        data_chunk.document_label = "Sample Subject metric extract".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[data_chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_focused_command_block_without_versions() {
        let mut distractor_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Environment transition transition:\n\
             1. Update the environment from baseline 1 to baseline 2.\n\
             2. Update the environment from baseline 2 to baseline 3.\n\
             3. Run sudo platform-release.",
        );
        distractor_chunk.document_label = "Environment transition transition".to_string();
        let mut product_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Target update:\n\
             sudo su\n\
             sample-transfer https://updates.example.invalid/alpha/update.sh -o /tmp/sample-runner.sh\n\
             sample-prepare +x /tmp/sample-runner.sh\n\
             /tmp/sample-runner.sh\n\
             The update script refreshes Sample Target dependencies.",
        );
        product_chunk.document_label = "Sample Target update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[distractor_chunk, product_chunk],
        )
        .expect("focused command update answer");

        assert!(answer.contains("Sample Target update guide"));
        assert!(answer.contains("/tmp/sample-runner.sh"));
        assert!(answer.contains("sample-prepare +x"));
        assert!(!answer.contains("**Evidence fragments:**"));
        assert!(answer.contains("`/tmp/sample-runner.sh`"));
        assert!(!answer.contains("platform-release"));
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_subject_match_without_action_match() {
        let mut upload_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target receipt upload:\n\
             sudo su\n\
             sample-transfer https://updates.example.invalid/alpha/upload-agent.sh -o /tmp/upload-agent.sh\n\
             /tmp/upload-agent.sh\n\
             The upload agent sends receipt payloads.",
        );
        upload_chunk.document_label = "Sample Target upload example".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[upload_chunk],
            )
            .is_none(),
            "a subject-matching command block must not become an update answer unless the action also matches"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_subject_variant_as_action_match() {
        let mut maintenance_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Alpha Service maintenance:\n\
             1. Check Alpha Service package version 1.0.0.\n\
             2. Restart Alpha Service workers.",
        );
        maintenance_chunk.document_label = "Alpha Service maintenance guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to refresh Alpha Services?",
                &configure_update_focus_ir("Alpha Service"),
                &[maintenance_chunk],
            )
            .is_none(),
            "subject variants must not satisfy the action gate through fuzzy prefix matching"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_action_leak_from_adjacent_block() {
        let mut mixed_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target refresh notes mention the requested action.\n\
             \n\
             Sample Target upload helper:\n\
             sudo su\n\
             sample-transfer https://packages.example.invalid/alpha/upload-helper.sh -o /tmp/upload-helper.sh\n\
             sample-prepare +x /tmp/upload-helper.sh\n\
             /tmp/upload-helper.sh",
        );
        mixed_chunk.document_label = "Sample Target operations".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to refresh Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[mixed_chunk],
            )
            .is_none(),
            "action evidence in an adjacent block must not validate the selected procedure block"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_action_leak_from_quoted_step_reference() {
        let mut install_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target installation:\n\
             1. Install the Alpha subject artifact.\n\
             2. Install the agent. Read the section \"Installing and updating the agent\".\n\
             sudo su\n\
             sample-transfer https://packages.example.invalid/alpha/install-agent.sh -o /tmp/install-agent.sh\n\
             sample-prepare +x /tmp/install-agent.sh\n\
             /tmp/install-agent.sh",
        );
        install_chunk.document_label = "Sample Target installation guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[install_chunk],
            )
            .is_none(),
            "a quoted reference title inside an install step must not satisfy the requested update action"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_setup_script_under_action_heading() {
        let mut install_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update:\n\
             sudo su\n\
             sample-transfer https://packages.example.invalid/alpha/install-agent.sh -o /tmp/install-agent.sh\n\
             sample-prepare +x /tmp/install-agent.sh\n\
             /tmp/install-agent.sh",
        );
        install_chunk.document_label = "Sample Target maintenance guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[install_chunk],
            )
            .is_none(),
            "an install-script command sequence must not be accepted solely because the heading names the requested action"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_install_script_from_update_host() {
        let mut install_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update:\n\
             sudo su sample-transfer https://updates.example.invalid/alpha/install-agent.sh -o /tmp/install-agent.sh\n\
             sample-prepare +x /tmp/install-agent.sh\n\
             /tmp/install-agent.sh",
        );
        install_chunk.document_label = "Sample Target maintenance guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[install_chunk],
            )
            .is_none(),
            "the update host name must not make an install script action-specific"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_fetched_install_script_with_format_mark() {
        let mut install_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update:\n\
             sudo su sample-transfer https://updates.example.invalid/alpha/install-agent.sh\u{200e} -o /tmp/install-agent.sh\u{200e}\n\
             /tmp/install-agent.sh\u{200e}\n\
             Activate the node after package installation.",
        );
        install_chunk.document_label = "Sample Target maintenance guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[install_chunk],
            )
            .is_none(),
            "format marks and a missing sample-prepare line must not hide a materialized setup artifact"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_setup_script_with_joined_privilege_token() {
        let mut install_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update:\n\
             sudo susample-transfer https://packages.example.invalid/alpha/install-agent.sh -o /tmp/install-agent.sh\n\
             sample-prepare +x /tmp/install-agent.sh\n\
             /tmp/install-agent.sh",
        );
        install_chunk.document_label = "Sample Target maintenance guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[install_chunk],
            )
            .is_none(),
            "a joined su+sample-transfer layout artifact must still be treated as an install-script sequence"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_setup_script_with_joined_preparation_path() {
        let mut install_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update:\n\
             sudo susample-transfer https://packages.example.invalid/alpha/install-agent.sh -o /tmp/install-agent.sh\n\
             sample-prepare +x /tmp/install-agent.sh/tmp/install-agent.sh",
        );
        install_chunk.document_label = "Sample Target maintenance guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to update Sample Target?",
                &configure_update_focus_ir("Sample Target"),
                &[install_chunk],
            )
            .is_none(),
            "a sample-prepare line with a joined /tmp script path is still setup-script evidence, not the requested update procedure"
        );
    }

    #[test]
    fn update_procedure_sequence_answer_uses_later_valid_block_after_action_reject() {
        let mut mixed_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target installation:\n\
             1. Install the Alpha subject artifact.\n\
             2. Install the agent. Read the section \"Sample Target update\".\n\
             sudo su\n\
             sample-transfer https://packages.example.invalid/alpha/install-agent.sh -o /tmp/install-agent.sh\n\
             sample-prepare +x /tmp/install-agent.sh\n\
             /tmp/install-agent.sh\n\
             \n\
             Sample Target update:\n\
             1. Stop Alpha subject workers.\n\
             2. Install Alpha subject artifact version 2.0.0.\n\
             3. Restart Alpha subject workers.",
        );
        mixed_chunk.document_label = "Sample Target operations guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[mixed_chunk],
        )
        .expect("later valid update block should be selected");

        assert!(answer.contains("Stop Alpha subject workers"));
        assert!(answer.contains("2.0.0"));
        assert!(!answer.contains("install-agent.sh"));
    }

    #[test]
    fn update_procedure_sequence_answer_does_not_use_label_action_for_wrong_command_block() {
        let mut mixed_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Install Sample Target:\n\
             sudo su\n\
             sample-transfer https://packages.example.invalid/alpha/install-node.sh -o /tmp/install-node.sh\n\
             sample-prepare +x /tmp/install-node.sh\n\
             /tmp/install-node.sh\n\
             \n\
             Update Sample Target:\n\
             sudo su\n\
             sample-transfer https://example.invalid/sample/update-main -o sample-update-token\n\
             sample-prepare +x sample-update-token\n\
             sample-update-token",
        );
        mixed_chunk.document_label = "Sample Target install and update".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[mixed_chunk],
        )
        .expect("block action must win over generic document label action");

        assert!(answer.contains("/sample/update"), "{answer}");
        assert!(answer.contains("sample-update-token"), "{answer}");
        assert!(!answer.contains("install-node.sh"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_action_named_command_over_config_prep() {
        let mut mixed_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Prepare Sample Target package versions:\n\
             1. Download the configuration file.\n\
             sudo su\n\
             sample-transfer https://packages.example.invalid/alpha/packages_version.conf -o /tmp/packages_version.conf\n\
             2. List installed packages with sample-list --all | grep alpha.\n\
             3. Edit /tmp/packages_version.conf.\n\
             \n\
             Update Sample Target:\n\
             sudo su\n\
             sample-transfer https://example.invalid/sample/update-main -o sample-update-token\n\
             sample-prepare +x sample-update-token\n\
             sample-update-token",
        );
        mixed_chunk.document_label = "Sample Target install and update".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[mixed_chunk],
        )
        .expect("action-named update command should outrank config preparation");

        assert!(answer.contains("/sample/update"), "{answer}");
        assert!(!answer.contains("packages_version.conf"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_later_dense_update_chunk_over_config_prep_chunk() {
        let mut prep_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Prepare Sample Target package versions:\n\
             The package-version workflow can be used for install/update operations.\n\
             1. Download the configuration file.\n\
             sudo su\n\
             sample-transfer https://packages.example.invalid/alpha/packages_version.conf -o /tmp/packages_version.conf\n\
             2. List installed packages with sample-list --all | grep alpha.\n\
             3. Edit /tmp/packages_version.conf.",
        );
        prep_chunk.document_label = "Sample Target install and update".to_string();
        prep_chunk.score = Some(20.0);

        let mut update_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Update Sample Target\n\
             - If all components are installed on one host, update Sample Target with: sudo su sample-transfer https://example.invalid/sample/update-main -o sample-update-token sample-prepare +x sample-update-token sample-update-token\n\
             - If components are installed on multiple hosts, update Sample Target with: sudo su sample-transfer https://example.invalid/sample/update-secondary -o sample-update-token sample-prepare +x sample-update-token sample-update-token\n\
             The scripts update required dependencies.",
        );
        update_chunk.document_label = "Sample Target install and update".to_string();
        update_chunk.score = Some(19.0);

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[prep_chunk, update_chunk],
        )
        .expect("later dense update commands should outrank package-version prep");

        assert!(answer.contains("/sample/update"), "{answer}");
        assert!(answer.contains("sample-prepare +x sample-update-token"), "{answer}");
        assert!(!answer.contains("packages_version.conf"), "{answer}");
        assert!(!answer.contains("https://packages. example"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_command_runbook_over_transition_outline() {
        let mut outline_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Centralized Alpha node update:\n\
             1. Update the management server to version 4.0.63 or later.\n\
             2. Update to version 9.8.7 or later.\n\
             3. Update to version 10.4.2 or later.\n\
             4. Do not skip steps or change their order.",
        );
        outline_chunk.document_label = "Alpha node transition guide".to_string();
        outline_chunk.score = Some(25.0);

        let mut command_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Alpha node update commands:\n\
             1. Configure the package source.\n\
             sudo su\n\
             sample-transfer https://example.invalid/sample/update-main -o sample-update-token\n\
             sample-prepare +x sample-update-token\n\
             sample-update-token",
        );
        command_chunk.document_label = "Alpha node transition guide".to_string();
        command_chunk.score = Some(19.0);

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha node?",
            &configure_update_focus_ir("Alpha node"),
            &[outline_chunk, command_chunk],
        )
        .expect("actionable command runbook should outrank a version-only transition outline");

        assert!(answer.contains("/sample/update-main"), "{answer}");
        assert!(answer.contains("sample-prepare +x sample-update-token"), "{answer}");
        assert!(!answer.contains("Do not skip steps"), "{answer}");
    }

    #[test]
    fn update_procedure_extract_caps_command_signal_scores() {
        let query_ir = configure_update_focus_ir("Alpha node");
        let focus_model = update_procedure_focus_model("how to update Alpha node?", &query_ir);
        let block = (0..32)
            .map(|index| UpdateProcedureLine {
                text: format!("{index}. sudo alpha-tool update --target node"),
                has_order_marker: true,
                has_version: false,
                has_command: true,
            })
            .collect::<Vec<_>>();

        let extract = update_procedure_extract_from_block(0, &block, &focus_model)
            .expect("command block should extract");

        assert_eq!(extract.command_count, UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP);
        assert_eq!(extract.action_command_score, UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP);
    }

    #[test]
    fn update_procedure_sequence_answer_projects_focus_aligned_tail_from_mixed_block() {
        let mut mixed_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Alpha control plane update from environment 1 to environment 2:\n\
             2. Disable third-party sources.\n\
             3. Refresh prerequisites: sample-runner --refresh sample-runner --apply\n\
             4. Update platform dependencies: sample-runner --migrate\n\
             5. Run platform update: sample-platform-update\n\
             8. Re-enable subject bundle sources and refresh: sample-runner --refresh\n\
             9. Upgrade subject bundles: sample-runner --apply\n\
             10. Reconfigure the API package: sudo sample-configure alpha-rest\n\
             11. Restart the API service: sudo service alpha-rest restart",
        );
        mixed_chunk.document_label = "Alpha control plane update runbook".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha control plane version?",
            &configure_update_focus_ir("Alpha control plane"),
            &[mixed_chunk],
        )
        .expect("focus-aligned projection should be selected");

        assert!(answer.contains("`sample-runner --apply`"), "{answer}");
        assert!(answer.contains("`sudo sample-configure alpha-rest`"), "{answer}");
        assert!(answer.contains("`sudo service alpha-rest restart`"), "{answer}");
        assert!(!answer.contains("sample-platform-update"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_dense_update_chunk_over_preparation_text() {
        let mut prep_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Preparation notes\n\
             To install or update with explicit package versions:\n\
             1. Download packages_version.conf. sudo su sample-transfer https://packages.example.invalid/alpha/packages_version.conf -o /tmp/packages_version.conf\n\
             2. List installed packages with sample-list --all | grep alpha.\n\
             3. Write versions to /tmp/packages_version.conf.",
        );
        prep_chunk.document_label = "Install and update Alpha node".to_string();
        prep_chunk.score = Some(20.0);

        let mut update_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Alpha node update\n\
             - For a single-node layout, run: sudo su sample-transfer https://packages.example.invalid/alpha/update.sh -o /tmp/sample-runner.sh sample-prepare +x /tmp/sample-runner.sh /tmp/sample-runner.sh\n\
             - For a split-node layout, run: sudo su sample-transfer https://packages.example.invalid/alpha/update_secondary.sh -o /tmp/sample-runner.sh sample-prepare +x /tmp/sample-runner.sh /tmp/sample-runner.sh\n\
             The scripts download the update and refresh required dependencies.",
        );
        update_chunk.document_label = "Install and update Alpha node".to_string();
        update_chunk.score = Some(19.0);

        let mut query_ir = configure_update_focus_ir("Alpha node");
        query_ir.retrieval_query =
            Some("how to update Alpha node? Alpha node update procedure".to_string());

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha node?",
            &query_ir,
            &[prep_chunk, update_chunk],
        )
        .expect("dense update commands should outrank package prep");

        assert!(answer.contains("update_secondary.sh"), "{answer}");
        assert!(answer.contains("sample-prepare +x /tmp/sample-runner.sh"), "{answer}");
        assert!(!answer.contains("packages_version.conf"), "{answer}");
        assert!(!answer.contains("https://packages. example"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_matches_latin_script_artifact_from_retrieval_query() {
        let mut update_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Support node update\n\
             - For a single-node layout, run: sudo su sample-transfer https://packages.example.invalid/alpha/update.sh -o /tmp/sample-runner.sh sample-prepare +x /tmp/sample-runner.sh /tmp/sample-runner.sh\n\
             - For a split-node layout, run: sudo su sample-transfer https://packages.example.invalid/alpha/update_secondary.sh -o /tmp/sample-runner.sh sample-prepare +x /tmp/sample-runner.sh /tmp/sample-runner.sh",
        );
        update_chunk.document_label = "Install and update".to_string();

        let mut query_ir = configure_update_focus_ir("support node");
        query_ir.retrieval_query = Some("how to update support node?".to_string());

        let answer = build_update_procedure_sequence_answer(
            "how to update support node?",
            &query_ir,
            &[update_chunk],
        )
        .expect("latin-script command artifact family should match the retrieval action");

        assert!(answer.contains("update.sh"), "{answer}");
        assert!(answer.contains("update_secondary.sh"), "{answer}");
        assert!(answer.contains("sample-prepare +x /tmp/sample-runner.sh"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_keeps_heading_only_action_without_setup_signature() {
        let mut update_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update:\n\
             1. Stop Alpha subject workers.\n\
             2. Copy alpha-node-2.0.0 into /opt/alpha/bin.\n\
             3. Start Alpha subject workers.",
        );
        update_chunk.document_label = "Sample Target runbook".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[update_chunk],
        )
        .expect("heading-only update action should remain valid without setup-script signature");

        assert!(answer.contains("Stop Alpha subject workers"));
        assert!(answer.contains("alpha-node-2.0.0"));
        assert!(answer.contains("Start Alpha subject workers"));
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_title_action_with_structural_body() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Maintenance checklist:\n\
             1. Stop Alpha service workers.\n\
             2. Install Alpha service package version 2.0.0.\n\
             3. Start Alpha service workers.",
        );
        product_chunk.document_label = "Alpha service refresh runbook".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to refresh Alpha service?",
            &configure_update_focus_ir("Alpha service"),
            &[product_chunk],
        )
        .expect("title-aligned action should validate the structural runbook body");

        assert!(answer.contains("Alpha service refresh runbook"));
        assert!(answer.contains("Stop Alpha service workers"));
        assert!(answer.contains("2.0.0"));
        assert!(answer.contains("Start Alpha service workers"));
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_command_only_action_match() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target maintenance:\n\
             sudo su\n\
             sample-transfer https://example.invalid/sample/update-main -o sample-update-token\n\
             sample-prepare +x sample-update-token\n\
             sample-update-token",
        );
        product_chunk.document_label = "Sample Target maintenance runbook".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[product_chunk],
        )
        .expect("command-only update action should still be accepted");

        assert!(answer.contains("sample-update-token"));
        assert!(answer.contains("sample-prepare +x"));
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_full_source_over_truncated_excerpt_commands() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update:\n\
             sudo su\n\
             sample-transfer https://updates. example",
        );
        product_chunk.document_label = "Sample Target update".to_string();
        product_chunk.source_text = concat!(
            "Sample Target update:\n",
            "- For the single-node layout run: sudo su sample-transfer ",
            "https://updates.example.invalid/alpha/update-node.sh -o sample-update-token ",
            "sample-prepare +x sample-update-token sample-update-token"
        )
        .to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[product_chunk],
        )
        .expect("update procedure answer should use full command source");

        assert!(answer.contains("updates.example.invalid/alpha/update-node.sh"), "{answer}");
        assert!(answer.contains("/tmp/sample-runner.sh") || answer.contains("sample-update-token"));
        assert!(!answer.contains("https://updates. example"), "{answer}");
    }

    #[test]
    fn dense_procedure_line_splits_privilege_prefix_from_fetch_command() {
        assert_eq!(
            split_dense_procedure_line(
                "Sample Target lifecycle: sudo su sample-transfer https://example.invalid/sample/update-main -o sample-update-token"
            ),
            vec![
                "Sample Target lifecycle:",
                "sudo su",
                "sample-transfer https://example.invalid/sample/update-main -o sample-update-token"
            ]
        );
        assert_eq!(
            split_dense_procedure_line(
                "sudo susample-transfer https://example.invalid/sample/update-main -o sample-update-token"
            ),
            vec![
                "sudo su",
                "sample-transfer https://example.invalid/sample/update-main -o sample-update-token"
            ]
        );
    }

    #[test]
    fn dense_procedure_line_does_not_split_url_or_parent_relative_path() {
        assert_eq!(
            split_dense_procedure_line(
                "sample-transfer https://packages.example.invalidsample-update-token -o ../update-node.sh"
            ),
            vec![
                "sample-transfer https://packages.example.invalidsample-update-token -o ../update-node.sh"
            ]
        );
    }

    #[test]
    fn dense_procedure_line_splits_local_script_after_file_mode_command() {
        assert_eq!(
            split_dense_procedure_line("sample-prepare +x sample-update-token sample-update-token"),
            vec!["sample-prepare +x sample-update-token", "sample-update-token"]
        );
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_action_named_script() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target lifecycle:\n\
             sudo su\n\
             sample-transfer https://packages.example.invalid/alpha/refresh.sh -o /tmp/refresh.sh\n\
             sample-prepare +x /tmp/refresh.sh\n\
             /tmp/refresh.sh",
        );
        product_chunk.document_label = "Sample Target lifecycle guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to refresh Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[product_chunk],
        )
        .expect("action-named script should be accepted across question languages");

        assert!(answer.contains("refresh.sh"), "{answer}");
        assert!(answer.contains("sample-prepare +x"), "{answer}");
        assert!(answer.contains("`/tmp/refresh.sh`"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_update_script_with_joined_privilege_prefix() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target lifecycle:\n\
             sudo su sample-transfer https://example.invalid/sample/update-main -o sample-update-token\n\
             sample-prepare +x sample-update-token\n\
             sample-update-token",
        );
        product_chunk.document_label = "Sample Target lifecycle guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[product_chunk],
        )
        .expect("update-named script with privilege prefix should be accepted");

        assert!(answer.contains("`sudo su`"), "{answer}");
        assert!(
            answer.contains(
                "`sample-transfer https://example.invalid/sample/update-main -o sample-update-token`"
            ),
            "{answer}"
        );
        assert!(answer.contains("/sample/update-main"), "{answer}");
        assert!(!answer.contains("susample-transfer"), "{answer}");
        assert!(answer.contains("`sample-prepare +x sample-update-token`"), "{answer}");
        assert!(answer.contains("`sample-update-token`"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_update_script_with_joined_privilege_command_token()
    {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target lifecycle:\n\
             sudo susample-transfer https://example.invalid/sample/update-main -o sample-update-token\n\
             sample-prepare +x sample-update-token\n\
             sample-update-token",
        );
        product_chunk.document_label = "Sample Target lifecycle guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[product_chunk],
        )
        .expect("joined su+fetch token should be repaired for update-named scripts");

        assert!(answer.contains("`sudo su`"), "{answer}");
        assert!(
            answer.contains(
                "`sample-transfer https://example.invalid/sample/update-main -o sample-update-token`"
            ),
            "{answer}"
        );
        assert!(!answer.contains("susample-transfer"), "{answer}");
        assert!(answer.contains("`sample-prepare +x sample-update-token`"), "{answer}");
        assert!(answer.contains("`sample-update-token`"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_unicode_action_prefix_match() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Δέλτα Node δοκιμηενεργειας:\n\
             1. Stop Delta workers.\n\
             2. Install Delta package version 2.0.0.\n\
             3. Restart Delta workers.",
        );
        product_chunk.document_label = "Δέλτα Node runbook".to_string();

        let answer = build_update_procedure_sequence_answer(
            "δοκιμηενεργεια Delta Node?",
            &configure_update_focus_ir("Delta Node"),
            &[product_chunk],
        )
        .expect("unicode action prefix should match across inflected tokens");

        assert!(answer.contains("2.0.0"));
    }

    #[test]
    fn update_procedure_sequence_answer_matches_subject_acronym_evidence() {
        let mut distractor_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Host migration:\n\
             1. Update the environment from baseline 1 to baseline 2.\n\
             2. Run sample-platform-release.",
        );
        distractor_chunk.document_label = "Environment host migration".to_string();
        let mut product_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "AS update:\n\
             1. Stop AS workers.\n\
             2. Install AS package version 2.0.0.\n\
             3. Restart AS workers.",
        );
        product_chunk.document_label = "AS update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha Service?",
            &configure_update_focus_ir("Alpha Service"),
            &[distractor_chunk, product_chunk],
        )
        .expect("acronym-focused update procedure answer");

        assert!(answer.contains("AS update guide"));
        assert!(answer.contains("Stop AS workers"));
        assert!(answer.contains("2.0.0"));
        assert!(!answer.contains("sample-platform-release"));
    }

    #[test]
    fn update_procedure_sequence_answer_splits_command_from_following_prose() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Alpha control plane update:\n\
             1. Upgrade Alpha control plane to version 2.0: sudo alpha-update-runner If AlphaDB version differs: touch /etc/alpha.pref Add file content: Package: alpha* Pin: version <Version> Pin-Priority: 1001 Restart database: service alpha restart",
        );
        product_chunk.document_label = "Alpha control plane update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha control plane?",
            &configure_update_focus_ir("Alpha control plane"),
            &[product_chunk],
        )
        .expect("dense command/prose procedure answer");

        assert!(answer.contains("`sudo alpha-update-runner`"));
        assert!(answer.contains("`touch /etc/alpha.pref`"));
        assert!(answer.contains("`service alpha restart`"));
        assert!(!answer.contains("`sudo alpha-update-runner If"));
    }

    #[test]
    fn update_procedure_sequence_answer_splits_service_after_structural_delimiter() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Alpha node update:\n\
             1. Apply Alpha node package version 2.0.0.\n\
             2. Restart daemon: service alpha-node restart.",
        );
        product_chunk.document_label = "Alpha node update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha node?",
            &configure_update_focus_ir("Alpha node"),
            &[product_chunk],
        )
        .expect("service command after structural delimiter should be extracted");

        assert!(answer.contains("`service alpha-node restart`"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_keeps_directory_change_commands() {
        let mut product_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target versioned update:\n\
             1. Remove /etc/pkg/sources.list.\n\
             2. Install package alpha-upgrade command: pkgctl install alpha-upgrade\n\
             3. Run update script from /opt/alpha/bin: cd /opt/alpha/bin ./upgrade_alpha.sh\n\
             4. Wait until the script finishes.",
        );
        product_chunk.document_label = "Alpha subject versioned update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[product_chunk],
        )
        .expect("versioned update procedure answer with cd command");

        assert!(answer.contains("alpha-upgrade"));
        assert!(answer.contains("`cd /opt/alpha/bin`"));
        assert!(answer.contains("`./upgrade_alpha.sh`"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_richer_runbook_over_short_package_fragment() {
        let mut short_fragment = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target package refresh:\n\
             1. Update package cache with sudo pkgctl refresh.\n\
             2. Upgrade installed packages with sudo pkgctl upgrade.",
        );
        short_fragment.document_label = "Sample Target quick maintenance note".to_string();
        let mut full_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Target versioned update procedure:\n\
             1. Create a backup of the Alpha database.\n\
             2. Disable third-party package repositories in /etc/pkg/sources.list.d.\n\
             3. Run sudo pkgctl refresh.\n\
             4. Run sudo pkgctl upgrade.\n\
             5. Run sudo pkgctl full-upgrade.\n\
             6. Install the release manager with sudo pkgctl install alpha-release-manager.\n\
             7. Run sudo alpha-release-runner.\n\
             8. Re-enable the package repositories.\n\
             9. Run sudo sample-configure alpha-rest.\n\
             10. Restart the Alpha service with sudo service alpha-server restart.",
        );
        full_runbook.document_label = "Sample Target versioned update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &configure_update_focus_ir("Sample Target"),
            &[short_fragment, full_runbook],
        )
        .expect("full update procedure answer");

        assert!(answer.contains("Create a backup"), "{answer}");
        assert!(answer.contains("`sudo alpha-release-runner`"), "{answer}");
        assert!(answer.contains("`sudo sample-configure alpha-rest`"), "{answer}");
        assert!(answer.contains("`sudo service alpha-server restart`"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_command_runbook_over_version_note() {
        let mut version_note = evidence_chunk(
            1,
            Some("paragraph"),
            "Alpha orchestrator integration change:\n\
             1. Disable the legacy plugin in /opt/alpha/max.ini.\n\
             2. Update Alpha orchestrator to version 9.8.7.\n\
             3. Update Alpha subject artifact to version 8.7.6.",
        );
        version_note.document_label = "Alpha orchestrator integration version note".to_string();
        let mut command_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Alpha orchestrator update runbook:\n\
             1. Install the update manager with sudo pkgctl install alpha-update-manager.\n\
             2. Run sudo alpha-update-runner.\n\
             3. Reconfigure Alpha REST with sudo sample-configure alpha-rest.\n\
             4. Restart Alpha service with sudo service alpha-server restart.",
        );
        command_runbook.document_label = "Alpha orchestrator update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha orchestrator version?",
            &configure_update_focus_ir("Alpha orchestrator"),
            &[version_note, command_runbook],
        )
        .expect("command-bearing runbook should win over a prose-only version note");

        assert!(answer.contains("Alpha orchestrator update guide"), "{answer}");
        assert!(answer.contains("`sudo alpha-update-runner`"), "{answer}");
        assert!(answer.contains("`sudo sample-configure alpha-rest`"), "{answer}");
        assert!(!answer.contains("legacy plugin"), "{answer}");
        assert!(!answer.contains("8.7.6"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_rich_runbook_over_short_exact_note() {
        let mut short_exact_note = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target quick package refresh:\n\
             1. Refresh bundle metadata:\n\
             sample-runner --refresh.",
        );
        short_exact_note.document_label = "Sample Target quick update note".to_string();
        let mut rich_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Target update runbook:\n\
             1. Create a database backup.\n\
             2. Disable third-party package repositories.\n\
             3. Refresh subject references:\n\
             sample-runner --refresh\n\
             4. Update installed subject bundles:\n\
             sample-runner --apply\n\
             5. Reconfigure the REST package:\n\
             sudo sample-configure alpha-rest\n\
             6. Restart the REST service:\n\
             sudo service alpha-rest restart.",
        );
        rich_runbook.document_label = "Alpha platform update guide".to_string();
        let mut query_ir = configure_update_focus_ir("Sample Target");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target version?",
            &query_ir,
            &[short_exact_note, rich_runbook],
        )
        .expect("rich package-maintenance runbook should win");

        assert!(answer.contains("Alpha platform update guide"), "{answer}");
        assert!(answer.contains("Create a database backup"), "{answer}");
        assert!(answer.contains("`sample-runner --apply`"), "{answer}");
        assert!(answer.contains("`sudo sample-configure alpha-rest`"), "{answer}");
        assert!(answer.contains("`sudo service alpha-rest restart`"), "{answer}");
        assert!(!answer.contains("quick update note"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_acronym_runbook_script_over_neighbor_product_note()
    {
        let mut adjacent_product_note = evidence_chunk(
            1,
            Some("paragraph"),
            "Alpha subject endpoint manual update:\n\
             1. Install the endpoint artifact with pkgctl install alpha-pos.\n\
             2. Run sample-control transition for the endpoint environment.",
        );
        adjacent_product_note.document_label = "Manual update".to_string();

        let mut server_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Update AS\n\
             If all components are installed on one AS node, update the Alpha server with commands:\n\
             sudo su sample-transfer https://updates.example/static/as_env/update.sh -o /tmp/sample-runner.sh\n\
             sample-prepare +x /tmp/sample-runner.sh\n\
             /tmp/sample-runner.sh\n\
             The scripts update required dependencies.",
        );
        server_runbook.document_label = "Install and update".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha server?",
            &configure_update_focus_ir("Alpha server"),
            &[adjacent_product_note, server_runbook],
        )
        .expect("server runbook script answer");

        assert!(answer.contains("Install and update"), "{answer}");
        assert!(
            answer.contains(
                "`sample-transfer https://updates.example/static/as_env/update.sh -o /tmp/sample-runner.sh`"
            ),
            "{answer}"
        );
        assert!(answer.contains("`sample-prepare +x /tmp/sample-runner.sh`"), "{answer}");
        assert!(answer.contains("`/tmp/sample-runner.sh`"), "{answer}");
        assert!(!answer.contains("sample-control transition"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_target_transition_over_adjacent_transition() {
        let mut platform_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Environment Variant platform migration for hosted Sample Target nodes:\n\
             1. Install the release manager with sudo pkgctl install platform-release-helper.\n\
             2. Run sudo platform-release-upgrade.\n\
             3. Upgrade generic packages with sudo pkgctl upgrade.\n\
             4. Restart the backing service with sudo service environment-beta restart.",
        );
        platform_chunk.document_label = "Environment Variant platform migration guide".to_string();
        let mut product_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Alpha product transition runbook:\n\
             1. Update AlphaControlCenter to version 9.8.7.6 or higher.\n\
             2. Update subject licenses to version 5.\n\
             3. Update subject artifact to version 9.8.7-3 or higher.\n\
             4. Update subject artifact to version 10.4.2 or higher.\n\
             Manual package update:\n\
             1. Run pkgctl refresh.\n\
             2. Install the new product package with pkgctl install alpha-pos.",
        );
        product_chunk.document_label = "Alpha product transition guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target version?",
            &configure_update_focus_ir("Sample Target"),
            &[platform_chunk, product_chunk],
        )
        .expect("target transition procedure answer");

        assert!(answer.contains("Alpha product transition guide"), "{answer}");
        assert!(answer.contains("AlphaControlCenter"), "{answer}");
        assert!(answer.contains("10.4.2"), "{answer}");
        assert!(answer.contains("pkgctl install alpha-pos"), "{answer}");
        assert!(!answer.contains("platform-release-upgrade"), "{answer}");
        assert!(!answer.contains("Environment Variant platform"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_focus_aligned_tail_inside_mixed_transition_doc() {
        let mut mixed_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Alpha orchestrator update from platform 18.04 to 22.04:\n\
             \n\
             1. Update the host baseline to 20.04:\n\
             sudo sample-platform-update\n\
             \n\
             2. Update base prerequisites:\n\
             sample-runner --migrate\n\
             \n\
             3. Install the transition helper:\n\
             sample-runner --install helper-unit\n\
             \n\
             4. Refresh subject references:\n\
             sample-runner --refresh\n\
             \n\
             5. Update installed subject bundles:\n\
             sample-runner --apply\n\
             \n\
             6. Reconfigure the product REST package:\n\
             sudo sample-configure alpha-rest\n\
             \n\
             7. Restart the product service:\n\
             sudo service alpha-rest restart",
        );
        mixed_chunk.document_label = "Alpha orchestrator update guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Alpha orchestrator version?",
            &configure_update_focus_ir("Alpha orchestrator"),
            &[mixed_chunk],
        )
        .expect("product maintenance commands should be extracted from a mixed transition guide");

        assert!(answer.contains("Alpha orchestrator update guide"), "{answer}");
        assert!(answer.contains("`sample-runner --apply`"), "{answer}");
        assert!(answer.contains("`sudo sample-configure alpha-rest`"), "{answer}");
        assert!(answer.contains("`sudo service alpha-rest restart`"), "{answer}");
        assert!(!answer.contains("sample-platform-update"), "{answer}");
        assert!(!answer.contains("host baseline"), "{answer}");
        assert!(!answer.contains("base prerequisites"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_exact_target_label_over_neighbor_migration() {
        let mut migration_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Environment transition transition for hosted Sample Subject nodes:\n\
             1. Update the base environment with sample-platform-update.\n\
             2. Upgrade base prerequisites with sample-runner --migrate.\n\
             3. Install the transition helper with sample-runner --install helper-unit.\n\
             4. Restart the environment service with sudo service environment-beta restart.",
        );
        migration_chunk.document_label = "Environment transition migration guide".to_string();
        migration_chunk.score = Some(200.0);
        let mut product_chunk = evidence_chunk(
            2,
            Some("paragraph"),
            "Update Sample Subject 9.0 package:\n\
             1. Refresh subject references with sample-runner --refresh.\n\
             2. Upgrade installed subject bundles with sample-runner --apply.\n\
             3. Reconfigure the product REST package with sudo sample-configure alpha-rest.\n\
             4. Restart the product service with sudo service alpha-rest restart.",
        );
        product_chunk.document_label = "Sample Subject 9.0 update guide".to_string();
        product_chunk.score = Some(20.0);
        let mut query_ir = configure_update_focus_ir("Sample Subject 9.0");
        query_ir.target_types =
            vec!["artifact".to_string(), "procedure".to_string(), "version".to_string()];

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Subject 9.0 version?",
            &query_ir,
            &[migration_chunk, product_chunk],
        )
        .expect("exact target label should select product maintenance procedure");

        assert!(answer.contains("Sample Subject 9.0 update guide"), "{answer}");
        assert!(answer.contains("`sample-runner --refresh`"), "{answer}");
        assert!(answer.contains("`sample-runner --apply`"), "{answer}");
        assert!(answer.contains("`sudo sample-configure alpha-rest`"), "{answer}");
        assert!(answer.contains("`sudo service alpha-rest restart`"), "{answer}");
        assert!(!answer.contains("sample-platform-update"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_exact_target_source_over_generic_manual() {
        let mut generic_manual = evidence_chunk(
            1,
            Some("paragraph"),
            "Manual update runbook:\n\
             1. Run sudo generic-update --prepare.\n\
             2. Run sudo generic-update --apply.\n\
             3. Run sudo generic-update --restart.\n\
             4. Run sudo generic-update --validate.",
        );
        generic_manual.document_label = "Neighboring manual update runbook".to_string();
        generic_manual.score = Some(200.0);
        let mut target_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Target update procedure:\n\
             1. Open the release workspace.\n\
             2. Replace the control component package.\n\
             3. Validate the reported version.",
        );
        target_runbook.document_label = "Generic update guide".to_string();
        target_runbook.score = Some(20.0);

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target version?",
            &configure_update_focus_ir("Sample Target"),
            &[generic_manual, target_runbook],
        )
        .expect("exact target source should select target runbook");

        assert!(answer.contains("Generic update guide"), "{answer}");
        assert!(answer.contains("Replace the control component package"), "{answer}");
        assert!(answer.contains("Validate the reported version"), "{answer}");
        assert!(!answer.contains("generic-update"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_exact_target_label_over_generic_upgrade_script() {
        let mut generic_upgrade = evidence_chunk(
            1,
            Some("paragraph"),
            "Target Server hosted node major upgrade:\n\
             1. Run sudo generic-major-upgrade --prepare.\n\
             2. Run sudo generic-major-upgrade --migrate.\n\
             3. Run sudo generic-major-upgrade --restart.\n\
             4. Run sudo generic-major-upgrade --verify.",
        );
        generic_upgrade.document_label = "Target Server major upgrade runbook".to_string();
        generic_upgrade.score = Some(200.0);
        let mut target_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Target Server update procedure:\n\
             1. Stop dependent jobs.\n\
             2. Replace the Target Server package.\n\
             3. Validate the reported service version.",
        );
        target_runbook.document_label = "Target Server update guide".to_string();
        target_runbook.score = Some(20.0);

        let answer = build_update_procedure_sequence_answer(
            "how to update Target Server?",
            &configure_update_focus_ir("Target Server"),
            &[generic_upgrade, target_runbook],
        )
        .expect("exact target label should select target runbook");

        assert!(answer.contains("Target Server update guide"), "{answer}");
        assert!(answer.contains("Replace the Target Server package"), "{answer}");
        assert!(!answer.contains("generic-major-upgrade"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_prefers_focused_target_title_over_command_heavy_neighbor() {
        let mut generic_script = evidence_chunk(
            1,
            Some("paragraph"),
            "Target Server update script:\n\
             1. Run sudo generic-script --prepare.\n\
             2. Run sudo generic-script --migrate.\n\
             3. Run sudo generic-script --restart.\n\
             4. Run sudo generic-script --verify.",
        );
        generic_script.document_label = "Neighboring lifecycle runbook".to_string();
        generic_script.score = Some(200.0);
        let mut target_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Target Server update procedure:\n\
             1. Stop dependent jobs.\n\
             2. Replace the Target Server package.\n\
             3. Validate the reported service version.",
        );
        target_runbook.document_label = "Target Server update guide".to_string();
        target_runbook.score = Some(20.0);

        let answer = build_update_procedure_sequence_answer(
            "how to update Target Server?",
            &configure_update_focus_ir("Target Server"),
            &[generic_script, target_runbook],
        )
        .expect("focused target title should select target runbook");

        assert!(answer.contains("Target Server update guide"), "{answer}");
        assert!(answer.contains("Replace the Target Server package"), "{answer}");
        assert!(!answer.contains("generic-script"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_matches_inflected_target_identity_over_generic_runbook() {
        let mut generic_upgrade = evidence_chunk(
            1,
            Some("paragraph"),
            "Major lifecycle upgrade:\n\
             1. Run generic-runbook --prepare.\n\
             2. Run generic-runbook --migrate.\n\
             3. Run generic-runbook --restart.\n\
             4. Run generic-runbook --verify.",
        );
        generic_upgrade.document_label = "Major lifecycle upgrade runbook".to_string();
        generic_upgrade.score = Some(200.0);
        let mut target_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Processes update procedure:\n\
             1. Open the release workspace.\n\
             2. Replace the Sample Processes bundle.\n\
             3. Validate the reported Sample Processes version.",
        );
        target_runbook.document_label = "Sample Processes update guide".to_string();
        target_runbook.score = Some(20.0);

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Process version?",
            &configure_update_focus_ir("Sample Process"),
            &[generic_upgrade, target_runbook],
        )
        .expect("inflected target identity should select target runbook");

        assert!(answer.contains("Sample Processes update guide"), "{answer}");
        assert!(answer.contains("Replace the Sample Processes bundle"), "{answer}");
        assert!(!answer.contains("generic-runbook"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_uses_raw_question_identity_without_ir_subject() {
        let mut generic_upgrade = evidence_chunk(
            1,
            Some("paragraph"),
            "Manual update runbook:\n\
             1. Run generic-runbook --prepare.\n\
             2. Run generic-runbook --migrate.\n\
             3. Run generic-runbook --restart.\n\
             4. Run generic-runbook --verify.",
        );
        generic_upgrade.document_label = "Manual update checklist".to_string();
        generic_upgrade.score = Some(200.0);
        let mut target_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Sample Process update procedure:\n\
             1. Open the release workspace.\n\
             2. Replace the Sample Process bundle.\n\
             3. Validate the reported Sample Process version.",
        );
        target_runbook.document_label = "Sample Process update guide".to_string();
        target_runbook.score = Some(20.0);
        let mut query_ir = configure_update_focus_ir("unfocused");
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = Some("how to update Sample Process version?".to_string());

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Process version?",
            &query_ir,
            &[generic_upgrade, target_runbook],
        )
        .expect("raw question identity should select the target runbook");

        assert!(answer.contains("Sample Process update guide"), "{answer}");
        assert!(answer.contains("Replace the Sample Process bundle"), "{answer}");
        assert!(!answer.contains("generic-runbook"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_empty_ir_with_raw_command_runbook() {
        let mut generic_upgrade = evidence_chunk(
            1,
            Some("paragraph"),
            "Manual update runbook:\n\
             1. Run generic-runbook --prepare.\n\
             2. Run generic-runbook --migrate.\n\
             3. Run generic-runbook --restart.",
        );
        generic_upgrade.document_label = "Manual update checklist".to_string();
        generic_upgrade.score = Some(200.0);
        let mut target_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "support node update procedure:\n\
             1. Fetch the support node update bundle.\n\
             2. Run /tmp/support-node-update.sh.\n\
             3. Restart the support node service.",
        );
        target_runbook.document_label = "Maintenance instructions".to_string();
        target_runbook.score = Some(20.0);
        let mut query_ir = configure_how_focus_ir("ignored");
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.retrieval_query = None;
        query_ir.confidence = 0.2;

        let answer = build_update_procedure_sequence_answer(
            "how to update support node?",
            &query_ir,
            &[generic_upgrade, target_runbook],
        )
        .expect("empty IR should still allow a structurally grounded raw runbook answer");

        assert!(answer.contains("Maintenance instructions"), "{answer}");
        assert!(answer.contains("support-node-update.sh"), "{answer}");
        assert!(answer.contains("Restart the support node service"), "{answer}");
        assert!(!answer.contains("generic-runbook"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_keeps_action_bound_setup_signature_script() {
        let mut generic_upgrade = evidence_chunk(
            1,
            Some("paragraph"),
            "Manual update runbook:\n\
             1. Run generic-runbook --prepare.\n\
             2. Run generic-runbook --migrate.\n\
             3. Run generic-runbook --restart.\n\
             4. Run generic-runbook --verify.",
        );
        generic_upgrade.document_label = "Manual update checklist".to_string();
        generic_upgrade.score = Some(200.0);
        let mut target_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Install and update Sample Process agent\n\
             1. Download the maintenance artifact.\n\
             sample-transfer https://example.invalid/sample/install_agent.bin -o /tmp/install_agent.bin\n\
             2. sample-prepare +x /tmp/install_agent.bin\n\
             3. /tmp/install_agent.bin\n\
             4. Restart Sample Process workers.",
        );
        target_runbook.document_label = "Sample Process install and update".to_string();
        target_runbook.score = Some(20.0);

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Process?",
            &configure_update_focus_ir("Sample Process"),
            &[generic_upgrade, target_runbook],
        )
        .expect("action-bound setup signature should remain eligible");

        assert!(answer.contains("Sample Process install and update"), "{answer}");
        assert!(answer.contains("install_agent.bin"), "{answer}");
        assert!(!answer.contains("generic-runbook"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_uses_literal_component_identity() {
        let mut generic_manual = evidence_chunk(
            1,
            Some("paragraph"),
            "Target Server manual package update:\n\
             1. Run sudo generic-package --install.\n\
             2. Run sudo generic-package --configure.\n\
             3. Run sudo generic-package --restart.\n\
             4. Run sudo generic-package --verify.",
        );
        generic_manual.document_label = "Manual package install guide".to_string();
        generic_manual.score = Some(200.0);
        let mut component_runbook = evidence_chunk(
            2,
            Some("paragraph"),
            "Control Component version update:\n\
             1. Open the component release panel.\n\
             2. Replace Control Component with the requested version.\n\
             3. Validate the Control Component version report.",
        );
        component_runbook.document_label = "Control Component version guide".to_string();
        component_runbook.score = Some(20.0);
        let mut query_ir = configure_update_focus_ir("Target Server");
        query_ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "Control Component".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Other,
        }];

        let answer = build_update_procedure_sequence_answer(
            "how to update Control Component version?",
            &query_ir,
            &[generic_manual, component_runbook],
        )
        .expect("literal component identity should select component runbook");

        assert!(answer.contains("Control Component version guide"), "{answer}");
        assert!(answer.contains("Replace Control Component"), "{answer}");
        assert!(!answer.contains("generic-package"), "{answer}");
    }

    #[test]
    fn update_procedure_focus_model_ignores_scoped_previous_question_terms() {
        let query_ir = configure_update_focus_ir("Alternate Target");

        let bare =
            update_procedure_focus_model("how to update Alternate Target version?", &query_ir);
        let scoped = update_procedure_focus_model(
            "scope: how to update Sample Subject\nquestion: how to update Alternate Target version?",
            &query_ir,
        );

        assert_eq!(scoped.query_terms, bare.query_terms);
        assert_eq!(scoped.procedure_terms, bare.procedure_terms);
        assert_eq!(scoped.subject_terms, bare.subject_terms);
        assert!(!scoped.query_terms.contains("alpha"));
        assert!(!scoped.query_terms.contains("suite"));
    }

    #[test]
    fn update_procedure_sequence_answer_accepts_procedure_document_ir() {
        let mut query_ir = configure_how_focus_ir("Sample Target");
        query_ir.target_types = vec!["procedure".to_string(), "document".to_string()];

        let mut runbook_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target versioned update procedure:\n\
             1. Back up the Alpha database.\n\
             2. Run sample-runner --refresh.\n\
             3. Install the upgrade package with sample-install alpha-upgrade.\n\
             4. Run sudo service alpha-server restart.",
        );
        runbook_chunk.document_label = "Sample Target update runbook".to_string();

        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target?",
            &query_ir,
            &[runbook_chunk],
        )
        .expect("procedure/document IR should still allow grounded runbook extraction");

        assert!(answer.contains("Sample Target update runbook"), "{answer}");
        assert!(answer.contains("`sample-runner --refresh`"), "{answer}");
        assert!(answer.contains("`sample-install alpha-upgrade`"), "{answer}");
        assert!(answer.contains("`sudo service alpha-server restart`"), "{answer}");
    }

    #[test]
    fn update_procedure_sequence_answer_rejects_generic_concept_runbook_ir() {
        let mut query_ir = configure_how_focus_ir("Sample Target");
        query_ir.target_types =
            vec!["procedure".to_string(), "document".to_string(), "concept".to_string()];

        let mut runbook_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target versioned update procedure:\n\
             1. Back up the Alpha database.\n\
             2. Run sample-runner --refresh.\n\
             3. Install the upgrade package with sample-install alpha-upgrade.\n\
             4. Run sudo service alpha-server restart.",
        );
        runbook_chunk.document_label = "Sample Target update runbook".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to configure Sample Target?",
                &query_ir,
                &[runbook_chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn update_procedure_sequence_answer_skips_setup_configuration_ir() {
        let mut query_ir = configure_how_focus_ir("Environment Variant setup");
        query_ir.target_types =
            vec!["procedure".to_string(), "configuration_file".to_string(), "package".to_string()];

        let mut setup_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Environment Variant configuration:\n\
             sample-install beta-connector\n\
             sample-configure beta-connector\n\
             Settings are defined in /opt/beta/connector.conf.",
        );
        setup_chunk.document_label = "Environment Variant setup guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "how to configure Environment Variant?",
                &query_ir,
                &[setup_chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_lists_generic_variants() {
        let mut alpha = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\nsample-runner --install sample-unit\n\
             sample-configure alpha-subject\n\
             Settings are defined in /opt/subject/alpha/alpha.ini in section [AlphaQrp].\n\
             url = http://localhost\nprimaryKey = \"\"",
        );
        alpha.document_label = "Subject Alpha setup".to_string();
        let mut beta = evidence_chunk(
            2,
            Some("paragraph"),
            "Module configuration\nsample-install beta-subject\n\
             sample-configure beta-subject\n\
             Settings are defined in /opt/subject/beta/beta.conf in section [BetaQrp].\n\
             timeout = 60",
        );
        beta.document_label = "Subject Beta setup".to_string();
        let mut unrelated = evidence_chunk(
            3,
            Some("paragraph"),
            "Integrator manual: create one workflow method and set a token in the UI.",
        );
        unrelated.document_label = "Generic integrator manual".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "how to configure Subject?",
            &configure_how_focus_ir("Subject"),
            &[unrelated, alpha, beta],
        )
        .expect("setup configuration answer");

        assert!(answer.contains("**Setup variants:**"));
        assert!(answer.contains("Subject Alpha setup"));
        assert!(answer.contains("alpha-subject"));
        assert!(answer.contains("/opt/subject/alpha/alpha.ini"));
        assert!(answer.contains("AlphaQrp"));
        assert!(answer.contains("Subject Beta setup"));
        assert!(answer.contains("beta-subject"));
        assert!(answer.contains("/opt/subject/beta/beta.conf"));
        assert!(answer.contains("BetaQrp"));
        assert!(!answer.contains("token in the UI"));
    }

    #[test]
    fn setup_configuration_anchor_answer_accepts_procedure_artifact_ir_with_config_evidence() {
        let mut query_ir = configure_how_focus_ir("Sample Connector");
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];

        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Connector configuration\n\
             sample-configure sample-connector\n\
             Settings are stored in /etc/sample/connector.conf in section [Main].\n\
             sampleMerchantId = identifier issued by the provider.",
        );
        chunk.document_label = "Sample Connector setup guide".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "how to configure Sample Connector?",
            &query_ir,
            &[chunk],
        )
        .expect("setup configuration answer");

        assert!(answer.contains("Sample Connector setup guide"));
        assert!(answer.contains("sample-configure"));
        assert!(answer.contains("/etc/sample/connector.conf"));
        assert!(answer.contains("Main"));
        assert!(answer.contains("sampleMerchantId"));
    }

    #[test]
    fn setup_configuration_anchor_answer_accepts_subject_entity_config_evidence() {
        let mut query_ir = configure_how_focus_ir("Sample Connector");
        query_ir.document_focus = None;
        query_ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Connector".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];

        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Connector settings are stored in /etc/sample/connector.conf in section [Main].",
        );
        anchor.document_id = document_id;
        anchor.revision_id = revision_id;
        anchor.document_label = "Sample Connector setup guide".to_string();
        let mut parameters = evidence_chunk(
            2,
            Some("code_block"),
            "[Main]\n;merchantId = \"\"\n;printReceipt = false",
        );
        parameters.document_id = document_id;
        parameters.revision_id = revision_id;
        parameters.document_label = anchor.document_label.clone();

        let answer = build_setup_configuration_anchor_answer(
            "how to configure Sample Connector?",
            &query_ir,
            &[anchor, parameters],
        )
        .expect("subject-entity setup configuration answer");

        assert!(answer.contains("Sample Connector setup guide"), "{answer}");
        assert!(answer.contains("/etc/sample/connector.conf"), "{answer}");
        assert!(answer.contains("Main"), "{answer}");
        assert!(answer.contains("merchantId"), "{answer}");
        assert!(answer.contains("printReceipt"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_extracts_embedded_command_anchor() {
        let mut query_ir = configure_how_focus_ir("Sample Connector");
        query_ir.document_focus = None;
        query_ir.target_types = vec!["configuration_file".to_string(), "parameter".to_string()];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Connector".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Connector configuration\n\
             Install the package with sample-install sample-connector.\n\
             Configuration command sample-reconfigure sample-connector.\n\
             Settings are defined in /etc/sample/connector.conf in section [Main].\n\
             sampleMerchantId = \"\"\n\
             sampleTerminalKey = \"\"",
        );
        chunk.document_label = "Sample Connector setup guide".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "how to configure Sample Connector?",
            &query_ir,
            &[chunk],
        )
        .expect("setup configuration answer");

        assert!(answer.contains("sample-reconfigure sample-connector"), "{answer}");
        assert!(answer.contains("/etc/sample/connector.conf"), "{answer}");
        assert!(answer.contains("[Main]"), "{answer}");
        assert!(answer.contains("sampleMerchantId"), "{answer}");
    }

    #[test]
    fn setup_configuration_command_literals_reject_mixed_script_prose_fragments() {
        let commands = extract_setup_configuration_command_literals(
            "QR-code text appears in the user interface.\n\
             QR-код для оплаты отображается на экране.\n\
             Configuration command sample-reconfigure sample-connector.\n\
             Section [Main] contains sampleMerchantId.",
            8,
        );

        assert!(commands.iter().any(|command| command == "sample-reconfigure sample-connector"));
        assert!(!commands.iter().any(|command| command.contains("QR-код")), "{commands:?}");
        assert!(!commands.iter().any(|command| command.starts_with("Section ")), "{commands:?}");
    }

    #[test]
    fn setup_configuration_command_literals_reject_bare_word_prose_with_artifact() {
        let commands = extract_setup_configuration_command_literals(
            "address should be configured as https://localhost/api\n\
             package-name . sample-install package-name\n\
             sample-configure package-name",
            8,
        );

        assert_eq!(commands, vec!["sample-install package-name", "sample-configure package-name"]);
    }

    #[test]
    fn setup_configuration_anchor_answer_renders_beyond_four_variants() {
        let mut query_ir = configure_how_focus_ir("DeltaVariant");
        query_ir.target_types =
            vec!["configuration_file".to_string(), "package".to_string(), "procedure".to_string()];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "DeltaVariant".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        let chunks = (1..=5)
            .map(|index| {
                let mut chunk = evidence_chunk(
                    index,
                    Some("paragraph"),
                    &format!(
                        "DeltaVariant module {index} configuration\n\
                         sample-configure delta-module-{index}\n\
                         Settings are defined in /opt/delta/module-{index}.conf in section [Delta{index}]."
                    ),
                );
                chunk.document_label = format!("DeltaVariant setup variant {index}");
                chunk
            })
            .collect::<Vec<_>>();

        let answer =
            build_setup_configuration_anchor_answer("configure DeltaVariant", &query_ir, &chunks)
                .expect("setup configuration answer");

        assert!(answer.contains("DeltaVariant setup variant 5"), "{answer}");
        assert!(answer.contains("sample-configure delta-module-5"), "{answer}");
        assert!(answer.contains("/opt/delta/module-5.conf"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_suppresses_variant_heading_for_duplicate_anchors() {
        let mut primary = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\nsample-install alpha-subject\n\
             sample-configure alpha-subject\n\
             Settings are defined in /opt/subject/alpha/alpha.ini in section [AlphaMain].\n\
             url = http://localhost",
        );
        primary.document_label = "Subject Alpha setup".to_string();
        let mut secondary = evidence_chunk(
            2,
            Some("paragraph"),
            "Operational note\nsample-install alpha-subject\n\
             Settings file: /opt/subject/alpha/alpha.ini\n\
             timeout = 60",
        );
        secondary.document_label = "Subject Alpha operator guide".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "how to configure Subject Alpha?",
            &configure_how_focus_ir("Subject Alpha"),
            &[primary, secondary],
        )
        .expect("setup configuration answer");

        assert!(!answer.contains("**Setup variants:**"), "{answer}");
        assert!(answer.contains("Subject Alpha setup"));
        assert!(answer.contains("Subject Alpha operator guide"));
        assert!(answer.contains("alpha-subject"));
        assert!(answer.contains("/opt/subject/alpha/alpha.ini"));
    }

    #[test]
    fn setup_configuration_anchor_answer_skips_broad_retrieve_value_without_setup_target() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::RetrieveValue;
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Alpha reference".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Module reference\n\
             | Name | Type | Description |\n\
             | alpha_limit | integer | Maximum retained records |\n\
             | beta_window | duration | Rolling measurement window |",
        );
        chunk.document_label = "Neutral reference".to_string();

        assert!(
            build_setup_configuration_anchor_answer(
                "What value is listed for the alpha limit?",
                &query_ir,
                &[chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_skips_focused_value_status_lookup() {
        let mut query_ir = low_confidence_concept_ir();
        query_ir.act = QueryAct::RetrieveValue;
        query_ir.scope = QueryScope::SingleDocument;
        query_ir.target_types =
            vec!["document".to_string(), "config_key".to_string(), "error_code".to_string()];
        query_ir.document_focus =
            Some(crate::domains::query_ir::DocumentHint { hint: "reference.json".to_string() });
        query_ir.target_entities.push(crate::domains::query_ir::EntityMention {
            label: "adjustment record".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        });
        query_ir.literal_constraints.push(crate::domains::query_ir::LiteralSpan {
            text: "reason_code".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Identifier,
        });
        let mut chunk = evidence_chunk(
            1,
            Some("source_unit"),
            "fields: records.adjustment.reason_code.enum=receiving,return,damage,loss,correction; \
             records.adjustment.responses.422.description=Insufficient balance; \
             records.adjustment.section=[A-Z0-9-]; records.adjustment.parameter=reason_code",
        );
        chunk.document_label = "reference.json".to_string();

        assert!(
            build_setup_configuration_anchor_answer(
                "What reason codes are valid, and what status indicates insufficient balance?",
                &query_ir,
                &[chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_filters_structured_distractors_by_focus_token() {
        let mut irrelevant = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\nsample-install gamma-adapter\n\
             sample-configure gamma-adapter\n\
             Settings are defined in /opt/gamma/gamma.conf in section [GammaAdapter].\n\
             endpoint = http://localhost",
        );
        irrelevant.document_label = "Gamma Adapter setup reference".to_string();
        let mut relevant = evidence_chunk(
            2,
            Some("paragraph"),
            "Module configuration\nsample-install alpha-qps\n\
             sample-configure alpha-qps\n\
             Settings are defined in /opt/qps/alpha.conf in section [QpsAlpha].\n\
             primaryKey = \"\"",
        );
        relevant.document_label = "QPS Alpha setup reference".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "how to configure QPS?",
            &configure_how_focus_ir("QPS"),
            &[irrelevant, relevant],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("QPS Alpha setup reference"));
        assert!(answer.contains("alpha-qps"));
        assert!(answer.contains("/opt/qps/alpha.conf"));
        assert!(!answer.contains("Gamma Adapter setup reference"));
        assert!(!answer.contains("gamma-adapter"));
    }

    #[test]
    fn setup_configuration_anchor_answer_prefers_subject_label_over_body_only_match() {
        let mut body_only = evidence_chunk(
            1,
            Some("paragraph"),
            "Subject compatibility note\nsample-install dialog-tools\n\
             sample-configure dialog-tools\n\
             Settings are defined in /opt/dialog/dialog.conf in section [DialogQuestion].\n\
             subjectTimeout = 30\nquestionText = \"\"",
        );
        body_only.document_label = "Question dialog configuration guide".to_string();
        let mut focused = evidence_chunk(
            2,
            Some("paragraph"),
            "Workflow module configuration\nsample-runner --install sample-unit\n\
             sample-configure alpha-subject\n\
             Settings are defined in /opt/subject/alpha.conf in section [AlphaQrp].\n\
             primaryKey = \"\"",
        );
        focused.document_label = "Subject Alpha setup reference".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "how to configure Subject?",
            &configure_how_focus_ir("Subject"),
            &[body_only, focused],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("Subject Alpha setup reference"), "{answer}");
        assert!(answer.contains("alpha-subject"), "{answer}");
        assert!(!answer.contains("Question dialog configuration guide"), "{answer}");
        assert!(!answer.contains("dialog-tools"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_keeps_short_subject_followup_on_label_identity() {
        let mut stale = evidence_chunk(
            1,
            Some("paragraph"),
            "DeltaVariant compatibility parameter\nsample-runner --install sample-unit\n\
             sample-configure alpha-subject\n\
             Settings are defined in /opt/subject/alpha.conf in section [AlphaSubject].\n\
             deltaMode = true",
        );
        stale.document_label = "AlphaSubject setup reference".to_string();
        let mut focused = evidence_chunk(
            2,
            Some("paragraph"),
            "Workflow environment configuration\nsample-install delta-pay\n\
             sample-configure delta-pay\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].\n\
             primaryKey = \"\"",
        );
        focused.document_label = "DeltaVariant setup reference".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "DeltaVariant",
            &configure_how_focus_ir("DeltaVariant"),
            &[stale, focused],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("DeltaVariant setup reference"), "{answer}");
        assert!(answer.contains("delta-pay"), "{answer}");
        assert!(!answer.contains("AlphaSubject setup reference"), "{answer}");
        assert!(!answer.contains("alpha-subject"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_does_not_repeat_package_as_parameter() {
        let mut focused = evidence_chunk(
            1,
            Some("paragraph"),
            "Workflow environment configuration\nsample-install delta-pay\n\
             sample-configure delta-pay",
        );
        focused.document_label = "DeltaVariant setup reference".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "DeltaVariant",
            &configure_how_focus_ir("DeltaVariant"),
            &[focused],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("delta-pay"), "{answer}");
        assert!(!answer.contains("- **parameter:**"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_candidate_uses_actionable_single_variant_directly() {
        let mut focused = evidence_chunk(
            1,
            Some("paragraph"),
            "Workflow environment configuration\nsample-install delta-pay\n\
             sample-configure delta-pay\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].",
        );
        focused.document_label = "DeltaVariant setup reference".to_string();
        let mut query_ir = configure_how_focus_ir("DeltaVariant");
        query_ir.target_types =
            vec!["configuration_file".to_string(), "package".to_string(), "procedure".to_string()];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "DeltaVariant".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];

        let candidate = build_setup_configuration_anchor_candidate(
            "DeltaVariant",
            &query_ir,
            std::slice::from_ref(&focused),
        )
        .expect("focused setup configuration candidate");

        assert!(
            candidate.should_use_as_direct_answer(&query_ir, &[focused]),
            "single focused setup variants with package/path anchors should not fall through to LLM"
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_aggregates_split_document_config_chunks() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Workflow environment configuration\nsample-install delta-pay\n\
             sample-configure delta-pay\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].",
        );
        anchor.document_id = document_id;
        anchor.revision_id = revision_id;
        anchor.document_label = "DeltaVariant setup reference".to_string();
        let mut parameters = evidence_chunk(
            2,
            Some("table_row"),
            "primaryKey = \"\"\ncredentialToken = \"\"\ntimeout = 60",
        );
        parameters.document_id = document_id;
        parameters.revision_id = revision_id;
        parameters.document_label = anchor.document_label.clone();

        let answer = build_setup_configuration_anchor_answer(
            "DeltaVariant",
            &configure_how_focus_ir("DeltaVariant"),
            &[anchor, parameters],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("/opt/subject/delta.conf"), "{answer}");
        assert!(answer.contains("DeltaVariant"), "{answer}");
        assert!(answer.contains("primaryKey"), "{answer}");
        assert!(answer.contains("credentialToken"), "{answer}");
        assert!(answer.contains("timeout"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_keeps_late_same_document_parameters() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Workflow environment configuration\nsample-configure delta-pay\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].",
        );
        anchor.document_id = document_id;
        anchor.revision_id = revision_id;
        anchor.document_label = "DeltaVariant setup reference".to_string();
        let parameter_text = (0..16)
            .map(|index| format!("field{index:02} = true"))
            .chain(std::iter::once("lateCredential = 42".to_string()))
            .collect::<Vec<_>>()
            .join("\n");
        let mut parameters = evidence_chunk(2, Some("table_row"), &parameter_text);
        parameters.document_id = document_id;
        parameters.revision_id = revision_id;
        parameters.document_label = anchor.document_label.clone();

        let answer = build_setup_configuration_anchor_answer(
            "DeltaVariant",
            &configure_how_focus_ir("DeltaVariant"),
            &[anchor, parameters],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("lateCredential"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_preserves_split_command_only_anchor() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut command = evidence_chunk(1, Some("paragraph"), "sample-reconfigure delta-subject");
        command.document_id = document_id;
        command.revision_id = revision_id;
        command.document_label = "DeltaVariant setup reference".to_string();
        let mut parameters = evidence_chunk(
            2,
            Some("table_row"),
            "Sheet: Module settings | Row 12 | Name: primaryKey | Type: string | \
             Description: Primary credential value",
        );
        parameters.document_id = document_id;
        parameters.revision_id = revision_id;
        parameters.document_label = command.document_label.clone();
        let mut query_ir = configure_how_focus_ir("DeltaVariant");
        query_ir.target_types = vec![
            "configuration_file".to_string(),
            "parameter".to_string(),
            "procedure".to_string(),
        ];

        let answer = build_setup_configuration_anchor_answer(
            "how to configure DeltaVariant?",
            &query_ir,
            &[parameters, command],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("`sample-reconfigure delta-subject`"), "{answer}");
        assert!(answer.contains("`primaryKey`"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_preserves_structured_parameter_row_details() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Environment Delta configuration\n\
             Settings are defined in /opt/subject/delta.conf in section [DeltaVariant].",
        );
        anchor.document_id = document_id;
        anchor.revision_id = revision_id;
        anchor.document_label = "DeltaVariant setup reference".to_string();
        let mut first_row = evidence_chunk(
            2,
            Some("table_row"),
            "Sheet: Module settings | Row 12 | Name: staticQrId | Type: string | \
             Description: Static QR identifier | Notes: Required for static QR workflows",
        );
        first_row.document_id = document_id;
        first_row.revision_id = revision_id;
        first_row.document_label = anchor.document_label.clone();
        let mut second_row = evidence_chunk(
            3,
            Some("table_row"),
            "Sheet: Module settings | Row 13 | Name: callbackUrl | Type: URL | \
             Description: Workflow status callback endpoint | Notes: Defaults to the service URL",
        );
        second_row.document_id = document_id;
        second_row.revision_id = revision_id;
        second_row.document_label = anchor.document_label.clone();

        let answer = build_setup_configuration_anchor_answer(
            "DeltaVariant",
            &configure_how_focus_ir("DeltaVariant"),
            &[anchor, first_row, second_row],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("**Parameter details:**"), "{answer}");
        assert!(answer.contains("`staticQrId`"), "{answer}");
        assert!(answer.contains("Static QR identifier"), "{answer}");
        assert!(answer.contains("Required for static QR workflows"), "{answer}");
        assert!(answer.contains("`callbackUrl`"), "{answer}");
        assert!(answer.contains("Workflow status callback endpoint"), "{answer}");
        assert!(answer.contains("Defaults to the service URL"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_prefers_exact_subject_over_richer_generic_anchor() {
        let mut generic = evidence_chunk(
            1,
            Some("paragraph"),
            "Node configuration\nsample-install alpha-node-tools\n\
             sample-configure alpha-node-tools\n\
             Settings are defined in /etc/alpha/node.conf in section [Agent].\n\
             endpoint = http://localhost\n\
             token = \"\"\n\
             retryCount = 3",
        );
        generic.document_label = "Alpha node configuration guide".to_string();
        let mut focused = evidence_chunk(
            2,
            Some("paragraph"),
            "Subject Beta workflow setup\n\
             Settings are defined in /opt/subject/beta/beta.ini in section [QrpBeta].\n\
             primaryKey = \"\"\n\
             credentialToken = \"\"",
        );
        focused.document_label = "Subject Beta setup reference".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "Beta",
            &configure_how_focus_ir("Beta"),
            &[generic, focused],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("Subject Beta setup reference"), "{answer}");
        assert!(answer.contains("/opt/subject/beta/beta.ini"), "{answer}");
        assert!(!answer.contains("Alpha node configuration guide"), "{answer}");
        assert!(!answer.contains("alpha-node-tools"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_scores_label_identity_once_per_document() {
        let mut chunks = Vec::new();
        for index in 0..6 {
            let mut generic = evidence_chunk(
                index,
                Some("table_row"),
                "Sheet: Settings | Row 1 | Name: sharedTimeout | Type: integer",
            );
            generic.document_id = Uuid::from_u128(100);
            generic.document_label = "Subject Alpha setup reference".to_string();
            chunks.push(generic);
        }
        let mut exact = evidence_chunk(
            20,
            Some("paragraph"),
            "Module configuration\nsample-install beta-subject\n\
             sample-configure beta-subject\n\
             Settings are defined in /opt/subject/beta/beta.conf in section [QrpBeta].\n\
             primaryKey = \"\"",
        );
        exact.document_id = Uuid::from_u128(200);
        exact.document_label = "Subject Beta setup reference".to_string();
        chunks.push(exact);
        let mut query_ir = configure_how_focus_ir("Subject");
        query_ir.document_focus = None;
        query_ir.target_types = vec!["configuration_file".to_string(), "parameter".to_string()];
        query_ir.target_entities = vec![
            crate::domains::query_ir::EntityMention {
                label: "Subject".to_string(),
                role: crate::domains::query_ir::EntityRole::Subject,
            },
            crate::domains::query_ir::EntityMention {
                label: "Beta".to_string(),
                role: crate::domains::query_ir::EntityRole::Subject,
            },
        ];

        let answer = build_setup_configuration_anchor_answer(
            "how to configure Subject Beta?",
            &query_ir,
            &chunks,
        )
        .expect("focused setup configuration answer");

        let beta_position = answer.find("Subject Beta setup reference").expect(&answer);
        let alpha_position = answer.find("Subject Alpha setup reference").unwrap_or(usize::MAX);
        assert!(beta_position < alpha_position, "{answer}");
        assert!(answer.contains("/opt/subject/beta/beta.conf"), "{answer}");
        assert!(answer.contains("primaryKey"), "{answer}");
    }

    #[test]
    fn setup_configuration_anchor_answer_skips_describe_parameter_inventory() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Settings are defined in /x/s1-alpha.conf in section [Alpha].\n\
             firstValue = \"\"",
        );
        anchor.document_id = document_id;
        anchor.revision_id = revision_id;
        anchor.document_label = "S1 Alpha reference".to_string();
        let mut parameter_row = evidence_chunk(
            2,
            Some("table_row"),
            "Sheet: Parameters | Row 4 | Name: secondValue | Type: string | \
             Description: Secondary value | Notes: Optional",
        );
        parameter_row.document_id = document_id;
        parameter_row.revision_id = revision_id;
        parameter_row.document_label = anchor.document_label.clone();
        let mut query_ir = configure_how_focus_ir("S1");
        query_ir.act = QueryAct::Describe;
        query_ir.target_types = vec!["parameter".to_string()];

        assert!(
            build_setup_configuration_anchor_answer(
                "describe S1 values",
                &query_ir,
                &[anchor, parameter_row],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_keeps_section_only_variant() {
        let mut alpha = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\nsample-runner --install sample-unit\n\
             Settings are defined in /opt/subject/alpha/alpha.ini in section [AlphaQrp].\n\
             primaryKey = \"\"",
        );
        alpha.document_label = "Subject Alpha setup".to_string();
        let mut beta = evidence_chunk(
            2,
            Some("paragraph"),
            "Module configuration\n\
             Section [BetaQrp]\n\
             primaryKey = \"\"\n\
             timeout = 60",
        );
        beta.document_label = "Subject Beta setup".to_string();

        let answer = build_setup_configuration_anchor_answer(
            "how to configure Subject?",
            &configure_how_focus_ir("Subject"),
            &[alpha, beta],
        )
        .expect("setup variants answer");

        assert!(answer.contains("Subject Alpha setup"));
        assert!(answer.contains("Subject Beta setup"));
        assert!(answer.contains("BetaQrp"));
        assert!(answer.contains("timeout"));
    }

    #[test]
    fn setup_configuration_anchor_answer_ignores_footnote_only_sections() {
        let mut notes = evidence_chunk(
            1,
            Some("paragraph"),
            "Subject overview\n\
             The integration has several deployment options [1].\n\
             See the operational note [Note] and the release marker [OK].\n\
             Additional background is available in appendix [2].",
        );
        notes.document_label = "Subject overview notes".to_string();

        assert!(
            build_setup_configuration_anchor_answer(
                "how to configure Subject?",
                &configure_how_focus_ir("Subject"),
                &[notes],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_skips_service_port_inventory_ir() {
        let mut first = evidence_chunk(
            1,
            Some("paragraph"),
            "[Service]\n\
             name = alpha-api\n\
             port = 8101",
        );
        first.document_label = "Alpha services".to_string();
        let mut second = evidence_chunk(
            2,
            Some("paragraph"),
            "[Service]\n\
             name = beta-worker\n\
             port = 8102",
        );
        second.document_label = "Beta services".to_string();
        let mut query_ir = configure_how_focus_ir("services");
        query_ir.act = QueryAct::Describe;
        query_ir.document_focus = None;
        query_ir.target_types = vec!["service".to_string(), "port".to_string()];

        assert!(
            build_setup_configuration_anchor_answer(
                "What service ports are configured?",
                &query_ir,
                &[first, second],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_skips_control_detail_question() {
        let mut query_ir = configure_how_focus_ir("Alpha and Beta threshold controls");
        query_ir.target_entities = vec![
            crate::domains::query_ir::EntityMention {
                label: "Alpha component".to_string(),
                role: crate::domains::query_ir::EntityRole::Subject,
            },
            crate::domains::query_ir::EntityMention {
                label: "Beta component".to_string(),
                role: crate::domains::query_ir::EntityRole::Object,
            },
        ];
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\n\
             Settings are defined in /opt/neutral/alpha.conf in section [Alpha].\n\
             thresholdLimit = 100",
        );
        chunk.document_label = "Alpha component reference".to_string();

        assert!(
            build_setup_configuration_anchor_answer(
                "How do Alpha and Beta implement threshold handling? What configuration controls the threshold?",
                &query_ir,
                &[chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_skips_unambiguous_versioned_procedure() {
        let mut ir = configure_how_focus_ir("Sample Target");
        ir.document_focus = None;
        ir.target_types = vec!["artifact".to_string(), "procedure".to_string()];
        ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Target".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];

        let mut setup_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\nsample-runner --install sample-unit\n\
             Settings are defined in /opt/subject/alpha/alpha.ini.",
        );
        setup_chunk.document_label = "Subject Alpha setup".to_string();

        assert!(
            build_setup_configuration_anchor_answer(
                "how to update Sample Target?",
                &ir,
                &[setup_chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn setup_configuration_anchor_answer_skips_focused_concept_procedure_without_config_signal() {
        let mut ir = configure_how_focus_ir("Sample Target");
        ir.document_focus = None;
        ir.target_types = vec!["procedure".to_string(), "concept".to_string()];
        ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Target".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        let mut setup_chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Module settings\nsample-runner --install sample-unit\n\
             Settings are defined in /opt/sample/sample.ini.\n\
             timeout = 30",
        );
        setup_chunk.document_label = "Sample Target setup note".to_string();

        assert!(
            build_setup_configuration_anchor_answer(
                "how to update Sample Target?",
                &ir,
                &[setup_chunk],
            )
            .is_none()
        );
    }

    fn source_unit(index: i32, text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: index,
            chunk_kind: Some(super::super::SOURCE_UNIT_CHUNK_KIND.to_string()),
            document_label: "records.jsonl".to_string(),
            excerpt: text.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(3.0),
            source_text: text.to_string(),
        }
    }

    fn evidence_chunk(index: i32, kind: Option<&str>, text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: index,
            chunk_kind: kind.map(str::to_string),
            document_label: "records.jsonl".to_string(),
            excerpt: text.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::Relevance,
            score: Some(3.0),
            source_text: text.to_string(),
        }
    }

    fn release_identity_chunk(
        document_id: Uuid,
        revision_id: Uuid,
        index: i32,
        score: f32,
        label: &str,
        text: &str,
    ) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id,
            chunk_index: index,
            chunk_kind: Some("document_identity".to_string()),
            document_label: label.to_string(),
            excerpt: text.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::DocumentIdentity,
            score: Some(score),
            source_text: text.to_string(),
        }
    }

    fn release_relevance_chunk(
        document_id: Uuid,
        revision_id: Uuid,
        index: i32,
        score: f32,
        label: &str,
        text: &str,
    ) -> RuntimeMatchedChunk {
        let mut chunk = release_identity_chunk(document_id, revision_id, index, score, label, text);
        chunk.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::Relevance;
        chunk.chunk_kind = Some("paragraph".to_string());
        chunk
    }

    fn source_context_chunk(
        document_id: Uuid,
        revision_id: Uuid,
        index: i32,
        text: &str,
    ) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            revision_id,
            chunk_index: index,
            chunk_kind: Some("table_row".to_string()),
            document_label: "Subject Alpha setup".to_string(),
            excerpt: text.to_string(),
            score_kind: crate::services::query::execution::RuntimeChunkScoreKind::SourceContext,
            score: Some(100.0 - index as f32),
            source_text: text.to_string(),
        }
    }

    fn configure_how_ir() -> crate::domains::query_ir::QueryIR {
        crate::domains::query_ir::QueryIR {
            act: crate::domains::query_ir::QueryAct::ConfigureHow,
            scope: crate::domains::query_ir::QueryScope::SingleDocument,
            language: crate::domains::query_ir::QueryLanguage::Auto,
            target_types: vec!["parameter".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: Some(crate::domains::query_ir::DocumentHint {
                hint: "Subject Alpha setup".to_string(),
            }),
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    #[test]
    fn configure_answers_keep_more_than_two_source_context_rows_per_document() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunks = vec![
            source_context_chunk(
                document_id,
                revision_id,
                1,
                "Install package alpha-connector and edit /opt/alpha/connector.conf.",
            ),
            source_context_chunk(document_id, revision_id, 2, "| apiUrl | string | Service URL |"),
            source_context_chunk(
                document_id,
                revision_id,
                3,
                "| retryTimeout | integer | Response timeout | Default 10 |",
            ),
            source_context_chunk(
                document_id,
                revision_id,
                4,
                "| partnerId | string | Registered partner identifier |",
            ),
            source_context_chunk(
                document_id,
                revision_id,
                5,
                "| credentialToken | string | Shared authorization secret |",
            ),
        ];

        let section = render_canonical_chunk_section(
            "How do I configure Subject Alpha parameters?",
            &configure_how_ir(),
            &chunks,
            false,
        );

        assert!(section.contains("apiUrl"));
        assert!(section.contains("retryTimeout"));
        assert!(section.contains("partnerId"));
        assert!(section.contains("credentialToken"));
    }

    #[test]
    fn ordered_source_units_answer_lists_every_unit_once() {
        let answer = build_ordered_source_units_answer(
            &source_slice_ir(2),
            &[
                source_unit(
                    2,
                    "[unit_id=b occurred_at=2026-01-02T00:00:00+00:00 actor_label=Assistant] second",
                ),
                source_unit(
                    1,
                    "[unit_id=a occurred_at=2026-01-01T00:00:00+00:00 actor_label=User] first",
                ),
            ],
        )
        .expect("source slice answer");

        assert!(answer.starts_with("`records.jsonl` - 2/2"));
        assert!(answer.find("first").unwrap() < answer.find("second").unwrap());
        assert_eq!(answer.matches("\n1. ").count(), 1);
        assert_eq!(answer.matches("\n2. ").count(), 1);
    }

    #[test]
    fn ordered_source_units_answer_reports_partial_count() {
        let answer =
            build_ordered_source_units_answer(&source_slice_ir(3), &[source_unit(1, "body")])
                .expect("source slice answer");

        assert!(answer.starts_with("`records.jsonl` - 1/3"));
        assert!(answer.contains("`ordinal=1`"));
    }

    #[test]
    fn ordered_source_slice_unit_section_prefers_source_units_over_fallback_chunks() {
        let fallback = evidence_chunk(1, Some("paragraph"), "fallback paragraph");
        let selected = source_unit(7, "[unit_id=u-7] selected record");

        let section =
            render_ordered_source_slice_unit_section(&source_slice_ir(1), &[fallback, selected])
                .expect("source slice section");

        assert!(section.contains("returned_unit_count=1"));
        assert!(section.contains("[unit_id=u-7] selected record"));
        assert!(!section.contains("fallback paragraph"));
    }

    #[test]
    fn latest_source_slice_answer_falls_back_to_ranked_context_chunks() {
        let older_id = Uuid::now_v7();
        let newer_id = Uuid::now_v7();
        let older_revision_id = Uuid::now_v7();
        let newer_revision_id = Uuid::now_v7();
        let long_body = (1..=20)
            .map(|index| format!("detail {index:02} for Sample Subject"))
            .collect::<Vec<_>>()
            .join("\n");
        let chunks = vec![
            evidence_chunk(0, Some("source_profile"), "[source_profile unit_count=3]"),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                100.0,
                "Guide appendix",
                "mentions compatibility with 9.9.9 but is not a release identity",
            ),
            release_identity_chunk(
                older_id,
                older_revision_id,
                0,
                90.0,
                "Release 1.0.1",
                "older detail",
            ),
            release_identity_chunk(
                newer_id,
                newer_revision_id,
                0,
                30.0,
                "Release 1.0.2",
                &long_body,
            ),
            release_identity_chunk(
                newer_id,
                newer_revision_id,
                1,
                20.0,
                "Release 1.0.2 duplicate",
                "duplicate",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks)
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.used_context_fallback);
        assert!(answer.answer.starts_with("2/2"));
        assert!(
            answer.answer.find("Release 1.0.2").unwrap()
                < answer.answer.find("Release 1.0.1").unwrap()
        );
        assert!(!answer.answer.contains("source_profile"));
        assert!(!answer.answer.contains("Guide appendix"));
        assert!(!answer.answer.contains("duplicate"));
        assert!(answer.answer.contains("detail 01 for Sample Subject"));
        assert!(!answer.answer.contains("detail 20 for Sample Subject"));
    }

    #[test]
    fn latest_source_slice_answer_does_not_require_explicit_source_slice() {
        let mut ir = latest_source_slice_ir(2);
        ir.source_slice = None;
        ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "2".to_string(),
            kind: crate::domains::query_ir::LiteralKind::NumericCode,
        }];
        let older_id = Uuid::now_v7();
        let newer_id = Uuid::now_v7();
        let chunks = vec![
            release_identity_chunk(
                older_id,
                Uuid::now_v7(),
                0,
                10.0,
                "Sample Subject Release 1.0.1",
                "older detail",
            ),
            release_identity_chunk(
                newer_id,
                Uuid::now_v7(),
                0,
                10.0,
                "Sample Subject Release 1.0.2",
                "newer detail",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&ir, &[], &chunks)
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.used_context_fallback);
        assert!(answer.answer.starts_with("2/2"));
        assert!(answer.answer.contains("source=`Sample Subject Release 1.0.2`"));
        assert!(
            answer.answer.find("Sample Subject Release 1.0.2").unwrap()
                < answer.answer.find("Sample Subject Release 1.0.1").unwrap()
        );
    }

    #[test]
    fn latest_source_slice_fallback_yields_to_update_procedure_intent() {
        let mut ir = configure_update_focus_ir("Sample Target");
        ir.retrieval_query = Some("how to update Sample Target version?".to_string());
        let release_noise = release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Sample Subject Release 1.0.2",
            "Version 1.0.2\nCompatibility note for Sample Target.",
        );
        let mut procedure = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target update procedure:\n\
             sample-runner --refresh\n\
             sample-runner --apply\n\
             sudo sample-configure alpha-subject\n\
             sudo service alpha-subject restart",
        );
        procedure.document_label = "Instruction for updating Sample Target".to_string();

        assert!(
            build_ordered_source_slice_answer(
                &ir,
                &[],
                &[release_noise.clone(), procedure.clone()]
            )
            .is_none(),
            "inferred latest-version inventory must not preempt versioned update procedure answers"
        );
        let answer = build_update_procedure_sequence_answer(
            "how to update Sample Target version?",
            &ir,
            &[release_noise, procedure],
        )
        .expect("update procedure answer");
        assert!(answer.contains("sample-runner --refresh"), "{answer}");
        assert!(answer.contains("sample-runner --apply"), "{answer}");
        assert!(answer.contains("sample-configure"), "{answer}");
        assert!(answer.contains("service alpha-subject restart"), "{answer}");
    }

    #[test]
    fn latest_source_slice_answer_uses_query_ir_focus_body_versions() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut older = release_relevance_chunk(
            document_id,
            revision_id,
            1,
            90.0,
            "Sample Subject release history",
            "Version 1.0.1\nolder detail",
        );
        older.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
        let mut newer = release_relevance_chunk(
            document_id,
            revision_id,
            0,
            80.0,
            "Sample Subject release history",
            "Version 1.0.2\nnewer detail",
        );
        newer.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
        let noise = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Sample Subject release history",
            "Compatibility note 9.9.9\nnot a focused release unit",
        );

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(2),
            &[],
            &[older, noise, newer],
        )
        .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.used_context_fallback);
        assert!(answer.answer.contains("Version 1.0.2"));
        assert!(answer.answer.contains("Version 1.0.1"));
        assert!(
            answer.answer.find("Version 1.0.2").unwrap()
                < answer.answer.find("Version 1.0.1").unwrap()
        );
        assert!(!answer.answer.contains("not a focused release unit"));
    }

    #[test]
    fn latest_source_slice_answer_ignores_plain_relevance_body_versions() {
        let chunks = vec![release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Sample Subject release history",
            "Version 1.0.2\nplain relevance noise",
        )];

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks);

        assert!(answer.is_none());
    }

    #[test]
    fn latest_source_slice_answer_ignores_document_identity_body_only_versions() {
        let chunks = vec![release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Guide appendix",
            "Version 9.9.9\ncompatibility note, not a release identity",
        )];

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks);

        assert!(answer.is_none());
    }

    #[test]
    fn latest_source_slice_answer_removes_markdown_decoration() {
        let mut ir = latest_source_slice_ir(1);
        ir.source_slice = None;
        let document_id = Uuid::now_v7();
        let chunks = vec![release_identity_chunk(
            document_id,
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Release 1.0.2",
            "# Version 1.0.2\n![preview](https://example.invalid/preview.png)\n[diagram](https://example.invalid/diagram.png)\n---\n## Changes\nAdded deterministic inventory output.",
        )];

        let answer = build_ordered_source_slice_answer(&ir, &[], &chunks)
            .expect("latest source-slice answer");

        assert!(answer.answer.contains("Version 1.0.2"));
        assert!(answer.answer.contains("Changes"));
        assert!(answer.answer.contains("Added deterministic inventory output."));
        assert!(!answer.answer.contains("# Version"));
        assert!(!answer.answer.contains("![preview]"));
        assert!(!answer.answer.contains("[diagram]("));
        assert!(!answer.answer.contains("---"));
    }

    #[test]
    fn latest_source_slice_answer_uses_extended_source_unit_payload() {
        let mut unit = source_unit(
            1,
            "[unit_id=u-1 occurred_at=2026-01-02T00:00:00+00:00 actor_label=Recorder] ![preview](asset.png)\n\
             Version 1.0.2\n\
             - Added neutral evidence line\n\
             - Added second neutral evidence line",
        );
        unit.document_label = "Neutral record stream".to_string();

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[unit], &[])
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(!answer.used_context_fallback);
        assert!(answer.answer.contains("Version 1.0.2"));
        assert!(answer.answer.contains("Added neutral evidence line"));
        assert!(answer.answer.contains("Added second neutral evidence line"));
        assert!(!answer.answer.contains("![preview]"));
    }

    #[test]
    fn latest_source_slice_answer_respects_runtime_latest_rank_before_body_version() {
        let mut ranked_unit = source_unit(4, "[unit_id=u-4 version=1.0.1 change=ranked-evidence]");
        ranked_unit.document_label = "Neutral inventory".to_string();
        ranked_unit.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;
        ranked_unit.score = Some(120.0);

        let mut newer_noise = source_unit(1, "[unit_id=u-1 version=9.9.9 change=unrelated-newer]");
        newer_noise.document_label = "Neutral inventory".to_string();
        newer_noise.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;
        newer_noise.score = Some(80.0);

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(1),
            &[newer_noise, ranked_unit],
            &[],
        )
        .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("ranked-evidence"), "{}", answer.answer);
        assert!(!answer.answer.contains("unrelated-newer"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_skips_plain_overview_version_for_release_marker() {
        let overview = release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Sample Product 5.0",
            "Sample Product 5.0 overview and compatibility notes.",
        );
        let release = release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            50.0,
            "Build 2.4.259 - Neutral stream",
            "Changed neutral behavior.",
        );

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(1),
            &[],
            &[overview, release],
        )
        .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("Build 2.4.259"), "{}", answer.answer);
        assert!(answer.answer.contains("Changed neutral behavior"), "{}", answer.answer);
        assert!(!answer.answer.contains("Sample Product 5.0"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_preserves_version_header_payload() {
        let mut unit = source_unit(1, "[unit_id=u-1 version=1.0.2 change=neutral-header-evidence]");
        unit.document_label = "Neutral record stream".to_string();

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[unit], &[])
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("version=1.0.2"), "{}", answer.answer);
        assert!(answer.answer.contains("change=neutral-header-evidence"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_preserves_single_document_version_label_heading() {
        let mut unit = source_unit(1, "[unit_id=u-1] Changed neutral behavior.");
        unit.document_label = "Build 2.4.259 - Neutral stream".to_string();

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[unit], &[])
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("**Build 2.4.259 - Neutral stream**"));
        assert!(answer.answer.contains("Changed neutral behavior."));
    }

    #[test]
    fn latest_source_slice_answer_keeps_dominant_release_family() {
        let chunks = vec![
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Sample Subject Release 1.0.2",
                "alpha newer",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Sample Subject Release 1.0.1",
                "alpha older",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Beta Tool Release 9.0.0",
                "beta unrelated",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks)
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.answer.contains("Sample Subject Release 1.0.2"));
        assert!(answer.answer.contains("Sample Subject Release 1.0.1"));
        assert!(!answer.answer.contains("Beta Tool Release 9.0.0"));
    }

    #[test]
    fn low_confidence_context_release_series_uses_latest_inventory_fallback() {
        let alpha_new_id = Uuid::now_v7();
        let alpha_old_id = Uuid::now_v7();
        let beta_id = Uuid::now_v7();
        let mut ir = low_confidence_concept_ir();
        ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "2".to_string(),
            kind: crate::domains::query_ir::LiteralKind::NumericCode,
        }];
        let chunks = vec![
            release_relevance_chunk(
                alpha_old_id,
                Uuid::now_v7(),
                0,
                5.0,
                "Sample Subject Release 1.0.1",
                "older alpha detail",
            ),
            release_relevance_chunk(
                beta_id,
                Uuid::now_v7(),
                0,
                100.0,
                "Beta Tool Release 9.0.0",
                "beta distractor",
            ),
            release_relevance_chunk(
                alpha_new_id,
                Uuid::now_v7(),
                0,
                1.0,
                "Sample Subject Release 1.0.2",
                "newer alpha detail",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&ir, &[], &chunks)
            .expect("semver document series should infer latest inventory answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.used_context_fallback);
        assert!(answer.answer.starts_with("2/2"));
        assert!(
            answer.answer.find("Sample Subject Release 1.0.2").unwrap()
                < answer.answer.find("Sample Subject Release 1.0.1").unwrap()
        );
        assert!(!answer.answer.contains("Beta Tool Release 9.0.0"));
    }

    #[test]
    fn context_release_series_fallback_skips_exact_version_questions() {
        let mut ir = low_confidence_concept_ir();
        ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "1.0.2".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Version,
        }];
        let chunks = vec![
            release_relevance_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Sample Subject Release 1.0.2",
                "exact detail",
            ),
            release_relevance_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                9.0,
                "Sample Subject Release 1.0.1",
                "older detail",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&ir, &[], &chunks);

        assert!(answer.is_none());
    }

    #[test]
    fn context_release_series_fallback_skips_high_confidence_non_release_ir() {
        let mut ir = low_confidence_concept_ir();
        ir.confidence = 0.9;
        let chunks = vec![
            release_relevance_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Sample Subject Release 1.0.2",
                "newer detail",
            ),
            release_relevance_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                9.0,
                "Sample Subject Release 1.0.1",
                "older detail",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&ir, &[], &chunks);

        assert!(answer.is_none());
    }

    #[test]
    fn exact_version_query_does_not_use_latest_identity_fallback() {
        let mut ir = latest_source_slice_ir(5);
        ir.source_slice = None;
        ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "1.0.2".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Version,
        }];
        let chunks = vec![release_identity_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            10.0,
            "Sample Subject Release 1.0.2",
            "exact release detail",
        )];

        let answer = build_ordered_source_slice_answer(&ir, &[], &chunks);

        assert!(answer.is_none());
    }

    #[test]
    fn exact_version_change_answer_uses_matching_graph_evidence_bullets() {
        let lines = vec![
            "[graph-evidence target=\"Version 1.2.2\"]\nVersion 1.2.2\n- Older item"
                .to_string(),
            "[graph-evidence target=\"Version 1.2.3\"]\nVersion 1.2.3 - Sample Subject\n\nChanges\n\n- Added indexed lookup by suffix\n- Added `pricedocid` to `documents.goodsitem`\n- Updated monitor counters"
                .to_string(),
            "[graph-evidence target=\"Sample Subject --has_property--> 1.2.3\"]\nRelated guide\n- Adjacent workflow note"
                .to_string(),
        ];

        let answer =
            build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &[], &lines)
                .expect("exact version graph answer");

        assert!(answer.contains("Version 1.2.3 - Sample Subject"));
        assert!(answer.contains("Added indexed lookup by suffix"));
        assert!(answer.contains("`pricedocid`"));
        assert!(!answer.contains("Older item"));
        assert!(!answer.contains("Adjacent workflow note"));
    }

    #[test]
    fn exact_version_change_answer_falls_back_to_matching_context_chunks() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let chunks = vec![
            release_identity_chunk(
                document_id,
                revision_id,
                0,
                10.0,
                "Sample Subject Version 1.2.3",
                "# Version 1.2.3\n\n- Added operator audit\n- Added retry metric",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Sample Subject Version 1.2.2",
                "# Version 1.2.2\n\n- Older item",
            ),
        ];

        let answer =
            build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &chunks, &[])
                .expect("exact version chunk answer");

        assert!(answer.contains("Sample Subject Version 1.2.3"));
        assert!(answer.contains("Added operator audit"));
        assert!(answer.contains("Added retry metric"));
        assert!(!answer.contains("Older item"));
    }

    #[test]
    fn exact_version_change_answer_does_not_match_version_substrings() {
        let lines = vec![
            "[graph-evidence target=\"Version 1.2.30\"]\nVersion 1.2.30\n- Wrong patch line\n- Wrong second line"
                .to_string(),
            "[graph-evidence target=\"Version 21.2.3\"]\nVersion 21.2.3\n- Wrong major line\n- Wrong second line"
                .to_string(),
        ];

        let answer =
            build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &[], &lines);

        assert!(answer.is_none());
    }

    #[test]
    fn exact_version_change_answer_groups_chunks_by_document_id_before_label() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let shared_label = "Sample Subject Version 1.2.3";
        let chunks = vec![
            release_identity_chunk(
                document_id,
                revision_id,
                0,
                10.0,
                shared_label,
                "# Version 1.2.3\n\n- Added operator audit",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                1,
                10.0,
                shared_label,
                "# Version 1.2.3\n\n- Adjacent one-line note",
            ),
            release_identity_chunk(
                document_id,
                revision_id,
                2,
                10.0,
                shared_label,
                "# Version 1.2.3\n\n- Added retry metric",
            ),
        ];

        let answer =
            build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &chunks, &[])
                .expect("grouped exact version answer");

        assert!(answer.contains("Added operator audit"));
        assert!(answer.contains("Added retry metric"));
    }

    #[test]
    fn structured_source_unit_evidence_uses_extended_context() {
        let late_marker = "late-marker-structural-unit";
        let source_text = format!("[unit_id=a] {} {late_marker}", "content ".repeat(120));
        let chunk = evidence_chunk(7, Some(super::super::SOURCE_UNIT_CHUNK_KIND), &source_text);

        let lines = render_evidence_chunk_lines(&[&chunk], &[], "sampled");

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("scope=source_unit"));
        assert!(lines[0].contains(late_marker));
    }

    #[test]
    fn ordinary_evidence_chunks_remain_excerpt_bounded() {
        let late_marker = "late ordinary evidence marker";
        let source_text = format!("{} {late_marker}", "content ".repeat(120));
        let chunk = evidence_chunk(7, None, &source_text);

        let lines = render_evidence_chunk_lines(&[&chunk], &[], "sampled");

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("scope=excerpt"));
        assert!(!lines[0].contains(late_marker));
    }
}

#[cfg(test)]
#[path = "answer_document_label_tests.rs"]
mod document_label_tests;
