use super::super::types::{AuthoritativeAnswerEvidence, RuntimeMatchedChunk};
use super::{
    AuthorizedSemanticSource, CompleteDocumentProof, SemanticPreparationError,
    measure_selected_evidence,
};
use crate::{
    app::state::AppState,
    infra::knowledge_rows::{KnowledgeDocumentRow, KnowledgeRevisionRow},
    services::query::{
        assistant_grounding::AssistantGroundingEvidence,
        provider_billing::QueryProviderExecutionContext,
    },
};
use anyhow::Context;
use ironrag_contracts::assistant::semantic_verification::SemanticSourceLocator;
use std::collections::{BTreeSet, HashMap};
use uuid::Uuid;

pub(crate) fn source_scope_matches(
    execution: QueryProviderExecutionContext,
    document: &KnowledgeDocumentRow,
    revision: &KnowledgeRevisionRow,
) -> bool {
    document.workspace_id == execution.workspace_id
        && document.library_id == execution.library_id
        && revision.workspace_id == execution.workspace_id
        && revision.library_id == execution.library_id
        && revision.document_id == document.document_id
        && document.readable_revision_id == Some(revision.revision_id)
        && document.is_query_readable()
}

/// Read only selected identities through their canonical owners. Tool-wrapper
/// strings and runtime excerpt overlays never become source text here.
pub(crate) async fn load_semantic_sources(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    chunks: &[RuntimeMatchedChunk],
    evidence: &AuthoritativeAnswerEvidence,
    grounding: &AssistantGroundingEvidence,
) -> Result<Vec<AuthorizedSemanticSource>, SemanticPreparationError> {
    load_sources_inner(state, execution, chunks, evidence, grounding).await.map_err(|error| {
        tracing::warn!(execution_id = %execution.query_execution_id, %error, "semantic source provenance could not be established");
        SemanticPreparationError::InvalidSource
    })?
}

/// Loads the persisted reference bundle of this execution and proves it still
/// belongs to the execution scope and is current.
async fn load_current_reference_bundle(
    state: &AppState,
    execution: &QueryProviderExecutionContext,
    grounding: &AssistantGroundingEvidence,
) -> anyhow::Result<crate::infra::knowledge_rows::KnowledgeContextBundleReferenceSetRow> {
    let references = state
        .context_store
        .get_bundle_reference_set_by_query_execution(execution.query_execution_id)
        .await?
        .context("selected semantic reference set unavailable")?;
    anyhow::ensure!(
        references.bundle.query_execution_id == Some(execution.query_execution_id)
            && references.bundle.workspace_id == execution.workspace_id
            && references.bundle.library_id == execution.library_id,
        "semantic reference bundle crossed execution scope"
    );
    anyhow::ensure!(
        crate::infra::repositories::query_result_cache_repository::execution_sources_are_current(
            &state.persistence.postgres,
            execution.query_execution_id,
            execution.workspace_id,
            execution.library_id
        )
        .await?,
        "selected semantic execution references are no longer current"
    );
    anyhow::ensure!(
        grounding.document_references.is_empty()
            || references.bundle.assembly_diagnostics.is_object(),
        "selected document references cannot be recorded in malformed bundle diagnostics"
    );
    Ok(references)
}

/// Proves every retained chunk is still readable inside this execution scope.
async fn prove_retained_chunk_scope(
    state: &AppState,
    execution: &QueryProviderExecutionContext,
    references: &crate::infra::knowledge_rows::KnowledgeContextBundleReferenceSetRow,
) -> anyhow::Result<(BTreeSet<Uuid>, Vec<(Uuid, Uuid, Uuid)>)> {
    let query_chunks = sqlx::query_scalar::<_, Uuid>(
        "select chunk_id from query_chunk_reference where execution_id = $1",
    )
    .bind(execution.query_execution_id)
    .fetch_all(&state.persistence.postgres)
    .await?;
    let retained_chunks = references
        .chunk_references
        .iter()
        .map(|reference| reference.chunk_id)
        .chain(query_chunks.iter().copied())
        .collect::<BTreeSet<_>>();
    let retained_chunk_scopes = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>("select chunk_id, document_id, revision_id from knowledge_chunk where chunk_id = any($1::uuid[]) and workspace_id = $2 and library_id = $3")
        .bind(retained_chunks.iter().copied().collect::<Vec<_>>()).bind(execution.workspace_id).bind(execution.library_id).fetch_all(&state.persistence.postgres).await?;
    anyhow::ensure!(
        retained_chunk_scopes.len() == retained_chunks.len(),
        "retained semantic source reference disappeared or crossed scope"
    );
    Ok((retained_chunks, retained_chunk_scopes))
}

