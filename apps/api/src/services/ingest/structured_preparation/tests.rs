mod comparison;

use std::collections::HashSet;

use chrono::Utc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::blocks::{classify_scalar_block_kind, detect_boilerplate};
use super::{PrepareStructuredRevisionCommand, StructuredPreparationService};
use crate::shared::extraction::{
    ExtractionLineHint, ExtractionLineSignal, ExtractionOutput, ExtractionStructureHints,
    build_text_layout_from_content,
    file_extract::build_local_file_extraction_plan,
    record_jsonl::{StructuredRecordHint, extract_record_jsonl, normalize_structured_records},
    structured_document::StructuredBlockKind,
};

fn native_pdf_plan(pages: &[&str]) -> crate::shared::extraction::file_extract::FileExtractionPlan {
    use crate::integrations::{
        extraction_protocol::{Extraction, Limits, Operation, Request, Response, Unit},
        xberg::{XbergSource, project_native, validate_response},
    };
    use crate::shared::extraction::file_extract::build_xberg_pdf_extraction_plan;

    let count = u32::try_from(pages.len()).unwrap();
    let mut request = Request {
        path: "/var/tmp/source.pdf".into(),
        operation: Operation::Extract,
        mime: "application/pdf".into(),
        source_sha256: "a".repeat(64),
        limits: Limits::default(),
        start: Some(1),
        end: Some(count),
    };
    request.limits.text_bytes = 1024 * 1024;
    let native = Extraction {
        source_sha256: "a".repeat(64),
        text_byte_limit: request.limits.text_bytes,
        source_unit_count: count,
        unit_kind: "page".into(),
        selected_start: 1,
        selected_end: count,
        content: pages.join("\n\n"),
        units: pages
            .iter()
            .enumerate()
            .map(|(index, content)| Unit {
                ordinal: u32::try_from(index + 1).unwrap(),
                sheet_name: None,
                content: (*content).into(),
                tables: serde_json::json!([]),
            })
            .collect(),
        visual_inputs: vec![],
        warnings: vec![],
    };
    let native =
        validate_response(Response::new(Ok(native), std::time::Duration::ZERO, None), &request)
            .expect("validated native response");
    let extracted = project_native(
        native,
        XbergSource {
            file_name: Some("source.pdf"),
            mime_type: Some("application/pdf"),
            source_format: "pdf",
        },
    )
    .expect("native projection");
    build_xberg_pdf_extraction_plan(extracted)
}

fn prepare_native_pdf_plan(
    plan: crate::shared::extraction::file_extract::FileExtractionPlan,
) -> super::PreparedStructuredRevision {
    prepare_extracted_content(
        plan.source_format_metadata.source_format,
        plan.normalization_profile,
        plan.source_text.unwrap(),
        plan.normalized_text.unwrap(),
        plan.structure_hints,
        StructuredPreparationService::default(),
    )
}

#[test]
fn native_pdf_page_leading_rows_are_not_promoted_to_headers() {
    let first = "## Orders\n\n| Column | Type | Constraints | Description |\n| --- | --- | --- | --- |\n| id | UUID | PRIMARY KEY | Identifier |";
    let second = "| currency | CHAR(3) | NOT NULL, DEFAULT 'USD' | ISO 4217 currency code |\n| --- | --- | --- | --- |\n| shipping_address_json | JSONB | NOT NULL | Structured address |\n| created_at | TIMESTAMPTZ | NOT NULL, DEFAULT now() | Order placement timestamp |";
    let plan = native_pdf_plan(&[first, second]);
    for text in [plan.source_text.as_deref().unwrap(), plan.normalized_text.as_deref().unwrap()] {
        assert_unknown_pdf_header_policy(text);
    }
    let prepared = prepare_native_pdf_plan(plan);
    let rows = prepared
        .ordered_blocks
        .iter()
        .filter(|block| {
            block.block_kind == StructuredBlockKind::TableRow && block.page_number == Some(2)
        })
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3, "the first actual data row must survive as a row");
    assert!(rows[0].normalized_text.contains("col_1: currency"));
    assert!(rows[0].normalized_text.contains("col_2: CHAR(3)"));
    assert!(rows[1].normalized_text.contains("col_1: shipping_address_json"));
    assert!(rows[1].normalized_text.contains("col_2: JSONB"));
    assert!(rows[2].normalized_text.contains("col_1: created_at"));
    for row in rows {
        assert!(!row.normalized_text.contains("currency: shipping_address_json"));
        assert!(row.parent_block_id.is_some());
        assert!(row.source_span.is_some());
        assert_eq!(row.heading_trail, ["Orders"]);
    }
    assert!(
        !prepared
            .ordered_blocks
            .iter()
            .any(|block| block.block_kind == StructuredBlockKind::MetadataBlock
                && block.parent_block_id.is_some()),
        "unknown source roles do not justify column statistics"
    );
}

