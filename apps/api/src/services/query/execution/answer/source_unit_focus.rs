use super::{
    BTreeSet, HashMap, HashSet, QueryIR, SourceUnitField, Uuid, current_question_segment,
    label_terms,
};

#[cfg(test)]
use super::{
    LiteralKind, QueryAct, QueryScope, RuntimeMatchedChunk, focused_excerpt_for,
    focused_record_unit_excerpt, is_structured_source_unit_runtime_chunk, score_value,
    technical_chunk_selection_score, technical_literal_focus_keywords,
};

#[derive(Debug, Clone, Copy, Default)]
struct SourceUnitDocumentFocusScore {
    specific: usize,
    focused: usize,
    total: usize,
}

pub(super) fn source_unit_inventory_focused_document_ids(
    fields: &[SourceUnitField],
    focus: &StructuredSourceUnitFocus,
) -> Option<HashSet<Uuid>> {
    if fields.iter().map(|field| field.document_id).collect::<HashSet<_>>().len() <= 1
        || fields
            .iter()
            .map(|field| field.document_label.trim().to_lowercase())
            .collect::<HashSet<_>>()
            .len()
            <= 1
    {
        return None;
    }

    let mut by_document = HashMap::<Uuid, SourceUnitDocumentFocusScore>::new();
    for field in fields {
        let score = source_unit_document_focus_score(field, focus);
        by_document
            .entry(field.document_id)
            .and_modify(|entry| {
                entry.specific = entry.specific.saturating_add(score.specific);
                entry.focused = entry.focused.saturating_add(score.focused);
                entry.total = entry.total.saturating_add(score.total);
            })
            .or_insert(score);
    }
    if by_document.is_empty() {
        return None;
    }

    let best_specific = by_document.values().map(|score| score.specific).max().unwrap_or_default();
    if best_specific > 0 {
        let selected = by_document
            .into_iter()
            .filter_map(|(document_id, score)| {
                (score.specific == best_specific).then_some(document_id)
            })
            .collect::<HashSet<_>>();
        return (!selected.is_empty()).then_some(selected);
    }

    let mut ranked = by_document.into_iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| {
        right
            .1
            .focused
            .cmp(&left.1.focused)
            .then_with(|| right.1.total.cmp(&left.1.total))
            .then_with(|| left.0.cmp(&right.0))
    });
    let (best_document_id, best_score) = ranked.first().copied()?;
    if best_score.focused == 0 {
        return None;
    }
    let runner = ranked.get(1).map(|(_, score)| *score).unwrap_or_default();
    if best_score.focused >= runner.focused.saturating_add(4)
        || best_score.focused.saturating_mul(2)
            >= runner.focused.saturating_mul(3).saturating_add(1)
    {
        return Some(HashSet::from([best_document_id]));
    }

    None
}

fn source_unit_document_focus_score(
    field: &SourceUnitField,
    focus: &StructuredSourceUnitFocus,
) -> SourceUnitDocumentFocusScore {
    let field_terms = structured_source_unit_field_terms(field);
    let label_terms = exact_label_terms(&field.document_label, 2);
    let field_text =
        format!("{}\n{}\n{}", field.document_label, field.path, field.value).to_lowercase();
    let specific = focus
        .specific_terms
        .iter()
        .filter(|term| {
            let term = term.as_str();
            field_text.contains(term) || field_terms.contains(term) || label_terms.contains(term)
        })
        .count();
    let focused = field_terms.intersection(&focus.scoring_terms).count()
        + label_terms.intersection(&focus.scoring_terms).count();
    let total = field_terms.intersection(&focus.terms).count()
        + label_terms.intersection(&focus.terms).count();
    SourceUnitDocumentFocusScore { specific, focused, total }
}

pub(super) fn structured_source_unit_inventory_matches_specific_terms(
    lines: &[String],
    focus: &StructuredSourceUnitFocus,
) -> bool {
    let haystack = lines.join("\n").to_lowercase();
    if !focus.required_surface_terms.is_empty() {
        return focus.required_surface_terms.iter().any(|term| haystack.contains(term.as_str()));
    }
    if focus.specific_terms.is_empty() {
        return true;
    }
    focus.specific_terms.iter().any(|term| haystack.contains(term.as_str()))
}

pub(super) fn structured_source_unit_inventory_too_broad(
    selected: &[(usize, &SourceUnitField)],
    focus: &StructuredSourceUnitFocus,
) -> bool {
    if selected.len() <= 12 {
        return false;
    }
    let direct_count = selected
        .iter()
        .filter(|(_, field)| structured_source_unit_field_overlap_score(field, focus) > 0)
        .count();
    direct_count.saturating_mul(2) < selected.len()
}

