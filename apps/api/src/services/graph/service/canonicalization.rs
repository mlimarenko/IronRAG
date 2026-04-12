use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(test)]
use crate::services::graph::extract::{GraphEntityCandidate, GraphRelationCandidate};
use crate::{
    domains::runtime_graph::RuntimeNodeType,
    infra::{
        arangodb::graph_store::{
            KnowledgeEntityCandidateRow, KnowledgeRelationCandidateRow,
            NewKnowledgeEntityCandidate, NewKnowledgeRelationCandidate,
        },
        repositories,
    },
    services::graph::extract::GraphExtractionCandidateSet,
};

#[derive(Debug, Clone)]
pub(super) struct ReconciledEntityCandidate {
    pub(super) row: KnowledgeEntityCandidateRow,
    pub(super) normalization_key: String,
}

#[derive(Debug, Clone)]
pub(super) struct ReconciledRelationCandidate {
    pub(super) row: KnowledgeRelationCandidateRow,
    pub(super) subject_candidate_key: String,
    pub(super) predicate: String,
    pub(super) object_candidate_key: String,
    pub(super) normalized_assertion: String,
}

#[derive(Debug, Default)]
pub(super) struct MaterializedExtractCandidates {
    pub(super) entity_candidates: Vec<NewKnowledgeEntityCandidate>,
    pub(super) relation_candidates: Vec<NewKnowledgeRelationCandidate>,
}

#[must_use]
pub(super) fn placeholder_entity_parts_from_key(
    canonical_key: &str,
) -> Option<(RuntimeNodeType, String)> {
    let normalization_key = canonical_key.trim();
    if normalization_key.is_empty() {
        return None;
    }
    let canonical_label = normalization_key
        .split_once(':')
        .map(|(_, label)| label)
        .unwrap_or(normalization_key)
        .trim();
    if canonical_label.is_empty() {
        return None;
    }
    Some((
        crate::services::graph::identity::runtime_node_type_from_key(normalization_key),
        canonical_label.to_string(),
    ))
}

#[must_use]
pub(super) fn build_prefixed_entity_key_aliases(
    entity_candidates: &[ReconciledEntityCandidate],
) -> BTreeMap<String, String> {
    let known_keys = entity_candidates
        .iter()
        .map(|candidate| candidate.normalization_key.clone())
        .collect::<BTreeSet<_>>();
    let mut aliases = BTreeMap::<String, String>::new();

    for key in &known_keys {
        let Some((node_type, identity)) = key.split_once(':') else {
            continue;
        };
        let mut parts = identity.split('_');
        parts.next();
        let stripped_identity = parts.collect::<Vec<_>>().join("_");
        if stripped_identity.is_empty() {
            continue;
        }
        let stripped_key = format!("{node_type}:{stripped_identity}");
        if known_keys.contains(&stripped_key) {
            aliases.insert(stripped_key, key.clone());
        }
    }

    aliases
}

pub(super) fn apply_entity_key_aliases_to_relation_candidate(
    candidate: &mut ReconciledRelationCandidate,
    aliases: &BTreeMap<String, String>,
) {
    if let Some(canonical_key) = aliases.get(&candidate.subject_candidate_key) {
        candidate.subject_candidate_key = canonical_key.clone();
    }
    if let Some(canonical_key) = aliases.get(&candidate.object_candidate_key) {
        candidate.object_candidate_key = canonical_key.clone();
    }
    candidate.normalized_assertion = crate::services::graph::identity::canonical_edge_key(
        &candidate.subject_candidate_key,
        &candidate.predicate,
        &candidate.object_candidate_key,
    );
}

#[must_use]
pub(super) fn select_canonical_entity_label(
    rows: &[ReconciledEntityCandidate],
    normalization_key: &str,
) -> Option<String> {
    let expected_identity = normalization_key.split_once(':').map(|(_, identity)| identity)?;

    rows.iter()
        .filter_map(|candidate| {
            let label = candidate.row.candidate_label.trim();
            if label.is_empty() {
                return None;
            }
            let label_identity =
                crate::services::graph::identity::normalize_graph_identity_component(label);
            let exact_match = u8::from(label_identity == expected_identity);
            let word_like_bonus = u8::from(!label.contains('_'));
            let length = label.len();
            Some(((exact_match, word_like_bonus, length), label.to_string()))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)))
        .map(|(_, label)| label)
}

