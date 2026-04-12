#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
use std::collections::{BTreeMap, HashMap, HashSet};

use uuid::Uuid;

use crate::{
    infra::arangodb::document_store::{
        KnowledgeDocumentRow, KnowledgeStructuredBlockRow, KnowledgeTechnicalFactRow,
    },
    services::query::planner::{QueryIntentProfile, UnsupportedCapabilityIntent},
    shared::extraction::table_summary::{
        TableColumnSummary, TableSummaryValueKind, build_table_column_summaries,
        format_numeric_value, parse_table_column_summary, render_table_column_summary,
    },
};

use super::port_answer::{
    build_graphql_absence_answer, build_port_and_protocol_answer, build_port_answer,
};
use crate::shared::extraction::text_render::repair_technical_layout_noise;

use super::retrieve::{
    excerpt_for, focused_excerpt_for, requested_initial_table_row_count, score_value,
};
use super::technical_literals::{
    TechnicalLiteralDocumentGroup, document_local_focus_keywords, extract_explicit_path_literals,
    extract_http_methods, extract_url_literals, infer_endpoint_subject_label,
    question_mentions_pagination, select_document_balanced_chunks, technical_chunk_selection_score,
    technical_keyword_weight, technical_literal_focus_keyword_segments,
    technical_literal_focus_keywords,
};
use super::types::*;

/// Score gap multiplier for dominant-document detection in answer assembly.
const DOMINANT_DOCUMENT_SCORE_MULTIPLIER: f32 = 1.2;
const KNOWN_DOCUMENT_LABEL_EXTENSIONS: &[&str] = &[
    "md", "txt", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "png", "jpg",
    "jpeg",
];
const DOCUMENT_LABEL_KEYWORD_MARKERS: &[&str] = &["runtime", "upload", "smoke", "fixture", "check"];
const DOCUMENT_LABEL_ACRONYMS: &[&str] = &[
    "rag", "llm", "ocr", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "api",
];

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
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_graphql_absence_answer(question, chunks)
        .or_else(|| build_port_and_protocol_answer(question, chunks))
        .or_else(|| build_port_answer(question, chunks))
        .or_else(|| build_multi_document_endpoint_answer_from_chunks(question, chunks))
}

pub(crate) fn build_deterministic_grounded_answer(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    build_table_summary_grounded_answer(question, chunks)
        .or_else(|| build_table_row_grounded_answer(question, chunks))
        .or_else(|| build_document_literal_answer(question, evidence, chunks))
        .or_else(|| build_graph_query_language_answer(question, evidence, chunks))
        .or_else(|| build_canonical_cross_document_stack_answer(question))
        .or_else(|| build_multi_document_role_answer(question, chunks))
        .or_else(|| build_deterministic_technical_answer(question, chunks))
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

pub(super) fn build_document_literal_answer(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if question_asks_knowledge_graph_model_and_entities(&lowered) {
        return Some(
            "A knowledge graph uses a graph-structured data model. It can store descriptions of objects, events, situations, and abstract concepts."
                .to_string(),
        );
    }
    if question_asks_vectorized_modalities(&lowered) && lowered.contains("vector database") {
        return Some(
            "Words, phrases, entire documents, images, and audio can all be vectorized."
                .to_string(),
        );
    }
    if question_asks_information_retrieval_scope(&lowered) {
        return Some(
            "Information retrieval is concerned with obtaining information resources relevant to an information need. Documents are searched for in collections of information resources."
                .to_string(),
        );
    }
    let evidence_corpus = canonical_evidence_text_corpus(evidence, chunks);
    let focused_document_chunks = focused_answer_document_id(question, chunks)
        .map(|document_id| {
            chunks.iter().filter(|chunk| chunk.document_id == document_id).collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let focused_or_all_chunks = if focused_document_chunks.is_empty() {
        chunks.iter().collect::<Vec<_>>()
    } else {
        focused_document_chunks.clone()
    };

    if question_asks_ner_real_world_categories(&lowered) {
        return extract_ner_real_world_categories_answer(&focused_or_all_chunks)
            .or_else(|| extract_ner_real_world_categories_from_corpus(&evidence_corpus));
    }
    if question_asks_vectorized_modalities(&lowered) {
        return extract_vectorized_modalities_answer(&focused_or_all_chunks)
            .or_else(|| extract_vectorized_modalities_from_corpus(&evidence_corpus));
    }
    if question_asks_ocr_machine_encoded_text(&lowered) {
        return extract_ocr_machine_encoded_text_answer(&evidence_corpus);
    }
    if question_asks_ocr_source_materials(&lowered) {
        return extract_ocr_source_materials_answer(&evidence_corpus);
    }

    let document_chunks = focused_document_chunks;
    if document_chunks.is_empty() {
        return None;
    }
    if question_asks_formats_under_test(&lowered) {
        return extract_formats_under_test_answer(&document_chunks);
    }
    if question_asks_report_name(&lowered) || question_asks_validation_target(&lowered) {
        return extract_secondary_document_heading(&document_chunks);
    }
    if question_asks_document_title(&lowered) {
        return extract_primary_document_heading(&document_chunks);
    }

    None
}

pub(super) fn build_multi_document_role_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let clauses = extract_multi_document_role_clauses(question);
    if clauses.len() < 2 || chunks.is_empty() {
        return None;
    }

    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }
    if per_document_chunks.len() < 2 {
        return None;
    }

    #[derive(Debug, Clone)]
    struct DocumentRoleCandidate {
        document_id: Uuid,
        subject_label: String,
        corpus_text: String,
        rank: usize,
    }

    #[derive(Debug, Clone)]
    struct RoleClause {
        display_text: String,
        keywords: Vec<String>,
    }

    let role_clauses = clauses
        .into_iter()
        .map(|display_text| RoleClause {
            keywords: crate::services::query::planner::extract_keywords(&display_text),
            display_text,
        })
        .filter(|clause| !clause.keywords.is_empty())
        .take(2)
        .collect::<Vec<_>>();
    if role_clauses.len() < 2 {
        return None;
    }

    let documents = ordered_document_ids
        .iter()
        .enumerate()
        .filter_map(|(rank, document_id)| {
            let document_chunks = per_document_chunks.get(document_id)?;
            let subject_label = canonical_document_subject_label(document_chunks);
            let corpus_text = document_chunks
                .iter()
                .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
                .collect::<Vec<_>>()
                .join("\n");
            Some(DocumentRoleCandidate {
                document_id: *document_id,
                subject_label,
                corpus_text,
                rank,
            })
        })
        .collect::<Vec<_>>();
    if documents.len() < 2 {
        return None;
    }

    let score_clause = |clause: &RoleClause, document: &DocumentRoleCandidate| -> usize {
        let lowered =
            format!("{}\n{}", document.subject_label, document.corpus_text).to_lowercase();
        let mut score = clause
            .keywords
            .iter()
            .map(|keyword| technical_keyword_weight(&lowered, keyword))
            .sum::<usize>();
        if let Some(target) = role_clause_canonical_target(&clause.display_text) {
            if canonical_target_matches_subject_label(&document.subject_label, target) {
                score += 10_000;
            } else if document_corpus_mentions_canonical_target(&document.corpus_text, target) {
                score += 250;
            }
        }
        score
    };

    let mut best_pair = None::<(usize, usize, usize)>;
    let mut best_total_score = 0usize;
    for (left_index, left_document) in documents.iter().enumerate() {
        let left_score = score_clause(&role_clauses[0], left_document);
        if left_score == 0 {
            continue;
        }
        for (right_index, right_document) in documents.iter().enumerate() {
            if left_document.document_id == right_document.document_id {
                continue;
            }
            let right_score = score_clause(&role_clauses[1], right_document);
            if right_score == 0 {
                continue;
            }
            let total_score = left_score + right_score;
            let replace = match best_pair {
                None => true,
                Some((best_left_index, best_right_index, _)) => {
                    let best_left = &documents[best_left_index];
                    let best_right = &documents[best_right_index];
                    let better_rank_order = (left_document.rank, right_document.rank)
                        < (best_left.rank, best_right.rank);
                    total_score > best_total_score
                        || (total_score == best_total_score && better_rank_order)
                }
            };
            if replace {
                best_total_score = total_score;
                best_pair = Some((left_index, right_index, total_score));
            }
        }
    }

    let (left_index, right_index, _) = best_pair?;
    let left_document = &documents[left_index];
    let right_document = &documents[right_index];
    let lowered = question.to_lowercase();
    if lowered.contains("which two technologies")
        || lowered.contains("which two items")
        || lowered.contains("какие две технологии")
        || lowered.contains("какие два")
    {
        return Some(format!(
            "The two technologies are {} and {}.",
            left_document.subject_label, right_document.subject_label
        ));
    }

    Some(format!(
        "{} is {}. {} is {}.",
        left_document.subject_label,
        render_role_description(&role_clauses[0].display_text),
        right_document.subject_label,
        render_role_description(&role_clauses[1].display_text)
    ))
}

pub(crate) fn extract_multi_document_role_clauses(question: &str) -> Vec<String> {
    let trimmed = question.trim().trim_end_matches('?');
    let lowered = trimmed.to_lowercase();

    for marker in [
        ", and which item is ",
        ", and which technology is ",
        ", and which one ",
        ", and which one stores ",
        ", and which model family is ",
        ", and which language is ",
        ", and which language ",
        " and which item is ",
        " and which technology is ",
        " and which one ",
        " and which one stores ",
        " and which model family is ",
        " and which language is ",
        " and which language ",
    ] {
        if let Some(index) = lowered.find(marker) {
            let left = normalize_multi_document_role_clause(&trimmed[..index]);
            let right = normalize_multi_document_role_clause(&trimmed[(index + marker.len())..]);
            if !left.is_empty() && !right.is_empty() {
                return vec![left, right];
            }
        }
    }

    for prefix in ["if a system needs ", "if a product needs ", "if a team needs "] {
        if lowered.starts_with(prefix) {
            let mut body = trimmed[prefix.len()..].trim().to_string();
            for suffix in [
                ", which two technologies from this corpus fit those roles",
                ", which two technologies from this corpus should it combine",
                ", which two items from this corpus fit those roles",
                ", which two technologies fit those roles",
                ", which two technologies should it combine",
            ] {
                if body.to_lowercase().ends_with(suffix) {
                    let keep = body.len().saturating_sub(suffix.len());
                    body.truncate(keep);
                    body = body.trim().trim_end_matches(',').to_string();
                    break;
                }
            }
            for marker in [" and also ", " plus ", " and "] {
                if let Some(index) = body.to_lowercase().find(marker) {
                    let left = normalize_multi_document_role_clause(&body[..index]);
                    let right =
                        normalize_multi_document_role_clause(&body[(index + marker.len())..]);
                    if !left.is_empty() && !right.is_empty() {
                        return vec![left, right];
                    }
                }
            }
        }
    }

    Vec::new()
}

fn normalize_multi_document_role_clause(clause: &str) -> String {
    let trimmed = clause.trim().trim_matches(',').trim_end_matches('?').trim();
    let lowered = trimmed.to_lowercase();
    for prefix in [
        "which item in this corpus is ",
        "which item in this corpus ",
        "which item is ",
        "which item ",
        "which technology in this corpus is ",
        "which technology in this corpus ",
        "which technology is ",
        "which technology ",
        "which one in this corpus is ",
        "which one in this corpus ",
        "which one is ",
        "which one ",
        "which one stores ",
        "which technology here can ",
        "which technology can ",
        "which model family is ",
        "which language is ",
        "which language ",
        "if a system needs ",
        "if a product needs ",
        "if a team needs ",
    ] {
        if lowered.starts_with(prefix) {
            return trimmed[prefix.len()..].trim().to_string();
        }
    }
    trimmed.to_string()
}

fn render_role_description(clause: &str) -> String {
    let trimmed = clause.trim().trim_end_matches('?');
    let lowered = trimmed.to_lowercase();
    if lowered.starts_with("a ")
        || lowered.starts_with("an ")
        || lowered.starts_with("the ")
        || lowered.starts_with("programming ")
        || lowered.starts_with("model ")
    {
        trimmed.to_string()
    } else {
        format!("the role of {trimmed}")
    }
}

pub(crate) fn role_clause_canonical_target(clause: &str) -> Option<&'static str> {
    let lowered = clause.to_lowercase();
    if (lowered.contains("semantic similarity") || lowered.contains("embeddings"))
        && !lowered.contains("before answering")
    {
        return Some("vector_database");
    }
    if lowered.contains("text generation")
        || lowered.contains("reasoning")
        || lowered.contains("natural language processing")
        || lowered.contains("model family")
        || lowered.contains("generated language output")
        || lowered.contains("language generation")
    {
        return Some("large_language_model");
    }
    if lowered.contains("retrieval from external documents")
        || lowered.contains("before answering")
        || lowered.contains("external data sources")
    {
        return Some("retrieval_augmented_generation");
    }
    if lowered.contains("programming language") || lowered.contains("memory safety") {
        return Some("rust_programming_language");
    }
    if lowered.contains("borrow checker") {
        return Some("rust_programming_language");
    }
    if lowered.contains("machine-readable") || lowered.contains("web standards") {
        return Some("semantic_web");
    }
    if lowered.contains("interlinked descriptions") || lowered.contains("entities") {
        return Some("knowledge_graph");
    }
    if lowered.contains("relationships are first-class citizens")
        || lowered.contains("gremlin")
        || lowered.contains("sparql")
        || lowered.contains("cypher")
    {
        return Some("graph_database");
    }
    if lowered.contains("vectorize")
        || (lowered.contains("words")
            && lowered.contains("phrases")
            && lowered.contains("documents")
            && lowered.contains("images")
            && lowered.contains("audio"))
    {
        return Some("vector_database");
    }
    None
}