fn assert_unknown_pdf_header_policy(text: &str) {
    assert!(text.contains("data-irg-header-policy=\"unknown\""));
    assert!(
        !text.contains("| currency | CHAR(3)"),
        "full document must not declare the data row a Markdown header"
    );
    assert!(text.contains("shipping_address_json"));
}

#[test]
fn native_pdf_nonleading_independent_tables_keep_every_row_and_separate_parents() {
    let page = "Running page text.\n\n| key | amount |\n| --- | --- |\n| first | 5 |\n\n| other | state |\n| --- | --- |\n| second | ready |";
    let prepared = prepare_native_pdf_plan(native_pdf_plan(&[page]));
    let rows = prepared
        .ordered_blocks
        .iter()
        .filter(|block| block.block_kind == StructuredBlockKind::TableRow)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0].parent_block_id, rows[1].parent_block_id);
    assert_eq!(rows[2].parent_block_id, rows[3].parent_block_id);
    assert_ne!(rows[1].parent_block_id, rows[2].parent_block_id);
    assert!(rows[2].normalized_text.contains("col_1: other"));
    assert!(rows[3].normalized_text.contains("col_2: ready"));
}

#[test]
fn native_pdf_repeated_possible_header_rows_are_retained_without_inheritance() {
    let first = "| key | amount |\n| --- | --- |\n| first | 5 |";
    let second = "| key | amount |\n| --- | --- |\n| second | 9 |";
    let prepared = prepare_native_pdf_plan(native_pdf_plan(&[first, second]));
    let rows = prepared
        .ordered_blocks
        .iter()
        .filter(|block| block.block_kind == StructuredBlockKind::TableRow)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 4);
    assert!(rows[2].normalized_text.contains("col_1: key"));
    assert!(rows[3].normalized_text.contains("col_1: second"));
    assert_ne!(rows[0].parent_block_id, rows[2].parent_block_id);
    assert_eq!(rows[3].page_number, Some(2));
}