pub(super) fn expand_structured_source_unit_sibling_fields<'a>(
    fields: &'a [SourceUnitField],
    selected: &mut Vec<(usize, &'a SourceUnitField)>,
    focus: &StructuredSourceUnitFocus,
) {
    if selected.is_empty() {
        return;
    }
    let primary_fields = selected.iter().map(|(score, field)| (*score, *field)).collect::<Vec<_>>();
    let mut selected_index_by_identity = selected
        .iter()
        .enumerate()
        .map(|(index, (_, field))| (source_unit_field_identity(field), index))
        .collect::<HashMap<_, _>>();

    for field in fields {
        let identity = source_unit_field_identity(field);
        let score = primary_fields
            .iter()
            .filter_map(|(primary_score, primary)| {
                structured_source_unit_sibling_field_score(field, primary, *primary_score, focus)
            })
            .max()
            .unwrap_or(0);
        if let Some(index) = selected_index_by_identity.get(&identity).copied() {
            selected[index].0 = selected[index].0.max(score);
        } else if score > 0 {
            selected_index_by_identity.insert(identity, selected.len());
            selected.push((score, field));
        }
    }
}

fn structured_source_unit_sibling_field_score(
    field: &SourceUnitField,
    primary: &SourceUnitField,
    primary_score: usize,
    focus: &StructuredSourceUnitFocus,
) -> Option<usize> {
    if field.document_label != primary.document_label
        || field.path == primary.path && field.value == primary.value
    {
        return None;
    }

    let shared_prefix = source_unit_path_common_prefix_len(&field.path, &primary.path);
    let direct_overlap = structured_source_unit_field_overlap_score(field, focus);
    if shared_prefix >= 1
        && primary_score >= 3
        && source_unit_field_root_term(field).as_deref().is_some_and(|root| {
            source_unit_field_root_term(primary).as_deref() == Some(root)
                && focus.compact_root_direct_inventory(root)
        })
    {
        let value_specificity = structured_source_unit_value_specificity_score(&field.value);
        return Some(
            primary_score.max(4)
                + 4
                + direct_overlap.min(6)
                + value_specificity.min(3)
                + source_unit_path_specificity_score(&field.path).min(3),
        );
    }
    if shared_prefix < 3 {
        return None;
    }

    let value_specificity = structured_source_unit_value_specificity_score(&field.value);
    if shared_prefix < 4 && direct_overlap == 0 {
        return None;
    }

    Some(
        shared_prefix.min(8)
            + direct_overlap.min(12)
            + value_specificity.min(10)
            + source_unit_path_specificity_score(&field.path).min(4),
    )
}

fn source_unit_field_identity(field: &SourceUnitField) -> String {
    format!(
        "{}\n{}\n{}\n{}",
        field.document_id,
        field.document_label.trim(),
        field.path.trim(),
        field.value.trim()
    )
    .to_lowercase()
}

fn source_unit_path_common_prefix_len(left: &str, right: &str) -> usize {
    source_unit_path_segments(left)
        .into_iter()
        .zip(source_unit_path_segments(right))
        .take_while(|(left, right)| left == right)
        .count()
}

fn source_unit_path_segments(path: &str) -> Vec<String> {
    path.split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect()
}

fn source_unit_path_specificity_score(path: &str) -> usize {
    let segment_count = source_unit_path_segments(path).len();
    segment_count.saturating_sub(2)
}

fn structured_source_unit_value_specificity_score(value: &str) -> usize {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0;
    }
    let token_score = exact_label_terms(trimmed, 2).len().min(5);
    let syntax_score =
        if trimmed.chars().any(|ch| matches!(ch, '.' | '-' | '_' | '/' | ':' | '@' | '#')) {
            3
        } else {
            0
        };
    let digit_score = usize::from(trimmed.chars().any(|ch| ch.is_ascii_digit())) * 2;
    let mixed_case_score = usize::from(
        trimmed.chars().any(|ch| ch.is_ascii_uppercase())
            && trimmed.chars().any(|ch| ch.is_ascii_lowercase()),
    );
    let length_score = match trimmed.chars().count() {
        0..=3 => 0,
        4..=11 => 1,
        _ => 3,
    };
    token_score + syntax_score + digit_score + mixed_case_score + length_score
}

