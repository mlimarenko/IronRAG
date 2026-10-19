use super::{
    AnswerCompletionContract, BTreeSet, LiteralKind, QueryAct, QueryIR, QueryScope,
    QueryTargetKind, RuntimeMatchedChunk, deterministic_answer_labels, label_terms,
    line_has_command_signal, normalized_alnum_token_sequence,
    query_ir_allows_procedure_runbook_target, query_ir_has_setup_configuration_target,
    query_ir_requires_remediation_synthesis, strip_leading_order_marker,
    token_sequence_contains_tokens, update_procedure_step_is_structural,
    update_procedure_steps_from_chunks,
};

pub(in crate::services::query::execution) fn build_update_procedure_sequence_answer(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::Describe) {
        return None;
    }
    if query_ir_requires_remediation_synthesis(query_ir) {
        return None;
    }
    let focus_model = update_procedure_focus_model(query_ir);
    if !question_requests_update_procedure_answer(query_ir) {
        return None;
    }
    let selection = update_procedure_steps_from_chunks(&focus_model, chunks)?;
    if selection.steps.len() < 2 {
        return None;
    }
    let labels = deterministic_answer_labels(query_ir);

    let mut lines = vec![
        format!("**{}:** **{}**", labels.source, selection.source),
        String::new(),
        format!("**{}:**", labels.update_sequence),
    ];
    let rendered_steps = selection
        .steps
        .iter()
        .enumerate()
        .map(|(index, step)| render_update_procedure_step(index + 1, step))
        .collect::<Option<Vec<_>>>()?;
    lines.extend(rendered_steps);
    let answer = lines.join("\n");
    let has_executable_sequence =
        selection.steps.iter().filter(|step| line_has_command_signal(step)).take(2).count() >= 2;
    let completion = AnswerCompletionContract::from_query_ir(query_ir).evaluate(&answer);
    (has_executable_sequence || completion.complete).then_some(answer)
}

pub(super) fn render_update_procedure_step(index: usize, step: &str) -> Option<String> {
    let stripped = strip_leading_order_marker(step).trim();
    if stripped.trim_matches('`').trim().is_empty() {
        return None;
    }
    if line_has_command_signal(stripped) {
        let command = stripped.trim_matches('`').trim_end_matches(['.', ',', ';']).trim();
        (!command.is_empty()).then(|| format!("{index}. `{command}`"))
    } else {
        Some(format!("{index}. {stripped}"))
    }
}

fn question_requests_update_procedure_answer(query_ir: &QueryIR) -> bool {
    let allows_retrieved_single_document =
        query_ir_allows_retrieved_single_document_procedure_sequence(query_ir);
    let has_typed_procedure_action = query_ir_has_typed_procedure_action(query_ir);
    query_ir.source_slice.is_none()
        && (query_ir_has_explicit_procedure_focus(query_ir) || allows_retrieved_single_document)
        && ((has_typed_procedure_action && !query_ir_has_setup_configuration_target(query_ir))
            || query_ir_allows_procedure_runbook_target(query_ir)
            || allows_retrieved_single_document)
}

fn query_ir_has_explicit_procedure_focus(query_ir: &QueryIR) -> bool {
    !query_ir.target_entities.is_empty() || query_ir.document_focus.is_some()
}

fn query_ir_has_typed_procedure_action(query_ir: &QueryIR) -> bool {
    if !query_ir_has_explicit_procedure_focus(query_ir)
        || query_ir_has_setup_configuration_target(query_ir)
    {
        return false;
    }
    let mut has_procedure = false;
    let mut has_version_or_release = false;
    let mut has_concept = false;
    for target_type in &query_ir.target_types {
        match target_type {
            QueryTargetKind::Procedure => has_procedure = true,
            QueryTargetKind::Version | QueryTargetKind::Release => {
                has_version_or_release = true;
            }
            QueryTargetKind::Concept => has_concept = true,
            _ => {}
        }
    }
    has_procedure && has_version_or_release && !has_concept
}