#[test]
fn native_pdf_literal_cells_separator_data_and_unicode_source_spans_survive_preparation() {
    use crate::shared::extraction::table_markdown::{
        split_semantic_key_value, split_semantic_segments, unescape_semantic_segment,
    };
    let first = "Preface 🦀\n\n| key | value |\n| --- | --- |\n| literal | \\<br\\> |\n| entity | \\&amp; |\n| - | - |\n| : | --- |\n| \\- | \\- |\n| ключ | café 🦀 |";
    let second = "Continuation ü.\n\n| tail | café |\n| --- | --- |\n| lone |";
    let plan = native_pdf_plan(&[first, second]);
    let source = plan.source_text.as_ref().unwrap().clone();
    let normalized = plan.normalized_text.as_ref().unwrap().clone();
    assert!(source.contains("&lt;br&gt;"));
    assert!(source.contains("&amp;amp;"));
    let source_rows = plan
        .structure_hints
        .lines
        .iter()
        .filter(|line| line.text.starts_with("<tr>"))
        .cloned()
        .collect::<Vec<_>>();
    let prepared = prepare_native_pdf_plan(plan);
    let rows = prepared
        .ordered_blocks
        .iter()
        .filter(|block| block.block_kind == StructuredBlockKind::TableRow)
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 9);
    assert_eq!(source_rows.len(), rows.len());
    assert_eq!(rows.iter().filter(|row| row.page_number == Some(1)).count(), 7);
    assert_eq!(rows.iter().filter(|row| row.page_number == Some(2)).count(), 2);
    let values = rows
        .iter()
        .map(|row| {
            split_semantic_segments(&row.normalized_text)
                .into_iter()
                .filter_map(split_semantic_key_value)
                .filter(|(key, _)| key.starts_with("col_"))
                .map(|(_, value)| unescape_semantic_segment(value))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert_eq!(values[1], ["literal", "<br>"]);
    assert_eq!(values[2], ["entity", "&amp;"]);
    assert_eq!(values[3], ["-", "-"]);
    assert_eq!(values[4], [":", "---"]);
    assert_eq!(values[5], ["-", "-"]);
    assert_eq!(values[6], ["ключ", "café 🦀"]);
    assert_eq!(values[8], ["lone"]);
    assert_pdf_row_source_spans(&rows, &source_rows, &normalized);
    assert_ne!(rows[0].parent_block_id, rows[8].parent_block_id);
}

fn assert_pdf_row_source_spans(
    rows: &[&super::StructuredBlockData],
    source_rows: &[ExtractionLineHint],
    normalized: &str,
) {
    for (row, line) in rows.iter().zip(source_rows) {
        let span = row.source_span.as_ref().unwrap();
        assert_eq!(Some(span.start_offset), line.start_offset);
        assert_eq!(Some(span.end_offset), line.end_offset);
        assert_eq!(row.page_number, line.page_number);
        let actual = normalized
            .chars()
            .skip(usize::try_from(span.start_offset).unwrap())
            .take(usize::try_from(span.end_offset - span.start_offset).unwrap())
            .collect::<String>();
        assert_eq!(actual, line.text);
        assert!(row.parent_block_id.is_some());
    }
}

fn prepare_page_lines(entries: &[(i32, &str)]) -> super::PreparedStructuredRevision {
    let lines = entries
        .iter()
        .enumerate()
        .map(|(ordinal, (page_number, text))| ExtractionLineHint {
            ordinal: i32::try_from(ordinal).unwrap_or(i32::MAX),
            page_number: Some(*page_number),
            text: (*text).to_string(),
            ..ExtractionLineHint::default()
        })
        .collect::<Vec<_>>();
    let text = lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>().join("\n");
    StructuredPreparationService::new()
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "pdf".to_string(),
                language_code: None,
                source_text: text.clone(),
                normalized_text: text,
                structure_hints: ExtractionStructureHints { lines },
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared revision")
}

fn prepare_record_extraction(
    extracted: ExtractionOutput,
    service: StructuredPreparationService,
) -> super::PreparedStructuredRevision {
    prepare_extracted_content(
        extracted.extraction_kind,
        "default".to_string(),
        extracted.content_text.clone(),
        extracted.content_text,
        extracted.structure_hints,
        service,
    )
}

fn prepare_extracted_content(
    source_format: String,
    normalization_profile: String,
    source_text: String,
    normalized_text: String,
    structure_hints: ExtractionStructureHints,
    service: StructuredPreparationService,
) -> super::PreparedStructuredRevision {
    service
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile,
                source_format,
                language_code: None,
                source_text,
                normalized_text,
                structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared revision")
}

fn prepare_local_file_extraction(
    payload: &[u8],
    file_name: &str,
    service: StructuredPreparationService,
) -> super::PreparedStructuredRevision {
    let extracted = build_local_file_extraction_plan(Some(file_name), None, payload)
        .expect("local file extraction");
    prepare_extracted_content(
        extracted.extraction_kind,
        extracted.normalization_profile,
        extracted.source_text.expect("source text"),
        extracted.normalized_text.expect("normalized text"),
        extracted.structure_hints,
        service,
    )
}

fn prepared_profile_chunks(service: StructuredPreparationService) -> Vec<String> {
    let text = (0..8)
        .map(|index| format!("Segment {index}: {}.", "alpha ".repeat(110)))
        .collect::<Vec<_>>()
        .join("\n\n");
    prepare_extracted_content(
        "txt".to_string(),
        "default".to_string(),
        text.clone(),
        text,
        ExtractionStructureHints::default(),
        service,
    )
    .chunk_windows
    .into_iter()
    .map(|chunk| chunk.normalized_text)
    .collect()
}

#[test]
fn default_preparation_uses_the_library_naive_template() {
    use crate::domains::catalog::ChunkingTemplate;

    let canonical = prepared_profile_chunks(StructuredPreparationService::with_template(
        ChunkingTemplate::Naive,
    ));
    assert!(!canonical.is_empty());
    assert_eq!(prepared_profile_chunks(StructuredPreparationService::new()), canonical);
    assert_eq!(prepared_profile_chunks(StructuredPreparationService::default()), canonical);
}

#[test]
fn library_templates_drive_prepared_chunk_boundaries() {
    use crate::domains::catalog::ChunkingTemplate;

    for template in [
        ChunkingTemplate::Naive,
        ChunkingTemplate::Structured,
        ChunkingTemplate::Tabular,
        ChunkingTemplate::Qa,
        ChunkingTemplate::Code,
        ChunkingTemplate::Paper,
    ] {
        let (max_chars, overlap_chars) = template.chunking_params();
        assert_eq!(
            prepared_profile_chunks(StructuredPreparationService::with_template(template)),
            prepared_profile_chunks(StructuredPreparationService::with_chunking(
                max_chars,
                overlap_chars,
            )),
            "template {template:?} must select its own preparation profile"
        );
    }
    assert!(
        prepared_profile_chunks(StructuredPreparationService::with_template(ChunkingTemplate::Qa))
            .len()
            > prepared_profile_chunks(StructuredPreparationService::with_template(
                ChunkingTemplate::Naive,
            ))
            .len()
    );
}

#[test]
fn prepare_revision_derives_outline_from_heading_blocks() {
    let text = "# REST API\n\n## Authentication\n\nGET /v1/status\n";
    let prepared = StructuredPreparationService::new()
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "md".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared revision");

    assert!(prepared.prepared_revision.outline.iter().any(|entry| entry.heading == "REST API"));
    assert!(
        prepared.prepared_revision.outline.iter().any(|entry| entry.heading == "Authentication")
    );
    assert!(prepared.chunk_windows.iter().any(|chunk| !chunk.heading_trail.is_empty()));
}

#[test]
fn prepare_revision_classifies_lists_tables_and_endpoints() {
    // Tables must use valid markdown table syntax with header separator;
    // informal pipe-delimited text is no longer auto-classified as a table.
    let text = "# Products\n\n- Operations Console\n\n| Method | Path |\n| --- | --- |\n| GET | /v1/status |\n\nGET /v1/status\n";
    let prepared = StructuredPreparationService::new()
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "md".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared revision");

    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::ListItem))
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::Table))
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::TableRow))
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| matches!(block.block_kind, StructuredBlockKind::EndpointBlock))
    );
}

