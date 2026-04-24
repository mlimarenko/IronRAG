use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::{
    infra::arangodb::document_store::{
        KnowledgeDocumentRow, KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow,
    },
    services::query::latest_versions::{
        compare_version_desc, extract_semver_like_version, latest_version_family_key,
        question_requests_latest_versions, requested_latest_version_count,
        text_has_release_version_marker,
    },
    services::query::planner::{QueryIntentProfile, UnsupportedCapabilityIntent},
    shared::extraction::table_summary::parse_table_column_summary,
};

use super::endpoint_answer::{
    build_multi_document_endpoint_answer_from_facts, build_single_endpoint_answer_from_facts,
};
pub(crate) use super::focused_document_answer::build_focused_document_answer;
use super::port_answer::{build_port_and_protocol_answer_from_facts, build_port_answer_from_facts};
pub(crate) use super::role_answer::{
    build_multi_document_role_answer, extract_multi_document_role_clauses,
    role_clause_canonical_target,
};
use super::transport_answer::{
    build_graphql_absence_answer, build_transport_contract_comparison_answer,
};
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::retrieve::{excerpt_for, focused_excerpt_for};
use super::technical_answer::build_exact_technical_literal_answer;
use super::technical_literals::{
    question_mentions_pagination, select_document_balanced_chunks, technical_literal_focus_keywords,
};
use super::types::*;
use super::{
    build_table_row_grounded_answer, build_table_summary_grounded_answer,
    question_asks_table_aggregation,
};

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
When the context includes an Exact technical literals section, treat those literals as the highest-priority grounding for URLs, paths, parameter names, methods, ports, and status codes.\n\
Prefer exact literals extracted from documents over paraphrased graph summaries when both are present.\n\
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
    build_graphql_absence_answer(question, chunks)
        .or_else(|| build_transport_contract_comparison_answer(question, query_ir, chunks))
        .or_else(|| build_port_and_protocol_answer_from_facts(question, query_ir, evidence, chunks))
        .or_else(|| build_port_answer_from_facts(question, query_ir, evidence, chunks))
        .or_else(|| build_single_endpoint_answer_from_facts(question, query_ir, evidence, chunks))
        .or_else(|| {
            build_multi_document_endpoint_answer_from_facts(question, query_ir, evidence, chunks)
        })
        .or_else(|| build_exact_technical_literal_answer(question, query_ir, evidence, chunks))
}

pub(crate) fn build_deterministic_grounded_answer(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_table_summary_grounded_answer(question, chunks)
        .or_else(|| build_table_row_grounded_answer(question, Some(query_ir), chunks))
        .or_else(|| build_latest_version_grounded_answer(question, chunks))
        .or_else(|| build_focused_document_answer(question, chunks))
        .or_else(|| build_multi_document_role_answer(question, chunks))
        .or_else(|| build_deterministic_technical_answer(question, query_ir, evidence, chunks))
}

