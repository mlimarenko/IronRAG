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
const DOCUMENT_LABEL_ACRONYMS: &[&str] = &[
    "rag", "llm", "ocr", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "api",
];

#[derive(Debug, Clone)]
struct DocumentTargetCandidate {
    text: String,
    priority: usize,
}

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

    let explicit_literals = explicit_document_reference_literals(question);
    if !explicit_literals.is_empty() {
        return explicit_document_reference_matching_document_ids(&explicit_literals, values);
    }

    let mut best_match_scores = HashMap::<Uuid, (usize, usize)>::new();
    for (document_id, raw_value) in values {
        for candidate in ranked_document_target_candidates([raw_value]) {
            if candidate.text.len() >= 4 && normalized_question.contains(candidate.text.as_str()) {
                let score = (candidate.text.len(), candidate.priority);
                best_match_scores
                    .entry(document_id)
                    .and_modify(|best| *best = (*best).max(score))
                    .or_insert(score);
            }
        }
    }

    if let Some(best_score) = best_match_scores.values().copied().max() {
        return best_match_scores
            .into_iter()
            .filter_map(|(document_id, score)| (score == best_score).then_some(document_id))
            .collect();
    }

    BTreeSet::new()
}

pub(crate) fn explicit_document_reference_matching_document_ids<'a, I>(
    explicit_literals: &[String],
    values: I,
) -> BTreeSet<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    let explicit_literals = explicit_literals.iter().map(String::as_str).collect::<BTreeSet<_>>();
    if explicit_literals.is_empty() {
        return BTreeSet::new();
    }

    values
        .into_iter()
        .filter_map(|(document_id, raw_value)| {
            normalized_explicit_document_reference_candidates(raw_value)
                .into_iter()
                .any(|candidate| explicit_literals.contains(candidate.as_str()))
                .then_some(document_id)
        })
        .collect()
}

pub(crate) fn explicit_document_reference_literal_is_present<'a, I>(
    explicit_literal: &str,
    values: I,
) -> bool
where
    I: IntoIterator<Item = &'a str>,
{
    values.into_iter().any(|raw_value| {
        normalized_explicit_document_reference_candidates(raw_value)
            .into_iter()
            .any(|candidate| candidate == explicit_literal)
    })
}

pub(crate) fn normalized_document_target_candidates<'a, I>(values: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    ranked_document_target_candidates(values).into_iter().map(|candidate| candidate.text).collect()
}

fn ranked_document_target_candidates<'a, I>(values: I) -> Vec<DocumentTargetCandidate>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    let mut push_candidate =
        |value: String, priority: usize, candidates: &mut Vec<DocumentTargetCandidate>| {
            let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
            if normalized.is_empty() || !seen.insert(normalized.clone()) {
                return;
            }
            candidates.push(DocumentTargetCandidate { text: normalized, priority });
        };

    for raw in values {
        let normalized = normalize_document_target_text(raw);
        if normalized.is_empty() {
            continue;
        }
        push_candidate(normalized.clone(), 4, &mut candidates);
        if let Some(separator_variant) = separator_normalized_document_target_candidate(&normalized)
        {
            push_candidate(separator_variant, 2, &mut candidates);
        }
        if let Some((stem, _)) = normalized.rsplit_once('.') {
            let stem = stem.trim().to_string();
            if !stem.is_empty() {
                push_candidate(stem.clone(), 3, &mut candidates);
                if let Some(separator_variant) =
                    separator_normalized_document_target_candidate(&stem)
                {
                    push_candidate(separator_variant, 1, &mut candidates);
                }
            }
        }
    }

    candidates
}

