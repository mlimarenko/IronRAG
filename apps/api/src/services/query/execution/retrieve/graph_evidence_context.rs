use super::{
    BTreeSet, GRAPH_EVIDENCE_CONTEXT_EVIDENCE_FIELD_WEIGHT,
    GRAPH_EVIDENCE_CONTEXT_FETCH_MULTIPLIER, GRAPH_EVIDENCE_CONTEXT_LINE_CAP,
    GRAPH_EVIDENCE_CONTEXT_LINE_CHARS, GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS,
    GRAPH_EVIDENCE_CONTEXT_SOURCE_FIELD_WEIGHT, GRAPH_EVIDENCE_CONTEXT_TARGET_FIELD_WEIGHT,
    GRAPH_EVIDENCE_TARGET_CAP, GraphTargetEntityCoverageField, GraphTargetEntityCoverageFieldKind,
    GraphTargetEntityProfile, HashMap, HashSet, QueryGraphIndex, QueryIR, RuntimeMatchedChunk,
    RuntimeMatchedEntity, RuntimeMatchedRelationship, RuntimeQueryPlan, Uuid,
    associative_edges_for_entities, excerpt_for, focused_excerpt_for,
    graph_target_entity_coverage_score, normalized_alnum_token_sequence, normalized_alnum_tokens,
    query_relevant_graph_evidence_target_hits, repair_technical_layout_noise, repositories,
    token_sequence_contains,
};

#[must_use]
pub(super) fn graph_evidence_context_fetch_cap() -> usize {
    GRAPH_EVIDENCE_CONTEXT_LINE_CAP * GRAPH_EVIDENCE_CONTEXT_FETCH_MULTIPLIER
}

#[derive(Debug, Clone)]
pub(super) struct GraphEvidenceContextCandidate {
    pub(super) row: repositories::RuntimeGraphEvidenceRow,
    pub(super) ordinal: usize,
    pub(super) fields: Vec<GraphEvidenceContextCandidateField>,
    pub(super) tokens: BTreeSet<String>,
    pub(super) score: usize,
}

#[derive(Debug, Clone)]
pub(super) struct GraphEvidenceContextCandidateField {
    pub(super) text: String,
    pub(super) tokens: BTreeSet<String>,
    pub(super) weight: usize,
    pub(super) coverage_kind: GraphTargetEntityCoverageFieldKind,
}

pub(super) fn graph_evidence_context_candidates(
    text_evidence: &[repositories::RuntimeGraphEvidenceRow],
    target_evidence: &[repositories::RuntimeGraphEvidenceRow],
    graph_index: &QueryGraphIndex,
) -> Vec<GraphEvidenceContextCandidate> {
    let mut candidates = Vec::new();
    let mut seen_row_ids = BTreeSet::new();
    for (source_ordinal, rows) in [text_evidence, target_evidence].into_iter().enumerate() {
        for (row_ordinal, row) in rows.iter().enumerate() {
            if !seen_row_ids.insert(row.id) {
                continue;
            }
            let fields = graph_evidence_context_candidate_fields(row, graph_index);
            let tokens = fields
                .iter()
                .flat_map(|field| field.tokens.iter().cloned())
                .collect::<BTreeSet<_>>();
            candidates.push(GraphEvidenceContextCandidate {
                row: row.clone(),
                ordinal: source_ordinal.saturating_mul(graph_evidence_context_fetch_cap())
                    + row_ordinal,
                fields,
                tokens,
                score: 0,
            });
        }
    }
    candidates
}

pub(super) fn score_graph_evidence_context_candidates(
    candidates: &mut [GraphEvidenceContextCandidate],
    focus_tokens: &[(String, BTreeSet<String>)],
    target_entity_profiles: &[GraphTargetEntityProfile],
) {
    let mut token_frequencies = HashMap::<String, usize>::new();
    for candidate in candidates.iter() {
        for token in &candidate.tokens {
            *token_frequencies.entry(token.clone()).or_default() += 1;
        }
    }
    let candidate_count = candidates.len();
    for candidate in candidates {
        candidate.score = graph_evidence_context_relevance_score(
            &candidate.fields,
            focus_tokens,
            &token_frequencies,
            candidate_count,
            target_entity_profiles,
        );
    }
}

