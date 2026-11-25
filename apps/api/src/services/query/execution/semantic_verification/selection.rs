//! One bounded evidence selection shared by generation and verification.
//!
//! Generation and the semantic verifier must judge the same sources. The
//! retrieval/assembly owners admit sources through [`EvidenceBudget`] so the
//! generator prompt and the verifier consume the same bounded identities and
//! byte-bounded text. The verifier re-measures the selected set; a set that
//! still exceeds a bound is a typed refusal, never a silent truncation.

use std::collections::{BTreeSet, HashMap, HashSet};

use uuid::Uuid;

use crate::domains::query::QueryVerificationWarning;
use crate::domains::query_ir::QueryIR;
use crate::infra::knowledge_rows::{
    KnowledgeChunkRow, KnowledgeDocumentRow, KnowledgeStructuredBlockRow,
};
use crate::services::query::assistant_grounding::{
    AdmittedAgentSource, AssistantGroundingDocumentReference, AssistantGroundingEvidence,
};

use super::super::chunk_support::{bounded_evidence_text, chunk_answer_source_text};
use super::super::types::{AuthoritativeAnswerEvidence, RuntimeMatchedChunk};
use super::{
    MAX_EVIDENCE_UTF8_BYTES, MAX_SOURCE_ENTRIES, SemanticPreparationBound,
    SemanticPreparationError, preparation_limit,
};

/// Counts and bytes of the evidence a generation step selected.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SelectedEvidenceMeasurement {
    pub(crate) source_entries: usize,
    pub(crate) evidence_utf8_bytes: usize,
}

/// Canonical selection: distinct chunk and segment identities plus their
/// measurement. The verifier loads exactly these identities.
#[derive(Debug, Clone)]
pub(crate) struct SelectedEvidence {
    pub(crate) chunk_ids: BTreeSet<Uuid>,
    pub(crate) block_ids: BTreeSet<Uuid>,
    pub(crate) measurement: SelectedEvidenceMeasurement,
}

/// One admission decision. Duplicates must not be charged twice, and a source
/// that does not fit is skipped without consuming budget for a later one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EvidenceAdmission {
    Retained,
    Duplicate,
    OverBudget,
}

/// Running distinct-set budget used at the context/evidence assembly owner.
#[derive(Debug, Clone, Default)]
pub(crate) struct EvidenceBudget {
    chunk_ids: HashSet<Uuid>,
    block_ids: HashSet<Uuid>,
    document_reference_ids: HashSet<(Uuid, Uuid, usize, usize)>,
    source_entries: usize,
    evidence_utf8_bytes: usize,
}