fn separator_normalized_document_target_candidate(value: &str) -> Option<String> {
    let normalized = value
        .chars()
        .map(|character| match character {
            '_' | '-' | '.' => ' ',
            _ => character,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    (normalized != value).then_some(normalized).filter(|candidate| !candidate.is_empty())
}

fn normalized_explicit_document_reference_candidates(raw: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for value in [Some(raw), raw.rsplit(['/', '\\']).next()].into_iter().flatten() {
        let normalized = normalize_document_target_text(value);
        if !normalized.is_empty() && seen.insert(normalized.clone()) {
            candidates.push(normalized);
        }
    }
    candidates
}

pub(crate) fn normalize_document_target_text(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' '))
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
/// Answered directly from the compiled IR — `ir.is_multi_document()` covers
/// the `QueryScope::MultiDocument` case (compare / contrast / "across
/// documents" / "which two" and so on) by construction. Without IR the
/// caller has no canonical signal, so the answer is `false`.
pub(crate) fn question_requests_multi_document_scope(
    _question: &str,
    ir: Option<&QueryIR>,
) -> bool {
    ir.is_some_and(QueryIR::is_multi_document)
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
    let _ = question;
    false
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

    use super::{
        explicit_document_reference_literal_is_present, explicit_document_reference_literals,
        explicit_target_document_ids_from_values,
    };

    #[test]
    fn explicit_target_document_ids_prefer_exact_extension_match() {
        let csv_id = Uuid::now_v7();
        let xlsx_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "In people-100.csv what is Shelby Terrell's job title?",
            [(csv_id, "people-100.csv"), (xlsx_id, "people-100.xlsx")],
        );
        assert_eq!(matched, [csv_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_do_not_fuzzy_match_different_file_reference() {
        let organizations_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "In people-100.csv what is Shelby Terrell's job title?",
            [(organizations_id, "organizations-100.csv")],
        );
        assert!(matched.is_empty());
    }

    #[test]
    fn explicit_target_document_ids_keep_stem_ambiguous_without_extension() {
        let csv_id = Uuid::now_v7();
        let xlsx_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "What is in people-100?",
            [(csv_id, "people-100.csv"), (xlsx_id, "people-100.xlsx")],
        );
        assert_eq!(matched, [csv_id, xlsx_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_match_unicode_title_phrase_inside_long_question() {
        let return_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "How do I complete возврат тары and what matters for the empty container?",
            [(return_id, "Возврат тары")],
        );
        assert_eq!(matched, [return_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_match_separator_normalized_document_stems() {
        let monitoring_id = Uuid::now_v7();
        let schema_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "What alert rules are defined in the monitoring dashboard documentation?",
            [(monitoring_id, "monitoring_dashboard.pdf"), (schema_id, "database_schema.pdf")],
        );
        assert_eq!(matched, [monitoring_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_keep_longest_separator_match_canonical() {
        let generic_id = Uuid::now_v7();
        let specific_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "Summarize the monitoring dashboard guide.",
            [
                (generic_id, "monitoring_dashboard.pdf"),
                (specific_id, "monitoring_dashboard_guide.pdf"),
            ],
        );
        assert_eq!(matched, [specific_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_reject_partial_title_token_overlap() {
        let time_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "How should operators register рабочего времени at store opening?",
            [(time_id, "Регистрация рабочего времени")],
        );
        assert!(matched.is_empty());
    }

    #[test]
    fn explicit_target_document_ids_keep_ambiguous_exact_title_matches_tied() {
        let return_container_id = Uuid::now_v7();
        let return_product_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "Explain return process",
            [(return_container_id, "Return process"), (return_product_id, "Return process")],
        );
        assert_eq!(matched, [return_container_id, return_product_id].into_iter().collect());
    }

    #[test]
    fn explicit_target_document_ids_reject_one_token_generic_overlap() {
        let policy_id = Uuid::now_v7();
        let matched = explicit_target_document_ids_from_values(
            "What status should I use?",
            [(policy_id, "Status Policy")],
        );
        assert!(matched.is_empty());
    }

    #[test]
    fn extracts_explicit_document_reference_literals_from_question() {
        assert_eq!(
            explicit_document_reference_literals(
                "What is Shelby Terrell's job title in people-100.csv and what is in sample-heavy-1.xls?"
            ),
            vec!["people-100.csv".to_string(), "sample-heavy-1.xls".to_string()]
        );
    }

    #[test]
    fn explicit_document_reference_literal_matches_path_basename() {
        assert!(explicit_document_reference_literal_is_present(
            "people-100.csv",
            ["exports/archive/people-100.csv"]
        ));
    }
}
