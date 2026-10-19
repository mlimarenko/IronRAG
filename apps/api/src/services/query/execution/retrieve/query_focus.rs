use chrono::Datelike;

use super::super::chunk_support::document_has_exact_readable_revision;
use super::{
    BTreeSet, DateTime, EntityRole, HashMap, KnowledgeDocumentRow,
    LINKED_ANCHOR_CONTEXT_PREFIX_CHARS, LINKED_ANCHOR_CONTEXT_QUERY_CAP,
    LINKED_ANCHOR_CONTEXT_QUERY_PREFIX_CHARS, QueryAct, QueryIR, QueryScope, RuntimeMatchedChunk,
    RuntimeQueryPlan, TemporalConstraint, Utc, Uuid, common_prefix_char_count,
    normalized_alnum_token_sequence, normalized_alnum_tokens,
    query_ir_has_focused_document_answer_intent,
};

pub(crate) fn query_ir_lexical_focus_queries(query_ir: &QueryIR) -> Vec<String> {
    const MAX_QUERY_IR_FOCUS_QUERIES: usize = 6;
    const MAX_QUERY_IR_FOCUS_CHARS: usize = 160;

    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();
    let mut push_focus = |value: &str, queries: &mut Vec<String>| {
        if queries.len() >= MAX_QUERY_IR_FOCUS_QUERIES {
            return;
        }
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized) || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        let bounded = normalized.chars().take(MAX_QUERY_IR_FOCUS_CHARS).collect::<String>();
        queries.push(bounded);
    };

    let (mut primary_focus_values, mut modifier_focus_values) =
        query_ir_focus_value_groups(query_ir);
    sort_query_ir_focus_values_by_specificity(&mut primary_focus_values);
    sort_query_ir_focus_values_by_specificity(&mut modifier_focus_values);
    if query_ir_document_focus_should_anchor_focus_queries(query_ir)
        && let Some(document_focus) = &query_ir.document_focus
    {
        let mut anchored_compounds =
            document_focus_anchored_focus_compounds(&document_focus.hint, &primary_focus_values);
        sort_query_ir_focus_values_by_specificity(&mut anchored_compounds);
        for compound in &anchored_compounds {
            push_focus(compound, &mut queries);
        }
        push_focus(&document_focus.hint, &mut queries);
    }
    let compound_values = query_ir_compound_focus_values(query_ir);
    let mut compounds = adjacent_query_ir_focus_compounds(&compound_values);
    sort_query_ir_focus_values_by_specificity(&mut compounds);
    for compound in &compounds {
        push_focus(compound, &mut queries);
    }
    for focus in &primary_focus_values {
        push_focus(focus, &mut queries);
    }
    for focus in &modifier_focus_values {
        push_focus(focus, &mut queries);
    }
    if queries.is_empty()
        && let Some(document_focus) = &query_ir.document_focus
    {
        push_focus(&document_focus.hint, &mut queries);
    }
    queries
}

pub(super) fn query_ir_document_focus_should_anchor_focus_queries(query_ir: &QueryIR) -> bool {
    query_ir.document_focus.is_some()
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && matches!(query_ir.act, QueryAct::Compare | QueryAct::ConfigureHow)
}

pub(super) fn document_focus_anchored_focus_compounds(
    document_focus: &str,
    primary_focus_values: &[String],
) -> Vec<String> {
    let normalized_focus = document_focus.split_whitespace().collect::<Vec<_>>().join(" ");
    if !is_usable_query_ir_focus(&normalized_focus) {
        return Vec::new();
    }
    let focus_key = normalized_focus.to_lowercase();
    let mut seen = BTreeSet::new();
    let mut compounds = Vec::new();
    for primary in primary_focus_values {
        let normalized_primary = primary.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized_primary)
            || normalized_primary.to_lowercase() == focus_key
        {
            continue;
        }
        let compound = format!("{normalized_focus} {normalized_primary}");
        if seen.insert(compound.to_lowercase()) {
            compounds.push(compound);
        }
    }
    compounds
}

pub(super) fn query_ir_compound_focus_values(query_ir: &QueryIR) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    let mut push_value = |value: &str, values: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized) || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        values.push(normalized);
    };

    let focus_uses_target_entities = query_ir_has_focused_document_answer_intent(query_ir)
        && !query_ir.target_entities.is_empty();
    if focus_uses_target_entities {
        let (primary_entity_values, _) = query_ir_entity_focus_value_groups(query_ir);
        for entity_value in primary_entity_values {
            push_value(&entity_value, &mut values);
        }
    } else {
        for literal in &query_ir.literal_constraints {
            push_value(&literal.text, &mut values);
        }
        let (primary_entity_values, _) = query_ir_entity_focus_value_groups(query_ir);
        for entity_value in primary_entity_values {
            push_value(&entity_value, &mut values);
        }
    }
    values
}

