use std::collections::HashMap;

use ironrag_contracts::assistant::semantic_verification::{
    SemanticClaimSupport, SemanticSourceLocator, SemanticVerificationOutcome,
    SemanticVerificationReport,
};
use uuid::Uuid;

use crate::services::query::reference_selection::{
    ReferenceSelectionCandidate, chunk_group_key, document_group_key, entity_group_key,
    relation_group_key, revision_group_key, segment_group_key, select_reference_indices,
};

use super::*;

pub(super) use crate::domains::query::VISIBLE_REFERENCE_LIMIT as UI_VISIBLE_REFERENCE_LIMIT;

pub(super) struct UiReferenceProjection {
    chunk_references: Vec<ironrag_contracts::assistant::AssistantChunkReference>,
    prepared_segment_references:
        Vec<ironrag_contracts::assistant::AssistantPreparedSegmentReference>,
    technical_fact_references: Vec<ironrag_contracts::assistant::AssistantTechnicalFactReference>,
    entity_references: Vec<ironrag_contracts::assistant::AssistantEntityReference>,
    relation_references: Vec<ironrag_contracts::assistant::AssistantRelationReference>,
    summary: ironrag_contracts::assistant::AssistantReferenceSummary,
}

enum UiRankedReference {
    Chunk(ironrag_contracts::assistant::AssistantChunkReference),
    PreparedSegment(ironrag_contracts::assistant::AssistantPreparedSegmentReference),
    TechnicalFact(ironrag_contracts::assistant::AssistantTechnicalFactReference),
    Entity(ironrag_contracts::assistant::AssistantEntityReference),
    Relation(ironrag_contracts::assistant::AssistantRelationReference),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum UiReferenceIdentity {
    Chunk(Uuid),
    Segment(Uuid),
}

/// The reference arrays of one evidence bundle or execution detail, before the
/// shared UI projection assigns their mixed selection.
pub(super) struct UiReferenceArrays {
    pub(super) chunk_references: Vec<ironrag_contracts::assistant::AssistantChunkReference>,
    pub(super) prepared_segment_references:
        Vec<ironrag_contracts::assistant::AssistantPreparedSegmentReference>,
    pub(super) technical_fact_references:
        Vec<ironrag_contracts::assistant::AssistantTechnicalFactReference>,
    pub(super) entity_references: Vec<ironrag_contracts::assistant::AssistantEntityReference>,
    pub(super) relation_references: Vec<ironrag_contracts::assistant::AssistantRelationReference>,
}

pub(super) fn project_ui_reference_arrays(
    arrays: UiReferenceArrays,
    total_count_hint: Option<usize>,
    semantic_verification: Option<&SemanticVerificationReport>,
) -> UiReferenceProjection {
    let UiReferenceArrays {
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
    } = arrays;
    let visible_count = chunk_references
        .len()
        .saturating_add(prepared_segment_references.len())
        .saturating_add(technical_fact_references.len())
        .saturating_add(entity_references.len())
        .saturating_add(relation_references.len());
    let total_count = total_count_hint.unwrap_or(visible_count).max(visible_count);
    let mut references = Vec::with_capacity(visible_count);
    let mut candidates = Vec::with_capacity(visible_count);
    let mut identities = HashMap::new();

    for reference in chunk_references {
        let index = references.len();
        identities.insert(UiReferenceIdentity::Chunk(reference.chunk_id), index);
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 0,
            group: reference
                .document_id
                .map(document_group_key)
                .unwrap_or_else(|| chunk_group_key(reference.chunk_id)),
            document_backed: true,
        });
        references.push(Some(UiRankedReference::Chunk(reference)));
    }
    for reference in prepared_segment_references {
        let index = references.len();
        identities.insert(UiReferenceIdentity::Segment(reference.segment_id), index);
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 1,
            group: reference
                .document_id
                .map(document_group_key)
                .unwrap_or_else(|| segment_group_key(reference.segment_id)),
            document_backed: true,
        });
        references.push(Some(UiRankedReference::PreparedSegment(reference)));
    }
    for reference in technical_fact_references {
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 2,
            group: revision_group_key(reference.revision_id),
            document_backed: false,
        });
        references.push(Some(UiRankedReference::TechnicalFact(reference)));
    }
    for reference in entity_references {
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 3,
            group: entity_group_key(reference.node_id),
            document_backed: false,
        });
        references.push(Some(UiRankedReference::Entity(reference)));
    }
    for reference in relation_references {
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 4,
            group: relation_group_key(reference.edge_id),
            document_backed: false,
        });
        references.push(Some(UiRankedReference::Relation(reference)));
    }

    // DocumentSlice evidence has no typed UI reference. Such a unit stays
    // unresolved here; the retained `semantic_verification` report remains the
    // authoritative coverage signal instead of a fabricated reference.
    let claim_units =
        semantic_verification.map(|report| ui_claim_units(report, &identities)).unwrap_or_default();
    let selection = select_reference_indices(&candidates, &claim_units, UI_VISIBLE_REFERENCE_LIMIT);
    let returned_count = selection.indices.len();
    let mut projected = UiReferenceProjection {
        chunk_references: Vec::new(),
        prepared_segment_references: Vec::new(),
        technical_fact_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        summary: ironrag_contracts::assistant::AssistantReferenceSummary {
            total_count,
            returned_count,
            truncated: total_count > returned_count,
        },
    };
    for index in selection.indices {
        let Some(reference) = references[index].take() else {
            continue;
        };
        match reference {
            UiRankedReference::Chunk(reference) => projected.chunk_references.push(reference),
            UiRankedReference::PreparedSegment(reference) => {
                projected.prepared_segment_references.push(reference);
            }
            UiRankedReference::TechnicalFact(reference) => {
                projected.technical_fact_references.push(reference);
            }
            UiRankedReference::Entity(reference) => projected.entity_references.push(reference),
            UiRankedReference::Relation(reference) => projected.relation_references.push(reference),
        }
    }
    projected
}