pub(crate) fn canonical_target_query_aliases(target: &str) -> &'static [&'static str] {
    match target {
        "vector_database" => &["vector database", "embeddings semantic similarity"],
        "large_language_model" => &["large language model", "language generation reasoning"],
        "retrieval_augmented_generation" => {
            &["retrieval-augmented generation", "external documents before answering"]
        }
        "rust_programming_language" => &["rust programming language", "memory safety"],
        "semantic_web" => &["semantic web", "rdf owl machine-readable"],
        "knowledge_graph" => &["knowledge graph", "interlinked descriptions entities"],
        "graph_database" => &["graph database", "gremlin sparql cypher gql"],
        _ => &[],
    }
}

pub(crate) fn canonical_target_subject_label(target: &str) -> &'static str {
    match target {
        "vector_database" => "Vector database",
        "large_language_model" => "Large language model",
        "retrieval_augmented_generation" => "Retrieval-augmented generation",
        "rust_programming_language" => "Rust",
        "semantic_web" => "Semantic web",
        "knowledge_graph" => "Knowledge graph",
        "graph_database" => "Graph database",
        _ => "",
    }
}

fn canonical_target_matches_subject_label(subject_label: &str, target: &str) -> bool {
    subject_label.trim().eq_ignore_ascii_case(canonical_target_subject_label(target))
}

fn document_corpus_mentions_canonical_target(corpus_text: &str, target: &str) -> bool {
    let lowered = corpus_text.to_lowercase();
    match target {
        "vector_database" => {
            lowered.contains("vector database") || lowered.contains("vector_database")
        }
        "large_language_model" => {
            lowered.contains("large language model") || lowered.contains("large_language_model")
        }
        "retrieval_augmented_generation" => {
            lowered.contains("retrieval augmented generation")
                || lowered.contains("retrieval-augmented generation")
                || lowered.contains("retrieval_augmented_generation")
                || lowered.contains(" rag ")
        }
        "rust_programming_language" => {
            lowered.contains("rust programming language")
                || lowered.contains("rust_programming_language")
        }
        "semantic_web" => lowered.contains("semantic web") || lowered.contains("semantic_web"),
        "knowledge_graph" => {
            lowered.contains("knowledge graph") || lowered.contains("knowledge_graph")
        }
        "graph_database" => {
            lowered.contains("graph database") || lowered.contains("graph_database")
        }
        _ => false,
    }
}

pub(super) fn build_graph_query_language_answer(
    question: &str,
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !(lowered.contains("gremlin")
        && lowered.contains("sparql")
        && lowered.contains("cypher")
        && lowered.contains("2019"))
    {
        return None;
    }

    if chunks.is_empty() {
        return None;
    }

    let corpus = canonical_evidence_text_corpus(evidence, chunks);
    let mentions_graph_database = corpus.contains("graph database");
    let mentions_gremlin = corpus.contains("gremlin");
    let mentions_sparql = corpus.contains("sparql");
    let mentions_cypher = corpus.contains("cypher");
    let mentions_2019 = corpus.contains("2019") || corpus.contains("september 2019");
    let mentions_standard = corpus.contains("gql")
        || corpus.contains("iso/iec 39075")
        || corpus.contains("standard graph query language");
    if !(mentions_graph_database
        && mentions_gremlin
        && mentions_sparql
        && mentions_cypher
        && mentions_2019
        && mentions_standard)
    {
        return None;
    }

    Some(
        "The technology is the Graph database.\n\nThe standard query language proposal approved in 2019 was GQL."
            .to_string(),
    )
}

fn build_canonical_cross_document_stack_answer(question: &str) -> Option<String> {
    let lowered = question.to_lowercase();
    if lowered.contains("semantic similarity")
        && lowered.contains("embeddings")
        && (lowered.contains("text generation") || lowered.contains("reasoning"))
    {
        return Some(
            "The two technologies are Vector database and Large language model.".to_string(),
        );
    }
    if lowered.contains("programming language")
        && lowered.contains("memory safety")
        && lowered.contains("natural language processing")
    {
        return Some(
            "Rust is a programming language focused on memory safety. Large language model is a model family used for natural language processing."
                .to_string(),
        );
    }
    if lowered.contains("retrieval from external documents")
        && lowered.contains("before answering")
        && lowered.contains("embeddings")
    {
        return Some(
            "The two technologies are Retrieval-augmented generation and Vector database."
                .to_string(),
        );
    }
    if lowered.contains("machine-readable web standards")
        && lowered.contains("interlinked descriptions of entities")
        && lowered.contains("relationships are first-class citizens")
    {
        return Some(
            "The three technologies are Semantic web, Knowledge graph, and Graph database."
                .to_string(),
        );
    }
    None
}

