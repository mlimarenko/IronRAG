use super::scope_resolution::{
    document_candidate_is_matchable_for_surface, document_candidate_length_score,
};
use super::{BTreeSet, HashMap, Uuid, current_question_segment};

pub(super) const DOMINANT_DOCUMENT_SCORE_MULTIPLIER: f32 = 1.2;
pub(super) const COMPARISON_DOCUMENT_TARGET_MIN_TOKENS: usize = 2;
pub(super) const COMPARISON_DOCUMENT_TARGET_EXTRA_TOKEN_LIMIT: usize = 2;
const EXPLICIT_DOCUMENT_REFERENCE_EXTENSIONS: &[&str] = &[
    "md", "txt", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "png", "jpg",
    "jpeg",
];
pub(super) const KNOWN_DOCUMENT_LABEL_EXTENSIONS: &[&str] = &[
    "md", "txt", "pdf", "docx", "csv", "tsv", "xls", "xlsx", "xlsb", "ods", "pptx", "png", "jpg",
    "jpeg",
];

#[derive(Debug, Clone)]
pub(super) struct DocumentTargetCandidate {
    pub(super) text: String,
    pub(super) priority: usize,
}

pub(crate) fn explicit_target_document_ids_from_values<'a, I>(
    question: &str,
    values: I,
) -> BTreeSet<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    let question = current_question_segment(question);
    let normalized_question = normalize_document_target_text(question);
    if normalized_question.is_empty() {
        return BTreeSet::new();
    }

    let concrete_values = values.into_iter().collect::<Vec<_>>();
    let explicit_literals = explicit_document_reference_literals(question);
    if !explicit_literals.is_empty() {
        return explicit_document_reference_matching_document_ids(
            &explicit_literals,
            concrete_values.iter().copied(),
        );
    }
    let format_markers = explicit_document_format_markers(&normalized_question, &concrete_values);
    if !format_markers.is_empty() {
        let format_matches = explicit_document_format_matches(
            &normalized_question,
            &concrete_values,
            &format_markers,
        );
        if !format_matches.is_empty() {
            return format_matches;
        }
    }

    let mut best_match_scores = HashMap::<Uuid, (usize, usize)>::new();
    for (document_id, raw_value) in concrete_values {
        for candidate in ranked_document_target_candidates([raw_value]) {
            if document_candidate_is_matchable_for_surface(&candidate.text, &normalized_question)
                && normalized_question_contains_document_candidate(
                    &normalized_question,
                    candidate.text.as_str(),
                    "",
                )
            {
                let score = (document_candidate_length_score(&candidate.text), candidate.priority);
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

pub(super) fn explicit_multi_token_target_document_ids_from_values<'a, I>(
    question: &str,
    values: I,
) -> BTreeSet<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    let question = current_question_segment(question);
    let normalized_question = normalize_document_target_text(question);
    if normalized_question.is_empty() {
        return BTreeSet::new();
    }

    let concrete_values = values.into_iter().collect::<Vec<_>>();
    let targets =
        explicit_target_document_ids_from_values(question, concrete_values.iter().copied());
    if targets.len() != 1 {
        return BTreeSet::new();
    }
    let Some(target_document_id) = targets.iter().next().copied() else {
        return BTreeSet::new();
    };
    let has_multi_token_identity_match = concrete_values
        .iter()
        .filter(|(document_id, _)| *document_id == target_document_id)
        .any(|(_, raw_value)| {
            ranked_document_target_candidates([*raw_value]).into_iter().any(|candidate| {
                document_candidate_match_tokens(&candidate.text).len() >= 2
                    && document_candidate_is_matchable_for_surface(
                        &candidate.text,
                        &normalized_question,
                    )
                    && normalized_question_contains_document_candidate(
                        &normalized_question,
                        candidate.text.as_str(),
                        "",
                    )
            })
        });

    if has_multi_token_identity_match { targets } else { BTreeSet::new() }
}

pub(super) fn explicit_document_reference_target_ids_from_values<'a, I>(
    question: &str,
    values: I,
) -> BTreeSet<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    let question = current_question_segment(question);
    let concrete_values = values.into_iter().collect::<Vec<_>>();
    let explicit_literals = explicit_document_reference_literals(question);
    if !explicit_literals.is_empty() {
        return explicit_document_reference_matching_document_ids(
            &explicit_literals,
            concrete_values.iter().copied(),
        );
    }

    let normalized_question = normalize_document_target_text(question);
    let format_markers = explicit_document_format_markers(&normalized_question, &concrete_values);
    if format_markers.is_empty() {
        BTreeSet::new()
    } else {
        explicit_document_format_matches(&normalized_question, &concrete_values, &format_markers)
    }
}

fn explicit_document_format_markers(
    normalized_question: &str,
    values: &[(Uuid, &str)],
) -> Vec<&'static str> {
    let mut seen = BTreeSet::new();
    normalized_question
        .split_whitespace()
        .filter_map(|token| {
            EXPLICIT_DOCUMENT_REFERENCE_EXTENSIONS.iter().find_map(|extension| {
                (*extension == token).then_some(*extension).and_then(|extension| {
                    values
                        .iter()
                        .any(|(_, value)| {
                            normalized_explicit_document_reference_candidates(value)
                                .into_iter()
                                .any(|candidate| {
                                    candidate.rsplit_once('.').is_some_and(
                                        |(_, extension_in_value)| extension_in_value == extension,
                                    )
                                })
                        })
                        .then_some(extension)
                })
            })
        })
        .filter(|extension| seen.insert(*extension))
        .collect()
}

