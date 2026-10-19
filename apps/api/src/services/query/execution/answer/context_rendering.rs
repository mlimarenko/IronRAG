use super::{
    BTreeSet, DocumentBalancedChunkSelection, HashMap, HashSet, KnowledgeStructuredBlockRow,
    KnowledgeTechnicalFactRow, QueryAct, QueryIR, QueryScope, QueryTargetKind,
    RuntimeChunkScoreKind, RuntimeMatchedChunk, chunk_is_setup_focus_command_path_anchor,
    command_dense_excerpt_for, comparison_declaration_chunks, excerpt_for,
    extract_parameter_literals, focus_token_overlap_count, focused_excerpt_for,
    focused_record_unit_excerpt, normalized_alnum_tokens, parse_table_column_summary,
    query_ir_document_focus_tokens, query_ir_has_setup_configuration_target,
    question_asks_table_aggregation, repair_technical_layout_noise, salient_source_excerpt_for,
    select_document_balanced_chunks, structured_literal_excerpt_for,
    technical_literal_focus_keywords,
};

const SOURCE_COVERAGE_MAX_TOTAL_CHUNKS: usize = 32;
const SOURCE_COVERAGE_MAX_CHUNKS_PER_DOCUMENT: usize = 24;
const PREPARED_SEGMENT_EXCERPT_CHARS: usize = 420;
const PREPARED_STRUCTURAL_BLOCK_CHARS: usize = 4_000;
const EVIDENCE_CHUNK_EXCERPT_CHARS: usize = 560;
const EVIDENCE_CODE_BLOCK_CHARS: usize = 4_000;
const STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS: usize = 4_000;

pub(crate) fn render_authoritative_technical_fact_section(
    facts: &[KnowledgeTechnicalFactRow],
) -> String {
    if facts.is_empty() {
        return String::new();
    }
    let mut lines = Vec::<String>::new();
    for fact in facts.iter().take(24) {
        let qualifiers = serde_json::from_value::<
            Vec<crate::shared::extraction::technical_facts::TechnicalFactQualifier>,
        >(fact.qualifiers_json.clone())
        .unwrap_or_default();
        let qualifier_suffix = if qualifiers.is_empty() {
            String::new()
        } else {
            format!(
                " ({})",
                qualifiers
                    .iter()
                    .map(|qualifier| format!("{}={}", qualifier.key, qualifier.value))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        lines.push(format!("- {}: `{}`{}", fact.fact_kind, fact.display_value, qualifier_suffix));
    }
    format!("Technical facts\n{}", lines.join("\n"))
}

pub(crate) fn render_prepared_segment_section(
    question: &str,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
    blocks: &[KnowledgeStructuredBlockRow],
    suppress_tabular_detail: bool,
) -> String {
    if suppress_tabular_detail {
        return String::new();
    }
    if blocks.is_empty() {
        return String::new();
    }
    let ranked_blocks = rank_prepared_segments_for_answer(question, query_ir, blocks);
    let focus_keywords =
        prepared_segment_answer_focus_tokens(question, query_ir).into_iter().collect::<Vec<_>>();
    let mut lines = Vec::<String>::new();
    for block in ranked_blocks.into_iter().take(super::MAX_ANSWER_BLOCKS) {
        let label = if block.heading_trail.is_empty() {
            block.block_kind.clone()
        } else {
            format!("{} > {}", block.block_kind, block.heading_trail.join(" > "))
        };
        let excerpt = prepared_segment_answer_text(block, &focus_keywords);
        if block.block_kind == "code_block" {
            lines.push(format!(
                "- {} (coverage={}):\n```text\n{}\n```",
                label,
                prepared_segment_answer_coverage(block, PREPARED_STRUCTURAL_BLOCK_CHARS),
                excerpt
            ));
        } else {
            lines.push(format!("- {}: {}", label, excerpt));
        }
    }
    format!("Prepared segments\n{}", lines.join("\n"))
}

fn prepared_segment_answer_text(
    block: &KnowledgeStructuredBlockRow,
    focus_keywords: &[String],
) -> String {
    let repaired = repair_technical_layout_noise(&block.normalized_text);
    let max_chars = if prepared_segment_is_structural(block) {
        PREPARED_STRUCTURAL_BLOCK_CHARS
    } else {
        PREPARED_SEGMENT_EXCERPT_CHARS
    };
    if repaired.trim().chars().count() <= max_chars {
        return repaired.trim().to_string();
    }
    let focused = focused_excerpt_for(&repaired, focus_keywords, max_chars);
    if !focused.trim().is_empty() {
        return focused;
    }
    excerpt_for(&repaired, max_chars)
}

fn prepared_segment_is_structural(block: &KnowledgeStructuredBlockRow) -> bool {
    matches!(block.block_kind.as_str(), "code_block" | "table_row" | "list_item")
}

fn prepared_segment_answer_coverage(
    block: &KnowledgeStructuredBlockRow,
    max_chars: usize,
) -> &'static str {
    if repair_technical_layout_noise(&block.normalized_text).trim().chars().count() <= max_chars {
        "full"
    } else {
        "excerpt"
    }
}

fn rank_prepared_segments_for_answer<'a>(
    question: &str,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
    blocks: &'a [KnowledgeStructuredBlockRow],
) -> Vec<&'a KnowledgeStructuredBlockRow> {
    let focus_tokens = prepared_segment_answer_focus_tokens(question, query_ir);
    if focus_tokens.is_empty() {
        return blocks.iter().collect();
    }

    let token_frequencies = prepared_segment_answer_token_frequencies(blocks);
    let candidate_count = blocks.len().max(1);
    let mut ranked = blocks
        .iter()
        .map(|block| {
            let score = prepared_segment_answer_score(
                block,
                &focus_tokens,
                &token_frequencies,
                candidate_count,
            );
            (block, score)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|(left, left_score), (right, right_score)| {
        right_score
            .cmp(left_score)
            .then_with(|| left.ordinal.cmp(&right.ordinal))
            .then_with(|| left.block_id.cmp(&right.block_id))
    });
    ranked.into_iter().map(|(block, _)| block).collect()
}

