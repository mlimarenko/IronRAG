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

pub(super) enum UiRankedReference {
    Chunk(ironrag_contracts::assistant::AssistantChunkReference),
    PreparedSegment(ironrag_contracts::assistant::AssistantPreparedSegmentReference),
    TechnicalFact(ironrag_contracts::assistant::AssistantTechnicalFactReference),
    Entity(ironrag_contracts::assistant::AssistantEntityReference),
    Relation(ironrag_contracts::assistant::AssistantRelationReference),
}

impl UiRankedReference {
    const fn rank(&self) -> i32 {
        match self {
            Self::Chunk(reference) => reference.rank,
            Self::PreparedSegment(reference) => reference.rank,
            Self::TechnicalFact(reference) => reference.rank,
            Self::Entity(reference) => reference.rank,
            Self::Relation(reference) => reference.rank,
        }
    }

    const fn kind_order(&self) -> u8 {
        match self {
            Self::Chunk(_) => 0,
            Self::PreparedSegment(_) => 1,
            Self::TechnicalFact(_) => 2,
            Self::Entity(_) => 3,
            Self::Relation(_) => 4,
        }
    }
}

pub(super) fn project_ui_reference_arrays(
    chunk_references: Vec<ironrag_contracts::assistant::AssistantChunkReference>,
    prepared_segment_references: Vec<
        ironrag_contracts::assistant::AssistantPreparedSegmentReference,
    >,
    technical_fact_references: Vec<ironrag_contracts::assistant::AssistantTechnicalFactReference>,
    entity_references: Vec<ironrag_contracts::assistant::AssistantEntityReference>,
    relation_references: Vec<ironrag_contracts::assistant::AssistantRelationReference>,
    total_count_hint: Option<usize>,
) -> UiReferenceProjection {
    let visible_count = chunk_references
        .len()
        .saturating_add(prepared_segment_references.len())
        .saturating_add(technical_fact_references.len())
        .saturating_add(entity_references.len())
        .saturating_add(relation_references.len());
    let total_count = total_count_hint.unwrap_or(visible_count).max(visible_count);
    let mut ranked = Vec::with_capacity(visible_count);

    ranked.extend(
        chunk_references.into_iter().enumerate().map(|(original_order, reference)| {
            (original_order, UiRankedReference::Chunk(reference))
        }),
    );
    ranked.extend(prepared_segment_references.into_iter().enumerate().map(
        |(original_order, reference)| {
            (original_order, UiRankedReference::PreparedSegment(reference))
        },
    ));
    ranked.extend(technical_fact_references.into_iter().enumerate().map(
        |(original_order, reference)| (original_order, UiRankedReference::TechnicalFact(reference)),
    ));
    ranked.extend(
        entity_references.into_iter().enumerate().map(|(original_order, reference)| {
            (original_order, UiRankedReference::Entity(reference))
        }),
    );
    ranked.extend(relation_references.into_iter().enumerate().map(
        |(original_order, reference)| (original_order, UiRankedReference::Relation(reference)),
    ));
    ranked.sort_by_key(|(original_order, reference)| {
        (reference.rank(), reference.kind_order(), *original_order)
    });

    let returned_count = ranked.len().min(UI_VISIBLE_REFERENCE_LIMIT);
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
    for (_, reference) in ranked.into_iter().take(UI_VISIBLE_REFERENCE_LIMIT) {
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
        answer_disposition,
        clarification,
        runtime_summary,
        runtime_stage_summaries,
    } = evidence;
    let projection = project_ui_reference_arrays(
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary.map(|summary| summary.total_count),
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
        answer_disposition,
        clarification,
    } = detail;
    let projection = project_ui_reference_arrays(
        chunk_references,
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        reference_summary.map(|summary| summary.total_count),
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
        answer_disposition,
        clarification,
    }
}
