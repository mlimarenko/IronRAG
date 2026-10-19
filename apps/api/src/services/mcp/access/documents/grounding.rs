use super::*;

/// 1-based, saturating conversion from a search-hit index to an `i32`
/// rank. Split out of the former `services/mcp/support.rs` god-file
/// (plan §6.4) as a module-private helper: this was its only caller in
/// the MCP domain (`services::query::service` independently defines an
/// equivalent for its own ranking, tracked separately, out of scope
/// here).
pub(super) fn saturating_rank(index: usize) -> i32 {
    i32::try_from(index.saturating_add(1)).unwrap_or(i32::MAX)
}

pub(crate) async fn collect_revision_grounding_references(
    state: &AppState,
    revision_id: Uuid,
    chunk_ids: &[Uuid],
    limit: usize,
) -> Result<McpRevisionGroundingReferences, ApiError> {
    let technical_facts = state
        .document_store
        .list_technical_facts_by_revision(revision_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let mut technical_fact_rows = technical_facts;
    technical_fact_rows.sort_by(|left, right| {
        technical_fact_support_score(right, chunk_ids)
            .cmp(&technical_fact_support_score(left, chunk_ids))
            .then_with(|| {
                right.confidence.unwrap_or(0.0).total_cmp(&left.confidence.unwrap_or(0.0))
            })
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.fact_id.cmp(&right.fact_id))
    });
    technical_fact_rows.truncate(limit);
    let technical_fact_references = technical_fact_rows
        .into_iter()
        .enumerate()
        .map(|(index, fact)| McpTechnicalFactReference {
            fact_id: fact.fact_id,
            fact_kind: fact.fact_kind,
            normalized_value: fact.normalized_value_text,
            display_value: fact.display_value,
            rank: saturating_rank(index),
            score: fact.confidence.unwrap_or(1.0),
            inclusion_reason: Some(
                if fact_supports_requested_chunks(&fact.support_chunk_ids, chunk_ids) {
                    "chunk_supported_fact"
                } else {
                    "revision_fact"
                }
                .to_string(),
            ),
        })
        .collect::<Vec<_>>();
    let evidence_rows = state
        .graph_store
        .list_evidence_by_revision(revision_id)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;
    let mut evidence_rows = evidence_rows;
    evidence_rows.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.evidence_id.cmp(&right.evidence_id))
    });
    evidence_rows.truncate(limit);
    let evidence_references = evidence_rows
        .iter()
        .enumerate()
        .map(|(index, evidence)| McpEvidenceReference {
            evidence_id: evidence.evidence_id,
            rank: saturating_rank(index),
            score: evidence.confidence.unwrap_or(1.0),
            inclusion_reason: Some("revision_evidence".to_string()),
        })
        .collect::<Vec<_>>();
    let evidence_ids = evidence_rows.iter().map(|row| row.evidence_id).collect::<Vec<_>>();

    let entity_references = if chunk_ids.is_empty() {
        Vec::new()
    } else {
        let rows = sqlx::query_as::<_, PgEntityReferenceRow>(
            "select to_id as entity_id,
                    min(rank) as rank,
                    max(score) as score,
                    (array_agg(
                        inclusion_reason
                        order by rank asc nulls last,
                                 created_at asc nulls last,
                                 from_id asc,
                                 to_id asc,
                                 relation_type asc
                    ))[1] as inclusion_reason
             from knowledge_chunk_entity_mention
             where from_id = any($1::uuid[])
             group by to_id
             order by min(rank) asc nulls last,
                      max(score) desc nulls last,
                      to_id asc
             limit $2",
        )
        .bind(chunk_ids)
        .bind(limit.max(1) as i64)
        .fetch_all(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

        rows.into_iter().map(PgEntityReferenceRow::into_mcp).collect()
    };

    let relation_references = if evidence_ids.is_empty() {
        Vec::new()
    } else {
        let rows = sqlx::query_as::<_, PgRelationReferenceRow>(
            "select to_id as relation_id,
                    min(rank) as rank,
                    max(score) as score,
                    (array_agg(
                        inclusion_reason
                        order by rank asc nulls last,
                                 created_at asc nulls last,
                                 from_id asc,
                                 to_id asc,
                                 relation_type asc
                    ))[1] as inclusion_reason
             from knowledge_evidence_relation_support
             where from_id = any($1::uuid[])
             group by to_id
             order by min(rank) asc nulls last,
                      max(score) desc nulls last,
                      to_id asc
             limit $2",
        )
        .bind(&evidence_ids)
        .bind(limit.max(1) as i64)
        .fetch_all(&state.persistence.postgres)
        .await
        .map_err(|error| ApiError::internal_with_log(error, "internal"))?;

        rows.into_iter().map(PgRelationReferenceRow::into_mcp).collect()
    };

    Ok(McpRevisionGroundingReferences {
        technical_fact_references,
        entity_references,
        relation_references,
        evidence_references,
    })
}