fn prepared_segment_answer_focus_tokens(
    question: &str,
    query_ir: Option<&crate::domains::query_ir::QueryIR>,
) -> BTreeSet<String> {
    let mut tokens = normalized_alnum_tokens(question, 3);
    let Some(query_ir) = query_ir else {
        return tokens;
    };
    if let Some(document_focus) = query_ir.document_focus.as_ref() {
        tokens.extend(normalized_alnum_tokens(&document_focus.hint, 3));
    }
    for entity in &query_ir.target_entities {
        tokens.extend(normalized_alnum_tokens(&entity.label, 3));
    }
    for literal in &query_ir.literal_constraints {
        tokens.extend(normalized_alnum_tokens(&literal.text, 3));
    }
    tokens
}

fn prepared_segment_answer_token_frequencies(
    blocks: &[KnowledgeStructuredBlockRow],
) -> HashMap<String, usize> {
    let mut frequencies = HashMap::<String, usize>::new();
    for block in blocks {
        for token in prepared_segment_answer_block_tokens(block) {
            *frequencies.entry(token).or_default() += 1;
        }
    }
    frequencies
}

fn prepared_segment_answer_score(
    block: &KnowledgeStructuredBlockRow,
    focus_tokens: &BTreeSet<String>,
    token_frequencies: &HashMap<String, usize>,
    candidate_count: usize,
) -> usize {
    let heading_tokens = prepared_segment_answer_heading_tokens(block);
    let body_tokens = normalized_alnum_tokens(&block.normalized_text, 3);
    let heading_score = prepared_segment_answer_overlap_score(
        focus_tokens,
        &heading_tokens,
        token_frequencies,
        candidate_count,
    ) * 8;
    let body_score = prepared_segment_answer_overlap_score(
        focus_tokens,
        &body_tokens,
        token_frequencies,
        candidate_count,
    );
    heading_score + body_score
}

