use anyhow::{Context, Result as AnyhowResult, anyhow};

use crate::domains::runtime_graph::RuntimeNodeType;
use crate::services::graph::error::GraphServiceError;
use crate::shared::extraction::text_quality::{
    is_low_confidence_text, is_structurally_unstable_fragment,
};

use super::types::{
    FailedNormalizationAttempt, GraphEntityCandidate, GraphExtractionCandidateSet,
    GraphExtractionTaskFailure, GraphExtractionTaskFailureCode, GraphRelationCandidate,
    NormalizedGraphExtractionAttempt,
};

pub(crate) fn normalize_graph_extraction_output(
    output_text: &str,
) -> std::result::Result<NormalizedGraphExtractionAttempt, FailedNormalizationAttempt> {
    parse_graph_extraction_output(output_text)
        .map(|normalized| NormalizedGraphExtractionAttempt {
            normalized,
            normalization_path: "direct",
        })
        .map_err(|error| FailedNormalizationAttempt { parse_error: error.to_string() })
}

pub fn parse_graph_extraction_output(
    output_text: &str,
) -> std::result::Result<GraphExtractionCandidateSet, GraphServiceError> {
    let parsed = extract_json_payload(output_text).map_err(|error| {
        GraphServiceError::ProviderUnavailable {
            message: format!(
                "{}: {error}",
                GraphExtractionTaskFailureCode::MalformedOutput.as_str()
            ),
        }
    })?;
    let entities = parsed
        .get("entities")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(parse_entity_candidate).collect::<Vec<_>>())
        .unwrap_or_default();
    let relations = parsed
        .get("relations")
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(parse_relation_candidate).collect::<Vec<_>>())
        .unwrap_or_default();

    let candidate_set = GraphExtractionCandidateSet { entities, relations };
    validate_graph_extraction_candidate_set(&candidate_set).map_err(|failure| {
        GraphServiceError::ProviderUnavailable { message: failure.summary.clone() }
    })?;
    Ok(candidate_set)
}

#[must_use]
pub fn sanitize_graph_extraction_candidate_set(
    candidate_set: GraphExtractionCandidateSet,
    source_text: &str,
) -> GraphExtractionCandidateSet {
    if is_low_confidence_text(source_text) {
        return GraphExtractionCandidateSet::default();
    }

    let entities = candidate_set
        .entities
        .into_iter()
        .filter_map(|mut entity| {
            if is_unstable_graph_label(&entity.label) {
                return None;
            }
            entity.aliases.retain(|alias| !is_unstable_graph_label(alias));
            if entity.summary.as_deref().is_some_and(is_low_confidence_text) {
                entity.summary = None;
            }
            Some(entity)
        })
        .collect::<Vec<_>>();
    let relations = candidate_set
        .relations
        .into_iter()
        .filter_map(|mut relation| {
            if is_unstable_graph_label(&relation.source_label)
                || is_unstable_graph_label(&relation.target_label)
            {
                return None;
            }
            if relation.summary.as_deref().is_some_and(is_low_confidence_text) {
                relation.summary = None;
            }
            Some(relation)
        })
        .collect::<Vec<_>>();

    GraphExtractionCandidateSet { entities, relations }
}

fn is_unstable_graph_label(value: &str) -> bool {
    is_low_confidence_text(value) || is_structurally_unstable_fragment(value)
}

pub fn validate_graph_extraction_candidate_set(
    candidate_set: &GraphExtractionCandidateSet,
) -> std::result::Result<(), GraphExtractionTaskFailure> {
    if candidate_set.entities.iter().any(|entity| entity.label.trim().is_empty())
        || candidate_set.relations.iter().any(|relation| {
            relation.source_label.trim().is_empty()
                || relation.target_label.trim().is_empty()
                || relation.relation_type.trim().is_empty()
        })
    {
        return Err(GraphExtractionTaskFailure {
            code: GraphExtractionTaskFailureCode::InvalidCandidateSet.as_str().to_string(),
            summary: "graph extraction candidate set contains empty labels or relation fields"
                .to_string(),
        });
    }

    Ok(())
}

