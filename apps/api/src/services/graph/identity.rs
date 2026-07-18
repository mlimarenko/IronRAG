use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::domains::runtime_graph::RuntimeNodeType;

/// Formal relation identifier limits. Meaning is selected by the configured
/// extraction model and is never inferred from a handwritten catalog.
pub(crate) const RELATION_TYPE_MAX_LENGTH: usize = 64;
pub(crate) const RELATION_TYPE_JSON_SCHEMA_PATTERN: &str = r"^[a-z][a-z0-9]*(_[a-z0-9]+)*$";

#[must_use]
pub(crate) fn is_structural_literal_label(label: &str) -> bool {
    matches!(
        serde_json::from_str::<serde_json::Value>(label.trim()),
        Ok(serde_json::Value::Bool(_) | serde_json::Value::Null)
    )
}

#[derive(Debug, Clone, Default)]
pub(crate) struct GraphLabelNodeTypeIndex {
    node_types_by_identity: BTreeMap<String, RuntimeNodeType>,
}

impl GraphLabelNodeTypeIndex {
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self { node_types_by_identity: BTreeMap::new() }
    }

    pub(crate) fn insert(&mut self, label: &str, node_type: RuntimeNodeType) {
        let identity = normalize_graph_identity_component(label);
        if identity.is_empty() {
            return;
        }
        match self.node_types_by_identity.get(&identity) {
            Some(existing) if node_type_priority(existing) >= node_type_priority(&node_type) => {}
            _ => {
                self.node_types_by_identity.insert(identity, node_type);
            }
        }
    }

    pub(crate) fn insert_aliases(
        &mut self,
        label: &str,
        aliases: &[String],
        node_type: RuntimeNodeType,
    ) {
        self.insert(label, node_type);
        for alias in aliases {
            self.insert(alias, node_type);
        }
    }

    #[must_use]
    pub(crate) fn canonical_node_type_for_label(&self, label: &str) -> RuntimeNodeType {
        let identity = normalize_graph_identity_component(label);
        self.node_types_by_identity.get(&identity).copied().unwrap_or(RuntimeNodeType::Entity)
    }

    #[must_use]
    pub(crate) fn canonical_node_key_for_label(&self, label: &str) -> String {
        canonical_node_key(self.canonical_node_type_for_label(label), label)
    }
}

#[must_use]
pub(crate) fn canonical_node_key(node_type: RuntimeNodeType, label: &str) -> String {
    format!("{}:{}", runtime_node_type_slug(&node_type), normalize_graph_identity_component(label))
}

#[must_use]
pub(crate) fn canonical_edge_key(
    from_node_key: &str,
    relation_type: &str,
    to_node_key: &str,
) -> String {
    format!("{from_node_key}--{}--{to_node_key}", normalize_relation_type(relation_type))
}

#[must_use]
pub(crate) fn normalize_relation_type(relation_type: &str) -> String {
    let candidate = relation_type.trim();
    if is_valid_relation_type(candidate) { candidate.to_string() } else { String::new() }
}

#[must_use]
pub(crate) fn is_valid_relation_type(relation_type: &str) -> bool {
    let bytes = relation_type.as_bytes();
    if bytes.is_empty() || bytes.len() > RELATION_TYPE_MAX_LENGTH || !bytes[0].is_ascii_lowercase()
    {
        return false;
    }

    let mut previous_was_underscore = false;
    for &byte in bytes {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() {
            previous_was_underscore = false;
        } else if byte == b'_' && !previous_was_underscore {
            previous_was_underscore = true;
        } else {
            return false;
        }
    }
    !previous_was_underscore
}

#[must_use]
pub(crate) const fn runtime_node_type_slug(node_type: &RuntimeNodeType) -> &'static str {
    match node_type {
        RuntimeNodeType::Document => "document",
        RuntimeNodeType::Person => "person",
        RuntimeNodeType::Organization => "organization",
        RuntimeNodeType::Location => "location",
        RuntimeNodeType::Event => "event",
        RuntimeNodeType::Artifact => "artifact",
        RuntimeNodeType::Natural => "natural",
        RuntimeNodeType::Process => "process",
        RuntimeNodeType::Concept => "concept",
        RuntimeNodeType::Attribute => "attribute",
        RuntimeNodeType::Entity => "entity",
    }
}

#[must_use]
pub(crate) fn runtime_node_type_from_key(canonical_node_key: &str) -> RuntimeNodeType {
    canonical_node_key
        .split_once(':')
        .map(|(node_type, _)| node_type)
        .and_then(|node_type| match node_type {
            "document" => Some(RuntimeNodeType::Document),
            "person" => Some(RuntimeNodeType::Person),
            "organization" => Some(RuntimeNodeType::Organization),
            "location" => Some(RuntimeNodeType::Location),
            "event" => Some(RuntimeNodeType::Event),
            "artifact" => Some(RuntimeNodeType::Artifact),
            "natural" => Some(RuntimeNodeType::Natural),
            "process" => Some(RuntimeNodeType::Process),
            "concept" => Some(RuntimeNodeType::Concept),
            "attribute" => Some(RuntimeNodeType::Attribute),
            "entity" => Some(RuntimeNodeType::Entity),
            _ => None,
        })
        .unwrap_or(RuntimeNodeType::Entity)
}

