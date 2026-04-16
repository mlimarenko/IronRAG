use std::collections::BTreeSet;

use super::super::{
    FactCandidate, StructuredBlockData, StructuredBlockKind, TechnicalFactKind, build_candidate,
    matches_any_substring, trim_technical_token,
};

pub(crate) fn extract_catalog_link_identifier_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    if !matches!(block.block_kind, StructuredBlockKind::ListItem) {
        return Vec::new();
    }

    let Some(brand_prefix) = infer_catalog_brand_prefix(block) else {
        return Vec::new();
    };

    extract_markdown_link_labels(line)
        .into_iter()
        .filter_map(|label| normalize_catalog_link_label(&label))
        .filter_map(|label| {
            let display = if label
                .split_whitespace()
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case(&brand_prefix))
            {
                label
            } else {
                format!("{brand_prefix} {label}")
            };
            build_candidate(
                block,
                TechnicalFactKind::Identifier,
                &display,
                Vec::new(),
                line,
                "catalog_link_identifier",
            )
        })
        .collect()
}

pub(crate) fn extract_branded_identifier_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    if !matches!(
        block.block_kind,
        StructuredBlockKind::Heading | StructuredBlockKind::MetadataBlock
    ) {
        return Vec::new();
    }

    let mut identifiers = BTreeSet::<String>::new();
    if let Some(identifier) = extract_namespace_style_identifier(line) {
        identifiers.insert(identifier);
    }
    if let Some(identifier) = extract_branded_phrase_identifier(line) {
        identifiers.insert(identifier);
    }

    identifiers
        .into_iter()
        .filter_map(|identifier| {
            build_candidate(
                block,
                TechnicalFactKind::Identifier,
                &identifier,
                Vec::new(),
                line,
                "branded_identifier",
            )
        })
        .collect()
}

fn infer_catalog_brand_prefix(block: &StructuredBlockData) -> Option<String> {
    block.heading_trail.iter().rev().find_map(|heading| {
        let normalized = normalize_catalog_link_label(heading)?;
        let ascii_token = normalized
            .split_whitespace()
            .find(|word| !word.is_empty() && word.chars().all(|c| c.is_ascii_alphanumeric()))
            .map(str::to_string);
        ascii_token.or_else(|| normalized.split_whitespace().next().map(str::to_string))
    })
}

fn extract_markdown_link_labels(line: &str) -> Vec<String> {
    let mut labels = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find('[') {
        let after_start = &rest[start + 1..];
        let Some(end) = after_start.find(']') else {
            break;
        };
        let label = after_start[..end].trim();
        let after_label = &after_start[end + 1..];
        if after_label.starts_with('(') && !label.is_empty() {
            labels.push(label.to_string());
        }
        rest = after_label;
    }
    labels
}

fn normalize_catalog_link_label(label: &str) -> Option<String> {
    let parts = label
        .split_whitespace()
        .map(trim_technical_token)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }

    let normalized = parts.join(" ");
    if normalized.len() < 3 || is_generic_ascii_heading(&normalized) {
        return None;
    }

    Some(normalized)
}

fn extract_namespace_style_identifier(line: &str) -> Option<String> {
    let (left, right) = line.split_once(':')?;
    let left = trim_technical_token(left);
    let right = trim_technical_token(right);
    if left.is_empty() || right.is_empty() || left.contains(' ') || right.contains(' ') {
        return None;
    }
    Some(format!("{left}:{right}"))
}

fn extract_branded_phrase_identifier(line: &str) -> Option<String> {
    let primary = split_primary_phrase(line);
    looks_like_branded_product_phrase(primary).then(|| primary.to_string())
}

fn split_primary_phrase(value: &str) -> &str {
    value.split(['(', '[', ',', ';', ':']).next().unwrap_or(value).trim()
}

fn looks_like_branded_product_phrase(candidate: &str) -> bool {
    let words = candidate
        .split_whitespace()
        .map(trim_technical_token)
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.len() < 2 || words.len() > 4 {
        return false;
    }
    if is_generic_ascii_heading(candidate) {
        return false;
    }
    words.iter().all(|word| branded_identifier_part(word))
        && words.iter().any(|word| looks_like_brand_context_word(word))
}

fn branded_identifier_part(candidate: &str) -> bool {
    is_ascii_titlecase_word(candidate)
        || is_ascii_uppercase_acronym(candidate)
        || has_ascii_camel_case(candidate)
}

pub(crate) fn is_ascii_titlecase_word(word: &str) -> bool {
    let compact = trim_technical_token(word);
    let mut chars = compact.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase() && chars.all(|ch| ch.is_ascii_lowercase())
}

fn is_ascii_uppercase_acronym(word: &str) -> bool {
    let compact = trim_technical_token(word);
    compact.len() >= 2 && compact.chars().all(|ch| ch.is_ascii_uppercase())
}

pub(crate) fn has_ascii_camel_case(word: &str) -> bool {
    let compact = trim_technical_token(word);
    compact.chars().any(|ch| ch.is_ascii_uppercase())
        && compact.chars().any(|ch| ch.is_ascii_lowercase())
}

fn looks_like_brand_context_word(word: &str) -> bool {
    matches_any_substring(
        &trim_technical_token(word).to_ascii_lowercase(),
        &["api", "sdk", "cloud", "auth", "gateway", "platform", "service"],
    )
}

fn is_generic_ascii_heading(candidate: &str) -> bool {
    let lower = candidate.to_ascii_lowercase();
    matches_any_substring(
        &lower,
        &[
            "overview",
            "introduction",
            "getting started",
            "configuration",
            "parameters",
            "authentication",
            "errors",
            "response",
            "request",
        ],
    )
}