/// One graph-evidence context ranking pass: the evidence row sets to rank, the
/// query signal they are ranked against, the graph and target-profile indexes
/// and how many context rows to keep.
pub(crate) struct GraphEvidenceContextRequest<'a> {
    pub(crate) text_evidence: &'a [repositories::RuntimeGraphEvidenceRow],
    pub(crate) target_evidence: &'a [repositories::RuntimeGraphEvidenceRow],
    pub(crate) question: &'a str,
    pub(crate) text_queries: &'a [String],
    pub(crate) graph_index: &'a QueryGraphIndex,
    pub(crate) target_entity_profiles: &'a [GraphTargetEntityProfile],
    pub(crate) limit: usize,
}

pub(crate) fn rank_graph_evidence_context_rows(
    request: GraphEvidenceContextRequest<'_>,
) -> Vec<repositories::RuntimeGraphEvidenceRow> {
    let GraphEvidenceContextRequest {
        text_evidence,
        target_evidence,
        question,
        text_queries,
        graph_index,
        target_entity_profiles,
        limit,
    } = request;
    if limit == 0 {
        return Vec::new();
    }

    let focus_texts = graph_evidence_context_focus_texts(question, text_queries);
    let focus_tokens = graph_evidence_context_focus_tokens(&focus_texts);
    let mut candidates =
        graph_evidence_context_candidates(text_evidence, target_evidence, graph_index);

    if candidates.is_empty() {
        return Vec::new();
    }

    score_graph_evidence_context_candidates(&mut candidates, &focus_tokens, target_entity_profiles);

    candidates.sort_by(|left, right| {
        let left_confidence = left.row.confidence_score.unwrap_or(f64::NEG_INFINITY);
        let right_confidence = right.row.confidence_score.unwrap_or(f64::NEG_INFINITY);
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
            .then_with(|| {
                right_confidence.partial_cmp(&left_confidence).unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| right.row.created_at.cmp(&left.row.created_at))
            .then_with(|| right.row.id.cmp(&left.row.id))
    });

    let mut selected = Vec::new();
    let mut seen_bodies = BTreeSet::new();
    for candidate in candidates {
        if selected.len() >= limit {
            break;
        }
        let body_key = graph_evidence_context_body_key(&candidate.row.evidence_text);
        if body_key.is_empty() || !seen_bodies.insert(body_key) {
            continue;
        }
        selected.push(candidate.row);
    }
    selected
}

pub(super) fn graph_evidence_context_focus_texts(
    question: &str,
    text_queries: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut focus_texts = Vec::new();
    let mut push_focus = |value: &str, focus_texts: &mut Vec<String>| {
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            return;
        }
        focus_texts.push(normalized);
    };

    for text_query in text_queries {
        push_focus(text_query, &mut focus_texts);
    }
    push_focus(question, &mut focus_texts);

    focus_texts
}

pub(super) fn graph_evidence_context_focus_tokens(
    focus_texts: &[String],
) -> Vec<(String, BTreeSet<String>)> {
    focus_texts
        .iter()
        .filter_map(|focus_text| {
            let tokens = normalized_alnum_token_sequence(
                focus_text,
                GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS,
            )
            .into_iter()
            .collect::<BTreeSet<_>>();
            (!tokens.is_empty()).then(|| (focus_text.clone(), tokens))
        })
        .collect()
}