/// Loads the selected evidence rows and proves they stay inside the execution
/// scope.
async fn load_scoped_semantic_evidence(
    state: &AppState,
    execution: &QueryProviderExecutionContext,
    references: &crate::infra::knowledge_rows::KnowledgeContextBundleReferenceSetRow,
) -> anyhow::Result<Vec<crate::infra::knowledge_rows::KnowledgeEvidenceRow>> {
    let selected_evidence = state
        .graph_store
        .list_evidence_by_ids(
            &references
                .evidence_references
                .iter()
                .map(|reference| reference.evidence_id)
                .collect::<Vec<_>>(),
        )
        .await?;
    anyhow::ensure!(
        selected_evidence.iter().all(|row| row.workspace_id == execution.workspace_id
            && row.library_id == execution.library_id),
        "semantic evidence references crossed execution scope"
    );
    Ok(selected_evidence)
}

/// Loads the selected and retained technical facts and proves they stay inside
/// the execution scope.
async fn load_scoped_semantic_facts(
    state: &AppState,
    execution: &QueryProviderExecutionContext,
    references: &crate::infra::knowledge_rows::KnowledgeContextBundleReferenceSetRow,
    retained_chunks: &BTreeSet<Uuid>,
    selected_evidence: &[crate::infra::knowledge_rows::KnowledgeEvidenceRow],
) -> anyhow::Result<(BTreeSet<Uuid>, Vec<crate::infra::knowledge_rows::KnowledgeTechnicalFactRow>)>
{
    let fact_ids = references
        .bundle
        .selected_fact_ids
        .iter()
        .copied()
        .chain(selected_evidence.iter().filter_map(|reference| reference.fact_id))
        .collect::<BTreeSet<_>>();
    let mut selected_facts = state
        .document_store
        .list_technical_facts_by_ids(&fact_ids.iter().copied().collect::<Vec<_>>())
        .await?;
    anyhow::ensure!(
        selected_facts.len() == fact_ids.len(),
        "selected semantic fact reference disappeared"
    );
    selected_facts.extend(
        state
            .document_store
            .list_technical_facts_by_chunk_ids(&retained_chunks.iter().copied().collect::<Vec<_>>())
            .await?,
    );
    anyhow::ensure!(
        selected_facts.iter().all(|fact| fact.workspace_id == execution.workspace_id
            && fact.library_id == execution.library_id),
        "semantic fact reference crossed execution scope"
    );
    Ok((fact_ids, selected_facts))
}

/// Reports the measured selection without touching its content.
fn log_selected_semantic_evidence(
    execution_id: Uuid,
    source_entries: usize,
    evidence_utf8_bytes: usize,
) {
    tracing::debug!(
        execution_id = %execution_id,
        source_entries,
        evidence_utf8_bytes,
        "semantic verification evidence selected"
    );
}

/// True when the execution selected no chunk and no segment, and records no
/// document reference worth proving.
fn has_no_selected_evidence(
    ids: &BTreeSet<Uuid>,
    block_ids: &BTreeSet<Uuid>,
    grounding: &AssistantGroundingEvidence,
) -> bool {
    ids.is_empty() && block_ids.is_empty() && grounding.document_references.is_empty()
}

