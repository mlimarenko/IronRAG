use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

use anyhow::Context;
use uuid::Uuid;

use crate::{
    app::state::AppState,
    domains::query_ir::{QueryAct, QueryIR, QueryScope, SourceSliceDirection},
    infra::arangodb::document_store::{KnowledgeChunkRow, KnowledgeDocumentRow},
    shared::extraction::table_summary::is_table_summary_text,
};

use super::{
    RuntimeChunkScoreKind, RuntimeMatchedChunk, canonical_document_revision_id, map_chunk_hit,
    merge_chunks, question_asks_table_aggregation, question_intent::canonical_target_type_tag,
};

pub(crate) fn requested_initial_table_row_count(ir: Option<&QueryIR>) -> Option<usize> {
    let ir = ir?;
    let slice = ir.source_slice.as_ref()?;
    if slice.direction != SourceSliceDirection::Head {
        return None;
    }
    let targets_table_rows = ir.target_types.iter().any(|tag| tag == "table_row");
    targets_table_rows.then(|| usize::from(slice.count.unwrap_or(12)).clamp(1, 32))
}

pub(crate) async fn load_initial_table_rows_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    row_count: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    load_table_rows_for_documents(state, document_index, targeted_document_ids, row_count, keywords)
        .await
}

pub(crate) async fn load_table_rows_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    limit_per_document: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() || limit_per_document == 0 {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for document_id in targeted_document_ids {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let rows = state
            .arango_document_store
            .list_chunks_by_revision(revision_id)
            .await
            .with_context(|| format!("failed to load table rows for document {document_id}"))?;
        let synthetic_base_score = 0.5_f32;
        let normalized_keywords =
            keywords.iter().map(|keyword| keyword.to_lowercase()).collect::<Vec<_>>();
        let mut ranked_rows = rows
            .into_iter()
            .filter(|chunk| chunk.chunk_kind.as_deref() == Some("table_row"))
            .enumerate()
            .map(|(ordinal, chunk)| {
                let match_count =
                    table_row_keyword_match_count(&chunk, &normalized_keywords) as f32;
                let score = synthetic_base_score + match_count * TABLE_ROW_KEYWORD_SCORE_BOOST
                    - ordinal as f32 * 0.0001;
                (score, match_count, ordinal, chunk)
            })
            .collect::<Vec<_>>();
        ranked_rows.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(Ordering::Equal)
                .then_with(|| right.1.partial_cmp(&left.1).unwrap_or(Ordering::Equal))
                .then_with(|| left.2.cmp(&right.2))
        });
        chunks.extend(
            ranked_rows.into_iter().take(limit_per_document).filter_map(|(score, _, _, row)| {
                map_chunk_hit(row, score, document_index, keywords)
            }),
        );
    }

    Ok(chunks)
}

const TABLE_ROW_KEYWORD_SCORE_BOOST: f32 = 0.08;
const TABLE_SECTION_SIBLING_FORWARD: i32 = 64;
const TABLE_SECTION_SIBLING_BACKWARD: i32 = 3;
const TABLE_SECTION_SIBLING_SCORE_BOOST: f32 = 2.0;

fn table_row_keyword_match_count(
    chunk: &KnowledgeChunkRow,
    normalized_keywords: &[String],
) -> usize {
    if normalized_keywords.is_empty() {
        return 0;
    }
    let haystack = format!("{} {}", chunk.normalized_text, chunk.content_text).to_lowercase();
    normalized_keywords
        .iter()
        .map(|keyword| {
            let keyword = keyword.trim().to_lowercase();
            if keyword.is_empty() { 0 } else { haystack.match_indices(&keyword).count() }
        })
        .sum()
}

pub(crate) fn query_ir_requests_table_section_siblings(ir: &QueryIR) -> bool {
    if ir.source_slice.is_some()
        || !matches!(ir.scope, QueryScope::SingleDocument)
        || !matches!(ir.act, QueryAct::Describe | QueryAct::Enumerate | QueryAct::RetrieveValue)
        || question_asks_table_aggregation("", Some(ir))
    {
        return false;
    }
    let target_types = ir
        .target_types
        .iter()
        .map(|target_type| canonical_target_type_tag(target_type))
        .collect::<BTreeSet<_>>();
    target_types.contains("table_row") && target_types.contains("table_summary")
}

