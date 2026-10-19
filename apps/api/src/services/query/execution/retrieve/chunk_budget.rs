use super::{
    BTreeSet, CONTENT_ANCHOR_CONTEXT_RESERVATION_LIMIT,
    DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT, EXACT_LITERAL_CONTEXT_RESERVATION_LIMIT, HashMap,
    HashSet, QueryAct, QueryIR, QueryScope, QueryTargetKind, RetrievalBundle,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, SETUP_VARIANT_DOCUMENT_CAP, Uuid,
    VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT,
    VERSIONED_UPDATE_PROCEDURE_RESERVED_NEIGHBORS_PER_ANCHOR,
    VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP, chunk_is_setup_focus_command_path_anchor,
    ensure_anchor_chunks_retained, ensure_content_anchor_chunks_retained,
    ensure_exact_literal_context_anchors_retained, ensure_setup_focus_anchor_retained,
    ensure_setup_variant_anchors_retained,
    ensure_versioned_update_procedure_runbook_anchor_retained, extract_explicit_path_literals,
    extract_package_command_literals, extract_parameter_literals, focus_token_overlap_count,
    literal_kind_has_exact_technical_shape, normalized_alnum_tokens,
    query_ir_requests_versioned_update_procedure_context, query_multi_document_relevance_terms,
    query_requests_latest_versions, requested_latest_version_count, score_desc_chunks,
    score_kind_priority, score_value, setup_variant_anchor_family,
    setup_variant_anchor_family_model, source_context_reservation_count,
    technical_literal_focus_keyword_segments, truncate_chunks_for_multi_document_scope,
    truncate_chunks_with_source_context_reservation, versioned_update_exact_target_runbook_score,
    versioned_update_procedure_chunk_runbook_evidence, versioned_update_procedure_focus_model,
};

#[cfg(test)]
pub(crate) fn truncate_bundle(
    bundle: &mut RetrievalBundle,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
) {
    truncate_bundle_with_semantic_chunk_ranks(
        bundle,
        top_k,
        query_ir,
        demoted_document_ids,
        &HashMap::new(),
    );
}

pub(crate) fn truncate_bundle_with_semantic_chunk_ranks(
    bundle: &mut RetrievalBundle,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
    semantic_chunk_ranks: &HashMap<Uuid, usize>,
) {
    bundle.entities.truncate(entity_context_top_k(top_k, query_ir));
    bundle.relationships.truncate(top_k);
    truncate_chunks_for_context_with_semantic_ranks(
        &mut bundle.chunks,
        top_k,
        query_ir,
        demoted_document_ids,
        semantic_chunk_ranks,
    );
}

