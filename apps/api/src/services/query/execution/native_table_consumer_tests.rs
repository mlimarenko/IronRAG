//! Deterministic regressions for the native table content consumer.
//!
//! Native logical tables use the shared canonical renderer, alongside ordinary
//! prose. The tests drive the real pipeline
//! `build_text_layout_from_content -> normalize_for_structured_preparation ->
//! StructuredPreparationService::prepare_revision -> build_structured_chunk_windows`
//! and then the private query row/summary decoders, without external services.

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::domains::query_ir::{QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind};
use crate::services::ingest::structured_preparation::{
    PrepareStructuredRevisionCommand, PreparedStructuredRevision, StructuredPreparationError,
    StructuredPreparationService,
};
use crate::shared::extraction::{
    build_text_layout_from_content,
    structured_document::{StructuredBlockData, StructuredBlockKind, StructuredChunkWindow},
    table_logical::{LogicalCellDraft, LogicalTable, TableHeaderOrigin, TableLayoutError},
    table_summary::{TableSummaryValueKind, is_table_summary_text, parse_table_column_summary},
    text_render::normalize_for_structured_preparation,
};

use super::table_row_answer::{ParsedTableRow, parse_table_row_chunk};
use super::table_summary_answer::build_table_summary_grounded_answer;
use super::types::{RuntimeChunkScoreKind, RuntimeMatchedChunk};

fn inventory_content(merged_header: bool) -> String {
    let mut cells = if merged_header {
        vec![
            LogicalCellDraft::new(0, 0, "Inventory").with_anchor("r0c0").with_spans(1, 2),
            LogicalCellDraft::new(0, 2, "Location").with_anchor("r0c2"),
            LogicalCellDraft::new(1, 0, "Material"),
            LogicalCellDraft::new(1, 1, "Stock"),
            LogicalCellDraft::new(1, 2, "Bin"),
        ]
    } else {
        vec![
            LogicalCellDraft::new(0, 0, "Material"),
            LogicalCellDraft::new(0, 1, "Stock"),
            LogicalCellDraft::new(0, 2, "Bin"),
        ]
    };
    let header_rows = if merged_header { 2 } else { 1 };
    for (index, values) in
        [["Copper", "7", "Amber"], ["Silver", "19", "Violet"], ["Zinc", "31", "Indigo"]]
            .into_iter()
            .enumerate()
    {
        for (column, value) in values.into_iter().enumerate() {
            cells.push(LogicalCellDraft::new(header_rows + index, column, value));
        }
    }
    let table = LogicalTable::from_cells(header_rows + 3, 3, cells)
        .expect("inventory table")
        .render_canonical();
    let (before, after) = if merged_header {
        ("MONTHLY INVENTORY", "ARCHIVE NOTE OUTSIDE THE GRID")
    } else {
        ("WAREHOUSE ENGLISH SCAN", "END OF STOCK TABLE")
    };
    format!("{before}\n\n{table}\n\n{after}")
}

fn prepare(content: &str) -> (PreparedStructuredRevision, String) {
    try_prepare(content).unwrap_or_else(|error| panic!("prepare failed: {error:?}"))
}

fn try_prepare(
    content: &str,
) -> Result<(PreparedStructuredRevision, String), StructuredPreparationError> {
    let layout = build_text_layout_from_content(content);
    let normalized =
        normalize_for_structured_preparation(&layout.content_text, Some(&layout.structure_hints));
    let prepared = StructuredPreparationService::new().prepare_revision(
        PrepareStructuredRevisionCommand {
            revision_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            preparation_state: "prepared".to_string(),
            normalization_profile: normalized.normalization_profile.clone(),
            source_format: "png".to_string(),
            language_code: Some("en".to_string()),
            source_text: layout.content_text,
            normalized_text: normalized.normalized_text.clone(),
            structure_hints: normalized.structure_hints,
            typed_fact_count: 0,
            prepared_at: Utc::now(),
        },
        &CancellationToken::new(),
    )?;
    Ok((prepared, normalized.normalized_text))
}

