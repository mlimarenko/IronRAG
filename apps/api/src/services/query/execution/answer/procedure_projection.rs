use super::{
    BTreeSet, HashSet, RuntimeMatchedChunk, UpdateProcedureExtract, UpdateProcedureFocusModel,
    UpdateProcedureLine, excerpt_for, extract_configuration_section_literals,
    extract_explicit_path_literals, extract_package_command_literals, extract_parameter_literals,
    label_terms, line_has_command_signal, line_has_order_marker, procedure_line_has_command_start,
    push_unique_values, shellish_token_has_artifact_preparation_signal,
    shellish_token_is_invocable_head, shellish_token_is_local_artifact,
    shellish_token_is_path_command_start, shellish_tokens_start_command,
    strip_leading_order_marker, update_procedure_capped_command_score,
    update_procedure_command_candidate_bonus, update_procedure_line_blocks,
    update_procedure_line_has_version, update_procedure_step_from_line,
    update_procedure_text_target_identity_priority,
};

pub(super) fn update_procedure_chunk_text(
    chunk: &RuntimeMatchedChunk,
    focus_model: &UpdateProcedureFocusModel,
) -> String {
    let excerpt = chunk.excerpt.trim();
    let source_text = chunk.source_text.trim();
    if source_text.is_empty() || excerpt == source_text {
        return excerpt.to_string();
    }
    if excerpt.is_empty() {
        return update_procedure_focused_source_text(source_text, focus_model)
            .unwrap_or_else(|| source_text.to_string());
    }
    if let Some(focused_source) = update_procedure_focused_source_text(source_text, focus_model)
        && !focused_source.trim().is_empty()
    {
        return focused_source;
    }
    excerpt.to_string()
}

