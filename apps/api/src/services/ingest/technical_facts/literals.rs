use super::*;

pub(super) fn extract_url_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    technical_tokens(line)
        .into_iter()
        .filter_map(|token| {
            extract_url_like_token(&token).and_then(|url| {
                build_candidate(
                    block,
                    TechnicalFactKind::Url,
                    &url,
                    Vec::new(),
                    line,
                    "literal_url",
                )
            })
        })
        .collect()
}

pub(super) fn extract_endpoint_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let descriptors = line_descriptors(line);
    let methods = descriptors
        .iter()
        .filter_map(|descriptor| normalize_http_method(&descriptor.value))
        .collect::<Vec<_>>();
    let mut facts = methods
        .iter()
        .filter_map(|method| {
            build_candidate(
                block,
                TechnicalFactKind::HttpMethod,
                method,
                Vec::new(),
                line,
                "endpoint_method",
            )
        })
        .collect::<Vec<_>>();

    let method_qualifiers = if methods.len() == 1 {
        vec![TechnicalFactQualifier { key: "method".to_string(), value: methods[0].to_string() }]
    } else {
        Vec::new()
    };

    let path_descriptors = descriptors
        .iter()
        .filter_map(|descriptor| {
            extract_path_like_token(&descriptor.value)
                .or_else(|| {
                    extract_url_like_token(&descriptor.value).and_then(|url| extract_url_path(&url))
                })
                .map(|path| (descriptor.value.as_str(), path))
        })
        .collect::<Vec<_>>();

    for (_, path) in path_descriptors {
        if let Some(candidate) = build_candidate(
            block,
            TechnicalFactKind::EndpointPath,
            &path,
            method_qualifiers.clone(),
            line,
            "endpoint_path",
        ) {
            facts.push(candidate);
        }
    }

    facts
}

pub(super) fn extract_port_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let mut ports = BTreeSet::<String>::new();

    for token in technical_tokens(line) {
        if let Some(url) = extract_url_like_token(&token)
            && let Some(port_literal) = extract_port_from_url(&url)
        {
            ports.insert(port_literal);
        }
    }

    ports
        .into_iter()
        .filter_map(|port| {
            build_candidate(block, TechnicalFactKind::Port, &port, Vec::new(), line, "network_port")
        })
        .collect()
}

pub(super) fn extract_status_code_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    extract_http_response_status_literals(line)
        .into_iter()
        .filter_map(|status| {
            build_candidate(
                block,
                TechnicalFactKind::StatusCode,
                &status,
                Vec::new(),
                line,
                "http_status",
            )
        })
        .collect()
}

pub(super) fn extract_protocol_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let mut protocols = BTreeSet::<String>::new();
    for token in technical_tokens(line) {
        if let Some(url) = extract_url_like_token(&token)
            && let Some((scheme, _)) = url.split_once("://")
            && PROTOCOLS.iter().any(|protocol| protocol == &scheme)
        {
            protocols.insert(scheme.to_string());
        }
        let normalized = trim_technical_token(&token).to_ascii_lowercase();
        if PROTOCOLS.iter().any(|protocol| protocol == &normalized) {
            protocols.insert(normalized);
        }
    }

    protocols
        .into_iter()
        .filter_map(|protocol| {
            build_candidate(
                block,
                TechnicalFactKind::Protocol,
                &protocol,
                Vec::new(),
                line,
                "protocol",
            )
        })
        .collect()
}