fn query_ir_allows_retrieved_single_document_procedure_sequence(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.act, QueryAct::ConfigureHow)
        || !matches!(query_ir.scope, QueryScope::SingleDocument)
        || query_ir.retrieval_query.as_deref().map(str::trim).is_none_or(str::is_empty)
        || query_ir.needs_clarification.is_some()
        || query_ir_has_setup_configuration_target(query_ir)
    {
        return false;
    }
    let mut has_procedure = false;
    let mut has_concept = false;
    let mut has_document_or_revision_signal = false;
    for target_type in &query_ir.target_types {
        match target_type {
            QueryTargetKind::Procedure => has_procedure = true,
            QueryTargetKind::Concept => has_concept = true,
            QueryTargetKind::Artifact
            | QueryTargetKind::Document
            | QueryTargetKind::Entity
            | QueryTargetKind::PrimaryHeading
            | QueryTargetKind::SecondaryHeading
            | QueryTargetKind::Version
            | QueryTargetKind::Release => {
                has_document_or_revision_signal = true;
            }
            _ => {}
        }
    }
    let has_explicit_subject_focus =
        query_ir.document_focus.is_some() || !query_ir.target_entities.is_empty();
    let has_literal_focus = !query_ir.literal_constraints.is_empty();
    let has_typed_subject_focus = has_explicit_subject_focus || has_literal_focus;
    has_procedure && has_typed_subject_focus && (has_concept || has_document_or_revision_signal)
}

pub(super) struct UpdateProcedureFocusModel {
    pub(super) subject_terms: BTreeSet<String>,
    pub(super) target_identity_sequences: Vec<UpdateProcedureIdentitySequence>,
    pub(super) requires_exact_document_subject: bool,
}

#[derive(Debug, Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct UpdateProcedureIdentitySequence {
    tokens: Vec<String>,
    priority: usize,
}
pub(super) fn update_procedure_focus_model(query_ir: &QueryIR) -> UpdateProcedureFocusModel {
    let mut subject_terms = BTreeSet::<String>::new();
    for entity in &query_ir.target_entities {
        subject_terms.extend(label_terms(&entity.label, 2));
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        subject_terms.extend(label_terms(&document_focus.hint, 2));
    }
    for literal in &query_ir.literal_constraints {
        if matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other) {
            subject_terms.extend(label_terms(&literal.text, 1));
        }
    }
    let target_identity_sequences = update_procedure_target_identity_token_sequences(query_ir);
    let requires_exact_document_subject = (query_ir.document_focus.is_some()
        || !query_ir.target_entities.is_empty())
        && query_ir.targets(QueryTargetKind::Document);
    UpdateProcedureFocusModel {
        subject_terms,
        target_identity_sequences,
        requires_exact_document_subject,
    }
}

fn update_procedure_target_identity_token_sequences(
    query_ir: &QueryIR,
) -> Vec<UpdateProcedureIdentitySequence> {
    let mut seen = BTreeSet::<Vec<String>>::new();
    let mut sequences = Vec::new();
    for entity in &query_ir.target_entities {
        push_update_procedure_target_identity_sequence(
            &mut sequences,
            &mut seen,
            &entity.label,
            30,
            false,
        );
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_update_procedure_target_identity_sequence(
            &mut sequences,
            &mut seen,
            &document_focus.hint,
            20,
            false,
        );
    }
    for literal in &query_ir.literal_constraints {
        if matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Other) {
            push_update_procedure_target_identity_sequence(
                &mut sequences,
                &mut seen,
                &literal.text,
                50,
                true,
            );
        }
    }
    sequences
}

fn push_update_procedure_target_identity_sequence(
    sequences: &mut Vec<UpdateProcedureIdentitySequence>,
    seen: &mut BTreeSet<Vec<String>>,
    label: &str,
    priority: usize,
    allow_single_token: bool,
) {
    let sequence = normalized_alnum_token_sequence(label, 1);
    if sequence.is_empty() || (sequence.len() < 2 && !allow_single_token) {
        return;
    }
    if !seen.insert(sequence.clone()) {
        if let Some(existing) = sequences.iter_mut().find(|existing| existing.tokens == sequence) {
            existing.priority = existing.priority.max(priority);
        }
        return;
    }
    sequences.push(UpdateProcedureIdentitySequence { tokens: sequence, priority });
}

