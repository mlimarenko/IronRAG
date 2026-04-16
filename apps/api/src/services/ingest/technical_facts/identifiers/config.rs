use super::super::{
    FactCandidate, StructuredBlockData, StructuredBlockKind, TechnicalFactKind, build_candidate,
};

/// Extracts config keys via structural parsing (JSON, YAML, TOML).
pub(crate) fn extract_config_key_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    if !matches!(
        block.block_kind,
        StructuredBlockKind::CodeBlock | StructuredBlockKind::MetadataBlock
    ) {
        return Vec::new();
    }

    match block.code_language.as_deref() {
        Some("json" | "jsonc") => {
            extract_parsed_keys(block, line, parse_json_keys(&block.text), "parsed_json_key")
        }
        Some("yaml" | "yml") => {
            extract_parsed_keys(block, line, parse_yaml_keys(&block.text), "parsed_yaml_key")
        }
        Some("toml") => {
            extract_parsed_keys(block, line, parse_toml_keys(&block.text), "parsed_toml_key")
        }
        _ => Vec::new(),
    }
}

fn extract_parsed_keys(
    block: &StructuredBlockData,
    line: &str,
    parsed_keys: Option<Vec<String>>,
    suffix: &str,
) -> Vec<FactCandidate> {
    let Some(keys) = parsed_keys else { return Vec::new() };
    let trimmed = line.trim();
    keys.into_iter()
        .filter(|k| trimmed.contains(k.as_str()) && is_config_key_name(k))
        .filter_map(|key| {
            build_candidate(
                block,
                TechnicalFactKind::ConfigurationKey,
                &key,
                Vec::new(),
                line,
                suffix,
            )
        })
        .collect()
}

fn parse_json_keys(text: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let mut keys = Vec::new();
    walk_json(&value, &mut keys);
    Some(keys)
}

fn walk_json(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                out.push(k.clone());
                walk_json(v, out);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                walk_json(v, out);
            }
        }
        _ => {}
    }
}

fn parse_yaml_keys(text: &str) -> Option<Vec<String>> {
    let value: serde_yaml::Value = serde_yaml::from_str(text).ok()?;
    let mut keys = Vec::new();
    walk_yaml(&value, &mut keys);
    Some(keys)
}

fn walk_yaml(value: &serde_yaml::Value, out: &mut Vec<String>) {
    match value {
        serde_yaml::Value::Mapping(map) => {
            for (k, v) in map {
                if let Some(s) = k.as_str() {
                    out.push(s.to_string());
                }
                walk_yaml(v, out);
            }
        }
        serde_yaml::Value::Sequence(arr) => {
            for v in arr {
                walk_yaml(v, out);
            }
        }
        _ => {}
    }
}

fn parse_toml_keys(text: &str) -> Option<Vec<String>> {
    let doc: toml_edit::DocumentMut = text.parse().ok()?;
    let mut keys = Vec::new();
    walk_toml(doc.as_item(), &mut keys);
    Some(keys)
}

fn walk_toml(item: &toml_edit::Item, out: &mut Vec<String>) {
    if let Some(table) = item.as_table() {
        for (k, v) in table.iter() {
            out.push(k.to_string());
            walk_toml(v, out);
        }
    } else if let Some(arr) = item.as_array_of_tables() {
        for table in arr.iter() {
            for (k, v) in table.iter() {
                out.push(k.to_string());
                walk_toml(v, out);
            }
        }
    }
}

fn is_config_key_name(candidate: &str) -> bool {
    !candidate.is_empty()
        && candidate.len() <= 64
        && candidate.chars().all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
        && candidate.chars().any(|ch| ch.is_ascii_alphabetic())
}
