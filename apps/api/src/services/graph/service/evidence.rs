use anyhow::{Context, Result};
use chrono::Utc;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::knowledge::{GraphEvidenceLiteralSpan, GraphEvidenceRecord, TypedTechnicalFact},
    infra::arangodb::graph_store::{
        KnowledgeEntityCandidateRow, KnowledgeEntityRow, KnowledgeEvidenceRow,
        KnowledgeRelationCandidateRow, KnowledgeRelationRow, NewKnowledgeEntityCandidate,
        NewKnowledgeEvidence, NewKnowledgeRelationCandidate,
    },
};

use super::{
    ArangoRevisionContext, GraphService, ReconciledRelationCandidate, canonical_evidence_id,
};

#[derive(Debug, Clone, Default)]
pub(super) struct ResolvedGraphEvidenceSupport {
    pub(super) block_id: Option<Uuid>,
    pub(super) fact_id: Option<Uuid>,
    pub(super) literal_spans: Vec<GraphEvidenceLiteralSpan>,
    pub(super) evidence_kind: String,
}

pub(super) trait ArangoEntityEvidenceCandidate {
    fn chunk_id(&self) -> Option<Uuid>;
    fn candidate_label(&self) -> &str;
    fn extraction_method(&self) -> &str;
    fn confidence(&self) -> Option<f64>;
}

impl ArangoEntityEvidenceCandidate for KnowledgeEntityCandidateRow {
    fn chunk_id(&self) -> Option<Uuid> {
        self.chunk_id
    }

    fn candidate_label(&self) -> &str {
        &self.candidate_label
    }

