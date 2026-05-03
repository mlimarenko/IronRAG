use std::collections::{BTreeSet, HashMap};

pub(crate) use crate::shared::text_tokens::{
    normalized_alnum_token_sequence, normalized_alnum_tokens,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelatedTokenSelection {
    tokens: BTreeSet<String>,
    allow_near: bool,
}

impl RelatedTokenSelection {
    pub(crate) fn empty() -> Self {
        Self { tokens: BTreeSet::new(), allow_near: false }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    pub(crate) fn matches_tokens(&self, label_tokens: &BTreeSet<String>) -> bool {
        if self.allow_near {
            return self.tokens.iter().any(|target_token| {
                label_tokens.iter().any(|label_token| near_token_match(target_token, label_token))
            });
        }
        self.tokens.iter().any(|target_token| label_tokens.contains(target_token))
    }
}

pub(crate) fn token_sequence_contains(haystack: &str, needle: &str, min_chars: usize) -> bool {
    let needle_tokens = normalized_alnum_token_sequence(needle, min_chars);
    if needle_tokens.is_empty() {
        return false;
    }
    let haystack_tokens = normalized_alnum_token_sequence(haystack, min_chars);
    if haystack_tokens.len() < needle_tokens.len() {
        return false;
    }
    haystack_tokens.windows(needle_tokens.len()).any(|window| window == needle_tokens)
}

pub(crate) fn token_sequence_exact_or_contains(left: &str, right: &str, min_chars: usize) -> bool {
    token_sequence_contains(left, right, min_chars)
        || token_sequence_contains(right, left, min_chars)
}

pub(crate) fn near_token_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    if left_len < 5 || right_len < 5 || left_len.abs_diff(right_len) > 1 {
        return false;
    }
    if left.chars().next() != right.chars().next() {
        return false;
    }
    bounded_edit_distance_at_most_one(left, right)
}

pub(crate) fn near_token_overlap_count(left: &BTreeSet<String>, right: &BTreeSet<String>) -> usize {
    left.iter()
        .filter(|left_token| {
            right.iter().any(|right_token| near_token_match(left_token, right_token))
        })
        .count()
}

pub(crate) fn select_related_overlap_tokens<I, S>(
    target_label: &str,
    candidate_labels: I,
    min_chars: usize,
) -> RelatedTokenSelection
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let target_sequence = normalized_alnum_token_sequence(target_label, min_chars);
    if target_sequence.is_empty() {
        return RelatedTokenSelection::empty();
    }
    let target_tokens = target_sequence.iter().cloned().collect::<BTreeSet<_>>();
    let mut exact_frequencies =
        target_tokens.iter().map(|token| (token.clone(), 0usize)).collect::<HashMap<_, _>>();
    let mut near_frequencies = exact_frequencies.clone();

    for candidate_label in candidate_labels {
        let label = candidate_label.as_ref().trim();
        if label.is_empty() {
            continue;
        }
        if token_sequence_exact_or_contains(label, target_label, min_chars) {
            continue;
        }
        let label_tokens = normalized_alnum_tokens(label, min_chars);
        if label_tokens.is_empty() {
            continue;
        }
        for target_token in &target_tokens {
            if label_tokens.contains(target_token) {
                *exact_frequencies.entry(target_token.clone()).or_insert(0) += 1;
            } else if label_tokens
                .iter()
                .any(|label_token| near_token_match(target_token, label_token))
            {
                *near_frequencies.entry(target_token.clone()).or_insert(0) += 1;
            }
        }
    }

    if let Some(tokens) = select_min_frequency_tokens(&exact_frequencies, &target_sequence) {
        return RelatedTokenSelection { tokens, allow_near: false };
    }
    if let Some(tokens) = select_min_frequency_tokens(&near_frequencies, &target_sequence) {
        return RelatedTokenSelection { tokens, allow_near: true };
    }
    RelatedTokenSelection::empty()
}

fn select_min_frequency_tokens(
    frequencies: &HashMap<String, usize>,
    target_sequence: &[String],
) -> Option<BTreeSet<String>> {
    let min_positive_frequency =
        frequencies.values().copied().filter(|frequency| *frequency > 0).min()?;
    let mut selected = BTreeSet::new();
    if let Some(token) = target_sequence
        .iter()
        .rev()
        .find(|token| frequencies.get(*token).copied() == Some(min_positive_frequency))
    {
        selected.insert(token.clone());
    }
    if selected.is_empty() { None } else { Some(selected) }
}

fn bounded_edit_distance_at_most_one(left: &str, right: &str) -> bool {
    let left_chars = left.chars().collect::<Vec<_>>();
    let right_chars = right.chars().collect::<Vec<_>>();
    if left_chars == right_chars {
        return true;
    }
    match left_chars.len().cmp(&right_chars.len()) {
        std::cmp::Ordering::Equal => {
            left_chars.iter().zip(right_chars.iter()).filter(|(left, right)| left != right).count()
                <= 1
        }
        std::cmp::Ordering::Less => one_insert_or_delete_apart(&left_chars, &right_chars),
        std::cmp::Ordering::Greater => one_insert_or_delete_apart(&right_chars, &left_chars),
    }
}

fn one_insert_or_delete_apart(shorter: &[char], longer: &[char]) -> bool {
    if longer.len() != shorter.len() + 1 {
        return false;
    }
    let mut short_index = 0;
    let mut long_index = 0;
    let mut edits = 0;
    while short_index < shorter.len() && long_index < longer.len() {
        if shorter[short_index] == longer[long_index] {
            short_index += 1;
            long_index += 1;
        } else {
            edits += 1;
            if edits > 1 {
                return false;
            }
            long_index += 1;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn near_token_match_accepts_single_edit_for_long_tokens() {
        assert!(near_token_match("targetnme", "targetname"));
        assert!(near_token_match("paymant", "payment"));
    }

    #[test]
    fn near_token_match_rejects_short_or_distant_tokens() {
        assert!(!near_token_match("api", "app"));
        assert!(!near_token_match("target", "payment"));
    }

    #[test]
    fn token_sequence_exact_or_contains_rejects_embedded_short_labels() {
        assert!(token_sequence_exact_or_contains("Project Omega", "Omega", 3));
        assert!(token_sequence_exact_or_contains("Project Omega", "Project Omega", 3));
        assert!(!token_sequence_exact_or_contains("Sasha Otoya", "OTO", 3));
    }

    #[test]
    fn normalized_tokens_use_unicode_compatibility_case_folding() {
        assert_eq!(
            normalized_alnum_token_sequence("ＣＡＦÉ ΔELTA alpha-beta", 3),
            vec!["café".to_string(), "δelta".to_string(), "alpha".to_string(), "beta".to_string()]
        );
    }

    #[test]
    fn related_overlap_prefers_exact_rare_token_over_near_name_noise() {
        let selection = select_related_overlap_tokens(
            "Alpha Omega",
            ["Omega Delta", "Alphb Person", "Alpha Team"],
            3,
        );
        assert!(selection.matches_tokens(&normalized_alnum_tokens("Omega Delta", 3)));
        assert!(!selection.matches_tokens(&normalized_alnum_tokens("Alphb Person", 3)));
        assert!(!selection.matches_tokens(&normalized_alnum_tokens("Alpha Team", 3)));
    }

    #[test]
    fn related_overlap_allows_near_match_when_no_exact_token_candidate_exists() {
        let selection = select_related_overlap_tokens("Omegax", ["Omega Delta"], 3);
        assert!(selection.matches_tokens(&normalized_alnum_tokens("Omega Delta", 3)));
    }
}