pub(super) fn query_ir_entity_focus_value_groups(query_ir: &QueryIR) -> (Vec<String>, Vec<String>) {
    let mut primary_values = Vec::new();
    let mut modifier_values = Vec::new();
    for entity in &query_ir.target_entities {
        match entity.role {
            EntityRole::Subject | EntityRole::Object => {
                primary_values.push(entity.label.clone());
            }
            EntityRole::Modifier => {
                modifier_values.push(entity.label.clone());
            }
        }
    }
    (primary_values, modifier_values)
}

pub(super) fn push_unique_query_ir_focus(
    value: &str,
    seen: &mut BTreeSet<String>,
    values: &mut Vec<String>,
) {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if is_usable_query_ir_focus(&normalized) && seen.insert(normalized.to_lowercase()) {
        values.push(normalized);
    }
}

pub(super) fn push_query_ir_focus_values(
    values_to_add: impl IntoIterator<Item = String>,
    seen: &mut BTreeSet<String>,
    values: &mut Vec<String>,
) {
    for value in values_to_add {
        push_unique_query_ir_focus(&value, seen, values);
    }
}

pub(super) fn query_ir_focus_value_groups(query_ir: &QueryIR) -> (Vec<String>, Vec<String>) {
    let mut seen = BTreeSet::new();
    let mut primary_values = Vec::new();
    let mut modifier_values = Vec::new();
    push_query_ir_focus_values(
        query_ir.temporal_constraints.iter().flat_map(temporal_constraint_focus_values),
        &mut seen,
        &mut primary_values,
    );
    let focus_uses_target_entities = query_ir_has_focused_document_answer_intent(query_ir)
        && !query_ir.target_entities.is_empty();
    if !focus_uses_target_entities {
        push_query_ir_focus_values(
            query_ir.literal_constraints.iter().map(|literal| literal.text.clone()),
            &mut seen,
            &mut primary_values,
        );
    }
    let (primary_entity_values, modifier_entity_values) =
        query_ir_entity_focus_value_groups(query_ir);
    push_query_ir_focus_values(primary_entity_values, &mut seen, &mut primary_values);
    push_query_ir_focus_values(modifier_entity_values, &mut seen, &mut modifier_values);
    (primary_values, modifier_values)
}

pub(super) fn adjacent_query_ir_focus_compounds(focus_values: &[String]) -> Vec<String> {
    if focus_values.len() < 2 {
        return Vec::new();
    }
    focus_values.windows(2).map(|window| window.join(" ")).collect::<Vec<_>>()
}

pub(super) fn sort_query_ir_focus_values_by_specificity(values: &mut [String]) {
    values.sort_by(|left, right| {
        query_ir_focus_specificity_score(right)
            .cmp(&query_ir_focus_specificity_score(left))
            .then_with(|| left.cmp(right))
    });
}

pub(super) fn query_ir_focus_specificity_score(value: &str) -> usize {
    let tokens = normalized_alnum_token_sequence(value, 2);
    if tokens.is_empty() {
        return 0;
    }
    let token_score = tokens
        .iter()
        .map(|token| {
            let char_count = token.chars().count();
            let numeric_bonus = token.chars().any(char::is_numeric) as usize * 16;
            char_count.saturating_add(numeric_bonus)
        })
        .sum::<usize>();
    let structural_bonus =
        value.chars().any(|ch| !ch.is_alphanumeric() && !ch.is_whitespace()) as usize * 8;
    token_score.saturating_add(tokens.len().saturating_mul(4)).saturating_add(structural_bonus)
}

pub(super) fn temporal_constraint_focus_values(temporal: &TemporalConstraint) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut values = Vec::new();
    let mut push = |value: String, values: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        values.push(normalized);
    };

    let start = temporal.start.as_deref().and_then(parse_temporal_bound);
    let end = temporal.end.as_deref().and_then(parse_temporal_bound);
    if let Some(start) = start {
        for prefix in temporal_bound_prefixes(start, end) {
            push(prefix, &mut values);
        }
    }
    if values.is_empty() {
        push(temporal.surface.clone(), &mut values);
    }
    values
}

pub(super) fn parse_temporal_bound(value: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value).ok().map(|parsed| parsed.with_timezone(&Utc))
}

