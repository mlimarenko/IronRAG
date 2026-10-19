use super::{
    BTreeSet, EntityRole, HashMap, KnowledgeChunkRow, KnowledgeDocumentRow, QueryAct, QueryIR,
    QueryScope, QueryTargetKind, RAW_SETUP_FOCUS_CANDIDATE_TOKEN_DOCUMENT_DIVISOR,
    RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_CAP,
    RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_FLOOR,
    RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR, RuntimeMatchedChunk,
    SETUP_FOCUS_CONFIG_PATH_EXTENSIONS, SETUP_FOCUS_DOCUMENT_CHUNK_CAP,
    SETUP_FOCUS_DOCUMENT_FORWARD_CHUNKS, SETUP_FOCUS_DOCUMENT_SCORE_BASE, Uuid,
    adjacent_query_ir_focus_compounds, extract_explicit_path_literals,
    extract_package_command_literals, extract_parameter_literals, is_usable_query_ir_focus,
    near_token_overlap_count, normalized_alnum_token_sequence, normalized_alnum_tokens,
    query_ir_has_specific_adjacent_entity_identity,
    query_ir_requests_versioned_update_procedure_context, score_desc_chunks,
    short_acronym_identity_tokens, strip_leading_question_marker, token_sequence_contains_tokens,
};

pub(super) fn select_setup_focus_document_chunks(
    document_candidates: Vec<(usize, Uuid, Vec<RuntimeMatchedChunk>)>,
) -> Vec<RuntimeMatchedChunk> {
    let mut document_candidates = document_candidates;
    document_candidates.sort_by(
        |(left_score, left_id, left_chunks), (right_score, right_id, right_chunks)| {
            right_score
                .cmp(left_score)
                .then_with(|| {
                    setup_focus_candidate_best_chunk_score(right_chunks)
                        .total_cmp(&setup_focus_candidate_best_chunk_score(left_chunks))
                })
                .then_with(|| left_id.cmp(right_id))
        },
    );
    let mut selected = Vec::new();
    for (_, _, mut chunks) in document_candidates {
        chunks.sort_by(|left, right| {
            left.chunk_index
                .cmp(&right.chunk_index)
                .then_with(|| score_desc_chunks(left, right))
                .then_with(|| left.chunk_id.cmp(&right.chunk_id))
        });
        for chunk in chunks {
            selected.push(chunk);
            if selected.len() >= SETUP_FOCUS_DOCUMENT_CHUNK_CAP {
                return selected;
            }
        }
    }
    selected
}

pub(super) fn setup_focus_candidate_best_chunk_score(chunks: &[RuntimeMatchedChunk]) -> f32 {
    chunks.iter().filter_map(|chunk| chunk.score).max_by(f32::total_cmp).unwrap_or(0.0)
}

pub(super) fn query_ir_requests_setup_focus_document_candidates(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some()
        || query_ir_requests_versioned_update_procedure_context("", query_ir)
        || !query_ir_has_setup_focus_identity(query_ir)
    {
        return false;
    }
    // A configure/how-to intent qualifies for the focused-document lane when it
    // names a concrete subject or declares a command-object/configuration/parameter target.
    // Candidate selection still requires document-identity overlap, so generic
    // configure questions without a usable identity stay out of this lane.
    if matches!(query_ir.act, QueryAct::ConfigureHow)
        && (query_ir_has_setup_focus_target(query_ir)
            || query_ir.document_focus.is_some()
            || setup_focus_single_subject_identity(query_ir).is_some()
            || query_ir_has_setup_focus_subject_modifier_identity(query_ir)
            || query_ir_has_setup_focus_followup_object_identity(query_ir)
            || query_ir_has_specific_adjacent_entity_identity(query_ir))
    {
        return true;
    }
    let mut has_command_object_target = false;
    let mut has_configuration_target = false;
    for target_type in &query_ir.target_types {
        match target_type {
            QueryTargetKind::Package => has_command_object_target = true,
            QueryTargetKind::ConfigurationFile | QueryTargetKind::ConfigKey => {
                has_configuration_target = true;
            }
            _ => {}
        }
    }
    has_command_object_target && has_configuration_target
}