fn runtime_chunk(document_id: Uuid, chunk: &StructuredChunkWindow) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id: Uuid::now_v7(),
        chunk_index: chunk.chunk_index,
        chunk_kind: Some(chunk.chunk_kind.as_str().to_string()),
        document_label: "native consumer regression".to_string(),
        excerpt: String::new(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: chunk.normalized_text.clone(),
    }
}

fn blocks_of(
    prepared: &PreparedStructuredRevision,
    kind: StructuredBlockKind,
) -> Vec<&StructuredBlockData> {
    prepared.ordered_blocks.iter().filter(|block| block.block_kind == kind).collect()
}

fn row_chunks(prepared: &PreparedStructuredRevision) -> Vec<&StructuredChunkWindow> {
    prepared
        .chunk_windows
        .iter()
        .filter(|chunk| chunk.chunk_kind == StructuredBlockKind::TableRow)
        .collect()
}

fn parsed_rows(prepared: &PreparedStructuredRevision, document_id: Uuid) -> Vec<ParsedTableRow> {
    row_chunks(prepared)
        .into_iter()
        .filter_map(|chunk| parse_table_row_chunk(&runtime_chunk(document_id, chunk)))
        .collect()
}

fn summary_by_column(
    prepared: &PreparedStructuredRevision,
    column: &str,
) -> crate::shared::extraction::table_summary::TableColumnSummary {
    prepared
        .chunk_windows
        .iter()
        .filter(|chunk| {
            chunk.chunk_kind == StructuredBlockKind::MetadataBlock
                && is_table_summary_text(&chunk.normalized_text)
        })
        .filter_map(|chunk| parse_table_column_summary(&chunk.normalized_text))
        .find(|summary| summary.column_name == column)
        .unwrap_or_else(|| panic!("summary for column {column:?}"))
}

fn normalized_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn table_average_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::TableAverage],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: Some("average stock".to_string()),
        confidence: 0.95,
    }
}

#[test]
fn native_table_consumer_english_rows_and_stock_summary() {
    let content = inventory_content(false);
    let (prepared, _) = prepare(&content);
    let document_id = Uuid::now_v7();

    let chunks = row_chunks(&prepared);
    assert_eq!(chunks.len(), 3, "exactly three body-row chunks");
    let rows = parsed_rows(&prepared, document_id);
    assert_eq!(rows.len(), 3, "parse_table_row_chunk decodes every row chunk");
    assert_eq!(
        rows[0].fields,
        vec![
            ("Material".to_string(), "Copper".to_string()),
            ("Stock".to_string(), "7".to_string()),
            ("Bin".to_string(), "Amber".to_string()),
        ]
    );
    assert_eq!(
        rows[1].fields,
        vec![
            ("Material".to_string(), "Silver".to_string()),
            ("Stock".to_string(), "19".to_string()),
            ("Bin".to_string(), "Violet".to_string()),
        ]
    );
    assert_eq!(
        rows[2].fields,
        vec![
            ("Material".to_string(), "Zinc".to_string()),
            ("Stock".to_string(), "31".to_string()),
            ("Bin".to_string(), "Indigo".to_string()),
        ]
    );

    let stock = summary_by_column(&prepared, "Stock");
    assert_eq!(stock.value_kind, TableSummaryValueKind::Numeric);
    assert_eq!(stock.row_count, 3);
    assert_eq!(stock.non_empty_count, 3);
    assert_eq!(stock.min, Some(7.0));
    assert_eq!(stock.max, Some(31.0));
    assert_eq!(stock.average, Some(19.0));

    let runtime_chunks = prepared
        .chunk_windows
        .iter()
        .map(|chunk| runtime_chunk(document_id, chunk))
        .collect::<Vec<_>>();
    let answer = build_table_summary_grounded_answer(
        "average stock",
        Some(&table_average_ir()),
        &runtime_chunks,
    )
    .expect("average answer from canonical native content");
    assert_eq!(answer, "The average `Stock` is `19` across `3` rows.");
}