#[test]
fn endpoint_signal_is_not_overridden_by_url_path_vocabulary() {
    let line = ExtractionLineHint {
        text: "GET https://service.example/x/resource".to_string(),
        signals: vec![ExtractionLineSignal::EndpointCandidate],
        ..ExtractionLineHint::default()
    };

    assert_eq!(classify_scalar_block_kind(&line), StructuredBlockKind::EndpointBlock);
}

#[test]
fn prepare_revision_preserves_record_source_profile_as_structural_chunk() {
    let extracted = extract_record_jsonl(
        br#"{"id":"unit-1","kind":"message","occurredAt":"2026-04-28T09:00:00Z","actor":{"role":"user","label":"User One"},"text":"First unit"}
{"id":"unit-2","kind":"message","occurredAt":"2026-04-28T10:00:00Z","actor":{"role":"assistant","label":"Assistant"},"text":"Second unit"}"#,
    )
    .expect("record jsonl extraction");

    let prepared = prepare_record_extraction(extracted, StructuredPreparationService::new());

    assert_eq!(prepared.ordered_blocks[0].block_kind, StructuredBlockKind::SourceProfile);
    assert_eq!(prepared.ordered_blocks[1].block_kind, StructuredBlockKind::SourceUnit);
    assert_eq!(prepared.ordered_blocks[2].block_kind, StructuredBlockKind::SourceUnit);
    assert_eq!(prepared.chunk_windows[0].chunk_kind, StructuredBlockKind::SourceProfile);
    assert_eq!(prepared.chunk_windows[1].chunk_kind, StructuredBlockKind::SourceUnit);
    assert_eq!(prepared.chunk_windows[2].chunk_kind, StructuredBlockKind::SourceUnit);
    assert!(prepared.chunk_windows[0].content_text.contains("unit_count=2"));
    assert!(prepared.chunk_windows[1].content_text.contains("First unit"));
    assert!(prepared.chunk_windows[2].content_text.contains("Second unit"));
}