pub(super) fn query_ir_has_setup_focus_identity(query_ir: &QueryIR) -> bool {
    query_ir.document_focus.is_some()
        || query_ir_has_setup_focus_subject_identity(query_ir)
        || query_ir_has_setup_focus_subject_modifier_identity(query_ir)
        || query_ir_has_setup_focus_followup_object_identity(query_ir)
        || query_ir_has_specific_adjacent_entity_identity(query_ir)
}

pub(super) fn query_ir_has_setup_focus_subject_identity(query_ir: &QueryIR) -> bool {
    query_ir.target_entities.iter().any(|entity| {
        matches!(entity.role, EntityRole::Subject)
            && setup_focus_entity_has_usable_identity(entity.label.trim())
    })
}

pub(super) fn query_ir_has_setup_focus_followup_object_identity(query_ir: &QueryIR) -> bool {
    !query_ir.conversation_refs.is_empty()
        && matches!(query_ir.act, QueryAct::ConfigureHow)
        && query_ir.target_entities.iter().any(|entity| {
            matches!(entity.role, EntityRole::Object)
                && !setup_focus_identity_tokens(entity.label.trim()).is_empty()
        })
}

pub(super) fn setup_focus_single_subject_identity(query_ir: &QueryIR) -> Option<&str> {
    if query_ir.comparison.is_some()
        || !query_ir.temporal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
        || !query_ir.literal_constraints.is_empty()
    {
        return None;
    }
    let mut subjects = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject))
        .filter_map(|entity| {
            let label = entity.label.trim();
            setup_focus_entity_has_usable_identity(label).then_some(label)
        });
    let subject = subjects.next()?;
    subjects.next().is_none().then_some(subject)
}

pub(super) fn setup_focus_entity_has_usable_identity(label: &str) -> bool {
    normalized_alnum_tokens(label, 3).len() >= 2 || !short_acronym_identity_tokens(label).is_empty()
}

pub(super) fn query_ir_has_setup_focus_subject_modifier_identity(query_ir: &QueryIR) -> bool {
    let Some(subject) = setup_focus_single_subject_label(query_ir) else {
        return false;
    };
    !setup_focus_subject_modifier_identity_terms(query_ir, subject).is_empty()
}

pub(super) fn setup_focus_single_subject_label(query_ir: &QueryIR) -> Option<&str> {
    if query_ir.comparison.is_some()
        || !query_ir.temporal_constraints.is_empty()
        || !query_ir.conversation_refs.is_empty()
        || !query_ir.literal_constraints.is_empty()
    {
        return None;
    }
    let mut subjects = query_ir
        .target_entities
        .iter()
        .filter(|entity| matches!(entity.role, EntityRole::Subject))
        .filter_map(|entity| {
            let label = entity.label.trim();
            (!setup_focus_identity_tokens(label).is_empty()).then_some(label)
        });
    let subject = subjects.next()?;
    subjects.next().is_none().then_some(subject)
}

pub(super) fn query_ir_has_setup_focus_target(query_ir: &QueryIR) -> bool {
    query_ir.targets_any(&[
        QueryTargetKind::Package,
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Parameter,
    ])
}

