use std::collections::BTreeSet;

const MAX_STRUCTURAL_DECLARATION_KEY_CHARS: usize = 160;

/// Returns whether a query identifier is named by a declaration-shaped line
/// whose block either contains an indented body or uses flattened brace lines.
///
/// This is intentionally a minimal, language-neutral shape check rather than a
/// parser. Search scoring and source excerpt selection share it so declaration
/// evidence cannot drift across the two ranking stages.
pub(crate) fn content_declares_any_identifier(content: &str, identifiers: &[String]) -> bool {
    if identifiers.is_empty() {
        return false;
    }
    let lines = content.lines().collect::<Vec<_>>();
    lines.iter().enumerate().any(|(index, line)| {
        line_is_structural_declaration(&lines, index)
            && identifiers
                .iter()
                .any(|identifier| declaration_line_names_identifier(line, identifier))
    })
}

pub(crate) fn matching_structural_declaration_identifiers(
    content: &str,
    mut matches: impl FnMut(&str) -> bool,
    limit: usize,
) -> Vec<String> {
    if limit == 0 {
        return Vec::new();
    }
    let lines = content.lines().collect::<Vec<_>>();
    let mut identifiers = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        if !line_is_structural_declaration(&lines, index) {
            continue;
        }
        let Some(identifier) = declaration_identifier(line) else {
            continue;
        };
        let identifier = identifier.to_string();
        if matches(&identifier) && seen.insert(identifier.clone()) {
            identifiers.push(identifier);
            if identifiers.len() >= limit {
                break;
            }
        }
    }
    identifiers
}

fn line_is_structural_declaration(lines: &[&str], index: usize) -> bool {
    structural_declaration_line_has_shape(lines[index])
        && (structural_declaration_has_indented_body(lines, index)
            || structural_declaration_has_flattened_brace_block(lines, index))
}

fn structural_declaration_has_flattened_brace_block(lines: &[&str], index: usize) -> bool {
    lines[index].trim_end().ends_with('{')
        && lines
            .iter()
            .skip(index.saturating_add(1))
            .any(|line| flattened_line_closes_brace_block(line))
}

fn flattened_line_closes_brace_block(line: &str) -> bool {
    line.trim()
        .strip_prefix('}')
        .is_some_and(|suffix| suffix.chars().all(|character| matches!(character, ',' | ';')))
}

pub(crate) fn declaration_block_end(
    lines: &[&str],
    start: usize,
    declaration_depth: usize,
) -> usize {
    lines
        .iter()
        .enumerate()
        .skip(start.saturating_add(1))
        .find_map(|(index, line)| {
            (!line.trim().is_empty() && line_indentation_width(line) <= declaration_depth)
                .then_some(index)
        })
        .unwrap_or(lines.len())
}

pub(crate) fn declaration_has_indented_body(
    lines: &[&str],
    start: usize,
    end: usize,
    declaration_depth: usize,
) -> bool {
    lines[start.saturating_add(1)..end]
        .iter()
        .any(|line| !line.trim().is_empty() && line_indentation_width(line) > declaration_depth)
}

pub(crate) fn structural_declaration_line_matches_focus(
    line: &str,
    normalized_keywords: &[String],
) -> bool {
    if !structural_declaration_line_has_shape(line) {
        return false;
    }
    let trimmed = line.trim();
    let focus_surface = trimmed.split_once('(').map_or(trimmed, |(prefix, _)| prefix.trim_end());
    declaration_key_matches_focus(focus_surface, normalized_keywords)
}

pub(crate) fn structural_declaration_line_has_shape(line: &str) -> bool {
    let trimmed = line.trim();
    structural_declaration_surface_is_eligible(trimmed)
        && !trimmed.ends_with([';', ','])
        && trimmed.chars().any(|character| matches!(character, '(' | '{' | ':' | '=' | '&'))
}

pub(crate) fn structural_declaration_has_indented_body(lines: &[&str], index: usize) -> bool {
    let declaration_depth = line_indentation_width(lines[index]);
    let end = declaration_block_end(lines, index, declaration_depth);
    declaration_has_indented_body(lines, index, end, declaration_depth)
}

pub(crate) fn structural_declaration_surface_is_eligible(surface: &str) -> bool {
    !surface.is_empty()
        && surface.chars().count() <= MAX_STRUCTURAL_DECLARATION_KEY_CHARS
        && !surface.starts_with(['#', '-', '*', '+'])
        && !surface.contains("://")
        && surface.chars().any(char::is_alphanumeric)
}

pub(crate) fn declaration_key_matches_focus(key: &str, normalized_keywords: &[String]) -> bool {
    let key_tokens = key
        .split(|character: char| !character.is_alphanumeric() && character != '_')
        .filter(|token| !token.is_empty())
        .map(str::to_lowercase)
        .collect::<BTreeSet<_>>();
    normalized_keywords.iter().any(|keyword| {
        keyword
            .split(|character: char| !character.is_alphanumeric() && character != '_')
            .filter(|token| !token.is_empty())
            .any(|token| key_tokens.contains(&token.to_lowercase()))
    })
}

