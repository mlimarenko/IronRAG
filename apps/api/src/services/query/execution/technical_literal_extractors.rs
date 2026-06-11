use std::{collections::HashSet, sync::LazyLock};

use super::technical_literals::trim_literal_token;

static PACKAGE_COMMAND_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    regex::RegexBuilder::new(
        r"(?im)(?:^|[\s:;])(?:aptitude\s+install|apt(?:-get)?\s+install|dpkg-reconfigure)\s+([A-Za-z0-9][A-Za-z0-9_.+-]{1,160})(?:$|[\s.,;:)])",
    )
    .build()
    .expect("package command regex must compile")
});

static CONFIG_ASSIGNMENT_LITERAL_REGEX: LazyLock<regex::Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    regex::RegexBuilder::new(
        r"(?m)(?:^|[`;\r\n])\s*[#;]?\s*([A-Za-z][A-Za-z0-9_.-]{1,160})\s*=\s*([^`;\r\n]{1,220})",
    )
    .build()
    .expect("config assignment literal regex must compile")
});

pub(super) fn push_unique_limited(
    target: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: String,
    limit: usize,
) {
    if value.is_empty() || target.len() >= limit {
        return;
    }
    if seen.insert(value.clone()) {
        target.push(value);
    }
}

pub(super) fn extract_url_literals(text: &str, limit: usize) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        let trailing_open_placeholder = cleaned.rfind('<').is_some_and(|left_index| {
            cleaned.rfind('>').is_none_or(|right_index| left_index > right_index)
        });
        let has_unbalanced_angle_brackets = (cleaned.contains('<') && !cleaned.contains('>'))
            || (cleaned.contains('>') && !cleaned.contains('<'));
        if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
            if !has_unbalanced_angle_brackets && !trailing_open_placeholder {
                push_unique_limited(&mut urls, &mut seen, cleaned.to_string(), limit);
            }
        }
    }
    urls
}

pub(super) fn derive_path_literals_from_url(url: &str) -> Vec<String> {
    let Some(scheme_index) = url.find("://") else {
        return Vec::new();
    };
    let remainder = &url[(scheme_index + 3)..];
    let Some(path_index) = remainder.find('/') else {
        return Vec::new();
    };
    let path = &remainder[path_index..];
    if path.is_empty() {
        return Vec::new();
    }

    let mut paths = vec![path.to_string()];
    let segments =
        path.trim_matches('/').split('/').filter(|segment| !segment.is_empty()).collect::<Vec<_>>();
    if segments.len() >= 2 {
        paths.push(format!("/{}/{}/", segments[0], segments[1]));
    }
    if segments.len() >= 3 && !segments[2].contains('.') {
        paths.push(format!("/{}/{}/{}/", segments[0], segments[1], segments[2]));
    }
    paths
}

pub(super) fn extract_explicit_path_literals(text: &str, limit: usize) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        if cleaned.starts_with('/') && cleaned.matches('/').count() >= 1 {
            push_unique_limited(&mut paths, &mut seen, cleaned.to_string(), limit);
        }
    }

    if paths.is_empty() {
        for url in extract_url_literals(text, limit.saturating_mul(2).max(4)) {
            if let Some(full_path) = derive_path_literals_from_url(&url).into_iter().next() {
                push_unique_limited(&mut paths, &mut seen, full_path, limit);
            }
        }
    }

    paths
}

pub(super) fn extract_package_command_literals(text: &str, limit: usize) -> Vec<String> {
    let mut packages = Vec::new();
    let mut seen = HashSet::new();
    for capture in PACKAGE_COMMAND_REGEX.captures_iter(text) {
        let Some(package) = capture
            .get(1)
            .map(|value| value.as_str().trim().trim_end_matches(['.', ',', ';', ':', ')']))
        else {
            continue;
        };
        push_unique_limited(&mut packages, &mut seen, package.to_string(), limit);
    }
    packages
}

pub(super) fn extract_prefix_literals(text: &str, limit: usize) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut seen = HashSet::new();

    for url in extract_url_literals(text, limit.saturating_mul(2).max(4)) {
        for candidate in derive_path_literals_from_url(&url) {
            if candidate.ends_with('/') {
                push_unique_limited(&mut prefixes, &mut seen, candidate, limit);
            }
        }
    }

    prefixes
}

#[cfg(test)]
pub(super) fn extract_protocol_literals(text: &str, limit: usize) -> Vec<String> {
    let mut protocols = Vec::new();
    let mut seen = HashSet::new();
    let lowered = text.to_lowercase();

    if lowered.contains("graphql") {
        push_unique_limited(&mut protocols, &mut seen, "GraphQL".to_string(), limit);
    }
    if lowered.contains("soap") {
        push_unique_limited(&mut protocols, &mut seen, "SOAP".to_string(), limit);
    }
    if lowered.contains("rest")
        || lowered.contains("restful api")
        || lowered.contains("rest interface")
    {
        push_unique_limited(&mut protocols, &mut seen, "REST".to_string(), limit);
    }

    protocols
}

pub(super) fn extract_http_methods(text: &str, limit: usize) -> Vec<String> {
    let mut methods = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        if matches!(cleaned, "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
            push_unique_limited(&mut methods, &mut seen, cleaned.to_string(), limit);
        }
    }

    methods
}

