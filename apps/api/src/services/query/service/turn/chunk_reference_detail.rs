use super::*;
use crate::domains::query::{
    PreparedSegmentReference, QueryChunkReference, QueryGraphEdgeReference,
    QueryGraphNodeReference, TechnicalFactReference, VISIBLE_REFERENCE_LIMIT,
};
use crate::mcp_types::MCP_COMPACT_MAX_REFERENCES;
use crate::services::query::reference_selection::{
    ReferenceSelectionCandidate, chunk_group_key, document_group_key, entity_group_key,
    relation_group_key, revision_group_key, segment_group_key, select_reference_indices,
};
use ironrag_contracts::assistant::semantic_verification::{
    SemanticClaimSupport, SemanticSourceLocator, SemanticVerificationOutcome,
    SemanticVerificationReport,
};
use std::collections::BTreeSet;

/// How much of a chunk's text the sources panel carries. Enough to recognise
/// the passage, short enough that a hundred references do not become a page of
/// prose in the response.
const EXCERPT_MAX_CHARS: usize = 320;

/// Maximum chunk ids bound into one metadata repository request. The persisted
/// reference count is not a request bound; this slice size is. Every batch is
/// still text-free, and all batches together cover the full candidate set so
/// document-diverse and claim-linked lower ranks remain selectable.
const CHUNK_METADATA_REQUEST_BATCH_SIZE: usize = 64;

fn excerpt_of(content_text: &str) -> Option<String> {
    let trimmed = content_text.split_whitespace().collect::<Vec<_>>().join(" ");
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= EXCERPT_MAX_CHARS {
        return Some(trimmed);
    }
    let cut = trimmed.char_indices().nth(EXCERPT_MAX_CHARS).map_or(trimmed.len(), |(at, _)| at);
    Some(format!("{}…", trimmed[..cut].trim_end()))
}

struct HydratedChunkBody {
    chunk_id: Uuid,
    document_id: Uuid,
    section_path: Vec<String>,
    heading_trail: Vec<String>,
    content_text: String,
}

/// The persisted reference material one execution detail carries besides its
/// chunk references, plus the semantic report that links claim units. Selection
/// and hydration read the same set.
#[derive(Clone, Copy)]
pub(crate) struct ReferenceSelectionInputs<'a> {
    pub(crate) prepared_segment_references: &'a [PreparedSegmentReference],
    pub(crate) technical_fact_references: &'a [TechnicalFactReference],
    pub(crate) entity_references: &'a [QueryGraphNodeReference],
    pub(crate) relation_references: &'a [QueryGraphEdgeReference],
    pub(crate) semantic_verification: Option<&'a SemanticVerificationReport>,
}

