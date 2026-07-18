use std::collections::BTreeSet;

use unicode_normalization::UnicodeNormalization;

pub(crate) fn normalized_alnum_tokens(value: &str, min_chars: usize) -> BTreeSet<String> {
    normalized_alnum_token_sequence(value, min_chars).into_iter().collect()
}

/// Returns the text found between each matching pair of backticks in
/// `text`, in order. An unterminated trailing backtick is ignored (no
/// closing tick to pair it with). Shared by the answer-provenance literal
/// detector (MCP `grounded_answer`) and the agent loop's own literal
/// detection, which both need to recognize inline-code spans the same way.
pub(crate) fn backtick_literal_spans(text: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut span_start: Option<usize> = None;
    for (index, ch) in text.char_indices() {
        if ch != '`' {
            continue;
        }
        if let Some(start) = span_start.take() {
            if start < index {
                spans.push(text[start..index].to_string());
            }
        } else {
            span_start = Some(index + ch.len_utf8());
        }
    }
    spans
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

pub(crate) fn literal_wildcard_prefixes(value: &str, min_alnum_chars: usize) -> Vec<String> {
    let normalized = value.nfkc().flat_map(char::to_lowercase).collect::<String>();
    let mut seen = BTreeSet::new();
    let mut prefixes = Vec::new();

    for fragment in normalized.split_whitespace() {
        let Some(wildcard_index) = fragment.find('*') else {
            continue;
        };
        let candidate = &fragment[..wildcard_index];
        let prefix = candidate.trim_matches(|ch: char| {
            !ch.is_alphanumeric() && !is_literal_wildcard_prefix_separator(ch)
        });
        if prefix.is_empty() {
            continue;
        }
        let alnum_chars = prefix.chars().filter(|ch| ch.is_alphanumeric()).count();
        if alnum_chars < min_alnum_chars {
            continue;
        }
        if seen.insert(prefix.to_string()) {
            prefixes.push(prefix.to_string());
        }
    }

    prefixes
}

const fn is_literal_wildcard_prefix_separator(ch: char) -> bool {
    matches!(ch, '-' | '_' | '.' | '/' | ':' | '@')
}

#[cfg(test)]
mod tests {
    use super::literal_wildcard_prefixes;

    #[test]
    fn wildcard_prefixes_keep_structural_prefixes_without_language_lists() {
        assert_eq!(
            literal_wildcard_prefixes("show `alpha-*` and beta_module* entries", 2),
            vec!["alpha-".to_string(), "beta_module".to_string()]
        );
    }

    #[test]
    fn wildcard_prefixes_reject_unanchored_suffix_patterns() {
        assert!(literal_wildcard_prefixes("show *.pdf and *", 2).is_empty());
    }
}
