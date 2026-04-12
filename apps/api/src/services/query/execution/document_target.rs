#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
use std::collections::{BTreeSet, HashMap};

use uuid::Uuid;

const EXPLICIT_DOCUMENT_REFERENCE_EXTENSIONS: &[&str] = &[
    "md", "txt", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "png", "jpg",
    "jpeg",
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