#[must_use]
pub(crate) fn normalize_graph_identity_component(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let normalized = trimmed
        .nfkc()
        .flat_map(char::to_lowercase)
        .fold(String::new(), |mut output, ch| {
            if ch.is_alphanumeric() {
                output.push(ch);
            } else if !output.is_empty() && !output.ends_with('_') {
                output.push('_');
            }
            output
        })
        .trim_end_matches('_')
        .to_string();

    if !normalized.is_empty() {
        return normalized;
    }

    let fallback_seed = trimmed.nfkc().flat_map(char::to_lowercase).collect::<String>();
    let digest = Sha256::digest(fallback_seed.as_bytes());
    format!("u{}", hex::encode(&digest[..8]))
}

#[must_use]
const fn node_type_priority(node_type: &RuntimeNodeType) -> u8 {
    match node_type {
        RuntimeNodeType::Person
        | RuntimeNodeType::Organization
        | RuntimeNodeType::Location
        | RuntimeNodeType::Event
        | RuntimeNodeType::Artifact
        | RuntimeNodeType::Natural
        | RuntimeNodeType::Process
        | RuntimeNodeType::Attribute
        | RuntimeNodeType::Entity => 2,
        RuntimeNodeType::Concept => 1,
        RuntimeNodeType::Document => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_node_key_preserves_non_ascii_identity() {
        assert_eq!(
            canonical_node_key(RuntimeNodeType::Entity, "Acme Imprenta Düsseldorf"),
            "entity:acme_imprenta_düsseldorf"
        );
    }

    #[test]
    fn canonical_node_key_normalizes_mixed_script_labels() {
        assert_eq!(
            canonical_node_key(RuntimeNodeType::Entity, "GraphRAG περί Retail 2.0"),
            "entity:graphrag_περί_retail_2_0"
        );
    }

    #[test]
    fn relation_type_validation_accepts_arbitrary_ascii_snake_case() {
        assert!(is_valid_relation_type("opaque_predicate_7"));
        assert!(is_valid_relation_type("unknown"));
        assert!(is_valid_relation_type("a"));
        assert_eq!(normalize_relation_type(" opaque_predicate_7 "), "opaque_predicate_7");
    }

    #[test]
    fn structural_literal_label_detection_is_json_bool_or_null_only() {
        assert!(is_structural_literal_label("false"));
        assert!(is_structural_literal_label(" true "));
        assert!(is_structural_literal_label("null"));
        assert!(!is_structural_literal_label("False"));
        assert!(!is_structural_literal_label("42"));
        assert!(!is_structural_literal_label("3.12.4"));
        assert!(!is_structural_literal_label("Alpha false mode"));
    }

    #[test]
    fn relation_type_validation_rejects_invalid_shape_and_encoding() {
        for invalid in [
            "",
            "_predicate",
            "predicate_",
            "predicate__7",
            "Predicate",
            "7_predicate",
            "predicate-name",
            "predicáte",
            "predicate\nnext",
        ] {
            assert!(!is_valid_relation_type(invalid), "{invalid:?}");
            assert!(normalize_relation_type(invalid).is_empty(), "{invalid:?}");
        }
    }

    #[test]
    fn normalize_graph_identity_component_folds_compatibility_forms() {
        assert_eq!(normalize_graph_identity_component("Cafe\u{301}"), "café");
        assert_eq!(normalize_graph_identity_component("ＡI"), "ai");
    }

    #[test]
    fn normalize_graph_identity_component_keeps_segments_verbatim() {
        assert_eq!(normalize_graph_identity_component("Alpha SuitePos"), "alpha_suitepos");
        assert_eq!(normalize_graph_identity_component("Beta AppBot"), "beta_appbot");
    }

    #[test]
    fn punctuation_only_labels_get_stable_fallback_identity() {
        let bang = normalize_graph_identity_component("!!!");
        let question = normalize_graph_identity_component("???");

        assert!(!bang.is_empty());
        assert!(!question.is_empty());
        assert_ne!(bang, question);
    }

    #[test]
    fn label_node_type_index_prefers_entity_for_ambiguous_labels() {
        let mut index = GraphLabelNodeTypeIndex::new();
        index.insert("Register", RuntimeNodeType::Concept);
        index.insert("Register", RuntimeNodeType::Entity);

        assert_eq!(index.canonical_node_key_for_label("Register"), "entity:register");
    }

    #[test]
    fn label_node_type_index_prefers_entity_for_alias_collisions() {
        let mut index = GraphLabelNodeTypeIndex::new();
        index.insert_aliases("Acme POS", &["Register".to_string()], RuntimeNodeType::Concept);
        index.insert("Register", RuntimeNodeType::Entity);

        assert_eq!(index.canonical_node_key_for_label("Register"), "entity:register");
    }

    #[test]
    fn relation_type_validation_enforces_the_length_bound() {
        let at_limit = "a".repeat(RELATION_TYPE_MAX_LENGTH);
        let over_limit = "a".repeat(RELATION_TYPE_MAX_LENGTH + 1);

        assert!(is_valid_relation_type(&at_limit));
        assert!(!is_valid_relation_type(&over_limit));
        assert_eq!(normalize_relation_type(&at_limit), at_limit);
        assert!(normalize_relation_type(&over_limit).is_empty());
    }

    #[test]
    fn runtime_node_type_from_key_uses_canonical_prefix() {
        assert_eq!(runtime_node_type_from_key("concept:supply"), RuntimeNodeType::Concept);
        assert_eq!(runtime_node_type_from_key("entity:register"), RuntimeNodeType::Entity);
        assert_eq!(runtime_node_type_from_key("unknown:foo"), RuntimeNodeType::Entity);
    }
}
