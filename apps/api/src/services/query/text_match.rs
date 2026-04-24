use std::collections::BTreeSet;

pub(crate) fn normalized_alnum_tokens(value: &str, min_chars: usize) -> BTreeSet<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|token| token.chars().count() >= min_chars)
        .map(str::to_lowercase)
        .collect()
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
}