pub(super) fn semantic_chunk_rank_order(
    left: &RuntimeMatchedChunk,
    right: &RuntimeMatchedChunk,
    semantic_chunk_ranks: &HashMap<Uuid, usize>,
) -> std::cmp::Ordering {
    match (semantic_chunk_ranks.get(&left.chunk_id), semantic_chunk_ranks.get(&right.chunk_id)) {
        (Some(left_rank), Some(right_rank)) => left_rank.cmp(right_rank),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

pub(super) fn entity_context_top_k(top_k: usize, query_ir: Option<&QueryIR>) -> usize {
    let Some(query_ir) = query_ir else {
        return top_k;
    };
    if matches!(query_ir.act, QueryAct::Enumerate | QueryAct::Meta)
        && (query_ir.scope == QueryScope::LibraryMeta || !query_ir.target_entities.is_empty())
    {
        return top_k.saturating_mul(3).clamp(top_k, 96);
    }
    top_k
}

#[cfg(test)]
pub(super) fn truncate_chunks_for_context(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
) {
    truncate_chunks_for_context_with_semantic_ranks(
        chunks,
        top_k,
        query_ir,
        demoted_document_ids,
        &HashMap::new(),
    );
}

pub(super) fn truncate_chunks_for_context_with_semantic_ranks(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    top_k: usize,
    query_ir: Option<&QueryIR>,
    demoted_document_ids: &HashSet<Uuid>,
    semantic_chunk_ranks: &HashMap<Uuid, usize>,
) {
    if chunks.len() <= top_k {
        return;
    }
    let mut indexed = std::mem::take(chunks).into_iter().enumerate().collect::<Vec<_>>();
    // Attached-context documents (image attachments collapsed onto their parent
    // page) sink below every peer/primary chunk so they fill only the residual
    // budget after real content — they are searchable context, not competing
    // documents. The demotion is the FIRST sort key so a flood of one-chunk
    // image siblings can never displace the parent's procedural/graph evidence.
    // The set already excludes any document the query explicitly targets, so an
    // exact "show screenshot X" ask is not demoted.
    indexed.sort_by(|(left_index, left), (right_index, right)| {
        demoted_document_ids
            .contains(&left.document_id)
            .cmp(&demoted_document_ids.contains(&right.document_id))
            .then_with(|| {
                score_kind_priority(right.score_kind).cmp(&score_kind_priority(left.score_kind))
            })
            // Semantic rank is a selection signal, not a replacement score.
            // Configured demotion and protected evidence lanes remain stronger;
            // within one lane, the scored provider prefix precedes raw-score
            // fallback while every provenance score stays untouched.
            .then_with(|| semantic_chunk_rank_order(left, right, semantic_chunk_ranks))
            .then_with(|| score_desc_chunks(left, right))
            .then_with(|| left_index.cmp(right_index))
    });
    let reserved_document_evidence =
        reserved_document_evidence_anchor_chunks(&indexed, top_k, query_ir);
    let reserved_content_anchor_chunks = reserved_content_anchor_chunks(&indexed, top_k);
    // Reserve the focused-document setup anchor (the chunk carrying both a
    // command-object literal and a configuration path) before the score-ordered
    // truncation runs, so a confident single-document configure/how-to answer
    // never loses the "what to install" line to denser parameter chunks.
    let reserved_anchor = query_ir.and_then(|ir| best_setup_focus_anchor_chunk(&indexed, ir));
    let reserved_versioned_update_runbook_anchor =
        query_ir.and_then(|ir| best_versioned_update_procedure_runbook_anchor_chunk(&indexed, ir));
    let reserved_setup_variant_anchors =
        query_ir.map(|ir| setup_variant_anchor_chunks(&indexed, top_k, ir)).unwrap_or_default();
    let reserved_exact_literal_context_anchors = query_ir
        .map(|ir| reserved_exact_literal_context_chunks(&indexed, top_k, ir))
        .unwrap_or_default();
    let mut selected = if let Some(query_ir) = query_ir {
        if query_requests_latest_versions(query_ir) {
            truncate_chunks_with_latest_version_reservation(&indexed, top_k, query_ir)
        } else if matches!(query_ir.scope, QueryScope::MultiDocument) {
            truncate_chunks_for_multi_document_scope(&indexed, top_k, query_ir)
        } else if let Some(reserved_count) =
            source_context_reservation_count(&indexed, top_k, query_ir)
        {
            truncate_chunks_with_source_context_reservation(&indexed, top_k, reserved_count)
        } else {
            indexed.iter().take(top_k).cloned().collect::<Vec<_>>()
        }
    } else {
        indexed.iter().take(top_k).cloned().collect::<Vec<_>>()
    };
    ensure_document_evidence_anchors_retained(&mut selected, reserved_document_evidence, top_k);
    if let Some(anchor) = reserved_anchor {
        ensure_setup_focus_anchor_retained(&mut selected, anchor, top_k);
    }
    ensure_setup_variant_anchors_retained(&mut selected, reserved_setup_variant_anchors, top_k);
    ensure_exact_literal_context_anchors_retained(
        &mut selected,
        reserved_exact_literal_context_anchors,
        top_k,
    );
    ensure_content_anchor_chunks_retained(&mut selected, reserved_content_anchor_chunks, top_k);
    if let Some(anchor) = reserved_versioned_update_runbook_anchor {
        ensure_versioned_update_procedure_runbook_anchor_retained(&mut selected, anchor, top_k);
    }
    *chunks = selected.into_iter().map(|(_, chunk)| chunk).collect();
}

pub(super) fn reserved_content_anchor_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2 {
        return Vec::new();
    }
    indexed
        .iter()
        .filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::ContentAnchor)
        .take(CONTENT_ANCHOR_CONTEXT_RESERVATION_LIMIT.min(top_k.saturating_sub(1)))
        .cloned()
        .collect()
}