fn prepared_segment_answer_overlap_score(
    focus_tokens: &BTreeSet<String>,
    block_tokens: &BTreeSet<String>,
    token_frequencies: &HashMap<String, usize>,
    candidate_count: usize,
) -> usize {
    focus_tokens
        .iter()
        .filter(|token| block_tokens.contains(*token))
        .map(|token| {
            let frequency = token_frequencies.get(token).copied().unwrap_or(candidate_count);
            candidate_count.saturating_sub(frequency).saturating_add(1)
        })
        .sum()
}

fn prepared_segment_answer_block_tokens(block: &KnowledgeStructuredBlockRow) -> BTreeSet<String> {
    let mut tokens = prepared_segment_answer_heading_tokens(block);
    tokens.extend(normalized_alnum_tokens(&block.normalized_text, 3));
    tokens
}

fn prepared_segment_answer_heading_tokens(block: &KnowledgeStructuredBlockRow) -> BTreeSet<String> {
    let mut heading_text = String::new();
    if !block.heading_trail.is_empty() {
        heading_text.push_str(&block.heading_trail.join(" "));
        heading_text.push(' ');
    }
    if !block.section_path.is_empty() {
        heading_text.push_str(&block.section_path.join(" "));
    }
    normalized_alnum_tokens(&heading_text, 3)
}

pub(crate) fn render_primary_chunk_section(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
    suppress_tabular_detail: bool,
) -> String {
    if suppress_tabular_detail && question_asks_table_aggregation(question, Some(query_ir)) {
        return String::new();
    }
    if chunks.is_empty() {
        return String::new();
    }
    let filtered_chunks = chunks
        .iter()
        .filter(|chunk| parse_table_column_summary(&chunk.source_text).is_none())
        .cloned()
        .collect::<Vec<_>>();
    if filtered_chunks.is_empty() {
        return String::new();
    }
    if query_ir.requests_source_slice_context()
        && let Some(section) = render_ordered_source_slice_unit_section(query_ir, &filtered_chunks)
    {
        return section;
    }
    let question_keywords = technical_literal_focus_keywords(question, Some(query_ir));
    let mut selected =
        select_primary_answer_chunks(question, query_ir, &filtered_chunks, &question_keywords);
    prepend_source_profile_chunks_for_coverage(query_ir, chunks, &mut selected);
    render_primary_chunk_sections(question, query_ir, &filtered_chunks, &selected)
}

fn select_primary_answer_chunks(
    question: &str,
    query_ir: &QueryIR,
    filtered_chunks: &[RuntimeMatchedChunk],
    question_keywords: &[String],
) -> Vec<RuntimeMatchedChunk> {
    let (max_total_chunks, max_chunks_per_document) = if query_ir.requests_source_coverage_context()
        || query_ir_needs_expanded_setup_evidence(question, query_ir, filtered_chunks)
    {
        (SOURCE_COVERAGE_MAX_TOTAL_CHUNKS, SOURCE_COVERAGE_MAX_CHUNKS_PER_DOCUMENT)
    } else {
        (super::MAX_CHUNKS_PER_DOCUMENT, super::MIN_CHUNKS_PER_DOCUMENT)
    };
    let balanced = select_document_balanced_chunks(DocumentBalancedChunkSelection {
        question,
        ir: Some(query_ir),
        chunks: filtered_chunks,
        keywords: question_keywords,
        pagination_requested: false,
        max_total_chunks,
        max_chunks_per_document,
    })
    .into_iter()
    .cloned()
    .collect::<Vec<_>>();
    let mut selected = comparison_declaration_chunks(
        query_ir,
        filtered_chunks,
        question_keywords,
        max_total_chunks,
    );
    let mut seen_chunk_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    selected.extend(
        balanced
            .into_iter()
            .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
            .take(max_total_chunks.saturating_sub(selected.len())),
    );
    if selected.is_empty() {
        selected = filtered_chunks.iter().take(8).cloned().collect();
    }
    selected
}