pub(super) fn setup_focus_candidate_document_ids(
    query_ir: &QueryIR,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    let query_identity_terms = if query_ir_has_specific_adjacent_entity_identity(query_ir) {
        setup_focus_adjacent_entity_identity_terms(query_ir)
    } else {
        setup_focus_query_identity_terms(query_ir)
    };
    let focus_terms = query_identity_terms
        .into_iter()
        .filter_map(|term| {
            let tokens = setup_focus_identity_tokens(&term);
            (!tokens.is_empty()).then_some((term, tokens))
        })
        .collect::<Vec<_>>();
    if focus_terms.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut candidates = document_index
        .values()
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let best = focus_terms
                .iter()
                .enumerate()
                .flat_map(|(focus_index, (focus, focus_tokens))| {
                    let required_overlap = focus_tokens.len().clamp(1, 2);
                    let focus_token_set = focus_tokens.iter().cloned().collect::<BTreeSet<_>>();
                    setup_focus_document_identity_values(document).into_iter().filter_map(
                        move |value| {
                            let value_tokens = setup_focus_identity_tokens(&value);
                            let overlap = near_token_overlap_count(&focus_token_set, &value_tokens);
                            (overlap >= required_overlap).then(|| {
                                let exact = normalize_document_identity_value(&value)
                                    == normalize_document_identity_value(focus);
                                let specificity = focus_tokens.len();
                                let length = value.chars().count();
                                (
                                    std::cmp::Reverse(focus_index),
                                    overlap,
                                    exact,
                                    specificity,
                                    std::cmp::Reverse(length),
                                    document.document_id,
                                )
                            })
                        },
                    )
                })
                .max();
            best.map(|score| (score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left, _), (right, _)| right.cmp(left));
    candidates.into_iter().map(|(_, document_id)| document_id).take(limit).collect()
}

pub(super) fn document_evidence_anchor_candidate_document_ids(
    question: &str,
    query_ir: Option<&QueryIR>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    limit: usize,
) -> Vec<Uuid> {
    if limit == 0 {
        return Vec::new();
    }
    if let Some(query_ir) = query_ir
        && (query_ir.source_slice.is_some()
            || query_ir.comparison.is_some()
            || matches!(query_ir.scope, QueryScope::MultiDocument | QueryScope::LibraryMeta))
    {
        return Vec::new();
    }
    let focus_terms = document_evidence_anchor_focus_terms(question, query_ir);
    let focus_tokens = focus_terms.into_iter().collect::<BTreeSet<_>>();
    if focus_tokens.is_empty() {
        return Vec::new();
    }
    let distinctive_tokens =
        raw_setup_focus_distinctive_question_tokens(&focus_tokens, document_index);
    let scoring_tokens =
        if distinctive_tokens.is_empty() { focus_tokens } else { distinctive_tokens };
    let mut candidates = document_index
        .values()
        .filter(|document| !setup_focus_document_is_standalone_image(document))
        .filter_map(|document| {
            let score = setup_focus_document_identity_values(document)
                .into_iter()
                .map(|value| document_evidence_anchor_identity_score(&scoring_tokens, &value))
                .max()
                .unwrap_or(0);
            (score > 0).then_some((score, document.document_id))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|(left_score, left_id), (right_score, right_id)| {
        right_score.cmp(left_score).then_with(|| left_id.cmp(right_id))
    });
    candidates.into_iter().map(|(_, document_id)| document_id).take(limit).collect()
}

pub(super) fn document_evidence_anchor_focus_terms(
    question: &str,
    query_ir: Option<&QueryIR>,
) -> Vec<String> {
    let mut terms = normalized_alnum_tokens(strip_leading_question_marker(question), 3);
    if let Some(query_ir) = query_ir {
        if let Some(retrieval_query) = query_ir.retrieval_query.as_deref() {
            terms.extend(normalized_alnum_tokens(retrieval_query, 3));
        }
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            terms.extend(normalized_alnum_tokens(&document_focus.hint, 3));
        }
        for entity in &query_ir.target_entities {
            terms.extend(normalized_alnum_tokens(&entity.label, 3));
        }
        for literal in &query_ir.literal_constraints {
            terms.extend(normalized_alnum_tokens(&literal.text, 3));
        }
    }
    terms.into_iter().collect()
}

