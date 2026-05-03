use anyhow::{Context, Result, anyhow};

use crate::domains::runtime_graph::RuntimeNodeType;
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

pub fn parse_graph_extraction_output(output_text: &str) -> Result<GraphExtractionCandidateSet> {
    let parsed = extract_json_payload(output_text).map_err(|error| {
        anyhow!("{}: {}", GraphExtractionTaskFailureCode::MalformedOutput.as_str(), error)
    })?;
    // Some models (small-weight Ollama checkpoints, older instruction-
    // tuned SLMs) emit `"nodes"` / `"relationships"` instead of the
    // canonical `"entities"` / `"relations"`. Accept both spellings at
    // the top level so the output from a locally-hosted model is not
    // thrown away over a naming disagreement.
    let entities = parsed
        .get("entities")
        .or_else(|| parsed.get("nodes"))
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(parse_entity_candidate).collect::<Vec<_>>())
        .unwrap_or_default();
    let relations = parsed
        .get("relations")
        .or_else(|| parsed.get("relationships"))
        .or_else(|| parsed.get("edges"))
        .and_then(serde_json::Value::as_array)
        .map(|items| items.iter().filter_map(parse_relation_candidate).collect::<Vec<_>>())
        .unwrap_or_default();

    // Post-extraction: refine "mentions" relations using summary heuristics
    let relations = relations
        .into_iter()
        .map(|mut rel| {
            let refined = refine_mentions_relation(
                &rel.relation_type,
                rel.summary.as_deref(),
                &RuntimeNodeType::Entity, // placeholder — full type-aware refinement in graph_merge
                &RuntimeNodeType::Entity,
            );
            if refined != rel.relation_type
                && crate::services::graph::identity::is_canonical_relation_type(&refined)
            {
                rel.relation_type = refined;
            }
            rel
        })
        .collect::<Vec<_>>();

    let candidate_set = GraphExtractionCandidateSet { entities, relations };
    validate_graph_extraction_candidate_set(&candidate_set)
        .map_err(|failure| anyhow!(failure.summary.clone()))?;
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
) -> Result<(), GraphExtractionTaskFailure> {
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
    if let Some(label) = value.as_str().map(str::trim).filter(|value| !value.is_empty()) {
        return Some(GraphEntityCandidate {
            label: label.to_string(),
            node_type: RuntimeNodeType::Entity,
            sub_type: None,
            aliases: Vec::new(),
            summary: None,
        });
    }

    // Accept `label` (canonical) or `name` (emitted by smaller instruct
    // models). Same for `node_type` vs `type`.
    let label = value
        .get("label")
        .or_else(|| value.get("name"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    if label.is_empty() {
        return None;
    }
    let raw_node_type = value.get("node_type").or_else(|| value.get("type"));
    let node_type = match raw_node_type.and_then(serde_json::Value::as_str) {
        None => RuntimeNodeType::Entity,
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                RuntimeNodeType::Entity
            } else {
                match trimmed.to_ascii_lowercase().as_str() {
                    "person" => RuntimeNodeType::Person,
                    "organization" => RuntimeNodeType::Organization,
                    "location" => RuntimeNodeType::Location,
                    "event" => RuntimeNodeType::Event,
                    "artifact" => RuntimeNodeType::Artifact,
                    "natural" => RuntimeNodeType::Natural,
                    "process" => RuntimeNodeType::Process,
                    "concept" => RuntimeNodeType::Concept,
                    "attribute" => RuntimeNodeType::Attribute,
                    "entity" => RuntimeNodeType::Entity,
                    _ => RuntimeNodeType::Entity,
                }
            }
        }
    };
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
    // Accept the canonical `source_label` / `target_label` /
    // `relation_type` trio plus the common aliases emitted by smaller
    // Ollama-hosted models (`source`/`from`, `target`/`to`,
    // `relation`/`type`).
    let source_label = value
        .get("source_label")
        .or_else(|| value.get("source"))
        .or_else(|| value.get("from"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    let target_label = value
        .get("target_label")
        .or_else(|| value.get("target"))
        .or_else(|| value.get("to"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    let relation_type = value
        .get("relation_type")
        .or_else(|| value.get("relation"))
        .or_else(|| value.get("type"))
        .and_then(serde_json::Value::as_str)?
        .trim();
    if source_label.is_empty() || target_label.is_empty() || relation_type.is_empty() {
        return None;
    }
    let relation_slug =
        crate::services::graph::identity::normalize_graph_identity_component(relation_type);
    if crate::services::graph::identity::is_noise_relation_type(&relation_slug) {
        return None;
    }
    let normalized_relation_type = normalize_relation_candidate_type(relation_type)?;

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

fn normalize_relation_candidate_type(relation_type: &str) -> Option<String> {
    let normalized = crate::services::graph::identity::normalize_relation_type(relation_type);
    if normalized.is_empty()
        || !relation_type_is_canonical_ascii(&normalized)
        || !crate::services::graph::identity::is_canonical_relation_type(&normalized)
    {
        return None;
    }
    Some(normalized)
}

fn relation_type_is_canonical_ascii(normalized_relation_type: &str) -> bool {
    normalized_relation_type.bytes().all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'_'))
}

/// Post-extraction heuristic to reduce "mentions" overuse.
/// When the LLM outputs "mentions" but the summary text suggests a more specific relation,
/// upgrade to the more specific type.
fn refine_mentions_relation(
    relation_type: &str,
    summary: Option<&str>,
    source_type: &crate::domains::runtime_graph::RuntimeNodeType,
    _target_type: &crate::domains::runtime_graph::RuntimeNodeType,
) -> String {
    if relation_type != "mentions" {
        return relation_type.to_string();
    }

    // Check summary for action verbs that suggest a more specific relation
    if let Some(summary) = summary {
        let s = summary.to_ascii_lowercase();
        if s.contains("depends on") || s.contains("requires") || s.contains("needs") {
            return "depends_on".to_string();
        }
        if s.contains("uses") || s.contains("utilizes") || s.contains("leverages") {
            return "uses".to_string();
        }
        if s.contains("contains") || s.contains("includes") || s.contains("consists of") {
            return "contains".to_string();
        }
        if s.contains("implements") || s.contains("implementation of") {
            return "implements".to_string();
        }
        if s.contains("extends") || s.contains("inherits") {
            return "extends".to_string();
        }
        if s.contains("returns") || s.contains("produces") || s.contains("outputs") {
            return "returns".to_string();
        }
        if s.contains("configures") || s.contains("configuration") {
            return "configures".to_string();
        }
        if s.contains("calls") || s.contains("invokes") {
            return "calls".to_string();
        }
        if s.contains("authenticat") || s.contains("authoriz") {
            return "authenticates".to_string();
        }
        if s.contains("defines") || s.contains("declares") || s.contains("specifies") {
            return "defines".to_string();
        }
        if s.contains("provides") || s.contains("exposes") || s.contains("offers") {
            return "provides".to_string();
        }
        if s.contains("deployed") || s.contains("runs on") || s.contains("hosted") {
            return "deployed_on".to_string();
        }
    }

    // Type-based heuristic: document → entity/code_symbol is usually "describes" not "mentions"
    use crate::domains::runtime_graph::RuntimeNodeType;
    if *source_type == RuntimeNodeType::Document {
        return "describes".to_string();
    }

    relation_type.to_string()
}

fn extract_json_payload(output_text: &str) -> Result<serde_json::Value> {
    let trimmed = output_text.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("graph extraction output is empty"));
    }
    serde_json::from_str::<serde_json::Value>(trimmed).context("invalid graph extraction json")
}