fn assert_merged_inventory_projection(logical: &LogicalTable) {
    let inventory = logical.cell_by_anchor("r0c0").expect("inventory source anchor");
    assert_eq!(inventory.text(), "Inventory");
    assert_eq!(inventory.row_span(), 1);
    assert_eq!(inventory.column_span(), 2);
    assert_eq!(
        logical.cells().iter().filter(|cell| cell.text() == "Inventory").count(),
        1,
        "Inventory stays one source cell, never two copies"
    );
    assert_eq!(logical.cell_by_anchor("r0c2").expect("location").column_span(), 1);

    let projection = logical.project(Default::default()).expect("derived projection");
    assert_eq!(projection.header.origin, TableHeaderOrigin::DerivedAnchorBand);
    assert_eq!(
        projection.header.paths.iter().map(|path| path.display.as_str()).collect::<Vec<_>>(),
        vec!["Inventory / Material", "Inventory / Stock", "Location / Bin"]
    );
    assert_eq!(projection.body_rows.len(), 3);
}

fn assert_table_row_source_spans(
    row_blocks: &[&StructuredBlockData],
    table_block: &StructuredBlockData,
    normalized_text: &str,
) {
    for row in row_blocks {
        assert_eq!(row.parent_block_id, Some(table_block.block_id));
        let span = row.source_span.as_ref().expect("row source span");
        assert!(span.start_offset < span.end_offset);
        assert_eq!(
            &normalized_text[usize::try_from(span.start_offset).expect("start")
                ..usize::try_from(span.end_offset).expect("end")],
            row.text,
            "row source span resolves the original table row line"
        );
        assert!(table_block.source_span.as_ref().is_some_and(|table_span| {
            table_span.start_offset <= span.start_offset && table_span.end_offset >= span.end_offset
        }));
    }
}

fn assert_table_row_chunk_support(
    prepared: &PreparedStructuredRevision,
    table_block: &StructuredBlockData,
    row_blocks: &[&StructuredBlockData],
) {
    for chunk in row_chunks(prepared) {
        assert!(chunk.support_block_ids.contains(&table_block.block_id));
        assert!(row_blocks.iter().any(|row| chunk.support_block_ids.contains(&row.block_id)));
        assert!(!chunk.normalized_text.contains("data-irg-anchor"));
    }
}

#[test]
fn native_table_consumer_merged_header_keeps_one_anchor_and_derived_paths() {
    let content = inventory_content(true);
    let (prepared, normalized_text) = prepare(&content);
    let document_id = Uuid::now_v7();

    let table_blocks = blocks_of(&prepared, StructuredBlockKind::Table);
    assert_eq!(table_blocks.len(), 1);
    let table_block = table_blocks[0];
    let lines = table_block.text.lines().collect::<Vec<_>>();
    let logical = LogicalTable::parse_canonical_lines(&lines).expect("canonical table block");
    assert_merged_inventory_projection(&logical);
    assert!(content.contains("data-irg-header-policy=\"anchor-band-derived-v1\""));

    let row_blocks = blocks_of(&prepared, StructuredBlockKind::TableRow);
    assert_eq!(row_blocks.len(), 3, "the header band is not a body row");
    assert_table_row_source_spans(&row_blocks, table_block, &normalized_text);
    assert_table_row_chunk_support(&prepared, table_block, &row_blocks);

    let rows = parsed_rows(&prepared, document_id);
    assert_eq!(
        rows[0].fields,
        vec![
            ("Inventory / Material".to_string(), "Copper".to_string()),
            ("Inventory / Stock".to_string(), "7".to_string()),
            ("Location / Bin".to_string(), "Amber".to_string()),
        ]
    );
    let stock = summary_by_column(&prepared, "Inventory / Stock");
    assert_eq!(stock.row_count, 3);
    assert_eq!(stock.non_empty_count, 3);
    assert_eq!(stock.min, Some(7.0));
    assert_eq!(stock.max, Some(31.0));
    assert_eq!(stock.average, Some(19.0));
}