fn update_procedure_focused_source_text(
    source_text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> Option<String> {
    const UPDATE_PROCEDURE_SOURCE_VIEW_CHARS: usize = 4_000;

    let lines =
        source_text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    let selected = update_procedure_focused_line_indexes(&lines, focus_model);
    if selected.is_empty() {
        return None;
    }
    render_update_procedure_focused_lines(&lines, selected, UPDATE_PROCEDURE_SOURCE_VIEW_CHARS)
}

fn update_procedure_focused_line_indexes(
    lines: &[&str],
    focus_model: &UpdateProcedureFocusModel,
) -> BTreeSet<usize> {
    let mut selected = BTreeSet::new();
    for (index, line) in lines.iter().enumerate() {
        if update_procedure_text_target_identity_priority(line, focus_model) == 0
            && update_procedure_text_focus_score(line, focus_model) == 0
        {
            continue;
        }
        selected.insert(index);
        if index > 0
            && (line_has_order_marker(lines[index - 1])
                || line_has_command_signal(lines[index - 1]))
        {
            selected.insert(index - 1);
        }
        select_update_procedure_lookahead(lines, index, focus_model, &mut selected);
    }
    selected
}

fn select_update_procedure_lookahead(
    lines: &[&str],
    index: usize,
    focus_model: &UpdateProcedureFocusModel,
    selected: &mut BTreeSet<usize>,
) {
    for next_index in (index + 1)..=(index + 2) {
        let Some(next_line) = lines.get(next_index) else {
            break;
        };
        let relevant = line_has_order_marker(next_line)
            || line_has_command_signal(next_line)
            || update_procedure_text_target_identity_priority(next_line, focus_model) > 0
            || update_procedure_text_focus_score(next_line, focus_model) > 0;
        if !relevant {
            break;
        }
        selected.insert(next_index);
    }
}

fn render_update_procedure_focused_lines(
    lines: &[&str],
    selected: BTreeSet<usize>,
    max_chars: usize,
) -> Option<String> {
    let mut focused = String::new();
    let mut previous_index = None;
    for index in selected {
        if !focused.is_empty() {
            focused.push_str(if previous_index.is_some_and(|previous| index > previous + 1) {
                "\n...\n"
            } else {
                "\n"
            });
        }
        focused.push_str(lines[index]);
        previous_index = Some(index);
        if focused.chars().count() >= max_chars {
            return Some(excerpt_for(&focused, max_chars));
        }
    }
    (!focused.trim().is_empty()).then_some(focused)
}

pub(super) fn update_procedure_evidence_anchors(
    steps: &[String],
    source_text: &str,
    limit: usize,
) -> Vec<String> {
    let step_text = steps.join("\n");
    let mut anchors = Vec::new();
    push_update_procedure_evidence_anchors_from_text(&step_text, &mut anchors, limit);
    if anchors.len() < 2 {
        push_update_procedure_evidence_anchors_from_text(source_text, &mut anchors, limit);
    }
    anchors
}

fn push_update_procedure_evidence_anchors_from_text(
    text: &str,
    anchors: &mut Vec<String>,
    limit: usize,
) {
    let mut values = Vec::new();
    values.extend(extract_package_command_literals(text, limit));
    values.extend(extract_explicit_path_literals(text, limit));
    values.extend(extract_configuration_section_literals(text, limit));
    values.extend(extract_parameter_literals(text, limit));
    push_unique_values(anchors, values, limit);
}

pub(super) fn update_procedure_extracts_from_text(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> Vec<UpdateProcedureExtract> {
    let blocks = update_procedure_line_blocks(text);
    let mut extracts = Vec::<UpdateProcedureExtract>::new();
    for (block_index, block) in blocks.into_iter().enumerate() {
        if !update_procedure_block_is_qualified(&block) {
            continue;
        }
        if let Some(extract) = update_procedure_extract_from_block(block_index, &block, focus_model)
        {
            let focus_projection = update_procedure_focus_aligned_command_projection_from_block(
                block_index,
                &block,
                focus_model,
                &extract,
            );
            if let Some(maintenance_extract) = focus_projection {
                extracts.push(maintenance_extract);
            }
            if let Some(tail_extract) = update_procedure_command_tail_projection_from_block(
                block_index,
                &block,
                focus_model,
                &extract,
            ) {
                extracts.push(tail_extract);
            }
            extracts.push(extract);
        }
    }
    extracts.sort_by(|left, right| {
        right.score.cmp(&left.score).then_with(|| right.steps.len().cmp(&left.steps.len()))
    });
    extracts
}

fn update_procedure_extract_from_block(
    block_index: usize,
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> Option<UpdateProcedureExtract> {
    let score = update_procedure_block_score(block, focus_model);
    let mut seen = std::collections::HashSet::new();
    let mut structural_steps_seen = 0usize;
    let mut steps = Vec::new();
    for line in block {
        let is_structural = line.has_version || line.has_order_marker || line.has_command;
        let include = if is_structural {
            structural_steps_seen = structural_steps_seen.saturating_add(1);
            true
        } else {
            structural_steps_seen >= 2 && line.text.chars().count() <= 160
        };
        if !include {
            continue;
        }
        if seen.insert(line.text.to_lowercase()) {
            steps.push(update_procedure_step_from_line(line));
        }
        if steps.len() >= 16 {
            break;
        }
    }
    if steps.iter().filter(|step| update_procedure_step_is_structural(step)).count() < 2 {
        return None;
    }
    let command_count =
        update_procedure_capped_command_score(block.iter().filter(|line| line.has_command).count());
    let preparatory_command_score = update_procedure_capped_command_score(
        update_procedure_preparatory_command_score(block, focus_model),
    );
    let focus_aligned_command_score = update_procedure_capped_command_score(
        update_procedure_focus_aligned_command_score(block, focus_model),
    );
    let unfocused_command_score = update_procedure_capped_command_score(
        update_procedure_unfocused_command_score(block, focus_model),
    );
    let block_text = block.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
    let has_setup_script_signature = update_procedure_block_has_setup_script_signature(block);
    Some(UpdateProcedureExtract {
        block_index,
        score,
        steps,
        block_text,
        command_count,
        preparatory_command_score,
        focus_aligned_command_score,
        unfocused_command_score,
        has_setup_script_signature,
        is_focus_projection: false,
    })
}

fn update_procedure_focus_aligned_command_projection_from_block(
    block_index: usize,
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
    source_extract: &UpdateProcedureExtract,
) -> Option<UpdateProcedureExtract> {
    if source_extract.focus_aligned_command_score == 0
        || (source_extract.unfocused_command_score == 0
            && source_extract.preparatory_command_score == 0)
    {
        return None;
    }
    let mut projection = UpdateProcedureCommandProjection::default();
    projection.prepend_ordered_context(block, focus_model);
    for line in block.iter().filter(|line| line.has_command) {
        projection.push_command(line, focus_model);
        if projection.steps.len() >= 16 {
            break;
        }
    }
    append_trailing_artifact_chain_steps(
        &mut projection.steps,
        &mut projection.seen,
        &projection.pending_preparatory,
    );
    if projection.focus_aligned_command_score == 0 || projection.steps.len() < 2 {
        return None;
    }
    if projection.steps.iter().filter(|step| update_procedure_step_is_structural(step)).count() < 2
    {
        return None;
    }
    let block_text = source_extract.block_text.clone();
    let focused_structural_score =
        update_procedure_focused_structural_score(&block_text, focus_model);
    let score = source_extract
        .score
        .saturating_add(focused_structural_score.saturating_mul(160))
        .saturating_add(
            update_procedure_capped_command_score(projection.preparatory_command_score)
                .saturating_mul(2048),
        )
        .saturating_add(
            update_procedure_capped_command_score(projection.focus_aligned_command_score)
                .saturating_mul(8192),
        )
        .saturating_add(update_procedure_command_candidate_bonus(projection.steps.len()));
    Some(UpdateProcedureExtract {
        block_index,
        score,
        command_count: update_procedure_capped_command_score(projection.steps.len()),
        steps: projection.steps,
        block_text,
        preparatory_command_score: projection.preparatory_command_score,
        focus_aligned_command_score: projection.focus_aligned_command_score,
        unfocused_command_score: 0,
        has_setup_script_signature: source_extract.has_setup_script_signature,
        is_focus_projection: true,
    })
}

#[derive(Default)]
struct UpdateProcedureCommandProjection<'a> {
    seen: HashSet<String>,
    steps: Vec<String>,
    preparatory_command_score: usize,
    focus_aligned_command_score: usize,
    pending_preparatory: Vec<&'a UpdateProcedureLine>,
}

impl<'a> UpdateProcedureCommandProjection<'a> {
    fn prepend_ordered_context(
        &mut self,
        block: &'a [UpdateProcedureLine],
        focus_model: &UpdateProcedureFocusModel,
    ) {
        let Some(first_aligned_index) = block.iter().position(|line| {
            line.has_command
                && update_procedure_command_focus_aligned_score(
                    strip_leading_order_marker(&line.text),
                    focus_model,
                ) > 0
        }) else {
            return;
        };
        if block.iter().take(first_aligned_index).any(|line| line.has_command) {
            return;
        }
        for line in block.iter().take(first_aligned_index).filter(|line| line.has_order_marker) {
            self.push_unique_step(line);
        }
    }

    fn push_command(
        &mut self,
        line: &'a UpdateProcedureLine,
        focus_model: &UpdateProcedureFocusModel,
    ) {
        let aligned_score = update_procedure_command_focus_aligned_score(
            strip_leading_order_marker(&line.text),
            focus_model,
        );
        if aligned_score == 0 {
            self.pending_preparatory.push(line);
            if self.pending_preparatory.len() > 3 {
                self.pending_preparatory.remove(0);
            }
            return;
        }
        let preparatory =
            self.pending_preparatory.drain(..).rev().take(2).rev().collect::<Vec<_>>();
        for preparatory in preparatory {
            if self.push_unique_step(preparatory) {
                self.preparatory_command_score = update_procedure_capped_command_score(
                    self.preparatory_command_score.saturating_add(1),
                );
            }
        }
        self.focus_aligned_command_score = update_procedure_capped_command_score(
            self.focus_aligned_command_score.saturating_add(aligned_score),
        );
        self.push_unique_step(line);
    }

    fn push_unique_step(&mut self, line: &UpdateProcedureLine) -> bool {
        if !self.seen.insert(line.text.to_lowercase()) {
            return false;
        }
        self.steps.push(update_procedure_step_from_line(line));
        true
    }
}

fn append_trailing_artifact_chain_steps(
    steps: &mut Vec<String>,
    seen: &mut HashSet<String>,
    trailing: &[&UpdateProcedureLine],
) {
    let mut artifacts =
        steps.iter().flat_map(|step| command_local_artifact_keys(step)).collect::<BTreeSet<_>>();
    if artifacts.is_empty() {
        return;
    }
    for line in trailing {
        let line_artifacts = command_local_artifact_keys(&line.text);
        if line_artifacts.is_empty() || line_artifacts.is_disjoint(&artifacts) {
            continue;
        }
        artifacts.extend(line_artifacts);
        if seen.insert(line.text.to_lowercase()) {
            steps.push(update_procedure_step_from_line(line));
        }
    }
}

fn command_local_artifact_keys(command: &str) -> BTreeSet<String> {
    command_token_values(strip_leading_order_marker(command))
        .into_iter()
        .enumerate()
        .filter_map(|(index, token)| {
            let usable = if index == 0 {
                shellish_token_is_path_command_start(&token)
            } else {
                token_has_local_command_artifact(&token)
            };
            usable.then(|| trim_command_boundary_token_decorations(&token).to_ascii_lowercase())
        })
        .collect()
}

fn update_procedure_command_tail_projection_from_block(
    block_index: usize,
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
    source_extract: &UpdateProcedureExtract,
) -> Option<UpdateProcedureExtract> {
    if source_extract.command_count < 5 {
        return None;
    }
    let command_indices = block
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.has_command.then_some(index))
        .collect::<Vec<_>>();
    if command_indices.len() < 5 {
        return None;
    }
    let first_target_command_offset = command_indices.iter().position(|index| {
        update_procedure_text_target_identity_priority(&block[*index].text, focus_model) > 0
    });
    if first_target_command_offset.is_none()
        && !source_extract.block_text.lines().any(update_procedure_line_has_version)
    {
        return None;
    }
    let mut start_offset = first_target_command_offset
        .map(|offset| offset.saturating_sub(3))
        .unwrap_or_else(|| command_indices.len().saturating_sub(4));
    if start_offset > 0 {
        let previous_index = command_indices[start_offset - 1];
        let first_index = command_indices[start_offset];
        if update_procedure_command_heads_match(
            &block[previous_index].text,
            &block[first_index].text,
        ) {
            start_offset -= 1;
        }
    }
    let projection_block = command_indices
        .iter()
        .skip(start_offset)
        .take(8)
        .map(|index| block[*index].clone())
        .collect::<Vec<_>>();
    if projection_block.len() < 4 {
        return None;
    }
    let mut seen = std::collections::HashSet::new();
    let steps = projection_block
        .iter()
        .filter(|line| seen.insert(line.text.to_lowercase()))
        .map(update_procedure_step_from_line)
        .collect::<Vec<_>>();
    if steps.len() < 4 {
        return None;
    }
    let block_text = steps.join("\n");
    let command_count = update_procedure_capped_command_score(projection_block.len());
    let preparatory_command_score =
        usize::from(first_target_command_offset.is_some_and(|offset| start_offset < offset));
    let focus_aligned_command_score = update_procedure_capped_command_score(
        update_procedure_focus_aligned_command_score(&projection_block, focus_model),
    );
    let unfocused_command_score = update_procedure_capped_command_score(
        update_procedure_unfocused_command_score(&projection_block, focus_model),
    );
    let target_identity_priority =
        update_procedure_text_target_identity_priority(&block_text, focus_model);
    let score = source_extract
        .score
        .saturating_add(command_count.saturating_mul(2048))
        .saturating_add(focus_aligned_command_score.saturating_mul(4096))
        .saturating_add(target_identity_priority.saturating_mul(2048))
        .saturating_sub(unfocused_command_score.saturating_mul(512));
    Some(UpdateProcedureExtract {
        block_index,
        score,
        steps,
        block_text,
        command_count,
        preparatory_command_score,
        focus_aligned_command_score,
        unfocused_command_score,
        has_setup_script_signature: source_extract.has_setup_script_signature,
        is_focus_projection: true,
    })
}

fn update_procedure_block_is_qualified(block: &[UpdateProcedureLine]) -> bool {
    let version_count = block.iter().filter(|line| line.has_version).count();
    let ordered_count = block.iter().filter(|line| line.has_order_marker).count();
    let command_count = block.iter().filter(|line| line.has_command).count();
    ordered_count >= 2
        || command_count >= 2
        || (ordered_count >= 1 && command_count >= 1)
        || (ordered_count >= 1 && version_count >= 1)
}

fn update_procedure_block_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let focus_text = block.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
    let focus_score = update_procedure_text_focus_score(&focus_text, focus_model);
    let version_count = block.iter().filter(|line| line.has_version).count();
    let ordered_count = block.iter().filter(|line| line.has_order_marker).count();
    let command_count = block.iter().filter(|line| line.has_command).count();
    focus_score
        .saturating_mul(128)
        .saturating_add(command_count.saturating_mul(32))
        .saturating_add(ordered_count.saturating_mul(24))
        .saturating_add(version_count.saturating_mul(8))
        .saturating_add(block.len().min(16))
}