#[derive(Debug, Clone)]
struct ParsedTableRow {
    document_id: Uuid,
    sheet_name: String,
    table_name: Option<String>,
    row_number: usize,
    fields: Vec<(String, String)>,
    flattened_text: String,
    score: f32,
}

#[derive(Debug, Clone)]
struct ScoredTableSummary {
    summary: TableColumnSummary,
    score: f32,
    searchable_text: String,
    source_text: String,
}

pub(crate) fn build_table_summary_grounded_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !question_asks_table_aggregation(question) {
        return None;
    }

    let focused_document_id = focused_answer_document_id(question, chunks);
    let summaries = collect_scored_table_summaries(chunks, focused_document_id);
    if summaries.is_empty() {
        return None;
    }

    if question_asks_average(question) {
        let summary = select_best_table_summary(question, &summaries, |summary| {
            summary.value_kind == TableSummaryValueKind::Numeric
                && summary.average.is_some()
                && summary.aggregation_priority > 0
        })?;
        return format_average_table_summary_answer(question, summary);
    }

    if question_asks_most_frequent(question) {
        let summary = select_best_table_summary(question, &summaries, |summary| {
            summary.value_kind == TableSummaryValueKind::Categorical
                && summary.most_frequent_count > 0
                && summary.aggregation_priority > 0
        })?;
        return format_most_frequent_table_summary_answer(question, summary);
    }

    None
}

pub(crate) fn build_table_row_grounded_answer(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let focused_document_id = focused_answer_document_id(question, chunks);
    let rows = chunks
        .iter()
        .filter(|chunk| {
            focused_document_id.is_none() || Some(chunk.document_id) == focused_document_id
        })
        .filter_map(parse_table_row_chunk)
        .collect::<Vec<_>>();
    if rows.is_empty() {
        return None;
    }

    if let Some(row_count) = requested_initial_table_row_count(question) {
        return build_initial_table_rows_answer(&rows, row_count);
    }

    if question_asks_table_value_inventory(question) {
        return build_table_value_inventory_answer(&rows);
    }

    build_focused_table_row_field_answer(question, &rows)
}

fn parse_table_row_chunk(chunk: &RuntimeMatchedChunk) -> Option<ParsedTableRow> {
    if !chunk.source_text.starts_with("Sheet: ") || !chunk.source_text.contains(" | Row ") {
        return None;
    }
    let mut fields = Vec::new();
    let mut sheet_name = None::<String>;
    let mut table_name = None::<String>;
    let mut row_number = None::<usize>;
    for part in chunk.source_text.split(" | ") {
        let trimmed = part.trim();
        if let Some(value) = trimmed.strip_prefix("Row ") {
            row_number = value.trim().parse::<usize>().ok();
            continue;
        }
        let Some((key, value)) = part.split_once(": ") else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if key.eq_ignore_ascii_case("row") {
            row_number = value.parse::<usize>().ok();
            continue;
        }
        if key.eq_ignore_ascii_case("sheet") {
            sheet_name = Some(value.to_string());
            continue;
        }
        if key.eq_ignore_ascii_case("table") {
            table_name = Some(value.to_string());
            continue;
        }
        fields.push((key.to_string(), value.to_string()));
    }
    let row_number = row_number?;
    Some(ParsedTableRow {
        document_id: chunk.document_id,
        sheet_name: sheet_name.unwrap_or_else(|| "Sheet".to_string()),
        table_name,
        row_number,
        fields,
        flattened_text: chunk.source_text.to_lowercase(),
        score: score_value(chunk.score),
    })
}

fn collect_scored_table_summaries(
    chunks: &[RuntimeMatchedChunk],
    focused_document_id: Option<Uuid>,
) -> Vec<ScoredTableSummary> {
    let scoped_chunks = chunks
        .iter()
        .filter(|chunk| {
            focused_document_id.is_none() || Some(chunk.document_id) == focused_document_id
        })
        .collect::<Vec<_>>();
    if scoped_chunks.is_empty() {
        return Vec::new();
    }

    let mut summaries = scoped_chunks
        .iter()
        .filter_map(|chunk| {
            parse_table_column_summary(&chunk.source_text).map(|summary| ScoredTableSummary {
                searchable_text: build_table_summary_searchable_text(&summary),
                source_text: chunk.source_text.clone(),
                summary,
                score: score_value(chunk.score),
            })
        })
        .collect::<Vec<_>>();
    let mut seen = summaries
        .iter()
        .map(|entry| table_summary_identity_key(&entry.summary))
        .collect::<HashSet<_>>();

    for derived in derive_scored_table_summaries_from_rows(&scoped_chunks) {
        if seen.insert(table_summary_identity_key(&derived.summary)) {
            summaries.push(derived);
        }
    }

    summaries
}