pub(super) fn document_evidence_anchor_identity_score(
    focus_tokens: &BTreeSet<String>,
    identity_value: &str,
) -> usize {
    let identity_tokens = normalized_alnum_tokens(identity_value, 3);
    let overlap = focus_tokens.intersection(&identity_tokens).count();
    if overlap < 2 {
        return 0;
    }
    overlap.saturating_mul(32).saturating_add(identity_tokens.len().min(16))
}

pub(super) fn raw_setup_focus_distinctive_question_tokens(
    question_tokens: &BTreeSet<String>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> BTreeSet<String> {
    if question_tokens.is_empty() || document_index.is_empty() {
        return BTreeSet::new();
    }
    let max_document_frequency =
        raw_setup_focus_candidate_token_max_document_frequency(document_index.len());
    let mut document_frequency = HashMap::<String, usize>::new();
    for document in document_index.values() {
        let mut seen_for_document = BTreeSet::<String>::new();
        for value in setup_focus_document_identity_values(document) {
            for token in setup_focus_identity_tokens(&value) {
                if question_tokens.contains(&token) {
                    seen_for_document.insert(token);
                }
            }
        }
        for token in seen_for_document {
            *document_frequency.entry(token).or_default() += 1;
        }
    }
    question_tokens
        .iter()
        .filter(|token| {
            document_frequency
                .get(*token)
                .is_some_and(|frequency| *frequency <= max_document_frequency)
        })
        .cloned()
        .collect()
}

pub(super) fn raw_setup_focus_candidate_token_max_document_frequency(
    document_count: usize,
) -> usize {
    document_count.saturating_div(RAW_SETUP_FOCUS_CANDIDATE_TOKEN_DOCUMENT_DIVISOR).clamp(
        RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_FLOOR,
        RAW_SETUP_FOCUS_CANDIDATE_TOKEN_MAX_DOC_FREQUENCY_CAP,
    )
}

pub(super) fn raw_setup_focus_text_structural_score(text: &str) -> usize {
    let command_literal_count = extract_package_command_literals(text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(text);
    let assignment_count = setup_focus_parameter_assignment_count(text);
    let parameter_literal_count = setup_focus_parameter_literal_count(text);
    let section_count = setup_focus_section_header_count(text);
    let url_count = setup_focus_url_count(text);

    command_literal_count
        .saturating_mul(8)
        .saturating_add(configuration_path_count.saturating_mul(12))
        .saturating_add(assignment_count.saturating_mul(4))
        .saturating_add(parameter_literal_count.saturating_mul(6))
        .saturating_add(section_count.saturating_mul(3))
        .saturating_add(url_count.saturating_mul(4))
}

pub(super) fn setup_focus_query_identity_terms(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut terms = Vec::new();
    if !query_ir_has_specific_adjacent_entity_identity(query_ir)
        && let Some(subject) = setup_focus_subject_only_identity(query_ir)
    {
        push_setup_focus_identity_term(subject, &mut seen, &mut terms);
        for compound in setup_focus_subject_modifier_identity_terms(query_ir, subject) {
            push_setup_focus_identity_term(&compound, &mut seen, &mut terms);
        }
        return terms;
    }
    if !query_ir_has_specific_adjacent_entity_identity(query_ir)
        && let Some(subject) = setup_focus_single_subject_label(query_ir)
    {
        let compounds = setup_focus_subject_modifier_identity_terms(query_ir, subject);
        if !compounds.is_empty() {
            push_setup_focus_identity_term(subject, &mut seen, &mut terms);
            for compound in compounds {
                push_setup_focus_identity_term(&compound, &mut seen, &mut terms);
            }
            return terms;
        }
    }
    for compound in setup_focus_adjacent_entity_identity_terms(query_ir) {
        push_setup_focus_identity_term(&compound, &mut seen, &mut terms);
    }
    for entity in &query_ir.target_entities {
        let label = entity.label.trim();
        push_setup_focus_identity_term(label, &mut seen, &mut terms);
    }
    if let Some(document_focus) = &query_ir.document_focus {
        let focus = document_focus.hint.trim();
        push_setup_focus_identity_term(focus, &mut seen, &mut terms);
    }
    for literal in &query_ir.literal_constraints {
        let literal = literal.text.trim();
        push_setup_focus_identity_term(literal, &mut seen, &mut terms);
    }
    terms
}

pub(super) fn push_setup_focus_identity_term(
    term: &str,
    seen: &mut BTreeSet<String>,
    terms: &mut Vec<String>,
) {
    let trimmed = term.trim();
    if !trimmed.is_empty() && seen.insert(trimmed.to_lowercase()) {
        terms.push(trimmed.to_string());
    }
}

pub(super) fn setup_focus_adjacent_entity_identity_terms(query_ir: &QueryIR) -> Vec<String> {
    let entity_values = query_ir
        .target_entities
        .iter()
        .filter_map(|entity| {
            let normalized = entity.label.split_whitespace().collect::<Vec<_>>().join(" ");
            is_usable_query_ir_focus(&normalized).then_some(normalized)
        })
        .collect::<Vec<_>>();
    adjacent_query_ir_focus_compounds(&entity_values)
}

pub(super) fn setup_focus_subject_modifier_identity_terms(
    query_ir: &QueryIR,
    subject: &str,
) -> Vec<String> {
    let subject_tokens = setup_focus_identity_tokens(subject);
    if subject_tokens.len() > 1 {
        return Vec::new();
    }
    query_ir
        .target_entities
        .iter()
        .filter(|entity| !matches!(entity.role, EntityRole::Subject))
        .filter_map(|entity| {
            let modifier = entity.label.trim();
            if !setup_focus_identity_tokens(modifier).is_empty() {
                Some(format!("{subject} {modifier}"))
            } else {
                None
            }
        })
        .collect()
}

pub(super) fn setup_focus_subject_only_identity(query_ir: &QueryIR) -> Option<&str> {
    if !matches!(query_ir.act, QueryAct::ConfigureHow)
        || query_ir.document_focus.is_some()
        || query_ir_has_setup_focus_target(query_ir)
    {
        return None;
    }
    setup_focus_single_subject_identity(query_ir)
}

pub(super) fn setup_focus_identity_tokens(value: &str) -> BTreeSet<String> {
    let mut tokens = normalized_alnum_tokens(value, 3);
    tokens.extend(short_acronym_identity_tokens(value));
    tokens
}

pub(super) fn setup_focus_document_is_standalone_image(document: &KnowledgeDocumentRow) -> bool {
    setup_focus_document_identity_values(document).into_iter().any(|value| {
        let lowered = value.trim().to_ascii_lowercase();
        SETUP_FOCUS_IMAGE_EXTENSIONS.iter().any(|extension| lowered.ends_with(extension))
    })
}

pub(super) const SETUP_FOCUS_IMAGE_EXTENSIONS: [&str; 8] =
    [".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp", ".tif", ".tiff"];

pub(super) fn setup_focus_path_has_configuration_extension(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    SETUP_FOCUS_CONFIG_PATH_EXTENSIONS.iter().any(|extension| lowered.ends_with(extension))
}

pub(super) fn setup_focus_configuration_path_count(text: &str) -> usize {
    extract_explicit_path_literals(text, 8)
        .into_iter()
        .filter(|path| setup_focus_path_has_configuration_extension(path))
        .count()
}

pub(super) fn setup_focus_parameter_assignment_count(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            let Some((key, value)) = trimmed.split_once('=') else {
                return false;
            };
            let key = key.trim();
            let value = value.trim();
            !key.is_empty()
                && !value.is_empty()
                && key.chars().any(|ch| ch.is_alphanumeric())
                && key.chars().all(|ch| {
                    ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | ':' | '[' | ']')
                })
        })
        .take(8)
        .count()
}

