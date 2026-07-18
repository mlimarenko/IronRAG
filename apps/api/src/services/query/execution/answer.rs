use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::services::query::text_match::{
    label_terms, normalized_alnum_token_sequence, normalized_alnum_tokens,
    token_sequence_contains_tokens, token_sequence_exact_or_contains_tokens,
};
use crate::{
    domains::query_ir::{
        EntityRole, LiteralKind, QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind,
    },
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
    QuestionIntent, classify_question_or_ir_intents, query_ir_allows_procedure_runbook_target,
    query_ir_has_setup_configuration_target, query_ir_is_unambiguous_versioned_procedure,
    query_ir_requires_remediation_synthesis,
};
use crate::services::query::completion_policy::AnswerCompletionContract;
use crate::services::query::effective_query::current_question_segment;
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::chunk_support::{
    chunk_is_setup_focus_command_path_anchor, command_dense_excerpt_for, excerpt_for,
    focused_excerpt_for,
};
use super::command_shape::{
    procedure_line_has_command_start, procedure_line_has_list_marker,
    shellish_token_file_artifact_name, shellish_token_has_artifact_preparation_signal,
    shellish_token_has_executable_name_shape, shellish_token_is_command_argument_signal,
    shellish_token_is_invocable_head, shellish_token_is_local_artifact,
    shellish_token_is_path_command_start, shellish_tokens_start_command,
    split_concatenated_local_artifact_token, strip_leading_numeric_order_marker,
};
use super::source_excerpt::{salient_source_excerpt_for, structured_literal_excerpt_for};
use super::technical_answer::build_exact_technical_literal_answer;
#[cfg(test)]
use super::technical_literals::technical_chunk_selection_score;
use super::technical_literals::{
    extract_config_assignment_literals, extract_explicit_path_literals, extract_http_methods,
    extract_package_command_literals, extract_parameter_literals, extract_prefix_literals,
    extract_url_literals, select_document_balanced_chunks, technical_literal_focus_keywords,
};
use super::types::{CanonicalAnswerEvidence, RuntimeChunkScoreKind, RuntimeMatchedChunk};
use super::{
    build_table_row_grounded_answer, build_table_summary_grounded_answer,
    focus_token_overlap_count, query_ir_document_focus_tokens, question_asks_table_aggregation,
};
use crate::services::query::latest_versions::{
    ReleaseSourceIdentity, compare_version_desc, extract_release_context_version,
    extract_semver_like_version, is_version_token_continuation, query_requests_latest_versions,
    requested_latest_version_count,
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
        build_port_and_protocol_answer_from_facts(question, query_ir, evidence, chunks),
        question,
        query_ir,
        evidence,
        chunks,
    )
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
    if query_ir_requires_remediation_synthesis(query_ir) {
        return None;
    }
    let answer = build_table_summary_grounded_answer(question, Some(query_ir), chunks)
        .or_else(|| build_table_row_grounded_answer(question, Some(query_ir), chunks))
        .or_else(|| build_multi_document_evidence_comparison_answer(query_ir, chunks))
        .or_else(|| {
            build_setup_configuration_anchor_candidate(question, query_ir, chunks)
                .filter(|candidate| candidate.should_use_as_preflight_answer(query_ir, chunks))
                .map(SetupConfigurationAnchorCandidate::into_answer)
        })
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

fn build_multi_document_evidence_comparison_answer(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !matches!(query_ir.act, QueryAct::Compare)
        || !matches!(query_ir.scope, QueryScope::MultiDocument)
    {
        return None;
    }
    let mut best_by_document = HashMap::<Uuid, &RuntimeMatchedChunk>::new();
    for chunk in chunks {
        let text = chunk.source_text.trim();
        if text.is_empty() {
            continue;
        }
        best_by_document
            .entry(chunk.document_id)
            .and_modify(|current| {
                if score_value(chunk.score) > score_value(current.score)
                    || score_value(chunk.score) == score_value(current.score)
                        && chunk.chunk_index < current.chunk_index
                {
                    *current = chunk;
                }
            })
            .or_insert(chunk);
    }
    if best_by_document.len() < 2 {
        return None;
    }
    let mut selected = best_by_document.into_values().collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        score_value(right.score)
            .total_cmp(&score_value(left.score))
            .then_with(|| left.document_label.cmp(&right.document_label))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
    });
    let sections = selected
        .into_iter()
        .take(8)
        .map(|chunk| {
            format!(
                "### {}\n{}",
                chunk.document_label.trim(),
                excerpt_for(chunk.source_text.trim(), 1_200)
            )
        })
        .collect::<Vec<_>>();
    (sections.len() >= 2).then(|| sections.join("\n\n"))
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
    let procedure_focus = matches!(query_ir.act, QueryAct::ConfigureHow)
        .then(|| update_procedure_focus_model(query_ir));
    let mut candidates = chunks
        .iter()
        .filter(|chunk| {
            procedure_focus.as_ref().is_none_or(|focus| {
                structured_list_chunk_has_acceptable_procedure_identity(chunk, focus)
            })
        })
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
    if matches!(query_ir.act, QueryAct::ConfigureHow)
        && candidate
            .items
            .iter()
            .filter(|item| structured_list_item_has_procedure_shape(item))
            .take(2)
            .count()
            < 2
    {
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
    let answer = lines.join("\n");
    AnswerCompletionContract::from_query_ir(query_ir).evaluate(&answer).complete.then_some(answer)
}

fn structured_list_item_has_procedure_shape(item: &str) -> bool {
    if line_has_command_signal(item) {
        return true;
    }
    let trimmed = item.trim();
    matches!(trimmed.chars().next_back(), Some('.' | ';' | ':' | '!' | '?'))
        && normalized_alnum_tokens(trimmed, 1).len() >= 2
}

fn structured_list_chunk_has_acceptable_procedure_identity(
    chunk: &RuntimeMatchedChunk,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    if focus_model.target_identity_sequences.is_empty() {
        return true;
    }
    let text = repair_technical_layout_noise(&chunk.source_text);
    update_procedure_text_target_identity_priority(&chunk.document_label, focus_model) > 0
        || text
            .lines()
            .any(|line| update_procedure_text_target_identity_priority(line, focus_model) > 0)
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
        let Some(candidate) =
            marked_list_candidate(lines, line_index, line, focus_terms, chunk_index)
        else {
            continue;
        };
        candidates.push(candidate);
    }
    candidates
}

fn marked_list_candidate(
    lines: &[&str],
    line_index: usize,
    line: &str,
    focus_terms: &BTreeSet<String>,
    chunk_index: i32,
) -> Option<StructuredListCandidate> {
    if !line.ends_with(':') {
        return None;
    }
    let heading_score = structured_list_line_focus_score(line, focus_terms);
    if heading_score == 0 && !focus_terms.is_empty() {
        return None;
    }
    let (items, ordered) = marked_list_items_after_heading(lines, line_index);
    if items.len() < 2 {
        return None;
    }
    let item_score =
        items.iter().map(|item| structured_list_line_focus_score(item, focus_terms)).sum();
    Some(StructuredListCandidate {
        items,
        ordered,
        score: heading_score.saturating_mul(4).saturating_add(item_score),
        first_chunk_index: chunk_index,
    })
}

fn marked_list_items_after_heading(lines: &[&str], line_index: usize) -> (Vec<String>, bool) {
    let mut items = Vec::new();
    let mut ordered = false;
    for following in lines.iter().skip(line_index + 1) {
        if let Some((item_ordered, item)) = parse_structured_list_item(following) {
            ordered |= item_ordered;
            push_unique_structured_list_item(&mut items, item);
        } else if !items.is_empty() {
            break;
        }
    }
    (items, ordered)
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
        !query_ir_requires_remediation_synthesis(query_ir)
            && !self.is_multi_variant()
            && query_ir_has_setup_configuration_target(query_ir)
            && (self.should_use_as_preflight_answer(query_ir, chunks) || self.has_actionable_anchor)
    }
    pub(super) fn should_use_as_preflight_answer(
        &self,
        query_ir: &QueryIR,
        chunks: &[RuntimeMatchedChunk],
    ) -> bool {
        !query_ir_requires_remediation_synthesis(query_ir)
            && !self.is_multi_variant()
            && query_ir_has_setup_configuration_target(query_ir)
            && (query_has_multi_document_setup_anchors(query_ir, chunks)
                || self.has_parameter_details())
    }
}

#[cfg(test)]
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
        render_setup_configuration_variant(&mut lines, variant, labels);
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

fn render_setup_configuration_variant(
    lines: &mut Vec<String>,
    variant: &SetupConfigurationVariant,
    labels: DeterministicAnswerLabels,
) {
    lines.push(format!("**{}:** **{}**", labels.source, variant.source));
    push_setup_configuration_values(lines, labels.package, &variant.packages);
    push_setup_configuration_values(lines, labels.reconfigure, &variant.reconfigure_packages);
    push_setup_configuration_values(lines, labels.path, &variant.paths);
    if !variant.sections.is_empty() {
        let sections =
            variant.sections.iter().map(|section| format!("[{section}]")).collect::<Vec<_>>();
        push_setup_configuration_values(lines, labels.section, &sections);
    }
    push_setup_configuration_values(lines, labels.parameter, &variant.parameters);
    if !variant.parameter_rows.is_empty() {
        lines.push(format!("- **{}:**", labels.parameter_details));
        lines.extend(
            variant
                .parameter_rows
                .iter()
                .map(|row| format!("  - `{}` — {}", row.name, row.render_details(labels))),
        );
    }
    lines.push(String::new());
}