#[must_use]
fn runtime_node_type_from_candidate_type(candidate_type: &str) -> RuntimeNodeType {
    match candidate_type.trim() {
        "document" => RuntimeNodeType::Document,
        "person" => RuntimeNodeType::Person,
        "organization" => RuntimeNodeType::Organization,
        "location" => RuntimeNodeType::Location,
        "event" => RuntimeNodeType::Event,
        "artifact" => RuntimeNodeType::Artifact,
        "natural" => RuntimeNodeType::Natural,
        "process" => RuntimeNodeType::Process,
        "concept" => RuntimeNodeType::Concept,
        "attribute" => RuntimeNodeType::Attribute,
        "topic" => RuntimeNodeType::Concept,
        "technology" => RuntimeNodeType::Artifact,
        "api" => RuntimeNodeType::Artifact,
        "code_symbol" => RuntimeNodeType::Artifact,
        "natural_kind" => RuntimeNodeType::Natural,
        "metric" => RuntimeNodeType::Attribute,
        "regulation" => RuntimeNodeType::Artifact,
        _ => RuntimeNodeType::Entity,
    }
}

#[must_use]
pub(super) fn build_materialized_extract_candidates(
    revision: &crate::infra::arangodb::document_store::KnowledgeRevisionRow,
    chunk_result: &repositories::extract_repository::ExtractChunkResultRow,
    node_candidates: &[repositories::extract_repository::ExtractNodeCandidateRow],
    edge_candidates: &[repositories::extract_repository::ExtractEdgeCandidateRow],
) -> MaterializedExtractCandidates {
    let mut display_labels_by_key = BTreeMap::<String, String>::new();
    let entity_candidates = node_candidates
        .iter()
        .filter_map(|candidate| {
            let display_label = candidate.display_label.trim();
            if display_label.is_empty() {
                return None;
            }
            let node_type = runtime_node_type_from_candidate_type(&candidate.node_kind);
            let candidate_type =
                crate::services::graph::identity::runtime_node_type_slug(&node_type).to_string();
            let normalization_key =
                crate::services::graph::identity::canonical_node_key(node_type, display_label);
            let display_label = display_label.to_string();
            if !candidate.canonical_key.trim().is_empty() {
                display_labels_by_key
                    .insert(candidate.canonical_key.trim().to_string(), display_label.clone());
            }
            display_labels_by_key.insert(normalization_key.clone(), display_label.clone());
            Some(NewKnowledgeEntityCandidate {
                candidate_id: candidate.id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                revision_id: revision.revision_id,
                chunk_id: Some(chunk_result.chunk_id),
                candidate_label: display_label,
                candidate_type,
                candidate_sub_type: None,
                normalization_key,
                confidence: None,
                extraction_method: "extract_chunk_result".to_string(),
                candidate_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
        })
        .collect::<Vec<_>>();

    let relation_candidates = edge_candidates
        .iter()
        .filter_map(|candidate| {
            let predicate = candidate.edge_kind.trim();
            if predicate.is_empty() {
                return None;
            }
            let subject_candidate_key = candidate.from_canonical_key.trim();
            let object_candidate_key = candidate.to_canonical_key.trim();
            let subject_display_label =
                display_labels_by_key.get(subject_candidate_key).cloned().or_else(|| {
                    placeholder_entity_parts_from_key(subject_candidate_key)
                        .map(|(_, canonical_label)| canonical_label)
                })?;
            let object_display_label =
                display_labels_by_key.get(object_candidate_key).cloned().or_else(|| {
                    placeholder_entity_parts_from_key(object_candidate_key)
                        .map(|(_, canonical_label)| canonical_label)
                })?;
            let normalized_subject_key = crate::services::graph::identity::canonical_node_key(
                crate::services::graph::identity::runtime_node_type_from_key(subject_candidate_key),
                &subject_display_label,
            );
            let normalized_object_key = crate::services::graph::identity::canonical_node_key(
                crate::services::graph::identity::runtime_node_type_from_key(object_candidate_key),
                &object_display_label,
            );
            Some(NewKnowledgeRelationCandidate {
                candidate_id: candidate.id,
                workspace_id: revision.workspace_id,
                library_id: revision.library_id,
                revision_id: revision.revision_id,
                chunk_id: Some(chunk_result.chunk_id),
                subject_label: subject_display_label,
                subject_candidate_key: normalized_subject_key.clone(),
                predicate: predicate.to_string(),
                object_label: object_display_label,
                object_candidate_key: normalized_object_key.clone(),
                normalized_assertion: canonical_relation_assertion_from_keys(
                    &normalized_subject_key,
                    predicate,
                    &normalized_object_key,
                ),
                confidence: None,
                extraction_method: "extract_chunk_result".to_string(),
                candidate_state: "active".to_string(),
                created_at: Some(Utc::now()),
                updated_at: Some(Utc::now()),
            })
        })
        .collect::<Vec<_>>();

    MaterializedExtractCandidates { entity_candidates, relation_candidates }
}

#[must_use]
pub(super) fn reconcile_entity_candidate_row(
    row: KnowledgeEntityCandidateRow,
    entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
) -> Option<ReconciledEntityCandidate> {
    let trimmed_label = row.candidate_label.trim();
    if trimmed_label.is_empty() {
        return None;
    }
    Some(ReconciledEntityCandidate {
        normalization_key: entity_key_index.canonical_node_key_for_label(trimmed_label),
        row,
    })
}

#[must_use]
pub(super) fn reconcile_relation_candidate_row(
    row: KnowledgeRelationCandidateRow,
    entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
) -> Option<ReconciledRelationCandidate> {
    let predicate = row.predicate.trim().to_string();
    if predicate.is_empty() {
        return None;
    }

    let subject_candidate_key = if !row.subject_label.trim().is_empty() {
        entity_key_index.canonical_node_key_for_label(row.subject_label.trim())
    } else if let Some((_, canonical_label)) =
        placeholder_entity_parts_from_key(&row.subject_candidate_key)
    {
        entity_key_index.canonical_node_key_for_label(&canonical_label)
    } else {
        return None;
    };

    let object_candidate_key = if !row.object_label.trim().is_empty() {
        entity_key_index.canonical_node_key_for_label(row.object_label.trim())
    } else if let Some((_, canonical_label)) =
        placeholder_entity_parts_from_key(&row.object_candidate_key)
    {
        entity_key_index.canonical_node_key_for_label(&canonical_label)
    } else {
        return None;
    };

    Some(ReconciledRelationCandidate {
        normalized_assertion: canonical_relation_assertion_from_keys(
            &subject_candidate_key,
            &predicate,
            &object_candidate_key,
        ),
        row,
        subject_candidate_key,
        predicate,
        object_candidate_key,
    })
}

#[must_use]
pub(super) fn relation_candidate_keys_are_materializable(
    subject_candidate_key: &str,
    predicate: &str,
    object_candidate_key: &str,
) -> bool {
    !super::normalize_evidence_literal(predicate).is_empty()
        && placeholder_entity_parts_from_key(subject_candidate_key).is_some()
        && placeholder_entity_parts_from_key(object_candidate_key).is_some()
}

#[cfg(test)]
pub(super) fn canonical_entity_normalization_key(entity: &GraphEntityCandidate) -> String {
    crate::services::graph::identity::canonical_node_key(entity.node_type.clone(), &entity.label)
}

#[cfg(test)]
#[must_use]
pub(super) fn canonical_relation_assertion(relation: &GraphRelationCandidate) -> String {
    canonical_relation_assertion_from_keys(
        &crate::services::graph::identity::canonical_node_key(
            RuntimeNodeType::Entity,
            &relation.source_label,
        ),
        &relation.relation_type,
        &crate::services::graph::identity::canonical_node_key(
            RuntimeNodeType::Entity,
            &relation.target_label,
        ),
    )
}

#[must_use]
pub(super) fn canonical_relation_assertion_from_keys(
    source_candidate_key: &str,
    relation_type: &str,
    target_candidate_key: &str,
) -> String {
    crate::services::graph::identity::canonical_edge_key(
        source_candidate_key,
        relation_type,
        target_candidate_key,
    )
}

#[must_use]
pub(super) fn build_relation_entity_key_index(
    candidates: &GraphExtractionCandidateSet,
) -> crate::services::graph::identity::GraphLabelNodeTypeIndex {
    let mut index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    for entity in &candidates.entities {
        index.insert_aliases(&entity.label, &entity.aliases, entity.node_type.clone());
    }
    index
}

#[must_use]
pub(super) fn build_entity_candidate_key_index(
    candidates: &[KnowledgeEntityCandidateRow],
) -> crate::services::graph::identity::GraphLabelNodeTypeIndex {
    let mut index = crate::services::graph::identity::GraphLabelNodeTypeIndex::new();
    for candidate in candidates {
        let label = candidate.candidate_label.trim();
        if label.is_empty() {
            continue;
        }
        index.insert(label, runtime_node_type_from_candidate_type(&candidate.candidate_type));
    }
    index
}

#[must_use]
pub(super) fn canonical_entity_candidate_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    normalization_key: &str,
    label: &str,
    node_type: &RuntimeNodeType,
) -> Uuid {
    stable_uuid(&format!(
        "arango-entity-candidate:{library_id}:{revision_id}:{chunk_id}:{normalization_key}:{label}:{}",
        crate::services::graph::identity::runtime_node_type_slug(node_type)
    ))
}

#[must_use]
pub(super) fn canonical_relation_candidate_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    normalized_assertion: &str,
    source_label: &str,
    target_label: &str,
    relation_type: &str,
) -> Uuid {
    stable_uuid(&format!(
        "arango-relation-candidate:{library_id}:{revision_id}:{chunk_id}:{normalized_assertion}:{source_label}:{target_label}:{relation_type}"
    ))
}

