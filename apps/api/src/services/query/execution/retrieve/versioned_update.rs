use crate::infra::knowledge_plane::ChunkLexicalQuery;

use futures::StreamExt;

use anyhow::Context;

use super::{
    AppState, BTreeMap, BTreeSet, HashMap, KnowledgeChunkRow, KnowledgeDocumentRow,
    LexicalQueryOrigin, LiteralKind, QueryAct, QueryIR, QueryTargetKind, RetrievalLaneContext,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, Uuid,
    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT,
    VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP,
    VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS,
    VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT,
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP,
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY,
    VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT,
    VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP,
    VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR,
    VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT, batch_hydrate_hits,
    chunk_is_setup_focus_command_path_anchor, current_document_revision_id,
    extract_semver_like_version, map_chunk_hit, normalized_alnum_token_sequence,
    procedure_artifact_token_count, query_ir_has_setup_configuration_target,
    query_ir_requires_remediation_synthesis, score_desc_chunks,
    setup_focus_document_is_standalone_image, shellish_inline_token_starts_command,
    shellish_token_has_artifact_preparation_signal, shellish_token_has_external_artifact,
    shellish_token_is_local_artifact, shellish_tokens_from_text, stream,
    strip_leading_procedure_order_marker, token_sequence_contains_tokens,
    versioned_update_procedure_chunk_score, versioned_update_procedure_line_has_command_start,
};

#[derive(Clone)]
pub(super) struct VersionedUpdateProcedureFocusModel {
    pub(super) focus_terms: BTreeSet<String>,
    pub(super) target_identity_sequences: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy)]
pub(super) struct VersionedUpdateProcedureChunkEvidence {
    pub(super) target_identity_score: usize,
    pub(super) label_has_target_identity: bool,
    pub(super) ordered_step_score: usize,
    pub(super) command_score: usize,
    pub(super) version_transition_score: usize,
    pub(super) structural_score: usize,
    pub(super) score: usize,
    pub(super) has_setup_script_signature: bool,
}

