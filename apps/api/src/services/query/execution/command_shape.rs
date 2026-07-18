//! Domain-neutral structural recognition for shell-like procedure evidence.

pub(super) fn content_is_command_dense(content: &str) -> bool {
    let mut non_empty_lines = 0usize;
    let mut command_lines = 0usize;
    let mut artifact_lines = 0usize;
    for line in content.lines().map(str::trim).filter(|line| !line.is_empty()) {
        non_empty_lines = non_empty_lines.saturating_add(1);
        if procedure_line_has_command_start(line) {
            command_lines = command_lines.saturating_add(1);
        }
        if procedure_artifact_token_count(line) > 0 {
            artifact_lines = artifact_lines.saturating_add(1);
        }
    }
    command_lines >= 2
        || (command_lines >= 1 && artifact_lines >= 2)
        || (non_empty_lines >= 3 && command_lines >= 1 && artifact_lines >= 1)
}

pub(super) fn procedure_line_has_command_start(line: &str) -> bool {
    let has_order_marker = procedure_line_has_explicit_order_marker(line);
    let trimmed = strip_leading_procedure_order_marker(line).trim();
    let has_code_delimiter = procedure_line_is_code_delimited(trimmed);
    let tokens = shellish_tokens_from_text(trimmed);
    shellish_tokens_start_command_with_context(&tokens, has_order_marker || has_code_delimiter)
}

pub(super) fn strip_leading_procedure_order_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    let trimmed = trimmed.strip_prefix(['-', '*', '•']).unwrap_or(trimmed).trim_start();
    strip_explicit_numeric_order_marker(trimmed).unwrap_or(trimmed)
}

pub(super) fn strip_leading_numeric_order_marker(line: &str) -> &str {
    let trimmed = line.trim();
    strip_explicit_numeric_order_marker(trimmed).unwrap_or(trimmed)
}

fn procedure_line_has_explicit_order_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    let trimmed = trimmed.strip_prefix(['-', '*', '•']).unwrap_or(trimmed).trim_start();
    strip_explicit_numeric_order_marker(trimmed).is_some()
}

pub(super) fn procedure_line_has_list_marker(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with(['-', '*', '•']) || strip_explicit_numeric_order_marker(trimmed).is_some()
}

fn strip_explicit_numeric_order_marker(trimmed: &str) -> Option<&str> {
    let mut chars = trimmed.char_indices().peekable();
    let mut has_digit = false;
    while let Some((_index, ch)) = chars.peek().copied() {
        if ch.is_ascii_digit() {
            has_digit = true;
            chars.next();
        } else {
            break;
        }
    }
    if !has_digit {
        return None;
    }
    let (marker_index, marker) = chars.next()?;
    if !matches!(marker, '.' | ')') {
        return None;
    }
    let remainder = &trimmed[marker_index + marker.len_utf8()..];
    (remainder.is_empty() || remainder.starts_with(char::is_whitespace))
        .then(|| remainder.trim_start())
}

fn procedure_line_is_code_delimited(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.len() >= 2
        && trimmed.starts_with('`')
        && trimmed.ends_with('`')
        && !trimmed.trim_matches('`').trim().is_empty()
}

pub(super) fn procedure_artifact_token_count(text: &str) -> usize {
    text.split_whitespace().filter(|token| procedure_artifact_token(token)).count()
}

fn procedure_artifact_token(token: &str) -> bool {
    let token = clean_shellish_token(token);
    if !token.chars().any(char::is_alphanumeric) {
        return false;
    }
    token.contains('/')
        || token.contains('\\')
        || token.contains("--")
        || token.contains('=')
        || token.starts_with("./")
        || token.chars().filter(|ch| *ch == '.').count() >= 2
}

pub(super) fn shellish_inline_token_starts_command(tokens: &[String], index: usize) -> bool {
    let Some(token) = tokens.get(index).map(String::as_str) else {
        return false;
    };
    (shellish_token_has_executable_name_shape(token)
        && shellish_tokens_have_structural_command_shape(&tokens[index..]))
        || (shellish_token_is_path_command_start(token) && shellish_token_is_local_artifact(token))
}

pub(super) fn shellish_tokens_start_command(tokens: &[String]) -> bool {
    shellish_tokens_start_command_with_context(tokens, false)
}