pub(super) fn push_graph_evidence_source_field(
    fields: &mut Vec<GraphEvidenceContextCandidateField>,
    row: &repositories::RuntimeGraphEvidenceRow,
) {
    let source_parts = [row.source_file_name.as_deref(), row.page_ref.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if !source_parts.is_empty() {
        push_graph_evidence_context_candidate_field(
            fields,
            source_parts.join(" "),
            GRAPH_EVIDENCE_CONTEXT_SOURCE_FIELD_WEIGHT,
            GraphTargetEntityCoverageFieldKind::Summary,
        );
    }
}

pub(super) fn graph_evidence_context_candidate_fields(
    row: &repositories::RuntimeGraphEvidenceRow,
    graph_index: &QueryGraphIndex,
) -> Vec<GraphEvidenceContextCandidateField> {
    let mut fields = Vec::new();
    push_graph_evidence_context_candidate_field(
        &mut fields,
        row.evidence_text.clone(),
        GRAPH_EVIDENCE_CONTEXT_EVIDENCE_FIELD_WEIGHT,
        GraphTargetEntityCoverageFieldKind::Evidence,
    );
    if let Some(target_label) =
        graph_evidence_target_label(&row.target_kind, row.target_id, graph_index)
    {
        push_graph_evidence_context_candidate_field(
            &mut fields,
            target_label,
            GRAPH_EVIDENCE_CONTEXT_TARGET_FIELD_WEIGHT,
            GraphTargetEntityCoverageFieldKind::Label,
        );
    }
    push_graph_evidence_source_field(&mut fields, row);
    fields
}

pub(super) fn push_graph_evidence_context_candidate_field(
    fields: &mut Vec<GraphEvidenceContextCandidateField>,
    text: String,
    weight: usize,
    coverage_kind: GraphTargetEntityCoverageFieldKind,
) {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return;
    }
    let tokens = normalized_alnum_tokens(&normalized, GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS);
    fields.push(GraphEvidenceContextCandidateField {
        text: normalized,
        tokens,
        weight,
        coverage_kind,
    });
}

pub(super) fn graph_evidence_focus_relevance_score(
    candidate_fields: &[GraphEvidenceContextCandidateField],
    focus_text: &str,
    tokens: &BTreeSet<String>,
    token_frequencies: &HashMap<String, usize>,
    candidate_count: usize,
    weight: usize,
) -> usize {
    let overlapping = tokens
        .iter()
        .filter_map(|token| {
            let field_weight = candidate_fields
                .iter()
                .filter(|field| field.tokens.contains(token))
                .map(|field| field.weight)
                .max()
                .unwrap_or_default();
            (field_weight > 0).then(|| {
                let frequency = token_frequencies.get(token).copied().unwrap_or(candidate_count);
                candidate_count
                    .saturating_sub(frequency)
                    .saturating_add(1)
                    .saturating_mul(field_weight)
            })
        })
        .collect::<Vec<_>>();
    if overlapping.is_empty() {
        return 0;
    }
    let overlap_score = overlapping.iter().sum::<usize>();
    let full_overlap_bonus = if overlapping.len() == tokens.len() {
        16usize.saturating_mul(weight).saturating_add(overlap_score)
    } else {
        0
    };
    let sequence_score = candidate_fields
        .iter()
        .filter(|field| {
            token_sequence_contains(
                &field.text,
                focus_text,
                GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS,
            )
        })
        .map(|field| 32usize.saturating_mul(weight).saturating_mul(field.weight))
        .sum::<usize>();
    overlap_score
        .saturating_mul(weight)
        .saturating_add(full_overlap_bonus)
        .saturating_add(sequence_score)
}

pub(super) fn graph_evidence_context_relevance_score(
    candidate_fields: &[GraphEvidenceContextCandidateField],
    focus_tokens: &[(String, BTreeSet<String>)],
    token_frequencies: &HashMap<String, usize>,
    candidate_count: usize,
    target_entity_profiles: &[GraphTargetEntityProfile],
) -> usize {
    if candidate_fields.is_empty() || focus_tokens.is_empty() {
        return 0;
    }

    let score = focus_tokens
        .iter()
        .enumerate()
        .map(|(ordinal, (focus_text, tokens))| {
            graph_evidence_focus_relevance_score(
                candidate_fields,
                focus_text,
                tokens,
                token_frequencies,
                candidate_count,
                focus_tokens.len().saturating_sub(ordinal).max(1),
            )
        })
        .sum::<usize>();
    let coverage_fields = candidate_fields
        .iter()
        .map(|field| GraphTargetEntityCoverageField {
            text: field.text.as_str(),
            kind: field.coverage_kind,
        })
        .collect::<Vec<_>>();
    score.saturating_add(graph_target_entity_coverage_score(
        &coverage_fields,
        target_entity_profiles,
    ))
}

