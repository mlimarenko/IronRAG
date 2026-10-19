use super::{
    BTreeMap, BTreeSet, HashMap, HashSet, RuntimeMatchedChunk, UpdateProcedureFocusModel, Uuid,
    line_has_command_signal, repair_technical_layout_noise, strip_leading_order_marker,
    trim_command_boundary_token_decorations, update_procedure_chunk_text,
    update_procedure_command_head, update_procedure_command_heads_match,
    update_procedure_evidence_anchors, update_procedure_extract_is_typed_evidence,
    update_procedure_extracts_from_text, update_procedure_focused_structural_score,
    update_procedure_line_blocks, update_procedure_step_from_line,
    update_procedure_step_is_structural, update_procedure_text_focus_score,
    update_procedure_text_has_bound_target_identity_runbook,
    update_procedure_text_target_identity_priority,
};

const UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP: usize = 8;

#[derive(Debug, Clone)]
pub(super) struct UpdateProcedureSelection {
    pub(super) source: String,
    pub(super) steps: Vec<String>,
    anchors: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct UpdateProcedureExtract {
    pub(super) block_index: usize,
    pub(super) score: usize,
    pub(super) steps: Vec<String>,
    pub(super) block_text: String,
    pub(super) command_count: usize,
    pub(super) preparatory_command_score: usize,
    pub(super) focus_aligned_command_score: usize,
    pub(super) unfocused_command_score: usize,
    pub(super) has_setup_script_signature: bool,
    pub(super) is_focus_projection: bool,
}

#[derive(Debug, Clone)]
struct UpdateProcedureCandidate {
    label_target_identity: bool,
    target_identity_priority: usize,
    target_identity_focus_score: usize,
    score: usize,
    command_count: usize,
    focused_structural_score: usize,
    selection: UpdateProcedureSelection,
}

pub(super) fn update_procedure_steps_from_chunks(
    focus_model: &UpdateProcedureFocusModel,
    chunks: &[RuntimeMatchedChunk],
) -> Option<UpdateProcedureSelection> {
    let document_bindings = update_procedure_document_bindings(focus_model, chunks);
    let mut candidates = chunks
        .iter()
        .flat_map(|chunk| {
            update_procedure_candidates_from_chunk(
                focus_model,
                chunk,
                document_bindings.get(&chunk.document_id).copied().unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    select_update_procedure_candidate(focus_model, &mut candidates)
}

fn update_procedure_candidates_from_chunk(
    focus_model: &UpdateProcedureFocusModel,
    chunk: &RuntimeMatchedChunk,
    document_binding: UpdateProcedureDocumentBinding,
) -> Vec<UpdateProcedureCandidate> {
    let label_target_identity_priority =
        update_procedure_text_target_identity_priority(&chunk.document_label, focus_model);
    let text = repair_technical_layout_noise(&update_procedure_chunk_text(chunk, focus_model));
    let raw_extracts = update_procedure_extracts_from_text(&text, focus_model);
    let mut extracts = raw_extracts
        .iter()
        .filter(|extract| {
            update_procedure_extract_is_typed_evidence(extract, focus_model, &chunk.document_label)
        })
        .cloned()
        .collect::<Vec<_>>();
    if let Some(aggregate) = update_procedure_focus_aligned_command_aggregate(
        &raw_extracts,
        focus_model,
        &chunk.document_label,
    )
    .filter(|extract| {
        update_procedure_extract_is_typed_evidence(extract, focus_model, &chunk.document_label)
    }) {
        extracts.push(aggregate);
    }
    let has_richer_extract = extracts.iter().any(|extract| extract.steps.len() >= 4);
    extracts
        .into_iter()
        .filter(|extract| !has_richer_extract || extract.steps.len() >= 4)
        .filter_map(|extract| {
            update_procedure_candidate_from_extract(
                focus_model,
                chunk,
                document_binding,
                label_target_identity_priority,
                extract,
            )
        })
        .collect()
}

fn update_procedure_candidate_from_extract(
    focus_model: &UpdateProcedureFocusModel,
    chunk: &RuntimeMatchedChunk,
    document_binding: UpdateProcedureDocumentBinding,
    label_target_identity_priority: usize,
    extract: UpdateProcedureExtract,
) -> Option<UpdateProcedureCandidate> {
    let label_focus_score = update_procedure_text_focus_score(&chunk.document_label, focus_model);
    let block_focus_score = update_procedure_text_focus_score(&extract.block_text, focus_model);
    let focused_structural_score =
        update_procedure_focused_structural_score(&extract.block_text, focus_model);
    let raw_block_target_identity_priority =
        update_procedure_text_target_identity_priority(&extract.block_text, focus_model);
    let block_target_identity_is_bound = raw_block_target_identity_priority > 0
        && (document_binding.strong_body_target_binding
            || (label_target_identity_priority > 0
                && update_procedure_text_has_bound_target_identity_runbook(
                    &extract.block_text,
                    focus_model,
                )));
    if label_target_identity_priority == 0
        && raw_block_target_identity_priority > 0
        && !block_target_identity_is_bound
    {
        return None;
    }
    let block_target_identity_priority =
        raw_block_target_identity_priority * usize::from(block_target_identity_is_bound);
    let target_identity_priority =
        label_target_identity_priority.max(block_target_identity_priority);
    if target_identity_priority == 0 {
        return None;
    }
    let target_identity_focus_score =
        if label_target_identity_priority > 0 { label_focus_score } else { block_focus_score };
    let score = update_procedure_candidate_score(
        &extract,
        label_focus_score,
        target_identity_focus_score,
        focused_structural_score,
    );
    let command_count = extract.command_count;
    let steps = update_procedure_steps_with_adjacent_same_head_preparation(
        extract.steps,
        &extract.block_text,
    );
    let anchors = update_procedure_evidence_anchors(&steps, &extract.block_text, 8);
    Some(UpdateProcedureCandidate {
        label_target_identity: label_target_identity_priority > 0,
        target_identity_priority,
        target_identity_focus_score,
        score,
        command_count,
        focused_structural_score,
        selection: UpdateProcedureSelection {
            source: chunk.document_label.clone(),
            steps,
            anchors,
        },
    })
}

fn update_procedure_candidate_score(
    extract: &UpdateProcedureExtract,
    label_focus_score: usize,
    target_identity_focus_score: usize,
    focused_structural_score: usize,
) -> usize {
    let exact_target_identity_bonus =
        if extract.steps.len() >= 4 && extract.command_count >= 2 { 32768 } else { 8192 };
    let unfocused_command_penalty =
        if extract.focus_aligned_command_score > 0 { 4096 } else { 256 };
    extract
        .score
        .saturating_add(
            usize::from(extract.is_focus_projection && extract.command_count >= 2)
                .saturating_mul(65_536),
        )
        .saturating_add(exact_target_identity_bonus)
        .saturating_add(label_focus_score.saturating_mul(96))
        .saturating_add(target_identity_focus_score.saturating_mul(1024))
        .saturating_add(focused_structural_score.saturating_mul(160))
        .saturating_add(extract.steps.len().saturating_mul(24))
        .saturating_add(extract.command_count.saturating_mul(512))
        .saturating_add(update_procedure_command_candidate_bonus(extract.command_count))
        .saturating_add(extract.preparatory_command_score.saturating_mul(512))
        .saturating_add(extract.focus_aligned_command_score.saturating_mul(4096))
        .saturating_sub(extract.unfocused_command_score.saturating_mul(unfocused_command_penalty))
}

fn select_update_procedure_candidate(
    focus_model: &UpdateProcedureFocusModel,
    candidates: &mut Vec<UpdateProcedureCandidate>,
) -> Option<UpdateProcedureSelection> {
    if focus_model.requires_exact_document_subject
        && !candidates.iter().any(update_procedure_candidate_has_target_identity_preference)
    {
        return None;
    }
    retain_preferred_update_procedure_candidates(candidates);
    candidates
        .drain(..)
        .max_by(compare_update_procedure_candidates)
        .map(|candidate| candidate.selection)
}

fn retain_preferred_update_procedure_candidates(candidates: &mut Vec<UpdateProcedureCandidate>) {
    let preferred_priority = candidates
        .iter()
        .filter(|candidate| update_procedure_candidate_has_target_identity_preference(candidate))
        .map(|candidate| candidate.target_identity_priority)
        .max()
        .unwrap_or_default();
    if preferred_priority > 0 {
        candidates.retain(|candidate| {
            candidate.target_identity_priority == preferred_priority
                && update_procedure_candidate_has_target_identity_preference(candidate)
        });
    }
    if candidates.iter().any(|candidate| {
        candidate.label_target_identity
            && update_procedure_candidate_has_target_identity_preference(candidate)
    }) {
        candidates.retain(|candidate| {
            candidate.label_target_identity
                && update_procedure_candidate_has_target_identity_preference(candidate)
        });
    }
    if candidates.iter().any(update_procedure_candidate_has_structural_command_preference) {
        candidates.retain(update_procedure_candidate_has_structural_command_preference);
    }
}

fn compare_update_procedure_candidates(
    left: &UpdateProcedureCandidate,
    right: &UpdateProcedureCandidate,
) -> std::cmp::Ordering {
    left.score
        .cmp(&right.score)
        .then_with(|| left.target_identity_priority.cmp(&right.target_identity_priority))
        .then_with(|| left.target_identity_focus_score.cmp(&right.target_identity_focus_score))
        .then_with(|| left.focused_structural_score.cmp(&right.focused_structural_score))
        .then_with(|| left.selection.steps.len().cmp(&right.selection.steps.len()))
        .then_with(|| left.command_count.cmp(&right.command_count))
        .then_with(|| right.selection.source.cmp(&left.selection.source))
        .then_with(|| right.selection.anchors.cmp(&left.selection.anchors))
}

#[derive(Debug, Clone, Copy, Default)]
struct UpdateProcedureDocumentBinding {
    strong_body_target_binding: bool,
}

fn update_procedure_document_bindings(
    focus_model: &UpdateProcedureFocusModel,
    chunks: &[RuntimeMatchedChunk],
) -> HashMap<Uuid, UpdateProcedureDocumentBinding> {
    let mut chunks_by_document = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();
    for chunk in chunks {
        chunks_by_document.entry(chunk.document_id).or_default().push(chunk);
    }
    chunks_by_document
        .into_iter()
        .map(|(document_id, mut document_chunks)| {
            document_chunks.sort_by(|left, right| {
                left.chunk_index
                    .cmp(&right.chunk_index)
                    .then_with(|| left.chunk_id.cmp(&right.chunk_id))
            });
            let mut seen_fragments = HashSet::<String>::new();
            let mut body_fragments = Vec::<&str>::new();
            for chunk in &document_chunks {
                for fragment in [chunk.source_text.as_str(), chunk.excerpt.as_str()] {
                    let fragment = fragment.trim();
                    if !fragment.is_empty() && seen_fragments.insert(fragment.to_string()) {
                        body_fragments.push(fragment);
                    }
                }
            }
            let body = repair_technical_layout_noise(&body_fragments.join("\n"));
            let strong_body_target_binding =
                update_procedure_text_has_strong_target_procedure_binding(&body, focus_model);
            (document_id, UpdateProcedureDocumentBinding { strong_body_target_binding })
        })
        .collect()
}

fn update_procedure_text_has_strong_target_procedure_binding(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    if focus_model.target_identity_sequences.is_empty() {
        return false;
    }
    let lines = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    let distinct_target_steps = lines
        .iter()
        .filter(|line| {
            update_procedure_text_target_identity_priority(line, focus_model) > 0
                && update_procedure_step_is_structural(line)
        })
        .map(|line| update_procedure_normalized_step(line).to_lowercase())
        .collect::<BTreeSet<_>>();
    if distinct_target_steps.len() >= 2 {
        return true;
    }

    lines.iter().enumerate().any(|(heading_index, heading)| {
        if strip_leading_order_marker(heading).trim() != *heading
            || line_has_command_signal(heading)
            || update_procedure_text_target_identity_priority(heading, focus_model) == 0
        {
            return false;
        }
        lines
            .iter()
            .skip(heading_index + 1)
            .take(8)
            .filter(|line| update_procedure_step_is_structural(line))
            .take(2)
            .count()
            >= 2
    })
}

fn update_procedure_steps_with_adjacent_same_head_preparation(
    steps: Vec<String>,
    block_text: &str,
) -> Vec<String> {
    let Some(first_step) = steps.first() else {
        return steps;
    };
    let first_step_key = update_procedure_step_key(first_step);
    let block_lines = update_procedure_block_structural_lines(block_text);
    let Some(first_index) =
        block_lines.iter().position(|line| update_procedure_step_key(line) == first_step_key)
    else {
        if let Some(previous) =
            update_procedure_previous_same_head_command_from_dense_text(first_step, block_text)
        {
            return update_procedure_prepend_step_if_absent(steps, previous);
        }
        return steps;
    };
    if first_index == 0 {
        if let Some(previous) =
            update_procedure_previous_same_head_command_from_dense_text(first_step, block_text)
        {
            return update_procedure_prepend_step_if_absent(steps, previous);
        }
        return steps;
    }
    let previous = &block_lines[first_index - 1];
    if !line_has_command_signal(previous)
        || !update_procedure_command_heads_match(previous, first_step)
        || steps
            .iter()
            .any(|step| update_procedure_step_key(step) == update_procedure_step_key(previous))
    {
        return steps;
    }
    update_procedure_prepend_step_if_absent(steps, update_procedure_normalized_step(previous))
}

fn update_procedure_prepend_step_if_absent(steps: Vec<String>, previous: String) -> Vec<String> {
    if steps
        .iter()
        .any(|step| update_procedure_step_key(step) == update_procedure_step_key(&previous))
    {
        return steps;
    }
    let mut augmented = Vec::with_capacity(steps.len().saturating_add(1));
    augmented.push(previous);
    augmented.extend(steps);
    augmented
}

fn update_procedure_previous_same_head_command_from_dense_text(
    first_step: &str,
    block_text: &str,
) -> Option<String> {
    let first_key = update_procedure_step_key(first_step);
    let head = update_procedure_command_head(first_step)?;
    let normalized = block_text.split_whitespace().collect::<Vec<_>>().join(" ");
    let lower = normalized.to_lowercase();
    let first_pos = lower.find(&first_key)?;
    let prefix = normalized.get(..first_pos)?.trim();
    if prefix.is_empty() {
        return None;
    }
    let tokens = prefix.split_whitespace().collect::<Vec<_>>();
    for index in (0..tokens.len()).rev() {
        let token = trim_command_boundary_token_decorations(tokens[index]).to_ascii_lowercase();
        if token != head {
            continue;
        }
        let candidate = update_procedure_normalized_step(&tokens[index..].join(" "));
        if !candidate.is_empty()
            && candidate != first_key
            && line_has_command_signal(&candidate)
            && update_procedure_command_heads_match(&candidate, first_step)
        {
            return Some(candidate);
        }
    }
    None
}

fn update_procedure_block_structural_lines(block_text: &str) -> Vec<String> {
    update_procedure_line_blocks(block_text)
        .into_iter()
        .flatten()
        .filter(|line| line.has_command || line.has_order_marker || line.has_version)
        .map(|line| update_procedure_step_from_line(&line))
        .collect()
}

fn update_procedure_step_key(step: &str) -> String {
    update_procedure_normalized_step(step).to_lowercase()
}

pub(super) fn update_procedure_normalized_step(step: &str) -> String {
    strip_leading_order_marker(step)
        .trim()
        .trim_matches('`')
        .trim_end_matches(['.', ',', ';'])
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn update_procedure_candidate_has_target_identity_preference(
    candidate: &UpdateProcedureCandidate,
) -> bool {
    candidate.command_count >= 2
        || candidate.selection.steps.len() >= 3
        || candidate.focused_structural_score > 0
}

fn update_procedure_candidate_has_structural_command_preference(
    candidate: &UpdateProcedureCandidate,
) -> bool {
    candidate.command_count >= 2
        && (candidate.label_target_identity
            || candidate.target_identity_focus_score > 0
            || candidate.focused_structural_score > 0)
}

fn update_procedure_focus_aligned_command_aggregate(
    extracts: &[UpdateProcedureExtract],
    focus_model: &UpdateProcedureFocusModel,
    document_label: &str,
) -> Option<UpdateProcedureExtract> {
    let label_focus_score = update_procedure_text_focus_score(document_label, focus_model);
    let body_focus_score = extracts
        .iter()
        .map(|extract| update_procedure_text_focus_score(&extract.block_text, focus_model))
        .max()
        .unwrap_or_default();
    let aggregate_focus_score = label_focus_score.max(body_focus_score);
    if aggregate_focus_score == 0 {
        return None;
    }
    let selected = selected_update_procedure_extracts(extracts, focus_model, document_label);
    if selected.len() < 2 {
        return None;
    }
    let steps = unique_update_procedure_steps(&selected, 16);
    if steps.iter().filter(|step| update_procedure_step_is_structural(step)).count() < 2 {
        return None;
    }

    let score =
        selected.iter().fold(aggregate_focus_score.saturating_mul(256), |score, extract| {
            score.saturating_add(extract.score).saturating_add(
                update_procedure_capped_command_score(extract.focus_aligned_command_score)
                    .saturating_mul(4096),
            )
        });
    Some(UpdateProcedureExtract {
        block_index: selected.first().map(|extract| extract.block_index).unwrap_or_default(),
        score,
        steps,
        block_text: selected
            .iter()
            .map(|extract| extract.block_text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        command_count: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.command_count).sum(),
        ),
        preparatory_command_score: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.preparatory_command_score).sum(),
        ),
        focus_aligned_command_score: update_procedure_capped_command_score(
            selected.iter().map(|extract| extract.focus_aligned_command_score).sum(),
        ),
        unfocused_command_score: 0,
        has_setup_script_signature: selected
            .iter()
            .any(|extract| extract.has_setup_script_signature),
        is_focus_projection: false,
    })
}

fn selected_update_procedure_extracts<'a>(
    extracts: &'a [UpdateProcedureExtract],
    focus_model: &UpdateProcedureFocusModel,
    document_label: &str,
) -> Vec<&'a UpdateProcedureExtract> {
    let mut selected_by_block = BTreeMap::<usize, &UpdateProcedureExtract>::new();
    for extract in extracts.iter().filter(|extract| {
        extract.command_count > 0
            && update_procedure_extract_is_typed_evidence(extract, focus_model, document_label)
    }) {
        selected_by_block
            .entry(extract.block_index)
            .and_modify(|current| {
                if (extract.score, extract.steps.len(), extract.is_focus_projection)
                    > (current.score, current.steps.len(), current.is_focus_projection)
                {
                    *current = extract;
                }
            })
            .or_insert(extract);
    }
    let mut selected = selected_by_block.into_values().collect::<Vec<_>>();
    let preparatory_indexes = selected
        .iter()
        .filter(|extract| extract.preparatory_command_score > 0)
        .map(|extract| extract.block_index)
        .collect::<Vec<_>>();
    if preparatory_indexes.len() > 2
        && let Some(keep_from) = preparatory_indexes.get(preparatory_indexes.len() - 2)
    {
        selected.retain(|extract| {
            extract.focus_aligned_command_score > 0 || extract.block_index >= *keep_from
        });
    }
    selected
}

fn unique_update_procedure_steps(
    extracts: &[&UpdateProcedureExtract],
    limit: usize,
) -> Vec<String> {
    let mut seen = HashSet::new();
    extracts
        .iter()
        .flat_map(|extract| &extract.steps)
        .filter(|step| seen.insert(step.to_lowercase()))
        .take(limit)
        .cloned()
        .collect()
}

pub(super) fn update_procedure_command_candidate_bonus(command_count: usize) -> usize {
    if command_count > 0 { 4096 } else { 0 }
}

pub(super) fn update_procedure_capped_command_score(score: usize) -> usize {
    score.min(UPDATE_PROCEDURE_COMMAND_SIGNAL_SCORE_CAP)
}