pub(super) fn temporal_bound_prefixes(
    start: DateTime<Utc>,
    end: Option<DateTime<Utc>>,
) -> Vec<String> {
    const MAX_TEMPORAL_PREFIXES: usize = 8;

    let mut prefixes = Vec::new();
    let push_unique = |value: String, prefixes: &mut Vec<String>| {
        if prefixes.len() >= MAX_TEMPORAL_PREFIXES
            || prefixes.iter().any(|existing| existing == &value)
        {
            return;
        }
        prefixes.push(value);
    };

    let day_prefix = iso_day_prefix(start);
    let month_prefix = iso_month_prefix(start.year(), start.month());
    let range_seconds = end.map(|end| end.signed_duration_since(start).num_seconds());
    let single_day_range = range_seconds.is_some_and(|seconds| seconds > 0 && seconds <= 86_400);

    if single_day_range {
        push_unique(day_prefix, &mut prefixes);
        push_unique(month_prefix, &mut prefixes);
    } else {
        if let Some(end) = end {
            let month_span = temporal_month_span(start, end);
            if month_span > 6 {
                push_unique(format!("{:04}", start.year()), &mut prefixes);
            }
            for (year, month) in temporal_month_prefix_range(start, end) {
                push_unique(iso_month_prefix(year, month), &mut prefixes);
            }
        }
        push_unique(month_prefix, &mut prefixes);
        push_unique(day_prefix, &mut prefixes);
    }

    prefixes
}

pub(super) fn temporal_month_prefix_range(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
) -> impl Iterator<Item = (i32, u32)> {
    let end_key = (end.year(), end.month());
    let mut year = start.year();
    let mut month = start.month();
    std::iter::from_fn(move || {
        if (year, month) >= end_key {
            return None;
        }
        let current = (year, month);
        (year, month) = next_month(year, month);
        Some(current)
    })
    .take(12)
}

pub(super) fn temporal_month_span(start: DateTime<Utc>, end: DateTime<Utc>) -> i32 {
    let years = end.year().saturating_sub(start.year());
    let end_month = i32::try_from(end.month()).unwrap_or(12);
    let start_month = i32::try_from(start.month()).unwrap_or(1);
    years.saturating_mul(12).saturating_add(end_month.saturating_sub(start_month))
}

pub(super) const fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month >= 12 { (year + 1, 1) } else { (year, month + 1) }
}

pub(super) fn iso_day_prefix(value: DateTime<Utc>) -> String {
    format!("{:04}-{:02}-{:02}", value.year(), value.month(), value.day())
}

pub(super) fn iso_month_prefix(year: i32, month: u32) -> String {
    format!("{year:04}-{month:02}")
}

pub(crate) fn query_ir_focus_search_queries(
    question: &str,
    focus_queries: &[String],
) -> Vec<String> {
    const MAX_QUERY_IR_FOCUS_SEARCH_QUERIES: usize = 5;
    const MAX_QUERY_IR_FOCUS_CHARS: usize = 160;

    let mut seen = BTreeSet::new();
    let mut queries = Vec::new();
    let mut push_focus = |value: &str, queries: &mut Vec<String>| {
        if queries.len() >= MAX_QUERY_IR_FOCUS_SEARCH_QUERIES {
            return;
        }
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !is_usable_query_ir_focus(&normalized) || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        queries.push(normalized.chars().take(MAX_QUERY_IR_FOCUS_CHARS).collect());
    };

    for focus_query in focus_queries {
        push_focus(focus_query, &mut queries);
    }
    if queries.is_empty() {
        push_focus(question, &mut queries);
    }

    queries
}

pub(super) fn linked_anchor_scored_labels(
    chunks: &[RuntimeMatchedChunk],
    focus_tokens: &BTreeSet<String>,
) -> Vec<(usize, String)> {
    let mut seen = BTreeSet::new();
    let mut scored = chunks
        .iter()
        .flat_map(|chunk| {
            markdown_link_labels(&chunk.source_text)
                .into_iter()
                .chain(markdown_link_labels(&chunk.excerpt))
        })
        .map(|label| label.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|label| {
            is_usable_query_ir_focus(label)
                && label.chars().count() <= 120
                && seen.insert(label.to_lowercase())
        })
        .filter_map(|label| {
            let overlap = linked_anchor_token_overlap(&label, focus_tokens);
            (overlap > 0).then_some((overlap, label))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(&right.1)));
    scored
}

pub(crate) fn linked_anchor_focus_queries(
    question: &str,
    query_ir: Option<&QueryIR>,
    plan_keywords: &[String],
    chunks: &[RuntimeMatchedChunk],
) -> Vec<String> {
    let focus_tokens = linked_anchor_focus_tokens(question, query_ir, plan_keywords);
    if focus_tokens.is_empty() {
        return Vec::new();
    }

    let scored_labels = linked_anchor_scored_labels(chunks, &focus_tokens);
    let mut queries = Vec::new();
    let mut seen_queries = BTreeSet::new();
    for (_, label) in scored_labels {
        for query in linked_anchor_query_variants(&label) {
            if seen_queries.insert(query.to_lowercase()) {
                queries.push(query);
            }
            if queries.len() >= LINKED_ANCHOR_CONTEXT_QUERY_CAP {
                return queries;
            }
        }
    }
    queries
}