/// Fills in what a chunk reference needs to read as a source: its document,
/// revision, the headings above it, and the opening of its own text.
///
/// The stored reference is a chunk id, a rank and a score. Identity is read
/// first as a text-free projection for every persisted candidate; the actual
/// bounded UI and MCP mixed selections are then evaluated with the canonical
/// selector. Only chunk bodies selected by those real mixtures are read, so
/// claim order and non-chunk positions stay aligned with what a consumer sees.
///
/// Both repository reads require prepared rows, which proves materialization,
/// not publication. The execution-detail owner rechecks source currentness
/// after hydration and before exposing any references.
pub(crate) async fn enrich_chunk_references(
    state: &AppState,
    chunk_references: &mut [QueryChunkReference],
    inputs: &ReferenceSelectionInputs<'_>,
) {
    if chunk_references.is_empty() {
        return;
    }
    let chunk_ids = chunk_references
        .iter()
        .map(|reference| reference.chunk_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut metadata = Vec::with_capacity(chunk_ids.len());
    for batch in metadata_request_batches(&chunk_ids) {
        match state.document_store.list_chunk_reference_metadata_by_ids(batch).await {
            Ok(rows) => metadata.extend(rows),
            Err(_) => return,
        }
    }
    let metadata_by_chunk =
        metadata.iter().map(|row| (row.chunk_id, row)).collect::<HashMap<_, _>>();

    // Authoritative identity for every candidate, without chunk text. This is
    // what lets the projection group by document and surface a lower-ranked
    // source instead of relying on a rank prefix.
    for reference in chunk_references.iter_mut() {
        let Some(row) = metadata_by_chunk.get(&reference.chunk_id) else {
            continue;
        };
        reference.document_id = Some(row.document_id);
        reference.revision_id = Some(row.revision_id);
        reference.section_path = if row.heading_trail.is_empty() {
            row.section_path.clone()
        } else {
            row.heading_trail.clone()
        };
    }

    let document_ids = metadata
        .iter()
        .map(|row| row.document_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let titles = state
        .document_store
        .list_documents_by_ids(&document_ids)
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|document| (document.document_id, document.title))
        .collect::<HashMap<_, _>>();
    for reference in chunk_references.iter_mut() {
        if let Some(document_id) = reference.document_id {
            reference.document_title = titles.get(&document_id).cloned().flatten();
        }
    }

    let selected_chunk_ids = chunk_hydration_plan(chunk_references, inputs);
    if selected_chunk_ids.is_empty() {
        return;
    }
    let Ok(chunks) = state.document_store.list_chunks_by_ids(&selected_chunk_ids).await else {
        return;
    };
    let bodies = chunks
        .into_iter()
        .map(|chunk| HydratedChunkBody {
            chunk_id: chunk.chunk_id,
            document_id: chunk.document_id,
            section_path: chunk.section_path,
            heading_trail: chunk.heading_trail,
            content_text: chunk.content_text,
        })
        .collect::<Vec<_>>();
    apply_chunk_bodies(chunk_references, &bodies, &titles);
}

fn metadata_request_batches(chunk_ids: &[Uuid]) -> Vec<&[Uuid]> {
    chunk_ids.chunks(CHUNK_METADATA_REQUEST_BATCH_SIZE).collect()
}

fn apply_chunk_bodies(
    references: &mut [QueryChunkReference],
    bodies: &[HydratedChunkBody],
    titles: &HashMap<Uuid, Option<String>>,
) {
    let by_chunk = bodies.iter().map(|body| (body.chunk_id, body)).collect::<HashMap<_, _>>();
    for reference in references.iter_mut() {
        let Some(body) = by_chunk.get(&reference.chunk_id) else {
            continue;
        };
        reference.document_title = titles.get(&body.document_id).cloned().flatten();
        reference.section_path = if body.heading_trail.is_empty() {
            body.section_path.clone()
        } else {
            body.heading_trail.clone()
        };
        reference.excerpt = excerpt_of(&body.content_text);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum HydrationIdentity {
    Chunk(Uuid),
    Segment(Uuid),
}

/// Candidate list, per-candidate chunk id (absent for non-chunk kinds) and the
/// hydration identity index, in one pass over the reference set.
type SelectionCandidates =
    (Vec<ReferenceSelectionCandidate>, Vec<Option<Uuid>>, HashMap<HydrationIdentity, usize>);

fn selection_candidates(
    chunk_references: &[QueryChunkReference],
    inputs: &ReferenceSelectionInputs<'_>,
) -> SelectionCandidates {
    let ReferenceSelectionInputs {
        prepared_segment_references,
        technical_fact_references,
        entity_references,
        relation_references,
        ..
    } = *inputs;
    let mut candidates = Vec::new();
    let mut chunk_ids = Vec::new();
    let mut identities = HashMap::new();

    for reference in chunk_references {
        let index = candidates.len();
        identities.insert(HydrationIdentity::Chunk(reference.chunk_id), index);
        chunk_ids.push(Some(reference.chunk_id));
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 0,
            group: reference
                .document_id
                .map(document_group_key)
                .unwrap_or_else(|| chunk_group_key(reference.chunk_id)),
            document_backed: true,
        });
    }
    for reference in prepared_segment_references {
        let index = candidates.len();
        identities.insert(HydrationIdentity::Segment(reference.segment_id), index);
        chunk_ids.push(None);
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 1,
            group: reference
                .document_id
                .map(document_group_key)
                .unwrap_or_else(|| segment_group_key(reference.segment_id)),
            document_backed: true,
        });
    }
    for reference in technical_fact_references {
        chunk_ids.push(None);
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 2,
            group: revision_group_key(reference.revision_id),
            document_backed: false,
        });
    }
    for reference in entity_references {
        chunk_ids.push(None);
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 3,
            group: entity_group_key(reference.node_id),
            document_backed: false,
        });
    }
    for reference in relation_references {
        chunk_ids.push(None);
        candidates.push(ReferenceSelectionCandidate {
            rank: reference.rank,
            kind_order: 4,
            group: relation_group_key(reference.edge_id),
            document_backed: false,
        });
    }
    (candidates, chunk_ids, identities)
}