fn looks_like_parameter_identifier(token: &str) -> bool {
    if token.len() < 3 || token.len() > 160 || !token.is_ascii() {
        return false;
    }
    let Some(first) = token.chars().next() else {
        return false;
    };
    if !first.is_ascii_alphabetic() {
        return false;
    }
    if !token.chars().any(|ch| ch.is_ascii_alphabetic()) {
        return false;
    }
    if !token.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/')) {
        return false;
    }
    if token.chars().all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '-' | '/')) {
        return false;
    }

    token.contains('_')
        || token.contains('-')
        || token.contains('.')
        || token.contains('/')
        || has_internal_ascii_case_boundary(token)
}

fn has_internal_ascii_case_boundary(token: &str) -> bool {
    let mut seen_lowercase = false;
    for ch in token.chars() {
        if ch.is_ascii_lowercase() {
            seen_lowercase = true;
        } else if ch.is_ascii_uppercase() && seen_lowercase {
            return true;
        }
    }
    false
}

fn looks_like_parameter_assignment_name(token: &str) -> bool {
    if token.is_empty() || token.len() > 160 || !token.is_ascii() {
        return false;
    }
    let Some(first) = token.chars().next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && token.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
        && (token.chars().any(|ch| ch.is_ascii_lowercase())
            || token.chars().any(|ch| matches!(ch, '_' | '-' | '.'))
            || token.chars().any(|ch| ch.is_ascii_digit()))
}

fn clean_parameter_candidate(candidate: &str) -> &str {
    trim_literal_token(candidate).trim_start_matches('?').trim_matches(|ch: char| {
        matches!(ch, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}')
    })
}

pub(super) fn extract_parameter_literals(text: &str, limit: usize) -> Vec<String> {
    let mut parameters = Vec::new();
    let mut seen = HashSet::new();

    for capture in CONFIG_ASSIGNMENT_LITERAL_REGEX.captures_iter(text) {
        let Some(name) = capture.get(1).map(|value| clean_parameter_candidate(value.as_str()))
        else {
            continue;
        };
        if looks_like_parameter_assignment_name(name) {
            push_unique_limited(&mut parameters, &mut seen, name.to_string(), limit);
        }
    }

    for token in text.split_whitespace() {
        let has_literal_marker =
            token.contains('`') || token.starts_with('?') || token.contains('=');
        if looks_like_config_section_literal(clean_config_section_candidate(token)) {
            continue;
        }
        let literal_candidate = trim_literal_token(token);
        let cleaned = literal_candidate.trim_end_matches(|ch: char| {
            matches!(ch, '.' | ':' | ';' | '?' | '!' | ',' | ')' | ']' | '}')
        });
        if let Some((name, value)) = cleaned.split_once('=') {
            let name = clean_parameter_candidate(name);
            if looks_like_parameter_assignment_name(name) {
                push_unique_limited(&mut parameters, &mut seen, name.to_string(), limit);
            }
            let value = clean_parameter_candidate(value);
            if looks_like_parameter_identifier(value) {
                push_unique_limited(&mut parameters, &mut seen, value.to_string(), limit);
            }
            continue;
        }
        let cleaned = clean_parameter_candidate(cleaned);
        if looks_like_parameter_identifier(cleaned)
            || (has_literal_marker && looks_like_parameter_assignment_name(cleaned))
        {
            push_unique_limited(&mut parameters, &mut seen, cleaned.to_string(), limit);
        }
    }

    parameters
}

pub(super) fn extract_config_assignment_literals(text: &str, limit: usize) -> Vec<String> {
    let mut assignments = Vec::new();
    let mut seen = HashSet::new();

    for capture in CONFIG_ASSIGNMENT_LITERAL_REGEX.captures_iter(text) {
        let Some(name) = capture.get(1).map(|value| clean_parameter_candidate(value.as_str()))
        else {
            continue;
        };
        let Some(value) = capture.get(2).map(|value| value.as_str().trim()) else {
            continue;
        };
        if !looks_like_parameter_assignment_name(name) || value.is_empty() {
            continue;
        }
        push_unique_limited(&mut assignments, &mut seen, format!("{name} = {value}"), limit);
    }

    assignments
}

fn clean_config_section_candidate(candidate: &str) -> &str {
    candidate
        .trim_matches(|ch: char| {
            ch.is_whitespace() || matches!(ch, '`' | '"' | '\'' | ',' | ';' | ':' | '.')
        })
        .trim_matches(|ch: char| matches!(ch, '(' | ')' | '{' | '}'))
}

fn looks_like_config_section_literal(candidate: &str) -> bool {
    let Some(inner) = candidate.strip_prefix('[').and_then(|value| value.strip_suffix(']')) else {
        return false;
    };
    if inner.len() < 2 || inner.len() > 160 || !inner.is_ascii() {
        return false;
    }
    let Some(first) = inner.chars().next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && inner.chars().any(|ch| ch.is_ascii_alphabetic())
        && inner.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
}

pub(super) fn extract_config_section_literals(text: &str, limit: usize) -> Vec<String> {
    let mut sections = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned = clean_config_section_candidate(token);
        if looks_like_config_section_literal(cleaned) {
            push_unique_limited(&mut sections, &mut seen, cleaned.to_string(), limit);
        }
    }

    sections
}