fn build_latest_version_grounded_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_requests_latest_versions(question) {
        return None;
    }
    let requested_count = requested_latest_version_count(question);
    let mut documents = chunks
        .iter()
        .filter_map(|chunk| {
            let label = chunk.document_label.trim();
            if !text_has_release_version_marker(label) {
                return None;
            }
            let version = extract_semver_like_version(label)?;
            Some(LatestVersionAnswerDocument {
                document_id: chunk.document_id,
                label: label.to_string(),
                family_key: latest_version_family_key(label),
                version,
                chunks: vec![chunk.clone()],
            })
        })
        .fold(HashMap::<Uuid, LatestVersionAnswerDocument>::new(), |mut acc, document| {
            acc.entry(document.document_id)
                .and_modify(|existing| existing.chunks.extend(document.chunks.clone()))
                .or_insert(document);
            acc
        })
        .into_values()
        .collect::<Vec<_>>();
    if documents.is_empty() {
        return None;
    }
    if requested_count > 1 {
        let family_sizes =
            documents.iter().fold(HashMap::<String, usize>::new(), |mut acc, document| {
                *acc.entry(document.family_key.clone()).or_default() += 1;
                acc
            });
        let top_two_counts = {
            let mut counts = family_sizes.values().copied().collect::<Vec<_>>();
            counts.sort_unstable_by(|left, right| right.cmp(left));
            counts
        };
        if let Some((family_key, family_count)) = family_sizes
            .iter()
            .max_by(|left, right| left.1.cmp(right.1).then_with(|| left.0.cmp(right.0)))
            .map(|(family_key, count)| (family_key.clone(), *count))
        {
            let runner_up = top_two_counts.get(1).copied().unwrap_or(0);
            if family_count >= requested_count && family_count > runner_up {
                documents.retain(|document| document.family_key == family_key);
            }
        }
    }
    documents.sort_by(|left, right| {
        compare_version_desc(&left.version, &right.version)
            .then_with(|| left.label.cmp(&right.label))
    });
    documents.dedup_by(|left, right| {
        left.version == right.version && left.label.eq_ignore_ascii_case(&right.label)
    });
    documents.truncate(requested_count);
    if documents.is_empty() {
        return None;
    }

    let mut rendered = Vec::new();
    let mut missing_change_lists = 0usize;
    for document in &mut documents {
        document.chunks.sort_by(|left, right| {
            left.chunk_index
                .cmp(&right.chunk_index)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        let version_text =
            document.version.iter().map(u32::to_string).collect::<Vec<_>>().join(".");
        rendered.push(format!("Версия {version_text}"));
        let changes = extract_latest_version_change_lines(&document.chunks);
        if changes.is_empty() {
            missing_change_lists += 1;
            rendered.push(
                "- Список изменений не попал в релизные фрагменты, которые вошли в текущий контекст."
                    .to_string(),
            );
        } else {
            rendered.extend(changes.into_iter().map(|line| format!("- {line}")));
        }
        rendered.push(String::new());
    }
    if rendered.last().is_some_and(|line| line.is_empty()) {
        rendered.pop();
    }
    if rendered.is_empty() {
        return None;
    }
    let body = rendered.join("\n");
    if missing_change_lists == 0 {
        Some(body)
    } else {
        Some(format!(
            "Ниже перечислены последние релизы, которые уверенно попали в контекст. Для части версий список изменений в текущих фрагментах неполный.\n\n{body}"
        ))
    }
}

#[derive(Clone)]
struct LatestVersionAnswerDocument {
    document_id: Uuid,
    label: String,
    family_key: String,
    version: Vec<u32>,
    chunks: Vec<RuntimeMatchedChunk>,
}

fn extract_latest_version_change_lines(chunks: &[RuntimeMatchedChunk]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut seen = HashSet::<String>::new();
    let mut in_change_section = false;
    for chunk in chunks {
        let text = repair_technical_layout_noise(&chunk.source_text);
        for raw_line in text.lines() {
            let compact = raw_line.split_whitespace().collect::<Vec<_>>().join(" ");
            if compact.is_empty() {
                continue;
            }
            let lower = compact.to_lowercase();
            if lower.contains("новые возможности")
                || lower.contains("список изменений")
                || lower.contains("история изменений")
            {
                in_change_section = true;
                continue;
            }
            if lower.starts_with("версия ") || lower.starts_with("version ") {
                continue;
            }
            if lower.contains("руководство администратора") {
                continue;
            }
            if !in_change_section && !looks_like_latest_version_change_line(&compact) {
                continue;
            }
            if looks_like_latest_version_section_heading(&compact) {
                continue;
            }
            let candidate = compact
                .trim_start_matches(|ch: char| matches!(ch, '-' | '*' | '•' | '·' | '–'))
                .trim();
            if candidate.starts_with('#') {
                continue;
            }
            if candidate.chars().count() < 8 {
                continue;
            }
            if seen.insert(candidate.to_string()) {
                lines.push(candidate.to_string());
            }
            if lines.len() >= 6 {
                return lines;
            }
        }
    }
    lines
}

fn looks_like_latest_version_change_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    line.starts_with('-')
        || line.starts_with('*')
        || line.starts_with('•')
        || lower.starts_with("добав")
        || lower.starts_with("реализ")
        || lower.starts_with("исправ")
        || lower.starts_with("обнов")
        || lower.starts_with("понижен")
        || lower.starts_with("начиная")
}