    fn extraction_method(&self) -> &str {
        &self.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

impl ArangoEntityEvidenceCandidate for NewKnowledgeEntityCandidate {
    fn chunk_id(&self) -> Option<Uuid> {
        self.chunk_id
    }

    fn candidate_label(&self) -> &str {
        &self.candidate_label
    }

    fn extraction_method(&self) -> &str {
        &self.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

pub(super) trait ArangoRelationEvidenceCandidate {
    fn chunk_id(&self) -> Option<Uuid>;
    fn subject_candidate_key(&self) -> &str;
    fn predicate(&self) -> &str;
    fn object_candidate_key(&self) -> &str;
    fn normalized_assertion(&self) -> &str;
    fn extraction_method(&self) -> &str;
    fn confidence(&self) -> Option<f64>;
}

impl ArangoRelationEvidenceCandidate for KnowledgeRelationCandidateRow {
    fn chunk_id(&self) -> Option<Uuid> {
        self.chunk_id
    }

    fn subject_candidate_key(&self) -> &str {
        &self.subject_candidate_key
    }

    fn predicate(&self) -> &str {
        &self.predicate
    }

    fn object_candidate_key(&self) -> &str {
        &self.object_candidate_key
    }

    fn normalized_assertion(&self) -> &str {
        &self.normalized_assertion
    }

    fn extraction_method(&self) -> &str {
        &self.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

impl ArangoRelationEvidenceCandidate for NewKnowledgeRelationCandidate {
    fn chunk_id(&self) -> Option<Uuid> {
        self.chunk_id
    }

    fn subject_candidate_key(&self) -> &str {
        &self.subject_candidate_key
    }

    fn predicate(&self) -> &str {
        &self.predicate
    }

    fn object_candidate_key(&self) -> &str {
        &self.object_candidate_key
    }

    fn normalized_assertion(&self) -> &str {
        &self.normalized_assertion
    }

    fn extraction_method(&self) -> &str {
        &self.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.confidence
    }
}

impl ArangoRelationEvidenceCandidate for ReconciledRelationCandidate {
    fn chunk_id(&self) -> Option<Uuid> {
        self.row.chunk_id
    }

    fn subject_candidate_key(&self) -> &str {
        &self.subject_candidate_key
    }

    fn predicate(&self) -> &str {
        &self.predicate
    }

    fn object_candidate_key(&self) -> &str {
        &self.object_candidate_key
    }

    fn normalized_assertion(&self) -> &str {
        &self.normalized_assertion
    }

    fn extraction_method(&self) -> &str {
        &self.row.extraction_method
    }

    fn confidence(&self) -> Option<f64> {
        self.row.confidence
    }
}

impl GraphService {
    pub(super) async fn upsert_current_entity_evidence<C>(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
        candidate: &C,
        entity: &KnowledgeEntityRow,
        canonical_key: &str,
        supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
        revision_facts: &[TypedTechnicalFact],
    ) -> Result<KnowledgeEvidenceRow>
    where
        C: ArangoEntityEvidenceCandidate,
    {
        let evidence_id = canonical_evidence_id(
            revision.library_id,
            revision.revision_id,
            candidate.chunk_id(),
            "entity",
            canonical_key,
        );
        let excerpt = candidate.candidate_label().to_string();
        let support = resolve_entity_evidence_support(
            candidate.candidate_label(),
            excerpt.as_str(),
            supporting_chunk,
            revision_facts,
        );
        let evidence = GraphEvidenceRecord {
            evidence_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: candidate.chunk_id(),
            block_id: support.block_id,
            fact_id: support.fact_id,
            quote_text: excerpt,
            literal_spans: support.literal_spans,
            confidence: candidate.confidence(),
            evidence_kind: support.evidence_kind,
            created_at: Utc::now(),
        };
        let row = state
            .arango_graph_store
            .upsert_evidence_with_edges(
                &graph_evidence_record_to_new_evidence(
                    revision.workspace_id,
                    revision.document_id,
                    revision.revision_number,
                    candidate.extraction_method(),
                    &evidence,
                ),
                Some(revision.revision_id),
                Some(entity.entity_id),
                None,
                evidence.fact_id,
            )
            .await
            .context("failed to upsert arango entity evidence")?;
        if let Some(chunk_id) = candidate.chunk_id() {
            self.upsert_chunk_mentions_entity_edge(
                state,
                chunk_id,
                entity.entity_id,
                candidate.confidence(),
            )
            .await?;
        }
        Ok(row)
    }

    pub(super) async fn upsert_current_relation_evidence<C>(
        &self,
        state: &AppState,
        revision: &ArangoRevisionContext,
        candidate: &C,
        relation: &KnowledgeRelationRow,
        supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
        revision_facts: &[TypedTechnicalFact],
    ) -> Result<KnowledgeEvidenceRow>
    where
        C: ArangoRelationEvidenceCandidate,
    {
        let evidence_id = canonical_evidence_id(
            revision.library_id,
            revision.revision_id,
            candidate.chunk_id(),
            "relation",
            candidate.normalized_assertion(),
        );
        let excerpt = format!(
            "{} {} {}",
            candidate.subject_candidate_key(),
            candidate.predicate(),
            candidate.object_candidate_key()
        );
        let support = resolve_relation_evidence_support(
            candidate.subject_candidate_key(),
            candidate.predicate(),
            candidate.object_candidate_key(),
            excerpt.as_str(),
            supporting_chunk,
            revision_facts,
        );
        let evidence = GraphEvidenceRecord {
            evidence_id,
            library_id: revision.library_id,
            revision_id: revision.revision_id,
            chunk_id: candidate.chunk_id(),
            block_id: support.block_id,
            fact_id: support.fact_id,
            quote_text: excerpt,
            literal_spans: support.literal_spans,
            confidence: candidate.confidence(),
            evidence_kind: support.evidence_kind,
            created_at: Utc::now(),
        };
        let row = state
            .arango_graph_store
            .upsert_evidence_with_edges(
                &graph_evidence_record_to_new_evidence(
                    revision.workspace_id,
                    revision.document_id,
                    revision.revision_number,
                    candidate.extraction_method(),
                    &evidence,
                ),
                Some(revision.revision_id),
                None,
                Some(relation.relation_id),
                evidence.fact_id,
            )
            .await
            .context("failed to upsert arango relation evidence")?;
        let subject = self
            .upsert_placeholder_entity_for_key(
                state,
                revision.library_id,
                revision.workspace_id,
                candidate.subject_candidate_key(),
            )
            .await?;
        let object = self
            .upsert_placeholder_entity_for_key(
                state,
                revision.library_id,
                revision.workspace_id,
                candidate.object_candidate_key(),
            )
            .await?;
        self.upsert_relation_edges(state, relation, &subject, &object).await?;
        Ok(row)
    }
}

fn graph_evidence_record_to_new_evidence(
    workspace_id: Uuid,
    document_id: Uuid,
    freshness_generation: i64,
    extraction_method: &str,
    record: &GraphEvidenceRecord,
) -> NewKnowledgeEvidence {
    let span_start = record.literal_spans.iter().map(|span| span.start_offset).min();
    let span_end = record.literal_spans.iter().map(|span| span.end_offset).max();
    NewKnowledgeEvidence {
        evidence_id: record.evidence_id,
        workspace_id,
        library_id: record.library_id,
        document_id,
        revision_id: record.revision_id,
        chunk_id: record.chunk_id,
        block_id: record.block_id,
        fact_id: record.fact_id,
        span_start,
        span_end,
        quote_text: record.quote_text.clone(),
        literal_spans_json: serde_json::to_value(&record.literal_spans)
            .unwrap_or_else(|_| serde_json::json!([])),
        evidence_kind: record.evidence_kind.clone(),
        extraction_method: extraction_method.to_string(),
        confidence: record.confidence,
        evidence_state: "active".to_string(),
        freshness_generation,
        created_at: Some(record.created_at),
        updated_at: Some(Utc::now()),
    }
}

pub(super) fn resolve_entity_evidence_support(
    candidate_label: &str,
    quote_text: &str,
    supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
    revision_facts: &[TypedTechnicalFact],
) -> ResolvedGraphEvidenceSupport {
    let fact = revision_facts
        .iter()
        .filter(|fact| fact_supports_chunk(fact, supporting_chunk))
        .find(|fact| technical_fact_matches_literals(fact, &[candidate_label]));
    let block_id = fact
        .and_then(|fact| fact.support_block_ids.first().copied())
        .or_else(|| supporting_chunk.and_then(|chunk| chunk.support_block_ids.first().copied()));
    ResolvedGraphEvidenceSupport {
        block_id,
        fact_id: fact.map(|fact| fact.fact_id),
        literal_spans: literal_spans_for_quote(quote_text, &[candidate_label]),
        evidence_kind: if fact.is_some() {
            "entity_fact_support".to_string()
        } else if block_id.is_some() {
            "entity_block_support".to_string()
        } else {
            "entity_candidate".to_string()
        },
    }
}

fn resolve_relation_evidence_support(
    subject: &str,
    predicate: &str,
    object: &str,
    quote_text: &str,
    supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
    revision_facts: &[TypedTechnicalFact],
) -> ResolvedGraphEvidenceSupport {
    let fact = revision_facts
        .iter()
        .filter(|fact| fact_supports_chunk(fact, supporting_chunk))
        .find(|fact| technical_fact_matches_relation(fact, subject, predicate, object));
    let block_id = fact
        .and_then(|fact| fact.support_block_ids.first().copied())
        .or_else(|| supporting_chunk.and_then(|chunk| chunk.support_block_ids.first().copied()));
    ResolvedGraphEvidenceSupport {
        block_id,
        fact_id: fact.map(|fact| fact.fact_id),
        literal_spans: literal_spans_for_quote(quote_text, &[subject, predicate, object]),
        evidence_kind: if fact.is_some() {
            "relation_fact_support".to_string()
        } else if block_id.is_some() {
            "relation_block_support".to_string()
        } else {
            "relation_candidate".to_string()
        },
    }
}

fn fact_supports_chunk(
    fact: &TypedTechnicalFact,
    supporting_chunk: Option<&crate::infra::arangodb::document_store::KnowledgeChunkRow>,
) -> bool {
    let Some(chunk) = supporting_chunk else {
        return true;
    };
    fact.support_chunk_ids.contains(&chunk.chunk_id)
        || fact.support_block_ids.iter().any(|block_id| chunk.support_block_ids.contains(block_id))
}

fn technical_fact_matches_literals(fact: &TypedTechnicalFact, literals: &[&str]) -> bool {
    let haystack = technical_fact_match_haystack(fact);
    literals
        .iter()
        .map(|literal| normalize_evidence_literal(literal))
        .filter(|literal| !literal.is_empty())
        .all(|literal| haystack.contains(&literal))
}

fn technical_fact_matches_relation(
    fact: &TypedTechnicalFact,
    subject: &str,
    predicate: &str,
    object: &str,
) -> bool {
    let haystack = technical_fact_match_haystack(fact);
    let needles = [subject, predicate, object]
        .into_iter()
        .map(normalize_evidence_literal)
        .filter(|literal| !literal.is_empty())
        .collect::<Vec<_>>();
    if needles.is_empty() {
        return false;
    }
    let matched = needles.iter().filter(|literal| haystack.contains(literal.as_str())).count();
    matched >= needles.len().min(2)
}

fn technical_fact_match_haystack(fact: &TypedTechnicalFact) -> String {
    let mut parts = Vec::<String>::new();
    parts.push(normalize_evidence_literal(&fact.display_value));
    parts.push(normalize_evidence_literal(&fact.canonical_value.canonical_string()));
    for qualifier in &fact.qualifiers {
        parts.push(normalize_evidence_literal(&qualifier.key));
        parts.push(normalize_evidence_literal(&qualifier.value));
    }
    parts.retain(|part| !part.is_empty());
    parts.join(" ")
}

fn literal_spans_for_quote(quote_text: &str, literals: &[&str]) -> Vec<GraphEvidenceLiteralSpan> {
    let mut spans = Vec::<GraphEvidenceLiteralSpan>::new();
    for literal in literals {
        let trimmed = literal.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(start_offset) = quote_text.find(trimmed) {
            let end_offset = start_offset.saturating_add(trimmed.len());
            spans.push(GraphEvidenceLiteralSpan {
                start_offset: i32::try_from(start_offset).unwrap_or(i32::MAX),
                end_offset: i32::try_from(end_offset).unwrap_or(i32::MAX),
                literal: trimmed.to_string(),
            });
        }
    }
    spans
}

pub(super) fn relation_fields_are_semantically_empty(
    subject: &str,
    predicate: &str,
    object: &str,
) -> bool {
    [
        normalize_evidence_literal(subject),
        normalize_evidence_literal(predicate),
        normalize_evidence_literal(object),
    ]
    .into_iter()
    .any(|value| value.is_empty())
}

pub(super) fn normalize_evidence_literal(value: &str) -> String {
    value
        .trim()
        .chars()
        .filter(|ch| !ch.is_whitespace() && !matches!(ch, '"' | '\'' | '`'))
        .flat_map(char::to_lowercase)
        .collect()
}
