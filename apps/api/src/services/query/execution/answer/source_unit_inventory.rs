use super::{
    BTreeSet, HashSet, LiteralKind, QueryAct, QueryIR, QueryTargetKind, RuntimeMatchedChunk,
    StructuredSourceUnitFocus, Uuid, expand_structured_source_unit_sibling_fields,
    is_source_profile_runtime_chunk, is_structured_source_unit_runtime_chunk,
    parse_source_unit_text, query_requests_latest_versions, score_value,
    source_unit_inventory_focused_document_ids,
    structured_source_unit_compact_root_inventory_score,
    structured_source_unit_field_overlap_score, structured_source_unit_inventory_focus_terms,
    structured_source_unit_inventory_matches_specific_terms,
    structured_source_unit_inventory_too_broad,
};

const STRUCTURED_SOURCE_UNIT_INVENTORY_TARGETS: &[QueryTargetKind] = &[
    QueryTargetKind::Attribute,
    QueryTargetKind::BaseUrl,
    QueryTargetKind::ConfigKey,
    QueryTargetKind::Connection,
    QueryTargetKind::Credential,
    QueryTargetKind::Endpoint,
    QueryTargetKind::Entry,
    QueryTargetKind::EnvVar,
    QueryTargetKind::ErrorCode,
    QueryTargetKind::Event,
    QueryTargetKind::Field,
    QueryTargetKind::Flag,
    QueryTargetKind::Group,
    QueryTargetKind::Item,
    QueryTargetKind::Parameter,
    QueryTargetKind::Port,
    QueryTargetKind::Record,
    QueryTargetKind::Service,
    QueryTargetKind::State,
    QueryTargetKind::Status,
    QueryTargetKind::TableRow,
    QueryTargetKind::TableSummary,
    QueryTargetKind::Url,
    QueryTargetKind::Value,
];

pub(in crate::services::query::execution) fn build_structured_source_unit_inventory_answer(
    question: &str,
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    if !query_ir_allows_structured_source_unit_inventory(query_ir) {
        return None;
    }
    let fields = collect_source_unit_fields_for_question(chunks, question, query_ir);
    if fields.is_empty() {
        return None;
    }
    build_structured_source_unit_overlap_inventory_answer(question, query_ir, &fields)
}

pub(super) fn query_ir_allows_structured_source_unit_inventory(query_ir: &QueryIR) -> bool {
    if query_ir_has_typed_table_column_inventory_intent(query_ir) {
        return false;
    }

    if query_requests_latest_versions(query_ir)
        || query_ir.source_slice.as_ref().is_some_and(|slice| {
            matches!(slice.filter, crate::domains::query_ir::SourceSliceFilter::ReleaseMarker)
        })
    {
        return false;
    }

    if query_ir_is_named_document_overview(query_ir) {
        return false;
    }

    if query_ir.source_slice.is_some()
        || query_ir_has_inventory_field_literals(query_ir)
        || query_ir.comparison.is_some()
    {
        return true;
    }

    if matches!(query_ir.act, QueryAct::Enumerate | QueryAct::RetrieveValue)
        && query_ir.targets_any(STRUCTURED_SOURCE_UNIT_INVENTORY_TARGETS)
    {
        return true;
    }

    if !matches!(query_ir.act, QueryAct::Compare | QueryAct::Describe | QueryAct::ConfigureHow) {
        return false;
    }

    query_ir.targets_any(STRUCTURED_SOURCE_UNIT_INVENTORY_TARGETS)
}

fn query_ir_is_named_document_overview(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some() || query_ir.comparison.is_some() {
        return false;
    }
    let only_document = query_ir.target_types.is_empty()
        || query_ir.target_types.iter().all(|kind| matches!(kind, QueryTargetKind::Document));
    if !only_document {
        return false;
    }
    let has_document_name =
        query_ir.document_focus.as_ref().is_some_and(|focus| !focus.hint.trim().is_empty())
            || query_ir.target_entities.iter().any(|entity| !entity.label.trim().is_empty());
    has_document_name && !query_ir_has_inventory_field_literals(query_ir)
}

fn query_ir_has_inventory_field_literals(query_ir: &QueryIR) -> bool {
    query_ir
        .literal_constraints
        .iter()
        .any(|literal| !matches!(literal.kind, LiteralKind::Path | LiteralKind::Url))
}

pub(super) fn query_ir_has_typed_table_column_inventory_intent(query_ir: &QueryIR) -> bool {
    if query_ir.source_slice.is_some()
        || !matches!(
            query_ir.act,
            QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue
        )
    {
        return false;
    }
    query_ir.targets(QueryTargetKind::TableRow) && query_ir.targets(QueryTargetKind::TableSummary)
}

#[derive(Debug, Clone)]
pub(super) struct SourceUnitField {
    pub(super) document_id: Uuid,
    pub(super) document_label: String,
    pub(super) path: String,
    pub(super) value: String,
}

fn collect_source_unit_fields_for_question(
    chunks: &[RuntimeMatchedChunk],
    question: &str,
    query_ir: &QueryIR,
) -> Vec<SourceUnitField> {
    collect_source_unit_fields_with_document_filter(
        chunks,
        source_unit_inventory_primary_document_ids(chunks, question, query_ir).as_ref(),
    )
}