/// The proven reference rows of one execution: its retained chunks, their
/// scopes, the selected evidence and the selected and retained technical facts.
struct ProvenReferences {
    retained_chunks: BTreeSet<Uuid>,
    retained_chunk_scopes: Vec<(Uuid, Uuid, Uuid)>,
    selected_evidence: Vec<crate::infra::knowledge_rows::KnowledgeEvidenceRow>,
    selected_facts: Vec<crate::infra::knowledge_rows::KnowledgeTechnicalFactRow>,
    fact_ids: BTreeSet<Uuid>,
}

/// Loads and proves every reference row the selected evidence will be checked
/// against.
async fn load_proven_references(
    state: &AppState,
    execution: &QueryProviderExecutionContext,
    grounding: &AssistantGroundingEvidence,
) -> anyhow::Result<ProvenReferences> {
    let references = load_current_reference_bundle(state, execution, grounding).await?;
    let (retained_chunks, retained_chunk_scopes) =
        prove_retained_chunk_scope(state, execution, &references).await?;
    let selected_evidence = load_scoped_semantic_evidence(state, execution, &references).await?;
    let (fact_ids, selected_facts) = load_scoped_semantic_facts(
        state,
        execution,
        &references,
        &retained_chunks,
        &selected_evidence,
    )
    .await?;
    Ok(ProvenReferences {
        retained_chunks,
        retained_chunk_scopes,
        selected_evidence,
        selected_facts,
        fact_ids,
    })
}

/// Everything the source-owner proof needs, carried in one value.
struct ReferenceScopeProof<'a> {
    ids: &'a BTreeSet<Uuid>,
    retained_chunks: &'a BTreeSet<Uuid>,
    retained_chunk_scopes: &'a [(Uuid, Uuid, Uuid)],
    selected_evidence: &'a [crate::infra::knowledge_rows::KnowledgeEvidenceRow],
    selected_facts: &'a [crate::infra::knowledge_rows::KnowledgeTechnicalFactRow],
    fact_ids: &'a BTreeSet<Uuid>,
    evidence: &'a AuthoritativeAnswerEvidence,
    grounding: &'a AssistantGroundingEvidence,
    chunks: &'a [RuntimeMatchedChunk],
}

impl ReferenceScopeProof<'_> {
    /// Every scope the selected material claims to belong to, for the ownership
    /// proof that runs after all text reads.
    fn stable_scopes(&self) -> BTreeSet<(Uuid, Uuid)> {
        self.retained_chunk_scopes
            .iter()
            .map(|(_, document, revision)| (*document, *revision))
            .chain(
                self.selected_evidence
                    .iter()
                    .map(|reference| (reference.document_id, reference.revision_id)),
            )
            .chain(
                self.selected_facts
                    .iter()
                    .filter(|fact| self.fact_ids.contains(&fact.fact_id))
                    .map(|fact| (fact.document_id, fact.revision_id)),
            )
            .chain(self.grounding.document_references.iter().filter_map(|reference| {
                reference.revision_id.map(|revision| (reference.document_id, revision))
            }))
            // Typed agent-manifest chunks are authorized sources admitted before
            // generation; their exact owners are still rechecked below.
            .chain(self.chunks.iter().map(|chunk| (chunk.document_id, chunk.revision_id)))
            // Exact typed child passthrough may select segments from its report,
            // not from the current bundle; reload/currentness checks below remain.
            .chain(
                self.evidence
                    .structured_blocks
                    .iter()
                    .map(|block| (block.document_id, block.revision_id)),
            )
            .collect()
    }

    /// The chunk ids the selected material authorizes.
    fn allowed_chunk_ids(&self) -> BTreeSet<Uuid> {
        allowed_semantic_chunk_ids(
            self.ids,
            self.retained_chunks,
            self.selected_evidence,
            self.selected_facts,
        )
    }
}