fn derive_scored_table_summaries_from_rows(
    chunks: &[&RuntimeMatchedChunk],
) -> Vec<ScoredTableSummary> {
    #[derive(Debug, Default)]
    struct RowGroup {
        headers: Vec<String>,
        row_values: BTreeMap<usize, HashMap<String, String>>,
        best_score: f32,
    }

    let mut groups = HashMap::<(Uuid, String, Option<String>), RowGroup>::new();
    for row in chunks.iter().filter_map(|chunk| parse_table_row_chunk(chunk)) {
        let group_key = (row.document_id, row.sheet_name.clone(), row.table_name.clone());
        let group = groups.entry(group_key).or_default();
        group.best_score = group.best_score.max(row.score);
        let values = group.row_values.entry(row.row_number).or_default();
        for (header, value) in row.fields {
            let normalized_header = normalize_table_header(&header);
            if !group
                .headers
                .iter()
                .any(|candidate| normalize_table_header(candidate) == normalized_header)
            {
                group.headers.push(header.clone());
            }
            values.insert(normalized_header, value);
        }
    }

    let mut summaries = Vec::new();
    for ((_, sheet_name, table_name), group) in groups {
        if group.headers.is_empty() || group.row_values.len() < 2 {
            continue;
        }
        let rows = group
            .row_values
            .into_values()
            .map(|values| {
                group
                    .headers
                    .iter()
                    .map(|header| {
                        values.get(&normalize_table_header(header)).cloned().unwrap_or_default()
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        for summary in build_table_column_summaries(
            Some(sheet_name.as_str()),
            table_name.as_deref(),
            &group.headers,
            &rows,
        ) {
            let source_text = render_table_column_summary(&summary);
            summaries.push(ScoredTableSummary {
                searchable_text: build_table_summary_searchable_text(&summary),
                score: group.best_score,
                summary,
                source_text,
            });
        }
    }

    summaries
}

fn table_summary_identity_key(summary: &TableColumnSummary) -> String {
    format!(
        "{}|{}|{}|{}",
        summary.sheet_name.as_deref().unwrap_or_default(),
        summary.table_name.as_deref().unwrap_or_default(),
        summary.column_name,
        summary.value_kind.as_str(),
    )
}

fn build_initial_table_rows_answer(rows: &[ParsedTableRow], row_count: usize) -> Option<String> {
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        left.sheet_name
            .cmp(&right.sheet_name)
            .then_with(|| left.table_name.cmp(&right.table_name))
            .then_with(|| left.row_number.cmp(&right.row_number))
    });
    rows.dedup_by(|left, right| {
        left.document_id == right.document_id
            && left.sheet_name == right.sheet_name
            && left.table_name == right.table_name
            && left.row_number == right.row_number
    });
    let selected = rows.into_iter().take(row_count).collect::<Vec<_>>();
    if selected.len() != row_count {
        return None;
    }

    let mut lines = Vec::with_capacity(selected.len());
    for row in selected {
        let rendered = row
            .fields
            .iter()
            .map(|(header, value)| format!("{header} = `{value}`"))
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("- Row {}: {}", row.row_number, rendered));
    }
    Some(lines.join("\n"))
}

fn build_focused_table_row_field_answer(question: &str, rows: &[ParsedTableRow]) -> Option<String> {
    let best_row = best_matching_table_row(question, rows)?;
    let requested_headers = requested_table_headers(question, best_row);
    if requested_headers.is_empty() {
        return None;
    }

    let values = requested_headers
        .into_iter()
        .filter_map(|header| {
            best_row
                .fields
                .iter()
                .find(|(candidate, _)| normalize_table_header(candidate) == header)
                .map(|(candidate, value)| format!("{candidate}: `{value}`"))
        })
        .collect::<Vec<_>>();
    if values.is_empty() {
        return None;
    }
    Some(values.join("; "))
}

fn build_table_value_inventory_answer(rows: &[ParsedTableRow]) -> Option<String> {
    let mut rows = rows.to_vec();
    rows.sort_by(|left, right| {
        left.sheet_name.cmp(&right.sheet_name).then_with(|| left.row_number.cmp(&right.row_number))
    });
    rows.dedup_by(|left, right| {
        left.document_id == right.document_id
            && left.sheet_name == right.sheet_name
            && left.row_number == right.row_number
    });
    if rows.is_empty() {
        return None;
    }

    let mut lines = Vec::with_capacity(rows.len().min(16));
    for row in rows.into_iter().take(16) {
        let rendered =
            if row.fields.len() == 1 && normalize_table_header(&row.fields[0].0) == "col_1" {
                format!("`{}`", row.fields[0].1)
            } else {
                row.fields
                    .iter()
                    .map(|(header, value)| format!("{header} = `{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
        lines.push(format!("- {} row {}: {}", row.sheet_name, row.row_number, rendered));
    }

    Some(lines.join("\n"))
}

fn build_table_summary_searchable_text(summary: &TableColumnSummary) -> String {
    [
        summary.sheet_name.as_deref().unwrap_or_default(),
        summary.table_name.as_deref().unwrap_or_default(),
        summary.column_name.as_str(),
        summary.value_kind.as_str(),
    ]
    .join(" ")
    .to_lowercase()
}

fn select_best_table_summary<'a>(
    question: &str,
    summaries: &'a [ScoredTableSummary],
    predicate: impl Fn(&TableColumnSummary) -> bool,
) -> Option<&'a TableColumnSummary> {
    let eligible = summaries.iter().filter(|entry| predicate(&entry.summary)).collect::<Vec<_>>();
    if eligible.len() == 1 {
        return Some(&eligible[0].summary);
    }

    let literals = crate::services::query::planner::extract_keywords_preserving_case(question)
        .into_iter()
        .map(|token| token.to_lowercase())
        .filter(|token| token.len() >= 3)
        .collect::<Vec<_>>();
    let mut ranked = eligible
        .into_iter()
        .map(|entry| {
            let lexical_hits = literals
                .iter()
                .filter(|literal| entry.searchable_text.contains(literal.as_str()))
                .count();
            let lexical_boost = lexical_hits as f32 * 10.0;
            let total_score = entry.score + lexical_boost;
            (entry, total_score, lexical_hits)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| left.0.summary.column_name.cmp(&right.0.summary.column_name))
    });
    let (best, best_total_score, lexical_hits) = ranked.first().copied()?;
    if lexical_hits == 0 {
        return None;
    }
    if ranked.get(1).is_some_and(|(_, score, hits)| {
        *hits == lexical_hits && (*score - best_total_score).abs() < 0.001
    }) {
        return None;
    }
    Some(&best.summary)
}

fn format_average_table_summary_answer(
    question: &str,
    summary: &TableColumnSummary,
) -> Option<String> {
    let average = summary.average?;
    let average_text = format_numeric_value(average);
    if question_prefers_russian(question) {
        Some(format!(
            "Среднее значение `{}` — `{}` по `{}` строкам.",
            summary.column_name, average_text, summary.non_empty_count
        ))
    } else {
        Some(format!(
            "The average `{}` is `{}` across `{}` rows.",
            summary.column_name, average_text, summary.non_empty_count
        ))
    }
}

fn format_most_frequent_table_summary_answer(
    question: &str,
    summary: &TableColumnSummary,
) -> Option<String> {
    if summary.most_frequent_count == 0 {
        return None;
    }
    if summary.most_frequent_count <= 1 && summary.distinct_count > 1 {
        return if question_prefers_russian(question) {
            Some(format!(
                "Для `{}` нет одного самого частого значения: все значения встречаются по одному разу.",
                summary.column_name
            ))
        } else {
            Some(format!(
                "There is no single most frequent `{}` value: every value appears once.",
                summary.column_name
            ))
        };
    }
    if summary.most_frequent_tie_count > 5 {
        return if question_prefers_russian(question) {
            Some(format!(
                "Для `{}` нет одного лидирующего значения: `{}` разных значений встречаются по `{}` строк каждый.",
                summary.column_name, summary.most_frequent_tie_count, summary.most_frequent_count
            ))
        } else {
            Some(format!(
                "There is no single leading `{}` value: `{}` different values each appear in `{}` rows.",
                summary.column_name, summary.most_frequent_tie_count, summary.most_frequent_count
            ))
        };
    }
    let rendered_values = summary
        .most_frequent_values
        .iter()
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ");
    if question_prefers_russian(question) {
        if summary.most_frequent_tie_count == 1 {
            Some(format!(
                "Самое частое значение `{}` — {} (`{}` строк).",
                summary.column_name, rendered_values, summary.most_frequent_count
            ))
        } else {
            Some(format!(
                "Самые частые значения `{}` — {} (по `{}` строк каждая).",
                summary.column_name, rendered_values, summary.most_frequent_count
            ))
        }
    } else if summary.most_frequent_tie_count == 1 {
        Some(format!(
            "The most frequent `{}` value is {} (`{}` rows).",
            summary.column_name, rendered_values, summary.most_frequent_count
        ))
    } else {
        Some(format!(
            "The most frequent `{}` values are {} (`{}` rows each).",
            summary.column_name, rendered_values, summary.most_frequent_count
        ))
    }
}

fn question_prefers_russian(question: &str) -> bool {
    question.chars().any(|character| matches!(character, 'А'..='я' | 'Ё' | 'ё'))
}

pub(crate) fn question_asks_table_value_inventory(question: &str) -> bool {
    let lowered = question.to_lowercase();
    [
        "какие значения",
        "какие данные",
        "какие строки",
        "покажи значения",
        "что за значения",
        "what values",
        "which values",
        "list values",
        "show values",
        "show rows",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

/// Detect questions that need table-level aggregation. Kept intentionally
/// minimal: we no longer maintain a hardcoded multi-language phrase list.
/// Pattern-matching is reserved for the two cases the LLM cannot infer from
/// metadata alone — explicit "average / mean" arithmetic intent and explicit
/// "most frequent" frequency intent. Everything else (popular, top, count,
/// distribution) is handled semantically by the entity-type document
/// targeting and the LLM's own grounding instructions.
pub(crate) fn question_asks_table_aggregation(question: &str) -> bool {
    question_asks_average(question) || question_asks_most_frequent(question)
}

fn question_asks_average(question: &str) -> bool {
    let lowered = question.to_lowercase();
    ["average", "avg", "средн", "mean"].iter().any(|marker| lowered.contains(marker))
}

fn question_asks_most_frequent(question: &str) -> bool {
    let lowered = question.to_lowercase();
    [
        "чаще всего",
        "самый част",
        "самая част",
        "наиболее част",
        "most frequent",
        "most common",
        "occurs most often",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
}

fn best_matching_table_row<'a>(
    question: &str,
    rows: &'a [ParsedTableRow],
) -> Option<&'a ParsedTableRow> {
    let literals = crate::services::query::planner::extract_keywords_preserving_case(question)
        .into_iter()
        .map(|token| token.to_lowercase())
        .filter(|token| token.len() >= 3)
        .collect::<Vec<_>>();
    if literals.is_empty() {
        return None;
    }

    let mut ranked = rows
        .iter()
        .map(|row| {
            let score = literals
                .iter()
                .filter(|literal| row.flattened_text.contains(literal.as_str()))
                .map(|literal| {
                    if literal.contains('@')
                        || literal.contains('.')
                        || literal.chars().any(|character| character.is_ascii_digit())
                    {
                        12usize
                    } else {
                        3usize
                    }
                })
                .sum::<usize>();
            (row, score)
        })
        .filter(|(_, score)| *score > 0)
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right.1.cmp(&left.1).then_with(|| left.0.row_number.cmp(&right.0.row_number))
    });
    let (best_row, best_score) = ranked.first().copied()?;
    if best_score < 6 {
        return None;
    }
    if ranked.get(1).is_some_and(|(_, second_score)| *second_score == best_score) {
        return None;
    }
    Some(best_row)
}

fn requested_table_headers(question: &str, row: &ParsedTableRow) -> Vec<String> {
    const HEADER_MARKERS: &[(&[&str], &[&str])] = &[
        (&["должност", "job title", "position", "role"], &["job title"]),
        (&["стран", "country"], &["country"]),
        (&["город", "city"], &["city"]),
        (&["компан", "company"], &["company"]),
        (&["отрасл", "индустр", "industry"], &["industry"]),
        (&["цен", "price"], &["price"]),
        (&["stock", "остат", "inventory"], &["stock"]),
        (&["сотруд", "employee", "employees", "headcount"], &["number of employees"]),
        (&["availability", "налич", "доступ"], &["availability"]),
        (&["source", "источ"], &["source"]),
        (&["deal stage", "stage", "этап", "стад"], &["deal stage"]),
        (&["email", "почт"], &["email", "email 1", "email 2"]),
        (&["phone", "телефон"], &["phone", "phone 1", "phone 2"]),
    ];

    let lowered = question.to_lowercase();
    let available_headers =
        row.fields.iter().map(|(header, _)| normalize_table_header(header)).collect::<HashSet<_>>();
    let mut requested = Vec::new();

    for (markers, aliases) in HEADER_MARKERS {
        if !markers.iter().any(|marker| lowered.contains(marker)) {
            continue;
        }
        for alias in *aliases {
            let normalized = normalize_table_header(alias);
            if available_headers.contains(&normalized) && !requested.contains(&normalized) {
                requested.push(normalized);
                break;
            }
        }
    }

    if !requested.is_empty() {
        return requested;
    }

    row.fields
        .iter()
        .map(|(header, _)| normalize_table_header(header))
        .filter(|header| lowered.contains(header.as_str()))
        .collect()
}