pub(super) fn update_procedure_text_focus_score(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let tokens = label_terms(text, 2);
    if tokens.is_empty() {
        return 0;
    }
    focus_model.subject_terms.intersection(&tokens).count().saturating_mul(16)
}

pub(super) fn update_procedure_focused_structural_score(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    text.lines()
        .filter_map(|line| {
            let is_structural =
                update_procedure_line_has_version(line) || line_has_command_signal(line);
            if !is_structural {
                return None;
            }
            let focus_score = update_procedure_text_focus_score(line, focus_model);
            (focus_score > 0).then_some(focus_score)
        })
        .sum::<usize>()
}

pub(super) fn update_procedure_extract_is_typed_evidence(
    extract: &UpdateProcedureExtract,
    focus_model: &UpdateProcedureFocusModel,
    document_label: &str,
) -> bool {
    let has_target_identity =
        update_procedure_text_target_identity_priority(&extract.block_text, focus_model) > 0
            || update_procedure_text_target_identity_priority(document_label, focus_model) > 0;
    if !has_target_identity {
        return false;
    }
    if !extract.has_setup_script_signature {
        return true;
    }
    extract.command_count > 0
        && extract
            .block_text
            .lines()
            .filter(|line| line_has_order_marker(line) && !line_has_command_signal(line))
            .take(2)
            .count()
            >= 2
}