fn prepend_source_profile_chunks_for_coverage(
    query_ir: &QueryIR,
    chunks: &[RuntimeMatchedChunk],
    selected: &mut Vec<RuntimeMatchedChunk>,
) {
    if !query_ir.requests_source_coverage_context() {
        return;
    }
    let mut seen_chunk_ids = selected.iter().map(|chunk| chunk.chunk_id).collect::<HashSet<_>>();
    let mut source_profile_chunks = chunks
        .iter()
        .filter(|chunk| is_source_profile_runtime_chunk(chunk))
        .filter(|chunk| seen_chunk_ids.insert(chunk.chunk_id))
        .cloned()
        .collect::<Vec<_>>();
    if !source_profile_chunks.is_empty() {
        source_profile_chunks.append(selected);
        *selected = source_profile_chunks;
    }
}

fn render_primary_chunk_sections(
    question: &str,
    query_ir: &QueryIR,
    filtered_chunks: &[RuntimeMatchedChunk],
    selected: &[RuntimeMatchedChunk],
) -> String {
    let question_keywords = crate::services::query::planner::extract_keywords(question);
    let setup_install_anchor = focused_setup_install_anchor(question, query_ir, filtered_chunks);
    let anchor_chunk_id = setup_install_anchor.map(|chunk| chunk.chunk_id);
    let (source_profile_chunks, content_chunks): (Vec<_>, Vec<_>) = selected
        .iter()
        .filter(|chunk| Some(chunk.chunk_id) != anchor_chunk_id)
        .partition(|chunk| is_source_profile_runtime_chunk(chunk));
    let mut sections = Vec::new();
    if let Some(anchor) = setup_install_anchor {
        sections.push(format!(
            "Setup install anchor (scope=document; coverage=full; document=\"{}\")\n{}",
            context_label(&anchor.document_label),
            anchor.source_text.trim()
        ));
    }
    if !source_profile_chunks.is_empty() {
        let lines = source_profile_chunks
            .iter()
            .map(|chunk| {
                format!(
                    "- [AGGREGATE_PROFILE scope=document coverage=full document=\"{}\"] {}",
                    context_label(&chunk.document_label),
                    source_profile_excerpt(chunk)
                )
            })
            .collect::<Vec<_>>();
        sections.push(format!(
            "AGGREGATE_PROFILE blocks (scope=document; coverage=full)\n{}",
            lines.join("\n")
        ));
    }
    let lines = render_evidence_chunk_lines(&content_chunks, &question_keywords, "sampled");
    if !lines.is_empty() {
        sections.push(format!(
            "EVIDENCE_CHUNK blocks (scope=excerpt; coverage=sampled)\n{}",
            lines.join("\n")
        ));
    }
    sections.join("\n\n")
}

fn focused_setup_install_anchor<'a>(
    question: &str,
    query_ir: &QueryIR,
    chunks: &'a [RuntimeMatchedChunk],
) -> Option<&'a RuntimeMatchedChunk> {
    let focus_tokens = if matches!(query_ir.act, QueryAct::ConfigureHow)
        && matches!(query_ir.scope, QueryScope::SingleDocument)
        && query_ir.document_focus.is_some()
    {
        query_ir_document_focus_tokens(query_ir)?
    } else if query_ir_has_typed_setup_anchor_focus(query_ir) {
        let tokens = technical_literal_focus_keywords(question, Some(query_ir))
            .into_iter()
            .collect::<BTreeSet<_>>();
        if tokens.is_empty() {
            return None;
        }
        tokens
    } else {
        return None;
    };
    chunks
        .iter()
        .filter(|chunk| chunk_is_setup_focus_command_path_anchor(chunk))
        .filter(|chunk| {
            let label_tokens = normalized_alnum_tokens(&chunk.document_label, 3);
            focus_token_overlap_count(&focus_tokens, &label_tokens) > 0
        })
        .max_by(|left, right| left.score.unwrap_or(0.0).total_cmp(&right.score.unwrap_or(0.0)))
}

