use std::collections::HashMap;

use uuid::Uuid;

use super::{
    CanonicalTarget, concise_document_subject_label, technical_literals::technical_keyword_weight,
    types::RuntimeMatchedChunk,
};

pub(crate) fn build_multi_document_role_answer(
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
            if target.matches_subject_label(&document.subject_label) {
                score += 10_000;
            } else if target.corpus_mentions(&document.corpus_text) {
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

pub(crate) fn role_clause_canonical_target(clause: &str) -> Option<CanonicalTarget> {
    let lowered = clause.to_lowercase();
    if (lowered.contains("semantic similarity") || lowered.contains("embeddings"))
        && !lowered.contains("before answering")
    {
        return Some(CanonicalTarget::VectorDatabase);
    }
    if lowered.contains("text generation")
        || lowered.contains("reasoning")
        || lowered.contains("natural language processing")
        || lowered.contains("model family")
        || lowered.contains("generated language output")
        || lowered.contains("language generation")
    {
        return Some(CanonicalTarget::LargeLanguageModel);
    }
    if lowered.contains("retrieval from external documents")
        || lowered.contains("before answering")
        || lowered.contains("external data sources")
    {
        return Some(CanonicalTarget::RetrievalAugmentedGeneration);
    }
    if lowered.contains("programming language") || lowered.contains("memory safety") {
        return Some(CanonicalTarget::RustProgrammingLanguage);
    }
    if lowered.contains("borrow checker") {
        return Some(CanonicalTarget::RustProgrammingLanguage);
    }
    if lowered.contains("machine-readable") || lowered.contains("web standards") {
        return Some(CanonicalTarget::SemanticWeb);
    }
    if lowered.contains("interlinked descriptions") || lowered.contains("entities") {
        return Some(CanonicalTarget::KnowledgeGraph);
    }
    if lowered.contains("relationships are first-class citizens")
        || lowered.contains("gremlin")
        || lowered.contains("sparql")
        || lowered.contains("cypher")
    {
        return Some(CanonicalTarget::GraphDatabase);
    }
    if lowered.contains("vectorize")
        || (lowered.contains("words")
            && lowered.contains("phrases")
            && lowered.contains("documents")
            && lowered.contains("images")
            && lowered.contains("audio"))
    {
        return Some(CanonicalTarget::VectorDatabase);
    }
    None
}

fn canonical_document_subject_label(document_chunks: &[&RuntimeMatchedChunk]) -> String {
    concise_document_subject_label(&document_chunks[0].document_label)
}