fn update_procedure_preparatory_command_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let first_aligned = block.iter().position(|line| {
        line.has_command
            && update_procedure_command_focus_aligned_score(
                strip_leading_order_marker(&line.text),
                focus_model,
            ) > 0
    });
    let Some(first_aligned) = first_aligned else {
        return 0;
    };
    block.iter().take(first_aligned).filter(|line| line.has_command).count()
}

fn update_procedure_focus_aligned_command_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    block
        .iter()
        .filter(|line| line.has_command)
        .map(|line| {
            update_procedure_command_focus_aligned_score(
                strip_leading_order_marker(&line.text),
                focus_model,
            )
        })
        .sum()
}

fn update_procedure_unfocused_command_score(
    block: &[UpdateProcedureLine],
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let Some(first_aligned) = block.iter().position(|line| {
        line.has_command
            && update_procedure_command_focus_aligned_score(
                strip_leading_order_marker(&line.text),
                focus_model,
            ) > 0
    }) else {
        return block.iter().filter(|line| line.has_command).count();
    };
    block
        .iter()
        .skip(first_aligned.saturating_add(1))
        .filter(|line| line.has_command)
        .filter(|line| {
            update_procedure_command_focus_aligned_score(
                strip_leading_order_marker(&line.text),
                focus_model,
            ) == 0
        })
        .count()
}

