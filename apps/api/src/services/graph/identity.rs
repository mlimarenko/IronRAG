use std::collections::BTreeMap;

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

use crate::domains::runtime_graph::RuntimeNodeType;

const CANONICAL_RELATION_TYPES: &[&str] = &[
    "accepts_from",
    "accepts_receipts_from",
    "aliases",
    "applies_to",
    "assists",
    "attached_to",
    "authenticates",
    "authorizes",
    "awards",
    "belongs_to",
    "builds_on",
    "calls",
    "can_be",
    "complies_with",
    "composed_of",
    "configures",
    "connects_to",
    "consists_of",
    "consumes",
    "contains",
    "created_by",
    "defined_by",
    "defines",
    "delegates_to",
    "deployed_on",
    "depends_on",
    "deprecated_by",
    "described_in",
    "describes",
    "developed_by",
    "documents",
    "emits",
    "enables",
    "exchanges_data_with",
    "extends",
    "followed_by",
    "has_mode",
    "has_property",
    "hosted_by",
    "implements",
    "imports",
    "includes",
    "inherits_from",
    "instance_of",
    "integrates_with",
    "intended_for",
    "invokes",
    "is_a",
    "located_in",
    "logs",
    "maintained_by",
    "manages",
    "may_include",
    "mentions",
    "mentions_in",
    "migrated_to",
    "monitored_by",
    "monitors",
    "notifies",
    "offered_by",
    "operates_in",
    "overrides",
    "owned_by",
    "part_of",
    "preceded_by",
    "produces",
    "provides",
    "proxies",
    "replaces",
    "requires",
    "returns",
    "routes_to",
    "runs_on",
    "scales_to",
    "serializes",
    "services",
    "stores",
    "supports",
    "tested_by",
    "transforms",
    "updates",
    "used_by",
    "used_for",
    "uses",
    "validates",
    "version_of",
    "wraps",
];

/// Slugs from [`normalize_graph_identity_component`] for relation text that should not be stored
/// or shown: placeholders, generic “related” wording, etc. Used at extraction and in graph
/// quality filtering (including non-canonical raw strings that never map into the catalog).
pub const NOISE_RELATION_TYPES: &[&str] = &[
    "connected",
    "connected_to",
    "connection",
    "linked",
    "linked_to",
    "n_a",
    "na",
    "none",
    "null",
    "related",
    "related_to",
    "relation",
    "relationship",
    "tbd",
    "unknown",
    "unspecified",
];

#[must_use]
pub fn is_noise_relation_type(normalized_slug: &str) -> bool {
    NOISE_RELATION_TYPES.contains(&normalized_slug)
}

#[derive(Debug, Clone, Default)]
pub struct GraphLabelNodeTypeIndex {
    node_types_by_identity: BTreeMap<String, RuntimeNodeType>,
}

impl GraphLabelNodeTypeIndex {
    #[must_use]
    pub const fn new() -> Self {
        Self { node_types_by_identity: BTreeMap::new() }
    }

    pub fn insert(&mut self, label: &str, node_type: RuntimeNodeType) {
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

    pub fn insert_aliases(&mut self, label: &str, aliases: &[String], node_type: RuntimeNodeType) {
        self.insert(label, node_type.clone());
        for alias in aliases {
            self.insert(alias, node_type.clone());
        }
    }

    #[must_use]
    pub fn canonical_node_type_for_label(&self, label: &str) -> RuntimeNodeType {
        let identity = normalize_graph_identity_component(label);
        self.node_types_by_identity.get(&identity).cloned().unwrap_or(RuntimeNodeType::Entity)
    }

    #[must_use]
    pub fn canonical_node_key_for_label(&self, label: &str) -> String {
        canonical_node_key(self.canonical_node_type_for_label(label), label)
    }
}

#[must_use]
pub fn canonical_node_key(node_type: RuntimeNodeType, label: &str) -> String {
    format!("{}:{}", node_type_slug(node_type), normalize_graph_identity_component(label))
}

#[must_use]
pub fn canonical_edge_key(from_node_key: &str, relation_type: &str, to_node_key: &str) -> String {
    format!("{from_node_key}--{}--{to_node_key}", normalize_relation_type(relation_type))
}

#[must_use]
pub fn normalize_relation_type(relation_type: &str) -> String {
    let normalized = normalize_graph_identity_component(relation_type);
    is_canonical_relation_type(&normalized).then_some(normalized).unwrap_or_default()
}

#[must_use]
pub const fn canonical_relation_type_catalog() -> &'static [&'static str] {
    CANONICAL_RELATION_TYPES
}

#[must_use]
pub fn is_canonical_relation_type(relation_type: &str) -> bool {
    CANONICAL_RELATION_TYPES.contains(&relation_type)
}

