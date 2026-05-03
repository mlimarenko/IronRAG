use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::services::query::text_match::normalized_alnum_tokens;
use crate::{
    infra::arangodb::document_store::{
        KnowledgeDocumentRow, KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow,
    },
    shared::extraction::table_summary::parse_table_column_summary,
    shared::extraction::technical_facts::TechnicalFactKind,
};

use super::endpoint_answer::{
    build_multi_document_endpoint_answer_from_facts, build_single_endpoint_answer_from_facts,
};
pub(crate) use super::focused_document_answer::build_focused_document_answer;
use super::port_answer::{build_port_and_protocol_answer_from_facts, build_port_answer_from_facts};
use super::question_intent::{QuestionIntent, classify_question_or_ir_intents};
use super::transport_answer::build_transport_contract_comparison_answer;
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::retrieve::{excerpt_for, focused_excerpt_for};
use super::technical_answer::build_exact_technical_literal_answer;
use super::technical_literals::{
    extract_explicit_path_literals, extract_http_methods, extract_parameter_literals,
    extract_prefix_literals, extract_url_literals, question_mentions_pagination,
    select_document_balanced_chunks, technical_literal_focus_keywords,
};
use super::types::*;
use super::{
    build_table_row_grounded_answer, build_table_summary_grounded_answer,
    question_asks_table_aggregation,
};

const SOURCE_COVERAGE_MAX_TOTAL_CHUNKS: usize = 14;
const SOURCE_COVERAGE_MAX_CHUNKS_PER_DOCUMENT: usize = 8;
const EVIDENCE_CHUNK_EXCERPT_CHARS: usize = 560;
const STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS: usize = 4_000;

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
The context may include library summary facts, recent document metadata, document excerpts, graph entities, and graph relationships gathered across many documents.\n\
Silently synthesize across the available evidence instead of stopping after the first partial hit.\n\
When Context includes a Table summaries section for a tabular question, treat that section as the authoritative source for aggregate answers such as averages, min/max ranges, and most frequent values.\n\
Do not infer aggregate table answers from individual table rows, technical facts, or neighboring snippets when a Table summaries section is present.\n\
For questions about the latest documents, document inventory, readiness, counts, or pipeline state, answer from library summary and recent document metadata even when chunk excerpts alone are not enough.\n\
Combine metadata, grounded excerpts, and graph references before deciding that the answer is unavailable.\n\
Present the answer directly. Do not narrate the retrieval process and do not mention chunks, internal search steps, the library context, or source document names unless the user explicitly asks for sources, evidence, or document names.\n\
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
When the context includes a library summary, trust those summary counts and readiness facts over individual chunk snippets for totals and overall status.\n\
When Context includes AGGREGATE_PROFILE blocks, treat them as source-level aggregate metadata for counts, time ranges, formats, roles, and unit distribution.\n\
Treat EVIDENCE_CHUNK blocks as sampled excerpts. Do not make whole-source frequency, ranking, or coverage claims from EVIDENCE_CHUNK blocks unless an AGGREGATE_PROFILE block supports the claim.\n\
When Context includes COMPARISON_COVERAGE status=partial, compare only the covered operands and explicitly state which requested operands are not grounded in Context.\n\
When the context includes an Exact technical literals section, treat those literals as the highest-priority grounding for URLs, paths, parameter names, methods, ports, and status codes.\n\
Prefer exact literals extracted from documents over paraphrased graph summaries when both are present.\n\
When Context includes Retrieved graph evidence or graph-evidence blocks, treat their evidence text as direct source wording. If a graph-evidence block contains delimited row fields, preserve each requested field's own value and do not copy a neighboring field value into it.\n\
When source evidence contains exact labels, headings, table names, field values, quoted phrases, identifiers, or short rare phrases that directly answer the question, copy those source spellings verbatim at least once before adding any paraphrase.\n\
If the answer language differs from a source phrase that directly answers the question, keep that source phrase verbatim and explain around it in the answer language.\n\
For rare graph-evidence phrases, include the shortest complete source phrase or row field value that contains the requested terms; do not substitute synonyms, translated equivalents, or inflected variants for the evidence phrase.\n\
When the question names or implies a source, section, table, or evidence location and Context contains that label, include the exact label with the answer.\n\
For source-, section-, table-, or troubleshooting-specific questions, name the exact source title and nearest available heading, table label, or evidence label before the action or conclusion.\n\
For workflow, list, and procedural answers, direct document excerpts are normative. Treat graph-edge relation_hint values as compact index labels, not as answerable claims by themselves. When a graph edge includes evidence text, answer from that evidence wording and scope; do not turn the hinted target into an unconditional item, document, or requirement unless the evidence itself states that.\n\
When Exact technical literals are grouped by document, keep each literal attached to its document heading and do not mix endpoints, URLs, paths, or methods from different documents unless the question explicitly asks you to compare or combine them.\n\
When Exact technical literals include both Paths and Prefixes, treat Paths as operation endpoints and use Prefixes only for questions that explicitly ask for a base prefix or base URL.\n\
When a grouped document entry also includes a matched excerpt, use that excerpt to decide which literal answers the user's condition inside that document.\n\
When the question asks for URLs, endpoints, paths, parameter names, HTTP methods, ports, status codes, field names, or exact behavioral rules, copy those literals verbatim from Context.\n\
Wrap exact technical literals such as URLs, paths, parameter names, HTTP methods, ports, and status codes in backticks.\n\
Do not normalize, rename, translate, repair, shorten, or expand technical literals from Context.\n\
Do not combine parts from different snippets into a synthetic URL, endpoint, path, or rule.\n\
If a literal does not appear verbatim in Context, do not invent it; state that the exact value is not grounded in the active library.\n\
If nearby snippets describe different examples or operations, answer only from the snippet that directly matches the user's condition and ignore unrelated adjacent error payloads or examples.\n\
For definition questions, preserve concrete enumerations, examples, and listed categories from Context instead of collapsing them into a generic paraphrase.\n\
When context includes a document summary, use it to understand the document's purpose before answering.\n\
When Context includes a short title, report name, validation target, or formats-under-test line for the focused document, answer with that literal directly.\n\
When Context includes SOURCE_SLICE_UNIT blocks, treat them as the runtime's canonical ordered source slice for the question, not as sampled excerpts. For positional source-slice requests, enumerate the matching records visible in those blocks and do not refuse merely because the blocks are a bounded slice.\n\
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
    query_ir: &crate::domains::query_ir::QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_table_summary_grounded_answer(question, Some(query_ir), chunks)
        .or_else(|| build_table_row_grounded_answer(question, Some(query_ir), chunks))
        .or_else(|| build_focused_document_answer(question, query_ir, chunks))
        .or_else(|| build_deterministic_technical_answer(question, query_ir, evidence, chunks))
}