pub(super) fn versioned_update_title_candidate_ids(
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> Vec<Uuid> {
    let mut candidates = document_index
        .values()
        .filter(|document| {
            document.document_role == crate::domains::content::DOCUMENT_ROLE_PRIMARY
                && !setup_focus_document_is_standalone_image(document)
        })
        .filter_map(|document| {
            let label = document.title.as_deref().unwrap_or(&document.external_key);
            versioned_update_procedure_text_has_target_identity_sequence(label, focus_model)
                .then_some((label.to_lowercase(), document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    candidates.into_iter().map(|(_, document_id)| document_id).collect()
}

pub(super) async fn versioned_update_discovered_document_ids(
    state: &AppState,
    library_id: Uuid,
    focus_model: &VersionedUpdateProcedureFocusModel,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    focus_terms: &[String],
) -> anyhow::Result<Vec<Uuid>> {
    let mut discovered_hits = Vec::new();
    for target_sequence in focus_model
        .target_identity_sequences
        .iter()
        .take(VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_QUERY_CAP)
    {
        let search_query = target_sequence.join(" ");
        if search_query.is_empty() {
            continue;
        }
        match state
            .search_store
            .search_chunks(ChunkLexicalQuery {
                library_id,
                query: &search_query,
                query_origin: LexicalQueryOrigin::DerivedProbe,
                limit: VERSIONED_UPDATE_PROCEDURE_EVIDENCE_SEARCH_HIT_LIMIT,
                temporal_start: None,
                temporal_end: None,
                text_search_config: None,
            })
            .await
        {
            Ok(rows) => {
                discovered_hits.extend(rows.into_iter().map(|row| (row.chunk_id, row.score as f32)))
            }
            Err(error) => tracing::warn!(
                stage = "retrieval.versioned_update_procedure_identity_probe_failed",
                error = %error,
                retrieval_degraded = true,
                "typed procedure identity probe failed; exact-title candidates remain available"
            ),
        }
    }
    let chunks =
        batch_hydrate_hits(state, discovered_hits, document_index, focus_terms, &BTreeSet::new())
            .await
            .context("failed to hydrate typed procedure identity probes")?;
    Ok(chunks
        .into_iter()
        .filter(|chunk| {
            let text = format!("{}\n{}", chunk.document_label, chunk.source_text);
            versioned_update_procedure_text_has_target_identity_sequence(&text, focus_model)
        })
        .map(|chunk| chunk.document_id)
        .collect())
}

pub(super) fn versioned_update_fetch_inputs(
    document_ids: &[Uuid],
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> Vec<(usize, Uuid, Uuid)> {
    document_ids
        .iter()
        .enumerate()
        .filter_map(|(rank, document_id)| {
            let revision_id = current_document_revision_id(document_index.get(document_id)?)?;
            Some((rank, *document_id, revision_id))
        })
        .collect()
}

pub(super) fn append_unique_document_ids(
    source: impl IntoIterator<Item = Uuid>,
    seen: &mut BTreeSet<Uuid>,
    destination: &mut Vec<Uuid>,
) {
    destination.extend(source.into_iter().filter(|document_id| seen.insert(*document_id)));
}

pub(super) async fn load_versioned_update_candidate_rows(
    state: &AppState,
    fetch_inputs: Vec<(usize, Uuid, Uuid)>,
    focus_terms: &[String],
) -> anyhow::Result<Vec<(usize, Uuid, Vec<KnowledgeChunkRow>)>> {
    let fetches = fetch_inputs.into_iter().map(|(document_rank, document_id, revision_id)| {
        let focus_terms = focus_terms.to_vec();
        async move {
            let matching_rows = state
                .document_store
                .list_chunks_by_revision_matching_terms(
                    revision_id,
                    &focus_terms,
                    VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT,
                )
                .await
                .with_context(|| {
                    format!("failed to load typed procedure identity chunks for document {document_id}")
                })?;
            let windows = matching_rows
                .iter()
                .map(|row| {
                    (
                        row.chunk_index
                            .saturating_sub(VERSIONED_UPDATE_PROCEDURE_CONTEXT_BACKWARD_CHUNKS)
                            .max(0),
                        row.chunk_index.saturating_add(3),
                    )
                })
                .collect::<Vec<_>>();
            let context_rows = if windows.is_empty() {
                Vec::new()
            } else {
                state
                    .document_store
                    .list_chunks_by_revision_windows(revision_id, &windows)
                    .await
                    .with_context(|| {
                        format!("failed to load typed procedure context windows for document {document_id}")
                    })?
            };
            let head_rows = state
                .document_store
                .list_chunks_by_revision_range(
                    revision_id,
                    0,
                    VERSIONED_UPDATE_PROCEDURE_CHUNKS_PER_DOCUMENT.saturating_sub(1) as i32,
                )
                .await
                .with_context(|| {
                    format!("failed to load typed procedure document head for document {document_id}")
                })?;
            Ok::<_, anyhow::Error>((
                document_rank,
                document_id,
                merge_versioned_update_procedure_rows(matching_rows, context_rows, head_rows),
            ))
        }
    });
    stream::iter(fetches)
        .buffer_unordered(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_FETCH_CONCURRENCY)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect()
}

pub(super) async fn load_versioned_update_procedure_chunks(
    context: RetrievalLaneContext<'_>,
    library_id: Uuid,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let RetrievalLaneContext { state, document_index, temporal_start, temporal_end, .. } = context;
    let Some(query_ir) = query_ir else {
        return Ok(Vec::new());
    };
    if temporal_start.is_some()
        || temporal_end.is_some()
        || !query_ir_requests_versioned_update_procedure_context("", query_ir)
    {
        return Ok(Vec::new());
    }
    let Some(focus_model) = versioned_update_procedure_focus_model(query_ir) else {
        return Ok(Vec::new());
    };
    let focus_terms = focus_model.focus_terms.iter().cloned().collect::<Vec<_>>();
    let mut candidate_document_ids = Vec::<Uuid>::new();
    let mut seen_document_ids = BTreeSet::<Uuid>::new();
    append_unique_document_ids(
        versioned_update_title_candidate_ids(document_index, &focus_model),
        &mut seen_document_ids,
        &mut candidate_document_ids,
    );
    let discovered_document_ids = versioned_update_discovered_document_ids(
        state,
        library_id,
        &focus_model,
        document_index,
        &focus_terms,
    )
    .await?;
    append_unique_document_ids(
        discovered_document_ids,
        &mut seen_document_ids,
        &mut candidate_document_ids,
    );
    candidate_document_ids.truncate(VERSIONED_UPDATE_PROCEDURE_DOCUMENT_CANDIDATE_CAP);
    if candidate_document_ids.is_empty() {
        return Ok(Vec::new());
    }

    let fetched = load_versioned_update_candidate_rows(
        state,
        versioned_update_fetch_inputs(&candidate_document_ids, document_index),
        &focus_terms,
    )
    .await?;

    let mut chunks = Vec::<RuntimeMatchedChunk>::new();
    for (document_rank, document_id, rows) in fetched {
        let mut mapped = rows
            .into_iter()
            .enumerate()
            .filter_map(|(chunk_rank, row)| {
                let score = versioned_update_procedure_chunk_score(document_rank, chunk_rank);
                map_chunk_hit(row, score, document_index, &focus_terms)
            })
            .collect::<Vec<_>>();
        mapped.sort_by(|left, right| {
            left.chunk_index
                .cmp(&right.chunk_index)
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        let Some(first) = mapped.first() else {
            continue;
        };
        let combined_text =
            mapped.iter().map(|chunk| chunk.source_text.as_str()).collect::<Vec<_>>().join("\n");
        let evidence = versioned_update_procedure_text_evidence(
            &first.document_label,
            &combined_text,
            &focus_model,
        );
        if !versioned_update_procedure_evidence_supports_runbook(evidence) {
            continue;
        }
        let label_has_target_identity = evidence.label_has_target_identity;
        for (chunk_rank, mut chunk) in mapped.into_iter().enumerate() {
            chunk.score = Some(
                versioned_update_procedure_chunk_score(document_rank, chunk_rank)
                    + evidence.score.min(65_536) as f32 / 32.0,
            );
            chunk.score_kind = if label_has_target_identity {
                RuntimeChunkScoreKind::DocumentIdentity
            } else {
                RuntimeChunkScoreKind::FocusedDocument
            };
            chunks.push(chunk);
        }
        tracing::debug!(
            stage = "retrieval.versioned_update_procedure_document",
            %document_id,
            structural_score = evidence.structural_score,
            ordered_step_score = evidence.ordered_step_score,
            command_score = evidence.command_score,
            "accepted exact typed-identity procedure document"
        );
    }
    chunks.sort_by(|left, right| {
        score_desc_chunks(left, right)
            .then_with(|| left.document_id.cmp(&right.document_id))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
    });
    chunks.truncate(VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT);
    Ok(chunks)
}

pub(super) fn merge_versioned_update_procedure_rows(
    matching_rows: Vec<KnowledgeChunkRow>,
    context_rows: Vec<KnowledgeChunkRow>,
    head_rows: Vec<KnowledgeChunkRow>,
) -> Vec<KnowledgeChunkRow> {
    let mut rows = BTreeMap::<Uuid, KnowledgeChunkRow>::new();
    for row in matching_rows.into_iter().chain(context_rows).chain(head_rows) {
        rows.entry(row.chunk_id).or_insert(row);
    }
    let mut rows = rows.into_values().collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        left.chunk_index.cmp(&right.chunk_index).then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    rows.truncate(VERSIONED_UPDATE_PROCEDURE_MATCH_PROBE_CHUNKS_PER_DOCUMENT);
    rows
}

pub(super) async fn load_versioned_update_procedure_source_local_runbook_chunks(
    _state: &AppState,
    anchor_chunks: &[RuntimeMatchedChunk],
    _document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    _focus_terms: &[String],
    _question: &str,
    query_ir: Option<&QueryIR>,
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    let Some(query_ir) = query_ir else {
        return Ok(Vec::new());
    };
    if !query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return Ok(Vec::new());
    }
    let mut chunks = anchor_chunks
        .iter()
        .filter_map(|chunk| {
            let score = versioned_update_exact_target_runbook_score("", query_ir, chunk)?;
            let mut chunk = chunk.clone();
            chunk.score = Some(
                versioned_update_procedure_chunk_score(0, 0) + score.min(65_536) as f32 / 32.0,
            );
            chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
            Some(chunk)
        })
        .collect::<Vec<_>>();
    chunks.sort_by(score_desc_chunks);
    chunks.truncate(VERSIONED_UPDATE_PROCEDURE_CONTEXT_RESERVATION_LIMIT);
    Ok(chunks)
}

pub(super) fn question_requests_versioned_update_procedure_evidence(
    _question: &str,
    query_ir: Option<&QueryIR>,
) -> bool {
    query_ir
        .is_some_and(|query_ir| query_ir_requests_versioned_update_procedure_context("", query_ir))
}

pub(in crate::services::query::execution) fn query_ir_requests_versioned_update_procedure_context(
    _question: &str,
    query_ir: &QueryIR,
) -> bool {
    matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        && query_ir.source_slice.is_none()
        && query_ir.needs_clarification.is_none()
        && !query_ir_requires_remediation_synthesis(query_ir)
        && !query_ir_has_setup_configuration_target(query_ir)
        && query_ir.targets(QueryTargetKind::Procedure)
        && query_ir.targets_any(&[QueryTargetKind::Version, QueryTargetKind::Release])
        && !query_ir.targets(QueryTargetKind::Concept)
        && versioned_update_procedure_focus_model(query_ir).is_some()
}

pub(super) fn versioned_update_procedure_focus_model(
    query_ir: &QueryIR,
) -> Option<VersionedUpdateProcedureFocusModel> {
    let mut sequences = Vec::<Vec<String>>::new();
    let mut seen_sequences = BTreeSet::<Vec<String>>::new();
    for label in
        query_ir.target_entities.iter().map(|entity| entity.label.as_str()).chain(
            query_ir.document_focus.iter().map(|document_focus| document_focus.hint.as_str()),
        )
    {
        let sequence = normalized_alnum_token_sequence(label, 1);
        if versioned_update_procedure_named_identity_is_usable(&sequence)
            && seen_sequences.insert(sequence.clone())
        {
            sequences.push(sequence);
        }
    }
    for literal in &query_ir.literal_constraints {
        if !matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other) {
            continue;
        }
        let sequence = normalized_alnum_token_sequence(&literal.text, 1);
        if !sequence.is_empty() && seen_sequences.insert(sequence.clone()) {
            sequences.push(sequence);
        }
    }
    if sequences.is_empty() {
        return None;
    }
    let focus_terms = sequences.iter().flatten().cloned().collect::<BTreeSet<_>>();
    (!focus_terms.is_empty()).then_some(VersionedUpdateProcedureFocusModel {
        focus_terms,
        target_identity_sequences: sequences,
    })
}

pub(super) fn versioned_update_procedure_named_identity_is_usable(sequence: &[String]) -> bool {
    sequence.len() >= 2
        && sequence.iter().map(|token| token.chars().count()).sum::<usize>() >= 7
        && sequence.iter().any(|token| token.chars().count() >= 3)
}

pub(super) fn versioned_update_procedure_text_has_target_identity_sequence(
    text: &str,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> bool {
    let text_sequence = normalized_alnum_token_sequence(text, 1);
    focus_model
        .target_identity_sequences
        .iter()
        .any(|target_sequence| token_sequence_contains_tokens(&text_sequence, target_sequence))
}

pub(super) fn versioned_update_procedure_label_has_exact_target_identity(
    label: &str,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> bool {
    versioned_update_procedure_text_has_target_identity_sequence(label, focus_model)
}

pub(super) fn versioned_update_procedure_text_evidence(
    document_label: &str,
    text: &str,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> VersionedUpdateProcedureChunkEvidence {
    let label_has_target_identity =
        versioned_update_procedure_label_has_exact_target_identity(document_label, focus_model);
    let body_has_target_identity =
        versioned_update_procedure_text_has_target_identity_sequence(text, focus_model);
    let target_identity_score = usize::from(label_has_target_identity).saturating_mul(2)
        + usize::from(body_has_target_identity);
    let ordered_step_score = versioned_update_procedure_formal_ordered_step_score(text);
    let command_score = versioned_update_procedure_formal_command_score(text);
    let version_transition_score =
        versioned_update_procedure_ordered_version_transition_score(text);
    let structural_score = ordered_step_score
        .saturating_add(command_score)
        .saturating_add(version_transition_score.saturating_mul(2))
        .saturating_add(procedure_artifact_token_count(text).min(4));
    let has_setup_script_signature =
        versioned_update_procedure_text_has_setup_script_signature(text);
    let score = target_identity_score
        .saturating_mul(4096)
        .saturating_add(ordered_step_score.saturating_mul(2048))
        .saturating_add(command_score.saturating_mul(1536))
        .saturating_add(version_transition_score.saturating_mul(1024))
        .saturating_add(structural_score.saturating_mul(128));
    VersionedUpdateProcedureChunkEvidence {
        target_identity_score,
        label_has_target_identity,
        ordered_step_score,
        command_score,
        version_transition_score,
        structural_score,
        score,
        has_setup_script_signature,
    }
}

pub(super) fn versioned_update_procedure_chunk_evidence(
    chunk: &RuntimeMatchedChunk,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> VersionedUpdateProcedureChunkEvidence {
    versioned_update_procedure_text_evidence(
        &chunk.document_label,
        &format!("{}\n{}", chunk.source_text, chunk.excerpt),
        focus_model,
    )
}

pub(super) fn versioned_update_procedure_chunk_runbook_evidence(
    chunk: &RuntimeMatchedChunk,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> Option<VersionedUpdateProcedureChunkEvidence> {
    let evidence = versioned_update_procedure_chunk_evidence(chunk, focus_model);
    if !versioned_update_procedure_evidence_supports_runbook(evidence) {
        return None;
    }
    let is_unordered_setup_anchor = chunk_is_setup_focus_command_path_anchor(chunk)
        && evidence.ordered_step_score == 0
        && evidence.version_transition_score == 0;
    (!is_unordered_setup_anchor).then_some(evidence)
}

pub(super) fn versioned_update_procedure_evidence_supports_runbook(
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> bool {
    let formal_step_count = evidence.ordered_step_score.saturating_add(evidence.command_score);
    evidence.target_identity_score > 0
        && formal_step_count >= 2
        && evidence.structural_score >= VERSIONED_UPDATE_PROCEDURE_EVIDENCE_STRUCTURAL_SCORE_FLOOR
        && versioned_update_procedure_setup_signature_allows_runbook(evidence)
}

pub(super) fn versioned_update_procedure_setup_signature_allows_runbook(
    evidence: VersionedUpdateProcedureChunkEvidence,
) -> bool {
    !evidence.has_setup_script_signature
        || (evidence.ordered_step_score >= 2 && evidence.command_score > 0)
}

pub(super) fn versioned_update_procedure_formal_ordered_step_score(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            ordered_step_marker_count(line) > 0 && !ordered_step_looks_like_sequential_record(line)
        })
        .count()
        .min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

pub(super) fn versioned_update_procedure_formal_command_score(text: &str) -> usize {
    let line_commands =
        text.lines().filter(|line| versioned_update_procedure_line_has_command_start(line)).count();
    let inline_commands = text
        .lines()
        .map(|line| {
            let tokens = shellish_tokens_from_text(line);
            (0..tokens.len())
                .filter(|index| shellish_inline_token_starts_command(&tokens, *index))
                .count()
        })
        .sum::<usize>();
    line_commands.max(inline_commands).min(VERSIONED_UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}

pub(super) fn ordered_step_looks_like_sequential_record(line: &str) -> bool {
    let body = strip_leading_procedure_order_marker(line).trim_start_matches(['-', '*', '•', ' ']);
    body.split_whitespace().next().is_some_and(formal_semver_field)
}

pub(super) fn formal_semver_field(field: &str) -> bool {
    let field =
        field.trim_matches(|character: char| character.is_ascii_punctuation() && character != '.');
    let numeric = field.strip_prefix('v').or_else(|| field.strip_prefix('V')).unwrap_or(field);
    let mut component_count = 0usize;
    let syntax_is_valid = numeric.split('.').all(|component| {
        component_count = component_count.saturating_add(1);
        !component.is_empty() && component.chars().all(|character| character.is_ascii_digit())
    });
    syntax_is_valid && component_count >= 2 && extract_semver_like_version(numeric).is_some()
}

pub(super) fn versioned_update_procedure_ordered_version_transition_score(text: &str) -> usize {
    let count = text
        .lines()
        .filter(|line| {
            !ordered_step_looks_like_sequential_record(line)
                && ordered_step_marker_count(line) > 0
                && ordered_version_transition_line_has_instruction_version(line)
        })
        .count();
    if count >= 2 { count } else { 0 }
}

pub(super) fn ordered_version_transition_line_has_instruction_version(line: &str) -> bool {
    let body = strip_leading_procedure_order_marker(line);
    body.split_whitespace()
        .position(|token| extract_semver_like_version(token).is_some())
        .is_some_and(|position| position >= 2)
}

pub(super) fn ordered_step_marker_count(text: &str) -> usize {
    let chars = text.chars().collect::<Vec<_>>();
    let mut index = 0usize;
    let mut count = 0usize;
    while index < chars.len() {
        if !chars[index].is_ascii_digit() || (index > 0 && !chars[index - 1].is_whitespace()) {
            index = index.saturating_add(1);
            continue;
        }
        let mut end = index.saturating_add(1);
        while end < chars.len() && chars[end].is_ascii_digit() {
            end = end.saturating_add(1);
        }
        if end < chars.len()
            && matches!(chars[end], '.' | ')')
            && chars.get(end.saturating_add(1)).is_some_and(|character| character.is_whitespace())
        {
            count = count.saturating_add(1);
            index = end.saturating_add(1);
            continue;
        }
        index = end;
    }
    count
}

pub(super) fn versioned_update_procedure_text_has_setup_script_signature(text: &str) -> bool {
    let tokens = shellish_tokens_from_text(text);
    let has_external_artifact =
        tokens.iter().any(|token| shellish_token_has_external_artifact(token));
    let has_local_artifact = tokens.iter().any(|token| shellish_token_is_local_artifact(token));
    let has_preparation_signal =
        tokens.iter().any(|token| shellish_token_has_artifact_preparation_signal(token));
    has_external_artifact && has_local_artifact && has_preparation_signal
}

pub(crate) fn versioned_update_exact_target_runbook_score(
    _question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<usize> {
    if !query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return None;
    }
    let focus_model = versioned_update_procedure_focus_model(query_ir)?;
    versioned_update_procedure_chunk_runbook_evidence(chunk, &focus_model)
        .map(|evidence| evidence.score)
}

pub(crate) fn versioned_update_procedure_runbook_anchor_score(
    _question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> Option<usize> {
    versioned_update_exact_target_runbook_score("", query_ir, chunk)
}

pub(crate) fn chunk_is_versioned_update_instruction_title_anchor(
    _question: &str,
    query_ir: &QueryIR,
    chunk: &RuntimeMatchedChunk,
) -> bool {
    let Some(focus_model) = versioned_update_procedure_focus_model(query_ir) else {
        return false;
    };
    query_ir_requests_versioned_update_procedure_context("", query_ir)
        && chunk_is_versioned_update_instruction_title_anchor_for_model(chunk, &focus_model)
}

pub(super) fn chunk_is_versioned_update_instruction_title_anchor_for_model(
    chunk: &RuntimeMatchedChunk,
    focus_model: &VersionedUpdateProcedureFocusModel,
) -> bool {
    versioned_update_procedure_chunk_runbook_evidence(chunk, focus_model)
        .is_some_and(|evidence| evidence.label_has_target_identity)
}

pub(super) fn versioned_update_procedure_focus_terms(
    _question: &str,
    query_ir: Option<&QueryIR>,
    _plan_keywords: &[String],
) -> Vec<String> {
    query_ir
        .and_then(versioned_update_procedure_focus_model)
        .map(|focus_model| focus_model.focus_terms.into_iter().collect())
        .unwrap_or_default()
}
