use super::{BTreeSet, CLARIFY_MAX_VARIANTS, HashSet};

pub(super) fn extract_query_specific_variants(
    user_question: &str,
    retrieved_documents: &[crate::services::query::execution::types::RuntimeRetrievedDocumentBrief],
    context_document_titles: &[String],
    ranked_labels: &[(usize, String)],
) -> Vec<String> {
    let candidate_labels = context_document_titles
        .iter()
        .map(String::as_str)
        .chain(retrieved_documents.iter().map(|document| document.title.as_str()))
        .chain(ranked_labels.iter().map(|(_, label)| label.as_str()))
        .collect::<Vec<_>>();
    let topic_tokens = clarification_focus_tokens(user_question, candidate_labels.iter().copied());
    let mut seen = HashSet::new();
    let mut topical = Vec::new();
    extend_topical_variants(
        context_document_titles.iter().map(String::as_str),
        &topic_tokens,
        &mut seen,
        &mut topical,
    );
    extend_topical_variants(
        retrieved_documents.iter().map(|document| document.title.as_str()),
        &topic_tokens,
        &mut seen,
        &mut topical,
    );
    if topical.is_empty() {
        extend_topical_variants(
            ranked_labels.iter().map(|(_, label)| label.as_str()),
            &topic_tokens,
            &mut seen,
            &mut topical,
        );
    }
    topical
}

pub(super) fn extend_topical_variants<'a>(
    labels: impl Iterator<Item = &'a str>,
    topic_tokens: &BTreeSet<String>,
    seen: &mut HashSet<String>,
    topical: &mut Vec<String>,
) {
    for label in labels.take(CLARIFY_MAX_VARIANTS.saturating_sub(topical.len())) {
        let trimmed = label.trim();
        let Some(dedup_key) = clarify_variant_dedup_key(trimmed) else {
            continue;
        };
        if label_matches_topic_tokens(topic_tokens, trimmed) && seen.insert(dedup_key) {
            topical.push(trimmed.to_string());
        }
    }
}

/// Returns the structural collapse key for a clarification label.
///
/// Attachment-style labels such as `"<page>: <file>.<ext>"` collapse to the
/// parent page, while bare attachment filenames are excluded. The check uses
/// only punctuation and filename shape, so it remains language-neutral.
pub(super) fn clarify_variant_dedup_key(label: &str) -> Option<String> {
    let trimmed = label.trim();
    match trailing_filename_qualifier_prefix(trimmed) {
        // `"X: file.png"` → collapse every same-prefix attachment onto `"x"`.
        Some(prefix) if !prefix.is_empty() => Some(prefix.to_lowercase()),
        // `"file.png"` with no logical prefix, or a bare filename token, is a
        // pure attachment artefact — exclude it from the variant menu.
        Some(_) => None,
        None if token_is_filename_shaped(trimmed) => None,
        None => Some(trimmed.to_lowercase()),
    }
}

/// If `label` ends in a `": <filename>.<ext>"` qualifier, return the logical
/// prefix that precedes the separating `:` (trimmed). `Some("")` means the
/// qualifier consumed the whole label (no logical prefix). `None` means there
/// is no such trailing qualifier.
///
/// "Filename-shaped" is structural: the trailing token after the `:` must
/// match `<one-or-more-non-space>.<2..=5 ascii-alnum>` and contain no path
/// separators — no extension allowlist, no language-specific tokens.
pub(super) fn trailing_filename_qualifier_prefix(label: &str) -> Option<&str> {
    let (prefix, qualifier) = label.rsplit_once(':')?;
    let qualifier = qualifier.trim();
    if !token_is_filename_shaped(qualifier) {
        return None;
    }
    Some(prefix.trim_end())
}

/// `true` when `token` looks like `name.ext` — a non-empty stem, a single
/// trailing `.ext` of 2..=5 ascii-alphanumeric chars, no path separators, and
/// no internal whitespace. Structural shape check only.
pub(super) fn token_is_filename_shaped(token: &str) -> bool {
    if token.is_empty()
        || token.contains('/')
        || token.contains('\\')
        || token.chars().any(char::is_whitespace)
    {
        return false;
    }
    let Some((stem, ext)) = token.rsplit_once('.') else {
        return false;
    };
    if stem.is_empty() {
        return false;
    }
    let ext_len = ext.chars().count();
    (2..=5).contains(&ext_len) && ext.chars().all(|ch| ch.is_ascii_alphanumeric())
}

pub(super) fn clarification_focus_tokens<'a, I>(
    user_question: &str,
    candidate_labels: I,
) -> std::collections::BTreeSet<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let topic_tokens = clarification_topic_tokens(user_question);
    if topic_tokens.len() <= 1 {
        return topic_tokens;
    }

    let label_token_sets = candidate_labels
        .into_iter()
        .map(|label| crate::services::query::text_match::normalized_alnum_tokens(label, 3))
        .filter(|tokens| !tokens.is_empty())
        .collect::<Vec<_>>();
    if label_token_sets.is_empty() {
        return topic_tokens;
    }

    let mut repeated = std::collections::BTreeSet::new();
    let mut discriminating = std::collections::BTreeSet::new();
    for token in &topic_tokens {
        let hit_count = label_token_sets
            .iter()
            .filter(|label_tokens| {
                label_tokens.iter().any(|label_token| {
                    crate::services::query::text_match::near_token_match(token, label_token)
                })
            })
            .count();
        if hit_count >= 2 {
            repeated.insert(token.clone());
        }
        if hit_count >= 2 && hit_count < label_token_sets.len() {
            discriminating.insert(token.clone());
        }
    }

    if !discriminating.is_empty() {
        return discriminating;
    }
    if !repeated.is_empty() {
        return repeated;
    }
    topic_tokens
}

pub(super) fn label_matches_topic_tokens(
    topic_tokens: &std::collections::BTreeSet<String>,
    label: &str,
) -> bool {
    if topic_tokens.is_empty() {
        return false;
    }
    let label_tokens = crate::services::query::text_match::normalized_alnum_tokens(label, 3);
    crate::services::query::text_match::near_token_overlap_count(topic_tokens, &label_tokens) > 0
}

pub(super) fn clarification_topic_tokens(
    user_question: &str,
) -> std::collections::BTreeSet<String> {
    crate::services::query::text_match::normalized_alnum_tokens(user_question, 3)
        .into_iter()
        .collect()
}