fn ui_claim_units(
    report: &SemanticVerificationReport,
    identities: &HashMap<UiReferenceIdentity, usize>,
) -> Vec<Vec<usize>> {
    let SemanticVerificationOutcome::Completed { assessment } = &report.outcome else {
        return Vec::new();
    };
    let sources = report
        .sources
        .iter()
        .map(|source| (source.source_id.as_str(), source))
        .collect::<HashMap<_, _>>();
    assessment
        .units
        .iter()
        .filter(|unit| unit.support != SemanticClaimSupport::NoFactualClaims)
        .map(|unit| {
            let mut indices = Vec::new();
            for evidence in &unit.evidence {
                let Some(source) = sources.get(evidence.source_id.as_str()).copied() else {
                    continue;
                };
                let index = match &source.locator {
                    SemanticSourceLocator::Chunk { chunk_id } => {
                        identities.get(&UiReferenceIdentity::Chunk(*chunk_id)).copied()
                    }
                    SemanticSourceLocator::Segment { segment_id } => {
                        identities.get(&UiReferenceIdentity::Segment(*segment_id)).copied()
                    }
                    SemanticSourceLocator::DocumentSlice { .. } => None,
                };
                if let Some(index) = index
                    && !indices.contains(&index)
                {
                    indices.push(index);
                }
            }
            indices
        })
        .collect()
}

pub(super) fn project_ui_evidence_bundle(
    evidence: ironrag_contracts::assistant::AssistantEvidenceBundle,
) -> ironrag_contracts::assistant::AssistantEvidenceBundle {
    let ironrag_contracts::assistant::AssistantEvidenceBundle {
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary,
        verification_state,
        verification_warnings,
        semantic_verification,
        answer_disposition,
        clarification,
        runtime_summary,
        runtime_stage_summaries,
    } = evidence;
    let projection = project_ui_reference_arrays(
        UiReferenceArrays {
            chunk_references,
            prepared_segment_references,
            technical_fact_references,
            entity_references,
            relation_references,
        },
        reference_summary.map(|summary| summary.total_count),
        semantic_verification.as_ref(),
    );
    ironrag_contracts::assistant::AssistantEvidenceBundle {
        chunk_references: projection.chunk_references,
        prepared_segment_references: projection.prepared_segment_references,
        technical_fact_references: projection.technical_fact_references,
        entity_references: projection.entity_references,
        relation_references: projection.relation_references,
        reference_summary: Some(projection.summary),
        verification_state,
        verification_warnings,
        semantic_verification,
        answer_disposition,
        clarification,
        runtime_summary,
        runtime_stage_summaries,
    }
}

pub(super) fn project_ui_execution_detail(
    detail: ironrag_contracts::assistant::AssistantExecutionDetail,
) -> ironrag_contracts::assistant::AssistantExecutionDetail {
    let ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id,
        execution,
        runtime_summary,
        runtime_stage_summaries,
        request_turn,
        response_turn,
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary,
        verification_state,
        verification_warnings,
        semantic_verification,
        answer_disposition,
        clarification,
    } = detail;
    let projection = project_ui_reference_arrays(
        UiReferenceArrays {
            chunk_references,
            prepared_segment_references,
            technical_fact_references,
            entity_references,
            relation_references,
        },
        reference_summary.map(|summary| summary.total_count),
        semantic_verification.as_ref(),
    );
    ironrag_contracts::assistant::AssistantExecutionDetail {
        context_bundle_id,
        execution,
        runtime_summary,
        runtime_stage_summaries,
        request_turn,
        response_turn,
        chunk_references: projection.chunk_references,
        prepared_segment_references: projection.prepared_segment_references,
        technical_fact_references: projection.technical_fact_references,
        entity_references: projection.entity_references,
        relation_references: projection.relation_references,
        reference_summary: Some(projection.summary),
        verification_state,
        verification_warnings,
        semantic_verification,
        answer_disposition,
        clarification,
    }
}
