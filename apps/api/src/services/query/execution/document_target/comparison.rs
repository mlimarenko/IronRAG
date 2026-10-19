use super::matching::{
    COMPARISON_DOCUMENT_TARGET_EXTRA_TOKEN_LIMIT, COMPARISON_DOCUMENT_TARGET_MIN_TOKENS,
    ranked_document_target_candidates,
};
use super::{
    BTreeSet, EntityRole, QueryAct, QueryIR, QueryScope, RuntimeMatchedChunk, Uuid,
    normalize_document_target_text, normalized_alnum_tokens,
};

pub(crate) fn question_requests_multi_document_scope(
    _question: &str,
    ir: Option<&QueryIR>,
) -> bool {
    ir.is_some_and(QueryIR::is_multi_document)
}

pub(crate) fn comparison_target_document_ids_from_values<'a, I>(
    ir: &QueryIR,
    values: I,
) -> Vec<Uuid>
where
    I: IntoIterator<Item = (Uuid, &'a str)>,
{
    if !matches!(ir.act, QueryAct::Compare) || !matches!(ir.scope, QueryScope::MultiDocument) {
        return Vec::new();
    }

    let values = values.into_iter().collect::<Vec<_>>();
    let mut resolved = BTreeSet::new();
    for hint in comparison_document_target_hints(ir) {
        let matches = values
            .iter()
            .filter_map(|(document_id, label)| {
                comparison_hint_matches_document_identity(&hint, label).then_some(*document_id)
            })
            .collect::<BTreeSet<_>>();
        if matches.len() == 1 {
            resolved.extend(matches);
        }
    }

    let mut seen = BTreeSet::new();
    values
        .into_iter()
        .filter_map(|(document_id, _)| {
            (resolved.contains(&document_id) && seen.insert(document_id)).then_some(document_id)
        })
        .collect()
}

pub(crate) fn promote_comparison_target_chunks(ir: &QueryIR, chunks: &mut [RuntimeMatchedChunk]) {
    let target_document_ids = comparison_target_document_ids_from_values(
        ir,
        chunks.iter().map(|chunk| (chunk.document_id, chunk.document_label.as_str())),
    )
    .into_iter()
    .collect::<BTreeSet<_>>();
    if target_document_ids.is_empty() {
        return;
    }
    chunks.sort_by_key(|chunk| !target_document_ids.contains(&chunk.document_id));
}

fn comparison_document_target_hints(ir: &QueryIR) -> Vec<String> {
    let mut hints = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push = |value: &str| {
        let normalized = normalize_document_target_text(value);
        if !normalized.is_empty() && seen.insert(normalized) {
            hints.push(value.to_string());
        }
    };

    for entity in &ir.target_entities {
        if matches!(entity.role, EntityRole::Subject | EntityRole::Object) {
            push(&entity.label);
        }
    }
    if let Some(comparison) = &ir.comparison {
        if let Some(side) = comparison.a.as_deref() {
            push(side);
        }
        if let Some(side) = comparison.b.as_deref() {
            push(side);
        }
    }
    if let Some(document_focus) = &ir.document_focus {
        push(&document_focus.hint);
    }
    for literal in &ir.literal_constraints {
        push(&literal.text);
    }
    hints
}

fn comparison_hint_matches_document_identity(hint: &str, document_label: &str) -> bool {
    let normalized_hint = normalize_document_target_text(hint);
    if normalized_hint.is_empty() {
        return false;
    }
    let hint_tokens =
        normalized_alnum_tokens(&normalized_hint, COMPARISON_DOCUMENT_TARGET_MIN_TOKENS);
    ranked_document_target_candidates([document_label]).into_iter().any(|candidate| {
        if candidate.text == normalized_hint {
            return true;
        }
        if hint_tokens.len() < COMPARISON_DOCUMENT_TARGET_MIN_TOKENS {
            return false;
        }
        let identity_tokens =
            normalized_alnum_tokens(&candidate.text, COMPARISON_DOCUMENT_TARGET_MIN_TOKENS);
        hint_tokens.is_subset(&identity_tokens)
            && identity_tokens.len().saturating_sub(hint_tokens.len())
                <= COMPARISON_DOCUMENT_TARGET_EXTRA_TOKEN_LIMIT
    })
}
