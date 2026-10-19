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
    let Some(keys) = parsed_keys else {
        return Vec::new();
    };
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
    let options = serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::AsOrdinary,
        non_finite_float_policy: serde_saphyr::NonFiniteFloatPolicy::PassThrough,
    };
    let value: serde_value::Value = serde_saphyr::from_str_with_options(text, options).ok()?;
    let mut keys = Vec::new();
    walk_yaml(&value, &mut keys);
    Some(keys)
}

fn walk_yaml(value: &serde_value::Value, out: &mut Vec<String>) {
    match value {
        serde_value::Value::Map(map) => {
            for (k, v) in map {
                if let serde_value::Value::String(s) = k {
                    out.push(s.to_string());
                }
                walk_yaml(v, out);
            }
        }
        serde_value::Value::Seq(arr) => {
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
        for (k, v) in table {
            out.push(k.to_string());
            walk_toml(v, out);
        }
    } else if let Some(arr) = item.as_array_of_tables() {
        for table in arr {
            for (k, v) in table {
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

#[cfg(test)]
mod tests {
    use super::parse_yaml_keys;

    #[test]
    fn yaml_keys_preserve_scalar_types_and_nested_non_string_key_values() {
        let keys =
            parse_yaml_keys("on: literal\ntrue: {nested: value}\n? [one, two]\n: {deep: value}\n")
                .expect("YAML keys");
        assert!(keys.iter().any(|key| key == "on"));
        assert!(keys.iter().any(|key| key == "nested"));
        assert!(keys.iter().any(|key| key == "deep"));
        assert!(!keys.iter().any(|key| key == "true"));
    }

    #[test]
    fn refuses_excessive_depth_and_recursive_aliases() {
        let nested = format!("value: {}0{}", "[".repeat(200), "]".repeat(200));
        assert!(parse_yaml_keys(&nested).is_none());
        assert!(parse_yaml_keys("value: &recursive [*recursive]\n").is_none());
    }
}