impl EvidenceBudget {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Rebuilds the budget from chunks already admitted by an earlier owner.
    pub(crate) fn from_chunks<'a>(
        chunks: impl IntoIterator<Item = &'a RuntimeMatchedChunk>,
    ) -> Self {
        let mut budget = Self::new();
        for chunk in chunks {
            budget.admit_chunk_unchecked(chunk.chunk_id, chunk_evidence_bytes(chunk));
        }
        budget
    }

    /// Rebuilds the budget from an already admitted typed agent manifest.
    pub(crate) fn from_agent_sources(sources: &[AdmittedAgentSource]) -> Self {
        let mut budget = Self::new();
        for source in sources {
            budget
                .admit_chunk_unchecked(source.chunk_id, agent_source_evidence_bytes(&source.text));
        }
        budget
    }

    pub(crate) fn measurement(&self) -> SelectedEvidenceMeasurement {
        SelectedEvidenceMeasurement {
            source_entries: self.source_entries,
            evidence_utf8_bytes: self.evidence_utf8_bytes,
        }
    }

    pub(crate) fn try_admit_chunk(
        &mut self,
        chunk_id: Uuid,
        text_bytes: usize,
    ) -> EvidenceAdmission {
        if self.chunk_ids.contains(&chunk_id) {
            return EvidenceAdmission::Duplicate;
        }
        if !self.fits(text_bytes) {
            return EvidenceAdmission::OverBudget;
        }
        self.admit_chunk_unchecked(chunk_id, text_bytes);
        EvidenceAdmission::Retained
    }

    pub(crate) fn try_admit_block(
        &mut self,
        block_id: Uuid,
        text_bytes: usize,
    ) -> EvidenceAdmission {
        if self.block_ids.contains(&block_id) {
            return EvidenceAdmission::Duplicate;
        }
        if !self.fits(text_bytes) {
            return EvidenceAdmission::OverBudget;
        }
        self.admit_block_unchecked(block_id, text_bytes);
        EvidenceAdmission::Retained
    }

    pub(crate) fn try_admit_document_reference(
        &mut self,
        reference: &AssistantGroundingDocumentReference,
    ) -> EvidenceAdmission {
        let revision_id = reference.revision_id.unwrap_or_else(Uuid::nil);
        let key = (
            reference.document_id,
            revision_id,
            reference.slice_start_offset,
            reference.slice_end_offset,
        );
        if self.document_reference_ids.contains(&key) {
            return EvidenceAdmission::Duplicate;
        }
        let text_bytes = document_reference_evidence_bytes(reference);
        if !self.fits(text_bytes) {
            return EvidenceAdmission::OverBudget;
        }
        self.admit_document_reference_unchecked(
            reference.document_id,
            revision_id,
            reference.slice_start_offset,
            reference.slice_end_offset,
            text_bytes,
        );
        EvidenceAdmission::Retained
    }

    fn admit_chunk_unchecked(&mut self, chunk_id: Uuid, text_bytes: usize) {
        if self.chunk_ids.insert(chunk_id) {
            self.charge(text_bytes);
        }
    }

    fn admit_block_unchecked(&mut self, block_id: Uuid, text_bytes: usize) {
        if self.block_ids.insert(block_id) {
            self.charge(text_bytes);
        }
    }

    fn admit_document_reference_unchecked(
        &mut self,
        document_id: Uuid,
        revision_id: Uuid,
        start: usize,
        end: usize,
        text_bytes: usize,
    ) {
        if self.document_reference_ids.insert((document_id, revision_id, start, end)) {
            self.charge(text_bytes);
        }
    }

    fn fits(&self, text_bytes: usize) -> bool {
        self.source_entries < MAX_SOURCE_ENTRIES
            && self.evidence_utf8_bytes.saturating_add(text_bytes) <= MAX_EVIDENCE_UTF8_BYTES
    }

    fn charge(&mut self, text_bytes: usize) {
        self.source_entries = self.source_entries.saturating_add(1);
        self.evidence_utf8_bytes = self.evidence_utf8_bytes.saturating_add(text_bytes);
    }
}

/// What a retention pass dropped, so callers can tell duplicates from real
/// coverage loss and never silently claim full coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct BoundedEvidenceRetention {
    pub(crate) duplicates: usize,
    pub(crate) over_budget: usize,
    pub(crate) measurement: SelectedEvidenceMeasurement,
}

impl BoundedEvidenceRetention {
    pub(crate) fn is_truncated(self) -> bool {
        self.over_budget > 0
    }
}