fn query_ir_has_typed_setup_anchor_focus(query_ir: &QueryIR) -> bool {
    matches!(query_ir.act, QueryAct::ConfigureHow | QueryAct::RetrieveValue)
        && query_ir.source_slice.is_none()
        && query_ir.document_focus.is_none()
        && query_ir_has_setup_configuration_target(query_ir)
        && (!query_ir.target_entities.is_empty() || !query_ir.literal_constraints.is_empty())
}

fn query_ir_needs_expanded_setup_evidence(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    if !chunks.iter().any(|chunk| {
        matches!(
            chunk.score_kind,
            RuntimeChunkScoreKind::DocumentIdentity
                | RuntimeChunkScoreKind::LatestVersion
                | RuntimeChunkScoreKind::SourceContext
        )
    }) {
        return false;
    }
    if matches!(query_ir.act, crate::domains::query_ir::QueryAct::ConfigureHow) {
        return true;
    }
    if query_ir_needs_expanded_short_technical_evidence(question, query_ir, chunks) {
        return true;
    }
    query_ir.targets_any(&[
        QueryTargetKind::ConfigurationFile,
        QueryTargetKind::ConfigKey,
        QueryTargetKind::Parameter,
        QueryTargetKind::Package,
    ])
}

fn query_ir_needs_expanded_short_technical_evidence(
    question: &str,
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> bool {
    query_ir_has_typed_setup_anchor_focus(query_ir)
        && technical_literal_focus_keywords(question, Some(query_ir))
            .iter()
            .any(|keyword| keyword.chars().count() < 4)
        && chunks.iter().any(|chunk| {
            chunk.score_kind == RuntimeChunkScoreKind::SourceContext
                && !extract_parameter_literals(
                    &format!("{}\n{}", chunk.excerpt, chunk.source_text),
                    2,
                )
                .is_empty()
        })
}

pub(super) fn render_ordered_source_slice_unit_section(
    query_ir: &crate::domains::query_ir::QueryIR,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let slice = query_ir.source_slice.as_ref()?;
    let mut source_profile_chunks =
        chunks.iter().filter(|chunk| is_source_profile_runtime_chunk(chunk)).collect::<Vec<_>>();
    let mut content_chunks =
        chunks.iter().filter(|chunk| !is_source_profile_runtime_chunk(chunk)).collect::<Vec<_>>();
    if content_chunks.iter().any(|chunk| super::source_context::is_source_unit_runtime_chunk(chunk))
    {
        content_chunks.retain(|chunk| super::source_context::is_source_unit_runtime_chunk(chunk));
    }
    if content_chunks.is_empty() {
        return None;
    }
    source_profile_chunks.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index));
    content_chunks.sort_by_key(|chunk| (chunk.document_label.clone(), chunk.chunk_index));
    let requested_count = super::source_slice_requested_count(query_ir).unwrap_or_default();
    let mut lines = Vec::<String>::new();
    lines.push(format!(
        "SOURCE_SLICE blocks (scope=ordered_source; coverage=ordered; direction={}; requested_count={}; returned_unit_count={})",
        source_slice_direction_label(slice.direction),
        requested_count,
        content_chunks.len()
    ));
    for chunk in source_profile_chunks {
        lines.push(format!(
            "- [SOURCE_PROFILE document=\"{}\"] {}",
            context_label(&chunk.document_label),
            source_profile_excerpt(chunk)
        ));
    }
    for chunk in content_chunks {
        let text = chunk_text_for_source_slice(chunk);
        lines.push(format!(
            "- [SOURCE_SLICE_UNIT direction={} requested_count={} document=\"{}\" ordinal={} coverage=ordered] {}",
            source_slice_direction_label(slice.direction),
            requested_count,
            context_label(&chunk.document_label),
            chunk.chunk_index,
            text
        ));
    }
    Some(lines.join("\n"))
}