fn collect_source_unit_fields_with_document_filter(
    chunks: &[RuntimeMatchedChunk],
    document_filter: Option<&HashSet<Uuid>>,
) -> Vec<SourceUnitField> {
    let mut fields = Vec::<SourceUnitField>::new();
    let mut seen = HashSet::<String>::new();
    for chunk in chunks.iter().filter(|chunk| is_structured_source_unit_runtime_chunk(chunk)) {
        if document_filter.is_some_and(|document_ids| !document_ids.contains(&chunk.document_id)) {
            continue;
        }
        let parsed = parse_source_unit_text(&chunk.source_text);
        let mut body = parsed.body.trim();
        if let Some((_, rest)) = body.split_once("fields:") {
            body = rest.trim();
        }
        for part in body.split(';').map(str::trim).filter(|part| !part.is_empty()) {
            let Some((path, value)) = part.split_once('=') else {
                continue;
            };
            let path = path.trim();
            let value = value.trim();
            if path.is_empty() || value.is_empty() {
                continue;
            }
            let key = format!("{}\n{}\n{}", chunk.document_label, path, value).to_lowercase();
            if !seen.insert(key) {
                continue;
            }
            fields.push(SourceUnitField {
                document_id: chunk.document_id,
                document_label: chunk.document_label.trim().to_string(),
                path: path.to_string(),
                value: value.to_string(),
            });
        }
    }
    fields
}

fn source_unit_inventory_primary_document_ids(
    chunks: &[RuntimeMatchedChunk],
    question: &str,
    query_ir: &QueryIR,
) -> Option<HashSet<Uuid>> {
    let focus_terms = structured_source_unit_inventory_focus_terms(question, query_ir);
    if focus_terms.is_empty() {
        return None;
    }
    let ordinary_best_score = chunks
        .iter()
        .filter(|chunk| {
            !is_structured_source_unit_runtime_chunk(chunk)
                && !is_source_profile_runtime_chunk(chunk)
        })
        .filter(|chunk| source_unit_ordinary_chunk_focus_score(chunk, &focus_terms) > 0)
        .map(|chunk| score_value(chunk.score))
        .filter(|score| score.is_finite())
        .max_by(|left, right| left.total_cmp(right));
    if let Some(best_score) = ordinary_best_score {
        let document_ids = chunks
            .iter()
            .filter(|chunk| {
                !is_structured_source_unit_runtime_chunk(chunk)
                    && !is_source_profile_runtime_chunk(chunk)
            })
            .filter(|chunk| source_unit_ordinary_chunk_focus_score(chunk, &focus_terms) > 0)
            .filter(|chunk| (score_value(chunk.score) - best_score).abs() <= f32::EPSILON)
            .map(|chunk| chunk.document_id)
            .collect::<HashSet<_>>();
        if !document_ids.is_empty() {
            return Some(document_ids);
        }
    }

    None
}

fn source_unit_ordinary_chunk_focus_score(
    chunk: &RuntimeMatchedChunk,
    focus_terms: &BTreeSet<String>,
) -> usize {
    let text = format!("{}\n{}\n{}", chunk.document_label, chunk.excerpt, chunk.source_text)
        .to_lowercase();
    focus_terms
        .iter()
        .filter(|term| term.chars().count() >= 3)
        .filter(|term| text.contains(term.as_str()))
        .count()
}

fn build_structured_source_unit_overlap_inventory_answer(
    question: &str,
    query_ir: &QueryIR,
    fields: &[SourceUnitField],
) -> Option<String> {
    let focus = StructuredSourceUnitFocus::new(question, query_ir, fields);
    if focus.is_empty() {
        return None;
    }
    let focused_fields;
    let fields =
        if let Some(document_ids) = source_unit_inventory_focused_document_ids(fields, &focus) {
            focused_fields = fields
                .iter()
                .filter(|field| document_ids.contains(&field.document_id))
                .cloned()
                .collect::<Vec<_>>();
            if focused_fields.is_empty() {
                return None;
            }
            focused_fields.as_slice()
        } else {
            fields
        };
    let focus = StructuredSourceUnitFocus::new(question, query_ir, fields);
    if focus.is_empty() {
        return None;
    }
    let mut selected = fields
        .iter()
        .filter_map(|field| {
            let score = structured_source_unit_field_overlap_score(field, &focus)
                .max(structured_source_unit_compact_root_inventory_score(field, &focus));
            (score > 0).then_some((score, field))
        })
        .collect::<Vec<_>>();
    expand_structured_source_unit_sibling_fields(fields, &mut selected, &focus);
    if structured_source_unit_inventory_too_broad(&selected, &focus) {
        return None;
    }
    selected.sort_by(|(left_score, left), (right_score, right)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.document_label.cmp(&right.document_label))
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.value.cmp(&right.value))
    });
    let mut seen = HashSet::<String>::new();
    let lines = selected
        .into_iter()
        .filter_map(|(_, field)| {
            let rendered = format!(
                "`{}`: `{}={}`",
                field.document_label,
                field.path.trim(),
                field.value.trim()
            );
            seen.insert(rendered.to_lowercase()).then_some(rendered)
        })
        .take(16)
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return None;
    }
    if !structured_source_unit_inventory_matches_specific_terms(&lines, &focus) {
        return None;
    }
    Some(lines.join("\n"))
}