/// Overview anchors compete for the same bytes and entries as ranked chunks.
/// Admission priority changes membership under pressure, never the returned rank order.
pub(crate) fn retain_bounded_chunks(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    budget: &mut EvidenceBudget,
    query_ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BoundedEvidenceRetention {
    let mut reserved = HashSet::new();
    for rank in super::super::source_context::overview_evidence_anchor_ranks(
        query_ir,
        chunks,
        document_index,
    ) {
        let chunk = &chunks[rank];
        if budget.try_admit_chunk(chunk.chunk_id, chunk_evidence_bytes(chunk))
            == EvidenceAdmission::Retained
        {
            reserved.insert(rank);
        }
    }
    let mut retention = BoundedEvidenceRetention::default();
    let mut kept = Vec::with_capacity(chunks.len().min(MAX_SOURCE_ENTRIES));
    for (rank, chunk) in chunks.drain(..).enumerate() {
        if reserved.contains(&rank) {
            kept.push(chunk);
            continue;
        }
        match budget.try_admit_chunk(chunk.chunk_id, chunk_evidence_bytes(&chunk)) {
            EvidenceAdmission::Retained => kept.push(chunk),
            EvidenceAdmission::Duplicate => retention.duplicates += 1,
            EvidenceAdmission::OverBudget => retention.over_budget += 1,
        }
    }
    *chunks = kept;
    retention.measurement = budget.measurement();
    retention
}

/// Keeps the highest-ranked distinct structured blocks that fit `budget`.
pub(crate) fn retain_bounded_blocks(
    blocks: &mut Vec<KnowledgeStructuredBlockRow>,
    budget: &mut EvidenceBudget,
) -> BoundedEvidenceRetention {
    let mut retention = BoundedEvidenceRetention::default();
    let mut kept = Vec::with_capacity(blocks.len().min(MAX_SOURCE_ENTRIES));
    for block in blocks.drain(..) {
        match budget.try_admit_block(block.block_id, block_evidence_bytes(&block)) {
            EvidenceAdmission::Retained => kept.push(block),
            EvidenceAdmission::Duplicate => retention.duplicates += 1,
            EvidenceAdmission::OverBudget => retention.over_budget += 1,
        }
    }
    *blocks = kept;
    retention.measurement = budget.measurement();
    retention
}

pub(crate) fn document_reference_evidence_bytes(
    reference: &AssistantGroundingDocumentReference,
) -> usize {
    if reference.excerpt.trim().is_empty() { 0 } else { reference.excerpt.len() }
}

pub(crate) fn chunk_evidence_bytes(chunk: &RuntimeMatchedChunk) -> usize {
    bounded_evidence_text(&chunk.source_text).len()
}

pub(crate) fn agent_source_evidence_bytes(text: &str) -> usize {
    bounded_evidence_text(text).len()
}

/// Canonical typed source for one selected chunk row. The text is the same
/// byte-bounded mapping the verifier uses; it is never rendered prose.
pub(crate) fn admitted_source_from_chunk_row(row: &KnowledgeChunkRow) -> AdmittedAgentSource {
    AdmittedAgentSource {
        chunk_id: row.chunk_id,
        document_id: row.document_id,
        document_revision_id: row.revision_id,
        text: chunk_answer_source_text(row),
    }
}

pub(crate) fn block_evidence_bytes(block: &KnowledgeStructuredBlockRow) -> usize {
    bounded_evidence_text(&block.text).len()
}

/// Coverage caveat appended to the generator prompt when the bounded selection
/// dropped authorized sources. It prevents a silent full-coverage claim.
pub(crate) fn bounded_evidence_notice(measurement: SelectedEvidenceMeasurement) -> String {
    format!(
        "EVIDENCE_COVERAGE status=partial recency=uncertified bounded_source_entries={} bounded_utf8_bytes={}",
        measurement.source_entries, measurement.evidence_utf8_bytes
    )
}

pub(crate) const EVIDENCE_SELECTION_BOUNDED: &str = "evidence_selection_bounded";

/// Observable, nonblocking warning for the final verification owner when the
/// bounded selection dropped authorized sources. It never changes support.
pub(crate) fn bounded_selection_warning(
    measurement: SelectedEvidenceMeasurement,
) -> QueryVerificationWarning {
    QueryVerificationWarning {
        code: EVIDENCE_SELECTION_BOUNDED.to_string(),
        message: format!(
            "Source coverage is partial ({} entries, {} UTF-8 bytes selected); the answer must not claim complete corpus coverage.",
            measurement.source_entries, measurement.evidence_utf8_bytes
        ),
        related_segment_id: None,
        related_fact_id: None,
    }
}

/// Measures the exact generation evidence without reading anything new.
///
/// Duplicate identities are charged once. The text of every chunk is produced
/// by the same byte-bounded mapping the generator and verifier use. Structured
/// blocks are bounded to the same per-source unit. Reaching the source-entry or
/// aggregate byte bound is a typed refusal, never a silent truncation.
pub(crate) fn measure_selected_evidence(
    chunks: &[RuntimeMatchedChunk],
    evidence: &AuthoritativeAnswerEvidence,
    grounding: &AssistantGroundingEvidence,
) -> Result<SelectedEvidence, SemanticPreparationError> {
    let mut chunk_ids = BTreeSet::new();
    let mut block_ids = BTreeSet::new();
    let mut budget = EvidenceBudget::new();
    let rows = evidence.chunk_rows.iter().map(|row| (row.chunk_id, row)).collect::<HashMap<_, _>>();
    for chunk in chunks {
        if !chunk_ids.insert(chunk.chunk_id) {
            continue;
        }
        let bytes = match rows.get(&chunk.chunk_id) {
            Some(row) => chunk_answer_source_text(row).len(),
            None => chunk_evidence_bytes(chunk),
        };
        budget.admit_chunk_unchecked(chunk.chunk_id, bytes);
    }
    for block in &evidence.structured_blocks {
        if !block_ids.insert(block.block_id) {
            continue;
        }
        budget.admit_block_unchecked(block.block_id, block_evidence_bytes(block));
    }
    for reference in &grounding.document_references {
        let revision_id = reference.revision_id.unwrap_or_else(Uuid::nil);
        let text_bytes =
            if reference.excerpt.trim().is_empty() { 0 } else { reference.excerpt.len() };
        budget.admit_document_reference_unchecked(
            reference.document_id,
            revision_id,
            reference.slice_start_offset,
            reference.slice_end_offset,
            text_bytes,
        );
    }

    let measurement = budget.measurement();
    if measurement.source_entries > MAX_SOURCE_ENTRIES {
        return Err(SemanticPreparationError::TooManySources(preparation_limit(
            SemanticPreparationBound::SourceEntries,
            measurement.source_entries,
        )));
    }
    if measurement.evidence_utf8_bytes > MAX_EVIDENCE_UTF8_BYTES {
        return Err(SemanticPreparationError::EvidenceTooLarge(preparation_limit(
            SemanticPreparationBound::EvidenceUtf8Bytes,
            measurement.evidence_utf8_bytes,
        )));
    }

    Ok(SelectedEvidence { chunk_ids, block_ids, measurement })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::{QueryAct, QueryScope};
    use crate::services::query::execution::chunk_support::MAX_EVIDENCE_SOURCE_UTF8_BYTES;
    use crate::services::query::execution::types::RuntimeChunkScoreKind;

    fn chunk(rank: u128, text: &str) -> RuntimeMatchedChunk {
        RuntimeMatchedChunk {
            chunk_id: Uuid::from_u128(rank),
            document_id: Uuid::from_u128(10),
            revision_id: Uuid::from_u128(11),
            chunk_index: rank as i32,
            chunk_kind: Some("paragraph".to_string()),
            document_label: "doc".to_string(),
            excerpt: text.to_string(),
            score_kind: RuntimeChunkScoreKind::Relevance,
            score: Some(1.0),
            source_text: text.to_string(),
        }
    }

    fn empty_evidence() -> AuthoritativeAnswerEvidence {
        AuthoritativeAnswerEvidence {
            bundle: None,
            chunk_rows: Vec::new(),
            structured_blocks: Vec::new(),
            technical_facts: Vec::new(),
        }
    }

    fn block(rank: u128, text: &str) -> KnowledgeStructuredBlockRow {
        let now = chrono::Utc::now();
        KnowledgeStructuredBlockRow {
            block_id: Uuid::from_u128(rank),
            workspace_id: Uuid::from_u128(20),
            library_id: Uuid::from_u128(21),
            document_id: Uuid::from_u128(22),
            revision_id: Uuid::from_u128(23),
            ordinal: rank as i32,
            block_kind: "table_row".to_string(),
            text: text.to_string(),
            normalized_text: text.to_string(),
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn multiple_sources_fit_and_report_exact_measurement() {
        let chunks =
            vec![chunk(1, "Alpha source."), chunk(2, "Bravo source."), chunk(3, "Charlie.")];
        let selected = measure_selected_evidence(
            &chunks,
            &empty_evidence(),
            &AssistantGroundingEvidence::default(),
        )
        .expect("three small sources fit");
        assert_eq!(selected.measurement.source_entries, 3);
        let expected = "Alpha source.".len() + "Bravo source.".len() + "Charlie.".len();
        assert_eq!(selected.measurement.evidence_utf8_bytes, expected);
        assert_eq!(selected.chunk_ids.len(), 3);
    }

    #[test]
    fn admitted_manifest_and_verifier_measure_the_same_identity_set() {
        let sources = vec![
            AdmittedAgentSource {
                chunk_id: Uuid::from_u128(1),
                document_id: Uuid::from_u128(10),
                document_revision_id: Uuid::from_u128(11),
                text: "alpha".to_string(),
            },
            AdmittedAgentSource {
                chunk_id: Uuid::from_u128(2),
                document_id: Uuid::from_u128(10),
                document_revision_id: Uuid::from_u128(11),
                text: "beta".to_string(),
            },
        ];
        let chunks = vec![chunk(1, "alpha"), chunk(2, "beta")];
        let manifest_budget = EvidenceBudget::from_agent_sources(&sources);
        let selected = measure_selected_evidence(
            &chunks,
            &empty_evidence(),
            &AssistantGroundingEvidence::default(),
        )
        .expect("admitted manifest fits");
        assert_eq!(
            selected.chunk_ids,
            sources.iter().map(|source| source.chunk_id).collect::<BTreeSet<_>>()
        );
        assert_eq!(selected.measurement, manifest_budget.measurement());
    }

    #[test]
    fn duplicate_chunk_identity_is_charged_once() {
        let chunks = vec![chunk(1, "Repeated source."), chunk(1, "Repeated source.")];
        let selected = measure_selected_evidence(
            &chunks,
            &empty_evidence(),
            &AssistantGroundingEvidence::default(),
        )
        .expect("duplicate identity fits");
        assert_eq!(selected.measurement.source_entries, 1);
        assert_eq!(selected.measurement.evidence_utf8_bytes, "Repeated source.".len());
        assert_eq!(selected.chunk_ids.len(), 1);
    }

    #[test]
    fn duplicate_block_identity_is_charged_once() {
        let mut evidence = empty_evidence();
        evidence.structured_blocks =
            vec![block(1, "Repeated segment."), block(1, "Repeated segment.")];
        let selected =
            measure_selected_evidence(&[], &evidence, &AssistantGroundingEvidence::default())
                .expect("duplicate segment fits");
        assert_eq!(selected.measurement.source_entries, 1);
        assert_eq!(selected.measurement.evidence_utf8_bytes, "Repeated segment.".len());
        assert_eq!(selected.block_ids.len(), 1);
    }

    #[test]
    fn too_many_sources_are_a_typed_entry_refusal() {
        let chunks = (0..=MAX_SOURCE_ENTRIES as u128)
            .map(|i| chunk(i + 1, &format!("s{i}")))
            .collect::<Vec<_>>();
        let error = measure_selected_evidence(
            &chunks,
            &empty_evidence(),
            &AssistantGroundingEvidence::default(),
        )
        .expect_err("source entries over the cap must refuse");
        let measurement = error.limit_measurement().expect("typed limit");
        assert_eq!(measurement.bound, SemanticPreparationBound::SourceEntries);
        assert_eq!(measurement.actual, (MAX_SOURCE_ENTRIES + 1) as u64);
        assert_eq!(measurement.limit, MAX_SOURCE_ENTRIES as u64);
    }

    #[test]
    fn one_long_russian_source_is_bounded_below_the_aggregate() {
        let russian = "Привет, мир! ".repeat(2_000);
        assert!(russian.len() > MAX_EVIDENCE_UTF8_BYTES);
        let chunks = vec![chunk(1, &russian)];
        let selected = measure_selected_evidence(
            &chunks,
            &empty_evidence(),
            &AssistantGroundingEvidence::default(),
        )
        .expect("a single source is byte-bounded below the aggregate");
        assert_eq!(selected.measurement.source_entries, 1);
        assert!(selected.measurement.evidence_utf8_bytes <= MAX_EVIDENCE_SOURCE_UTF8_BYTES);
        assert!(selected.measurement.evidence_utf8_bytes <= MAX_EVIDENCE_UTF8_BYTES);
    }

    #[test]
    fn multiple_bounded_sources_still_refuse_when_the_aggregate_is_exceeded() {
        let text = "я".repeat(MAX_EVIDENCE_SOURCE_UTF8_BYTES / 2 - 1);
        let chunks = vec![chunk(1, &text), chunk(2, &text)];
        let error = measure_selected_evidence(
            &chunks,
            &empty_evidence(),
            &AssistantGroundingEvidence::default(),
        )
        .expect_err("two near-cap sources exceed the aggregate byte bound");
        let measurement = error.limit_measurement().expect("typed limit");
        assert_eq!(measurement.bound, SemanticPreparationBound::EvidenceUtf8Bytes);
        assert!(measurement.actual > measurement.limit);
    }

    #[test]
    fn retain_bounded_chunks_keeps_rank_order_and_charges_distinct_identity_once() {
        let mut chunks = vec![
            chunk(1, "rank-1"),
            chunk(1, "rank-1-duplicate"),
            chunk(2, "rank-2"),
            chunk(3, "rank-3"),
        ];
        let mut budget = EvidenceBudget::new();
        let ir = QueryIR::test_with_act_scope(QueryAct::RetrieveValue, QueryScope::SingleDocument);
        let retention = retain_bounded_chunks(&mut chunks, &mut budget, &ir, &HashMap::new());
        assert_eq!(
            chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3),]
        );
        assert_eq!(retention.duplicates, 1);
        assert!(!retention.is_truncated());
        assert_eq!(retention.measurement.source_entries, 3);
        assert_eq!(retention.measurement.evidence_utf8_bytes, 6 + 6 + 6);
    }

    #[test]
    fn retain_bounded_chunks_stops_before_the_aggregate_and_skips_only_non_fitting_sources() {
        let large = "a".repeat(MAX_EVIDENCE_SOURCE_UTF8_BYTES);
        let mut chunks = vec![
            chunk(1, &large),
            chunk(2, &"b".repeat(MAX_EVIDENCE_SOURCE_UTF8_BYTES)),
            chunk(3, "small"),
        ];
        let mut budget = EvidenceBudget::new();
        let ir = QueryIR::test_with_act_scope(QueryAct::RetrieveValue, QueryScope::SingleDocument);
        let retention = retain_bounded_chunks(&mut chunks, &mut budget, &ir, &HashMap::new());
        // First source fits the per-source cap; second would exceed the
        // aggregate and is skipped; the later small source still fits.
        assert_eq!(
            chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>(),
            vec![Uuid::from_u128(1), Uuid::from_u128(3),]
        );
        assert_eq!(retention.over_budget, 1);
        assert!(retention.is_truncated());
        assert!(retention.measurement.evidence_utf8_bytes <= MAX_EVIDENCE_UTF8_BYTES);
    }

    #[test]
    fn retain_bounded_blocks_measures_distinct_segments() {
        let mut blocks = vec![block(1, "segment-a"), block(1, "segment-a"), block(2, "segment-b")];
        let mut budget = EvidenceBudget::new();
        let retention = retain_bounded_blocks(&mut blocks, &mut budget);
        assert_eq!(blocks.len(), 2);
        assert_eq!(retention.duplicates, 1);
        assert_eq!(retention.measurement.source_entries, 2);
        assert_eq!(
            retention.measurement.evidence_utf8_bytes,
            "segment-a".len() + "segment-b".len()
        );
    }

    #[test]
    fn structured_segments_are_counted_and_byte_bounded() {
        let russian = "я".repeat(MAX_EVIDENCE_SOURCE_UTF8_BYTES);
        let mut evidence = empty_evidence();
        evidence.structured_blocks = vec![block(1, &russian)];
        let selected =
            measure_selected_evidence(&[], &evidence, &AssistantGroundingEvidence::default())
                .expect("one bounded segment fits");
        assert_eq!(selected.measurement.source_entries, 1);
        assert_eq!(selected.measurement.evidence_utf8_bytes, MAX_EVIDENCE_SOURCE_UTF8_BYTES);
        assert_eq!(selected.block_ids.len(), 1);
    }

    #[test]
    fn structured_segments_cannot_bypass_the_aggregate() {
        let text = "я".repeat(MAX_EVIDENCE_SOURCE_UTF8_BYTES);
        let mut evidence = empty_evidence();
        evidence.structured_blocks = vec![block(1, &text), block(2, &text)];
        let error =
            measure_selected_evidence(&[], &evidence, &AssistantGroundingEvidence::default())
                .expect_err("two near-cap segments exceed the aggregate byte bound");
        let measurement = error.limit_measurement().expect("typed limit");
        assert_eq!(measurement.bound, SemanticPreparationBound::EvidenceUtf8Bytes);
        assert_eq!(measurement.actual, (MAX_EVIDENCE_SOURCE_UTF8_BYTES * 2) as u64);
        assert_eq!(measurement.limit, MAX_EVIDENCE_UTF8_BYTES as u64);
    }
}