pub(super) fn setup_focus_parameter_literal_count(text: &str) -> usize {
    extract_parameter_literals(text, 16).len()
}

pub(super) fn setup_focus_section_header_count(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.len() > 2 && trimmed.starts_with('[') && trimmed.ends_with(']')
        })
        .take(8)
        .count()
}

pub(super) fn setup_focus_url_count(text: &str) -> usize {
    text.match_indices("http://").count() + text.match_indices("https://").count()
}

pub(super) fn setup_focus_document_identity_values(document: &KnowledgeDocumentRow) -> Vec<String> {
    let mut values = Vec::new();
    if let Some(title) = document.title.as_deref() {
        values.push(title.to_string());
    }
    if let Some(file_name) = document.file_name.as_deref() {
        values.push(file_name.to_string());
    }
    values.push(document.external_key.clone());
    values
}

pub(super) fn normalize_document_identity_value(value: &str) -> String {
    normalized_alnum_tokens(value, 1).into_iter().collect::<Vec<_>>().join(" ")
}

pub(super) fn select_setup_focus_document_rows(
    rows: &[KnowledgeChunkRow],
) -> Vec<KnowledgeChunkRow> {
    let mut candidates = Vec::<(usize, &KnowledgeChunkRow)>::new();
    let mut selected_chunk_ids = BTreeSet::new();
    for row in rows {
        let score = setup_focus_row_score(row);
        if score > 0 && selected_chunk_ids.insert(row.chunk_id) {
            candidates.push((setup_focus_selection_row_score(row, score), row));
        }
    }
    for anchor in rows.iter().filter(|row| setup_focus_row_has_command_literal_and_path(row)) {
        let forward_limit = anchor.chunk_index.saturating_add(SETUP_FOCUS_DOCUMENT_FORWARD_CHUNKS);
        for row in rows
            .iter()
            .filter(|row| row.chunk_index > anchor.chunk_index && row.chunk_index <= forward_limit)
        {
            if selected_chunk_ids.insert(row.chunk_id) {
                candidates.push((1, row));
            }
        }
    }
    candidates.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    candidates
        .into_iter()
        .take(SETUP_FOCUS_DOCUMENT_CHUNK_CAP)
        .map(|(_, row)| row.clone())
        .collect()
}