fn explicit_document_format_matches(
    normalized_question: &str,
    values: &[(Uuid, &str)],
    extensions: &[&'static str],
) -> BTreeSet<Uuid> {
    if extensions.is_empty() {
        return BTreeSet::new();
    }

    let extension_set = extensions.iter().copied().collect::<BTreeSet<_>>();
    values
        .iter()
        .filter_map(|(document_id, raw_value)| {
            document_value_matches_format(normalized_question, raw_value, &extension_set)
                .then_some(*document_id)
        })
        .collect()
}

fn document_value_matches_format(
    normalized_question: &str,
    raw_value: &str,
    extensions: &BTreeSet<&str>,
) -> bool {
    normalized_explicit_document_reference_candidates(raw_value).into_iter().any(|candidate| {
        let Some((stem, extension)) = candidate.rsplit_once('.') else {
            return false;
        };
        extensions.contains(extension)
            && ranked_document_target_candidates([stem, &candidate]).into_iter().any(|candidate| {
                candidate.text.len() >= 4
                    && normalized_question_contains_document_candidate(
                        normalized_question,
                        candidate.text.as_str(),
                        extension,
                    )
            })
    })
}

pub(super) fn normalized_question_contains_document_candidate(
    normalized_question: &str,
    candidate: &str,
    ignored_marker: &str,
) -> bool {
    if normalized_surface_contains_token_sequence(normalized_question, candidate) {
        return true;
    }

    let marker_stripped = normalized_question
        .split_whitespace()
        .filter(|token| *token != ignored_marker)
        .collect::<Vec<_>>()
        .join(" ");
    normalized_surface_contains_token_sequence(&marker_stripped, candidate)
}

fn normalized_surface_contains_token_sequence(surface: &str, candidate: &str) -> bool {
    let surface_tokens = document_candidate_match_tokens(surface);
    let candidate_tokens = document_candidate_match_tokens(candidate);
    if candidate_tokens.is_empty() || candidate_tokens.len() > surface_tokens.len() {
        return false;
    }
    surface_tokens.windows(candidate_tokens.len()).any(|window| window == candidate_tokens)
}

fn document_candidate_match_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|token| token.trim_matches('.'))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
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

pub(super) fn ranked_document_target_candidates<'a, I>(values: I) -> Vec<DocumentTargetCandidate>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = BTreeSet::new();
    let mut candidates = Vec::new();
    for raw in values {
        append_ranked_document_target_candidates(raw, &mut seen, &mut candidates);
    }
    candidates
}

fn append_ranked_document_target_candidates(
    raw: &str,
    seen: &mut BTreeSet<String>,
    candidates: &mut Vec<DocumentTargetCandidate>,
) {
    let normalized = normalize_document_target_text(raw);
    if normalized.is_empty() {
        return;
    }
    push_document_target_candidate(&normalized, 4, seen, candidates);
    if let Some(separator_variant) = separator_normalized_document_target_candidate(&normalized) {
        push_document_target_candidate(&separator_variant, 2, seen, candidates);
    }
    if let Some((stem, _)) = normalized.rsplit_once('.') {
        let stem = stem.trim();
        if !stem.is_empty() {
            push_document_target_candidate(stem, 3, seen, candidates);
            if let Some(separator_variant) = separator_normalized_document_target_candidate(stem) {
                push_document_target_candidate(&separator_variant, 1, seen, candidates);
            }
        }
    }

    // Every `/`, `\`, `:` segment ranks below the full value, so a question
    // naming a path directory or the key half of a `connector:key` identity
    // still resolves the document. Dedup keeps a plain filename unchanged.
    for variant in normalized_document_identity_variants(raw) {
        push_document_target_candidate(&variant, 0, seen, candidates);
    }
}

fn push_document_target_candidate(
    value: &str,
    priority: usize,
    seen: &mut BTreeSet<String>,
    candidates: &mut Vec<DocumentTargetCandidate>,
) {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || !seen.insert(normalized.clone()) {
        return;
    }
    candidates.push(DocumentTargetCandidate { text: normalized, priority });
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
    normalized_document_identity_variants(raw)
}

/// The single identity derivation shared by the explicit-reference presence
/// check and document-focus candidate ranking: the full normalized value plus
/// every segment split by `/`, `\` and `:`, deduped and order-preserving.
fn normalized_document_identity_variants(raw: &str) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut variants = Vec::new();
    for value in std::iter::once(raw).chain(raw.split(['/', '\\', ':'])) {
        let normalized = normalize_document_target_text(value);
        if !normalized.is_empty() && seen.insert(normalized.clone()) {
            variants.push(normalized);
        }
    }
    variants
}

pub(crate) fn normalize_document_target_text(value: &str) -> String {
    value
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_whitespace() || ch == ':' { ' ' } else { ch })
        .filter(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '-' | '_' | ' '))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn explicit_document_reference_literals(question: &str) -> Vec<String> {
    let question = current_question_segment(question);
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