fn update_procedure_command_focus_aligned_score(
    command: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    let tokens = label_terms(command, 2);
    if tokens.is_empty() {
        return 0;
    }
    focus_model.subject_terms.intersection(&tokens).count().saturating_mul(8)
}

pub(super) fn update_procedure_block_has_setup_script_signature(
    block: &[UpdateProcedureLine],
) -> bool {
    let signature = block
        .iter()
        .filter(|line| line.has_command)
        .map(|line| command_token_values(strip_leading_order_marker(&line.text)))
        .filter(|tokens| !tokens.is_empty())
        .fold(UpdateProcedureSetupScriptSignature::default(), |signature, tokens| {
            signature.with_command_tokens(&tokens)
        });
    signature.is_complete()
}

#[derive(Default)]
struct UpdateProcedureSetupScriptSignature {
    has_external_artifact_materialization: bool,
    has_local_artifact_preparation: bool,
    has_local_artifact: bool,
    has_local_artifact_run: bool,
}

impl UpdateProcedureSetupScriptSignature {
    fn with_command_tokens(mut self, tokens: &[String]) -> Self {
        self.has_external_artifact_materialization |=
            command_tokens_have_external_artifact_materialization(tokens);
        let line_has_local_artifact =
            tokens.iter().any(|token| token_has_local_command_artifact(token));
        self.has_local_artifact |= line_has_local_artifact;
        self.has_local_artifact_preparation |= line_has_local_artifact
            && tokens.iter().any(|token| shellish_token_has_artifact_preparation_signal(token));
        self.has_local_artifact_run |=
            tokens.first().is_some_and(|token| shellish_token_is_path_command_start(token));
        self
    }