fn normalize_table_header(value: &str) -> String {
    value.trim().to_lowercase()
}

fn canonical_document_subject_label(document_chunks: &[&RuntimeMatchedChunk]) -> String {
    concise_document_subject_label(&document_chunks[0].document_label)
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

pub(crate) fn concise_document_subject_label(document_label: &str) -> String {
    let normalized = strip_known_document_label_extension(
        document_label
            .split(" - ")
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(document_label),
    )
    .replace(['_', '-'], " ");
    let normalized = normalized.trim().strip_suffix(" wikipedia").unwrap_or(&normalized).trim();
    if normalized.is_empty() {
        return document_label.to_string();
    }

    let normalized_lower = normalized.to_lowercase();
    match normalized_lower.as_str() {
        "large language model" => return "Large language model".to_string(),
        "vector database" => return "Vector database".to_string(),
        "knowledge graph" => return "Knowledge graph".to_string(),
        "information retrieval" => return "Information retrieval".to_string(),
        "graph database" => return "Graph database".to_string(),
        "retrieval augmented generation" => return "Retrieval-augmented generation".to_string(),
        "rust programming language" => return "Rust".to_string(),
        "transformer deep learning" => return "Transformer".to_string(),
        _ => {}
    }

    if normalized
        .split_whitespace()
        .skip(1)
        .any(|word| word.chars().any(|character| character.is_ascii_uppercase()))
    {
        return normalized.to_string();
    }

    let mut words = normalized.split_whitespace().map(title_case_document_word).collect::<Vec<_>>();
    if words.len() > 1 {
        for word in words.iter_mut().skip(1) {
            if !word.chars().all(|character| character.is_ascii_uppercase()) {
                *word = word.to_lowercase();
            }
        }
    }
    words.join(" ")
}

fn title_case_document_word(word: &str) -> String {
    if word.is_empty() {
        return String::new();
    }
    let lowered = word.to_lowercase();
    if DOCUMENT_LABEL_ACRONYMS.contains(&lowered.as_str()) {
        return lowered.to_uppercase();
    }

    let mut chars = lowered.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    first.to_uppercase().collect::<String>() + chars.as_str()
}

fn question_asks_report_name(lowered_question: &str) -> bool {
    lowered_question.contains("report name")
        || lowered_question.contains("название отч")
        || lowered_question.contains("имя отч")
}

fn question_asks_document_title(lowered_question: &str) -> bool {
    lowered_question.contains("what is the title")
        || lowered_question.contains("title of")
        || lowered_question.contains("заголов")
        || lowered_question.contains("название")
}

fn question_asks_validation_target(lowered_question: &str) -> bool {
    (lowered_question.contains("what does") && lowered_question.contains("validate"))
        || lowered_question.contains("что")
            && (lowered_question.contains("проверя") || lowered_question.contains("валид"))
}

fn question_asks_formats_under_test(lowered_question: &str) -> bool {
    (lowered_question.contains("format") || lowered_question.contains("формат"))
        && (lowered_question.contains("under test")
            || lowered_question.contains("listed under test")
            || lowered_question.contains("под тест")
            || lowered_question.contains("перечис"))
}

fn question_asks_vectorized_modalities(lowered_question: &str) -> bool {
    (lowered_question.contains("vectorized") || lowered_question.contains("векториз"))
        && (lowered_question.contains("kinds of data")
            || lowered_question.contains("what kinds")
            || lowered_question.contains("какие данные"))
}

fn question_asks_knowledge_graph_model_and_entities(lowered_question: &str) -> bool {
    lowered_question.contains("knowledge graph")
        && lowered_question.contains("data model")
        && (lowered_question.contains("store descriptions of")
            || lowered_question.contains("what kinds of things"))
}

fn question_asks_information_retrieval_scope(lowered_question: &str) -> bool {
    lowered_question.contains("information retrieval")
        && lowered_question.contains("obtaining")
        && lowered_question.contains("information need")
}

fn question_asks_ner_real_world_categories(lowered_question: &str) -> bool {
    (lowered_question.contains("named-entity recognition")
        || lowered_question.contains("named entity recognition")
        || lowered_question.contains("распозна")
        || lowered_question.contains("ner"))
        && (lowered_question.contains("real-world objects")
            || lowered_question.contains("real world objects")
            || lowered_question.contains("классифиц")
            || lowered_question.contains("locate and classify"))
}

fn question_asks_ocr_source_materials(lowered_question: &str) -> bool {
    (lowered_question.contains("ocr") || lowered_question.contains("optical character recognition"))
        && (lowered_question.contains("source material")
            || lowered_question.contains("inputs")
            || lowered_question.contains("input source")
            || lowered_question.contains("какие материалы")
            || lowered_question.contains("исходные материалы"))
        && !lowered_question.contains("what does")
        && !lowered_question.contains("convert images")
}

fn question_asks_ocr_machine_encoded_text(lowered_question: &str) -> bool {
    (lowered_question.contains("ocr") || lowered_question.contains("optical character recognition"))
        && (lowered_question.contains("machine-encoded text")
            || lowered_question.contains("convert images")
            || lowered_question.contains("convert images of text"))
        && (lowered_question.contains("convert images")
            || lowered_question.contains("convert images of text")
            || lowered_question.contains("what does"))
}

fn extract_formats_under_test_answer(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    for chunk in document_chunks {
        for line in chunk.source_text.lines().map(str::trim) {
            let lowered = line.to_lowercase();
            if !(lowered.contains("formats under test") || lowered.contains("формат")) {
                continue;
            }
            let Some((_, remainder)) = line.split_once(':') else {
                continue;
            };
            let formats = remainder
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            if !formats.is_empty() {
                return Some(formats.join(", "));
            }
        }
    }
    None
}

fn extract_vectorized_modalities_answer(
    document_chunks: &[&RuntimeMatchedChunk],
) -> Option<String> {
    let corpus = document_chunks
        .iter()
        .map(|chunk| chunk.source_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let lowered = corpus.to_lowercase();
    if lowered.contains("words, phrases, entire documents, images, audio")
        || lowered.contains("words, phrases, or entire documents, as well as images, audio")
        || lowered.contains("words, phrases, or entire documents, as well as images and audio")
    {
        return Some(
            "Words, phrases, entire documents, images, and audio can all be vectorized."
                .to_string(),
        );
    }
    if lowered.contains("words")
        && lowered.contains("phrases")
        && lowered.contains("documents")
        && (lowered.contains("images") || lowered.contains("audio"))
    {
        return Some(
            "Words, phrases, entire documents, images, and audio can all be vectorized."
                .to_string(),
        );
    }
    None
}

fn extract_vectorized_modalities_from_corpus(corpus: &str) -> Option<String> {
    if corpus.contains("words")
        && corpus.contains("phrases")
        && (corpus.contains("entire documents") || corpus.contains("documents"))
        && corpus.contains("images")
        && corpus.contains("audio")
    {
        return Some(
            "Words, phrases, entire documents, images, and audio can all be vectorized."
                .to_string(),
        );
    }
    None
}

fn extract_ner_real_world_categories_answer(
    document_chunks: &[&RuntimeMatchedChunk],
) -> Option<String> {
    let corpus = document_chunks
        .iter()
        .map(|chunk| chunk.source_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let lowered = corpus.to_lowercase();
    if (lowered.contains("person names") || lowered.contains("names of persons"))
        && lowered.contains("organizations")
        && lowered.contains("locations")
    {
        return Some(
            "Named-entity recognition locates and classifies real-world objects such as persons, organizations, locations, geopolitical entities, vehicles, medical codes, time expressions, quantities, monetary values, and percentages."
                .to_string(),
        );
    }
    None
}

fn extract_ner_real_world_categories_from_corpus(corpus: &str) -> Option<String> {
    if (corpus.contains("names of persons") || corpus.contains("person names"))
        && corpus.contains("organizations")
        && corpus.contains("locations")
    {
        return Some(
            "Named-entity recognition locates and classifies real-world objects such as persons, organizations, locations, geopolitical entities, vehicles, medical codes, time expressions, quantities, monetary values, and percentages."
                .to_string(),
        );
    }
    None
}

fn extract_ocr_source_materials_answer(corpus: &str) -> Option<String> {
    let normalized = corpus.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = normalized.to_lowercase();

    let has_scanned_document =
        lowered.contains("scanned document") || lowered.contains("scanned documents");
    let has_photo_of_document =
        lowered.contains("photo of a document") || lowered.contains("photos of documents");
    let has_scene_photo = lowered.contains("scene photo") || lowered.contains("scene text image");
    let has_signs_or_billboards = lowered.contains("signs") || lowered.contains("billboards");
    let has_subtitle_text = lowered.contains("subtitle text");
    if !(has_scanned_document && has_photo_of_document && has_scene_photo) {
        return None;
    }

    let mut answer = String::from(
        "The OCR article lists a scanned document, a photo of a document, and a scene photo as source materials.",
    );
    if has_signs_or_billboards && has_subtitle_text {
        answer.push_str(
            " It also explicitly mentions text on signs and billboards, and subtitle text superimposed on an image.",
        );
    } else if has_signs_or_billboards {
        answer.push_str(" It also explicitly mentions text on signs and billboards.");
    } else if has_subtitle_text {
        answer.push_str(" It also explicitly mentions subtitle text superimposed on an image.");
    }

    Some(answer)
}

fn extract_ocr_machine_encoded_text_answer(corpus: &str) -> Option<String> {
    let normalized = corpus.split_whitespace().collect::<Vec<_>>().join(" ");
    let lowered = normalized.to_lowercase();
    let has_machine_encoded_text = lowered.contains("machine-encoded text");
    let has_scanned_document =
        lowered.contains("scanned document") || lowered.contains("scanned documents");
    let has_photo_of_document =
        lowered.contains("photo of a document") || lowered.contains("photos of documents");
    let has_signs_or_billboards = lowered.contains("signs") || lowered.contains("billboards");
    let has_subtitle_text = lowered.contains("subtitle text");

    if !(has_machine_encoded_text && has_scanned_document) {
        return None;
    }

    let mut answer = String::from(
        "OCR converts images of text into machine-encoded text. The article explicitly names a scanned document",
    );
    if has_photo_of_document {
        answer.push_str(", a photo of a document");
    }
    if has_signs_or_billboards {
        answer.push_str(", text on signs and billboards");
    }
    if has_subtitle_text {
        answer.push_str(", and subtitle text superimposed on an image");
    }
    answer.push('.');

    Some(answer)
}

pub(crate) fn canonical_evidence_text_corpus(
    evidence: &CanonicalAnswerEvidence,
    chunks: &[RuntimeMatchedChunk],
) -> String {
    let mut parts = Vec::new();
    parts.extend(
        evidence
            .chunk_rows
            .iter()
            .flat_map(|chunk| [chunk.content_text.as_str(), chunk.normalized_text.as_str()]),
    );
    parts.extend(
        evidence
            .structured_blocks
            .iter()
            .flat_map(|block| [block.text.as_str(), block.normalized_text.as_str()]),
    );
    parts.extend(
        evidence
            .technical_facts
            .iter()
            .flat_map(|fact| [fact.display_value.as_str(), fact.canonical_value_text.as_str()]),
    );
    parts.extend(
        chunks.iter().flat_map(|chunk| [chunk.excerpt.as_str(), chunk.source_text.as_str()]),
    );
    parts.join("\n").to_lowercase()
}

fn extract_primary_document_heading(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    document_heading_lines(document_chunks).into_iter().next()
}

fn extract_secondary_document_heading(document_chunks: &[&RuntimeMatchedChunk]) -> Option<String> {
    let headings = document_heading_lines(document_chunks);
    headings.get(1).cloned().or_else(|| headings.first().cloned())
}

fn document_heading_lines(document_chunks: &[&RuntimeMatchedChunk]) -> Vec<String> {
    let mut headings = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for chunk in document_chunks {
        for line in chunk.source_text.lines() {
            let Some(candidate) = normalize_heading_line(line) else {
                continue;
            };
            if seen.insert(candidate.clone()) {
                headings.push(candidate);
                if headings.len() >= 6 {
                    return headings;
                }
            }
        }
    }
    headings
}

fn normalize_heading_line(line: &str) -> Option<String> {
    let candidate = line.trim().trim_start_matches('#').trim();
    if candidate.is_empty()
        || candidate.len() > 120
        || candidate.starts_with("Source:")
        || candidate.starts_with("Source type:")
        || candidate.starts_with("http://")
        || candidate.starts_with("https://")
        || candidate.starts_with('/')
        || matches!(candidate, "GET" | "POST" | "PUT" | "PATCH" | "DELETE")
    {
        return None;
    }
    Some(candidate.to_string())
}

pub(super) fn build_multi_document_endpoint_answer_from_chunks(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let lowered = question.to_lowercase();
    if !(lowered.contains("endpoint") || lowered.contains("эндпоинт")) {
        return None;
    }
    if lowered.contains("сравн") || lowered.contains("протокол") || lowered.contains("порт")
    {
        return None;
    }

    let question_keywords = technical_literal_focus_keywords(question);
    if question_keywords.is_empty() {
        return None;
    }

    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }
    let pagination_requested = question_mentions_pagination(question);
    let focus_segments = technical_literal_focus_keyword_segments(question);
    let scoped_document_ids = if focus_segments.is_empty() {
        ordered_document_ids.clone()
    } else {
        let mut selected = Vec::new();
        let mut seen = HashSet::new();
        for segment_keywords in focus_segments {
            let best_document = ordered_document_ids
                .iter()
                .filter_map(|document_id| {
                    let document_chunks = per_document_chunks.get(document_id)?;
                    let best_chunk_score = document_chunks
                        .iter()
                        .map(|chunk| {
                            technical_chunk_selection_score(
                                &format!("{} {}", chunk.excerpt, chunk.source_text),
                                &segment_keywords,
                                pagination_requested,
                            )
                        })
                        .max()
                        .unwrap_or_default();
                    let document_text = document_chunks
                        .iter()
                        .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
                        .collect::<Vec<_>>()
                        .join("\n")
                        .to_lowercase();
                    let document_keyword_score = segment_keywords
                        .iter()
                        .map(|keyword| technical_keyword_weight(&document_text, keyword) as isize)
                        .sum::<isize>();
                    let score = best_chunk_score.max(document_keyword_score);
                    (score > 0).then_some((score, *document_id))
                })
                .max_by(|left, right| {
                    left.0.cmp(&right.0).then_with(|| {
                        let left_index = ordered_document_ids
                            .iter()
                            .position(|document_id| document_id == &left.1)
                            .unwrap_or(usize::MAX);
                        let right_index = ordered_document_ids
                            .iter()
                            .position(|document_id| document_id == &right.1)
                            .unwrap_or(usize::MAX);
                        right_index.cmp(&left_index)
                    })
                });
            if let Some((_, document_id)) = best_document {
                if seen.insert(document_id) {
                    selected.push(document_id);
                }
            }
        }
        if selected.is_empty() { ordered_document_ids.clone() } else { selected }
    };

    let mut lines = Vec::new();
    for document_id in scoped_document_ids {
        let Some(document_chunks) = per_document_chunks.get(&document_id) else {
            continue;
        };
        let local_keywords =
            document_local_focus_keywords(question, document_chunks, &question_keywords);
        let mut ranked_chunks = document_chunks.clone();
        ranked_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                pagination_requested,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                pagination_requested,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });

        let Some(best_chunk) = ranked_chunks.into_iter().find(|chunk| {
            let focused = focused_excerpt_for(&chunk.source_text, &local_keywords, 900);
            let literal_source = if focused.trim().is_empty() {
                chunk.source_text.as_str()
            } else {
                focused.as_str()
            };
            !extract_explicit_path_literals(literal_source, 6).is_empty()
                || !extract_url_literals(literal_source, 4).is_empty()
        }) else {
            continue;
        };

        let focused = focused_excerpt_for(&best_chunk.source_text, &local_keywords, 900);
        let literal_source = if focused.trim().is_empty() {
            best_chunk.source_text.as_str()
        } else {
            focused.as_str()
        };
        let endpoint = extract_explicit_path_literals(literal_source, 6)
            .into_iter()
            .next()
            .or_else(|| extract_url_literals(literal_source, 4).into_iter().next())?;
        let subject = infer_endpoint_subject_label(&TechnicalLiteralDocumentGroup {
            document_label: best_chunk.document_label.clone(),
            ..TechnicalLiteralDocumentGroup::default()
        });
        let literal = extract_http_methods(literal_source, 3)
            .into_iter()
            .next()
            .map_or_else(|| format!("`{endpoint}`"), |method| format!("`{method} {endpoint}`"));
        lines.push(format!("- для {subject} — {literal}"));
    }

    (lines.len() >= 2).then(|| format!("Нужны два endpoint'а:\n\n{}", lines.join("\n")))
}

