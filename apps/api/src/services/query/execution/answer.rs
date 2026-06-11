use std::collections::{BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::services::query::text_match::normalized_alnum_tokens;
use crate::{
    domains::query_ir::{LiteralKind, QueryAct, QueryIR, QueryScope},
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
use super::question_intent::{
    QuestionIntent, canonical_target_type_tag, classify_question_or_ir_intents,
};
use super::transport_answer::build_transport_contract_comparison_answer;
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::retrieve::{chunk_is_setup_focus_package_path_anchor, excerpt_for, focused_excerpt_for};
use super::technical_answer::build_exact_technical_literal_answer;
use super::technical_literals::{
    extract_explicit_path_literals, extract_http_methods, extract_parameter_literals,
    extract_prefix_literals, extract_url_literals, select_document_balanced_chunks,
    technical_literal_focus_keywords,
};
use super::types::*;
use super::{
    build_table_row_grounded_answer, build_table_summary_grounded_answer,
    focus_token_overlap_count, query_ir_document_focus_tokens, question_asks_table_aggregation,
};
use crate::services::query::latest_versions::{
    compare_version_desc, extract_semver_like_version, latest_version_family_key,
    query_requests_latest_versions, requested_latest_version_count,
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
    build_table_summary_grounded_answer(question, Some(query_ir), chunks)
        .or_else(|| build_table_row_grounded_answer(question, Some(query_ir), chunks))
        .or_else(|| build_focused_document_answer(question, query_ir, chunks))
        .or_else(|| build_deterministic_technical_answer(question, query_ir, evidence, chunks))
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
    if query_ir.source_slice.is_none()
        && !explicit_latest_version_inventory
        && !inferred_latest_version_inventory
    {
        return Vec::new();
    }
    if !source_units.is_empty() {
        let mut units = source_units.to_vec();
        sort_source_slice_answer_units(query_ir, &mut units);
        return units;
    }
    if !explicit_latest_version_inventory && !inferred_latest_version_inventory {
        return Vec::new();
    }

    let requested_count = latest_source_slice_requested_count(query_ir);
    let mut units = context_chunks
        .iter()
        .filter(|chunk| !is_source_profile_runtime_chunk(chunk))
        .filter(|chunk| {
            if explicit_latest_version_inventory {
                matches!(
                    chunk.score_kind,
                    RuntimeChunkScoreKind::DocumentIdentity | RuntimeChunkScoreKind::LatestVersion
                )
            } else {
                latest_source_slice_answer_unit_version(chunk).is_some()
            }
        })
        .filter(|chunk| latest_source_slice_answer_unit_version(chunk).is_some())
        .cloned()
        .collect::<Vec<_>>();
    sort_source_slice_answer_units(query_ir, &mut units);

    let mut seen_revisions = HashSet::<Uuid>::new();
    units.retain(|unit| seen_revisions.insert(unit.revision_id));
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
        units.sort_by(latest_source_slice_answer_unit_order);
    } else {
        units
            .sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index, chunk.chunk_id));
    }
}

pub(crate) fn context_supports_latest_version_inventory(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
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
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
) -> std::cmp::Ordering {
    match (
        latest_source_slice_answer_unit_version(left),
        latest_source_slice_answer_unit_version(right),
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
    extract_semver_like_version(&chunk.document_label)
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

        let body = source_slice_unit_body_for_answer(latest_version_inventory, parsed.body.trim());
        if !body.is_empty() {
            lines.push(indent_source_unit_body(&body));
        }
    }

    Some(lines.join("\n"))
}