pub(crate) async fn load_table_section_sibling_chunks(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    anchor_chunks: &[RuntimeMatchedChunk],
    limit_per_section: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if anchor_chunks.is_empty() || limit_per_section == 0 {
        return Ok(Vec::new());
    }

    let anchor_chunk_ids = anchor_chunks.iter().map(|chunk| chunk.chunk_id).collect::<Vec<_>>();
    let anchor_scores = anchor_chunks
        .iter()
        .enumerate()
        .map(|(rank, chunk)| (chunk.chunk_id, (chunk.score.unwrap_or_default(), rank)))
        .collect::<HashMap<_, _>>();
    let anchor_rows = state
        .arango_document_store
        .list_chunks_by_ids(&anchor_chunk_ids)
        .await
        .context("failed to hydrate table section anchor chunks")?;
    let windows = table_section_sibling_windows(&anchor_rows);
    if windows.is_empty() {
        return Ok(Vec::new());
    }

    let sibling_rows = state
        .arango_document_store
        .list_chunks_by_revisions_windows(&windows)
        .await
        .context("failed to load table section sibling chunks")?;
    let selected_rows =
        select_table_section_sibling_rows(&sibling_rows, &anchor_rows, limit_per_section);
    let section_scores = table_section_anchor_scores(&anchor_rows, &anchor_scores);
    let mut chunks = Vec::new();
    for (rank, row) in selected_rows.into_iter().enumerate() {
        let section_key = table_section_key(&row);
        let base_score =
            section_scores.get(&section_key).map(|(score, _)| *score).unwrap_or_default();
        let score = base_score + TABLE_SECTION_SIBLING_SCORE_BOOST - rank as f32 * 0.01;
        if let Some(mut chunk) = map_chunk_hit(row, score, document_index, keywords) {
            chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
            chunks.push(chunk);
        }
    }
    Ok(chunks)
}

fn table_section_sibling_windows(rows: &[KnowledgeChunkRow]) -> Vec<(Uuid, i32, i32)> {
    rows.iter()
        .filter(|row| is_table_section_anchor_row(row))
        .filter(|row| !row.section_path.is_empty())
        .map(|row| {
            (
                row.revision_id,
                row.chunk_index.saturating_sub(TABLE_SECTION_SIBLING_BACKWARD).max(0),
                row.chunk_index.saturating_add(TABLE_SECTION_SIBLING_FORWARD),
            )
        })
        .collect()
}

fn table_section_anchor_scores(
    rows: &[KnowledgeChunkRow],
    anchor_scores: &HashMap<Uuid, (f32, usize)>,
) -> HashMap<(Uuid, Vec<String>), (f32, usize)> {
    let mut scores = HashMap::<(Uuid, Vec<String>), (f32, usize)>::new();
    for row in rows
        .iter()
        .filter(|row| is_table_section_anchor_row(row))
        .filter(|row| !row.section_path.is_empty())
    {
        let Some((score, rank)) = anchor_scores.get(&row.chunk_id).copied() else {
            continue;
        };
        let key = table_section_key(row);
        scores
            .entry(key)
            .and_modify(|existing| {
                if score > existing.0 || (score == existing.0 && rank < existing.1) {
                    *existing = (score, rank);
                }
            })
            .or_insert((score, rank));
    }
    scores
}

