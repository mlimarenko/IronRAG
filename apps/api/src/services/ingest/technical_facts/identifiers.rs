use std::collections::BTreeSet;

use super::{
    FactCandidate, StructuredBlockData, StructuredBlockKind, TechnicalFactKind, build_candidate,
    matches_any_substring, technical_tokens, trim_technical_token,
};

pub(super) fn extract_catalog_link_identifier_candidates(
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

pub(super) fn extract_branded_identifier_candidates(
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

pub(super) fn extract_environment_variable_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let mut env_vars = BTreeSet::<String>::new();

    let tokens = technical_tokens(line);
    let lower = line.to_ascii_lowercase();
    let has_env_context = matches_any_substring(
        &lower,
        &["environment", "env", "variable", "export", "getenv", "environ"],
    );

    for token in &tokens {
        if token.starts_with('$') {
            let name = token.trim_start_matches('$').trim_start_matches('{').trim_end_matches('}');
            if is_env_var_name(name) {
                env_vars.insert(name.to_string());
            }
        }

        if let Some(rest) = token.strip_prefix("process.env.") {
            let name = trim_technical_token(rest);
            if is_env_var_name(name) {
                env_vars.insert(name.to_string());
            }
        }
    }

    for pattern in &["os.getenv(", "os.environ["] {
        if let Some(pos) = lower.find(pattern) {
            let after = &line[pos + pattern.len()..];
            if let Some(name) = extract_quoted_argument(after)
                && is_env_var_name(&name)
            {
                env_vars.insert(name);
            }
        }
    }

    for pattern in &["env::var(", "std::env::var("] {
        if let Some(pos) = line.find(pattern) {
            let after = &line[pos + pattern.len()..];
            if let Some(name) = extract_quoted_argument(after)
                && is_env_var_name(&name)
            {
                env_vars.insert(name);
            }
        }
    }

    if let Some(pos) = line.find("ENV[") {
        let after = &line[pos + 4..];
        if let Some(name) = extract_quoted_argument(after)
            && is_env_var_name(&name)
        {
            env_vars.insert(name);
        }
    }

    if has_env_context {
        for token in &tokens {
            let candidate = trim_technical_token(token);
            if is_env_var_name(candidate) {
                env_vars.insert(candidate.to_string());
            }
        }
    }

    env_vars
        .into_iter()
        .filter_map(|var| {
            build_candidate(
                block,
                TechnicalFactKind::EnvironmentVariable,
                &var,
                Vec::new(),
                line,
                "environment_variable",
            )
        })
        .collect()
}

fn is_env_var_name(candidate: &str) -> bool {
    if candidate.len() < 3 {
        return false;
    }
    let mut chars = candidate.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_uppercase()
        && chars.all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
}

fn extract_quoted_argument(after: &str) -> Option<String> {
    let trimmed = after.trim_start();
    let quote = trimmed.chars().next()?;
    if !matches!(quote, '"' | '\'') {
        return None;
    }
    let rest = &trimmed[1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

pub(super) fn extract_version_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let lower = line.to_ascii_lowercase();
    let has_version_context = matches_any_substring(&lower, &["version", "release", " v.", " v "]);

    let mut versions = BTreeSet::<String>::new();
    let tokens = technical_tokens(line);

    for token in &tokens {
        let candidate = trim_technical_token(token);

        if let Some(rest) = candidate.strip_prefix('v').or_else(|| candidate.strip_prefix('V'))
            && is_semver_like(rest)
        {
            versions.insert(candidate.to_string());
        }

        if has_version_context && is_semver_like(candidate) {
            versions.insert(candidate.to_string());
        }

        if has_version_context && is_date_version(candidate) {
            versions.insert(candidate.to_string());
        }
    }

    versions
        .into_iter()
        .filter_map(|version| {
            build_candidate(
                block,
                TechnicalFactKind::VersionNumber,
                &version,
                Vec::new(),
                line,
                "version_number",
            )
        })
        .collect()
}

fn is_semver_like(candidate: &str) -> bool {
    let parts: Vec<&str> = candidate.splitn(2, |ch: char| ch == '-' || ch == '+').collect();
    let core = parts[0];
    let segments: Vec<&str> = core.split('.').collect();
    if segments.len() < 2 || segments.len() > 3 {
        return false;
    }
    segments.iter().all(|seg| !seg.is_empty() && seg.chars().all(|ch| ch.is_ascii_digit()))
}

fn is_date_version(candidate: &str) -> bool {
    let segments: Vec<&str> = candidate.split('.').collect();
    if segments.len() < 2 || segments.len() > 3 {
        return false;
    }
    let Some(year) = segments[0].parse::<u32>().ok() else {
        return false;
    };
    if !(2000..=2099).contains(&year) {
        return false;
    }
    segments[1..]
        .iter()
        .all(|seg| !seg.is_empty() && seg.len() <= 2 && seg.chars().all(|ch| ch.is_ascii_digit()))
}

pub(super) fn extract_code_identifier_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    if !matches!(block.block_kind, StructuredBlockKind::CodeBlock) {
        return Vec::new();
    }

    let mut identifiers = BTreeSet::<String>::new();

    for keyword in &["fn ", "struct ", "enum ", "impl ", "trait ", "mod "] {
        if let Some(name) = extract_keyword_identifier(line, keyword) {
            identifiers.insert(name);
        }
    }

    for keyword in &["def ", "class "] {
        if let Some(name) = extract_keyword_identifier(line, keyword) {
            identifiers.insert(name);
        }
    }

    if let Some(pos) = line.find("async def ") {
        let after = &line[pos + "async def ".len()..];
        if let Some(name) = extract_word_identifier(after) {
            identifiers.insert(name);
        }
    }

    if let Some(name) = extract_keyword_identifier(line, "function ") {
        identifiers.insert(name);
    }

    if let Some(pos) = line.find("const ") {
        let after = &line[pos + "const ".len()..];
        if let Some(name) = extract_word_identifier(after) {
            let rest = line[pos + "const ".len() + name.len()..].trim_start();
            if rest.starts_with('=') {
                identifiers.insert(name);
            }
        }
    }

    if let Some(pos) = line.find("export ") {
        let after = &line[pos + "export ".len()..];
        let after = after.strip_prefix("default ").unwrap_or(after);
        for keyword in &["function ", "class ", "const "] {
            if let Some(rest) = after.strip_prefix(keyword)
                && let Some(name) = extract_word_identifier(rest)
            {
                identifiers.insert(name);
            }
        }
    }

    identifiers
        .into_iter()
        .filter_map(|ident| {
            build_candidate(
                block,
                TechnicalFactKind::CodeIdentifier,
                &ident,
                Vec::new(),
                line,
                "code_identifier",
            )
        })
        .collect()
}

fn extract_keyword_identifier(line: &str, keyword: &str) -> Option<String> {
    let pos = line.find(keyword)?;
    let after = &line[pos + keyword.len()..];
    extract_word_identifier(after)
}

fn extract_word_identifier(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let name: String =
        trimmed.chars().take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_').collect();
    if name.is_empty() || name.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        return None;
    }
    Some(name)
}