/// Loads the selected chunk rows and structured blocks, proving each is covered
/// by a selected authoritative reference and keeps its selected identity.
async fn load_proven_selected_scope(
    state: &AppState,
    ids: &BTreeSet<Uuid>,
    block_ids: &BTreeSet<Uuid>,
    proof: &ReferenceScopeProof<'_>,
) -> anyhow::Result<(
    Vec<crate::infra::knowledge_rows::KnowledgeChunkRow>,
    Vec<crate::infra::knowledge_rows::KnowledgeStructuredBlockRow>,
)> {
    let rows =
        state.document_store.list_chunks_by_ids(&ids.iter().copied().collect::<Vec<_>>()).await?;
    anyhow::ensure!(rows.len() == ids.len(), "selected semantic chunk disappeared");
    anyhow::ensure!(
        rows.iter().all(|row| ids.contains(&row.chunk_id)),
        "semantic chunk owner returned an unselected identity"
    );
    let allowed_blocks = proof
        .evidence
        .structured_blocks
        .iter()
        .map(|block| block.block_id)
        .chain(rows.iter().flat_map(|row| row.support_block_ids.iter().copied()))
        .chain(proof.selected_evidence.iter().filter_map(|row| row.block_id))
        .chain(proof.selected_facts.iter().flat_map(|fact| fact.support_block_ids.iter().copied()))
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        block_ids.is_subset(&allowed_blocks),
        "semantic segment lacks a selected authoritative reference"
    );
    let blocks = state
        .document_store
        .list_structured_blocks_by_ids(&block_ids.iter().copied().collect::<Vec<_>>())
        .await?;
    anyhow::ensure!(blocks.len() == block_ids.len(), "selected semantic segment disappeared");
    anyhow::ensure!(
        blocks.iter().all(|row| block_ids.contains(&row.block_id)),
        "semantic segment owner returned an unselected identity"
    );
    Ok((rows, blocks))
}

/// Loads and authors the selected chunk sources, proving each is covered by its
/// selected source reference and keeps its selected owner.
async fn collect_chunk_sources(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    rows: Vec<crate::infra::knowledge_rows::KnowledgeChunkRow>,
    proof: &ReferenceScopeProof<'_>,
    owners: &mut HashMap<(Uuid, Uuid), (KnowledgeDocumentRow, KnowledgeRevisionRow)>,
) -> anyhow::Result<Vec<AuthorizedSemanticSource>> {
    let mut sources = Vec::new();
    for row in rows {
        validate_selected_chunk(&row, execution, proof.chunks, proof.evidence)?;
        let referenced = proof.ids.contains(&row.chunk_id)
            || chunk_is_referenced(
                &row,
                proof.retained_chunks,
                proof.selected_evidence,
                proof.selected_facts,
            );
        anyhow::ensure!(
            referenced,
            "semantic chunk document/revision is not covered by its selected source reference"
        );
        load_owner(state, execution, row.document_id, row.revision_id, owners).await?;
        sources.push(selected_chunk_source(&row));
    }
    Ok(sources)
}

async fn load_sources_inner(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    chunks: &[RuntimeMatchedChunk],
    evidence: &AuthoritativeAnswerEvidence,
    grounding: &AssistantGroundingEvidence,
) -> anyhow::Result<Result<Vec<AuthorizedSemanticSource>, SemanticPreparationError>> {
    let selection = match measure_selected_evidence(chunks, evidence, grounding) {
        Ok(selection) => selection,
        Err(error) => return Ok(Err(error)),
    };
    let measurement = selection.measurement;
    let ids = selection.chunk_ids;
    let block_ids = selection.block_ids;
    log_selected_semantic_evidence(
        execution.query_execution_id,
        measurement.source_entries,
        measurement.evidence_utf8_bytes,
    );
    if has_no_selected_evidence(&ids, &block_ids, grounding) {
        return Ok(Ok(Vec::new()));
    }
    let proven = load_proven_references(state, &execution, grounding).await?;
    let retained_chunks = proven.retained_chunks;
    let retained_chunk_scopes = proven.retained_chunk_scopes;
    let selected_evidence = proven.selected_evidence;
    let selected_facts = proven.selected_facts;
    let fact_ids = proven.fact_ids;
    let proof = ReferenceScopeProof {
        ids: &ids,
        retained_chunks: &retained_chunks,
        retained_chunk_scopes: &retained_chunk_scopes,
        selected_evidence: &selected_evidence,
        selected_facts: &selected_facts,
        fact_ids: &fact_ids,
        evidence,
        grounding,
        chunks,
    };
    let stable_reference_scopes = proof.stable_scopes();
    anyhow::ensure!(
        ids.is_subset(&proof.allowed_chunk_ids()),
        "semantic chunk lacks a selected authoritative reference"
    );
    let (rows, blocks) = load_proven_selected_scope(state, &ids, &block_ids, &proof).await?;
    let mut owners: HashMap<(Uuid, Uuid), (KnowledgeDocumentRow, KnowledgeRevisionRow)> =
        HashMap::new();
    let mut sources = collect_chunk_sources(state, execution, rows, &proof, &mut owners).await?;
    append_segment_sources(
        state,
        execution,
        blocks,
        &evidence.structured_blocks,
        &mut owners,
        &mut sources,
    )
    .await?;
    append_document_sources(state, execution, grounding, &mut owners, &mut sources).await?;
    // Recheck document heads after all text reads; do not silently discard a
    // source whose readable revision moved while preparation was loading.
    recheck_source_owners(state, execution, owners, &stable_reference_scopes).await?;
    Ok(Ok(sources))
}