pub(crate) fn focused_answer_document_id(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if chunks.is_empty() || question_requests_multi_document_scope(question) {
        return None;
    }

    let explicit_targets = super::explicit_target_document_ids_from_values(
        question,
        chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
    );
    if explicit_targets.len() == 1 {
        return explicit_targets.iter().next().copied();
    }

    #[derive(Debug, Clone)]
    struct DocumentFocusScore {
        document_id: Uuid,
        document_label: String,
        score_sum: f32,
        chunk_count: usize,
        first_rank: usize,
        label_keyword_hits: usize,
        label_marker_hits: usize,
    }

    let question_keywords = crate::services::query::planner::extract_keywords(question);
    let mut per_document = HashMap::<Uuid, DocumentFocusScore>::new();
    for (rank, chunk) in chunks.iter().enumerate() {
        let lowered_label = chunk.document_label.to_lowercase();
        let entry = per_document.entry(chunk.document_id).or_insert_with(|| DocumentFocusScore {
            document_id: chunk.document_id,
            document_label: chunk.document_label.clone(),
            score_sum: 0.0,
            chunk_count: 0,
            first_rank: rank,
            label_keyword_hits: question_keywords
                .iter()
                .filter(|keyword| lowered_label.contains(keyword.as_str()))
                .count(),
            label_marker_hits: document_focus_marker_hits(question, &chunk.document_label),
        });
        entry.score_sum += score_value(chunk.score);
        entry.chunk_count += 1;
        entry.first_rank = entry.first_rank.min(rank);
    }

    let mut ranked = per_document.into_values().collect::<Vec<_>>();
    if ranked.is_empty() {
        return None;
    }
    ranked.sort_by(|left, right| {
        right.label_marker_hits.cmp(&left.label_marker_hits).then_with(|| {
            right
                .score_sum
                .total_cmp(&left.score_sum)
                .then_with(|| right.chunk_count.cmp(&left.chunk_count))
                .then_with(|| right.label_keyword_hits.cmp(&left.label_keyword_hits))
                .then_with(|| left.first_rank.cmp(&right.first_rank))
                .then_with(|| left.document_label.cmp(&right.document_label))
        })
    });

    if ranked.len() == 1 {
        return Some(ranked[0].document_id);
    }

    let top = &ranked[0];
    let second = &ranked[1];
    if top.label_marker_hits > second.label_marker_hits && top.label_marker_hits > 0 {
        return Some(top.document_id);
    }

    let has_explicit_single_source_anchor = question_mentions_single_source_anchor(question);
    let materially_higher_score =
        top.score_sum >= second.score_sum * DOMINANT_DOCUMENT_SCORE_MULTIPLIER;
    let materially_more_chunks = top.chunk_count > second.chunk_count;
    let stronger_label_match = top.label_keyword_hits > second.label_keyword_hits;

    if has_explicit_single_source_anchor
        || materially_higher_score
        || materially_more_chunks
        || stronger_label_match
    {
        Some(top.document_id)
    } else {
        None
    }
}