#[test]
fn prepare_revision_keeps_yaml_comments_with_the_annotated_field_chunk() {
    let payload = include_bytes!("../../../../tests/fixtures/extraction/config-comments.yaml");
    let extracted = normalize_structured_records(payload, StructuredRecordHint::Yaml)
        .expect("annotated yaml extraction");
    let prepared =
        prepare_record_extraction(extracted, StructuredPreparationService::with_chunking(100, 0));

    let port_chunk = prepared
        .chunk_windows
        .iter()
        .find(|chunk| chunk.content_text.contains("services.rabbitmq.ports="))
        .expect("port chunk present");
    assert!(port_chunk.content_text.contains("# AMQP port: 5672"));
    assert!(port_chunk.content_text.contains("# Management UI port: 15672"));
}

#[test]
fn prepare_revision_exposes_yaml_header_comments_as_document_prose() {
    let payload =
        include_bytes!("../../../../tests/fixtures/extraction/config-header-comments.yaml");
    let prepared = prepare_local_file_extraction(
        payload,
        "runtime-units.yaml",
        StructuredPreparationService::with_chunking(180, 0),
    );
    let expected_forms = [
        "Gateway Node: External request routing and session handoff (port 7301)",
        "Ledger Node: Event persistence and audit lookup (port 7302)",
        "Relay Node: Background notifications and message delivery (port 7303)",
    ];

    let record_header_chunk = prepared
        .chunk_windows
        .iter()
        .find(|chunk| {
            chunk.chunk_kind == StructuredBlockKind::SourceUnit
                && expected_forms.iter().all(|form| chunk.content_text.contains(form))
        })
        .expect("header remains anchored to a record chunk");
    assert!(
        record_header_chunk.content_text.contains("services.gateway.image="),
        "the header must anchor to the first record chunk, got: {}",
        record_header_chunk.content_text
    );

    let header_chunk = prepared
        .chunk_windows
        .iter()
        .find(|chunk| {
            chunk.chunk_kind == StructuredBlockKind::Paragraph
                && expected_forms.iter().all(|form| chunk.content_text.contains(form))
        })
        .expect("searchable document header prose chunk present");
    assert!(header_chunk.content_text.starts_with("Runtime units:"));
}