#[test]
fn native_table_consumer_captions_stay_outside_cells_and_summaries() {
    for (merged_header, captions) in [
        (false, &["WAREHOUSE ENGLISH SCAN", "END OF STOCK TABLE"][..]),
        (true, &["MONTHLY INVENTORY", "ARCHIVE NOTE OUTSIDE THE GRID"][..]),
    ] {
        let content = inventory_content(merged_header);
        let (prepared, _) = prepare(&content);
        let tables = blocks_of(&prepared, StructuredBlockKind::Table);
        assert_eq!(tables.len(), 1);
        let lines = tables[0].text.lines().collect::<Vec<_>>();
        let logical = LogicalTable::parse_canonical_lines(&lines).expect("canonical table block");
        assert_eq!(row_chunks(&prepared).len(), 3);
        let stock_column = if merged_header { "Inventory / Stock" } else { "Stock" };
        assert_eq!(summary_by_column(&prepared, stock_column).row_count, 3);
        for caption in captions {
            assert!(logical.cells().iter().all(|cell| !cell.text().contains(caption)));
            assert!(
                prepared
                    .ordered_blocks
                    .iter()
                    .any(|block| block.text.contains(caption)
                        || block.normalized_text.contains(caption)),
                "caption {caption:?} must stay ordinary content"
            );
            for row in blocks_of(&prepared, StructuredBlockKind::TableRow) {
                assert!(!row.normalized_text.contains(caption));
            }
            for chunk in &prepared.chunk_windows {
                if chunk.chunk_kind == StructuredBlockKind::MetadataBlock {
                    assert!(!chunk.normalized_text.contains(caption));
                }
            }
        }
    }
}

#[test]
fn native_table_consumer_aligned_and_column_prose_create_no_table_structure() {
    for (case, passages) in [
        (
            "aligned-prose",
            &[
                "Copper arrived Monday.    Stock was counted twice.\nSilver arrived Tuesday.   Stock remained in storage.\nZinc arrived Wednesday.   Stock will be checked tomorrow.",
                "The aligned sentences describe deliveries, not a stock grid.",
            ][..],
        ),
        (
            "prose-columns",
            &[
                "Receiving notes                     Dispatch notes\nThe team counted 7 copper crates.    The amber bin is reserved.\nAnother 19 silver crates arrived.    The violet bin remains closed.",
                "The final shipment held 31 zinc crates. The indigo bin needs inspection.",
            ][..],
        ),
    ] {
        let content = passages.join("\n\n");
        let (prepared, _) = prepare(&content);

        assert!(blocks_of(&prepared, StructuredBlockKind::Table).is_empty(), "{case}: table block");
        assert!(
            blocks_of(&prepared, StructuredBlockKind::TableRow).is_empty(),
            "{case}: table row block"
        );
        assert!(
            prepared.chunk_windows.iter().all(|chunk| {
                chunk.chunk_kind != StructuredBlockKind::TableRow
                    && !(chunk.chunk_kind == StructuredBlockKind::MetadataBlock
                        && is_table_summary_text(&chunk.normalized_text))
            }),
            "{case}: table or summary chunk"
        );

        let all_text = prepared
            .ordered_blocks
            .iter()
            .map(|block| normalized_whitespace(&block.text))
            .collect::<Vec<_>>()
            .join(" ");
        for passage in passages {
            assert!(
                all_text.contains(&normalized_whitespace(passage)),
                "{case}: passage not retained: {}",
                passage
            );
        }
    }
}

#[test]
fn native_table_consumer_plain_summary_prefix_metadata_is_not_a_derived_summary() {
    let source = "Table Summary: notes";
    let (prepared, _) = prepare(source);
    let metadata = blocks_of(&prepared, StructuredBlockKind::MetadataBlock);
    assert_eq!(metadata.len(), 1);
    assert_eq!(metadata[0].text, source);
    assert!(metadata[0].parent_block_id.is_none());
    assert_eq!(metadata[0].normalized_text, source);
    assert!(prepared.chunk_windows.iter().any(|chunk| chunk.content_text.contains(source)));
    assert!(row_chunks(&prepared).is_empty());
}