pub(super) fn extract_parameter_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let mut parameters = BTreeSet::<String>::new();

    for token in technical_tokens(line) {
        if let Some(url) = extract_url_like_token(&token) {
            for key in extract_query_parameter_keys(&url) {
                parameters.insert(key);
            }
        }
    }

    let cells = table_cells(line);
    if cells.len() >= 2
        && let Some(parameter_name) = leading_identifier(&cells[0])
        && is_parameter_name_like(&parameter_name)
    {
        parameters.insert(parameter_name);
    }

    let cleaned = strip_leading_marker(&collapse_literal_whitespace(line));
    for separator in [":", "="] {
        if let Some((left, _right)) = cleaned.split_once(separator)
            && let Some(parameter_name) = leading_identifier(left)
            && is_parameter_name_like(&parameter_name)
        {
            parameters.insert(parameter_name);
        }
    }

    parameters
        .into_iter()
        .filter_map(|parameter_name| {
            build_candidate(
                block,
                TechnicalFactKind::ParameterName,
                &parameter_name,
                Vec::new(),
                line,
                "parameter_name",
            )
        })
        .collect()
}

pub(super) fn extract_auth_rule_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let literal = line
        .split_once(':')
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case("authorization"))
        .and_then(|(_, value)| value.split_ascii_whitespace().next())
        .and_then(|scheme| {
            if scheme.eq_ignore_ascii_case("bearer") {
                Some("bearer_token")
            } else if scheme.eq_ignore_ascii_case("basic") {
                Some("basic_auth")
            } else {
                None
            }
        });

    literal
        .and_then(|value| {
            build_candidate(
                block,
                TechnicalFactKind::AuthRule,
                value,
                Vec::new(),
                line,
                "auth_rule",
            )
        })
        .into_iter()
        .collect()
}

pub(super) fn technical_tokens(line: &str) -> Vec<String> {
    line.split_whitespace()
        .map(trim_technical_token)
        .map(str::to_string)
        .filter(|token| !token.is_empty())
        .collect()
}

#[derive(Debug, Clone)]
struct LineDescriptor {
    value: String,
}

fn line_descriptors(line: &str) -> Vec<LineDescriptor> {
    let mut descriptors = technical_tokens(line)
        .into_iter()
        .map(|value| LineDescriptor { value })
        .collect::<Vec<_>>();

    for cell in table_cells(line) {
        if !descriptors.iter().any(|descriptor| descriptor.value == cell) {
            descriptors.push(LineDescriptor { value: cell });
        }
    }

    descriptors
}

fn table_cells(line: &str) -> Vec<String> {
    if !line.contains('|') {
        return Vec::new();
    }
    line.split('|')
        .map(collapse_literal_whitespace)
        .map(|cell| strip_leading_marker(&cell))
        .filter(|cell| !cell.is_empty())
        .collect()
}

/// Validates a token as a well-formed URL using the `url` crate
/// (RFC 3986). Replaces the old `starts_with("http://")` heuristic
/// that couldn't distinguish a real URL from a comment that happens
/// to contain "http://" and missed edge cases like trailing parens,
/// angle brackets, and percent-encoded paths.
pub(super) fn extract_url_like_token(token: &str) -> Option<String> {
    let trimmed = trim_technical_token(token);
    let parsed = url::Url::parse(trimmed).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| trimmed.to_string())
}

/// Extracts the path component from a URL via RFC-compliant parsing.
fn extract_url_path(raw: &str) -> Option<String> {
    let parsed = url::Url::parse(raw).ok()?;
    let path = parsed.path();
    if path == "/" || path.is_empty() {
        return None;
    }
    extract_path_like_token(path)
}

/// Extracts query parameter keys from a URL via the `url` crate's
/// `query_pairs()` iterator. Replaces manual `split('&')` +
/// `split('=')` which silently broke on percent-encoded keys.
fn extract_query_parameter_keys(raw: &str) -> Vec<String> {
    let Ok(parsed) = url::Url::parse(raw) else {
        return Vec::new();
    };
    parsed
        .query_pairs()
        .map(|(key, _)| key.to_string())
        .filter(|key| is_parameter_name_like(key))
        .collect()
}

pub(super) fn extract_path_like_token(token: &str) -> Option<String> {
    let trimmed = trim_technical_token(token);
    if !trimmed.starts_with('/') || trimmed.len() < 2 {
        return None;
    }
    let path = trimmed.split(['?', '#']).next().unwrap_or_default();
    path.chars()
        .all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '/' | '_' | '-' | '.' | '{' | '}')
        })
        .then(|| collapse_literal_whitespace(path).replace(' ', ""))
}