pub(super) fn truncate_chunks_with_latest_version_reservation(
    chunks: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k == 0 {
        return Vec::new();
    }
    let latest_limit = requested_latest_version_count(query_ir).min(top_k);
    let mut selected = Vec::with_capacity(top_k);
    let mut selected_indices = HashSet::new();
    for (index, chunk) in
        chunks.iter().filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::LatestVersion)
    {
        if selected.len() >= latest_limit {
            break;
        }
        selected_indices.insert(*index);
        selected.push((*index, chunk.clone()));
    }
    for (index, chunk) in chunks {
        if selected.len() >= top_k {
            break;
        }
        if selected_indices.insert(*index) {
            selected.push((*index, chunk.clone()));
        }
    }
    selected
}

pub(super) fn reserved_document_evidence_anchor_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: Option<&QueryIR>,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2
        || query_ir.is_some_and(|ir| {
            matches!(ir.scope, QueryScope::MultiDocument | QueryScope::LibraryMeta)
        })
    {
        return Vec::new();
    }
    if let Some(query_ir) = query_ir
        && query_ir_requests_versioned_update_procedure_context("", query_ir)
    {
        let limit = VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT
            .min(top_k.saturating_sub(1));
        let mut selected = reserved_versioned_update_procedure_chunks(indexed, top_k, query_ir);
        if selected.len() >= limit {
            return selected;
        }
        let seen = selected.iter().map(|(_, chunk)| chunk.chunk_id).collect::<BTreeSet<_>>();
        for anchor in indexed
            .iter()
            .filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument)
        {
            if seen.contains(&anchor.1.chunk_id) {
                continue;
            }
            selected.push(anchor.clone());
            if selected.len() >= limit {
                break;
            }
        }
        return selected;
    }
    indexed
        .iter()
        .filter(|(_, chunk)| chunk.score_kind == RuntimeChunkScoreKind::FocusedDocument)
        .take(DOCUMENT_EVIDENCE_CONTEXT_RESERVATION_LIMIT.min(top_k.saturating_sub(1)))
        .cloned()
        .collect()
}

pub(super) fn reserved_exact_literal_context_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2 || !query_ir_requests_exact_literal_context_reservation(query_ir) {
        return Vec::new();
    }
    let focus_terms = exact_literal_context_focus_terms(query_ir);
    if focus_terms.is_empty() {
        return Vec::new();
    }

    let limit = EXACT_LITERAL_CONTEXT_RESERVATION_LIMIT.min(top_k.saturating_sub(1));
    let mut scored = indexed
        .iter()
        .filter_map(|(index, chunk)| {
            let score = exact_literal_context_chunk_score(chunk, &focus_terms)?;
            Some((score, score_value(chunk.score), *index, chunk.clone()))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.total_cmp(&left.1))
            .then_with(|| left.2.cmp(&right.2))
    });

    let mut selected = Vec::<(usize, RuntimeMatchedChunk)>::new();
    let mut seen_chunk_ids = HashSet::<Uuid>::new();
    let mut seen_document_ids = HashSet::<Uuid>::new();
    for (_, _, index, chunk) in &scored {
        if selected.len() >= limit {
            return selected;
        }
        if seen_document_ids.insert(chunk.document_id) && seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((*index, chunk.clone()));
        }
    }
    for (_, _, index, chunk) in scored {
        if selected.len() >= limit {
            break;
        }
        if seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((index, chunk));
        }
    }
    selected
}