#[test]
fn native_table_consumer_source_refs_follow_final_ordinals_after_code_splitting() {
    let table = LogicalTable::from_cells(
        3,
        1,
        vec![
            LogicalCellDraft::new(0, 0, "Stock"),
            LogicalCellDraft::new(1, 0, "7"),
            LogicalCellDraft::new(2, 0, "19"),
        ],
    )
    .expect("source")
    .render_canonical();
    let source = format!("```text\n{}\n```\n\n{table}", "x".repeat(3000));
    let (prepared, _) = prepare(&source);
    let code = blocks_of(&prepared, StructuredBlockKind::CodeBlock);
    assert!(code.len() >= 2, "preparation must actually expand the preceding code block");
    let table = blocks_of(&prepared, StructuredBlockKind::Table)[0];
    assert!(table.ordinal >= 2);
    let document = Uuid::now_v7();
    for row in row_chunks(&prepared) {
        let parsed = parse_table_row_chunk(&runtime_chunk(document, row)).expect("row");
        assert_eq!(parsed.table_ref, Some(table.ordinal));
        assert!(row.support_block_ids.contains(&table.block_id));
    }
    for chunk in
        prepared.chunk_windows.iter().filter(|chunk| is_table_summary_text(&chunk.normalized_text))
    {
        assert_eq!(
            parse_table_column_summary(&chunk.normalized_text).unwrap().table_ref,
            Some(table.ordinal)
        );
        assert!(chunk.support_block_ids.contains(&table.block_id));
    }
    for block in
        prepared.ordered_blocks.iter().filter(|block| block.parent_block_id == Some(table.block_id))
    {
        assert!(block.normalized_text.contains(&format!("Table Ref: {} | ", table.ordinal)));
    }
    let chunks = prepared
        .chunk_windows
        .iter()
        .map(|chunk| runtime_chunk(document, chunk))
        .collect::<Vec<_>>();
    assert_eq!(
        build_table_summary_grounded_answer("average Stock", Some(&table_average_ir()), &chunks),
        Some("The average `Stock` is `13` across `2` rows.".to_string())
    );
}

#[test]
fn native_table_consumer_aggregation_merge_retains_unnamed_rows_with_summaries() {
    let source = LogicalTable::from_cells(
        3,
        1,
        vec![
            LogicalCellDraft::new(0, 0, "Stock"),
            LogicalCellDraft::new(1, 0, "7"),
            LogicalCellDraft::new(2, 0, "19"),
        ],
    )
    .expect("source table")
    .render_canonical();
    let (prepared, _) = prepare(&source);
    let document = Uuid::now_v7();
    let rows = row_chunks(&prepared)
        .into_iter()
        .map(|chunk| runtime_chunk(document, chunk))
        .collect::<Vec<_>>();
    let summary = prepared
        .chunk_windows
        .iter()
        .find(|chunk| is_table_summary_text(&chunk.normalized_text))
        .expect("summary");
    let mut prose = runtime_chunk(document, summary);
    prose.source_text = "Ordinary paragraph, not a table.".into();
    prose.chunk_kind = Some("paragraph".into());
    let row_ids = rows.iter().map(|row| row.chunk_id).collect::<Vec<_>>();
    let merged = super::table_retrieval::merge_table_aggregation_chunks(
        vec![prose],
        vec![runtime_chunk(document, summary)],
        rows,
        16,
    );
    assert_eq!(merged.len(), 3);
    for row_id in row_ids {
        assert!(merged.iter().any(|chunk| chunk.chunk_id == row_id));
    }
    assert_eq!(merged.iter().filter(|chunk| parse_table_row_chunk(chunk).is_some()).count(), 2);
}

fn assert_stock_averages_by_table(chunks: &[RuntimeMatchedChunk], expected: &[(i32, &str)]) {
    for &(reference, average) in expected {
        let scoped = chunks
            .iter()
            .filter(|chunk| parse_table_row_chunk(chunk).unwrap().table_ref == Some(reference))
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(
            build_table_summary_grounded_answer(
                "average Stock",
                Some(&table_average_ir()),
                &scoped
            ),
            Some(format!("The average `Stock` is `{average}` across `2` rows."))
        );
    }
}