async fn append_segment_sources(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    blocks: Vec<crate::infra::knowledge_rows::KnowledgeStructuredBlockRow>,
    selected_blocks: &[crate::infra::knowledge_rows::KnowledgeStructuredBlockRow],
    owners: &mut HashMap<(Uuid, Uuid), (KnowledgeDocumentRow, KnowledgeRevisionRow)>,
    sources: &mut Vec<AuthorizedSemanticSource>,
) -> anyhow::Result<()> {
    for block in blocks {
        anyhow::ensure!(
            block.workspace_id == execution.workspace_id
                && block.library_id == execution.library_id,
            "selected semantic segment crossed execution scope"
        );
        anyhow::ensure!(
            selected_blocks
                .iter()
                .filter(|selected| selected.block_id == block.block_id)
                .all(|selected| selected.document_id == block.document_id
                    && selected.revision_id == block.revision_id),
            "selected semantic segment revision changed"
        );
        load_owner(state, execution, block.document_id, block.revision_id, owners).await?;
        sources.push(AuthorizedSemanticSource {
            document_id: block.document_id,
            document_revision_id: block.revision_id,
            locator: SemanticSourceLocator::Segment { segment_id: block.block_id },
            text: super::super::chunk_support::bounded_evidence_text(&block.text),
            complete_document: None,
        });
    }
    Ok(())
}

fn validate_selected_chunk(
    row: &crate::infra::knowledge_rows::KnowledgeChunkRow,
    execution: QueryProviderExecutionContext,
    chunks: &[RuntimeMatchedChunk],
    evidence: &AuthoritativeAnswerEvidence,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        row.workspace_id == execution.workspace_id
            && row.library_id == execution.library_id
            && row.chunk_state == "ready",
        "selected semantic chunk crossed execution scope or is not ready"
    );
    anyhow::ensure!(
        chunks
            .iter()
            .filter(|selected| selected.chunk_id == row.chunk_id)
            .all(|selected| selected.document_id == row.document_id
                && selected.revision_id == row.revision_id),
        "selected semantic chunk revision changed"
    );
    anyhow::ensure!(
        evidence
            .chunk_rows
            .iter()
            .filter(|selected| selected.chunk_id == row.chunk_id)
            .all(|selected| selected.document_id == row.document_id
                && selected.revision_id == row.revision_id),
        "selected authoritative chunk revision changed"
    );
    Ok(())
}

/// Selected canonical windows and table projections are the same evidence the
/// answer consumer sees. A single chunk still grants no complete-document proof.
/// The attributed text is the chunk's own projection only: the answer context
/// may additionally carry a containing window, and crediting that window to this
/// chunk id would cite text the cited chunk does not hold.
pub(super) fn selected_chunk_source(
    row: &crate::infra::knowledge_rows::KnowledgeChunkRow,
) -> AuthorizedSemanticSource {
    AuthorizedSemanticSource {
        document_id: row.document_id,
        document_revision_id: row.revision_id,
        locator: SemanticSourceLocator::Chunk { chunk_id: row.chunk_id },
        text: super::super::chunk_support::chunk_own_source_text(row),
        complete_document: None,
    }
}