fn shellish_tokens_start_command_with_context(
    tokens: &[String],
    has_explicit_context: bool,
) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    shellish_token_is_path_command_start(first)
        || shellish_tokens_have_structural_command_shape(tokens)
        || (has_explicit_context && shellish_token_is_invocable_head(first))
}

pub(super) fn shellish_tokens_from_text(text: &str) -> Vec<String> {
    text.split_whitespace()
        .flat_map(clean_shellish_token_expansions)
        .filter(|token| !token.is_empty())
        .collect()
}

fn clean_shellish_token_expansions(token: &str) -> Vec<String> {
    let cleaned = clean_shellish_token(token);
    if cleaned.is_empty() {
        return Vec::new();
    }
    expand_shellish_token(&cleaned)
}

fn expand_shellish_token(token: &str) -> Vec<String> {
    split_concatenated_local_artifact_token(token).into_iter().map(str::to_string).collect()
}

pub(super) fn split_concatenated_local_artifact_token(token: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0usize;
    for (index, _) in token.char_indices().skip(1) {
        let rest = &token[index..];
        let artifact = &token[start..index];
        if (rest.starts_with('/') || rest.starts_with("./"))
            && shellish_token_is_path_command_start(artifact)
            && shellish_token_file_artifact_name(artifact).is_some()
            && suffix_is_exact_artifact_repetition(artifact, rest)
        {
            segments.push(artifact);
            start = index;
        }
    }
    if start == 0 {
        return vec![token];
    }
    segments.push(&token[start..]);
    segments
}

fn suffix_is_exact_artifact_repetition(artifact: &str, mut suffix: &str) -> bool {
    if artifact.is_empty() {
        return false;
    }
    let mut repetitions = 0usize;
    while let Some(remainder) = suffix.strip_prefix(artifact) {
        repetitions = repetitions.saturating_add(1);
        suffix = remainder;
    }
    repetitions > 0 && suffix.is_empty()
}

fn clean_shellish_token(token: &str) -> String {
    let cleaned = token
        .trim_matches(|ch: char| {
            ch.is_ascii_punctuation() && !matches!(ch, '/' | '.' | '-' | '_' | '+' | '=' | ':')
        })
        .trim_matches('\u{200e}')
        .trim_matches('\u{200f}')
        .to_ascii_lowercase();
    if cleaned.chars().any(char::is_alphanumeric) { cleaned } else { String::new() }
}

pub(super) fn shellish_token_has_external_artifact(token: &str) -> bool {
    token.contains("://")
}

pub(super) fn shellish_token_is_local_artifact(token: &str) -> bool {
    shellish_token_is_path_command_start(token)
        || shellish_token_file_artifact_name(token).is_some()
}

pub(super) fn shellish_token_file_artifact_name(token: &str) -> Option<&str> {
    let file_name = token
        .rsplit('/')
        .next()?
        .split(['?', '#'])
        .next()?
        .trim_end_matches(|ch: char| ch.is_ascii_punctuation());
    let has_extension = file_name
        .rsplit_once('.')
        .is_some_and(|(_, extension)| (2..=12).contains(&extension.len()));
    let has_structural_name = file_name.contains('-')
        || file_name.contains('_')
        || file_name.chars().any(|ch| ch.is_ascii_digit());
    (has_extension || has_structural_name).then_some(file_name)
}

pub(super) fn shellish_token_has_artifact_preparation_signal(token: &str) -> bool {
    token.starts_with('+') || token.chars().all(|ch| ch.is_ascii_digit()) || token.starts_with('-')
}

pub(super) fn shellish_token_is_path_command_start(token: &str) -> bool {
    (token.starts_with("./")
        || token.starts_with("../")
        || token.starts_with('/')
        || token.contains('/')
        || token.contains('\\'))
        && token.chars().any(char::is_alphanumeric)
        && !token.contains("://")
}

pub(super) fn shellish_tokens_have_structural_command_shape(tokens: &[String]) -> bool {
    let Some(head) = tokens.first() else {
        return false;
    };
    if !shellish_token_is_invocable_head(head) {
        return false;
    }
    let Some((signal_index, signal)) =
        tokens.iter().skip(1).take(8).enumerate().find(|(_, token)| {
            shellish_token_is_command_argument_signal(token)
                || shellish_token_is_local_artifact(token)
        })
    else {
        return false;
    };
    let executable_head = shellish_token_has_executable_name_shape(head);
    (shellish_token_is_command_argument_signal(signal) && (signal_index == 0 || executable_head))
        || (shellish_token_is_local_artifact(signal) && executable_head)
}