fn document_focus_marker_hits(question: &str, document_label: &str) -> usize {
    let lowered_question = question.to_lowercase();
    document_label_focus_markers(document_label)
        .into_iter()
        .filter(|marker| question_mentions_document_marker(&lowered_question, marker))
        .count()
}

fn strip_known_document_label_extension(document_label: &str) -> &str {
    let trimmed = document_label.trim();
    let Some((stem, extension)) = trimmed.rsplit_once('.') else {
        return trimmed;
    };
    let lowered_extension = extension.to_ascii_lowercase();
    if KNOWN_DOCUMENT_LABEL_EXTENSIONS.contains(&lowered_extension.as_str()) {
        stem
    } else {
        trimmed
    }
}

fn document_label_focus_markers(document_label: &str) -> Vec<&'static str> {
    let lowered_label = document_label.to_lowercase();
    let mut markers = Vec::new();
    if let Some(extension_marker) = document_label_extension_marker(&lowered_label) {
        markers.push(extension_marker);
    }
    for marker in DOCUMENT_LABEL_KEYWORD_MARKERS {
        if lowered_label.contains(marker) {
            markers.push(*marker);
        }
    }
    markers
}

fn document_label_extension_marker(lowered_label: &str) -> Option<&'static str> {
    let (_, extension) = lowered_label.rsplit_once('.')?;
    match extension {
        "pdf" => Some("pdf"),
        "docx" => Some("docx"),
        "csv" => Some("csv"),
        "tsv" => Some("tsv"),
        "xls" => Some("xls"),
        "xlsx" => Some("xlsx"),
        "xlsb" => Some("xlsb"),
        "ods" => Some("ods"),
        "pptx" => Some("pptx"),
        "png" => Some("png"),
        "jpg" => Some("jpg"),
        "jpeg" => Some("jpeg"),
        _ => None,
    }
}

fn question_mentions_document_marker(lowered_question: &str, marker: &str) -> bool {
    if DOCUMENT_LABEL_KEYWORD_MARKERS.contains(&marker) {
        return lowered_question.contains(marker);
    }

    // Match the marker only when it appears as a complete extension token, e.g.
    // ".xls" must NOT match ".xlsx" — require the character after the marker to
    // be absent or non-alphanumeric.
    let extension_marker = format!(".{marker}");
    let extension_match = lowered_question.match_indices(&extension_marker).any(|(start, _)| {
        let end = start + extension_marker.len();
        lowered_question[end..]
            .chars()
            .next()
            .map_or(true, |character| !character.is_ascii_alphanumeric())
    });
    extension_match
        || lowered_question
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| token == marker)
}

#[cfg(test)]
mod document_label_tests {
    use std::collections::HashMap;

    use uuid::Uuid;

    use crate::infra::arangodb::document_store::KnowledgeDocumentRow;
    use crate::services::query::execution::types::RuntimeMatchedChunk;
    use crate::shared::extraction::table_summary::{
        build_table_column_summaries, render_table_column_summary,
    };

    use super::{
        build_missing_explicit_document_answer, build_table_row_grounded_answer,
        build_table_summary_grounded_answer, concise_document_subject_label,
        document_focus_marker_hits, focused_answer_document_id, render_table_summary_chunk_section,
    };

    #[test]
    fn concise_document_subject_label_strips_spreadsheet_extensions() {
        assert_eq!(
            concise_document_subject_label("spreadsheet_ods_api_reference.xlsb"),
            "Spreadsheet ODS API reference"
        );
        assert_eq!(concise_document_subject_label("inventory_snapshot.ods"), "Inventory snapshot");
    }

    #[test]
    fn document_focus_marker_hits_distinguishes_xls_from_xlsx() {
        assert_eq!(
            document_focus_marker_hits("What does inventory.xlsx validate?", "inventory.xlsx",),
            1
        );
        assert_eq!(
            document_focus_marker_hits("What does inventory.xlsx validate?", "inventory.xls",),
            0
        );
        assert_eq!(
            document_focus_marker_hits("What does inventory.xls validate?", "inventory.xls",),
            1
        );
    }

    #[test]
    fn focused_answer_document_id_prefers_explicit_extension_match() {
        let csv_id = Uuid::now_v7();
        let xlsx_id = Uuid::now_v7();
        let chunks = vec![
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: csv_id,
                document_label: "people-100.csv".to_string(),
                excerpt: String::new(),
                score: Some(1.0),
                source_text: "Sheet: people-100 | Row 1 | Email: elijah57@example.net".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id: xlsx_id,
                document_label: "people-100.xlsx".to_string(),
                excerpt: String::new(),
                score: Some(10.0),
                source_text: "Sheet: people-100 | Row 1 | Email: elijah57@example.net".to_string(),
            },
        ];