fn source_slice_unit_body_for_answer(latest_version_inventory: bool, body: &str) -> String {
    if latest_version_inventory {
        compact_source_slice_inventory_body(body)
    } else {
        body.trim().to_string()
    }
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
    // focused document's setup anchor (the chunk carrying both a package-install
    // command and a configuration path) in full, ahead of the sampled excerpts.
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
        .filter(|chunk| chunk_is_setup_focus_package_path_anchor(chunk))
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

    if chunk.chunk_kind.as_deref() == Some("code_block") {
        let source_text = repair_technical_layout_noise(&chunk.source_text);
        return ("code_block", excerpt_for(&source_text, EVIDENCE_CODE_BLOCK_CHARS));
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

    #[test]
    fn render_canonical_chunk_section_surfaces_setup_install_anchor_in_full() {
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "Module configuration\naptitude install alpha-connector\nSettings are defined in the file /opt/alpha/connector/connector.conf",
        );
        anchor.document_label = "Alpha Suite admin guide".to_string();
        anchor.score = Some(1.0);
        let mut dense_filler = evidence_chunk(
            2,
            Some("paragraph"),
            "Parameters: staticQrId secretKey sbpMerchantId currency qrCodeLifetime",
        );
        dense_filler.document_label = "Alpha Suite admin guide".to_string();
        dense_filler.score = Some(9_999.0);
        let chunks = vec![dense_filler, anchor];

        let section = render_canonical_chunk_section(
            "how to install and configure Alpha Suite",
            &configure_how_focus_ir("Alpha Suite"),
            &chunks,
            false,
        );

        assert!(section.contains("Setup install anchor"), "anchor section must be rendered");
        assert!(
            section.contains("aptitude install alpha-connector"),
            "install command must be present verbatim"
        );
    }

    #[test]
    fn render_canonical_chunk_section_skips_anchor_without_document_focus() {
        let mut anchor = evidence_chunk(
            1,
            Some("paragraph"),
            "aptitude install alpha-connector\nfile /opt/alpha/connector/connector.conf",
        );
        anchor.document_label = "Alpha Suite admin guide".to_string();
        let chunks = vec![anchor];
        let mut query_ir = configure_how_focus_ir("Alpha Suite");
        query_ir.document_focus = None;

        let section = render_canonical_chunk_section(
            "how to install and configure Alpha Suite",
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
            "Module configuration\naptitude install alpha-connector\nSettings are defined in the file /opt/alpha/connector/connector.conf",
        );
        anchor.document_label = "Alpha Suite admin guide".to_string();
        anchor.score = Some(1.0);
        let mut query_ir = configure_how_focus_ir("Alpha Suite");
        query_ir.act = QueryAct::Describe;
        query_ir.confidence = 0.25;
        query_ir.target_types.clear();
        query_ir.document_focus = None;

        let section =
            render_canonical_chunk_section("configure Alpha Suite", &query_ir, &[anchor], false);

        assert!(section.contains("Setup install anchor"), "fallback anchor must be rendered");
        assert!(
            section.contains("aptitude install alpha-connector"),
            "install command must remain in the prompt context"
        );
    }

    #[test]
    fn render_canonical_chunk_section_expands_low_confidence_short_technical_rows() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let mut query_ir = configure_how_focus_ir("Provider Alpha setup");
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
            chunk.document_label = "Provider Alpha setup".to_string();
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
        let mut query_ir = configure_how_focus_ir("Provider Alpha setup");
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
            document_label: "Provider Alpha setup".to_string(),
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
                hint: "Provider Alpha setup".to_string(),
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
                "| secretKey | string | Shared authorization secret |",
            ),
        ];

        let section = render_canonical_chunk_section(
            "How do I configure Provider Alpha parameters?",
            &configure_how_ir(),
            &chunks,
            false,
        );

        assert!(section.contains("apiUrl"));
        assert!(section.contains("retryTimeout"));
        assert!(section.contains("partnerId"));
        assert!(section.contains("secretKey"));
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
    fn latest_source_slice_answer_falls_back_to_ranked_context_chunks() {
        let older_id = Uuid::now_v7();
        let newer_id = Uuid::now_v7();
        let older_revision_id = Uuid::now_v7();
        let newer_revision_id = Uuid::now_v7();
        let long_body = (1..=20)
            .map(|index| format!("detail {index:02} for Alpha Suite"))
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
        assert!(answer.answer.contains("detail 01 for Alpha Suite"));
        assert!(!answer.answer.contains("detail 20 for Alpha Suite"));
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
                "Alpha Suite Release 1.0.1",
                "older detail",
            ),
            release_identity_chunk(
                newer_id,
                Uuid::now_v7(),
                0,
                10.0,
                "Alpha Suite Release 1.0.2",
                "newer detail",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&ir, &[], &chunks)
            .expect("latest source-slice answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.used_context_fallback);
        assert!(answer.answer.starts_with("2/2"));
        assert!(answer.answer.contains("source=`Alpha Suite Release 1.0.2`"));
        assert!(
            answer.answer.find("Alpha Suite Release 1.0.2").unwrap()
                < answer.answer.find("Alpha Suite Release 1.0.1").unwrap()
        );
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
            "Alpha Suite Release 1.0.2",
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
    fn latest_source_slice_answer_keeps_dominant_release_family() {
        let chunks = vec![
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Alpha Suite Release 1.0.2",
                "alpha newer",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Alpha Suite Release 1.0.1",
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
        assert!(answer.answer.contains("Alpha Suite Release 1.0.2"));
        assert!(answer.answer.contains("Alpha Suite Release 1.0.1"));
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
                "Alpha Suite Release 1.0.1",
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
                "Alpha Suite Release 1.0.2",
                "newer alpha detail",
            ),
        ];

        let answer = build_ordered_source_slice_answer(&ir, &[], &chunks)
            .expect("semver document series should infer latest inventory answer");

        assert_eq!(answer.unit_count, 2);
        assert!(answer.used_context_fallback);
        assert!(answer.answer.starts_with("2/2"));
        assert!(
            answer.answer.find("Alpha Suite Release 1.0.2").unwrap()
                < answer.answer.find("Alpha Suite Release 1.0.1").unwrap()
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
                "Alpha Suite Release 1.0.2",
                "exact detail",
            ),
            release_relevance_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                9.0,
                "Alpha Suite Release 1.0.1",
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
                "Alpha Suite Release 1.0.2",
                "newer detail",
            ),
            release_relevance_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                9.0,
                "Alpha Suite Release 1.0.1",
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
            "Alpha Suite Release 1.0.2",
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
            "[graph-evidence target=\"Version 1.2.3\"]\nVersion 1.2.3 - Alpha Suite\n\nChanges\n\n- Added indexed lookup by suffix\n- Added `pricedocid` to `documents.goodsitem`\n- Updated monitor counters"
                .to_string(),
            "[graph-evidence target=\"Alpha Suite --has_property--> 1.2.3\"]\nRelated guide\n- Adjacent payment note"
                .to_string(),
        ];

        let answer =
            build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &[], &lines)
                .expect("exact version graph answer");

        assert!(answer.contains("Version 1.2.3 - Alpha Suite"));
        assert!(answer.contains("Added indexed lookup by suffix"));
        assert!(answer.contains("`pricedocid`"));
        assert!(!answer.contains("Older item"));
        assert!(!answer.contains("Adjacent payment note"));
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
                "Alpha Suite Version 1.2.3",
                "# Version 1.2.3\n\n- Added operator audit\n- Added retry metric",
            ),
            release_identity_chunk(
                Uuid::now_v7(),
                Uuid::now_v7(),
                0,
                10.0,
                "Alpha Suite Version 1.2.2",
                "# Version 1.2.2\n\n- Older item",
            ),
        ];

        let answer =
            build_exact_version_change_summary_answer(&exact_version_ir("1.2.3"), &chunks, &[])
                .expect("exact version chunk answer");

        assert!(answer.contains("Alpha Suite Version 1.2.3"));
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
        let shared_label = "Alpha Suite Version 1.2.3";
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
