use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

use crate::domains::query_ir::QueryIR;

use super::{retrieve::score_value, types::RuntimeMatchedChunk};

/// Score gap multiplier for dominant-document detection in answer assembly.
const DOMINANT_DOCUMENT_SCORE_MULTIPLIER: f32 = 1.2;
const EXPLICIT_DOCUMENT_REFERENCE_EXTENSIONS: &[&str] = &[
    "md", "txt", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "png", "jpg",
    "jpeg",
];
const KNOWN_DOCUMENT_LABEL_EXTENSIONS: &[&str] = &[
    "md", "txt", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "png", "jpg",
    "jpeg",
];
const DOCUMENT_LABEL_KEYWORD_MARKERS: &[&str] = &["runtime", "upload", "smoke", "fixture", "check"];
const DOCUMENT_LABEL_ACRONYMS: &[&str] = &[
    "rag", "llm", "ocr", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "api",
];

pub(crate) fn explicit_target_document_ids_from_values<'a, I>(
    question: &str,
    values: I,
) -> BTreeSet<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    let normalized_question = normalize_document_target_text(question);
    if normalized_question.is_empty() {
        return BTreeSet::new();
    }

    let mut best_match_lengths = HashMap::<Uuid, usize>::new();
    for (document_id, raw_value) in values {
        for candidate in normalized_document_target_candidates([raw_value]) {
            if candidate.len() < 4 || !normalized_question.contains(candidate.as_str()) {
                continue;
            }
            best_match_lengths
                .entry(document_id)
                .and_modify(|best| *best = (*best).max(candidate.len()))
                .or_insert(candidate.len());
        }
    }

    let Some(best_length) = best_match_lengths.values().copied().max() else {
        return BTreeSet::new();
    };

    best_match_lengths
        .into_iter()
        .filter_map(|(document_id, candidate_length)| {
            (candidate_length == best_length).then_some(document_id)
        })
        .collect()
}

pub(crate) fn normalized_document_target_candidates<'a, I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();

    for raw in values {
        let normalized = normalize_document_target_text(raw);
        if normalized.is_empty() || !seen.insert(normalized.clone()) {
            continue;
        }
        candidates.push(normalized.clone());
        if let Some((stem, _)) = normalized.rsplit_once('.') {
            let stem = stem.trim().to_string();
            if !stem.is_empty() && seen.insert(stem.clone()) {
                candidates.push(stem);
            }
        }
    }

    candidates
}

pub(crate) fn normalize_document_target_text(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' '))
        .collect::<String>()
}

pub(crate) fn explicit_document_reference_literals(question: &str) -> Vec<String> {
    let normalized = normalize_document_target_text(question);
    let mut seen = BTreeSet::new();
    normalized
        .split_whitespace()
        .filter_map(|token| {
            let (stem, extension) = token.rsplit_once('.')?;
            if stem.is_empty() {
                return None;
            }
            EXPLICIT_DOCUMENT_REFERENCE_EXTENSIONS.contains(&extension).then(|| token.to_string())
        })
        .filter(|token| seen.insert(token.clone()))
        .collect()
}

/// Does the user's question request retrieval to span multiple documents?
///
/// With a compiled IR in scope the answer is direct: `ir.is_multi_document()`
/// covers the `QueryScope::MultiDocument` case (compare / contrast /
/// "across documents" / "which two" and so on) by construction. The three
/// bilingual marker lists below are kept only as a transitional fallback
/// for callers the IR plumbing hasn't reached yet; once every caller
/// threads IR through, the fallback disappears.
pub(crate) fn question_requests_multi_document_scope(question: &str, ir: Option<&QueryIR>) -> bool {
    if let Some(ir) = ir {
        return ir.is_multi_document();
    }
    let lowered = question.to_lowercase();
    if [
        "compare",
        "contrast",
        "difference between",
        "different from",
        "differ from",
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
        "отличается от",
        "отличие между",
        "разница между",
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

pub(crate) fn focused_answer_document_id(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<Uuid> {
    if chunks.is_empty() || question_requests_multi_document_scope(question, None) {
        return None;
    }

    let explicit_targets = explicit_target_document_ids_from_values(
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

pub(crate) fn document_focus_marker_hits(question: &str, document_label: &str) -> usize {
    let lowered_question = question.to_lowercase();
    document_label_focus_markers(document_label)
        .into_iter()
        .filter(|marker| question_mentions_document_marker(&lowered_question, marker))
        .count()
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

    let extension_marker = format!(".{marker}");
    let extension_match = lowered_question.match_indices(&extension_marker).any(|(start, _)| {
        let end = start + extension_marker.len();
        lowered_question[end..]
            .chars()
            .next()
            .is_none_or(|character| !character.is_ascii_alphanumeric())
    });
    extension_match
        || lowered_question
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| token == marker)
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

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{explicit_document_reference_literals, explicit_target_document_ids_from_values};

    #[test]
    fn explicit_target_document_ids_prefer_exact_extension_match() {
        let csv_id = Uuid::now_v7();
        let xlsx_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "В people-100.csv какая должность у Shelby Terrell?",
            [(csv_id, "people-100.csv"), (xlsx_id, "people-100.xlsx")],
        );
        assert_eq!(matched, [csv_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_keep_stem_ambiguous_without_extension() {
        let csv_id = Uuid::now_v7();
        let xlsx_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "Что есть в people-100?",
            [(csv_id, "people-100.csv"), (xlsx_id, "people-100.xlsx")],
        );
        assert_eq!(matched, [csv_id, xlsx_id].into_iter().collect());
    }

    #[test]
    fn extracts_explicit_document_reference_literals_from_question() {
        assert_eq!(
            explicit_document_reference_literals(
                "У Shelby Terrell в people-100.csv какой job title и что есть в sample-heavy-1.xls?"
            ),
            vec!["people-100.csv".to_string(), "sample-heavy-1.xls".to_string()]
        );
    }
}