pub(super) fn structured_source_unit_inventory_focus_terms(
    question: &str,
    query_ir: &QueryIR,
) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    extend_exact_label_terms(&mut terms, current_question_segment(question), 3);
    for entity in &query_ir.target_entities {
        extend_exact_label_terms(&mut terms, &entity.label, 2);
    }
    for literal in &query_ir.literal_constraints {
        extend_exact_label_terms(&mut terms, &literal.text, 2);
    }
    if let Some(comparison) = query_ir.comparison.as_ref() {
        if let Some(a) = comparison.a.as_deref() {
            extend_exact_label_terms(&mut terms, a, 2);
        }
        if let Some(b) = comparison.b.as_deref() {
            extend_exact_label_terms(&mut terms, b, 2);
        }
        extend_exact_label_terms(&mut terms, &comparison.dimension, 2);
    }
    terms
}

#[derive(Debug, Clone)]
pub(super) struct StructuredSourceUnitFocus {
    terms: BTreeSet<String>,
    scoring_terms: BTreeSet<String>,
    root_terms: BTreeSet<String>,
    specific_terms: BTreeSet<String>,
    required_surface_terms: BTreeSet<String>,
    root_field_counts: HashMap<String, usize>,
}

impl StructuredSourceUnitFocus {
    pub(super) fn new(question: &str, query_ir: &QueryIR, fields: &[SourceUnitField]) -> Self {
        let terms = structured_source_unit_inventory_focus_terms(question, query_ir);
        if terms.is_empty() {
            return Self {
                terms,
                scoring_terms: BTreeSet::new(),
                root_terms: BTreeSet::new(),
                specific_terms: BTreeSet::new(),
                required_surface_terms: BTreeSet::new(),
                root_field_counts: HashMap::new(),
            };
        }

        let broad_limit =
            if fields.len() <= 8 { fields.len() } else { (fields.len() / 4).clamp(3, 8) };
        let mut field_counts = HashMap::<String, usize>::new();
        let mut root_field_counts = HashMap::<String, usize>::new();
        for field in fields {
            if let Some(root) = source_unit_field_root_term(field) {
                *root_field_counts.entry(root).or_insert(0) += 1;
            }
            let field_terms = structured_source_unit_field_terms(field);
            for term in terms.intersection(&field_terms) {
                *field_counts.entry(term.clone()).or_insert(0) += 1;
            }
        }

        let root_terms = fields
            .iter()
            .filter_map(source_unit_field_root_term)
            .flat_map(|segment| exact_label_terms(&segment, 2))
            .filter(|term| terms.contains(term))
            .collect::<BTreeSet<_>>();

        let scoring_terms = terms
            .iter()
            .filter(|term| !root_terms.contains(*term))
            .filter(|term| field_counts.get(*term).copied().unwrap_or(0) <= broad_limit)
            .cloned()
            .collect::<BTreeSet<_>>();
        let specific_terms = structured_source_unit_specific_terms(question, query_ir)
            .into_iter()
            .filter(|term| !root_terms.contains(term))
            .collect();
        let required_surface_terms =
            structured_source_unit_required_surface_terms(question, query_ir);
        Self {
            terms,
            scoring_terms,
            root_terms,
            specific_terms,
            required_surface_terms,
            root_field_counts,
        }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.terms.is_empty() || self.scoring_terms.is_empty()
    }

    fn compact_root_direct_inventory(&self, root: &str) -> bool {
        self.root_terms.contains(root)
            && self.root_field_counts.get(root).copied().unwrap_or(0) <= 8
    }
}

fn structured_source_unit_specific_terms(question: &str, query_ir: &QueryIR) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    for term in distinctive_surface_terms(question) {
        terms.insert(term);
    }
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        extend_exact_label_terms(&mut terms, &document_focus.hint, 3);
    }
    for entity in &query_ir.target_entities {
        extend_exact_label_terms(&mut terms, &entity.label, 3);
    }
    for literal in &query_ir.literal_constraints {
        extend_exact_label_terms(&mut terms, &literal.text, 2);
    }
    if let Some(comparison) = query_ir.comparison.as_ref() {
        if let Some(a) = comparison.a.as_deref() {
            extend_exact_label_terms(&mut terms, a, 3);
        }
        if let Some(b) = comparison.b.as_deref() {
            extend_exact_label_terms(&mut terms, b, 3);
        }
    }
    terms
}

fn structured_source_unit_required_surface_terms(
    question: &str,
    query_ir: &QueryIR,
) -> BTreeSet<String> {
    let mut terms = distinctive_surface_terms(question);
    for literal in &query_ir.literal_constraints {
        terms.extend(distinctive_surface_terms(&literal.text));
    }
    terms
}