#[test]
fn native_table_consumer_unnamed_tables_keep_distinct_rows_and_summary_answers() {
    let make_table = |first: &str, second: &str| {
        LogicalTable::from_cells(
            3,
            1,
            vec![
                LogicalCellDraft::new(0, 0, "Stock"),
                LogicalCellDraft::new(1, 0, first),
                LogicalCellDraft::new(2, 0, second),
            ],
        )
        .expect("source table")
        .render_canonical()
    };
    let (prepared, _) = prepare(&format!(
        "{}\n\nBetween tables.\n\n{}",
        make_table("7", "19"),
        make_table("100", "200")
    ));
    let parents = blocks_of(&prepared, StructuredBlockKind::Table);
    assert_eq!(parents.len(), 2);
    let document_id = Uuid::now_v7();
    let rows = row_chunks(&prepared);
    assert_eq!(rows.len(), 4);
    for (index, row) in rows.iter().enumerate() {
        let parsed = parse_table_row_chunk(&runtime_chunk(document_id, row)).expect("semantic row");
        assert!(parsed.sheet_name.is_empty());
        assert!(parsed.table_name.is_none());
        assert_eq!(parsed.table_ref, Some(parents[index / 2].ordinal));
        assert_eq!(parsed.fields.len(), 1);
    }
    // The review's actual collision order: A1, B1, B2, A2.
    let interleaved = [0, 2, 3, 1].map(|index| runtime_chunk(document_id, rows[index]));
    assert!(
        build_table_summary_grounded_answer(
            "average Stock",
            Some(&table_average_ir()),
            &interleaved
        )
        .is_none(),
        "two independent tables must not manufacture the mixed average 59.50"
    );
    assert_stock_averages_by_table(
        &interleaved,
        &[(parents[0].ordinal, "13"), (parents[1].ordinal, "150")],
    );
    let summaries = prepared
        .chunk_windows
        .iter()
        .filter(|chunk| is_table_summary_text(&chunk.normalized_text))
        .map(|chunk| parse_table_column_summary(&chunk.normalized_text).expect("summary"))
        .collect::<Vec<_>>();
    assert_eq!(summaries.len(), 2);
    assert_ne!(summaries[0].table_ref, summaries[1].table_ref);
    assert_eq!(summaries[0].average, Some(13.0));
    assert_eq!(summaries[1].average, Some(150.0));
    let all = prepared
        .chunk_windows
        .iter()
        .map(|chunk| runtime_chunk(document_id, chunk))
        .collect::<Vec<_>>();
    assert!(
        build_table_summary_grounded_answer("average Stock", Some(&table_average_ir()), &all)
            .is_none()
    );
}