#[must_use]
pub(super) fn canonical_entity_id(library_id: Uuid, normalization_key: &str) -> Uuid {
    stable_uuid(&format!("arango-entity:{library_id}:{normalization_key}"))
}

#[must_use]
pub(super) fn canonical_relation_id(library_id: Uuid, normalized_assertion: &str) -> Uuid {
    stable_uuid(&format!("arango-relation:{library_id}:{normalized_assertion}"))
}

#[must_use]
pub(super) fn canonical_evidence_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Option<Uuid>,
    support_kind: &str,
    canonical_key: &str,
) -> Uuid {
    stable_uuid(&format!(
        "arango-evidence:{library_id}:{revision_id}:{}:{support_kind}:{canonical_key}",
        chunk_id.map(|value| value.to_string()).unwrap_or_else(|| "none".to_string())
    ))
}

#[must_use]
fn stable_uuid(seed: &str) -> Uuid {
    let digest = Sha256::digest(seed.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

#[must_use]
pub(super) fn canonical_document_revision_edge_key(document_id: Uuid, revision_id: Uuid) -> String {
    format!("document:{document_id}:revision:{revision_id}")
}

#[must_use]
pub(super) fn canonical_revision_chunk_edge_key(revision_id: Uuid, chunk_id: Uuid) -> String {
    format!("revision:{revision_id}:chunk:{chunk_id}")
}

#[must_use]
pub(super) fn canonical_edge_relation_key(
    relation_id: Uuid,
    entity_id: Uuid,
    edge_kind: &str,
) -> String {
    format!("relation:{relation_id}:{edge_kind}:{entity_id}")
}

#[must_use]
pub(super) fn canonical_chunk_mentions_entity_edge_key(chunk_id: Uuid, entity_id: Uuid) -> String {
    format!("chunk:{chunk_id}:mentions:{entity_id}")
}