pub(super) fn query_ir_requests_exact_literal_context_reservation(query_ir: &QueryIR) -> bool {
    if query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return false;
    }
    if matches!(query_ir.act, QueryAct::Compare) && query_ir.comparison.is_some() {
        return true;
    }
    if query_ir.is_exact_literal_technical() || query_ir.has_exact_technical_literal() {
        return true;
    }
    query_ir.targets_any(&[
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Connection,
        QueryTargetKind::Endpoint,
        QueryTargetKind::FilesystemPath,
        QueryTargetKind::HttpMethod,
        QueryTargetKind::Package,
        QueryTargetKind::Parameter,
        QueryTargetKind::Path,
        QueryTargetKind::Port,
        QueryTargetKind::Protocol,
        QueryTargetKind::Url,
        QueryTargetKind::Wsdl,
    ]) || query_ir
        .literal_constraints
        .iter()
        .any(|literal| literal_kind_has_exact_technical_shape(literal.kind, &literal.text))
}

pub(super) fn exact_literal_context_focus_terms(query_ir: &QueryIR) -> Vec<String> {
    let mut terms = BTreeSet::<String>::new();
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        for segment in technical_literal_focus_keyword_segments(retrieval_query, Some(query_ir)) {
            terms.extend(segment);
        }
        terms.extend(normalized_alnum_tokens(retrieval_query, 3));
    }
    terms.extend(query_multi_document_relevance_terms(query_ir));
    terms.into_iter().collect()
}

pub(super) fn exact_literal_context_chunk_score(
    chunk: &RuntimeMatchedChunk,
    focus_terms: &[String],
) -> Option<usize> {
    let literal_inventory = structural_literal_inventory_score(chunk);
    if literal_inventory == 0 {
        return None;
    }
    let haystack =
        format!("{} {} {}", chunk.document_label, chunk.excerpt, chunk.source_text).to_lowercase();
    let focus_score = focus_terms.iter().filter(|term| haystack.contains(term.as_str())).count();
    if focus_score == 0 {
        return None;
    }
    Some(literal_inventory.saturating_mul(128).saturating_add(focus_score.saturating_mul(16)))
}

pub(super) fn structural_literal_inventory_score(chunk: &RuntimeMatchedChunk) -> usize {
    let text = format!("{} {}", chunk.excerpt, chunk.source_text);
    extract_parameter_literals(&text, 32).len().saturating_mul(4)
        + extract_explicit_path_literals(&text, 8).len().saturating_mul(3)
        + extract_package_command_literals(&text, 4).len().saturating_mul(3)
}

pub(super) fn append_reserved_procedure_neighbors(
    indexed: &[(usize, RuntimeMatchedChunk)],
    anchor: &RuntimeMatchedChunk,
    limit: usize,
    seen_chunk_ids: &mut HashSet<Uuid>,
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
) {
    let neighbors = indexed.iter().filter(|(_, candidate)| {
        versioned_update_procedure_reservation_candidate_kind(candidate)
            && candidate.document_id == anchor.document_id
            && candidate.revision_id == anchor.revision_id
            && candidate.chunk_index > anchor.chunk_index
    });
    for (neighbor_index, neighbor) in
        neighbors.take(VERSIONED_UPDATE_PROCEDURE_RESERVED_NEIGHBORS_PER_ANCHOR)
    {
        if selected.len() >= limit {
            break;
        }
        if seen_chunk_ids.insert(neighbor.chunk_id) {
            selected.push((*neighbor_index, neighbor.clone()));
        }
    }
}