fn distinctive_surface_terms(text: &str) -> BTreeSet<String> {
    text.split_whitespace()
        .filter_map(|raw| {
            let candidate = raw
                .trim_matches(|ch: char| {
                    !(ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'))
                })
                .trim();
            if candidate.chars().filter(|ch| ch.is_alphanumeric()).count() < 3 {
                return None;
            }
            let has_structural_separator =
                candidate.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':' | '@'));
            let has_digit = candidate.chars().any(|ch| ch.is_ascii_digit());
            let has_upper_after_first = candidate.chars().skip(1).any(|ch| ch.is_ascii_uppercase());
            let has_lower = candidate.chars().any(|ch| ch.is_ascii_lowercase());
            if !(has_structural_separator || has_digit || (has_upper_after_first && has_lower)) {
                return None;
            }
            Some(candidate.to_lowercase())
        })
        .collect()
}

fn structured_source_unit_field_terms(field: &SourceUnitField) -> BTreeSet<String> {
    exact_label_terms(&field.path, 2)
        .into_iter()
        .chain(exact_label_terms(&field.value, 2))
        .collect()
}

fn source_unit_field_root_term(field: &SourceUnitField) -> Option<String> {
    source_unit_path_segments(&field.path).into_iter().next()
}

pub(super) fn structured_source_unit_field_overlap_score(
    field: &SourceUnitField,
    focus: &StructuredSourceUnitFocus,
) -> usize {
    let path_terms = exact_label_terms(&field.path, 2);
    let value_terms = exact_label_terms(&field.value, 2);
    let path_score = path_terms.intersection(&focus.scoring_terms).count() * 4;
    let value_score = value_terms.intersection(&focus.scoring_terms).count() * 3;
    path_score + value_score
}

pub(super) fn structured_source_unit_compact_root_inventory_score(
    field: &SourceUnitField,
    focus: &StructuredSourceUnitFocus,
) -> usize {
    let Some(root) = source_unit_field_root_term(field) else {
        return 0;
    };
    if !focus.compact_root_direct_inventory(&root) {
        return 0;
    }
    2 + structured_source_unit_value_specificity_score(&field.value).min(3)
        + source_unit_path_specificity_score(&field.path).min(3)
}

fn exact_label_terms(text: &str, min_token_chars: usize) -> BTreeSet<String> {
    let mut terms = BTreeSet::<String>::new();
    extend_exact_label_terms(&mut terms, text, min_token_chars);
    terms
}

fn extend_exact_label_terms(terms: &mut BTreeSet<String>, text: &str, min_token_chars: usize) {
    for term in label_terms(text, min_token_chars) {
        if term.chars().count() >= min_token_chars {
            terms.insert(term);
        }
    }
}

#[cfg(test)]
pub(super) fn build_structured_source_unit_field_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !structured_source_unit_field_answer_allowed(query_ir) {
        return None;
    }
    let focus_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    if focus_keywords.is_empty() {
        return None;
    }
    let mut candidates = chunks
        .iter()
        .filter(|chunk| is_structured_source_unit_runtime_chunk(chunk))
        .filter_map(|chunk| {
            let score = technical_chunk_selection_score(
                &format!("{}\n{}", chunk.excerpt, chunk.source_text),
                &focus_keywords,
                false,
            );
            (score > 0).then_some((score, chunk))
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }
    candidates.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
            .then_with(|| left.document_label.cmp(&right.document_label))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });

    let mut lines = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for (_, chunk) in candidates.into_iter().take(4) {
        let excerpt = focused_record_unit_excerpt(&chunk.source_text, &focus_keywords, 1_200)
            .or_else(|| focused_record_unit_excerpt(&chunk.excerpt, &focus_keywords, 1_200))
            .unwrap_or_else(|| focused_excerpt_for(&chunk.source_text, &focus_keywords, 1_200));
        let excerpt = excerpt.trim();
        if excerpt.is_empty() {
            continue;
        }
        let key = format!("{}\n{}", chunk.document_label, excerpt).to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        lines.push(format!("`{}`: {}", chunk.document_label.trim(), excerpt));
    }
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
fn structured_source_unit_field_answer_allowed(query_ir: &QueryIR) -> bool {
    if !matches!(query_ir.scope, QueryScope::SingleDocument)
        || !matches!(query_ir.act, QueryAct::RetrieveValue)
        || query_ir.target_entities.len() > 1
    {
        return false;
    }
    query_ir.is_exact_literal_technical()
        || (query_ir.target_entities.len() <= 1
            && query_ir.literal_constraints.iter().any(|literal| {
                matches!(literal.kind, LiteralKind::Identifier | LiteralKind::Path)
                    && literal.text.chars().any(|ch| matches!(ch, '_' | '-' | '.' | '/' | ':'))
            }))
}