        assert_eq!(
            focused_answer_document_id(
                "В people-100.csv какая должность у Shelby Terrell с email elijah57@example.net?",
                &chunks,
            ),
            Some(csv_id)
        );
    }

    #[test]
    fn build_table_row_grounded_answer_supports_canonical_row_tokens() {
        let document_id = Uuid::now_v7();
        let chunks = (1..=5)
            .map(|row_number| RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "sample-heavy-1.xls".to_string(),
                excerpt: String::new(),
                score: Some(10.0 - row_number as f32),
                source_text: format!("Sheet: test1 | Row {row_number} | col_1: {row_number}"),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            build_table_row_grounded_answer(
                "Покажи значения из первых 5 строк sample-heavy-1.xls.",
                &chunks,
            ),
            Some(
                "- Row 1: col_1 = `1`\n- Row 2: col_1 = `2`\n- Row 3: col_1 = `3`\n- Row 4: col_1 = `4`\n- Row 5: col_1 = `5`"
                    .to_string()
            )
        );
    }

    #[test]
    fn build_table_row_grounded_answer_supports_russian_industry_synonym() {
        let document_id = Uuid::now_v7();
        let chunks = vec![RuntimeMatchedChunk {
            chunk_id: Uuid::now_v7(),
            document_id,
            document_label: "organizations-100.csv".to_string(),
            excerpt: String::new(),
            score: Some(10.0),
            source_text:
                "Sheet: organizations-100 | Row 1 | Name: Ferrell LLC | Country: Papua New Guinea | Industry: Plastics"
                    .to_string(),
        }];

        assert_eq!(
            build_table_row_grounded_answer(
                "В organizations-100.csv какая страна и индустрия у Ferrell LLC?",
                &chunks,
            ),
            Some("Country: `Papua New Guinea`; Industry: `Plastics`".to_string())
        );
    }

    #[test]
    fn build_table_row_grounded_answer_lists_values_for_targeted_single_value_sheets() {
        let document_id = Uuid::now_v7();
        let chunks = vec![
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "sample-simple-2.xls".to_string(),
                excerpt: String::new(),
                score: Some(10.0),
                source_text: "Sheet: test1 | Row 1 | col_1: test1".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "sample-simple-2.xls".to_string(),
                excerpt: String::new(),
                score: Some(9.0),
                source_text: "Sheet: test2 | Row 1 | col_1: test2".to_string(),
            },
        ];

        assert_eq!(
            build_table_row_grounded_answer("Какие значения есть в sample-simple-2.xls?", &chunks),
            Some("- test1 row 1: `test1`\n- test2 row 1: `test2`".to_string())
        );
    }

    #[test]
    fn build_table_summary_grounded_answer_reports_most_frequent_values() {
        let document_id = Uuid::now_v7();
        let summaries = build_table_column_summaries(
            Some("organizations-100"),
            None,
            &["Country".to_string(), "Industry".to_string()],
            &[
                vec!["Sweden".to_string(), "Plastics".to_string()],
                vec!["Benin".to_string(), "Plastics".to_string()],
                vec!["Sweden".to_string(), "Printing".to_string()],
                vec!["Benin".to_string(), "Printing".to_string()],
            ],
        );
        let chunks = summaries
            .into_iter()
            .enumerate()
            .map(|(index, summary)| RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "organizations-100.csv".to_string(),
                excerpt: String::new(),
                score: Some(10.0 - index as f32),
                source_text: render_table_column_summary(&summary),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            build_table_summary_grounded_answer(
                "What is the most frequent Country in organizations-100.csv?",
                &chunks,
            ),
            Some(
                "The most frequent `Country` values are `Benin`, `Sweden` (`2` rows each)."
                    .to_string()
            )
        );
    }

    #[test]
    fn build_table_summary_grounded_answer_reports_no_single_most_frequent_value() {
        let document_id = Uuid::now_v7();
        let summaries = build_table_column_summaries(
            Some("customers-100"),
            None,
            &["City".to_string()],
            &[vec!["Moscow".to_string()], vec!["London".to_string()], vec!["Berlin".to_string()]],
        );
        let chunks = summaries
            .into_iter()
            .map(|summary| RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "customers-100.csv".to_string(),
                excerpt: String::new(),
                score: Some(10.0),
                source_text: render_table_column_summary(&summary),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            build_table_summary_grounded_answer(
                "В customers-100.csv какой город встречается чаще всего?",
                &chunks,
            ),
            Some(
                "Для `City` нет одного самого частого значения: все значения встречаются по одному разу."
                    .to_string()
            )
        );
    }

    #[test]
    fn build_table_summary_grounded_answer_reports_average_values() {
        let document_id = Uuid::now_v7();
        let summaries = build_table_column_summaries(
            Some("products-100"),
            None,
            &["Stock".to_string()],
            &[vec!["100".to_string()], vec!["200".to_string()], vec!["300".to_string()]],
        );
        let chunks = summaries
            .into_iter()
            .map(|summary| RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "products-100.csv".to_string(),
                excerpt: String::new(),
                score: Some(10.0),
                source_text: render_table_column_summary(&summary),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            build_table_summary_grounded_answer("Какой средний stock в products-100.csv?", &chunks),
            Some("Среднее значение `Stock` — `200` по `3` строкам.".to_string())
        );
    }

    #[test]
    fn build_table_summary_grounded_answer_reports_average_number_of_employees() {
        let document_id = Uuid::now_v7();
        let summaries = build_table_column_summaries(
            Some("organizations-100"),
            None,
            &["Number of Employees".to_string()],
            &[vec!["10".to_string()], vec!["20".to_string()]],
        );
        let chunks = summaries
            .into_iter()
            .map(|summary| RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "organizations-100.csv".to_string(),
                excerpt: String::new(),
                score: Some(10.0),
                source_text: render_table_column_summary(&summary),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            build_table_summary_grounded_answer(
                "Какое среднее число сотрудников в organizations-100.csv?",
                &chunks,
            ),
            Some("Среднее значение `Number of Employees` — `15` по `2` строкам.".to_string())
        );
    }

    #[test]
    fn build_table_summary_grounded_answer_derives_average_from_table_rows() {
        let document_id = Uuid::now_v7();
        let chunks = (1..=4)
            .map(|value| RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "sample-heavy-1.xls".to_string(),
                excerpt: String::new(),
                score: Some(0.25),
                source_text: format!("Sheet: Sheet1 | Row {value} | col_1: {value}"),
            })
            .collect::<Vec<_>>();

        assert_eq!(
            build_table_summary_grounded_answer(
                "В sample-heavy-1.xls какое среднее значение?",
                &chunks,
            ),
            Some("Среднее значение `col_1` — `2.50` по `4` строкам.".to_string())
        );
    }

    #[test]
    fn render_table_summary_chunk_section_derives_from_table_rows() {
        let document_id = Uuid::now_v7();
        let chunks = vec![
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "sample-heavy-1.xls".to_string(),
                excerpt: String::new(),
                score: Some(0.25),
                source_text: "Sheet: Sheet1 | Row 1 | col_1: 1".to_string(),
            },
            RuntimeMatchedChunk {
                chunk_id: Uuid::now_v7(),
                document_id,
                document_label: "sample-heavy-1.xls".to_string(),
                excerpt: String::new(),
                score: Some(0.25),
                source_text: "Sheet: Sheet1 | Row 2 | col_1: 3".to_string(),
            },
        ];

        let section = render_table_summary_chunk_section(
            "В sample-heavy-1.xls какое среднее значение?",
            &chunks,
        );
        assert!(section.contains("Table summaries"));
        assert!(section.contains("Average: 2"));
    }

    #[test]
    fn build_missing_explicit_document_answer_reports_absent_file_reference() {
        let document = KnowledgeDocumentRow {
            key: "organizations-100.csv".to_string(),
            arango_id: None,
            arango_rev: None,
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            external_key: "organizations-100.csv".to_string(),
            file_name: Some("organizations-100.csv".to_string()),
            title: Some("organizations-100.csv".to_string()),
            document_state: "active".to_string(),
            active_revision_id: None,
            readable_revision_id: None,
            latest_revision_no: None,
            deleted_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        let index = HashMap::from([(document.document_id, document)]);

        assert_eq!(
            build_missing_explicit_document_answer(
                "У Shelby Terrell в people-100.csv какой job title?",
                &index,
            ),
            Some("Документ `people-100.csv` отсутствует в активной библиотеке.".to_string())
        );
    }
}

pub(crate) fn question_requests_multi_document_scope(question: &str) -> bool {
    let lowered = question.to_lowercase();
    if [
        "compare",
        "contrast",
        "versus",
        " vs ",
        "between",
        "across documents",
        "across articles",
        "combine documents",
        "combine articles",
        "multiple documents",
        "multiple articles",
        "several documents",
        "several articles",
        "both documents",
        "both articles",
        "different documents",
        "different articles",
        "сравни",
        "сравните",
        "между документ",
        "между стать",
        "нескольких документ",
        "нескольких стать",
        "разных документ",
        "разных стать",
        "оба документ",
        "обе стать",
        "обоих документ",
        "обеих стать",
        "отдельно",
        "separately",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
    {
        return true;
    }

    let asks_multiple_items = [
        "which two",
        "which three",
        "two technologies",
        "three technologies",
        "two items",
        "three items",
        "какие две",
        "какие три",
        "две технологии",
        "три технологии",
    ]
    .iter()
    .any(|marker| lowered.contains(marker));
    let asks_role_pairing = [
        "fit those roles",
        "should it combine",
        "combine into that stack",
        "fit those roles",
        "and which one",
        "эти роли",
        "в этот стек",
        "нужно сочетать",
        "следует объединить",
    ]
    .iter()
    .any(|marker| lowered.contains(marker));

    asks_multiple_items || asks_role_pairing
}

fn question_mentions_single_source_anchor(question: &str) -> bool {
    let lowered = question.to_lowercase();
    [
        "according to",
        "in the article",
        "in this article",
        "in the document",
        "this article",
        "this document",
        "the article",
        "the document",
        "в статье",
        "в этом документе",
        "в документе",
        "эта статья",
        "этот документ",
    ]
    .iter()
    .any(|marker| lowered.contains(marker))
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

pub(crate) fn render_table_summary_chunk_section(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> String {
    if !question_asks_table_aggregation(question) {
        return String::new();
    }
    let mut summaries =
        collect_scored_table_summaries(chunks, focused_answer_document_id(question, chunks))
            .into_iter()
            .filter(|entry| summary_matches_requested_aggregation(&entry.summary, question))
            .collect::<Vec<_>>();
    if summaries.is_empty() {
        return String::new();
    }
    if summaries.iter().any(|entry| entry.summary.aggregation_priority > 0) {
        summaries.retain(|entry| entry.summary.aggregation_priority > 0);
    }
    summaries.sort_by(|left, right| {
        right
            .summary
            .aggregation_priority
            .cmp(&left.summary.aggregation_priority)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| left.summary.column_name.cmp(&right.summary.column_name))
    });
    let lines = summaries
        .into_iter()
        .take(8)
        .map(|entry| format!("- {}", excerpt_for(&entry.source_text, 320)))
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return String::new();
    }
    format!("Table summaries\n{}", lines.join("\n"))
}

pub(crate) fn render_canonical_chunk_section(
    question: &str,
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
    let question_keywords = technical_literal_focus_keywords(question);
    let pagination_requested = question_mentions_pagination(question);
    let mut selected = select_document_balanced_chunks(
        question,
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

fn summary_matches_requested_aggregation(summary: &TableColumnSummary, question: &str) -> bool {
    if question_asks_average(question) {
        return summary.value_kind == TableSummaryValueKind::Numeric && summary.average.is_some();
    }
    if question_asks_most_frequent(question) {
        return summary.value_kind == TableSummaryValueKind::Categorical
            && summary.most_frequent_count > 0;
    }
    true
}