fn refine_entity_type(label: &str, current_type: RuntimeNodeType) -> RuntimeNodeType {
    // Only refine generic "entity" types
    if current_type != RuntimeNodeType::Entity {
        return current_type;
    }

    let label_trimmed = label.trim();

    // Environment variables: ALL_CAPS_WITH_UNDERSCORES → Attribute (configuration parameters)
    if label_trimmed.len() > 2
        && label_trimmed.chars().all(|c| c.is_ascii_uppercase() || c == '_' || c.is_ascii_digit())
        && label_trimmed.contains('_')
    {
        return RuntimeNodeType::Attribute;
    }

    // URL paths: /api/v1/users → Artifact (human-made endpoints)
    if label_trimmed.starts_with('/') && label_trimmed.len() > 1 {
        return RuntimeNodeType::Artifact;
    }

    // HTTP methods → Artifact
    if matches!(label_trimmed, "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "OPTIONS" | "HEAD") {
        return RuntimeNodeType::Artifact;
    }

    // HTTP status codes: 3 digits 100-599 → Attribute (status indicators)
    if label_trimmed.len() == 3 {
        if let Ok(code) = label_trimmed.parse::<u16>() {
            if (100..600).contains(&code) {
                return RuntimeNodeType::Attribute;
            }
        }
    }

    // File paths: ends with known extension → Artifact (human-made files)
    if label_trimmed.contains('.') {
        let ext = label_trimmed.rsplit('.').next().unwrap_or("");
        if matches!(
            ext,
            "py" | "rs"
                | "ts"
                | "tsx"
                | "js"
                | "go"
                | "java"
                | "kt"
                | "sql"
                | "md"
                | "json"
                | "yaml"
                | "yml"
                | "toml"
                | "xml"
                | "html"
                | "css"
                | "tf"
                | "pdf"
                | "docx"
                | "xls"
                | "xlsx"
                | "xlsb"
                | "ods"
                | "pptx"
                | "pkl"
                | "csv"
        ) {
            return RuntimeNodeType::Artifact;
        }
    }

    // URLs → Artifact
    if label_trimmed.starts_with("http://") || label_trimmed.starts_with("https://") {
        return RuntimeNodeType::Artifact;
    }

    current_type
}

fn parse_entity_candidate(value: &serde_json::Value) -> Option<GraphEntityCandidate> {
    let label = value.get("label").and_then(serde_json::Value::as_str)?.trim();
    if label.is_empty() {
        return None;
    }
    let node_type = parse_canonical_node_type(value.get("node_type")?.as_str()?.trim())?;
    let aliases = value
        .get("aliases")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(std::string::ToString::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let node_type = refine_entity_type(label, node_type);
    let sub_type = value
        .get("sub_type")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);

    Some(GraphEntityCandidate {
        label: label.to_string(),
        node_type,
        sub_type,
        aliases,
        summary: value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(std::string::ToString::to_string),
    })
}

fn parse_relation_candidate(value: &serde_json::Value) -> Option<GraphRelationCandidate> {
    let source_label = value.get("source_label").and_then(serde_json::Value::as_str)?.trim();
    let target_label = value.get("target_label").and_then(serde_json::Value::as_str)?.trim();
    let relation_type = value.get("relation_type").and_then(serde_json::Value::as_str)?.trim();
    if source_label.is_empty() || target_label.is_empty() || relation_type.is_empty() {
        return None;
    }
    let relation_slug =
        crate::services::graph::identity::normalize_graph_identity_component(relation_type);
    if crate::services::graph::identity::is_noise_relation_type(&relation_slug) {
        return None;
    }
    let normalized_relation_type = canonical_relation_candidate_type(relation_type)?;

    Some(GraphRelationCandidate {
        source_label: source_label.to_string(),
        target_label: target_label.to_string(),
        relation_type: normalized_relation_type,
        summary: value
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(std::string::ToString::to_string),
    })
}

fn canonical_relation_candidate_type(relation_type: &str) -> Option<String> {
    if relation_type.is_empty()
        || !relation_type_is_canonical_ascii(relation_type)
        || !crate::services::graph::identity::is_canonical_relation_type(relation_type)
    {
        return None;
    }
    Some(relation_type.to_string())
}

fn relation_type_is_canonical_ascii(relation_type: &str) -> bool {
    relation_type.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

fn parse_canonical_node_type(raw: &str) -> Option<RuntimeNodeType> {
    match raw {
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
    }
}

fn extract_json_payload(output_text: &str) -> AnyhowResult<serde_json::Value> {
    let trimmed = output_text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("graph extraction output is empty"));
    }
    serde_json::from_str::<serde_json::Value>(trimmed).context("invalid graph extraction json")
}