pub(crate) fn build_ordered_source_units_answer(
    query_ir: &crate::domains::query_ir::QueryIR,
    source_units: &[RuntimeMatchedChunk],
) -> Option<String> {
    query_ir.source_slice.as_ref()?;
    if source_units.is_empty() {
        return None;
    }

    let mut units = source_units.to_vec();
    units.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id));
    let requested_count = super::source_slice_requested_count(query_ir).unwrap_or(units.len());
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
            heading_parts.push(format!("`{}`", unit.document_label.trim()));
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

        let body = parsed.body.trim();
        if !body.is_empty() {
            lines.push(indent_source_unit_body(body));
        }
    }

    Some(lines.join("\n"))
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
    let mut lines = Vec::<String>::new();
    for block in ranked_blocks.into_iter().take(super::MAX_ANSWER_BLOCKS) {
        let label = if block.heading_trail.is_empty() {
            block.block_kind.clone()
        } else {
            format!("{} > {}", block.block_kind, block.heading_trail.join(" > "))
        };
        let excerpt = excerpt_for(&repair_technical_layout_noise(&block.normalized_text), 420);
        lines.push(format!("- {}: {}", label, excerpt));
    }
    format!("Prepared segments\n{}", lines.join("\n"))
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
    let pagination_requested = question_mentions_pagination(question);
    let (max_total_chunks, max_chunks_per_document) = if query_ir.requests_source_coverage_context()
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
        selected = filtered_chunks.into_iter().take(8).collect();
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
    let (source_profile_chunks, content_chunks): (Vec<_>, Vec<_>) =
        selected.iter().partition(|chunk| is_source_profile_runtime_chunk(chunk));
    let mut sections = Vec::<String>::new();
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

fn render_ordered_source_slice_unit_section(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let slice = query_ir.source_slice.as_ref()?;
    let mut source_profile_chunks =
        chunks.iter().filter(|chunk| is_source_profile_runtime_chunk(chunk)).collect::<Vec<_>>();
    let mut content_chunks =
        chunks.iter().filter(|chunk| !is_source_profile_runtime_chunk(chunk)).collect::<Vec<_>>();
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
        let excerpt = focused_excerpt_for(
            source_text,
            question_keywords,
            STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS,
        );
        let excerpt = if excerpt.trim().is_empty() {
            excerpt_for(source_text, STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS)
        } else {
            excerpt
        };
        return ("source_unit", excerpt);
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
        || chunk.source_text.lines().map(str::trim_start).any(|line| line.starts_with("[unit_id="))
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
        return ParsedSourceUnitText { fields: HashMap::new(), body: trimmed.to_string() };
    };
    let Some((header, body)) = rest.split_once(']') else {
        return ParsedSourceUnitText { fields: HashMap::new(), body: trimmed.to_string() };
    };
    let fields = header
        .split_whitespace()
        .filter_map(|token| {
            let (key, value) = token.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect::<HashMap<_, _>>();
    ParsedSourceUnitText { fields, body: body.trim().to_string() }
}

fn indent_source_unit_body(body: &str) -> String {
    body.lines().map(|line| format!("   {}", line)).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod source_unit_answer_tests {
    use uuid::Uuid;

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
            }),
            confidence: 0.9,
        }
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
    fn structured_source_unit_evidence_uses_extended_context() {
        let late_marker = "late-marker-structural-unit";
        let source_text = format!("[unit_id=a] {} {late_marker}", "content ".repeat(120));
        let chunk = evidence_chunk(7, None, &source_text);

        let lines = render_evidence_chunk_lines(&[&chunk], &[], "sampled");

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("scope=source_unit"));
        assert!(lines[0].contains(late_marker));
    }

    #[test]
    fn ordinary_evidence_chunks_remain_excerpt_bounded() {
        let late_marker = "late-marker-ordinary-evidence";
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