#[test]
fn prepare_revision_builds_semantic_table_row_text_and_preserves_raw_row_text() {
    let text = "## people\n\n| Name | Email |\n| --- | --- |\n| Alice | alice@example.com |\n";
    let prepared = StructuredPreparationService::new()
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "csv".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared revision");

    let row_block = prepared
        .ordered_blocks
        .iter()
        .find(|block| matches!(block.block_kind, StructuredBlockKind::TableRow))
        .expect("table row block");

    assert_eq!(row_block.text, "| Alice | alice@example.com |");
    let parent = prepared
        .ordered_blocks
        .iter()
        .find(|block| Some(block.block_id) == row_block.parent_block_id)
        .expect("source Table parent");
    assert_eq!(parent.block_kind, StructuredBlockKind::Table);
    assert_eq!(
        row_block.normalized_text,
        format!(
            "Sheet: people | Table Ref: {} | Row 1 | Name: Alice | Email: alice@example.com",
            parent.ordinal
        )
    );
    assert!(
        prepared
            .chunk_windows
            .iter()
            .any(|chunk| chunk.chunk_kind == StructuredBlockKind::TableRow
                && chunk.normalized_text.contains("Name: Alice")),
        "table rows must produce queryable chunks"
    );
}

#[test]
fn prepare_revision_keeps_single_column_markdown_tables_as_table_blocks() {
    let text = "## test1\n\n| col_1 |\n| --- |\n| test1 |\n";
    let prepared = StructuredPreparationService::new()
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "xls".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared revision");

    let block_kinds =
        prepared.ordered_blocks.iter().map(|block| block.block_kind).collect::<Vec<_>>();
    assert_eq!(
        block_kinds,
        vec![
            StructuredBlockKind::Heading,
            StructuredBlockKind::Table,
            StructuredBlockKind::TableRow,
        ]
    );

    let row_block = prepared
        .ordered_blocks
        .iter()
        .find(|block| block.block_kind == StructuredBlockKind::TableRow)
        .expect("table row block");
    assert_eq!(row_block.text, "| test1 |");
    let parent = prepared
        .ordered_blocks
        .iter()
        .find(|block| Some(block.block_id) == row_block.parent_block_id)
        .expect("source Table parent");
    assert_eq!(parent.block_kind, StructuredBlockKind::Table);
    assert_eq!(
        row_block.normalized_text,
        format!("Sheet: test1 | Table Ref: {} | Row 1 | col_1: test1", parent.ordinal)
    );
}

#[test]
fn prepare_revision_builds_table_summary_metadata_blocks() {
    let text = "## organizations\n\n| Country | Employees |\n| --- | --- |\n| Sweden | 10 |\n| Benin | 20 |\n| Sweden | 30 |\n";
    let prepared = StructuredPreparationService::new()
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "csv".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.to_string(),
                normalized_text: text.to_string(),
                structure_hints: build_text_layout_from_content(text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared revision");

    let summary_blocks = prepared
        .ordered_blocks
        .iter()
        .filter(|block| block.block_kind == StructuredBlockKind::MetadataBlock)
        .collect::<Vec<_>>();

    assert_eq!(summary_blocks.len(), 2);
    assert!(summary_blocks.iter().any(|block| {
        block.normalized_text.contains("Table Summary")
            && block.normalized_text.contains("Column: Country")
            && block.parent_block_id.is_some()
    }));
    assert!(summary_blocks.iter().any(|block| {
        block.normalized_text.contains("Table Summary")
            && block.normalized_text.contains("Column: Employees")
            && block.parent_block_id.is_some()
    }));
}

#[test]
fn prepare_revision_persists_split_code_blocks_before_chunking() {
    let mut code = String::new();
    for function_index in 0..8 {
        code.push_str(&format!("fn func_{function_index}() {{\n"));
        for line_index in 0..12 {
            code.push_str(&format!(
                "    let value_{line_index} = \"synthetic segment {function_index}-{line_index}\";\n"
            ));
        }
        code.push_str("}\n\n");
    }
    let text = format!("# Code\n\n```\n{code}```\n");
    let prepared = StructuredPreparationService::with_chunking(220, 0)
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "default".to_string(),
                source_format: "md".to_string(),
                language_code: Some("en".to_string()),
                source_text: text.clone(),
                normalized_text: text.clone(),
                structure_hints: build_text_layout_from_content(&text).structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared revision");

    let known_block_ids =
        prepared.ordered_blocks.iter().map(|block| block.block_id).collect::<HashSet<_>>();
    let code_blocks = prepared
        .ordered_blocks
        .iter()
        .filter(|block| block.block_kind == StructuredBlockKind::CodeBlock)
        .collect::<Vec<_>>();

    assert!(code_blocks.len() > 1, "large code block should be split before chunking");
    assert!(
        code_blocks.iter().all(|block| block.parent_block_id.is_none()),
        "split code blocks must not point at a discarded parent block"
    );
    assert!(
        prepared
            .chunk_windows
            .iter()
            .flat_map(|chunk| chunk.support_block_ids.iter())
            .all(|block_id| known_block_ids.contains(block_id)),
        "chunk support ids must all reference persisted structured blocks"
    );
    assert_eq!(
        prepared.prepared_revision.block_count,
        i32::try_from(prepared.ordered_blocks.len()).unwrap_or(i32::MAX)
    );
    assert_eq!(
        prepared.prepared_revision.chunk_count,
        i32::try_from(prepared.chunk_windows.len()).unwrap_or(i32::MAX)
    );
}

#[test]
fn prepare_revision_allows_empty_normalized_content() {
    let prepared = StructuredPreparationService::new()
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: Uuid::now_v7(),
                document_id: Uuid::now_v7(),
                workspace_id: Uuid::now_v7(),
                library_id: Uuid::now_v7(),
                preparation_state: "prepared".to_string(),
                normalization_profile: "verbatim_v1".to_string(),
                source_format: "image".to_string(),
                language_code: None,
                source_text: String::new(),
                normalized_text: String::new(),
                structure_hints: build_text_layout_from_content("").structure_hints,
                typed_fact_count: 0,
                prepared_at: Utc::now(),
            },
            &CancellationToken::new(),
        )
        .expect("prepared empty revision");

    assert_eq!(prepared.prepared_revision.block_count, 0);
    assert_eq!(prepared.prepared_revision.chunk_count, 0);
    assert!(prepared.ordered_blocks.is_empty());
    assert!(prepared.chunk_windows.is_empty());
}