pub(super) fn linked_anchor_query_variants(label: &str) -> Vec<String> {
    let normalized = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return Vec::new();
    }

    let mut variants = Vec::new();
    let mut seen = BTreeSet::new();
    let mut push_variant = |value: String, variants: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if !normalized.is_empty() && seen.insert(normalized.to_lowercase()) {
            variants.push(normalized);
        }
    };

    push_variant(normalized.clone(), &mut variants);

    let lexical_tokens = normalized_alnum_token_sequence(&normalized, 2)
        .into_iter()
        .filter(|token| token.chars().any(|ch| ch.is_alphabetic()))
        .collect::<Vec<_>>();
    if lexical_tokens.is_empty() {
        return variants;
    }

    push_variant(lexical_tokens.join(" "), &mut variants);
    for (index, token) in lexical_tokens.iter().enumerate() {
        if token.chars().count() <= LINKED_ANCHOR_CONTEXT_QUERY_PREFIX_CHARS {
            continue;
        }
        let mut prefix_tokens = lexical_tokens.clone();
        prefix_tokens[index] =
            token.chars().take(LINKED_ANCHOR_CONTEXT_QUERY_PREFIX_CHARS).collect();
        push_variant(prefix_tokens.join(" "), &mut variants);
    }

    variants
}

pub(super) fn linked_anchor_focus_tokens(
    question: &str,
    query_ir: Option<&QueryIR>,
    plan_keywords: &[String],
) -> BTreeSet<String> {
    let mut tokens = normalized_alnum_tokens(question, 3);
    for keyword in plan_keywords {
        tokens.extend(normalized_alnum_tokens(keyword, 3));
    }
    if let Some(query_ir) = query_ir {
        if let Some(document_focus) = query_ir.document_focus.as_ref() {
            tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
        }
        for entity in &query_ir.target_entities {
            tokens.extend(normalized_alnum_tokens(&entity.label, 3));
        }
        for literal in &query_ir.literal_constraints {
            tokens.extend(normalized_alnum_tokens(&literal.text, 3));
        }
    }
    tokens
}

pub(super) fn markdown_link_labels(value: &str) -> Vec<&str> {
    let mut labels = Vec::new();
    let mut search_from = 0;
    while let Some(open_rel) = value[search_from..].find('[') {
        let open = search_from + open_rel;
        let label_start = open + '['.len_utf8();
        let Some(close_rel) = value[label_start..].find(']') else {
            break;
        };
        let close = label_start + close_rel;
        let after_close = close + ']'.len_utf8();
        if value[after_close..].starts_with('(') {
            let label = value[label_start..close].trim();
            if !label.is_empty() {
                labels.push(label);
            }
        }
        search_from = after_close;
    }
    labels
}

pub(super) fn linked_anchor_token_overlap(label: &str, focus_tokens: &BTreeSet<String>) -> usize {
    let label_tokens = normalized_alnum_tokens(label, 3);
    label_tokens
        .iter()
        .filter(|label_token| {
            focus_tokens
                .iter()
                .any(|focus_token| linked_anchor_token_match(label_token, focus_token))
        })
        .count()
}

pub(super) fn linked_anchor_token_match(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    let min_len = left_len.min(right_len);
    if min_len < 4 {
        return false;
    }
    left.contains(right)
        || right.contains(left)
        || common_prefix_char_count(left, right) >= LINKED_ANCHOR_CONTEXT_PREFIX_CHARS
}

pub(super) fn is_usable_query_ir_focus(value: &str) -> bool {
    let alphanumeric_count = value.chars().filter(|ch| ch.is_alphanumeric()).count();
    alphanumeric_count >= 2
}

pub(crate) fn request_safe_query(plan: &RuntimeQueryPlan) -> String {
    if !plan.low_level_keywords.is_empty() {
        let combined =
            format!("{} {}", plan.high_level_keywords.join(" "), plan.low_level_keywords.join(" "));
        return combined.trim().to_string();
    }
    plan.keywords.join(" ")
}

pub(crate) fn retain_current_document_head_chunks(
    chunks: &mut Vec<RuntimeMatchedChunk>,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
) -> usize {
    let before = chunks.len();
    chunks.retain(|chunk| {
        document_index.get(&chunk.document_id).is_some_and(|document| {
            document_has_exact_readable_revision(document, chunk.revision_id)
        })
    });
    before.saturating_sub(chunks.len())
}