pub(crate) fn line_indentation_width(line: &str) -> usize {
    line.chars().take_while(|character| character.is_whitespace()).count()
}

fn declaration_line_names_identifier(line: &str, identifier: &str) -> bool {
    let identifier = identifier.trim();
    if identifier.is_empty() {
        return false;
    }
    let declaration_surface = declaration_name_surface(line).to_lowercase();
    let identifier = identifier.to_lowercase();
    declaration_surface.match_indices(&identifier).any(|(start, _)| {
        let end = start + identifier.len();
        let left_is_boundary = declaration_surface[..start]
            .chars()
            .next_back()
            .is_none_or(|character| !is_identifier_continuation(character));
        let right_is_boundary = declaration_surface[end..]
            .chars()
            .next()
            .is_none_or(|character| !is_identifier_continuation(character));
        left_is_boundary && right_is_boundary
    })
}

fn declaration_name_surface(line: &str) -> &str {
    let trimmed = line.trim();
    let value_boundary = trimmed
        .char_indices()
        .filter(|(index, character)| {
            matches!(character, '=' | '{' | '&')
                || (*character == ':' && !colon_is_path_separator(trimmed, *index))
        })
        .map(|(index, _)| index)
        .min()
        .unwrap_or(trimmed.len());
    let header = trimmed[..value_boundary].trim_end();
    callable_name_surface(header)
}

fn declaration_identifier(line: &str) -> Option<&str> {
    declaration_name_surface(line)
        .rsplit(|character: char| !is_declaration_identifier_character(character))
        .find(|candidate| !candidate.is_empty() && candidate.chars().any(char::is_alphanumeric))
}

fn is_declaration_identifier_character(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '.')
}

fn callable_name_surface(header: &str) -> &str {
    let mut search_start = 0usize;
    if header.starts_with("pub(")
        && let Some(visibility_end) = header.find(')')
    {
        search_start = visibility_end.saturating_add(1);
    }
    header[search_start..]
        .find('(')
        .map_or(header, |relative_index| &header[..search_start + relative_index])
        .trim_end()
}

fn colon_is_path_separator(surface: &str, index: usize) -> bool {
    surface[..index].ends_with(':') || surface[index.saturating_add(1)..].starts_with(':')
}

fn is_identifier_continuation(character: char) -> bool {
    character.is_alphanumeric() || matches!(character, '_' | '-' | '.' | '/' | ':')
}

#[cfg(test)]
mod tests {
    use super::{content_declares_any_identifier, matching_structural_declaration_identifiers};

    #[test]
    fn declaration_match_requires_the_identifier_in_the_declared_surface_and_an_indented_body() {
        let identifiers = vec!["AlphaFault".to_string()];

        assert!(content_declares_any_identifier(
            "enum AlphaFault {\n    Missing,\n    Invalid,\n}",
            &identifiers,
        ));
        assert!(!content_declares_any_identifier(
            "handler: AlphaFault\n  retries: 3",
            &identifiers,
        ));
        assert!(!content_declares_any_identifier("enum AlphaFault {}", &identifiers));
    }

    #[test]
    fn declaration_match_handles_visibility_and_callable_declarations() {
        let identifiers = vec!["AlphaFault".to_string(), "build_alpha".to_string()];

        assert!(content_declares_any_identifier(
            "pub(crate) enum AlphaFault {\n    Missing,\n}",
            &identifiers,
        ));
        assert!(content_declares_any_identifier(
            "fn build_alpha(input: AlphaFault) {\n    consume(input);\n}",
            &identifiers,
        ));
    }

    #[test]
    fn flattened_live_chunk_declares_order_error() {
        let content =
            include_str!("../../tests/fixtures/structural_declarations/live_chunk_sample.txt");
        let identifiers = vec!["OrderError".to_string()];

        assert!(content_declares_any_identifier(content, &identifiers));
    }

    #[test]
    fn flattened_yaml_value_does_not_declare_identifier() {
        let content = "data:\n\nETCD_ENDPOINTS: http://etcd.internal/OrderError\n\nreplicas: 3";
        let identifiers = vec!["OrderError".to_string()];

        assert!(!content_declares_any_identifier(content, &identifiers));
    }

    #[test]
    fn matching_declaration_identifiers_copy_the_declared_spelling() {
        let content = concat!(
            "class RequestGuard:\n",
            "    state: GuardState\n",
            "class OtherGuard:\n",
            "    state: GuardState",
        );

        assert_eq!(
            matching_structural_declaration_identifiers(
                content,
                |identifier| identifier.eq_ignore_ascii_case("requestguard"),
                4,
            ),
            vec!["RequestGuard".to_string()],
        );

        let flattened =
            include_str!("../../tests/fixtures/structural_declarations/live_chunk_sample.txt");
        assert_eq!(
            matching_structural_declaration_identifiers(
                flattened,
                |identifier| identifier == "OrderError",
                4,
            ),
            vec!["OrderError".to_string()],
        );
    }
}