pub(super) fn update_procedure_text_target_identity_priority(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> usize {
    if focus_model.target_identity_sequences.is_empty() {
        return 0;
    }
    let text_sequence = normalized_alnum_token_sequence(text, 1);
    focus_model
        .target_identity_sequences
        .iter()
        .filter(|target_sequence| {
            token_sequence_contains_tokens(&text_sequence, &target_sequence.tokens)
        })
        .map(|target_sequence| target_sequence.priority)
        .max()
        .unwrap_or_default()
}

const UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_CHARS: usize = 480;
const UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_LINES: usize = 6;

pub(super) fn update_procedure_text_has_bound_target_identity_runbook(
    text: &str,
    focus_model: &UpdateProcedureFocusModel,
) -> bool {
    if focus_model.target_identity_sequences.is_empty() {
        return false;
    }
    update_procedure_bound_target_identity_windows(text).into_iter().any(|window| {
        update_procedure_text_target_identity_priority(&window, focus_model) > 0
            && window
                .lines()
                .filter(|line| update_procedure_step_is_structural(line))
                .take(2)
                .count()
                >= 2
    })
}

fn update_procedure_bound_target_identity_windows(text: &str) -> Vec<String> {
    let lines = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>();
    let mut seen = BTreeSet::<String>::new();
    let mut windows = Vec::new();

    if lines.len() > 1 {
        for start in 0..lines.len() {
            let window = lines
                .iter()
                .skip(start)
                .take(UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_LINES)
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            if window.chars().count() <= UPDATE_PROCEDURE_BOUND_TARGET_LINE_WINDOW_MAX_CHARS {
                push_update_procedure_bound_target_identity_window(&mut windows, &mut seen, window);
            }
        }
    } else if let Some(line) = lines.first()
        && !update_procedure_line_has_sentence_boundary(line)
    {
        push_update_procedure_bound_target_identity_window(
            &mut windows,
            &mut seen,
            (*line).to_string(),
        );
    }

    for line in &lines {
        for clause in update_procedure_sentence_clauses(line) {
            push_update_procedure_bound_target_identity_window(&mut windows, &mut seen, clause);
        }
    }
    windows
}

fn push_update_procedure_bound_target_identity_window(
    windows: &mut Vec<String>,
    seen: &mut BTreeSet<String>,
    window: String,
) {
    let normalized = window.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || !seen.insert(normalized.clone()) {
        return;
    }
    windows.push(window.trim().to_string());
}

fn update_procedure_sentence_clauses(text: &str) -> Vec<String> {
    let mut clauses = Vec::new();
    let mut start = 0usize;
    for (index, ch) in text.char_indices() {
        if !update_procedure_is_sentence_boundary(text, index, ch) {
            continue;
        }
        let clause = text[start..index].trim();
        if !clause.is_empty() {
            clauses.push(clause.to_string());
        }
        start = index.saturating_add(ch.len_utf8());
    }
    let clause = text[start..].trim();
    if !clause.is_empty() {
        clauses.push(clause.to_string());
    }
    clauses
}

fn update_procedure_line_has_sentence_boundary(text: &str) -> bool {
    text.char_indices().any(|(index, ch)| update_procedure_is_sentence_boundary(text, index, ch))
}

fn update_procedure_is_sentence_boundary(text: &str, index: usize, ch: char) -> bool {
    match ch {
        ';' | '!' | '?' => true,
        '.' => {
            let prev = text[..index].chars().next_back();
            let next_index = index.saturating_add(ch.len_utf8());
            let next = text[next_index..].chars().next();
            if prev.is_some_and(|prev| prev.is_ascii_digit())
                && next.is_some_and(|next| next.is_whitespace())
            {
                return false;
            }
            if next.is_some_and(|next| !next.is_whitespace()) {
                return false;
            }
            true
        }
        _ => false,
    }
}