pub(super) fn graph_evidence_context_body_key(evidence_text: &str) -> String {
    graph_evidence_text_for_context(evidence_text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn graph_evidence_context_line(
    row: &repositories::RuntimeGraphEvidenceRow,
    graph_index: &QueryGraphIndex,
    source_labels_by_document_id: &HashMap<Uuid, String>,
    focus_keywords: &[String],
) -> Option<String> {
    let evidence_text = graph_evidence_text_for_context(&row.evidence_text);
    if evidence_text.is_empty() {
        return None;
    }
    let evidence_text = focused_graph_evidence_context_text(&evidence_text, focus_keywords);
    if evidence_text.is_empty() {
        return None;
    }

    let mut attrs = Vec::new();
    if let Some(target_label) =
        graph_evidence_target_label(&row.target_kind, row.target_id, graph_index)
    {
        attrs.push(("target", target_label));
    }
    if let Some(source) = graph_evidence_context_source_label(row, source_labels_by_document_id) {
        attrs.push(("source", source));
    }
    if let Some(page) = row.page_ref.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        attrs.push(("page", page.to_string()));
    }

    let attr_text = attrs
        .into_iter()
        .map(|(key, value)| format!("{key}=\"{}\"", context_attribute_value(&value)))
        .collect::<Vec<_>>()
        .join(" ");
    let header = if attr_text.is_empty() {
        "[graph-evidence]".to_string()
    } else {
        format!("[graph-evidence {attr_text}]")
    };
    Some(format!("{header}\n{evidence_text}"))
}

pub(super) fn graph_evidence_context_source_label(
    row: &repositories::RuntimeGraphEvidenceRow,
    source_labels_by_document_id: &HashMap<Uuid, String>,
) -> Option<String> {
    if let Some(document_id) = row.document_id
        && let Some(source_label) = source_labels_by_document_id.get(&document_id)
        && let Some(source_label) = trimmed_non_empty(Some(source_label.as_str()))
    {
        return Some(source_label);
    }

    trimmed_non_empty(row.source_file_name.as_deref())
}

pub(super) fn trimmed_non_empty(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string)
}

pub(super) fn graph_evidence_context_line_focus_keywords(
    question: &str,
    text_queries: &[String],
) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut keywords = Vec::new();
    for focus_text in graph_evidence_context_focus_texts(question, text_queries) {
        for token in normalized_alnum_token_sequence(
            &focus_text,
            GRAPH_EVIDENCE_CONTEXT_SCORE_TOKEN_MIN_CHARS,
        ) {
            if seen.insert(token.clone()) {
                keywords.push(token);
            }
        }
    }
    keywords
}

pub(super) fn focused_graph_evidence_context_text(
    evidence_text: &str,
    focus_keywords: &[String],
) -> String {
    if evidence_text.chars().count() <= GRAPH_EVIDENCE_CONTEXT_LINE_CHARS {
        return evidence_text.to_string();
    }
    let excerpt =
        focused_excerpt_for(evidence_text, focus_keywords, GRAPH_EVIDENCE_CONTEXT_LINE_CHARS);
    if excerpt.trim().is_empty() {
        excerpt_for(evidence_text, GRAPH_EVIDENCE_CONTEXT_LINE_CHARS)
    } else {
        excerpt
    }
}

pub(super) fn graph_evidence_target_label(
    target_kind: &str,
    target_id: Uuid,
    graph_index: &QueryGraphIndex,
) -> Option<String> {
    match target_kind {
        "node" => {
            graph_index.node(target_id).map(|node| format!("{} ({})", node.label, node.node_type))
        }
        "edge" => graph_index.edge(target_id).map(|edge| {
            let from_label = graph_index
                .node(edge.from_node_id)
                .map(|node| node.label.as_str())
                .unwrap_or("<unknown>");
            let to_label = graph_index
                .node(edge.to_node_id)
                .map(|node| node.label.as_str())
                .unwrap_or("<unknown>");
            format!("{from_label} --{}--> {to_label}", edge.relation_type)
        }),
        _ => None,
    }
}