#[derive(Debug, sqlx::FromRow)]
struct PgEntityReferenceRow {
    entity_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
}

impl PgEntityReferenceRow {
    fn into_mcp(self) -> McpEntityReference {
        McpEntityReference {
            entity_id: self.entity_id,
            rank: self.rank.unwrap_or(i32::MAX),
            score: self.score.unwrap_or(0.0),
            inclusion_reason: self.inclusion_reason,
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct PgRelationReferenceRow {
    relation_id: Uuid,
    rank: Option<i32>,
    score: Option<f64>,
    inclusion_reason: Option<String>,
}

impl PgRelationReferenceRow {
    fn into_mcp(self) -> McpRelationReference {
        McpRelationReference {
            relation_id: self.relation_id,
            rank: self.rank.unwrap_or(i32::MAX),
            score: self.score.unwrap_or(0.0),
            inclusion_reason: self.inclusion_reason,
        }
    }
}

pub(crate) fn readable_status_reason(
    readiness_summary: &crate::domains::content::DocumentReadinessSummary,
    grounding: &McpRevisionGroundingReferences,
) -> Option<String> {
    if readiness_summary.readiness_kind == DocumentReadiness::Readable {
        return Some(
            "document text is readable, but structured preparation and graph extraction are still processing"
                .to_string(),
        );
    }
    if readiness_summary.graph_coverage_kind == "graph_sparse"
        && grounding.technical_fact_references.is_empty()
        && grounding.entity_references.is_empty()
        && grounding.relation_references.is_empty()
        && grounding.evidence_references.is_empty()
    {
        return Some(
            "document text is readable, but graph coverage is still sparse for this revision"
                .to_string(),
        );
    }
    (grounding.technical_fact_references.is_empty()
        && grounding.entity_references.is_empty()
        && grounding.relation_references.is_empty()
        && grounding.evidence_references.is_empty())
        .then_some(
            "document text is readable, but normalized technical facts and graph evidence are not available yet"
                .to_string(),
        )
}

pub(crate) const fn readability_state_from_kind(
    readiness_kind: DocumentReadiness,
) -> McpReadabilityState {
    match readiness_kind {
        DocumentReadiness::Failed => McpReadabilityState::Failed,
        DocumentReadiness::Processing => McpReadabilityState::Processing,
        DocumentReadiness::Empty => McpReadabilityState::Unavailable,
        DocumentReadiness::Readable
        | DocumentReadiness::GraphSparse
        | DocumentReadiness::GraphReady => McpReadabilityState::Readable,
    }
}

fn fact_supports_requested_chunks(support_chunk_ids: &[Uuid], chunk_ids: &[Uuid]) -> bool {
    !support_chunk_ids.is_empty()
        && support_chunk_ids.iter().any(|support_chunk_id| chunk_ids.contains(support_chunk_id))
}

fn technical_fact_support_score(
    fact: &crate::infra::knowledge_rows::KnowledgeTechnicalFactRow,
    chunk_ids: &[Uuid],
) -> (bool, usize, usize) {
    (
        fact_supports_requested_chunks(&fact.support_chunk_ids, chunk_ids),
        fact.support_chunk_ids.len(),
        fact.support_block_ids.len(),
    )
}