pub(super) fn shellish_token_is_invocable_head(token: &str) -> bool {
    !token.is_empty()
        && !token.starts_with('-')
        && !token.contains("://")
        && !token.contains('=')
        && token.chars().any(char::is_alphabetic)
        && token
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '-' | '+' | '/' | '\\'))
}

pub(super) fn shellish_token_has_executable_name_shape(token: &str) -> bool {
    token.contains('-')
        || token.contains('_')
        || token.contains('.')
        || token.contains('/')
        || token.contains('\\')
        || token.chars().any(|ch| ch.is_ascii_digit())
}

pub(super) fn shellish_token_is_command_argument_signal(token: &str) -> bool {
    token.starts_with('-')
        || token.contains("--")
        || token.contains('=')
        || token.contains('/')
        || token.contains('\\')
        || token.contains('|')
        || token.contains("://")
}

#[cfg(test)]
mod tests {
    use super::{
        procedure_line_has_command_start, procedure_line_has_list_marker,
        shellish_inline_token_starts_command, shellish_tokens_from_text,
        shellish_tokens_start_command, strip_leading_numeric_order_marker,
        strip_leading_procedure_order_marker,
    };

    #[test]
    fn structural_command_shape_is_independent_of_executable_spelling() {
        for executable in ["alpha-tool", "beta_tool", "./gamma"] {
            let tokens =
                shellish_tokens_from_text(&format!("{executable} --mode=strict /work/item.bin"));
            assert!(shellish_tokens_start_command(&tokens), "{tokens:?}");
        }
    }

    #[test]
    fn plain_words_are_not_promoted_without_formal_command_evidence() {
        for text in [
            "alpha",
            "alpha beta",
            "alpha beta gamma",
            "well-known limitation",
            "Alpha-2 remains stable",
            "Release version 2.0",
            "QR-code is not performed automatically.",
            "address should be configured as https://example.invalid/object",
            "refer to /work/item for details",
        ] {
            let tokens = shellish_tokens_from_text(text);
            assert!(!shellish_tokens_start_command(&tokens), "{tokens:?}");
            assert!(!procedure_line_has_command_start(text), "{text}");
        }
    }

    #[test]
    fn explicit_structure_preserves_ordered_path_flag_assignment_uri_and_code_steps() {
        for text in [
            "1. /work/runner",
            "2. runner --mode=strict",
            "3. runner mode=strict",
            "4. runner https://example.invalid/object",
            "`runner apply`",
        ] {
            assert!(procedure_line_has_command_start(text), "{text}");
        }
    }

    #[test]
    fn shellish_token_expansion_preserves_paths_and_splits_only_repeated_artifacts() {
        assert_eq!(
            shellish_tokens_from_text("copy-tool /work/source.bin /work/target.bin"),
            ["copy-tool", "/work/source.bin", "/work/target.bin"]
        );
        assert_eq!(
            shellish_tokens_from_text("/work/update-token.sh/work/update-token.sh"),
            ["/work/update-token.sh", "/work/update-token.sh"]
        );
    }

    #[test]
    fn decimal_prefix_is_not_treated_as_an_order_marker() {
        let text = "1.2 remains stable";
        assert!(!procedure_line_has_list_marker(text));
        assert_eq!(strip_leading_procedure_order_marker(text), text);
        assert_eq!(strip_leading_numeric_order_marker(text), text);
        assert!(!procedure_line_has_command_start(text));
    }

    #[test]
    fn inline_boundary_uses_options_and_paths_instead_of_command_names() {
        let tokens = shellish_tokens_from_text(
            "introductory prose runner-tool --mode=strict /work/item.bin",
        );
        assert!(!shellish_inline_token_starts_command(&tokens, 0));
        assert!(!shellish_inline_token_starts_command(&tokens, 1));
        assert!(shellish_inline_token_starts_command(&tokens, 2));
    }

    #[test]
    fn immediate_local_artifact_is_a_structural_command_argument() {
        let tokens = shellish_tokens_from_text(
            "context words runner-tool item-bundle next-tool --mode=strict",
        );

        assert!(!shellish_inline_token_starts_command(&tokens, 0));
        assert!(!shellish_inline_token_starts_command(&tokens, 1));
        assert!(shellish_inline_token_starts_command(&tokens, 2));
        assert!(shellish_inline_token_starts_command(&tokens, 4));
    }
}