/// The answer's own primary chunks and authoritative evidence rows are selected
/// sources even when the persisted bundle reference set does not repeat every
/// chunk the answer stage used. Document/revision scope and current-readable
/// checks still run on each loaded owner.
fn allowed_semantic_chunk_ids(
    answer_chunk_ids: &BTreeSet<Uuid>,
    retained: &BTreeSet<Uuid>,
    selected_evidence: &[crate::infra::knowledge_rows::KnowledgeEvidenceRow],
    selected_facts: &[crate::infra::knowledge_rows::KnowledgeTechnicalFactRow],
) -> BTreeSet<Uuid> {
    answer_chunk_ids
        .iter()
        .copied()
        .chain(retained.iter().copied())
        .chain(selected_evidence.iter().filter_map(|row| row.chunk_id))
        .chain(selected_facts.iter().flat_map(|fact| fact.support_chunk_ids.iter().copied()))
        .collect()
}

fn chunk_is_referenced(
    row: &crate::infra::knowledge_rows::KnowledgeChunkRow,
    retained: &BTreeSet<Uuid>,
    selected_evidence: &[crate::infra::knowledge_rows::KnowledgeEvidenceRow],
    selected_facts: &[crate::infra::knowledge_rows::KnowledgeTechnicalFactRow],
) -> bool {
    retained.contains(&row.chunk_id)
        || selected_evidence.iter().any(|reference| {
            reference.chunk_id == Some(row.chunk_id)
                && reference.document_id == row.document_id
                && reference.revision_id == row.revision_id
        })
        || selected_facts.iter().any(|fact| {
            fact.support_chunk_ids.contains(&row.chunk_id)
                && fact.document_id == row.document_id
                && fact.revision_id == row.revision_id
        })
}

async fn append_document_sources(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    grounding: &AssistantGroundingEvidence,
    owners: &mut HashMap<(Uuid, Uuid), (KnowledgeDocumentRow, KnowledgeRevisionRow)>,
    sources: &mut Vec<AuthorizedSemanticSource>,
) -> anyhow::Result<()> {
    for reference in &grounding.document_references {
        let revision_id = reference
            .revision_id
            .context("semantic document slice lacks immutable revision identity")?;
        // Existing selected read_document references are retained verbatim by
        // the final atomic update at assistantGrounding.documentReferences.
        // The canonical read-side source guard already covers that path; no
        // extra document is introduced solely by the report.
        load_owner(state, execution, reference.document_id, revision_id, owners).await?;
        let (_, revision) = owners
            .get(&(reference.document_id, revision_id))
            .context("semantic document owner missing")?;
        let canonical = revision
            .normalized_text
            .as_deref()
            .context("canonical document text is unavailable")?;
        let (start, end, text) = canonical_document_slice(
            canonical,
            reference.slice_start_offset,
            reference.slice_end_offset,
        )?;
        anyhow::ensure!(
            text == reference.excerpt,
            "selected semantic document slice changed or was projected"
        );
        let complete_document = if start == 0 && end == canonical.len() {
            Some(CompleteDocumentProof {
                canonical_text_sha256: revision
                    .text_checksum
                    .clone()
                    .context("complete canonical document checksum unavailable")?,
                canonical_utf8_bytes: canonical.len(),
            })
        } else {
            None
        };
        sources.push(AuthorizedSemanticSource {
            document_id: reference.document_id,
            document_revision_id: revision_id,
            locator: SemanticSourceLocator::DocumentSlice { start, end },
            text: text.to_owned(),
            complete_document,
        });
    }
    Ok(())
}