#[test]
fn detect_boilerplate_catches_nav_links() {
    assert!(
        detect_boilerplate(
            StructuredBlockKind::Paragraph,
            "Home | About | Contact | Blog | FAQ | Support",
        ),
        "pipe-separated nav links should be detected as boilerplate"
    );
}

#[test]
fn detect_boilerplate_catches_breadcrumbs() {
    assert!(
        detect_boilerplate(
            StructuredBlockKind::Paragraph,
            "Documentation > API Reference > Authentication > OAuth",
        ),
        "breadcrumb pattern should be detected as boilerplate"
    );
}

#[test]
fn detect_boilerplate_preserves_phrase_only_prose() {
    assert!(
        !detect_boilerplate(
            StructuredBlockKind::Paragraph,
            "We use cookies to improve your experience. Accept cookies",
        ),
        "natural-language wording alone must not discard evidence"
    );
}

#[test]
fn detect_boilerplate_skips_normal_text() {
    assert!(
        !detect_boilerplate(
            StructuredBlockKind::Paragraph,
            "FastAPI is a modern, fast web framework for building APIs with Python."
        ),
        "normal technical text should not be detected as boilerplate"
    );
}

#[test]
fn detect_boilerplate_uses_link_density_instead_of_absolute_link_count() {
    let text = "A long evidence paragraph references https://one.test, https://two.test, \
        https://three.test, https://four.test, and https://five.test while retaining enough \
        surrounding source material that the links are not the dominant block structure.";

    assert!(
        !detect_boilerplate(StructuredBlockKind::Paragraph, text),
        "link count alone must not discard an evidence-rich paragraph"
    );
}

#[test]
fn detect_boilerplate_marks_a_dense_link_block() {
    assert!(detect_boilerplate(
        StructuredBlockKind::Paragraph,
        "https://one.test https://two.test https://three.test",
    ));
}