fn chunk_text_for_source_slice(chunk: &RuntimeMatchedChunk) -> String {
    let source = chunk.source_text.trim();
    if !source.is_empty() {
        return source.to_string();
    }
    chunk.excerpt.trim().to_string()
}

pub(crate) fn render_targeted_evidence_chunk_section(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> String {
    if chunks.is_empty() {
        return String::new();
    }
    let question_keywords = crate::services::query::planner::extract_keywords(question);
    let chunk_refs = chunks.iter().collect::<Vec<_>>();
    let lines = render_evidence_chunk_lines(&chunk_refs, &question_keywords, "targeted");
    if lines.is_empty() {
        String::new()
    } else {
        format!("EVIDENCE_CHUNK blocks (scope=excerpt; coverage=targeted)\n{}", lines.join("\n"))
    }
}

pub(super) fn render_evidence_chunk_lines(
    chunks: &[&RuntimeMatchedChunk],
    question_keywords: &[String],
    coverage: &str,
) -> Vec<String> {
    chunks
        .iter()
        .map(|chunk| {
            let (scope, excerpt) = evidence_chunk_scope_and_excerpt(chunk, question_keywords);
            format!(
                "- [EVIDENCE_CHUNK scope={} coverage={} document=\"{}\" chunk_index={}] {}",
                scope,
                coverage,
                context_label(&chunk.document_label),
                chunk.chunk_index,
                excerpt
            )
        })
        .collect()
}

fn evidence_chunk_scope_and_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> (&'static str, String) {
    if let Some(evidence) = graph_evidence_scope_and_excerpt(chunk, question_keywords) {
        return evidence;
    }
    if let Some(evidence) = source_unit_scope_and_excerpt(chunk, question_keywords) {
        return evidence;
    }
    if let Some(evidence) = code_block_scope_and_excerpt(chunk, question_keywords) {
        return evidence;
    }
    if let Some(excerpt) = preferred_structured_excerpt(chunk, question_keywords) {
        return excerpt;
    }
    ("excerpt", focused_chunk_excerpt(chunk, question_keywords))
}

fn graph_evidence_scope_and_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> Option<(&'static str, String)> {
    if chunk.score_kind != RuntimeChunkScoreKind::GraphEvidence {
        return None;
    }
    let source_text = chunk.source_text.trim();
    if source_text.is_empty() {
        return None;
    }
    let excerpt = bounded_focused_excerpt(
        source_text,
        question_keywords,
        STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS,
    );
    Some(("graph_evidence", excerpt))
}

fn source_unit_scope_and_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> Option<(&'static str, String)> {
    if !is_structured_source_unit_runtime_chunk(chunk) {
        return None;
    }
    let source_text = chunk.source_text.trim();
    if source_text.chars().count() <= STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS {
        return Some(("source_unit", source_text.to_string()));
    }
    let excerpt = focused_record_unit_excerpt(
        source_text,
        question_keywords,
        STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS,
    )
    .unwrap_or_else(|| {
        focused_excerpt_for(source_text, question_keywords, STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS)
    });
    Some((
        "source_unit",
        nonempty_excerpt_or_fallback(excerpt, source_text, STRUCTURED_SOURCE_UNIT_EVIDENCE_CHARS),
    ))
}

fn code_block_scope_and_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> Option<(&'static str, String)> {
    if chunk.chunk_kind.as_deref() != Some("code_block") {
        return None;
    }
    let source_text = repair_technical_layout_noise(&chunk.source_text);
    let excerpt = if source_text.chars().count() <= EVIDENCE_CODE_BLOCK_CHARS {
        source_text
    } else {
        structured_literal_excerpt_for(&source_text, question_keywords, EVIDENCE_CODE_BLOCK_CHARS)
            .unwrap_or_else(|| excerpt_for(&source_text, EVIDENCE_CODE_BLOCK_CHARS))
    };
    Some(("code_block", excerpt))
}

fn preferred_structured_excerpt(
    chunk: &RuntimeMatchedChunk,
    question_keywords: &[String],
) -> Option<(&'static str, String)> {
    if let Some(excerpt) =
        salient_source_excerpt_for(&chunk.source_text, question_keywords, EVIDENCE_CODE_BLOCK_CHARS)
    {
        return Some(("salient_excerpt", excerpt));
    }
    if let Some(excerpt) = structured_literal_excerpt_for(
        &chunk.source_text,
        question_keywords,
        EVIDENCE_CODE_BLOCK_CHARS,
    ) {
        return Some(("structured_excerpt", excerpt));
    }
    command_dense_excerpt_for(&chunk.source_text, EVIDENCE_CODE_BLOCK_CHARS)
        .map(|excerpt| ("code_block", excerpt))
}

fn focused_chunk_excerpt(chunk: &RuntimeMatchedChunk, question_keywords: &[String]) -> String {
    let excerpt =
        focused_excerpt_for(&chunk.source_text, question_keywords, EVIDENCE_CHUNK_EXCERPT_CHARS);
    nonempty_excerpt_or_fallback(excerpt, &chunk.source_text, EVIDENCE_CHUNK_EXCERPT_CHARS)
}

fn bounded_focused_excerpt(
    source_text: &str,
    question_keywords: &[String],
    max_chars: usize,
) -> String {
    if source_text.chars().count() <= max_chars {
        return source_text.to_string();
    }
    let excerpt = focused_excerpt_for(source_text, question_keywords, max_chars);
    nonempty_excerpt_or_fallback(excerpt, source_text, max_chars)
}

fn nonempty_excerpt_or_fallback(excerpt: String, source_text: &str, max_chars: usize) -> String {
    if excerpt.trim().is_empty() { excerpt_for(source_text, max_chars) } else { excerpt }
}

pub(super) fn is_structured_source_unit_runtime_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    chunk.chunk_kind.as_deref() == Some(super::SOURCE_UNIT_CHUNK_KIND)
}