fn looks_like_latest_version_section_heading(line: &str) -> bool {
    matches!(
        line.to_lowercase().as_str(),
        "новые возможности" | "исправленные ошибки" | "исправления" | "доработки"
    )
}

pub(crate) fn build_missing_explicit_document_answer(
    question: &str,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Option<String> {
    let explicit_literals = super::explicit_document_reference_literals(question);
    if explicit_literals.is_empty() {
        return None;
    }

    let matched_document_ids = super::explicit_target_document_ids_from_values(
        question,
        document_index.values().flat_map(|document| {
            [
                document.file_name.as_deref(),
                document.title.as_deref(),
                Some(document.external_key.as_str()),
            ]
            .into_iter()
            .flatten()
            .map(move |value| (document.document_id, value))
        }),
    );
    if !matched_document_ids.is_empty() {
        return None;
    }

    let document_label = explicit_literals.first()?;
    Some(if question_prefers_russian(question) {
        format!("Документ `{document_label}` отсутствует в активной библиотеке.")
    } else {
        format!("The document `{document_label}` is not present in the active library.")
    })
}

pub(super) fn question_prefers_russian(question: &str) -> bool {
    question.chars().any(|character| matches!(character, 'А'..='я' | 'Ё' | 'ё'))
}

pub(crate) fn build_unsupported_capability_answer(
    intent_profile: &QueryIntentProfile,
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    match intent_profile.unsupported_capability {
        Some(UnsupportedCapabilityIntent::GraphQlApi) => {
            build_graphql_absence_answer(question, chunks)
        }
        None => None,
    }
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
    blocks: &[KnowledgeStructuredBlockRow],
    suppress_tabular_detail: bool,
) -> String {
    if suppress_tabular_detail && question_asks_table_aggregation(question) {
        return String::new();
    }
    if blocks.is_empty() {
        return String::new();
    }
    let mut lines = Vec::<String>::new();
    for block in blocks.iter().take(super::MAX_ANSWER_BLOCKS) {
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

pub(crate) fn render_canonical_chunk_section(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
    suppress_tabular_detail: bool,
) -> String {
    if suppress_tabular_detail && question_asks_table_aggregation(question) {
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
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let pagination_requested = question_mentions_pagination(question);
    let mut selected = select_document_balanced_chunks(
        question,
        Some(query_ir),
        &filtered_chunks,
        &question_keywords,
        pagination_requested,
        super::MAX_CHUNKS_PER_DOCUMENT,
        super::MIN_CHUNKS_PER_DOCUMENT,
    )
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    if selected.is_empty() {
        selected = filtered_chunks.into_iter().take(8).collect();
    }
    let question_keywords = crate::services::query::planner::extract_keywords(question);
    let lines = selected
        .iter()
        .map(|chunk| {
            let excerpt = focused_excerpt_for(&chunk.source_text, &question_keywords, 560);
            let excerpt = if excerpt.trim().is_empty() {
                excerpt_for(&chunk.source_text, 560)
            } else {
                excerpt
            };
            format!("- {}: {}", chunk.document_label, excerpt)
        })
        .collect::<Vec<_>>();
    format!("Selected chunk excerpts\n{}", lines.join("\n"))
}

#[cfg(test)]
#[path = "answer_document_label_tests.rs"]
mod document_label_tests;