pub(super) fn setup_focus_selection_row_score(row: &KnowledgeChunkRow, base_score: usize) -> usize {
    if setup_focus_row_has_command_literal_and_path(row) {
        base_score.saturating_add(RAW_SETUP_FOCUS_STRUCTURAL_DOCUMENT_SCORE_FLOOR.saturating_mul(4))
    } else {
        base_score
    }
}

pub(super) fn setup_focus_document_candidate_score(
    rows: &[KnowledgeChunkRow],
    query_ir: &QueryIR,
) -> usize {
    let literal_terms = query_ir
        .literal_constraints
        .iter()
        .map(|literal| literal.text.trim().to_ascii_lowercase())
        .filter(|literal| !literal.is_empty())
        .collect::<Vec<_>>();
    let mut score = 0usize;
    for row in rows {
        score = score.saturating_add(setup_focus_row_score(row));
        let text = setup_focus_row_text(row).to_ascii_lowercase();
        score = score.saturating_add(
            literal_terms
                .iter()
                .filter(|literal| text.contains(literal.as_str()))
                .count()
                .saturating_mul(64),
        );
    }
    score.saturating_add(rows.len())
}

pub(super) fn setup_focus_document_label_identity_score(
    document: &KnowledgeDocumentRow,
    query_ir: &QueryIR,
) -> usize {
    let target_sequences = setup_focus_query_label_identity_sequences(query_ir);
    if target_sequences.is_empty() {
        return 0;
    }
    let mut score = 0usize;
    let mut seen_sequences = BTreeSet::<Vec<String>>::new();
    for value in setup_focus_document_identity_values(document) {
        let value_sequence = normalized_alnum_token_sequence(&value, 1);
        if value_sequence.is_empty() {
            continue;
        }
        for target_sequence in &target_sequences {
            if token_sequence_contains_tokens(&value_sequence, target_sequence)
                && seen_sequences.insert(target_sequence.clone())
            {
                score = score.saturating_add(target_sequence.len().max(1));
            }
        }
    }
    score
}