pub(super) fn is_source_profile_runtime_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    super::source_profile::is_source_profile_runtime_chunk(chunk)
}

fn source_profile_excerpt(chunk: &RuntimeMatchedChunk) -> String {
    chunk
        .source_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_else(|| chunk.source_text.trim())
        .to_string()
}

fn source_slice_direction_label(
    direction: crate::domains::query_ir::SourceSliceDirection,
) -> &'static str {
    match direction {
        crate::domains::query_ir::SourceSliceDirection::Head => "head",
        crate::domains::query_ir::SourceSliceDirection::Tail => "tail",
        crate::domains::query_ir::SourceSliceDirection::All => "all",
    }
}

fn context_label(label: &str) -> String {
    label.replace('\\', "\\\\").replace('"', "\\\"")
}

#[derive(Debug, Default)]
pub(super) struct ParsedSourceUnitText {
    pub(super) header: Option<String>,
    fields: HashMap<String, String>,
    pub(super) body: String,
}

impl ParsedSourceUnitText {
    pub(super) fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str).filter(|value| !value.trim().is_empty())
    }
}

pub(super) fn parse_source_unit_text(text: &str) -> ParsedSourceUnitText {
    let trimmed = text.trim();
    let Some(rest) = trimmed.strip_prefix('[') else {
        return ParsedSourceUnitText {
            header: None,
            fields: HashMap::new(),
            body: trimmed.to_string(),
        };
    };
    let Some((header, body)) = rest.split_once(']') else {
        return ParsedSourceUnitText {
            header: None,
            fields: HashMap::new(),
            body: trimmed.to_string(),
        };
    };
    let fields = header
        .split_whitespace()
        .filter_map(|token| {
            let (key, value) = token.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect::<HashMap<_, _>>();
    ParsedSourceUnitText {
        header: Some(header.trim().to_string()),
        fields,
        body: body.trim().to_string(),
    }
}

pub(super) fn indent_source_unit_body(body: &str) -> String {
    body.lines().map(|line| format!("   {}", line)).collect::<Vec<_>>().join("\n")
}