pub(super) fn reserved_versioned_update_procedure_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2 || !query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return Vec::new();
    }
    let Some(focus_model) = versioned_update_procedure_focus_model(query_ir) else {
        return Vec::new();
    };
    let limit =
        VERSIONED_UPDATE_PROCEDURE_DOCUMENT_EVIDENCE_RESERVATION_LIMIT.min(top_k.saturating_sub(1));
    let mut scored = indexed
        .iter()
        .filter(|(_, chunk)| versioned_update_procedure_reservation_candidate_kind(chunk))
        .filter_map(|(index, chunk)| {
            let evidence = versioned_update_procedure_chunk_runbook_evidence(chunk, &focus_model)?;
            Some((
                evidence.label_has_target_identity,
                evidence.score,
                evidence.ordered_step_score,
                evidence.command_score,
                evidence.version_transition_score,
                *index,
                chunk.clone(),
            ))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| right.1.cmp(&left.1))
            .then_with(|| right.2.cmp(&left.2))
            .then_with(|| right.3.cmp(&left.3))
            .then_with(|| right.4.cmp(&left.4))
            .then_with(|| score_desc_chunks(&left.6, &right.6))
            .then_with(|| left.5.cmp(&right.5))
    });

    let mut selected = Vec::<(usize, RuntimeMatchedChunk)>::new();
    let mut seen_chunk_ids = HashSet::<Uuid>::new();
    let mut seen_document_ids = HashSet::<Uuid>::new();
    for (_, _, _, _, _, index, chunk) in &scored {
        if selected.len() >= VERSIONED_UPDATE_PROCEDURE_SUBJECT_TITLE_RESERVE_CAP.min(limit) {
            break;
        }
        if seen_document_ids.insert(chunk.document_id) && seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((*index, chunk.clone()));
        }
    }
    for (_, _, _, _, _, index, chunk) in scored {
        if selected.len() >= limit {
            break;
        }
        if seen_chunk_ids.insert(chunk.chunk_id) {
            selected.push((index, chunk.clone()));
        }
        append_reserved_procedure_neighbors(
            indexed,
            &chunk,
            limit,
            &mut seen_chunk_ids,
            &mut selected,
        );
    }
    selected
}

pub(super) fn versioned_update_procedure_reservation_candidate_kind(
    chunk: &RuntimeMatchedChunk,
) -> bool {
    matches!(
        chunk.score_kind,
        RuntimeChunkScoreKind::FocusedDocument
            | RuntimeChunkScoreKind::DocumentIdentity
            | RuntimeChunkScoreKind::SourceContext
    )
}

pub(super) fn ensure_document_evidence_anchors_retained(
    selected: &mut Vec<(usize, RuntimeMatchedChunk)>,
    anchors: Vec<(usize, RuntimeMatchedChunk)>,
    top_k: usize,
) {
    ensure_anchor_chunks_retained(
        selected,
        anchors,
        top_k,
        |selected, protected_anchor_ids, anchor| {
            selected
                .iter()
                .rposition(|(_, chunk)| {
                    !protected_anchor_ids.contains(&chunk.chunk_id)
                        && !matches!(
                            chunk.score_kind,
                            RuntimeChunkScoreKind::FocusedDocument
                                | RuntimeChunkScoreKind::SourceContext
                        )
                        && !chunk_is_setup_focus_command_path_anchor(chunk)
                })
                .or_else(|| {
                    selected.iter().rposition(|(_, chunk)| {
                        !protected_anchor_ids.contains(&chunk.chunk_id)
                            && chunk.score_kind != RuntimeChunkScoreKind::FocusedDocument
                    })
                })
                .or_else(|| {
                    selected.iter().rposition(|(_, chunk)| {
                        !protected_anchor_ids.contains(&chunk.chunk_id)
                            && chunk.document_id != anchor.1.document_id
                    })
                })
        },
        |anchor_chunk_id| {
            tracing::info!(
                stage = "retrieval.document_evidence_anchor_reserved",
                chunk_id = %anchor_chunk_id,
                "document evidence anchor reserved past score-ordered truncation"
            );
        },
    );
}