async fn recheck_source_owners(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    owners: HashMap<(Uuid, Uuid), (KnowledgeDocumentRow, KnowledgeRevisionRow)>,
    stable_reference_scopes: &BTreeSet<(Uuid, Uuid)>,
) -> anyhow::Result<()> {
    for ((document_id, _), (_, revision)) in owners {
        anyhow::ensure!(
            stable_reference_scopes.contains(&(document_id, revision.revision_id)),
            "semantic report would introduce an unrecorded document revision"
        );
        let current = state
            .document_store
            .get_document(document_id)
            .await?
            .context("semantic document disappeared")?;
        anyhow::ensure!(
            source_scope_matches(execution, &current, &revision),
            "semantic readable revision changed while loading sources"
        );
        anyhow::ensure!(
            source_is_current(state, execution, document_id, revision.revision_id).await?,
            "authoritative semantic source head changed while loading sources"
        );
    }
    Ok(())
}

async fn load_owner(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    document_id: Uuid,
    revision_id: Uuid,
    owners: &mut HashMap<(Uuid, Uuid), (KnowledgeDocumentRow, KnowledgeRevisionRow)>,
) -> anyhow::Result<()> {
    if owners.contains_key(&(document_id, revision_id)) {
        return Ok(());
    }
    let document = state
        .document_store
        .get_document(document_id)
        .await?
        .context("selected semantic document unavailable")?;
    let revision = state
        .document_store
        .get_revision(revision_id)
        .await?
        .context("selected semantic revision unavailable")?;
    anyhow::ensure!(
        document.document_id == document_id && revision.revision_id == revision_id,
        "semantic source owner returned a different identity"
    );
    let structured = state
        .document_store
        .get_structured_revision(revision_id)
        .await?
        .context("selected semantic revision not prepared")?;
    anyhow::ensure!(
        source_scope_matches(execution, &document, &revision),
        "semantic source is not the current readable execution-scoped revision"
    );
    anyhow::ensure!(
        structured.preparation_state == "prepared"
            && structured.workspace_id == execution.workspace_id
            && structured.library_id == execution.library_id
            && structured.document_id == document_id
            && structured.revision_id == revision_id,
        "semantic preparation scope mismatch"
    );
    anyhow::ensure!(
        source_is_current(state, execution, document_id, revision_id).await?,
        "semantic source is not an authoritative current revision"
    );
    owners.insert((document_id, revision_id), (document, revision));
    Ok(())
}

async fn source_is_current(
    state: &AppState,
    execution: QueryProviderExecutionContext,
    document_id: Uuid,
    revision_id: Uuid,
) -> anyhow::Result<bool> {
    sqlx::query_scalar::<_, bool>("select exists(select 1 from content_current_revision where document_id = $1 and revision_id = $2 and workspace_id = $3 and library_id = $4)")
        .bind(document_id).bind(revision_id).bind(execution.workspace_id).bind(execution.library_id).fetch_one(&state.persistence.postgres).await.map_err(Into::into)
}

/// read_document's server-owned offsets count Unicode characters. Convert at
/// the canonical text boundary; the semantic protocol itself always uses bytes.
pub(super) fn canonical_document_slice(
    text: &str,
    start_chars: usize,
    end_chars: usize,
) -> anyhow::Result<(usize, usize, &str)> {
    anyhow::ensure!(start_chars <= end_chars, "reversed selected document character range");
    let boundary = |offset| {
        text.char_indices().map(|(byte, _)| byte).chain(std::iter::once(text.len())).nth(offset)
    };
    let start = boundary(start_chars).context("selected document start exceeds canonical text")?;
    let end = boundary(end_chars).context("selected document end exceeds canonical text")?;
    let selected = text.get(start..end).context("invalid canonical document byte range")?;
    Ok((start, end, selected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_primary_chunks_are_authorized_without_a_bundle_reference() {
        let answer_chunk = Uuid::from_u128(1);
        let allowed =
            allowed_semantic_chunk_ids(&BTreeSet::from([answer_chunk]), &BTreeSet::new(), &[], &[]);
        assert!(
            allowed.contains(&answer_chunk),
            "the answer's own selected chunk must be authorized even when the persisted bundle does not repeat it"
        );
    }
}