#[test]
fn native_table_consumer_markdown_literal_values_use_the_same_projection() {
    let headers = vec!["Header | Name".to_string(), "Text: label".to_string()];
    let values = vec![
        vec!["<br>".to_string(), r"C:\Documents".to_string()],
        vec!["<table>".to_string(), "R&D | &lt;text&gt;; literal".to_string()],
    ];
    let content =
        crate::shared::extraction::table_markdown::render_markdown_table(&headers, &values)
            .expect("Markdown table");
    let (prepared, _) = prepare(&content);
    let rows = parsed_rows(&prepared, Uuid::now_v7());
    assert_eq!(rows.len(), values.len());
    for (actual, expected) in rows.iter().zip(&values) {
        assert_eq!(
            actual.fields,
            headers.iter().cloned().zip(expected.iter().cloned()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn native_table_consumer_code_and_literal_tags_do_not_create_tables() {
    let table = LogicalTable::from_cells(
        2,
        1,
        vec![LogicalCellDraft::new(0, 0, "Header"), LogicalCellDraft::new(1, 0, "Value")],
    )
    .expect("code example")
    .render_canonical();
    for content in [
        format!("```html\n{table}\n```"),
        table.replace('\n', ""),
        table.replace('<', "\\<"),
        table.lines().map(|line| format!("    {line}")).collect::<Vec<_>>().join("\n"),
        "<tr><td data-irg-anchor=\"a\" rowspan=\"1\" colspan=\"1\">Literal</td></tr>".into(),
        "A literal <table> example and &lt;table&gt; entity.".into(),
    ] {
        let (prepared, _) = prepare(&content);
        assert!(blocks_of(&prepared, StructuredBlockKind::Table).is_empty());
        assert!(row_chunks(&prepared).is_empty());
        assert!(
            prepared
                .chunk_windows
                .iter()
                .all(|chunk| !is_table_summary_text(&chunk.normalized_text))
        );
    }
}

#[test]
fn native_table_consumer_derived_headers_and_parent_support_without_external_fixtures() {
    let table = LogicalTable::from_cells(
        5,
        2,
        vec![
            LogicalCellDraft::new(0, 0, "Inventory").with_spans(1, 2),
            LogicalCellDraft::new(1, 0, "Material"),
            LogicalCellDraft::new(1, 1, "Stock"),
            LogicalCellDraft::new(2, 0, "Copper"),
            LogicalCellDraft::new(2, 1, "7"),
            LogicalCellDraft::new(3, 0, "Silver"),
            LogicalCellDraft::new(3, 1, "19"),
            LogicalCellDraft::new(4, 0, "Zinc"),
            LogicalCellDraft::new(4, 1, "31"),
        ],
    )
    .expect("derived header table");
    let (prepared, _) = prepare(&format!("SOURCE CAPTION\n\n{}", table.render_canonical()));
    let parent = blocks_of(&prepared, StructuredBlockKind::Table)[0];
    let rows = parsed_rows(&prepared, Uuid::now_v7());
    assert_eq!(rows.len(), 3);
    assert_eq!(
        rows[0].fields,
        vec![
            ("Inventory / Material".into(), "Copper".into()),
            ("Inventory / Stock".into(), "7".into()),
        ]
    );
    let summary = summary_by_column(&prepared, "Inventory / Stock");
    assert_eq!((summary.row_count, summary.average), (3, Some(19.0)));
    for chunk in row_chunks(&prepared) {
        assert!(chunk.support_block_ids.contains(&parent.block_id));
        assert!(chunk.support_block_ids.len() >= 2);
        assert!(!chunk.normalized_text.contains("SOURCE CAPTION"));
    }
}

#[test]
fn native_table_consumer_literal_markup_and_delimiters_round_trip() {
    let table = LogicalTable::from_cells(
        4,
        3,
        vec![
            LogicalCellDraft::new(0, 0, "Note: Type").with_anchor("h0"),
            LogicalCellDraft::new(0, 1, "Pipe | Header").with_anchor("h1"),
            LogicalCellDraft::new(0, 2, r"Back\slash").with_anchor("h2"),
            LogicalCellDraft::new(1, 0, "A | B"),
            LogicalCellDraft::new(1, 1, r"C\D"),
            LogicalCellDraft::new(1, 2, "R&D"),
            LogicalCellDraft::new(2, 0, "<br>"),
            LogicalCellDraft::new(2, 1, "<table>"),
            LogicalCellDraft::new(2, 2, ": value"),
            LogicalCellDraft::new(3, 0, "semi; colon"),
            LogicalCellDraft::new(3, 1, "&lt;table&gt;"),
            LogicalCellDraft::new(3, 2, "after: value | tail"),
        ],
    )
    .expect("literal table");
    let rendered = table.render_canonical();
    let content = format!("Literal table\n\n{rendered}\n\nAfter the table");
    let (prepared, _) = prepare(&content);
    let document_id = Uuid::now_v7();

    assert_eq!(blocks_of(&prepared, StructuredBlockKind::Table).len(), 1);
    assert_eq!(blocks_of(&prepared, StructuredBlockKind::TableRow).len(), 3);

    let rows = parsed_rows(&prepared, document_id);
    assert_eq!(
        rows[0].fields,
        vec![
            ("Note: Type".to_string(), "A | B".to_string()),
            ("Pipe | Header".to_string(), r"C\D".to_string()),
            (r"Back\slash".to_string(), "R&D".to_string()),
        ]
    );
    assert_eq!(
        rows[1].fields,
        vec![
            ("Note: Type".to_string(), "<br>".to_string()),
            ("Pipe | Header".to_string(), "<table>".to_string()),
            (r"Back\slash".to_string(), ": value".to_string()),
        ]
    );

    for column in ["Note: Type", "Pipe | Header", r"Back\slash"] {
        let summary = summary_by_column(&prepared, column);
        assert_eq!(summary.non_empty_count, 3);
    }
    let pipe = summary_by_column(&prepared, "Pipe | Header");
    assert!(pipe.most_frequent_values.contains(&r"C\D".to_string()));
    assert!(pipe.most_frequent_values.contains(&"<table>".to_string()));
    assert!(pipe.most_frequent_values.contains(&"&lt;table&gt;".to_string()));
    assert!(
        summary_by_column(&prepared, "Note: Type")
            .most_frequent_values
            .contains(&"semi; colon".to_string())
    );

    let table_text = blocks_of(&prepared, StructuredBlockKind::Table)[0].text.clone();
    assert!(table_text.contains("&lt;table&gt;"));
    assert!(table_text.contains("&amp;"));
}

#[test]
fn native_table_consumer_invalid_topology_fails_explicitly() {
    let mut cells = vec![
        LogicalCellDraft::new(0, 0, "A").with_anchor("r0c0"),
        LogicalCellDraft::new(0, 1, "B").with_anchor("r0c1"),
        LogicalCellDraft::new(1, 0, "1").with_anchor("r1c0"),
        LogicalCellDraft::new(1, 1, "2").with_anchor("r1c1"),
        LogicalCellDraft::new(2, 0, "3").with_anchor("r2c0"),
        LogicalCellDraft::new(2, 1, "4").with_anchor("r2c1"),
    ];
    let valid =
        LogicalTable::from_cells(3, 2, cells.clone()).expect("valid table").render_canonical();

    let duplicate_anchor =
        valid.replacen("data-irg-anchor=\"r1c1\"", "data-irg-anchor=\"r1c0\"", 1);
    assert!(matches!(
        try_prepare(&duplicate_anchor),
        Err(StructuredPreparationError::TableLayout(TableLayoutError::DuplicateAnchor { .. }))
    ));

    let out_of_bounds = valid.replacen("data-irg-columns=\"2\"", "data-irg-columns=\"1\"", 1);
    assert!(matches!(
        try_prepare(&out_of_bounds),
        Err(StructuredPreparationError::TableLayout(TableLayoutError::OutOfBounds { .. }))
    ));

    let ragged =
        valid.replacen("<td data-irg-anchor=\"r1c1\" rowspan=\"1\" colspan=\"1\">2</td>", "", 1);
    assert!(matches!(
        try_prepare(&ragged),
        Err(StructuredPreparationError::TableLayout(TableLayoutError::RaggedTable { .. }))
    ));

    cells[2].row_span = 2;
    let overlap = LogicalTable::from_cells(3, 2, cells).expect_err("overlap must fail");
    assert!(matches!(overlap, TableLayoutError::DuplicateCoveredSlot { .. }));

    let ambiguous = LogicalTable::from_cells(
        3,
        3,
        vec![
            LogicalCellDraft::new(0, 0, "Inventory").with_anchor("inventory").with_spans(1, 2),
            LogicalCellDraft::new(0, 2, "Location").with_anchor("location"),
            LogicalCellDraft::new(1, 0, "Material").with_anchor("material"),
            LogicalCellDraft::new(1, 1, "Stock").with_anchor("stock").with_spans(1, 2),
            LogicalCellDraft::new(2, 0, "Copper"),
            LogicalCellDraft::new(2, 1, "7"),
            LogicalCellDraft::new(2, 2, "Amber"),
        ],
    )
    .expect("valid topology")
    .render_canonical();
    assert!(matches!(
        try_prepare(&ambiguous),
        Err(StructuredPreparationError::TableLayout(TableLayoutError::AmbiguousHeaderBand { .. }))
    ));

    let unsupported_body_span = LogicalTable::from_cells(
        3,
        2,
        vec![
            LogicalCellDraft::new(0, 0, "Material").with_anchor("h0"),
            LogicalCellDraft::new(0, 1, "Stock").with_anchor("h1"),
            LogicalCellDraft::new(1, 0, "Copper").with_anchor("b1").with_spans(2, 1),
            LogicalCellDraft::new(1, 1, "7").with_anchor("n1"),
            LogicalCellDraft::new(2, 1, "19").with_anchor("n2"),
        ],
    )
    .expect("valid topology")
    .render_canonical();
    let result = try_prepare(&unsupported_body_span);
    assert!(
        matches!(
            &result,
            Err(StructuredPreparationError::TableLayout(
                TableLayoutError::UnsupportedBodySpan { .. }
            ))
        ),
        "{result:?}"
    );
}
