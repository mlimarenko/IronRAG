use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;

pub(crate) fn normalized_alnum_tokens(value: &str, min_chars: usize) -> BTreeSet<String> {
    normalized_alnum_token_sequence(value, min_chars).into_iter().collect()
}

pub(crate) fn normalized_alnum_token_sequence(value: &str, min_chars: usize) -> Vec<String> {
    normalized_alnum_token_sequence_by(value, |token| token.chars().count() >= min_chars, None)
}

pub(crate) fn normalized_alnum_token_sequence_by(
    value: &str,
    mut accept_token: impl FnMut(&str) -> bool,
    max_tokens: Option<usize>,
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut flush_token = |current: &mut String, tokens: &mut Vec<String>| {
        if max_tokens.is_some_and(|limit| tokens.len() >= limit) {
            current.clear();
            return;
        }
        let token = current.trim().to_string();
        current.clear();
        if !accept_token(&token) {
            return;
        }
        if seen.insert(token.clone()) {
            tokens.push(token);
        }
    };

    for ch in value.nfkc().flat_map(char::to_lowercase) {
        if ch.is_alphanumeric() {
            current.push(ch);
        } else {
            flush_token(&mut current, &mut tokens);
        }
    }
    flush_token(&mut current, &mut tokens);
    tokens
}