fn select_table_section_sibling_rows(
    rows: &[KnowledgeChunkRow],
    anchors: &[KnowledgeChunkRow],
    limit_per_section: usize,
) -> Vec<KnowledgeChunkRow> {
    if rows.is_empty() || anchors.is_empty() || limit_per_section == 0 {
        return Vec::new();
    }
    let anchor_sections = anchors
        .iter()
        .filter(|row| is_table_section_anchor_row(row))
        .filter(|row| !row.section_path.is_empty())
        .map(table_section_key)
        .collect::<BTreeSet<_>>();
    if anchor_sections.is_empty() {
        return Vec::new();
    }

    let mut selected_by_section = HashMap::<(Uuid, Vec<String>), Vec<&KnowledgeChunkRow>>::new();
    let mut eligible_rows = rows
        .iter()
        .filter(|row| table_section_sibling_row_is_visible(row))
        .filter(|row| anchor_sections.contains(&table_section_key(row)))
        .collect::<Vec<_>>();
    eligible_rows.sort_by(|left, right| {
        table_section_key(left)
            .cmp(&table_section_key(right))
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    for row in eligible_rows {
        let section_rows = selected_by_section.entry(table_section_key(row)).or_default();
        if section_rows.len() < limit_per_section {
            section_rows.push(row);
        }
    }

    let mut selected = selected_by_section.into_values().flatten().cloned().collect::<Vec<_>>();
    selected.sort_by(|left, right| {
        left.revision_id
            .cmp(&right.revision_id)
            .then_with(|| left.chunk_index.cmp(&right.chunk_index))
            .then_with(|| left.chunk_id.cmp(&right.chunk_id))
    });
    selected
}

fn table_section_key(row: &KnowledgeChunkRow) -> (Uuid, Vec<String>) {
    (row.revision_id, row.section_path.clone())
}

fn is_table_section_anchor_row(row: &KnowledgeChunkRow) -> bool {
    matches!(row.chunk_kind.as_deref(), Some("table_row"))
        || (matches!(row.chunk_kind.as_deref(), Some("heading"))
            && row.content_text.lines().any(|line| line.to_ascii_lowercase().contains("table:")))
}

fn table_section_sibling_row_is_visible(row: &KnowledgeChunkRow) -> bool {
    matches!(row.chunk_kind.as_deref(), Some("heading" | "table_row"))
        && (!row.content_text.trim().is_empty()
            || row.window_text.as_deref().is_some_and(|text| !text.trim().is_empty()))
}

pub(crate) async fn load_table_summary_chunks_for_documents(
    state: &AppState,
    document_index: &HashMap<Uuid, KnowledgeDocumentRow>,
    targeted_document_ids: &BTreeSet<Uuid>,
    limit_per_document: usize,
    keywords: &[String],
) -> anyhow::Result<Vec<RuntimeMatchedChunk>> {
    if targeted_document_ids.is_empty() || limit_per_document == 0 {
        return Ok(Vec::new());
    }

    let mut chunks = Vec::new();
    for document_id in targeted_document_ids {
        let Some(document) = document_index.get(document_id) else {
            continue;
        };
        let Some(revision_id) = canonical_document_revision_id(document) else {
            continue;
        };
        let revision_chunks =
            state.arango_document_store.list_chunks_by_revision(revision_id).await.with_context(
                || format!("failed to load table summaries for document {document_id}"),
            )?;
        let synthetic_base_score = 0.01_f32;
        chunks.extend(
            revision_chunks
                .into_iter()
                .filter(|chunk| {
                    chunk.chunk_kind.as_deref() == Some("metadata_block")
                        && is_table_summary_text(&chunk.normalized_text)
                })
                .take(limit_per_document)
                .enumerate()
                .filter_map(|(ordinal, chunk)| {
                    map_chunk_hit(
                        chunk,
                        synthetic_base_score - ordinal as f32 * 0.0001,
                        document_index,
                        keywords,
                    )
                }),
        );
    }

    Ok(chunks)
}

pub(crate) fn is_table_analytics_chunk(chunk: &RuntimeMatchedChunk) -> bool {
    let text = chunk.source_text.trim();
    is_table_summary_text(text) || (text.starts_with("Sheet: ") && text.contains(" | Row "))
}

pub(crate) fn merge_canonical_table_aggregation_chunks(
    existing_chunks: Vec<RuntimeMatchedChunk>,
    direct_summary_chunks: Vec<RuntimeMatchedChunk>,
    direct_row_chunks: Vec<RuntimeMatchedChunk>,
    top_k: usize,
) -> Vec<RuntimeMatchedChunk> {
    if direct_summary_chunks.is_empty() && direct_row_chunks.is_empty() {
        return existing_chunks;
    }

    let direct_chunks = merge_chunks(direct_summary_chunks, direct_row_chunks, top_k);
    let mut merged = merge_chunks(direct_chunks, existing_chunks, top_k);
    if merged.iter().any(is_table_analytics_chunk) {
        merged.retain(is_table_analytics_chunk);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domains::query_ir::QueryLanguage;
    use crate::infra::arangodb::document_store::KnowledgeChunkRow;

    fn table_row(
        chunk_id: Uuid,
        chunk_index: usize,
        normalized_text: &str,
        content_text: &str,
        kind: &str,
    ) -> KnowledgeChunkRow {
        KnowledgeChunkRow {
            key: chunk_id.to_string(),
            arango_id: None,
            arango_rev: None,
            chunk_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: Uuid::now_v7(),
            chunk_index: chunk_index as i32,
            chunk_kind: Some(kind.to_string()),
            content_text: content_text.to_string(),
            normalized_text: normalized_text.to_string(),
            span_start: None,
            span_end: None,
            token_count: None,
            support_block_ids: Vec::new(),
            section_path: Vec::new(),
            heading_trail: Vec::new(),
            literal_digest: None,
            chunk_state: "ready".to_string(),
            text_generation: Some(1),
            vector_generation: Some(1),
            quality_score: Some(1.0),
            window_text: None,
            raptor_level: None,
            occurred_at: None,
            occurred_until: None,
        }
    }

    fn section_row(
        document_id: Uuid,
        revision_id: Uuid,
        chunk_index: usize,
        kind: &str,
        text: &str,
        section_path: &[&str],
    ) -> KnowledgeChunkRow {
        let mut row = table_row(Uuid::now_v7(), chunk_index, text, text, kind);
        row.document_id = document_id;
        row.revision_id = revision_id;
        row.section_path = section_path.iter().map(|part| (*part).to_string()).collect();
        row
    }

    fn table_section_ir() -> QueryIR {
        QueryIR {
            act: QueryAct::Enumerate,
            scope: QueryScope::SingleDocument,
            language: QueryLanguage::Auto,
            target_types: vec!["table_row".to_string(), "table_summary".to_string()],
            target_entities: Vec::new(),
            literal_constraints: Vec::new(),
            temporal_constraints: Vec::new(),
            comparison: None,
            document_focus: None,
            conversation_refs: Vec::new(),
            needs_clarification: None,
            source_slice: None,
            retrieval_query: None,
            confidence: 0.9,
        }
    }

    #[test]
    fn table_section_siblings_keep_heading_and_same_section_rows() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let rows = vec![
            section_row(
                document_id,
                revision_id,
                0,
                "heading",
                "## Table: records",
                &["schema", "records"],
            ),
            section_row(
                document_id,
                revision_id,
                1,
                "table_row",
                "| field_one | text |",
                &["schema", "records"],
            ),
            section_row(
                document_id,
                revision_id,
                2,
                "table_row",
                "| field_two | text |",
                &["schema", "records"],
            ),
            section_row(document_id, revision_id, 3, "paragraph", "notes", &["schema", "records"]),
            section_row(
                document_id,
                revision_id,
                4,
                "heading",
                "## Table: events",
                &["schema", "events"],
            ),
            section_row(
                document_id,
                revision_id,
                5,
                "table_row",
                "| event_id | text |",
                &["schema", "events"],
            ),
        ];
        let anchors = vec![rows[2].clone()];

        let selected = select_table_section_sibling_rows(&rows, &anchors, 8);

        assert_eq!(selected.iter().map(|row| row.chunk_index).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn table_section_siblings_require_typed_table_inventory_ir() {
        let mut ir = table_section_ir();
        assert!(query_ir_requests_table_section_siblings(&ir));

        ir.target_types = vec!["table_row".to_string()];
        assert!(!query_ir_requests_table_section_siblings(&ir));

        ir = table_section_ir();
        ir.scope = QueryScope::MultiDocument;
        assert!(!query_ir_requests_table_section_siblings(&ir));
    }

    #[test]
    fn table_row_keyword_match_count_prefers_matching_rows_for_exact_value_queries() {
        let keywords = vec!["route_map".to_string(), "threshold".to_string()];
        let matching_row = table_row(
            Uuid::now_v7(),
            20,
            "system route_map inventory route_map = enabled",
            "unused",
            "table_row",
        );
        let distant_matching_row = table_row(
            Uuid::now_v7(),
            0,
            "other unrelated table_row",
            "route_map threshold 42",
            "table_row",
        );
        let non_matching_row = table_row(
            Uuid::now_v7(),
            2,
            "other unrelated table_row",
            "no technical tokens",
            "table_row",
        );

        let matching = table_row_keyword_match_count(&matching_row, &keywords);
        let distant_matching = table_row_keyword_match_count(&distant_matching_row, &keywords);
        let non_matching = table_row_keyword_match_count(&non_matching_row, &keywords);

        assert_eq!(matching, 2);
        assert_eq!(distant_matching, 2);
        assert_eq!(non_matching, 0);
    }

    #[test]
    fn table_row_keyword_match_count_is_case_insensitive() {
        let keywords = vec!["Route_Map".to_string()];
        let row = table_row(Uuid::now_v7(), 0, "Route_Map Inventory", "", "table_row");

        assert_eq!(table_row_keyword_match_count(&row, &keywords), 1);
    }
}
