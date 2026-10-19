use std::collections::{BTreeMap, BTreeSet};

use chrono::Utc;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[cfg(test)]
use crate::services::graph::extract::{GraphEntityCandidate, GraphRelationCandidate};
use crate::{
    domains::runtime_graph::RuntimeNodeType,
    infra::{
        knowledge_rows::{
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
    identity_key: &str,
) -> Option<(RuntimeNodeType, String)> {
    let normalization_key = identity_key.trim();
    let (node_type, primary_label) =
        crate::services::graph::identity::parse_graph_node_identity_key(normalization_key)?;
    Some((node_type, primary_label.to_string()))
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
    if let Some(identity_key) = aliases.get(&candidate.subject_candidate_key) {
        candidate.subject_candidate_key = identity_key.clone();
    }
    if let Some(identity_key) = aliases.get(&candidate.object_candidate_key) {
        candidate.object_candidate_key = identity_key.clone();
    }
    candidate.normalized_assertion = crate::services::graph::identity::graph_edge_identity_key(
        &candidate.subject_candidate_key,
        &candidate.predicate,
        &candidate.object_candidate_key,
    );
}

#[must_use]
pub(super) fn select_primary_entity_label(
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
    // One canonical slug vocabulary: an unknown or absent candidate type keeps
    // this path's explicit `Entity` default at the call site.
    crate::services::graph::identity::runtime_node_type_from_slug(candidate_type)
        .unwrap_or(RuntimeNodeType::Entity)
}

#[must_use]
pub(super) fn build_materialized_extract_candidates(
    revision: &crate::infra::knowledge_rows::KnowledgeRevisionRow,
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
                crate::services::graph::identity::graph_node_identity_key(node_type, display_label);
            let display_label = display_label.to_string();
            if !candidate.identity_key.trim().is_empty() {
                display_labels_by_key
                    .insert(candidate.identity_key.trim().to_string(), display_label.clone());
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
            let subject_candidate_key = candidate.from_identity_key.trim();
            let object_candidate_key = candidate.to_identity_key.trim();
            let subject_display_label =
                display_labels_by_key.get(subject_candidate_key).cloned().or_else(|| {
                    placeholder_entity_parts_from_key(subject_candidate_key)
                        .map(|(_, primary_label)| primary_label)
                })?;
            let object_display_label =
                display_labels_by_key.get(object_candidate_key).cloned().or_else(|| {
                    placeholder_entity_parts_from_key(object_candidate_key)
                        .map(|(_, primary_label)| primary_label)
                })?;
            let normalized_subject_key = crate::services::graph::identity::graph_node_identity_key(
                crate::services::graph::identity::runtime_node_type_from_key(subject_candidate_key),
                &subject_display_label,
            );
            let normalized_object_key = crate::services::graph::identity::graph_node_identity_key(
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
                normalized_assertion: normalized_relation_assertion_from_keys(
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
    _entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
) -> Option<ReconciledEntityCandidate> {
    let trimmed_label = row.candidate_label.trim();
    if trimmed_label.is_empty() {
        return None;
    }
    let node_type = runtime_node_type_from_candidate_type(&row.candidate_type);
    if !crate::services::graph::identity::graph_node_identity_key_matches(
        node_type,
        trimmed_label,
        row.normalization_key.trim(),
    ) {
        return None;
    }
    Some(ReconciledEntityCandidate {
        normalization_key: row.normalization_key.trim().to_string(),
        row,
    })
}

#[must_use]
pub(super) fn reconcile_relation_candidate_row(
    row: KnowledgeRelationCandidateRow,
    _entity_key_index: &crate::services::graph::identity::GraphLabelNodeTypeIndex,
) -> Option<ReconciledRelationCandidate> {
    let predicate = row.predicate.trim().to_string();
    if predicate.is_empty() {
        return None;
    }

    let subject_candidate_key =
        strict_endpoint_key(&row.subject_label, &row.subject_candidate_key)?;
    let object_candidate_key = strict_endpoint_key(&row.object_label, &row.object_candidate_key)?;

    Some(ReconciledRelationCandidate {
        normalized_assertion: normalized_relation_assertion_from_keys(
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
fn strict_endpoint_key(label: &str, candidate_key: &str) -> Option<String> {
    let label = label.trim();
    let candidate_key = candidate_key.trim();
    let (node_type, _) =
        crate::services::graph::identity::parse_graph_node_identity_key(candidate_key)?;
    if label.is_empty()
        || !crate::services::graph::identity::graph_node_identity_key_matches(
            node_type,
            label,
            candidate_key,
        )
    {
        return None;
    }
    Some(candidate_key.to_string())
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
pub(super) fn entity_identity_normalization_key(entity: &GraphEntityCandidate) -> String {
    crate::services::graph::identity::graph_node_identity_key(entity.node_type, &entity.label)
}

#[cfg(test)]
#[must_use]
pub(super) fn normalized_relation_assertion(relation: &GraphRelationCandidate) -> String {
    normalized_relation_assertion_from_keys(
        &crate::services::graph::identity::graph_node_identity_key(
            RuntimeNodeType::Entity,
            &relation.source_label,
        ),
        &relation.relation_type,
        &crate::services::graph::identity::graph_node_identity_key(
            RuntimeNodeType::Entity,
            &relation.target_label,
        ),
    )
}

#[must_use]
pub(super) fn normalized_relation_assertion_from_keys(
    source_candidate_key: &str,
    relation_type: &str,
    target_candidate_key: &str,
) -> String {
    crate::services::graph::identity::graph_edge_identity_key(
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
        index.insert_aliases(&entity.label, &entity.aliases, entity.node_type);
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
pub(super) fn deterministic_entity_candidate_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    normalization_key: &str,
    label: &str,
    node_type: &RuntimeNodeType,
) -> Uuid {
    stable_uuid(&format!(
        "knowledge-entity-candidate:{library_id}:{revision_id}:{chunk_id}:{normalization_key}:{label}:{}",
        crate::services::graph::identity::runtime_node_type_slug(node_type)
    ))
}

/// Identity fields that key a deterministic relation candidate id.
///
/// Grouping the four label/assertion strings keeps the same-typed positional
/// arguments from being swapped; the seed string is unchanged.
pub(super) struct RelationCandidateIdentity<'a> {
    pub(super) normalized_assertion: &'a str,
    pub(super) source_label: &'a str,
    pub(super) target_label: &'a str,
    pub(super) relation_type: &'a str,
}

#[must_use]
pub(super) fn deterministic_relation_candidate_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Uuid,
    identity: RelationCandidateIdentity<'_>,
) -> Uuid {
    let RelationCandidateIdentity {
        normalized_assertion,
        source_label,
        target_label,
        relation_type,
    } = identity;
    stable_uuid(&format!(
        "knowledge-relation-candidate:{library_id}:{revision_id}:{chunk_id}:{normalized_assertion}:{source_label}:{target_label}:{relation_type}"
    ))
}

#[must_use]
pub(super) fn deterministic_entity_id(library_id: Uuid, normalization_key: &str) -> Uuid {
    stable_uuid(&format!("knowledge-entity:{library_id}:{normalization_key}"))
}

#[must_use]
pub(super) fn deterministic_relation_id(library_id: Uuid, normalized_assertion: &str) -> Uuid {
    stable_uuid(&format!("knowledge-relation:{library_id}:{normalized_assertion}"))
}

#[must_use]
pub(super) fn deterministic_evidence_id(
    library_id: Uuid,
    revision_id: Uuid,
    chunk_id: Option<Uuid>,
    support_kind: &str,
    identity_key: &str,
) -> Uuid {
    stable_uuid(&format!(
        "knowledge-evidence:{library_id}:{revision_id}:{}:{support_kind}:{identity_key}",
        chunk_id.map_or_else(|| "none".to_string(), |value| value.to_string())
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