fn claim_units(
    report: Option<&SemanticVerificationReport>,
    identities: &HashMap<HydrationIdentity, usize>,
    include_projected_sources: bool,
    candidates: &mut Vec<ReferenceSelectionCandidate>,
    chunk_ids: &mut Vec<Option<Uuid>>,
) -> Vec<Vec<usize>> {
    let Some(report) = report else {
        return Vec::new();
    };
    let SemanticVerificationOutcome::Completed { assessment } = &report.outcome else {
        return Vec::new();
    };
    let sources = report
        .sources
        .iter()
        .map(|source| (source.source_id.as_str(), source))
        .collect::<HashMap<_, _>>();
    let mut projected_sources = HashMap::new();
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
                let identity = match &source.locator {
                    SemanticSourceLocator::Chunk { chunk_id } => {
                        Some(HydrationIdentity::Chunk(*chunk_id))
                    }
                    SemanticSourceLocator::Segment { segment_id } => {
                        Some(HydrationIdentity::Segment(*segment_id))
                    }
                    SemanticSourceLocator::DocumentSlice { .. } => None,
                };
                let index = match identity.and_then(|identity| identities.get(&identity).copied()) {
                    Some(index) => Some(index),
                    None if include_projected_sources => {
                        match projected_sources.get(&source.source_id) {
                            Some(&index) => Some(index),
                            None => {
                                let index = candidates.len();
                                candidates.push(ReferenceSelectionCandidate {
                                    rank: i32::MAX,
                                    kind_order: 1,
                                    group: document_group_key(source.document_id),
                                    document_backed: true,
                                });
                                chunk_ids.push(None);
                                projected_sources.insert(source.source_id.clone(), index);
                                Some(index)
                            }
                        }
                    }
                    None => None,
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

fn chunk_hydration_plan(
    chunk_references: &[QueryChunkReference],
    inputs: &ReferenceSelectionInputs<'_>,
) -> Vec<Uuid> {
    chunk_hydration_plan_with_limits(
        chunk_references,
        inputs,
        VISIBLE_REFERENCE_LIMIT,
        MCP_COMPACT_MAX_REFERENCES,
    )
}

fn chunk_hydration_plan_with_limits(
    chunk_references: &[QueryChunkReference],
    inputs: &ReferenceSelectionInputs<'_>,
    ui_limit: usize,
    mcp_limit: usize,
) -> Vec<Uuid> {
    let ReferenceSelectionInputs { semantic_verification, .. } = *inputs;
    let mut selected = Vec::new();

    let (mut ui_candidates, mut ui_chunk_ids, ui_identities) =
        selection_candidates(chunk_references, inputs);
    let ui_claim_units = claim_units(
        semantic_verification,
        &ui_identities,
        false,
        &mut ui_candidates,
        &mut ui_chunk_ids,
    );
    let ui_selection = select_reference_indices(&ui_candidates, &ui_claim_units, ui_limit);
    push_selected_chunks(&mut selected, &ui_selection.indices, &ui_chunk_ids);

    let (mut mcp_candidates, mut mcp_chunk_ids, mcp_identities) =
        selection_candidates(chunk_references, inputs);
    let mcp_claim_units = claim_units(
        semantic_verification,
        &mcp_identities,
        true,
        &mut mcp_candidates,
        &mut mcp_chunk_ids,
    );
    let mcp_selection = select_reference_indices(&mcp_candidates, &mcp_claim_units, mcp_limit);
    push_selected_chunks(&mut selected, &mcp_selection.indices, &mcp_chunk_ids);

    selected
}

fn push_selected_chunks(selected: &mut Vec<Uuid>, indices: &[usize], chunk_ids: &[Option<Uuid>]) {
    for &index in indices {
        let Some(Some(chunk_id)) = chunk_ids.get(index) else {
            continue;
        };
        if !selected.contains(chunk_id) {
            selected.push(*chunk_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CHUNK_METADATA_REQUEST_BATCH_SIZE, HydratedChunkBody, ReferenceSelectionInputs,
        apply_chunk_bodies, chunk_hydration_plan, chunk_hydration_plan_with_limits,
        metadata_request_batches,
    };
    use crate::domains::query::{
        PreparedSegmentReference, QueryChunkReference, QueryGraphEdgeReference,
        QueryGraphNodeReference, TechnicalFactReference, VISIBLE_REFERENCE_LIMIT,
    };
    use crate::mcp_types::MCP_COMPACT_MAX_REFERENCES;
    use crate::shared::extraction::structured_document::StructuredBlockKind;
    use ironrag_contracts::assistant::semantic_verification::{
        SEMANTIC_VERIFICATION_PROTOCOL_VERSION, SemanticAnswerUnit, SemanticAssessment,
        SemanticByteSpan, SemanticClaimSupport, SemanticCompleteness, SemanticEvidenceLink,
        SemanticEvidenceScope, SemanticResponseClassification, SemanticSourceEntry,
        SemanticSourceLocator, SemanticUnitVerdict, SemanticVerificationOutcome,
        SemanticVerificationReport,
    };
    use std::collections::HashMap;
    use uuid::Uuid;

    fn reference(chunk_id: Uuid, rank: i32) -> QueryChunkReference {
        QueryChunkReference {
            execution_id: Uuid::from_u128(1),
            chunk_id,
            revision_id: None,
            rank,
            score: 0.5,
            document_id: None,
            document_title: None,
            section_path: Vec::new(),
            excerpt: None,
        }
    }

    fn segment(segment_id: Uuid, document_id: Uuid, rank: i32) -> PreparedSegmentReference {
        PreparedSegmentReference {
            execution_id: Uuid::from_u128(1),
            segment_id,
            revision_id: Uuid::from_u128(9),
            block_kind: StructuredBlockKind::Paragraph,
            rank,
            score: 0.5,
            heading_trail: Vec::new(),
            section_path: Vec::new(),
            document_id: Some(document_id),
            document_title: None,
            source_uri: None,
            document_hint: None,
            source_access: None,
        }
    }

    fn source(
        source_id: &str,
        document_id: Uuid,
        locator: SemanticSourceLocator,
    ) -> SemanticSourceEntry {
        SemanticSourceEntry {
            source_id: source_id.to_string(),
            document_id,
            document_revision_id: Uuid::from_u128(9),
            locator,
            scope: SemanticEvidenceScope::SelectedExcerpt,
            text_sha256: "a".repeat(64),
            text: source_id.to_string(),
        }
    }

    fn unit(unit_id: &str, evidence: &[&str]) -> SemanticUnitVerdict {
        SemanticUnitVerdict {
            unit_id: unit_id.to_string(),
            support: SemanticClaimSupport::Supported,
            asserts_document_absence: false,
            evidence: evidence
                .iter()
                .map(|source_id| SemanticEvidenceLink {
                    source_id: (*source_id).to_string(),
                    span: SemanticByteSpan { start: 0, end: 1 },
                    quote: "q".to_string(),
                })
                .collect(),
        }
    }

    fn report(
        sources: Vec<SemanticSourceEntry>,
        units: Vec<SemanticUnitVerdict>,
    ) -> SemanticVerificationReport {
        let answer_units = units
            .iter()
            .map(|unit| SemanticAnswerUnit {
                unit_id: unit.unit_id.clone(),
                span: SemanticByteSpan { start: 0, end: 1 },
                text: "x".to_string(),
            })
            .collect();
        SemanticVerificationReport {
            protocol_version: SEMANTIC_VERIFICATION_PROTOCOL_VERSION,
            question_sha256: "b".repeat(64),
            checked_answer_sha256: "c".repeat(64),
            visible_answer_sha256: Some("c".repeat(64)),
            evidence_manifest_sha256: "d".repeat(64),
            answer_units,
            sources,
            elapsed_ms: 1,
            provider_call_id: None,
            preparation_limit: None,
            outcome: SemanticVerificationOutcome::Completed {
                assessment: SemanticAssessment {
                    claim_support: SemanticClaimSupport::Supported,
                    units,
                    completeness: SemanticCompleteness::Complete,
                    missing_caveats: Vec::new(),
                    response_classification: SemanticResponseClassification::FactualAnswer,
                },
            },
        }
    }

    fn empty_others()
    -> (Vec<TechnicalFactReference>, Vec<QueryGraphNodeReference>, Vec<QueryGraphEdgeReference>)
    {
        (Vec::new(), Vec::new(), Vec::new())
    }

    #[test]
    fn metadata_requests_are_bounded_batches_over_the_full_candidate_set() {
        let chunk_ids = (0..130).map(Uuid::from_u128).collect::<Vec<_>>();
        let batches = metadata_request_batches(&chunk_ids);
        assert!(batches.iter().all(|batch| batch.len() <= CHUNK_METADATA_REQUEST_BATCH_SIZE));
        assert_eq!(batches.iter().map(|batch| batch.len()).sum::<usize>(), chunk_ids.len());
        assert_eq!(batches.iter().flat_map(|batch| batch.iter()).count(), chunk_ids.len());
        assert_eq!(
            batches.first().map(|batch| batch.len()),
            Some(CHUNK_METADATA_REQUEST_BATCH_SIZE)
        );
    }

    #[test]
    fn selected_source_hydration_is_bounded_and_keeps_a_beyond_sixteen_source() {
        let document_a = Uuid::from_u128(10);
        let document_b = Uuid::from_u128(11);
        let selected_chunk = Uuid::from_u128(2_000);
        let unselected_chunk = Uuid::from_u128(1_019);
        let mut references = (0..20)
            .map(|index| {
                let mut reference = reference(
                    Uuid::from_u128(1_000 + index),
                    i32::try_from(index + 1).expect("test rank fits i32"),
                );
                reference.document_id = Some(document_a);
                reference
            })
            .collect::<Vec<_>>();
        let mut selected = reference(selected_chunk, 40);
        selected.document_id = Some(document_b);
        references.push(selected);
        let (facts, entities, relations) = empty_others();

        let plan = chunk_hydration_plan(
            &references,
            &ReferenceSelectionInputs {
                prepared_segment_references: &[],
                technical_fact_references: &facts,
                entity_references: &entities,
                relation_references: &relations,
                semantic_verification: None,
            },
        );

        assert!(plan.len() <= VISIBLE_REFERENCE_LIMIT + MCP_COMPACT_MAX_REFERENCES);
        assert!(plan.contains(&selected_chunk));
        assert!(!plan.contains(&unselected_chunk));
    }

    #[test]
    fn mixed_selection_hydrates_a_chunk_omitted_by_chunk_only_claim_compression() {
        let chunk_a = Uuid::from_u128(30);
        let chunk_d = Uuid::from_u128(31);
        let chunk_b = Uuid::from_u128(32);
        let chunk_c = Uuid::from_u128(33);
        let segment_s = Uuid::from_u128(34);
        let chunks = vec![
            reference(chunk_a, 1),
            reference(chunk_d, 2),
            reference(chunk_b, 3),
            reference(chunk_c, 4),
        ];
        let segments = vec![segment(segment_s, Uuid::from_u128(35), 1)];
        let report = report(
            vec![
                source(
                    "source-a",
                    Uuid::from_u128(36),
                    SemanticSourceLocator::Chunk { chunk_id: chunk_a },
                ),
                source(
                    "source-d",
                    Uuid::from_u128(37),
                    SemanticSourceLocator::Chunk { chunk_id: chunk_d },
                ),
                source(
                    "source-s",
                    Uuid::from_u128(35),
                    SemanticSourceLocator::Segment { segment_id: segment_s },
                ),
                source(
                    "source-b",
                    Uuid::from_u128(38),
                    SemanticSourceLocator::Chunk { chunk_id: chunk_b },
                ),
                source(
                    "source-c",
                    Uuid::from_u128(39),
                    SemanticSourceLocator::Chunk { chunk_id: chunk_c },
                ),
            ],
            vec![
                unit("unit-a", &["source-a", "source-d"]),
                unit("unit-b", &["source-s", "source-b"]),
                unit("unit-c", &["source-s", "source-c"]),
            ],
        );
        let (facts, entities, relations) = empty_others();

        let plan = chunk_hydration_plan_with_limits(
            &chunks,
            &ReferenceSelectionInputs {
                prepared_segment_references: &segments,
                technical_fact_references: &facts,
                entity_references: &entities,
                relation_references: &relations,
                semantic_verification: Some(&report),
            },
            3,
            3,
        );

        assert!(plan.contains(&chunk_a));
        assert!(plan.contains(&chunk_d));

        let bodies = plan
            .iter()
            .map(|&chunk_id| HydratedChunkBody {
                chunk_id,
                document_id: Uuid::from_u128(40),
                section_path: Vec::new(),
                heading_trail: Vec::new(),
                content_text: format!("Selected chunk {chunk_id}"),
            })
            .collect::<Vec<_>>();
        let mut hydrated = chunks.clone();
        apply_chunk_bodies(&mut hydrated, &bodies, &HashMap::new());
        let hydrated_d = hydrated.iter().find(|reference| reference.chunk_id == chunk_d).unwrap();
        assert!(hydrated_d.excerpt.is_some());
    }
}