fn push_setup_configuration_values(lines: &mut Vec<String>, label: &str, values: &[String]) {
    if !values.is_empty() {
        lines.push(format!("- **{label}:** `{}`", values.join("`, `")));
    }
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

fn question_requests_lifecycle_update_procedure(_question: &str, query_ir: &QueryIR) -> bool {
    if query_ir_is_unambiguous_versioned_procedure(query_ir) {
        return true;
    }
    if query_ir.targets_any(&[
        QueryTargetKind::Version,
        QueryTargetKind::Release,
        QueryTargetKind::Changelog,
    ]) {
        return true;
    }
    if query_ir.source_slice.as_ref().is_some_and(|slice| {
        matches!(slice.filter, crate::domains::query_ir::SourceSliceFilter::ReleaseMarker)
    }) {
        return true;
    }
    if query_ir
        .literal_constraints
        .iter()
        .any(|literal| matches!(literal.kind, LiteralKind::Version))
        || query_ir
            .retrieval_query
            .as_deref()
            .is_some_and(|query| extract_semver_like_version(query).is_some())
    {
        return true;
    }
    false
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
    query_ir.targets_any(&[
        QueryTargetKind::Connection,
        QueryTargetKind::Port,
        QueryTargetKind::Protocol,
        QueryTargetKind::Service,
    ])
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
    positional_assignment_value: Option<String>,
}

impl SetupConfigurationParameterRow {
    fn render_details(&self, _labels: DeterministicAnswerLabels) -> String {
        let mut rendered = self
            .details
            .iter()
            .filter(|(_, value)| !value.is_empty())
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if rendered.is_empty()
            && let Some(value) = self.positional_assignment_value.as_deref()
        {
            rendered.push_str(value);
        }
        rendered
    }

    fn positional_assignment(&self) -> Option<String> {
        let value = self.positional_assignment_value.as_deref()?.trim();
        if !setup_configuration_default_value_is_assignment_scalar(value) {
            return None;
        }
        Some(format!("{} = {value}", self.name))
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
        let Some(contribution) =
            setup_configuration_variant_from_chunk(chunk, &focus_tokens, &subject_label_sequences)
        else {
            continue;
        };
        merge_setup_configuration_variant(
            &mut variants,
            chunk,
            contribution,
            SETUP_CONFIGURATION_PARAMETER_LITERAL_LIMIT,
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

struct SetupConfigurationVariantContribution {
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

fn setup_configuration_variant_from_chunk(
    chunk: &RuntimeMatchedChunk,
    focus_tokens: &BTreeSet<String>,
    subject_label_sequences: &[Vec<String>],
) -> Option<SetupConfigurationVariantContribution> {
    const PARAMETER_LITERAL_LIMIT: usize = 24;
    const PARAMETER_ROW_LIMIT: usize = 16;
    let text = repair_technical_layout_noise(&format!("{}\n{}", chunk.source_text, chunk.excerpt));
    let packages = extract_package_command_literals(&text, 4);
    let paths = extract_configuration_path_literals(&text, 4);
    let sections = extract_configuration_section_literals(&text, 4);
    let reconfigure_packages = extract_setup_configuration_command_literals(&text, 6);
    let parameter_rows = extract_setup_configuration_parameter_rows(&text, PARAMETER_ROW_LIMIT);
    let parameters = filter_setup_configuration_parameters(
        extract_config_assignment_literals(&text, PARAMETER_LITERAL_LIMIT)
            .into_iter()
            .chain(
                parameter_rows
                    .iter()
                    .filter_map(SetupConfigurationParameterRow::positional_assignment),
            )
            .chain(extract_parameter_literals(&text, PARAMETER_LITERAL_LIMIT))
            .chain(parameter_rows.iter().map(|row| row.name.clone()))
            .collect(),
        &packages,
        &reconfigure_packages,
        &paths,
        &sections,
    );
    if packages.is_empty()
        && reconfigure_packages.is_empty()
        && paths.is_empty()
        && sections.is_empty()
        && parameters.is_empty()
    {
        return None;
    }
    let focus_score = setup_configuration_focus_score(focus_tokens, chunk, &text);
    let label_focus_score = setup_configuration_label_focus_score(subject_label_sequences, chunk);
    if !focus_tokens.is_empty() && focus_score == 0 && label_focus_score == 0 {
        return None;
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
    let parameter_only = packages.is_empty()
        && reconfigure_packages.is_empty()
        && paths.is_empty()
        && sections.is_empty();
    (score >= 16 || parameter_only).then_some(SetupConfigurationVariantContribution {
        score,
        focus_score,
        label_focus_score,
        packages,
        reconfigure_packages,
        paths,
        sections,
        parameters,
        parameter_rows,
    })
}

fn merge_setup_configuration_variant(
    variants: &mut BTreeMap<Uuid, SetupConfigurationVariant>,
    chunk: &RuntimeMatchedChunk,
    contribution: SetupConfigurationVariantContribution,
    parameter_limit: usize,
    parameter_row_limit: usize,
) {
    let entry = variants.entry(chunk.document_id).or_insert_with(|| SetupConfigurationVariant {
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
    entry.score = entry.score.saturating_add(contribution.score);
    entry.focus_score = entry.focus_score.saturating_add(contribution.focus_score);
    entry.label_focus_score = entry.label_focus_score.max(contribution.label_focus_score);
    push_unique_values(&mut entry.packages, contribution.packages, 4);
    push_unique_values(&mut entry.reconfigure_packages, contribution.reconfigure_packages, 4);
    push_unique_values(&mut entry.paths, contribution.paths, 4);
    push_unique_values(&mut entry.sections, contribution.sections, 4);
    push_unique_values(&mut entry.parameters, contribution.parameters, parameter_limit);
    push_unique_parameter_rows(
        &mut entry.parameter_rows,
        contribution.parameter_rows,
        parameter_row_limit,
    );
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
    anchor_sets.iter().enumerate().any(|(left_index, left)| {
        anchor_sets.iter().skip(left_index + 1).any(|right| {
            left.iter().any(|anchor| !right.contains(anchor))
                && right.iter().any(|anchor| !left.contains(anchor))
        })
    })
}

fn setup_configuration_variant_anchor_keys(
    variant: &SetupConfigurationVariant,
) -> BTreeSet<String> {
    let identity_anchors = variant
        .packages
        .iter()
        .chain(&variant.reconfigure_packages)
        .chain(&variant.sections)
        .map(|value| setup_configuration_literal_key(value))
        .filter(|value| !value.is_empty())
        .collect::<BTreeSet<_>>();
    if identity_anchors.is_empty() {
        return variant
            .paths
            .iter()
            .map(|value| setup_configuration_literal_key(value))
            .filter(|value| !value.is_empty())
            .collect();
    }
    identity_anchors
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
    if setup_configuration_text_starts_with_artifact(line) {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    let command = strip_leading_order_marker(line)
        .trim()
        .trim_end_matches(['.', ',', ';'])
        .trim()
        .to_string();
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

fn setup_configuration_text_starts_with_artifact(text: &str) -> bool {
    let Some(head) = strip_leading_order_marker(text)
        .split_whitespace()
        .next()
        .map(trim_command_boundary_token_decorations)
    else {
        return false;
    };
    head.contains("://") || shellish_token_is_path_command_start(head)
}

fn setup_configuration_embedded_command_literals(line: &str) -> Vec<String> {
    let tokens = line.split_whitespace().collect::<Vec<_>>();
    if tokens.len() < 3 {
        return Vec::new();
    }
    let mut commands = Vec::new();
    for start in 1..tokens.len() {
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

fn setup_configuration_command_literal_is_usable(command: &str) -> bool {
    !command.is_empty()
        && !setup_configuration_command_is_table_row(command)
        && !setup_configuration_command_is_commented_fragment(command)
        && !setup_configuration_command_is_standalone_path(command)
        && !setup_configuration_command_is_standalone_assignment(command)
        && setup_configuration_command_has_invocable_structural_head(command)
        && line_has_command_signal(command)
}

fn setup_configuration_command_has_invocable_structural_head(command: &str) -> bool {
    let tokens = command_token_values(strip_leading_order_marker(command));
    let Some(head) = tokens.first().map(String::as_str) else {
        return false;
    };
    shellish_token_has_executable_name_shape(head)
        && !shellish_token_is_path_command_start(head)
        && tokens.iter().skip(1).any(|token| {
            token.starts_with('-')
                || token.contains('=')
                || token.contains("://")
                || token.contains('|')
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
    let tokens = command_token_values(strip_leading_order_marker(command));
    let head = structural_command_head_token(&tokens)?;
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
    let positional_assignment_value =
        cells.iter().enumerate().rev().find_map(|(index, (_, value))| {
            (index != name_index && setup_configuration_default_value_is_assignment_scalar(value))
                .then(|| value.clone())
        });
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
    Some(SetupConfigurationParameterRow { name, details, positional_assignment_value })
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
    Some(SetupConfigurationParameterRow {
        name,
        details: Vec::new(),
        positional_assignment_value: Some(default),
    })
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
    if query_ir_requires_remediation_synthesis(query_ir) {
        return None;
    }
    let focus_model = update_procedure_focus_model(query_ir);
    if !question_requests_update_procedure_answer(query_ir) {
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
    let answer = lines.join("\n");
    let has_executable_sequence =
        selection.steps.iter().filter(|step| line_has_command_signal(step)).take(2).count() >= 2;
    (has_executable_sequence
        || AnswerCompletionContract::from_query_ir(query_ir).evaluate(&answer).complete)
        .then_some(answer)
}

fn render_update_procedure_step(index: usize, step: &str) -> String {
    let stripped = strip_leading_order_marker(step).trim();
    if line_has_command_signal(stripped) {
        let command = stripped.trim_matches('`').trim_end_matches(['.', ',', ';']).trim();
        format!("{index}. `{command}`")
    } else {
        format!("{index}. {stripped}")
    }
}

fn question_requests_update_procedure_answer(query_ir: &QueryIR) -> bool {
    let allows_retrieved_single_document =
        query_ir_allows_retrieved_single_document_procedure_sequence(query_ir);
    let has_typed_procedure_action = query_ir_has_typed_procedure_action(query_ir);
    query_ir.source_slice.is_none()
        && (query_ir_has_explicit_procedure_focus(query_ir) || allows_retrieved_single_document)
        && ((has_typed_procedure_action && !query_ir_has_setup_configuration_target(query_ir))
            || query_ir_allows_procedure_runbook_target(query_ir)
            || allows_retrieved_single_document)
}

fn query_ir_has_explicit_procedure_focus(query_ir: &QueryIR) -> bool {
    !query_ir.target_entities.is_empty() || query_ir.document_focus.is_some()
}

fn query_ir_has_typed_procedure_action(query_ir: &QueryIR) -> bool {
    if !query_ir_has_explicit_procedure_focus(query_ir)
        || query_ir_has_setup_configuration_target(query_ir)
    {
        return false;
    }
    let mut has_procedure = false;
    let mut has_version_or_release = false;
    let mut has_concept = false;
    for target_type in &query_ir.target_types {
        match target_type {
            QueryTargetKind::Procedure => has_procedure = true,
            QueryTargetKind::Version | QueryTargetKind::Release => {
                has_version_or_release = true;
            }
            QueryTargetKind::Concept => has_concept = true,
            _ => {}
        }
    }
    has_procedure && has_version_or_release && !has_concept
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
        match target_type {
            QueryTargetKind::Procedure => has_procedure = true,
            QueryTargetKind::Concept => has_concept = true,
            QueryTargetKind::Artifact
            | QueryTargetKind::Document
            | QueryTargetKind::Entity
            | QueryTargetKind::PrimaryHeading
            | QueryTargetKind::SecondaryHeading
            | QueryTargetKind::Version
            | QueryTargetKind::Release => {
                has_document_or_revision_signal = true;
            }
            _ => {}
        }
    }
    let has_explicit_subject_focus =
        query_ir.document_focus.is_some() || !query_ir.target_entities.is_empty();
    let has_literal_focus = !query_ir.literal_constraints.is_empty();
    let has_typed_subject_focus = has_explicit_subject_focus || has_literal_focus;
    has_procedure && has_typed_subject_focus && (has_concept || has_document_or_revision_signal)
}

struct UpdateProcedureFocusModel {
    subject_terms: BTreeSet<String>,
    target_identity_sequences: Vec<UpdateProcedureIdentitySequence>,
    requires_exact_document_subject: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
struct UpdateProcedureIdentitySequence {
    tokens: Vec<String>,
    priority: usize,
}
fn update_procedure_focus_model(query_ir: &QueryIR) -> UpdateProcedureFocusModel {
    let mut subject_terms = BTreeSet::<String>::new();
    for entity in &query_ir.target_entities {
        subject_terms.extend(label_terms(&entity.label, 2));
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        subject_terms.extend(label_terms(&document_focus.hint, 2));
    }
    for literal in &query_ir.literal_constraints {
        if matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other) {
            subject_terms.extend(label_terms(&literal.text, 1));
        }
    }
    let target_identity_sequences = update_procedure_target_identity_token_sequences(query_ir);
    let requires_exact_document_subject = (query_ir.document_focus.is_some()
        || !query_ir.target_entities.is_empty())
        && query_ir.targets(QueryTargetKind::Document);
    UpdateProcedureFocusModel {
        subject_terms,
        target_identity_sequences,
        requires_exact_document_subject,
    }
}

fn update_procedure_target_identity_token_sequences(
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
            false,
        );
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_update_procedure_target_identity_sequence(
            &mut sequences,
            &mut seen,
            &document_focus.hint,
            20,
            false,
        );
    }
    for literal in &query_ir.literal_constraints {
        if matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other) {
            push_update_procedure_target_identity_sequence(
                &mut sequences,
                &mut seen,
                &literal.text,
                50,
                true,
            );
        }
    }
    sequences
}

fn push_update_procedure_target_identity_sequence(
    sequences: &mut Vec<UpdateProcedureIdentitySequence>,
    seen: &mut BTreeSet<Vec<String>>,
    label: &str,
    priority: usize,
    allow_single_token: bool,
) {
    let sequence = normalized_alnum_token_sequence(label, 1);
    if sequence.is_empty() || (sequence.len() < 2 && !allow_single_token) {
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
        })
        .map(|target_sequence| target_sequence.priority)
        .max()
        .unwrap_or_default()
}

const UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_CHARS: usize = 480;
const UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_LINES: usize = 6;

fn update_procedure_text_has_bound_target_identity_runbook(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    if focus_model.target_identity_sequences.is_empty() {
        return false;
    }
    update_procedure_bound_target_identity_windows(text).into_iter().any(|window| {
        update_procedure_text_target_identity_priority(&window, focus_model) > 0
            && window
                .lines()
                .filter(|line| update_procedure_step_is_structural(line))
                .take(2)
                .count()
                >= 2
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
    } else if let Some(line) = lines.first()
        && !update_procedure_line_has_sentence_boundary(line)
    {
        push_update_procedure_bound_target_identity_window(
            &mut windows,
            &mut seen,
            (*line).to_string(),
        );
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
    command_count: usize,
    preparatory_command_score: usize,
    focus_aligned_command_score: usize,
    unfocused_command_score: usize,
    has_setup_script_signature: bool,
    is_focus_projection: bool,
}

#[derive(Debug, Clone)]
struct UpdateProcedureCandidate {
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
    let document_bindings = update_procedure_document_bindings(focus_model, chunks);
    let mut candidates = chunks
        .iter()
        .flat_map(|chunk| {
            update_procedure_candidates_from_chunk(
                focus_model,
                chunk,
                document_bindings.get(&chunk.document_id).copied().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    select_update_procedure_candidate(focus_model, &mut candidates)
}

fn update_procedure_candidates_from_chunk(
    focus_model: &UpdateProcedureFocusModel,
    chunk: &RuntimeMatchedChunk,
    document_binding: UpdateProcedureDocumentBinding,
) -> Vec<UpdateProcedureCandidate> {
    let label_target_identity_priority =
        update_procedure_text_target_identity_priority(&chunk.document_label, focus_model);
    let text = repair_technical_layout_noise(&update_procedure_chunk_text(chunk, focus_model));
    let raw_extracts = update_procedure_extracts_from_text(&text, focus_model);
    let mut extracts = raw_extracts
        .iter()
        .filter(|extract| {
            update_procedure_extract_is_typed_evidence(extract, focus_model, &chunk.document_label)
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(aggregate) = update_procedure_focus_aligned_command_aggregate(
        &raw_extracts,
        focus_model,
        &chunk.document_label,
    )
    .filter(|extract| {
        update_procedure_extract_is_typed_evidence(extract, focus_model, &chunk.document_label)
    }) {
        extracts.push(aggregate);
    }
    let has_richer_extract = extracts.iter().any(|extract| extract.steps.len() >= 4);
    extracts
        .into_iter()
        .filter(|extract| !has_richer_extract || extract.steps.len() >= 4)
        .filter_map(|extract| {
            update_procedure_candidate_from_extract(
                focus_model,
                chunk,
                document_binding,
                label_target_identity_priority,
                extract,
            )
        })
        .collect()
}

fn update_procedure_candidate_from_extract(
    focus_model: &UpdateProcedureFocusModel,
    chunk: &RuntimeMatchedChunk,
    document_binding: UpdateProcedureDocumentBinding,
    label_target_identity_priority: usize,
    extract: UpdateProcedureExtract,
) -> Option<UpdateProcedureCandidate> {
    let label_focus_score = update_procedure_text_focus_score(&chunk.document_label, focus_model);
    let block_focus_score = update_procedure_text_focus_score(&extract.block_text, focus_model);
    let focused_structural_score =
        update_procedure_focused_structural_score(&extract.block_text, focus_model);
    let raw_block_target_identity_priority =
        update_procedure_text_target_identity_priority(&extract.block_text, focus_model);
    let block_target_identity_is_bound = raw_block_target_identity_priority > 0
        && (document_binding.strong_body_target_binding
            || (label_target_identity_priority > 0
                && update_procedure_text_has_bound_target_identity_runbook(
                    &extract.block_text,
                    focus_model,
                )));
    if label_target_identity_priority == 0
        && raw_block_target_identity_priority > 0
        && !block_target_identity_is_bound
    {
        return None;
    }
    let block_target_identity_priority =
        raw_block_target_identity_priority * usize::from(block_target_identity_is_bound);
    let target_identity_priority =
        label_target_identity_priority.max(block_target_identity_priority);
    if target_identity_priority == 0 {
        return None;
    }
    let target_identity_focus_score =
        if label_target_identity_priority > 0 { label_focus_score } else { block_focus_score };
    let score = update_procedure_candidate_score(
        &extract,
        label_focus_score,
        target_identity_focus_score,
        focused_structural_score,
    );
    let command_count = extract.command_count;
    let steps = update_procedure_steps_with_adjacent_same_head_preparation(
        extract.steps,
        &extract.block_text,
    );
    let anchors = update_procedure_evidence_anchors(&steps, &extract.block_text, 8);
    Some(UpdateProcedureCandidate {
        label_target_identity: label_target_identity_priority > 0,
        target_identity_priority,
        target_identity_focus_score,
        score,
        command_count,
        focused_structural_score,
        selection: UpdateProcedureSelection {
            source: chunk.document_label.clone(),
            steps,
            anchors,
        },
    })
}

fn update_procedure_candidate_score(
    extract: &UpdateProcedureExtract,
    label_focus_score: usize,
    target_identity_focus_score: usize,
    focused_structural_score: usize,
) -> usize {
    let exact_target_identity_bonus =
        if extract.steps.len() >= 4 && extract.command_count >= 2 { 32768 } else { 8192 };
    let unfocused_command_penalty =
        if extract.focus_aligned_command_score > 0 { 4096 } else { 256 };
    extract
        .score
        .saturating_add(
            usize::from(extract.is_focus_projection && extract.command_count >= 2)
                .saturating_mul(65_536),
        )
        .saturating_add(exact_target_identity_bonus)
        .saturating_add(label_focus_score.saturating_mul(96))
        .saturating_add(target_identity_focus_score.saturating_mul(1024))
        .saturating_add(focused_structural_score.saturating_mul(160))
        .saturating_add(extract.steps.len().saturating_mul(24))
        .saturating_add(extract.command_count.saturating_mul(512))
        .saturating_add(update_procedure_command_candidate_bonus(extract.command_count))
        .saturating_add(extract.preparatory_command_score.saturating_mul(512))
        .saturating_add(extract.focus_aligned_command_score.saturating_mul(4096))
        .saturating_sub(extract.unfocused_command_score.saturating_mul(unfocused_command_penalty))
}

fn select_update_procedure_candidate(
    focus_model: &UpdateProcedureFocusModel,
    candidates: &mut Vec<UpdateProcedureCandidate>,
) -> Option<UpdateProcedureSelection> {
    if focus_model.requires_exact_document_subject
        && !candidates.iter().any(update_procedure_candidate_has_target_identity_preference)
    {
        return None;
    }
    retain_preferred_update_procedure_candidates(candidates);
    candidates
        .drain(..)
        .max_by(compare_update_procedure_candidates)
        .map(|candidate| candidate.selection)
}

fn retain_preferred_update_procedure_candidates(candidates: &mut Vec<UpdateProcedureCandidate>) {
    let preferred_priority = candidates
        .iter()
        .filter(|candidate| update_procedure_candidate_has_target_identity_preference(candidate))
        .map(|candidate| candidate.target_identity_priority)
        .max()
        .unwrap_or_default();
    if preferred_priority > 0 {
        candidates.retain(|candidate| {
            candidate.target_identity_priority == preferred_priority
                && update_procedure_candidate_has_target_identity_preference(candidate)
        });
    }
    if candidates.iter().any(|candidate| {
        candidate.label_target_identity
            && update_procedure_candidate_has_target_identity_preference(candidate)
    }) {
        candidates.retain(|candidate| {
            candidate.label_target_identity
                && update_procedure_candidate_has_target_identity_preference(candidate)
        });
    }
    if candidates.iter().any(update_procedure_candidate_has_structural_command_preference) {
        candidates.retain(update_procedure_candidate_has_structural_command_preference);
    }
}

fn compare_update_procedure_candidates(
    left: &UpdateProcedureCandidate,
    right: &UpdateProcedureCandidate,
) -> std::cmp::Ordering {
    left.score
        .cmp(&right.score)
        .then_with(|| left.target_identity_priority.cmp(&right.target_identity_priority))
        .then_with(|| left.target_identity_focus_score.cmp(&right.target_identity_focus_score))
        .then_with(|| left.focused_structural_score.cmp(&right.focused_structural_score))
        .then_with(|| left.selection.steps.len().cmp(&right.selection.steps.len()))
        .then_with(|| left.command_count.cmp(&right.command_count))
        .then_with(|| right.selection.source.cmp(&left.selection.source))
        .then_with(|| right.selection.anchors.cmp(&left.selection.anchors))
}

#[derive(Debug, Clone, Copy, Default)]
struct UpdateProcedureDocumentBinding {
    strong_body_target_binding: bool,
}

fn update_procedure_document_bindings(
    focus_model: &UpdateProcedureFocusModel,
    chunks: &[RuntimeMatchedChunk],
) -> HashMap<Uuid, UpdateProcedureDocumentBinding> {
    let mut chunks_by_document = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        chunks_by_document.entry(chunk.document_id).or_default().push(chunk);
    }
    chunks_by_document
        .into_iter()
        .map(|(document_id, mut document_chunks)| {
            document_chunks.sort_by(|left, right| {
                left.chunk_index
                    .cmp(&right.chunk_index)
                    .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            });
            let mut seen_fragments = HashSet::<String>::new();
            let mut body_fragments = Vec::<&str>::new();
            for chunk in &document_chunks {
                for fragment in [chunk.source_text.as_str(), chunk.excerpt.as_str()] {
                    let fragment = fragment.trim();
                    if !fragment.is_empty() && seen_fragments.insert(fragment.to_string()) {
                        body_fragments.push(fragment);
                    }
                }
            }
            let body = repair_technical_layout_noise(&body_fragments.join("\n"));
            let strong_body_target_binding =
                update_procedure_text_has_strong_target_procedure_binding(&body, focus_model);
            (document_id, UpdateProcedureDocumentBinding { strong_body_target_binding })
        })
        .collect()
}

fn update_procedure_text_has_strong_target_procedure_binding(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    if focus_model.target_identity_sequences.is_empty() {
        return false;
    }
    let lines = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    let distinct_target_steps = lines
        .iter()
        .filter(|line| {
            update_procedure_text_target_identity_priority(line, focus_model) > 0
                && update_procedure_step_is_structural(line)
        })
        .map(|line| update_procedure_normalized_step(line).to_lowercase())
        .collect::<BTreeSet<_>>();
    if distinct_target_steps.len() >= 2 {
        return true;
    }

    lines.iter().enumerate().any(|(heading_index, heading)| {
        if strip_leading_order_marker(heading).trim() != *heading
            || line_has_command_signal(heading)
            || update_procedure_text_target_identity_priority(heading, focus_model) == 0
        {
            return false;
        }
        lines
            .iter()
            .skip(heading_index + 1)
            .take(8)
            .filter(|line| update_procedure_step_is_structural(line))
            .take(2)
            .count()
            >= 2
    })
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
        let candidate = update_procedure_normalized_step(&tokens[index..].join(" "));
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
        .map(|line| update_procedure_step_from_line(&line))
        .collect()
}

fn update_procedure_step_key(step: &str) -> String {
    update_procedure_normalized_step(step).to_lowercase()
}

fn update_procedure_normalized_step(step: &str) -> String {
    strip_leading_order_marker(step)
        .trim()
        .trim_matches('`')
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

fn update_procedure_candidate_has_structural_command_preference(
    candidate: &UpdateProcedureCandidate,
) -> bool {
    candidate.command_count >= 2
        && (candidate.label_target_identity
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
    if aggregate_focus_score == 0 {
        return None;
    }
    let selected = selected_update_procedure_extracts(extracts, focus_model, document_label);
    if selected.len() < 2 {
        return None;
    }
    let steps = unique_update_procedure_steps(&selected, 16);
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
        command_count: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.command_count).sum(),
        ),
        preparatory_command_score: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.preparatory_command_score).sum(),
        ),
        focus_aligned_command_score: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.focus_aligned_command_score).sum(),
        ),
        unfocused_command_score: 0,
        has_setup_script_signature: selected
            .iter()
            .any(|extract| extract.has_setup_script_signature),
        is_focus_projection: false,
    })
}

fn selected_update_procedure_extracts<'a>(
    extracts: &'a [UpdateProcedureExtract],
    focus_model: &UpdateProcedureFocusModel,
    document_label: &str,
) -> Vec<&'a UpdateProcedureExtract> {
    let mut selected_by_block = BTreeMap::<usize, &UpdateProcedureExtract>::new();
    for extract in extracts.iter().filter(|extract| {
        extract.command_count > 0
            && update_procedure_extract_is_typed_evidence(extract, focus_model, document_label)
    }) {
        selected_by_block
            .entry(extract.block_index)
            .and_modify(|current| {
                if (extract.score, extract.steps.len(), extract.is_focus_projection)
                    > (current.score, current.steps.len(), current.is_focus_projection)
                {
                    *current = extract;
                }
            })
            .or_insert(extract);
    }
    let mut selected = selected_by_block.into_values().collect::<Vec<_>>();
    let preparatory_indexes = selected
        .iter()
        .filter(|extract| extract.preparatory_command_score > 0)
        .map(|extract| extract.block_index)
        .collect::<Vec<_>>();
    if preparatory_indexes.len() > 2
        && let Some(keep_from) = preparatory_indexes.get(preparatory_indexes.len() - 2)
    {
        selected.retain(|extract| {
            extract.focus_aligned_command_score > 0 || extract.block_index >= *keep_from
        });
    }
    selected
}

fn unique_update_procedure_steps(
    extracts: &[&UpdateProcedureExtract],
    limit: usize,
) -> Vec<String> {
    let mut seen = HashSet::new();
    extracts
        .iter()
        .flat_map(|extract| &extract.steps)
        .filter(|step| seen.insert(step.to_lowercase()))
        .take(limit)
        .cloned()
        .collect()
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
    let selected = update_procedure_focused_line_indexes(&lines, focus_model);
    if selected.is_empty() {
        return None;
    }
    render_update_procedure_focused_lines(&lines, selected, UPDATE_PROCEDURE_SOURCE_VIEW_CHARS)
}

fn update_procedure_focused_line_indexes(
    lines: &[&str],
    focus_model: &UpdateProcedureFocusModel,
) -> BTreeSet<usize> {
    let mut selected = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        if update_procedure_text_target_identity_priority(line, focus_model) == 0
            && update_procedure_text_focus_score(line, focus_model) == 0
        {
            continue;
        }
        selected.insert(index);
        if index > 0
            && (line_has_order_marker(lines[index - 1])
                || line_has_command_signal(lines[index - 1]))
        {
            selected.insert(index - 1);
        }
        select_update_procedure_lookahead(lines, index, focus_model, &mut selected);
    }
    selected
}

fn select_update_procedure_lookahead(
    lines: &[&str],
    index: usize,
    focus_model: &UpdateProcedureFocusModel,
    selected: &mut BTreeSet<usize>,
) {
    for next_index in (index + 1)..=(index + 2) {
        let Some(next_line) = lines.get(next_index) else {
            break;
        };
        let relevant = line_has_order_marker(next_line)
            || line_has_command_signal(next_line)
            || update_procedure_text_target_identity_priority(next_line, focus_model) > 0
            || update_procedure_text_focus_score(next_line, focus_model) > 0;
        if !relevant {
            break;
        }
        selected.insert(next_index);
    }
}

fn render_update_procedure_focused_lines(
    lines: &[&str],
    selected: BTreeSet<usize>,
    max_chars: usize,
) -> Option<String> {
    let mut focused = String::new();
    let mut previous_index = None;
    for index in selected {
        if !focused.is_empty() {
            focused.push_str(if previous_index.is_some_and(|previous| index > previous + 1) {
                "\n...\n"
            } else {
                "\n"
            });
        }
        focused.push_str(lines[index]);
        previous_index = Some(index);
        if focused.chars().count() >= max_chars {
            return Some(excerpt_for(&focused, max_chars));
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
            steps.push(update_procedure_step_from_line(line));
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
    let has_setup_script_signature = update_procedure_block_has_setup_script_signature(block);
    Some(UpdateProcedureExtract {
        block_index,
        score,
        steps,
        block_text,
        command_count,
        preparatory_command_score,
        focus_aligned_command_score,
        unfocused_command_score,
        has_setup_script_signature,
        is_focus_projection: false,
    })
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
    let mut projection = UpdateProcedureCommandProjection::default();
    projection.prepend_ordered_context(block, focus_model);
    for line in block.iter().filter(|line| line.has_command) {
        projection.push_command(line, focus_model);
        if projection.steps.len() >= 16 {
            break;
        }
    }
    append_trailing_artifact_chain_steps(
        &mut projection.steps,
        &mut projection.seen,
        &projection.pending_preparatory,
    );
    if projection.focus_aligned_command_score == 0 || projection.steps.len() < 2 {
        return None;
    }
    if projection.steps.iter().filter(|step| update_procedure_step_is_structural(step)).count() < 2
    {
        return None;
    }
    let block_text = source_extract.block_text.clone();
    let focused_structural_score =
        update_procedure_focused_structural_score(&block_text, focus_model);
    let score = source_extract
        .score
        .saturating_add(focused_structural_score.saturating_mul(160))
        .saturating_add(
            update_procedure_capped_command_score(projection.preparatory_command_score)
                .saturating_mul(2048),
        )
        .saturating_add(
            update_procedure_capped_command_score(projection.focus_aligned_command_score)
                .saturating_mul(8192),
        )
        .saturating_add(update_procedure_command_candidate_bonus(projection.steps.len()));
    Some(UpdateProcedureExtract {
        block_index,
        score,
        command_count: update_procedure_capped_command_score(projection.steps.len()),
        steps: projection.steps,
        block_text,
        preparatory_command_score: projection.preparatory_command_score,
        focus_aligned_command_score: projection.focus_aligned_command_score,
        unfocused_command_score: 0,
        has_setup_script_signature: source_extract.has_setup_script_signature,
        is_focus_projection: true,
    })
}

#[derive(Default)]
struct UpdateProcedureCommandProjection<'a> {
    seen: HashSet<String>,
    steps: Vec<String>,
    preparatory_command_score: usize,
    focus_aligned_command_score: usize,
    pending_preparatory: Vec<&'a UpdateProcedureLine>,
}

impl<'a> UpdateProcedureCommandProjection<'a> {
    fn prepend_ordered_context(
        &mut self,
        block: &'a [UpdateProcedureLine],
        focus_model: &UpdateProcedureFocusModel,
    ) {
        let Some(first_aligned_index) = block.iter().position(|line| {
            line.has_command
                && update_procedure_command_focus_aligned_score(
                    strip_leading_order_marker(&line.text),
                    focus_model,
                ) > 0
        }) else {
            return;
        };
        if block.iter().take(first_aligned_index).any(|line| line.has_command) {
            return;
        }
        for line in block.iter().take(first_aligned_index).filter(|line| line.has_order_marker) {
            self.push_unique_step(line);
        }
    }

    fn push_command(
        &mut self,
        line: &'a UpdateProcedureLine,
        focus_model: &UpdateProcedureFocusModel,
    ) {
        let aligned_score = update_procedure_command_focus_aligned_score(
            strip_leading_order_marker(&line.text),
            focus_model,
        );
        if aligned_score == 0 {
            self.pending_preparatory.push(line);
            if self.pending_preparatory.len() > 3 {
                self.pending_preparatory.remove(0);
            }
            return;
        }
        let preparatory =
            self.pending_preparatory.drain(..).rev().take(2).rev().collect::<Vec<_>>();
        for preparatory in preparatory {
            if self.push_unique_step(preparatory) {
                self.preparatory_command_score = update_procedure_capped_command_score(
                    self.preparatory_command_score.saturating_add(1),
                );
            }
        }
        self.focus_aligned_command_score = update_procedure_capped_command_score(
            self.focus_aligned_command_score.saturating_add(aligned_score),
        );
        self.push_unique_step(line);
    }

    fn push_unique_step(&mut self, line: &UpdateProcedureLine) -> bool {
        if !self.seen.insert(line.text.to_lowercase()) {
            return false;
        }
        self.steps.push(update_procedure_step_from_line(line));
        true
    }
}

fn append_trailing_artifact_chain_steps(
    steps: &mut Vec<String>,
    seen: &mut HashSet<String>,
    trailing: &[&UpdateProcedureLine],
) {
    let mut artifacts =
        steps.iter().flat_map(|step| command_local_artifact_keys(step)).collect::<BTreeSet<_>>();
    if artifacts.is_empty() {
        return;
    }
    for line in trailing {
        let line_artifacts = command_local_artifact_keys(&line.text);
        if line_artifacts.is_empty() || line_artifacts.is_disjoint(&artifacts) {
            continue;
        }
        artifacts.extend(line_artifacts);
        if seen.insert(line.text.to_lowercase()) {
            steps.push(update_procedure_step_from_line(line));
        }
    }
}

fn command_local_artifact_keys(command: &str) -> BTreeSet<String> {
    command_token_values(strip_leading_order_marker(command))
        .into_iter()
        .enumerate()
        .filter_map(|(index, token)| {
            let usable = if index == 0 {
                shellish_token_is_path_command_start(&token)
            } else {
                token_has_local_command_artifact(&token)
            };
            usable.then(|| trim_command_boundary_token_decorations(&token).to_ascii_lowercase())
        })
        .collect()
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
        .map(update_procedure_step_from_line)
        .collect::<Vec<_>>();
    if steps.len() < 4 {
        return None;
    }
    let block_text = steps.join("\n");
    let command_count = update_procedure_capped_command_score(projection_block.len());
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
        .saturating_add(focus_aligned_command_score.saturating_mul(4096))
        .saturating_add(target_identity_priority.saturating_mul(2048))
        .saturating_sub(unfocused_command_score.saturating_mul(512));
    Some(UpdateProcedureExtract {
        block_index,
        score,
        steps,
        block_text,
        command_count,
        preparatory_command_score,
        focus_aligned_command_score,
        unfocused_command_score,
        has_setup_script_signature: source_extract.has_setup_script_signature,
        is_focus_projection: true,
    })
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
    focus_model.subject_terms.intersection(&tokens).count().saturating_mul(16)
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

fn update_procedure_extract_is_typed_evidence(
    extract: &UpdateProcedureExtract,
    focus_model: &UpdateProcedureFocusModel,
    document_label: &str,
) -> bool {
    let has_target_identity =
        update_procedure_text_target_identity_priority(&extract.block_text, focus_model) > 0
            || update_procedure_text_target_identity_priority(document_label, focus_model) > 0;
    if !has_target_identity {
        return false;
    }
    if !extract.has_setup_script_signature {
        return true;
    }
    extract.command_count > 0
        && extract
            .block_text
            .lines()
            .filter(|line| line_has_order_marker(line) && !line_has_command_signal(line))
            .take(2)
            .count()
            >= 2
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
    focus_model.subject_terms.intersection(&tokens).count().saturating_mul(8)
}

fn update_procedure_block_has_setup_script_signature(block: &[UpdateProcedureLine]) -> bool {
    let signature = block
        .iter()
        .filter(|line| line.has_command)
        .map(|line| command_token_values(strip_leading_order_marker(&line.text)))
        .filter(|tokens| !tokens.is_empty())
        .fold(UpdateProcedureSetupScriptSignature::default(), |signature, tokens| {
            signature.with_command_tokens(&tokens)
        });
    signature.is_complete()
}

#[derive(Default)]
struct UpdateProcedureSetupScriptSignature {
    has_external_artifact_materialization: bool,
    has_local_artifact_preparation: bool,
    has_local_artifact: bool,
    has_local_artifact_run: bool,
}

impl UpdateProcedureSetupScriptSignature {
    fn with_command_tokens(mut self, tokens: &[String]) -> Self {
        self.has_external_artifact_materialization |=
            command_tokens_have_external_artifact_materialization(tokens);
        let line_has_local_artifact =
            tokens.iter().any(|token| token_has_local_command_artifact(token));
        self.has_local_artifact |= line_has_local_artifact;
        self.has_local_artifact_preparation |= line_has_local_artifact
            && tokens.iter().any(|token| shellish_token_has_artifact_preparation_signal(token));
        self.has_local_artifact_run |=
            tokens.first().is_some_and(|token| shellish_token_is_path_command_start(token));
        self
    }

    fn is_complete(&self) -> bool {
        self.has_external_artifact_materialization
            && (self.has_local_artifact_run
                || (self.has_local_artifact_preparation && self.has_local_artifact))
    }
}

fn command_tokens_have_external_artifact_materialization(tokens: &[String]) -> bool {
    let has_external_artifact = tokens.iter().any(|token| token.contains("://"));
    let has_local_artifact =
        tokens.iter().skip(1).any(|token| token_has_local_command_artifact(token));
    has_external_artifact && has_local_artifact
}

fn token_has_local_command_artifact(token: &str) -> bool {
    let normalized = trim_command_token_decorations(token);
    if normalized.contains("://") {
        return false;
    }
    if let Some((_, value)) = normalized.split_once('=')
        && !value.is_empty()
    {
        return shellish_token_is_local_artifact(value);
    }
    if normalized.starts_with('-') || normalized.starts_with('+') {
        return false;
    }
    shellish_token_is_local_artifact(normalized)
}

fn trim_command_token_decorations(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ',' | ':')
            || command_token_char_is_invisible_format(ch)
    })
}

fn trim_command_boundary_token_decorations(token: &str) -> &str {
    trim_command_token_decorations(token).trim_end_matches(['.', ',', ';'])
}

fn command_token_char_is_invisible_format(ch: char) -> bool {
    matches!(
        ch,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
    )
}

fn command_token_values(command_line: &str) -> Vec<String> {
    command_line
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
        .collect()
}

fn structural_command_head_token(tokens: &[String]) -> Option<&str> {
    shellish_tokens_start_command(tokens).then(|| tokens.first().map(String::as_str)).flatten()
}

fn update_procedure_command_head(command_line: &str) -> Option<String> {
    if !procedure_line_has_command_start(command_line) {
        return None;
    }
    let tokens = command_token_values(strip_leading_order_marker(command_line));
    tokens.first().filter(|head| shellish_token_is_invocable_head(head)).map(ToString::to_string)
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

fn update_procedure_step_from_line(line: &UpdateProcedureLine) -> String {
    if !line.has_command || line_has_command_signal(&line.text) {
        return line.text.clone();
    }
    let command = update_procedure_normalized_step(&line.text);
    format!("`{command}`")
}

fn update_procedure_line_blocks(text: &str) -> Vec<Vec<UpdateProcedureLine>> {
    let mut blocks = Vec::new();
    let mut state = UpdateProcedureBlockBuilder::default();
    for raw_line in text.lines() {
        let segments = split_dense_procedure_segments(raw_line);
        if segments.is_empty() {
            state.push_segment(
                DenseProcedureSegment {
                    text: raw_line.trim().to_string(),
                    explicit_command_context: false,
                },
                &mut blocks,
            );
            continue;
        }
        for segment in segments {
            state.push_segment(segment, &mut blocks);
        }
    }
    state.finish(&mut blocks);
    blocks
}

#[derive(Default)]
struct UpdateProcedureBlockBuilder {
    current: Vec<UpdateProcedureLine>,
    preceding_line_opens_command_context: bool,
}

impl UpdateProcedureBlockBuilder {
    fn push_segment(
        &mut self,
        segment: DenseProcedureSegment,
        blocks: &mut Vec<Vec<UpdateProcedureLine>>,
    ) {
        let trimmed = segment.text.trim();
        if trimmed.is_empty() || line_is_markdown_atx_heading(trimmed) {
            self.finish(blocks);
            return;
        }
        let has_order_marker = line_has_order_marker(trimmed);
        let line = trimmed.trim_matches(['-', '*', '•', ' ']).trim();
        let has_contextual_command = !has_order_marker
            && (segment.explicit_command_context || self.preceding_line_opens_command_context)
            && contextual_procedure_command_shape(line);
        if line.chars().count() < 8 && !line_has_command_signal(line) && !has_contextual_command {
            return;
        }
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        let has_version =
            !line_looks_data_record(&normalized) && update_procedure_line_has_version(&normalized);
        let has_command = line_has_command_signal(&normalized) || has_contextual_command;
        if update_procedure_line_is_section_heading(
            &normalized,
            has_order_marker,
            has_version,
            has_command,
        ) && update_procedure_block_has_setup_script_signature(&self.current)
        {
            self.finish(blocks);
        }
        self.current.push(UpdateProcedureLine {
            has_version,
            has_order_marker,
            has_command,
            text: normalized,
        });
        self.preceding_line_opens_command_context = !has_command
            && self.current.last().is_some_and(|line| {
                line.text.ends_with(':') && line.text.split_whitespace().count() <= 16
            });
    }

    fn finish(&mut self, blocks: &mut Vec<Vec<UpdateProcedureLine>>) {
        if !self.current.is_empty() {
            blocks.push(std::mem::take(&mut self.current));
        }
        self.preceding_line_opens_command_context = false;
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DenseProcedureSegment {
    text: String,
    explicit_command_context: bool,
}

fn line_is_markdown_atx_heading(line: &str) -> bool {
    let trimmed = line.trim_start();
    let marker_len = trimmed.chars().take_while(|ch| *ch == '#').count();
    (1..=6).contains(&marker_len) && trimmed.chars().nth(marker_len).is_none_or(char::is_whitespace)
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
    split_dense_procedure_segments(line).into_iter().map(|segment| segment.text).collect()
}

fn split_dense_procedure_segments(line: &str) -> Vec<DenseProcedureSegment> {
    let mut segments = Vec::<DenseProcedureSegment>::new();
    let mut current = Vec::<String>::new();
    let mut current_has_explicit_command_context = false;
    let tokens = line
        .split_whitespace()
        .flat_map(|token| {
            split_concatenated_local_artifact_token(token)
                .into_iter()
                .enumerate()
                .map(|(segment_index, token)| (token, segment_index > 0))
        })
        .collect::<Vec<_>>();
    for (token_index, (token, follows_concatenated_artifact_boundary)) in
        tokens.iter().copied().enumerate()
    {
        let next_token = tokens.get(token_index + 1).map(|(token, _)| *token);
        if current_has_explicit_command_context
            && contextual_command_segment_is_complete(&current)
            && !contextual_command_token_continues(&current, token)
        {
            push_dense_procedure_segment(
                &mut segments,
                &mut current,
                current_has_explicit_command_context,
            );
            current_has_explicit_command_context = false;
        }
        let inside_square_delimiter = tokens_have_unclosed_square_delimiter(&current);
        let follows_inline_command_delimiter =
            !inside_square_delimiter && current_ends_with_inline_command_delimiter(&current);
        let is_command_start = !inside_square_delimiter
            && (follows_concatenated_artifact_boundary
                || token_is_inline_command_boundary_start(token, next_token, &current));
        let is_order_start = !inside_square_delimiter && token_is_inline_order_marker(token);
        if (is_command_start || is_order_start) && !current.is_empty() {
            push_dense_procedure_segment(
                &mut segments,
                &mut current,
                current_has_explicit_command_context,
            );
            current_has_explicit_command_context = is_command_start
                && (follows_inline_command_delimiter || follows_concatenated_artifact_boundary);
        }
        current.push(token.to_string());
    }
    if !current.is_empty() {
        push_dense_procedure_segment(
            &mut segments,
            &mut current,
            current_has_explicit_command_context,
        );
    }
    segments
        .into_iter()
        .map(|segment| {
            let text = segment.text.trim_matches(['-', '*', '•', ' ']).trim();
            DenseProcedureSegment {
                text: compact_bracketed_identifier_spacing(text),
                explicit_command_context: segment.explicit_command_context,
            }
        })
        .filter(|segment| !segment.text.is_empty())
        .collect()
}

fn push_dense_procedure_segment(
    segments: &mut Vec<DenseProcedureSegment>,
    current: &mut Vec<String>,
    explicit_command_context: bool,
) {
    if current.is_empty() {
        return;
    }
    segments.push(DenseProcedureSegment { text: current.join(" "), explicit_command_context });
    current.clear();
}

fn tokens_have_unclosed_square_delimiter(tokens: &[String]) -> bool {
    tokens.iter().flat_map(|token| token.chars()).fold(0usize, |depth, ch| match ch {
        '[' => depth.saturating_add(1),
        ']' => depth.saturating_sub(1),
        _ => depth,
    }) > 0
}

fn compact_bracketed_identifier_spacing(text: &str) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(text.len());
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index] != '[' {
            output.push(chars[index]);
            index += 1;
            continue;
        }
        let Some(relative_end) = chars[index + 1..].iter().position(|ch| *ch == ']') else {
            output.push(chars[index]);
            index += 1;
            continue;
        };
        let end = index + 1 + relative_end;
        let inner = &chars[index + 1..end];
        let inner_text = inner.iter().collect::<String>();
        let identifier = inner_text.trim();
        let identifier_shaped = identifier.chars().any(char::is_alphanumeric)
            && !identifier.chars().any(char::is_whitespace)
            && identifier.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-'));
        if !identifier_shaped {
            output.extend(chars[index..=end].iter());
            index = end + 1;
            continue;
        }
        output.push('[');
        output.push_str(identifier);
        output.push(']');
        index = end + 1;
    }
    output
}

fn token_is_inline_command_boundary_start(
    token: &str,
    next_token: Option<&str>,
    current: &[String],
) -> bool {
    let cleaned = token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';'));
    let normalized = trim_command_boundary_token_decorations(cleaned).to_ascii_lowercase();
    if !current.is_empty()
        && !current_ends_with_inline_command_delimiter(current)
        && !shellish_token_is_path_command_start(cleaned)
        && current_is_plain_invocation_with_structural_argument(current, &normalized)
    {
        return false;
    }
    if current_command_prepares_local_artifact(current)
        && token_has_local_command_artifact(&normalized)
    {
        return true;
    }
    if shellish_token_is_path_command_start(cleaned) {
        return path_token_starts_inline_command(current, cleaned);
    }
    if invocable_token_starts_inline_command(current, &normalized, next_token) {
        return true;
    }
    false
}

fn path_token_starts_inline_command(current: &[String], token: &str) -> bool {
    current.is_empty()
        || current_ends_with_formal_shell_delimiter(current)
        || current_ends_with_directory_shaped_path_before_executable_artifact(current, token)
}

fn invocable_token_starts_inline_command(
    current: &[String],
    normalized: &str,
    next_token: Option<&str>,
) -> bool {
    if current_ends_with_inline_command_delimiter(current)
        && shellish_token_is_invocable_head(normalized)
    {
        return true;
    }
    if current_starts_with_command(current)
        && current_command_has_external_materialization(current)
        && shellish_token_is_invocable_head(normalized)
    {
        return true;
    }
    if current_command_expects_structural_value(current)
        && token_has_local_command_artifact(normalized)
    {
        return false;
    }
    !current.is_empty()
        && shellish_token_is_invocable_head(normalized)
        && shellish_token_has_executable_name_shape(normalized)
        && next_token.is_some_and(|token| {
            let next = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
            shellish_token_is_command_argument_signal(&next)
        })
}

fn contextual_command_segment_is_complete(current: &[String]) -> bool {
    let tokens = current
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>();
    shellish_tokens_start_command(&tokens)
        || tokens.iter().any(|token| {
            shellish_token_has_executable_name_shape(token)
                || shellish_token_is_path_command_start(token)
        })
}

fn contextual_command_token_continues(current: &[String], token: &str) -> bool {
    let token = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
    if shellish_token_is_command_argument_signal(&token)
        || shellish_token_is_local_artifact(&token)
        || current_command_expects_structural_value(current)
    {
        return true;
    }
    let normalized = current
        .iter()
        .map(|value| trim_command_boundary_token_decorations(value).to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if normalized.len() < 2 {
        return true;
    }
    let Some(last) = normalized.last() else {
        return false;
    };
    !shellish_token_is_path_command_start(last)
        && shellish_token_has_executable_name_shape(last)
        && normalized.iter().take(normalized.len().saturating_sub(1)).all(|value| {
            shellish_token_is_invocable_head(value)
                && !shellish_token_has_executable_name_shape(value)
        })
}

fn current_ends_with_directory_shaped_path_before_executable_artifact(
    current: &[String],
    token: &str,
) -> bool {
    if shellish_token_file_artifact_name(token).is_none() {
        return false;
    }
    let Some(previous) = current
        .last()
        .map(|value| trim_command_boundary_token_decorations(value).to_ascii_lowercase())
    else {
        return false;
    };
    shellish_token_is_path_command_start(&previous)
        && shellish_token_file_artifact_name(&previous).is_none()
        && current_starts_with_command(current)
}

fn contextual_procedure_command_shape(line: &str) -> bool {
    let tokens = command_token_values(strip_leading_order_marker(line));
    let Some(head) = tokens.first() else {
        return false;
    };
    tokens.len() >= 2
        && tokens.len() <= 12
        && shellish_token_is_invocable_head(head)
        && (shellish_tokens_start_command(&tokens)
            || tokens.iter().all(|token| shellish_token_is_invocable_head(token)))
}

fn current_is_plain_invocation_with_structural_argument(current: &[String], token: &str) -> bool {
    let Some(head) = current
        .first()
        .map(|value| trim_command_boundary_token_decorations(value).to_ascii_lowercase())
    else {
        return false;
    };
    !shellish_token_is_path_command_start(&head)
        && !shellish_token_has_executable_name_shape(&head)
        && shellish_token_is_invocable_head(&head)
        && shellish_token_is_command_argument_signal(token)
}

fn current_ends_with_inline_command_delimiter(tokens: &[String]) -> bool {
    tokens.last().is_some_and(|token| {
        token
            .trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ','))
            .ends_with(':')
    })
}

fn current_ends_with_formal_shell_delimiter(tokens: &[String]) -> bool {
    tokens.last().is_some_and(|token| {
        let token = token.trim_matches(|ch: char| matches!(ch, '`' | '\'' | '"' | '(' | ')'));
        ["&&", "||", ";", "|", "&"].iter().any(|delimiter| token.ends_with(delimiter))
    })
}

fn current_starts_with_command(tokens: &[String]) -> bool {
    let normalized = tokens
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    shellish_tokens_start_command(&normalized)
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
    let command_tokens = if tokens.first().is_some_and(|token| token_is_inline_order_marker(token))
    {
        &tokens[1..]
    } else {
        tokens
    };
    let normalized = command_tokens
        .iter()
        .map(|token| trim_command_boundary_token_decorations(token).to_ascii_lowercase())
        .collect::<Vec<_>>();
    normalized.iter().any(|token| shellish_token_has_artifact_preparation_signal(token))
        && normalized.iter().skip(1).any(|token| token_has_local_command_artifact(token))
}

fn current_command_expects_structural_value(tokens: &[String]) -> bool {
    tokens.last().is_some_and(|token| {
        let token = trim_command_boundary_token_decorations(token).to_ascii_lowercase();
        if let Some((_, value)) = token.split_once('=') {
            return value.is_empty();
        }
        token.starts_with('-') || token.starts_with('+') || token.contains("+")
    })
}

fn line_has_command_signal(line: &str) -> bool {
    let trimmed = strip_leading_order_marker(line).trim();
    if compact_configuration_section_header(trimmed) {
        return false;
    }
    let code_delimited = trimmed.len() >= 2
        && trimmed.starts_with('`')
        && trimmed.ends_with('`')
        && !trimmed.trim_matches('`').trim().is_empty();
    let command = trimmed.trim_matches('`').trim();
    let tokens = command_token_values(command);
    let Some(head) = tokens.first().map(String::as_str) else {
        return false;
    };
    shellish_tokens_start_command(&tokens)
        || (code_delimited && shellish_token_is_invocable_head(head))
        || (shellish_token_has_executable_name_shape(head)
            && (tokens.len() == 1 || procedure_line_has_list_marker(line)))
}

fn compact_configuration_section_header(line: &str) -> bool {
    let Some(inner) = line.strip_prefix('[').and_then(|line| line.strip_suffix(']')) else {
        return false;
    };
    !inner.is_empty()
        && !inner.chars().any(char::is_whitespace)
        && inner.chars().all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn line_has_order_marker(line: &str) -> bool {
    procedure_line_has_list_marker(line)
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
        .split(['.', '-', '_'])
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
    strip_leading_numeric_order_marker(line)
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

    query_ir.targets_any(&[
        QueryTargetKind::Attribute,
        QueryTargetKind::BaseUrl,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Connection,
        QueryTargetKind::Credential,
        QueryTargetKind::Endpoint,
        QueryTargetKind::Entry,
        QueryTargetKind::EnvVar,
        QueryTargetKind::ErrorCode,
        QueryTargetKind::Event,
        QueryTargetKind::Field,
        QueryTargetKind::Flag,
        QueryTargetKind::Group,
        QueryTargetKind::Item,
        QueryTargetKind::Parameter,
        QueryTargetKind::Port,
        QueryTargetKind::Record,
        QueryTargetKind::Service,
        QueryTargetKind::State,
        QueryTargetKind::Status,
        QueryTargetKind::TableRow,
        QueryTargetKind::TableSummary,
        QueryTargetKind::Url,
        QueryTargetKind::Value,
    ])
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
    query_ir.targets(QueryTargetKind::TableRow) && query_ir.targets(QueryTargetKind::TableSummary)
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
    let label_terms = exact_label_terms(&field.document_label, 2);
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
    let token_score = exact_label_terms(trimmed, 2).len().min(5);
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
    extend_exact_label_terms(&mut terms, current_question_segment(question), 3);
    for entity in &query_ir.target_entities {
        extend_exact_label_terms(&mut terms, &entity.label, 2);
    }
    for literal in &query_ir.literal_constraints {
        extend_exact_label_terms(&mut terms, &literal.text, 2);
    }
    if let Some(comparison) = query_ir.comparison.as_ref() {
        if let Some(a) = comparison.a.as_deref() {
            extend_exact_label_terms(&mut terms, a, 2);
        }
        if let Some(b) = comparison.b.as_deref() {
            extend_exact_label_terms(&mut terms, b, 2);
        }
        extend_exact_label_terms(&mut terms, &comparison.dimension, 2);
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
            .flat_map(|segment| exact_label_terms(&segment, 2))
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
        extend_exact_label_terms(&mut terms, &document_focus.hint, 3);
    }
    for entity in &query_ir.target_entities {
        extend_exact_label_terms(&mut terms, &entity.label, 3);
    }
    for literal in &query_ir.literal_constraints {
        extend_exact_label_terms(&mut terms, &literal.text, 2);
    }
    if let Some(comparison) = query_ir.comparison.as_ref() {
        if let Some(a) = comparison.a.as_deref() {
            extend_exact_label_terms(&mut terms, a, 3);
        }
        if let Some(b) = comparison.b.as_deref() {
            extend_exact_label_terms(&mut terms, b, 3);
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
    exact_label_terms(&field.path, 2)
        .into_iter()
        .chain(exact_label_terms(&field.value, 2))
        .collect()
}

fn source_unit_field_root_term(field: &SourceUnitField) -> Option<String> {
    source_unit_path_segments(&field.path).into_iter().next()
}

fn structured_source_unit_field_overlap_score(
    field: &SourceUnitField,
    focus: &StructuredSourceUnitFocus,
) -> usize {
    let path_terms = exact_label_terms(&field.path, 2);
    let value_terms = exact_label_terms(&field.value, 2);
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

fn exact_label_terms(text: &str, min_token_chars: usize) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    extend_exact_label_terms(&mut terms, text, min_token_chars);
    terms
}

fn extend_exact_label_terms(terms: &mut BTreeSet<String>, text: &str, min_token_chars: usize) {
    for term in label_terms(text, min_token_chars) {
        if term.chars().count() >= min_token_chars {
            terms.insert(term);
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
    if !query_ir.targets_any(&[
        QueryTargetKind::Version,
        QueryTargetKind::Release,
        QueryTargetKind::Changelog,
    ]) {
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
        !before.is_some_and(is_version_token_continuation)
            && !after.is_some_and(is_version_token_continuation)
    })
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
}

pub(crate) fn build_ordered_source_slice_answer(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
    context_chunks: &[RuntimeMatchedChunk],
) -> Option<OrderedSourceSliceAnswer> {
    let units = source_slice_answer_units(query_ir, source_units, context_chunks);
    let answer = build_ordered_source_units_answer(query_ir, &units)?;
    let source_unit_ids = source_units.iter().map(|unit| unit.chunk_id).collect::<HashSet<_>>();
    let used_context_fallback = source_units.is_empty()
        || units.iter().any(|unit| !source_unit_ids.contains(&unit.chunk_id));
    Some(OrderedSourceSliceAnswer { answer, unit_count: units.len(), used_context_fallback })
}

fn source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
    context_chunks: &[RuntimeMatchedChunk],
) -> Vec<RuntimeMatchedChunk> {
    let latest_version_inventory = query_requests_latest_versions(query_ir);
    if query_ir.source_slice.is_none() && !latest_version_inventory {
        return Vec::new();
    }
    if !source_units.is_empty() && !latest_version_inventory {
        let mut units = source_units.to_vec();
        sort_source_slice_answer_units(query_ir, &mut units);
        let requested_count = super::source_slice_requested_count(query_ir).unwrap_or(units.len());
        if requested_count > 0 && units.len() > requested_count {
            units.truncate(requested_count);
        }
        return units;
    }
    if !latest_version_inventory {
        return Vec::new();
    }

    let requested_count = latest_source_slice_requested_count(query_ir);
    let mut units = source_units
        .iter()
        .filter(|unit| source_slice_answer_unit_evidence(query_ir, unit).is_some())
        .cloned()
        .collect::<Vec<_>>();
    let mut seen_chunk_ids = units.iter().map(|unit| unit.chunk_id).collect::<HashSet<_>>();
    units.extend(
        context_chunks
            .iter()
            .filter(|chunk| !is_source_profile_runtime_chunk(chunk))
            .filter(|chunk| chunk_supports_explicit_latest_version_inventory(chunk))
            .filter(|chunk| source_slice_answer_unit_evidence(query_ir, chunk).is_some())
            .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
            .cloned(),
    );
    sort_source_slice_answer_units(query_ir, &mut units);

    dedupe_latest_source_slice_answer_units(query_ir, &mut units);
    if requested_count > 0 && units.len() > requested_count {
        units.truncate(requested_count);
    }
    units
}

fn sort_source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    units: &mut [RuntimeMatchedChunk],
) {
    if query_requests_latest_versions(query_ir) {
        units.sort_by(|left, right| latest_source_slice_answer_unit_order(query_ir, left, right));
    } else {
        units
            .sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id));
    }
}

fn latest_source_slice_answer_unit_order(
    query_ir: &crate::domains::query_ir::QueryIR,
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    let left_is_attested = latest_source_slice_answer_unit_is_attested(left);
    let right_is_attested = latest_source_slice_answer_unit_is_attested(right);
    match (left_is_attested, right_is_attested) {
        (true, false) => return std::cmp::Ordering::Less,
        (false, true) => return std::cmp::Ordering::Greater,
        (true, true) | (false, false) => {}
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

    if left_is_attested && right_is_attested {
        let score_order = score_value(right.score).total_cmp(&score_value(left.score));
        if !score_order.is_eq() {
            return score_order;
        }
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
    version: Vec<u32>,
}

fn source_slice_answer_unit_evidence(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<LatestSourceSliceEvidence> {
    if query_requests_latest_versions(query_ir) {
        return explicit_latest_source_slice_answer_unit_version(chunk)
            .map(|version| LatestSourceSliceEvidence { version });
    }
    latest_source_slice_answer_unit_version(chunk)
        .map(|version| LatestSourceSliceEvidence { version })
}

fn explicit_latest_source_slice_answer_unit_version(
    chunk: &RuntimeMatchedChunk,
) -> Option<Vec<u32>> {
    latest_source_slice_answer_unit_version(chunk).or_else(|| {
        if !latest_source_slice_answer_unit_is_attested(chunk) {
            return None;
        }
        let source = parse_source_unit_text(&chunk.source_text);
        let excerpt = parse_source_unit_text(&chunk.excerpt);
        source
            .field("version")
            .and_then(extract_semver_like_version)
            .or_else(|| excerpt.field("version").and_then(extract_semver_like_version))
            .or_else(|| extract_release_context_version(&chunk.source_text))
            .or_else(|| extract_release_context_version(&chunk.excerpt))
    })
}

fn latest_source_slice_answer_unit_is_attested(chunk: &RuntimeMatchedChunk) -> bool {
    matches!(chunk.score_kind, RuntimeChunkScoreKind::LatestVersion)
        || (matches!(chunk.score_kind, RuntimeChunkScoreKind::SourceContext)
            && is_structured_source_unit_runtime_chunk(chunk))
}

fn chunk_supports_explicit_latest_version_inventory(chunk: &RuntimeMatchedChunk) -> bool {
    if latest_source_slice_answer_unit_is_attested(chunk) {
        return explicit_latest_source_slice_answer_unit_version(chunk).is_some();
    }
    match chunk.score_kind {
        RuntimeChunkScoreKind::DocumentIdentity
        | RuntimeChunkScoreKind::QueryIrFocus
        | RuntimeChunkScoreKind::SourceContext
        | RuntimeChunkScoreKind::Relevance => {
            latest_source_slice_answer_unit_version(chunk).is_some()
        }
        _ => false,
    }
}

fn dedupe_latest_source_slice_answer_units(
    query_ir: &crate::domains::query_ir::QueryIR,
    units: &mut Vec<RuntimeMatchedChunk>,
) {
    if query_requests_latest_versions(query_ir) {
        let mut seen_versions = HashSet::<(ReleaseSourceIdentity, Vec<u32>)>::new();
        let mut seen_revisions = HashSet::<Uuid>::new();
        units.retain(|unit| {
            if let Some(evidence) = source_slice_answer_unit_evidence(query_ir, unit) {
                let source = ReleaseSourceIdentity::new(unit.document_id, unit.revision_id);
                return seen_versions.insert((source, evidence.version));
            }
            seen_revisions.insert(unit.revision_id)
        });
        return;
    }

    let mut seen_revisions = HashSet::<Uuid>::new();
    units.retain(|unit| seen_revisions.insert(unit.revision_id));
}

fn score_value(score: Option<f32>) -> f32 {
    score.unwrap_or(f32::NEG_INFINITY)
}

fn build_ordered_source_units_answer(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
) -> Option<String> {
    let latest_version_inventory = query_requests_latest_versions(query_ir);
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
    let source_identities = units
        .iter()
        .map(|unit| ReleaseSourceIdentity::new(unit.document_id, unit.revision_id))
        .collect::<HashSet<_>>();
    let single_source = source_identities.len() == 1;
    let include_document_label = !single_source;
    let mut lines = vec![ordered_source_units_summary(
        &units,
        requested_count,
        single_source,
        &document_labels,
    )];
    lines.push(String::new());
    for (index, unit) in units.iter().enumerate() {
        lines.extend(render_ordered_source_unit(
            index,
            unit,
            latest_version_inventory,
            include_document_label,
        ));
    }
    Some(lines.join("\n"))
}

fn ordered_source_units_summary(
    units: &[RuntimeMatchedChunk],
    requested_count: usize,
    single_source: bool,
    document_labels: &BTreeSet<&str>,
) -> String {
    if !single_source {
        return format!("{}/{}", units.len(), requested_count);
    }
    let label = document_labels.iter().next().copied().unwrap_or("source");
    format!("`{label}` - {}/{}", units.len(), requested_count)
}

fn render_ordered_source_unit(
    index: usize,
    unit: &RuntimeMatchedChunk,
    latest_version_inventory: bool,
    include_document_label: bool,
) -> Vec<String> {
    let parsed = parse_source_unit_text(&unit.source_text);
    let mut heading_parts = Vec::new();
    if include_document_label {
        heading_parts.push(format!("source=`{}`", unit.document_label.trim()));
    }
    if let Some(heading) =
        latest_inventory_source_unit_heading(latest_version_inventory, include_document_label, unit)
    {
        heading_parts.push(format!("**{heading}**"));
    }
    if let Some(timestamp) = parsed.field("occurred_at") {
        heading_parts.push(format!("**{timestamp}**"));
    }
    if let Some(actor) = parsed
        .field("actor_label")
        .or_else(|| parsed.field("actor_id"))
        .or_else(|| parsed.field("actor_role"))
    {
        heading_parts.push(format!("`{actor}`"));
    } else if let Some(unit_id) = parsed.field("unit_id") {
        heading_parts.push(format!("`unit_id={unit_id}`"));
    }
    if heading_parts.is_empty() {
        heading_parts.push(format!("`ordinal={}`", unit.chunk_index));
    }
    let mut lines = vec![format!("{}. {}", index + 1, heading_parts.join(" - "))];
    let body = source_slice_unit_body_for_answer(latest_version_inventory, &parsed);
    if !body.is_empty() {
        lines.push(indent_source_unit_body(&body));
    }
    lines
}

fn latest_inventory_source_unit_heading(
    latest_version_inventory: bool,
    include_document_label: bool,
    unit: &RuntimeMatchedChunk,
) -> Option<String> {
    if !latest_version_inventory || include_document_label {
        return None;
    }
    if let Some(label) = compact_source_slice_inventory_line(&unit.document_label)
        && extract_semver_like_version(&label).is_some()
    {
        return Some(excerpt_for(&label, 160));
    }
    explicit_latest_source_slice_answer_unit_version(unit).map(|version| {
        format!("Version {}", version.iter().map(u32::to_string).collect::<Vec<_>>().join("."))
    })
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
    let mut selected =
        select_canonical_answer_chunks(question, query_ir, &filtered_chunks, &question_keywords);
    prepend_source_profile_chunks_for_coverage(query_ir, chunks, &mut selected);
    render_canonical_chunk_sections(question, query_ir, &filtered_chunks, &selected)
}

fn select_canonical_answer_chunks(
    question: &str,
    query_ir: &QueryIR,
    filtered_chunks: &[RuntimeMatchedChunk],
    question_keywords: &[String],
) -> Vec<RuntimeMatchedChunk> {
    let (max_total_chunks, max_chunks_per_document) = if query_ir.requests_source_coverage_context()
        || query_ir_needs_expanded_setup_evidence(question, query_ir, filtered_chunks)
    {
        (SOURCE_COVERAGE_MAX_TOTAL_CHUNKS, SOURCE_COVERAGE_MAX_CHUNKS_PER_DOCUMENT)
    } else {
        (super::MAX_CHUNKS_PER_DOCUMENT, super::MIN_CHUNKS_PER_DOCUMENT)
    };
    let mut selected = select_document_balanced_chunks(
        question,
        Some(query_ir),
        filtered_chunks,
        question_keywords,
        false,
        max_total_chunks,
        max_chunks_per_document,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    if selected.is_empty() {
        selected = filtered_chunks.iter().take(8).cloned().collect();
    }
    selected
}

fn prepend_source_profile_chunks_for_coverage(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    selected: &mut Vec<RuntimeMatchedChunk>,
) {
    if !query_ir.requests_source_coverage_context() {
        return;
    }
    let mut seen_chunk_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut source_profile_chunks = chunks
        .iter()
        .filter(|chunk| is_source_profile_runtime_chunk(chunk))
        .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    if !source_profile_chunks.is_empty() {
        source_profile_chunks.append(selected);
        *selected = source_profile_chunks;
    }
}

fn render_canonical_chunk_sections(
    question: &str,
    query_ir: &QueryIR,
    filtered_chunks: &[RuntimeMatchedChunk],
    selected: &[RuntimeMatchedChunk],
) -> String {
    let question_keywords = crate::services::query::planner::extract_keywords(question);
    let setup_install_anchor = focused_setup_install_anchor(question, query_ir, filtered_chunks);
    let anchor_chunk_id = setup_install_anchor.map(|chunk| chunk.chunk_id);
    let (source_profile_chunks, content_chunks): (Vec<_>, Vec<_>) = selected
        .iter()
        .filter(|chunk| Some(chunk.chunk_id) != anchor_chunk_id)
        .partition(|chunk| is_source_profile_runtime_chunk(chunk));
    let mut sections = Vec::new();
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
    } else if query_ir_has_typed_setup_anchor_focus(query_ir) {
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

fn query_ir_has_typed_setup_anchor_focus(query_ir: &QueryIR) -> bool {
    matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::RetrieveValue)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir_has_setup_configuration_target(query_ir)
        && (!query_ir.target_entities.is_empty() || !query_ir.literal_constraints.is_empty())
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
    query_ir.targets_any(&[
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Parameter,
        QueryTargetKind::Package,
    ])
}

fn query_ir_needs_expanded_short_technical_evidence(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    query_ir_has_typed_setup_anchor_focus(query_ir)
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
    if let Some(evidence) = graph_evidence_scope_and_excerpt(chunk, question_keywords) {
        return evidence;
    }
    if let Some(evidence) = source_unit_scope_and_excerpt(chunk, question_keywords) {
        return evidence;
    }
    if let Some(evidence) = code_block_scope_and_excerpt(chunk, question_keywords) {
        return evidence;
    }
    if let Some(excerpt) = preferred_structured_excerpt(chunk, question_keywords) {
        return excerpt;
    }
    ("excerpt", focused_chunk_excerpt(chunk, question_keywords))
}

fn graph_evidence_scope_and_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> Option<(&'static str, String)> {
    if chunk.score_kind != RuntimeChunkScoreKind::GraphEvidence {
        return None;
    }
    let source_text = chunk.source_text.trim();
    if source_text.is_empty() {
        return None;
    }
    let excerpt = bounded_focused_excerpt(
        source_text,
        question_keywords,
        STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS,
    );
    Some(("graph_evidence", excerpt))
}

fn source_unit_scope_and_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> Option<(&'static str, String)> {
    if !is_structured_source_unit_runtime_chunk(chunk) {
        return None;
    }
    let source_text = chunk.source_text.trim();
    if source_text.chars().count() <= STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS {
        return Some(("source_unit", source_text.to_string()));
    }
    let excerpt = focused_record_unit_excerpt(
        source_text,
        question_keywords,
        STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS,
    )
    .unwrap_or_else(|| {
        focused_excerpt_for(source_text, question_keywords, STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS)
    });
    Some((
        "source_unit",
        nonempty_excerpt_or_fallback(excerpt, source_text, STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS),
    ))
}

fn code_block_scope_and_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> Option<(&'static str, String)> {
    if chunk.chunk_kind.as_deref() != Some("code_block") {
        return None;
    }
    let source_text = repair_technical_layout_noise(&chunk.source_text);
    let excerpt = if source_text.chars().count() <= EVIDENCE_CODE_BLOCK_CHARS {
        source_text
    } else {
        structured_literal_excerpt_for(&source_text, question_keywords, EVIDENCE_CODE_BLOCK_CHARS)
            .unwrap_or_else(|| excerpt_for(&source_text, EVIDENCE_CODE_BLOCK_CHARS))
    };
    Some(("code_block", excerpt))
}

fn preferred_structured_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> Option<(&'static str, String)> {
    if let Some(excerpt) =
        salient_source_excerpt_for(&chunk.source_text, question_keywords, EVIDENCE_CODE_BLOCK_CHARS)
    {
        return Some(("salient_excerpt", excerpt));
    }
    if let Some(excerpt) = structured_literal_excerpt_for(
        &chunk.source_text,
        question_keywords,
        EVIDENCE_CODE_BLOCK_CHARS,
    ) {
        return Some(("structured_excerpt", excerpt));
    }
    command_dense_excerpt_for(&chunk.source_text, EVIDENCE_CODE_BLOCK_CHARS)
        .map(|excerpt| ("code_block", excerpt))
}

fn focused_chunk_excerpt(chunk: &RuntimeMatchedChunk, question_keywords: &[String]) -> String {
    let excerpt =
        focused_excerpt_for(&chunk.source_text, question_keywords, EVIDENCE_CHUNK_EXCERPT_CHARS);
    nonempty_excerpt_or_fallback(excerpt, &chunk.source_text, EVIDENCE_CHUNK_EXCERPT_CHARS)
}

fn bounded_focused_excerpt(
    source_text: &str,
    question_keywords: &[String],
    max_chars: usize,
) -> String {
    if source_text.chars().count() <= max_chars {
        return source_text.to_string();
    }
    let excerpt = focused_excerpt_for(source_text, question_keywords, max_chars);
    nonempty_excerpt_or_fallback(excerpt, source_text, max_chars)
}

fn nonempty_excerpt_or_fallback(excerpt: String, source_text: &str, max_chars: usize) -> String {
    if excerpt.trim().is_empty() { excerpt_for(source_text, max_chars) } else { excerpt }
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
            target_types: vec![QueryTargetKind::Record],
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
        ir.target_types = vec![QueryTargetKind::Release];
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
            target_types: vec![QueryTargetKind::Concept],
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
            target_types: vec![QueryTargetKind::Version],
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
            target_types: vec![QueryTargetKind::Procedure],
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
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Version];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: focus.to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        query_ir
    }

    #[test]
    fn typed_versioned_procedure_renders_exact_identity_formal_steps() {
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            concat!(
                "Sample Target lifecycle sequence:\n",
                "1. alpha-admin prepare --target=/srv/alpha\n",
                "2. alpha-admin switch --version=2.0.0\n",
                "3. alpha-admin verify --format=json",
            ),
        );
        chunk.document_label = "Sample Target lifecycle guide".to_string();

        let answer = build_update_procedure_sequence_answer(
            "opaque request text",
            &configure_update_focus_ir("Sample Target"),
            &[chunk],
        )
        .expect("typed exact identity and formal steps should produce an answer");

        assert!(answer.contains("alpha-admin prepare --target=/srv/alpha"));
        assert!(answer.contains("alpha-admin switch --version=2.0.0"));
        assert!(answer.contains("alpha-admin verify --format=json"));
    }

    #[test]
    fn typed_versioned_procedure_fails_closed_without_version_or_with_concept() {
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Target:\n1. alpha-admin prepare\n2. alpha-admin verify",
        );
        chunk.document_label = "Sample Target guide".to_string();
        let mut without_version = configure_update_focus_ir("Sample Target");
        without_version.target_types = vec![QueryTargetKind::Procedure];
        let mut concept = configure_update_focus_ir("Sample Target");
        concept.target_types.push(QueryTargetKind::Concept);

        assert!(
            build_update_procedure_sequence_answer(
                "opaque request text",
                &without_version,
                std::slice::from_ref(&chunk),
            )
            .is_none()
        );
        assert!(
            build_update_procedure_sequence_answer("opaque request text", &concept, &[chunk])
                .is_none()
        );
    }

    #[test]
    fn typed_versioned_procedure_requires_exact_identity() {
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Targets:\n1. alpha-admin prepare\n2. alpha-admin verify",
        );
        chunk.document_label = "Sample Targets guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "opaque request text",
                &configure_update_focus_ir("Sample Target"),
                &[chunk],
            )
            .is_none()
        );
    }

    #[test]
    fn typed_versioned_procedure_rejects_unbound_materialization_script() {
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            concat!(
                "Sample Target:\n",
                "1. curl https://example.invalid/runner.sh -o /tmp/runner.sh\n",
                "2. chmod +x /tmp/runner.sh\n",
                "3. /tmp/runner.sh",
            ),
        );
        chunk.document_label = "Sample Target guide".to_string();

        assert!(
            build_update_procedure_sequence_answer(
                "opaque request text",
                &configure_update_focus_ir("Sample Target"),
                &[chunk],
            )
            .is_none()
        );
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
    fn render_canonical_chunk_section_surfaces_setup_anchor_for_typed_setup_ir() {
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\nsample-runner --install sample-link\nSettings are defined in the file /opt/alpha/connector/connector.conf",
        );
        anchor.document_label = "Sample Subject admin guide".to_string();
        anchor.score = Some(1.0);
        let mut query_ir = configure_how_focus_ir("Sample Subject");
        query_ir.act = QueryAct::ConfigureHow;
        query_ir.confidence = 0.25;
        query_ir.target_types =
            vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Subject".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        query_ir.document_focus = None;

        let section =
            render_canonical_chunk_section("configure Sample Subject", &query_ir, &[anchor], false);

        assert!(section.contains("Setup install anchor"), "typed setup anchor must be rendered");
        assert!(
            section.contains("sample-runner --install sample-link"),
            "install command must remain in the prompt context"
        );
    }

    #[test]
    fn render_canonical_chunk_section_does_not_infer_setup_anchor_for_empty_ir() {
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "sample-runner --install sample-link\nfile /opt/alpha/connector/connector.conf",
        );
        anchor.document_label = "Sample Subject admin guide".to_string();
        let mut query_ir = configure_how_focus_ir("Sample Subject");
        query_ir.act = QueryAct::Describe;
        query_ir.target_types.clear();
        query_ir.target_entities.clear();
        query_ir.document_focus = None;
        query_ir.confidence = 0.2;

        let section =
            render_canonical_chunk_section("configure Sample Subject", &query_ir, &[anchor], false);

        assert!(!section.contains("Setup install anchor"));
    }

    #[test]
    fn render_canonical_chunk_section_expands_typed_short_technical_rows() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = configure_how_focus_ir("Subject Alpha setup");
        query_ir.act = QueryAct::ConfigureHow;
        query_ir.confidence = 0.25;
        query_ir.target_types =
            vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "QX".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
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
            "typed short-token structured rows below the old per-document cap must remain visible"
        );
    }

    #[test]
    fn render_canonical_chunk_section_keeps_late_setup_code_blocks() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = configure_how_focus_ir("Subject Alpha setup");
        query_ir.act = QueryAct::ConfigureHow;
        query_ir.confidence = 0.25;
        query_ir.target_types =
            vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::ConfigKey];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "QX".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
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
        query_ir.target_types = vec![QueryTargetKind::Service, QueryTargetKind::Port];
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
        query_ir.target_types = vec![QueryTargetKind::Group, QueryTargetKind::Flag];
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
        query_ir.target_types = vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary];

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
        query_ir.target_types = vec![QueryTargetKind::Parameter, QueryTargetKind::ErrorCode];
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
        query_ir.target_types = vec![QueryTargetKind::Event, QueryTargetKind::Credential];
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
        query_ir.target_types = vec![QueryTargetKind::Event, QueryTargetKind::Credential];
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
        query_ir.target_types = vec![QueryTargetKind::Event, QueryTargetKind::Credential];
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
        query_ir.target_types = vec![QueryTargetKind::Group, QueryTargetKind::State];
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
        query_ir.target_types = vec![QueryTargetKind::Item, QueryTargetKind::Value];
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
        query_ir.target_types = vec![QueryTargetKind::Group, QueryTargetKind::State];
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
        query_ir.target_types = vec![QueryTargetKind::Service, QueryTargetKind::Connection];

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
        query_ir.target_types = vec![QueryTargetKind::Entry, QueryTargetKind::Value];
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
        query_ir.target_types = vec![QueryTargetKind::Value];
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
        query_ir.target_types = vec![QueryTargetKind::Event];
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
        query_ir.target_types = vec![QueryTargetKind::Protocol, QueryTargetKind::Concept];
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
    fn deterministic_answer_yields_troubleshooting_lists_to_grounded_synthesis() {
        let mut query_ir = configure_update_focus_ir("Sample Incident");
        query_ir.target_types = vec![
            QueryTargetKind::Procedure,
            QueryTargetKind::Troubleshooting,
            QueryTargetKind::Remediation,
            QueryTargetKind::ErrorMessage,
        ];
        query_ir.literal_constraints = vec![crate::domains::query_ir::LiteralSpan {
            text: "sample operation was already completed".to_string(),
            kind: crate::domains::query_ir::LiteralKind::Other,
        }];
        let mut generic_runbook = evidence_chunk(
            1,
            Some("paragraph"),
            "Error: sample operation was already completed.\n\
             Sample Incident workflow:\n\
             1. Open the generic operations screen.\n\
             2. Review the current queue.\n\
             3. Continue the ordinary workflow.",
        );
        generic_runbook.document_label = "Sample operations guide".to_string();
        let evidence = CanonicalAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        };

        let answer = build_deterministic_grounded_answer(
            "What should I do when the error says \"sample operation was already completed\"?",
            &query_ir,
            &evidence,
            &[generic_runbook],
        );

        assert!(
            answer.is_none(),
            "typed troubleshooting must reach grounded remediation synthesis instead of any deterministic list shortcut: {answer:?}"
        );
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
        query_ir.target_types = vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary];
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
    fn structured_list_answer_yields_non_actionable_how_to_inventory_to_synthesis() {
        let mut query_ir = configure_how_focus_ir("Sample Connector");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Connector configuration:\n\
             1. [Main]\n\
             2. endpointUrl\n\
             3. Default value false",
        );
        chunk.document_label = "Sample Connector setup guide".to_string();

        let answer = build_structured_list_grounded_answer(
            "How do I configure Sample Connector?",
            &query_ir,
            &[chunk],
        );

        assert!(
            answer.is_none(),
            "a how-to inventory without two action-bearing steps must reach grounded synthesis: {answer:?}"
        );
    }

    #[test]
    fn structured_list_answer_keeps_actionable_how_to_steps() {
        let mut query_ir = configure_how_focus_ir("Sample Connector");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Connector setup:\n\
             1. Install the connector package.\n\
             2. Open the connector settings.\n\
             3. Save the configured mode.",
        );
        chunk.document_label = "Sample Connector setup guide".to_string();

        let answer = build_structured_list_grounded_answer(
            "How do I configure Sample Connector?",
            &query_ir,
            &[chunk],
        )
        .expect("actionable structured procedure");

        assert!(answer.contains("Install the connector package"), "{answer}");
        assert!(answer.contains("Open the connector settings"), "{answer}");
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
    fn dense_procedure_line_preserves_ordered_command_arguments() {
        for line in
            ["1. alpha-admin prepare --target=/srv/alpha", "2. alpha-admin switch --version=2.0.0"]
        {
            assert_eq!(split_dense_procedure_line(line), vec![line]);
        }
    }

    #[test]
    fn command_shape_does_not_depend_on_executable_spelling() {
        for executable in ["alpha-tool", "beta_tool", "./gamma-tool"] {
            assert!(line_has_command_signal(&format!(
                "{executable} --mode=strict /work/update-token"
            )));
        }
    }

    #[test]
    fn command_signal_rejects_configuration_sections_and_hyphenated_sentences() {
        assert!(!line_has_command_signal("[Sample2]"));
        assert!(!line_has_command_signal("QR-code is not performed automatically."));
        assert!(line_has_command_signal("sample-runner --configure connector"));
        assert!(line_has_command_signal("SampleRunner --configure connector"));
        assert!(line_has_command_signal("工具 --configure connector"));
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
        query_ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];

        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Connector configuration\n\
             sample-configure --target=sample-connector\n\
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
        query_ir.target_types = vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure];
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
        query_ir.target_types =
            vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::Parameter];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Connector".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];
        let mut chunk = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Connector configuration\n\
             Install the package with sample-install --package=sample-connector.\n\
             Configuration command sample-reconfigure --package=sample-connector.\n\
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

        assert!(answer.contains("sample-reconfigure --package=sample-connector"), "{answer}");
        assert!(answer.contains("/etc/sample/connector.conf"), "{answer}");
        assert!(answer.contains("[Main]"), "{answer}");
        assert!(answer.contains("sampleMerchantId"), "{answer}");
    }

    #[test]
    fn setup_configuration_command_literals_reject_mixed_script_prose_fragments() {
        let commands = extract_setup_configuration_command_literals(
            "QR-code text appears in the user interface.\n\
              QR-код для оплаты отображается на экране.\n\
              Configuration command sample-reconfigure --package=sample-connector.\n\
             Section [Main] contains sampleMerchantId.",
            8,
        );

        assert!(
            commands
                .iter()
                .any(|command| command == "sample-reconfigure --package=sample-connector")
        );
        assert!(!commands.iter().any(|command| command.contains("QR-код")), "{commands:?}");
        assert!(!commands.iter().any(|command| command.starts_with("Section ")), "{commands:?}");
    }

    #[test]
    fn setup_configuration_command_literals_require_formal_command_signals() {
        let ambiguous = extract_setup_configuration_command_literals(
            "address should be configured as https://localhost/api\n\
             package-name . sample-install package-name\n\
             sample-configure package-name",
            8,
        );
        assert!(ambiguous.is_empty(), "{ambiguous:?}");

        let commands = extract_setup_configuration_command_literals(
            "sample-install --package=package-name\n\
             sample-configure package=package-name",
            8,
        );

        assert_eq!(
            commands,
            vec!["sample-install --package=package-name", "sample-configure package=package-name",]
        );
    }

    #[test]
    fn setup_configuration_command_literals_reject_artifact_headed_text() {
        let commands = extract_setup_configuration_command_literals(
            "/work/sample/config.ini contains mode=strict\n\
             https://example.invalid/schema describes mode=strict\n\
             runner-tool --mode=strict",
            8,
        );

        assert_eq!(commands, vec!["runner-tool --mode=strict"]);
    }

    #[test]
    fn setup_configuration_anchor_answer_renders_beyond_four_variants() {
        let mut query_ir = configure_how_focus_ir("DeltaVariant");
        query_ir.target_types = vec![
            QueryTargetKind::ConfigurationFile,
            QueryTargetKind::Package,
            QueryTargetKind::Procedure,
        ];
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
                         sample-configure --target=delta-module-{index}\n\
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
        assert!(answer.contains("sample-configure --target=delta-module-5"), "{answer}");
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
            vec![QueryTargetKind::Document, QueryTargetKind::ConfigKey, QueryTargetKind::ErrorCode];
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
        query_ir.target_types = vec![
            QueryTargetKind::ConfigurationFile,
            QueryTargetKind::Package,
            QueryTargetKind::Procedure,
        ];
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
    fn setup_configuration_anchor_candidate_does_not_short_circuit_generic_procedure() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut focused = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Connector configuration\n\
             Settings are defined in /opt/subject/connector.conf in section [Main].",
        );
        focused.document_id = document_id;
        focused.revision_id = revision_id;
        focused.document_label = "Sample Connector setup guide".to_string();
        let mut parameter_row = evidence_chunk(
            2,
            Some("table_row"),
            "Sheet: Module settings | Row 12 | Name: endpoint | Type: URL | \
             Description: Service endpoint | Notes: Defaults to http://localhost",
        );
        parameter_row.document_id = document_id;
        parameter_row.revision_id = revision_id;
        parameter_row.document_label = focused.document_label.clone();
        let mut query_ir = configure_how_focus_ir("Sample Connector");
        query_ir.target_types = vec![QueryTargetKind::Procedure, QueryTargetKind::Concept];
        query_ir.target_entities = vec![crate::domains::query_ir::EntityMention {
            label: "Sample Connector".to_string(),
            role: crate::domains::query_ir::EntityRole::Subject,
        }];

        let candidate = build_setup_configuration_anchor_candidate(
            "How do I configure Sample Connector?",
            &query_ir,
            &[focused.clone(), parameter_row.clone()],
        )
        .expect("focused setup configuration candidate");

        assert!(
            !candidate
                .should_use_as_direct_answer(&query_ir, &[focused.clone(), parameter_row.clone()],),
            "a generic how-to needs ordered synthesis; parameter presence alone must not select the inventory renderer"
        );
        assert!(
            !candidate.should_use_as_preflight_answer(&query_ir, &[focused, parameter_row]),
            "canonical preflight must not reintroduce the same generic parameter dump"
        );
    }

    #[test]
    fn setup_configuration_anchor_candidate_yields_mixed_remediation_ir_to_synthesis() {
        let mut focused = evidence_chunk(
            1,
            Some("paragraph"),
            "Sample Incident configuration\n\
             sample-configure incident-handler\n\
             Settings are defined in /opt/sample/incident.conf in section [Main].",
        );
        focused.document_label = "Sample Incident setup guide".to_string();
        let mut query_ir = configure_how_focus_ir("Sample Incident");
        query_ir.target_types = vec![
            QueryTargetKind::Procedure,
            QueryTargetKind::Remediation,
            QueryTargetKind::ErrorMessage,
            QueryTargetKind::ConfigurationFile,
        ];

        let candidate = build_setup_configuration_anchor_candidate(
            "How do I resolve the Sample Incident error?",
            &query_ir,
            std::slice::from_ref(&focused),
        )
        .expect("mixed remediation/configuration candidate");

        assert!(!candidate.should_use_as_direct_answer(&query_ir, std::slice::from_ref(&focused)));
        assert!(
            !candidate.should_use_as_preflight_answer(&query_ir, std::slice::from_ref(&focused))
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
        let mut command =
            evidence_chunk(1, Some("paragraph"), "sample-reconfigure --target=delta-subject");
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
            QueryTargetKind::ConfigurationFile,
            QueryTargetKind::Parameter,
            QueryTargetKind::Procedure,
        ];

        let answer = build_setup_configuration_anchor_answer(
            "how to configure DeltaVariant?",
            &query_ir,
            &[parameters, command],
        )
        .expect("focused setup configuration answer");

        assert!(answer.contains("`sample-reconfigure --target=delta-subject`"), "{answer}");
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
        query_ir.target_types =
            vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::Parameter];
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
        query_ir.target_types = vec![QueryTargetKind::Parameter];

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
        query_ir.target_types = vec![QueryTargetKind::Service, QueryTargetKind::Port];

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
        ir.target_types =
            vec![QueryTargetKind::Artifact, QueryTargetKind::Procedure, QueryTargetKind::Version];
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
    fn setup_configuration_anchor_answer_skips_typed_release_procedure_without_config_target() {
        let mut ir = configure_how_focus_ir("Sample Target");
        ir.document_focus = None;
        ir.target_types =
            vec![QueryTargetKind::Procedure, QueryTargetKind::Concept, QueryTargetKind::Release];
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

    #[test]
    fn generic_procedure_target_is_not_a_lifecycle_signal() {
        let mut ir = configure_how_focus_ir("Sample Target");
        ir.target_types = vec![QueryTargetKind::Procedure];

        assert!(!question_requests_lifecycle_update_procedure("", &ir));
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
            target_types: vec![QueryTargetKind::Parameter],
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
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut second = source_unit(
            2,
            "[unit_id=b occurred_at=2026-01-02T00:00:00+00:00 actor_label=Assistant] second",
        );
        second.document_id = document_id;
        second.revision_id = revision_id;
        let mut first = source_unit(
            1,
            "[unit_id=a occurred_at=2026-01-01T00:00:00+00:00 actor_label=User] first",
        );
        first.document_id = document_id;
        first.revision_id = revision_id;
        let answer = build_ordered_source_units_answer(&source_slice_ir(2), &[second, first])
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
    fn latest_source_slice_answer_requires_typed_release_source_slice() {
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

        assert!(build_ordered_source_slice_answer(&ir, &[], &chunks).is_none());
    }

    #[test]
    fn latest_source_slice_answer_rejects_query_ir_focus_body_versions() {
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
        );

        assert!(answer.is_none());
    }

    #[test]
    fn latest_source_slice_answer_rejects_generic_source_context_body_versions() {
        let mut focused = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            80.0,
            "Sample Subject release history",
            "Version 1.0.3\ngeneric source-context detail",
        );
        focused.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::SourceContext;
        focused.excerpt = "Version 1.0.2\nfocused excerpt detail".to_string();

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[], &[focused]);

        assert!(answer.is_none());
    }

    #[test]
    fn latest_source_slice_answer_keeps_attested_lane_ahead_of_higher_noise_version() {
        let mut attested = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            80.0,
            "Sample Subject release history",
            "Version 3.0.0\nattested release detail",
        );
        attested.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;
        let mut compatibility_noise = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Sample Subject compatibility 99.0.0",
            "compatibility-only detail",
        );
        compatibility_noise.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(1),
            &[],
            &[compatibility_noise, attested],
        )
        .expect("attested latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("Version 3.0.0"), "{}", answer.answer);
        assert!(!answer.answer.contains("99.0.0"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_does_not_drop_attested_lane_for_a_larger_noise_family() {
        let mut attested = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            80.0,
            "Canonical release history",
            "Version 3.0.0\nattested release detail",
        );
        attested.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;
        let mut noise_99 = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Compatibility record 99.0.0",
            "compatibility-only detail",
        );
        noise_99.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
        let mut noise_98 = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            90.0,
            "Compatibility record 98.0.0",
            "compatibility-only detail",
        );
        noise_98.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(2),
            &[],
            &[noise_99, noise_98, attested],
        )
        .expect("attested latest source-slice answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.answer.contains("Version 3.0.0"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_keeps_typed_source_unit_ahead_of_a_larger_noise_family() {
        let mut typed_unit =
            source_unit(1, "Version 3.0.0\ncanonical structured source-unit detail");
        typed_unit.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::SourceContext;
        let mut noise_99 = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Compatibility record 99.0.0",
            "compatibility-only detail",
        );
        noise_99.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
        let mut noise_98 = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            90.0,
            "Compatibility record 98.0.0",
            "compatibility-only detail",
        );
        noise_98.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(2),
            &[typed_unit],
            &[noise_99, noise_98],
        )
        .expect("typed source-unit latest answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.answer.contains("Version 3.0.0"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_does_not_fill_from_unattested_source_units() {
        let mut unattested = source_unit(1, "Version 99.0.0\ncompatibility-only detail");
        unattested.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::QueryIrFocus;
        let mut attested = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            80.0,
            "Sample Subject release history",
            "Version 3.0.0\nattested release detail",
        );
        attested.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(2),
            &[unattested],
            &[attested],
        )
        .expect("attested latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("Version 3.0.0"), "{}", answer.answer);
        assert!(!answer.answer.contains("99.0.0"), "{}", answer.answer);
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
        let ir = latest_source_slice_ir(1);
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
    fn latest_source_slice_answer_uses_typed_source_context_unit_payload() {
        let mut unit = source_unit(
            1,
            "[unit_id=u-1 occurred_at=2026-01-02T00:00:00+00:00 actor_label=Recorder] ![preview](asset.png)\n\
             Version 1.0.2\n\
             - Added neutral evidence line\n\
             - Added second neutral evidence line",
        );
        unit.document_label = "Neutral record stream".to_string();
        unit.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::SourceContext;

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
    fn latest_source_slice_answer_orders_grounded_version_before_runtime_rank() {
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
        assert!(answer.answer.contains("unrelated-newer"), "{}", answer.answer);
        assert!(!answer.answer.contains("ranked-evidence"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_merges_newer_context_with_stale_source_units() {
        let mut stale = source_unit(9, "[unit_id=stale] Version 1.0.0\n- Stale neutral change");
        stale.document_label = "Neutral release history".to_string();
        let newer = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            100.0,
            "Version 3.0.0",
            "Version 3.0.0\n- Newest neutral change",
        );
        let middle = release_relevance_chunk(
            Uuid::now_v7(),
            Uuid::now_v7(),
            0,
            90.0,
            "Version 2.0.0",
            "Version 2.0.0\n- Middle neutral change",
        );

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(2),
            &[stale],
            &[middle, newer],
        )
        .expect("merged latest-version answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.used_context_fallback);
        let newest = answer.answer.find("3.0.0").expect("newest version");
        let middle = answer.answer.find("2.0.0").expect("middle version");
        assert!(newest < middle, "{}", answer.answer);
        assert!(!answer.answer.contains("1.0.0"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_dedupes_title_variants_for_same_typed_source() {
        let repeated_document_id = Uuid::now_v7();
        let repeated_revision_id = Uuid::now_v7();
        let chunks = vec![
            release_identity_chunk(
                repeated_document_id,
                repeated_revision_id,
                0,
                100.0,
                "Delta 9.0.5",
                "newest neutral change",
            ),
            release_identity_chunk(
                repeated_document_id,
                repeated_revision_id,
                0,
                99.0,
                "Delta 9.0.5 - Administration",
                "duplicate rendering of the newest neutral change",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                98.0,
                "Delta Suite 9.0.4",
                "second neutral change",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                97.0,
                "Delta Suite 9.0.3 - Delta Suite Administration",
                "third neutral change",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                96.0,
                "Delta Suite 9.0.2 - Delta Suite Administration",
                "fourth neutral change",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                95.0,
                "Delta Suite 9.0.1 - Delta Suite Administration",
                "fifth neutral change",
            ),
        ];

        let query_ir = latest_source_slice_ir(5);
        let answer = build_ordered_source_slice_answer(&query_ir, &[], &chunks)
            .expect("complete latest-version answer");
        let completion =
            AnswerCompletionContract::from_query_ir(&query_ir).evaluate(&answer.answer);

        assert_eq!(answer.unit_count, 5);
        assert!(completion.complete, "{completion:?}");
        assert!(answer.answer.contains("Delta Suite 9.0.1"), "{}", answer.answer);
        assert_eq!(answer.answer.matches("9.0.5").count(), 1, "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_keeps_generic_title_variants_from_distinct_sources() {
        let chunks = vec![
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                100.0,
                "Delta Suite 9.0.5",
                "first independent record",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                99.0,
                "Delta Suite 9.0.5 - Administration",
                "second independent record",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks)
            .expect("distinct-source latest-version answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.answer.contains("first independent record"), "{}", answer.answer);
        assert!(answer.answer.contains("second independent record"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_keeps_exact_titles_from_distinct_sources() {
        let chunks = vec![
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                100.0,
                "Delta Suite 9.0.5",
                "first exact-title record",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                99.0,
                "Delta Suite 9.0.5",
                "second exact-title record",
            ),
        ];

        let query_ir = latest_source_slice_ir(2);
        let answer = build_ordered_source_slice_answer(&query_ir, &[], &chunks)
            .expect("exact-title distinct-source latest-version answer");
        let completion =
            AnswerCompletionContract::from_query_ir(&query_ir).evaluate(&answer.answer);

        assert_eq!(answer.unit_count, 2);
        assert!(completion.complete, "{completion:?}");
        assert!(answer.answer.contains("first exact-title record"), "{}", answer.answer);
        assert!(answer.answer.contains("second exact-title record"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_renders_body_version_instead_of_internal_ordinal() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut release = release_relevance_chunk(
            document_id,
            revision_id,
            37,
            100.0,
            "Neutral release history",
            "Version 7.8.901\n- Added a neutral capability",
        );
        release.score_kind =
            crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[], &[release])
            .expect("latest source-slice answer");

        assert!(answer.answer.contains("7.8.901"), "{}", answer.answer);
        assert!(!answer.answer.contains("ordinal=37"), "{}", answer.answer);
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
            "Build 7.8.901 - Neutral stream",
            "Changed neutral behavior.",
        );

        let answer = build_ordered_source_slice_answer(
            &latest_source_slice_ir(1),
            &[],
            &[overview, release],
        )
        .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("Build 7.8.901"), "{}", answer.answer);
        assert!(answer.answer.contains("Changed neutral behavior"), "{}", answer.answer);
        assert!(!answer.answer.contains("Sample Product 5.0"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_preserves_version_header_payload() {
        let mut unit = source_unit(1, "[unit_id=u-1 version=1.0.2 change=neutral-header-evidence]");
        unit.document_label = "Neutral record stream".to_string();
        unit.score_kind = crate::services::query::execution::RuntimeChunkScoreKind::LatestVersion;

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[unit], &[])
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("version=1.0.2"), "{}", answer.answer);
        assert!(answer.answer.contains("change=neutral-header-evidence"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_preserves_single_document_version_label_heading() {
        let mut unit = source_unit(1, "[unit_id=u-1] Changed neutral behavior.");
        unit.document_label = "Build 7.8.901 - Neutral stream".to_string();

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(1), &[unit], &[])
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 1);
        assert!(answer.answer.contains("**Build 7.8.901 - Neutral stream**"));
        assert!(answer.answer.contains("Changed neutral behavior."));
    }

    #[test]
    fn latest_source_slice_answer_does_not_infer_dominance_from_exact_title_families() {
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
        assert!(answer.answer.contains("Beta Tool Release 9.0.0"));
        assert!(answer.answer.contains("Sample Subject Release 1.0.2"));
        assert!(!answer.answer.contains("Sample Subject Release 1.0.1"));
    }

    #[test]
    fn latest_source_slice_answer_does_not_infer_dominance_from_title_extensions() {
        let chunks = vec![
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Delta Suite 3.0.3",
                "desired newest",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Delta Suite 3.0.2 - Delta Suite Administration",
                "desired middle",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Delta Suite 3.0.1 - Delta Suite Administration",
                "desired oldest",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Omega Stack 99.0.2",
                "noise newest",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Omega Stack 99.0.1",
                "noise older",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(3), &[], &chunks)
            .expect("mixed latest-version answer");

        assert_eq!(answer.unit_count, 3);
        assert!(answer.answer.contains("Delta Suite 3.0.3"), "{}", answer.answer);
        assert!(answer.answer.contains("Omega Stack 99.0.2"), "{}", answer.answer);
        assert!(answer.answer.contains("Omega Stack 99.0.1"), "{}", answer.answer);
        assert!(!answer.answer.contains("Delta Suite 3.0.1"), "{}", answer.answer);
    }

    #[test]
    fn latest_source_slice_answer_orders_mixed_sources_by_release_version() {
        let chunks = vec![
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Legacy Stream Release 1.0.3",
                "legacy newest",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Legacy Stream Release 1.0.2",
                "legacy middle",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Legacy Stream Release 1.0.1",
                "legacy oldest",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Current Stream Release 9.0.2",
                "current newest",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Current Stream Release 9.0.1",
                "current older",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(2), &[], &chunks)
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.answer.contains("Current Stream Release 9.0.2"));
        assert!(answer.answer.contains("Current Stream Release 9.0.1"));
        assert!(!answer.answer.contains("Legacy Stream Release"));
    }

    #[test]
    fn latest_source_slice_answer_does_not_apply_title_family_filtering() {
        let mut chunks = (1..=6)
            .map(|version| {
                release_identity_chunk(
                    Uuid::now_v7(),
                    Uuid::now_v7(),
                    0,
                    10.0,
                    &format!("Legacy Stream Release 1.0.{version}"),
                    "legacy change",
                )
            })
            .collect::<Vec<_>>();
        chunks.extend((1..=4).map(|version| {
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                &format!("Current Stream Release 9.0.{version}"),
                "current change",
            )
        }));

        let answer = build_ordered_source_slice_answer(&latest_source_slice_ir(5), &[], &chunks)
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 5);
        for version in 1..=4 {
            assert!(
                answer.answer.contains(&format!("Current Stream Release 9.0.{version}")),
                "{}",
                answer.answer
            );
        }
        assert!(answer.answer.contains("Legacy Stream Release 1.0.6"), "{}", answer.answer);
        assert!(!answer.answer.contains("Legacy Stream Release 1.0.5"), "{}", answer.answer);
    }

    #[test]
    fn low_confidence_context_release_series_does_not_infer_latest_inventory() {
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

        assert!(build_ordered_source_slice_answer(&ir, &[], &chunks).is_none());
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

#[cfg(test)]
#[path = "answer_procedure_syntax_tests.rs"]
mod procedure_syntax_tests;