pub(super) fn setup_focus_query_label_identity_sequences(query_ir: &QueryIR) -> Vec<Vec<String>> {
    let mut sequences = Vec::new();
    let mut seen = BTreeSet::<Vec<String>>::new();
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        push_setup_focus_label_identity_sequence(&mut sequences, &mut seen, &document_focus.hint);
    }
    for entity in &query_ir.target_entities {
        push_setup_focus_label_identity_sequence(&mut sequences, &mut seen, &entity.label);
    }
    sequences
}

pub(super) fn push_setup_focus_label_identity_sequence(
    sequences: &mut Vec<Vec<String>>,
    seen: &mut BTreeSet<Vec<String>>,
    value: &str,
) {
    let sequence = normalized_alnum_token_sequence(value, 1);
    if !sequence.is_empty() && seen.insert(sequence.clone()) {
        sequences.push(sequence);
    }
}

pub(super) fn setup_focus_row_has_command_literal_and_path(row: &KnowledgeChunkRow) -> bool {
    let text = setup_focus_row_text(row);
    let command_literal_count = extract_package_command_literals(&text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(&text);
    let setup_signal_count = command_literal_count
        .saturating_add(setup_focus_parameter_assignment_count(&text))
        .saturating_add(setup_focus_parameter_literal_count(&text))
        .saturating_add(setup_focus_section_header_count(&text))
        .saturating_add(setup_focus_url_count(&text));
    configuration_path_count > 0 && setup_signal_count > 0
}

pub(super) fn setup_focus_row_score(row: &KnowledgeChunkRow) -> usize {
    let text = setup_focus_row_text(row);
    let command_literal_count = extract_package_command_literals(&text, 2).len();
    let configuration_path_count = setup_focus_configuration_path_count(&text);
    let assignment_count = setup_focus_parameter_assignment_count(&text);
    let parameter_literal_count = setup_focus_parameter_literal_count(&text);
    let section_count = setup_focus_section_header_count(&text);
    let url_count = setup_focus_url_count(&text);
    command_literal_count
        .saturating_mul(8)
        .saturating_add(configuration_path_count.saturating_mul(12))
        .saturating_add(assignment_count.saturating_mul(4))
        .saturating_add(parameter_literal_count.saturating_mul(6))
        .saturating_add(section_count.saturating_mul(3))
        .saturating_add(url_count.saturating_mul(4))
}

pub(super) fn setup_focus_row_text(row: &KnowledgeChunkRow) -> String {
    format!("{}\n{}", row.content_text, row.window_text.as_deref().unwrap_or_default())
}

pub(super) fn setup_focus_document_chunk_score(chunk: &RuntimeMatchedChunk) -> f32 {
    let command_literal_count =
        extract_package_command_literals(&chunk.source_text, 2).len() as f32;
    let path_count = setup_focus_configuration_path_count(&chunk.source_text) as f32;
    let assignment_count = setup_focus_parameter_assignment_count(&chunk.source_text) as f32;
    let parameter_literal_count = setup_focus_parameter_literal_count(&chunk.source_text) as f32;
    let section_count = setup_focus_section_header_count(&chunk.source_text) as f32;
    let url_count = setup_focus_url_count(&chunk.source_text) as f32;
    SETUP_FOCUS_DOCUMENT_SCORE_BASE
        + command_literal_count * 1_000.0
        + path_count * 500.0
        + assignment_count * 100.0
        + parameter_literal_count * 250.0
        + section_count * 50.0
        + url_count * 100.0
        - chunk.chunk_index.max(0) as f32
}