/// Pick the highest-scoring focused-document setup anchor among the candidate
/// chunks for a confident single-document configure/how-to query. An anchor is a
/// chunk that carries both a command-object literal and a configuration path
/// and comes from a document whose label overlaps the query's `document_focus`.
pub(super) fn best_setup_focus_anchor_chunk(
    indexed: &[(usize, RuntimeMatchedChunk)],
    query_ir: &QueryIR,
) -> Option<(usize, RuntimeMatchedChunk)> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow)
        || query_ir.document_focus.is_none()
        || query_ir_requests_versioned_update_procedure_context("", query_ir)
    {
        return None;
    }
    let focus_tokens = setup_focus_anchor_query_tokens(query_ir);
    indexed
        .iter()
        .filter(|(_, chunk)| chunk_is_setup_focus_command_path_anchor(chunk))
        .filter(|(_, chunk)| {
            if focus_tokens.is_empty() {
                return true;
            }
            let label_tokens = normalized_alnum_tokens(&chunk.document_label, 3);
            focus_token_overlap_count(&focus_tokens, &label_tokens) > 0
        })
        .max_by(|(_, left), (_, right)| score_desc_chunks(right, left))
        .map(|(index, chunk)| (*index, chunk.clone()))
}

pub(super) fn best_versioned_update_procedure_runbook_anchor_chunk(
    indexed: &[(usize, RuntimeMatchedChunk)],
    query_ir: &QueryIR,
) -> Option<(usize, RuntimeMatchedChunk)> {
    if !query_ir_requests_versioned_update_procedure_context("", query_ir) {
        return None;
    }
    indexed
        .iter()
        .filter_map(|(index, chunk)| {
            let runbook_score = versioned_update_exact_target_runbook_score("", query_ir, chunk)?;
            Some((runbook_score, score_value(chunk.score), *index, chunk))
        })
        .max_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.total_cmp(&right.1))
                .then_with(|| right.2.cmp(&left.2))
        })
        .map(|(runbook_score, score, index, chunk)| {
            tracing::debug!(
                stage = "retrieval.versioned_update_procedure_runbook_anchor_selected",
                chunk_id = %chunk.chunk_id,
                document_id = %chunk.document_id,
                runbook_score,
                score,
                index,
                "versioned update runbook anchor selected for retention"
            );
            (index, chunk.clone())
        })
}

pub(super) fn setup_focus_anchor_query_tokens(query_ir: &QueryIR) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
        tokens.extend(normalized_alnum_tokens(retrieval_query, 3));
    }
    for entity in &query_ir.target_entities {
        tokens.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    tokens
}

pub(super) fn setup_variant_anchor_chunks(
    indexed: &[(usize, RuntimeMatchedChunk)],
    top_k: usize,
    query_ir: &QueryIR,
) -> Vec<(usize, RuntimeMatchedChunk)> {
    if top_k < 2 || !query_ir_requests_setup_variant_anchor_reservation(query_ir) {
        return Vec::new();
    }
    let query_terms = setup_focus_anchor_query_tokens(query_ir);
    let anchor_labels = indexed
        .iter()
        .filter(|(_, chunk)| chunk_is_setup_focus_command_path_anchor(chunk))
        .map(|(_, chunk)| chunk.document_label.as_str())
        .collect::<Vec<_>>();
    if anchor_labels.len() < 2 {
        return Vec::new();
    }
    let family_model = setup_variant_anchor_family_model(&anchor_labels, &query_terms);
    let mut selected = Vec::new();
    let mut seen_families = BTreeSet::<String>::new();
    let reserve_limit = SETUP_VARIANT_DOCUMENT_CAP.min(top_k.saturating_sub(1));
    for (index, chunk) in
        indexed.iter().filter(|(_, chunk)| chunk_is_setup_focus_command_path_anchor(chunk))
    {
        let family =
            setup_variant_anchor_family(&chunk.document_label, &query_terms, &family_model);
        if seen_families.insert(family) {
            selected.push((*index, chunk.clone()));
            if selected.len() >= reserve_limit {
                break;
            }
        }
    }
    if selected.len() < 2 { Vec::new() } else { selected }
}

pub(super) fn query_ir_requests_setup_variant_anchor_reservation(query_ir: &QueryIR) -> bool {
    matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe)
        && query_ir.document_focus.is_some()
        && query_ir.source_slice.is_none()
        && query_ir.literal_constraints.is_empty()
        && !query_ir_requests_versioned_update_procedure_context("", query_ir)
}