#[test]
fn prepare_revision_marks_exact_short_blocks_repeated_across_pages() {
    let repeated = "Repeated structural block";
    let prepared = prepare_page_lines(&[
        (1, repeated),
        (2, repeated),
        (3, repeated),
        (4, repeated),
        (5, repeated),
        (3, "Unique evidence block"),
    ]);

    assert_eq!(
        prepared
            .ordered_blocks
            .iter()
            .filter(|block| block.text == repeated && block.is_boilerplate)
            .count(),
        5
    );
    assert!(
        prepared
            .ordered_blocks
            .iter()
            .any(|block| block.text == "Unique evidence block" && !block.is_boilerplate)
    );
}

#[test]
fn prepare_revision_keeps_content_when_every_block_matches_a_structural_shape() {
    let prepared = prepare_page_lines(&[(1, "A | B | C | D")]);

    assert!(prepared.ordered_blocks.iter().all(|block| !block.is_boilerplate));
}

#[test]
fn detect_boilerplate_preserves_pipe_tables() {
    assert!(
        !detect_boilerplate(StructuredBlockKind::TableRow, "| Name | Value | Status |"),
        "table rows must not be dropped as navigation boilerplate"
    );
}

#[test]
fn detect_boilerplate_preserves_record_stream_units() {
    let unit = "[unit_ordinal=0] fields: \
        services.api.environment.PUBLIC_URL=http://api.local; \
        services.api.healthcheck.test=CMD-SHELL, curl -f http://localhost:8000/health || exit 1; \
        services.auth.environment.AUTH_URL=http://auth.local; \
        services.user.environment.AUTH_SERVICE_URL=http://auth-service:3001; \
        services.notification.environment.WEBHOOK_URL=https://hooks.example.test/events";

    assert!(
        !detect_boilerplate(StructuredBlockKind::SourceUnit, unit),
        "structured record units are authoritative evidence, not web navigation boilerplate"
    );
}

#[test]
fn prepare_revision_keeps_a_soft_wrapped_paragraph_in_one_block() {
    // Markdown wraps prose at the source line width, so a single sentence and
    // the qualifier that scopes it routinely sit on different physical lines.
    // Splitting them into separate blocks strands a value in one block and the
    // condition that restricts it in another, which no downstream consumer can
    // put back together.
    let text = "# Service matrix\n\nThe primary listener at node-a.internal:8443 is the only entry point\nreachable from outside the boundary. Probe paths are served on\nnode-b.internal:9090, never on the primary listener.\n";
    let prepared = prepare_extracted_content(
        "md".to_string(),
        "default".to_string(),
        text.to_string(),
        text.to_string(),
        build_text_layout_from_content(text).structure_hints,
        StructuredPreparationService::new(),
    );

    let carrying_value = prepared
        .ordered_blocks
        .iter()
        .filter(|block| block.text.contains("node-a.internal:8443"))
        .collect::<Vec<_>>();
    assert_eq!(carrying_value.len(), 1, "the value must live in exactly one block");
    assert!(
        carrying_value[0].text.contains("reachable from outside the boundary"),
        "the qualifier scoping the value must stay in the same block, got: {}",
        carrying_value[0].text
    );
    assert!(
        prepared.ordered_blocks.iter().any(|block| block.text.contains("node-b.internal:9090")
            && block.text.contains("never on the primary listener")),
        "a negated statement must keep its subject in the same block"
    );
}

#[test]
fn prepare_revision_keeps_listed_endpoint_lines_addressable() {
    // A line holding nothing but an address is an entry someone looks up on
    // its own, so prose above it must not swallow it into one block.
    let text =
        "# Endpoints\n\nThe following endpoints are available:\n/v1/alpha\n/v1/beta\n/v1/gamma\n";
    let prepared = prepare_extracted_content(
        "md".to_string(),
        "default".to_string(),
        text.to_string(),
        text.to_string(),
        build_text_layout_from_content(text).structure_hints,
        StructuredPreparationService::new(),
    );

    for path in ["/v1/alpha", "/v1/beta", "/v1/gamma"] {
        assert_eq!(
            prepared.ordered_blocks.iter().filter(|block| block.text == path).count(),
            1,
            "{path} must stay its own block"
        );
    }
}