pub(super) fn extract_config_key_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let mut keys = BTreeSet::<String>::new();
    let trimmed = line.trim();

    if trimmed.starts_with('[') && trimmed.ends_with(']') && !trimmed.starts_with("[[") {
        let inner = &trimmed[1..trimmed.len() - 1];
        if is_config_key_name(inner) {
            keys.insert(inner.to_string());
        }
    }

    if let Some((left, _right)) = trimmed.split_once(':') {
        let key = left.trim();
        if is_config_key_name(key) && !key.contains(' ') {
            keys.insert(key.to_string());
        }
    }

    if let Some((left, _right)) = trimmed.split_once('=') {
        let key = left.trim();
        if is_config_key_name(key) && !key.contains(' ') {
            keys.insert(key.to_string());
        }
    }

    if let Some(pos) = trimmed.find("\":") {
        let before = &trimmed[..pos];
        if let Some(quote_start) = before.rfind('"') {
            let key = &before[quote_start + 1..];
            if is_config_key_name(key) {
                keys.insert(key.to_string());
            }
        }
    }

    if keys.is_empty() {
        return Vec::new();
    }

    let is_config_block = matches!(
        block.block_kind,
        StructuredBlockKind::CodeBlock | StructuredBlockKind::MetadataBlock
    ) || has_config_context(block);

    if !is_config_block {
        return Vec::new();
    }

    keys.into_iter()
        .filter_map(|key| {
            build_candidate(
                block,
                TechnicalFactKind::ConfigurationKey,
                &key,
                Vec::new(),
                line,
                "config_key",
            )
        })
        .collect()
}

fn is_config_key_name(candidate: &str) -> bool {
    if candidate.is_empty() || candidate.len() > 64 {
        return false;
    }
    candidate.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && candidate.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn has_config_context(block: &StructuredBlockData) -> bool {
    let lower_heading = block.heading_trail.join(" ").to_ascii_lowercase();
    matches_any_substring(&lower_heading, &["config", "setting", "параметр", "настройк", "конфиг"])
        || block.code_language.as_deref().is_some_and(|lang| {
            matches!(lang, "yaml" | "yml" | "toml" | "json" | "ini" | "properties")
        })
}

pub(super) fn extract_error_code_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let lower = line.to_ascii_lowercase();
    if !matches_any_substring(&lower, &["error", "code", "exception", "ошибк"]) {
        return Vec::new();
    }

    let mut codes = BTreeSet::<String>::new();
    for token in technical_tokens(line) {
        let candidate = trim_technical_token(&token);

        if candidate.starts_with('E')
            && candidate.len() >= 4
            && candidate.len() <= 6
            && candidate[1..].chars().all(|ch| ch.is_ascii_digit())
        {
            codes.insert(candidate.to_string());
            continue;
        }

        if (candidate.starts_with("ERR_") || candidate.starts_with("ERROR_"))
            && candidate.len() > 4
            && candidate
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
        {
            codes.insert(candidate.to_string());
        }
    }

    codes
        .into_iter()
        .filter_map(|code| {
            build_candidate(
                block,
                TechnicalFactKind::ErrorCode,
                &code,
                Vec::new(),
                line,
                "error_code",
            )
        })
        .collect()
}

fn infer_catalog_brand_prefix(block: &StructuredBlockData) -> Option<String> {
    block.heading_trail.iter().rev().find_map(|heading| {
        let normalized = normalize_catalog_link_label(heading)?;
        // Prefer the first ASCII-only token (typical for brand names like
        // "Acme") over the leading word in non-Latin headings such as
        // "Программные продукты Acme".
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

pub(super) fn is_ascii_titlecase_word(word: &str) -> bool {
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

pub(super) fn has_ascii_camel_case(word: &str) -> bool {
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