    fn is_complete(&self) -> bool {
        self.has_external_artifact_materialization
            && (self.has_local_artifact_run
                || (self.has_local_artifact_preparation && self.has_local_artifact))
    }
}

fn command_tokens_have_external_artifact_materialization(tokens: &[String]) -> bool {
    let has_external_artifact = tokens.iter().any(|token| token.contains("://"));
    let has_local_artifact =
        tokens.iter().skip(1).any(|token| token_has_local_command_artifact(token));
    has_external_artifact && has_local_artifact
}

pub(super) fn token_has_local_command_artifact(token: &str) -> bool {
    let normalized = trim_command_token_decorations(token);
    if normalized.contains("://") {
        return false;
    }
    if let Some((_, value)) = normalized.split_once('=')
        && !value.is_empty()
    {
        return shellish_token_is_local_artifact(value);
    }
    if normalized.starts_with('-') || normalized.starts_with('+') {
        return false;
    }
    shellish_token_is_local_artifact(normalized)
}

fn trim_command_token_decorations(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ',' | ':')
            || command_token_char_is_invisible_format(ch)
    })
}

pub(super) fn trim_command_boundary_token_decorations(token: &str) -> &str {
    trim_command_token_decorations(token).trim_end_matches(['.', ',', ';'])
}

fn command_token_char_is_invisible_format(ch: char) -> bool {
    matches!(
        ch,
        '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}'
    )
}

pub(super) fn command_token_values(command_line: &str) -> Vec<String> {
    command_line
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|ch: char| {
                    matches!(ch, '`' | '\'' | '"' | '(' | ')' | ';' | ',' | ':')
                        || command_token_char_is_invisible_format(ch)
                })
                .to_ascii_lowercase()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

pub(super) fn structural_command_head_token(tokens: &[String]) -> Option<&str> {
    shellish_tokens_start_command(tokens).then(|| tokens.first().map(String::as_str)).flatten()
}

pub(super) fn update_procedure_command_head(command_line: &str) -> Option<String> {
    if !procedure_line_has_command_start(command_line) {
        return None;
    }
    let tokens = command_token_values(strip_leading_order_marker(command_line));
    tokens.first().filter(|head| shellish_token_is_invocable_head(head)).map(ToString::to_string)
}

pub(super) fn update_procedure_command_heads_match(left: &str, right: &str) -> bool {
    let Some(left_head) = update_procedure_command_head(left) else {
        return false;
    };
    let Some(right_head) = update_procedure_command_head(right) else {
        return false;
    };
    left_head == right_head
}

pub(super) fn update_procedure_step_is_structural(step: &str) -> bool {
    line_has_order_marker(step)
        || update_procedure_line_has_version(step)
        || line_has_command_signal(step)
}