#[must_use]
pub fn runtime_node_type_slug(node_type: &RuntimeNodeType) -> &'static str {
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
pub fn runtime_node_type_from_key(canonical_node_key: &str) -> RuntimeNodeType {
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
pub fn normalize_graph_identity_component(value: &str) -> String {
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
        return normalize_compound_identity_segments(&normalized);
    }

    let fallback_seed = trimmed.nfkc().flat_map(char::to_lowercase).collect::<String>();
    let digest = Sha256::digest(fallback_seed.as_bytes());
    format!("u{}", hex::encode(&digest[..8]))
}

#[must_use]
fn normalize_compound_identity_segments(normalized: &str) -> String {
    normalized
        .split('_')
        .filter(|segment| !segment.is_empty())
        .flat_map(expand_compound_identity_segment)
        .collect::<Vec<_>>()
        .join("_")
}

fn expand_compound_identity_segment(segment: &str) -> Vec<String> {
    match segment {
        "consultantapp" => vec!["consultant".to_string(), "app".to_string()],
        "digitalsignage" => vec!["digital".to_string(), "signage".to_string()],
        "hybridpos" => vec!["hybrid".to_string(), "pos".to_string()],
        "loyaltymanagement" => vec!["loyalty".to_string(), "management".to_string()],
        "pricechecker" => vec!["price".to_string(), "checker".to_string()],
        "tgbot" => vec!["telegram".to_string(), "bot".to_string()],
        "virtualpos" => vec!["virtual".to_string(), "pos".to_string()],
        _ => vec![segment.to_string()],
    }
}

#[must_use]
fn node_type_slug(node_type: RuntimeNodeType) -> &'static str {
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
    fn is_noise_relation_type_matches_uninformative_slugs() {
        assert!(is_noise_relation_type("unknown"));
        assert!(is_noise_relation_type("related_to"));
        assert!(!is_noise_relation_type("uses"));
    }

    #[test]
    fn normalize_relation_type_rejects_unknown_predicates_after_unicode_normalization() {
        assert!(normalize_relation_type("Συνδέεται με").is_empty());
    }

    #[test]
    fn normalize_graph_identity_component_folds_compatibility_forms() {
        assert_eq!(normalize_graph_identity_component("Cafe\u{301}"), "café");
        assert_eq!(normalize_graph_identity_component("ＡI"), "ai");
    }

    #[test]
    fn normalize_graph_identity_component_splits_known_compound_segments() {
        assert_eq!(normalize_graph_identity_component("Acme Hybridpos"), "acme_hybrid_pos");
        assert_eq!(normalize_graph_identity_component("Acme Tgbot"), "acme_telegram_bot");
        assert_eq!(normalize_graph_identity_component("Acme VirtualPos"), "acme_virtual_pos");
        assert_eq!(normalize_graph_identity_component("Acme ConsultantApp"), "acme_consultant_app");
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
    fn normalize_relation_type_accepts_only_canonical_catalog_members_after_text_normalization() {
        assert_eq!(normalize_relation_type("Mentions In"), "mentions_in");
        assert_eq!(normalize_relation_type("used by"), "used_by");
        assert_eq!(normalize_relation_type("is a"), "is_a");
        assert_eq!(normalize_relation_type("part_of"), "part_of");
    }

    #[test]
    /// "contains" is now a canonical relation type, so it must be accepted.
    fn normalize_relation_type_rejects_localized_and_paraphrased_predicates() {
        assert!(normalize_relation_type("χρησιμοποιεί").is_empty());
        assert!(normalize_relation_type("διαχειρίζεται").is_empty());
        assert!(normalize_relation_type("ανταλλάσσει δεδομένα με").is_empty());
        assert!(normalize_relation_type("desarrollado por").is_empty());
        assert!(normalize_relation_type("destinado a").is_empty());
        assert!(normalize_relation_type("περιέχει").is_empty());
        assert_eq!(normalize_relation_type("contains"), "contains");
        assert!(normalize_relation_type("contains section").is_empty());
        assert!(normalize_relation_type("based on").is_empty());
        assert!(normalize_relation_type("documented on").is_empty());
    }

    #[test]
    fn canonical_relation_type_catalog_is_controlled_and_ascii_only() {
        assert!(canonical_relation_type_catalog().contains(&"integrates_with"));
        assert!(canonical_relation_type_catalog().contains(&"described_in"));
        assert!(canonical_relation_type_catalog().iter().all(|predicate| {
            predicate.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
        }));
    }

    #[test]
    fn runtime_node_type_from_key_uses_canonical_prefix() {
        assert_eq!(runtime_node_type_from_key("concept:supply"), RuntimeNodeType::Concept);
        assert_eq!(runtime_node_type_from_key("entity:register"), RuntimeNodeType::Entity);
        assert_eq!(runtime_node_type_from_key("unknown:foo"), RuntimeNodeType::Entity);
    }
}