pub(super) fn extract_port_from_url(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?;
    let (_, remainder) = url.split_once("://")?;
    let authority = remainder.split(['/', '?', '#']).next().unwrap_or_default();
    let host_and_port = authority.rsplit('@').next().unwrap_or_default();
    let port = if host_and_port.starts_with('[') {
        host_and_port.split_once("]:")?.1
    } else {
        host_and_port.rsplit_once(':')?.1
    };
    extract_port_literal(port)
}

fn extract_port_literal(value: &str) -> Option<String> {
    if value.is_empty() || !value.chars().all(|character| character.is_ascii_digit()) {
        return None;
    }
    let parsed = value.parse::<u16>().ok()?;
    ((1..=65535).contains(&parsed)).then_some(parsed.to_string())
}

pub(super) fn extract_http_response_status_literals(line: &str) -> BTreeSet<String> {
    technical_tokens(line)
        .windows(2)
        .filter_map(|window| {
            is_http_version_literal(&window[0])
                .then(|| trim_technical_token(&window[1]))
                .filter(|candidate| {
                    candidate.len() == 3
                        && candidate.chars().all(|character| character.is_ascii_digit())
                })
                .and_then(|candidate| {
                    candidate
                        .parse::<u16>()
                        .ok()
                        .filter(|status| (100..=599).contains(status))
                        .map(|_| candidate.to_string())
                })
        })
        .collect()
}

fn is_http_version_literal(value: &str) -> bool {
    let Some(version) = trim_technical_token(value).strip_prefix("HTTP/") else {
        return false;
    };
    let mut segments = version.split('.');
    let Some(major) = segments.next() else {
        return false;
    };
    if major.is_empty() || !major.chars().all(|character| character.is_ascii_digit()) {
        return false;
    }
    match (segments.next(), segments.next()) {
        (None, None) => true,
        (Some(minor), None) => {
            !minor.is_empty() && minor.chars().all(|character| character.is_ascii_digit())
        }
        _ => false,
    }
}

pub(super) fn is_prefixed_semver_literal(value: &str) -> bool {
    value
        .strip_prefix('v')
        .or_else(|| value.strip_prefix('V'))
        .is_some_and(|version| semver::Version::parse(version).is_ok())
}

pub(super) fn normalize_http_method(value: &str) -> Option<&'static str> {
    let upper = trim_technical_token(value).to_ascii_uppercase();
    HTTP_METHODS.into_iter().find(|method| method == &upper)
}

fn leading_identifier(value: &str) -> Option<String> {
    let trimmed = strip_leading_marker(value);
    let candidate = trim_technical_token(trimmed.split_whitespace().next().unwrap_or_default());
    is_parameter_name_like(candidate).then(|| candidate.to_string())
}

fn is_parameter_name_like(candidate: &str) -> bool {
    let compact = trim_technical_token(candidate);
    !compact.is_empty()
        && compact.len() <= 64
        && compact.chars().any(|character| character.is_ascii_alphabetic())
        && compact.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.')
        })
        && (compact.contains('_')
            || compact.contains('-')
            || compact
                .chars()
                .zip(compact.chars().skip(1))
                .any(|(a, b)| a.is_ascii_lowercase() && b.is_ascii_uppercase())
            || (compact.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && compact.chars().skip(1).all(|c| c.is_ascii_lowercase())))
}

fn strip_leading_marker(value: &str) -> String {
    value
        .trim_start_matches(|character: char| {
            matches!(character, '-' | '*' | '+' | '#' | '>' | '"' | '\'')
        })
        .trim()
        .to_string()
}

pub(super) fn trim_technical_token(token: &str) -> &str {
    token.trim_matches(|character: char| {
        matches!(character, ',' | ';' | ':' | ')' | '(' | ']' | '[' | '"' | '\'' | '`' | '{' | '}')
    })
}