pub(super) fn context_attribute_value(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(crate) fn apply_graph_evidence_texts_to_chunks(
    chunks: &mut [RuntimeMatchedChunk],
    evidence_texts_by_chunk: &HashMap<Uuid, Vec<String>>,
    plan_keywords: &[String],
    focus_keywords: &[String],
) {
    for chunk in chunks {
        let Some(evidence_texts) = evidence_texts_by_chunk.get(&chunk.chunk_id) else {
            continue;
        };
        let evidence_source_text =
            graph_evidence_source_text(&chunk.source_text, evidence_texts, focus_keywords);
        if evidence_source_text.trim().is_empty() {
            continue;
        }
        chunk.source_text = evidence_source_text;
        chunk.excerpt = focused_excerpt_for(&chunk.source_text, plan_keywords, 280);
    }
}

pub(super) fn graph_evidence_source_text(
    chunk_source_text: &str,
    evidence_texts: &[String],
    focus_keywords: &[String],
) -> String {
    let mut parts = Vec::new();
    let mut seen = BTreeSet::new();
    for text in evidence_texts {
        let normalized = graph_evidence_text_for_context(text);
        if normalized.is_empty() || !seen.insert(normalized.to_lowercase()) {
            continue;
        }
        parts.push(focused_graph_evidence_context_text(&normalized, focus_keywords));
    }
    if parts.is_empty() {
        return chunk_source_text.trim().to_string();
    }

    let evidence_text = parts.join("\n\n");
    let chunk_text = chunk_source_text.trim();
    if chunk_text.is_empty() || chunk_text.contains(&evidence_text) {
        evidence_text
    } else {
        format!("{evidence_text}\n\nSource chunk:\n{chunk_text}")
    }
}

pub(super) fn graph_evidence_text_for_context(value: &str) -> String {
    let normalized = repair_technical_layout_noise(value.trim());
    if normalized.is_empty() {
        return String::new();
    }
    let fields = normalized
        .split(" | ")
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();
    if fields.len() < 2 {
        return normalized;
    }
    fields.into_iter().map(|field| format!("- {field}")).collect::<Vec<_>>().join("\n")
}

pub(crate) fn graph_evidence_targets(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
) -> Vec<(String, Uuid)> {
    let mut seen = HashSet::<(String, Uuid)>::new();
    let mut targets = Vec::with_capacity(entities.len() + relationships.len());
    for entity in entities {
        let key = ("node".to_string(), entity.node_id);
        if seen.insert(key.clone()) {
            targets.push(key);
        }
    }
    for relationship in relationships {
        let key = ("edge".to_string(), relationship.edge_id);
        if seen.insert(key.clone()) {
            targets.push(key);
        }
    }
    targets
}

pub(crate) fn graph_evidence_targets_for_query(
    entities: &[RuntimeMatchedEntity],
    relationships: &[RuntimeMatchedRelationship],
    plan: &RuntimeQueryPlan,
    query_ir: Option<&QueryIR>,
    target_entity_profiles: &[GraphTargetEntityProfile],
    graph_index: &QueryGraphIndex,
) -> Vec<(String, Uuid)> {
    let mut seen = HashSet::<(String, Uuid)>::new();
    let mut targets = Vec::new();
    let retrieved_targets = graph_evidence_targets(entities, relationships);

    let query_entities = query_relevant_graph_evidence_target_hits(
        plan,
        query_ir,
        target_entity_profiles,
        graph_index,
        GRAPH_EVIDENCE_TARGET_CAP / 2,
    );
    let query_relationships = associative_edges_for_entities(
        &query_entities,
        graph_index,
        plan,
        query_ir,
        GRAPH_EVIDENCE_TARGET_CAP / 2,
    );
    let query_targets = graph_evidence_targets(&query_entities, &query_relationships);

    if !target_entity_profiles.is_empty() {
        append_graph_evidence_targets(&mut targets, &mut seen, query_targets);
        append_graph_evidence_targets(&mut targets, &mut seen, retrieved_targets);
    } else {
        append_graph_evidence_targets(&mut targets, &mut seen, retrieved_targets);
        append_graph_evidence_targets(&mut targets, &mut seen, query_targets);
    }
    targets
}

pub(super) fn append_graph_evidence_targets(
    targets: &mut Vec<(String, Uuid)>,
    seen: &mut HashSet<(String, Uuid)>,
    candidates: Vec<(String, Uuid)>,
) {
    for target in candidates {
        if targets.len() >= GRAPH_EVIDENCE_TARGET_CAP {
            return;
        }
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    }
}
