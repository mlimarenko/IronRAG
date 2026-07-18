use super::*;
use crate::domains::query_ir::{QueryAct, QueryIR, QueryLanguage, QueryScope, QueryTargetKind};
use crate::services::query::execution::RuntimeChunkScoreKind;
use crate::shared::extraction::table_markdown::build_semantic_table_row_text;

fn table_inventory_ir(target_types: Vec<QueryTargetKind>) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types,
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: Some("dataset_00".to_string()),
        confidence: 0.95,
    }
}

fn parsed_inventory_row(
    document_id: Uuid,
    table_name: &str,
    row_number: usize,
    fields: &[(&str, &str)],
) -> ParsedTableRow {
    ParsedTableRow {
        document_id,
        sheet_name: "sheet_01".to_string(),
        table_name: Some(table_name.to_string()),
        row_number,
        fields: fields
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect(),
        flattened_text: fields
            .iter()
            .flat_map(|(key, value)| [*key, *value])
            .collect::<Vec<_>>()
            .join(" "),
        score: 1.0,
    }
}

fn runtime_table_chunk(
    document_id: Uuid,
    chunk_index: i32,
    chunk_kind: &str,
    source_text: &str,
) -> RuntimeMatchedChunk {
    RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id,
        revision_id: Uuid::now_v7(),
        chunk_index,
        chunk_kind: Some(chunk_kind.to_string()),
        document_label: "document_01".to_string(),
        excerpt: String::new(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: source_text.to_string(),
    }
}

#[test]
fn table_column_inventory_requires_typed_table_intent() {
    let untyped = table_inventory_ir(Vec::new());
    assert!(!query_ir_requests_table_column_inventory(Some(&untyped)));

    let typed = table_inventory_ir(vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary]);
    assert!(query_ir_requests_table_column_inventory(Some(&typed)));
}

#[test]
fn structured_inventory_preserves_arbitrary_source_fields_without_role_words() {
    let document_id = Uuid::now_v7();
    let rows = vec![
        parsed_inventory_row(
            document_id,
            "dataset_17",
            1,
            &[("f_01", "v_01"), ("f_02", "v_02"), ("f_03", "v_03")],
        ),
        parsed_inventory_row(
            document_id,
            "dataset_17",
            2,
            &[("f_01", "v_11"), ("f_02", "v_12"), ("f_03", "v_13")],
        ),
    ];
    let mut ir = table_inventory_ir(vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary]);
    ir.retrieval_query = Some("dataset_17".to_string());

    assert_eq!(
        build_table_column_inventory_answer(Some(&ir), &rows),
        Some(
            "`dataset_17`:\n- `f_01` = `v_01`; `f_02` = `v_02`; `f_03` = `v_03`\n- `f_01` = `v_11`; `f_02` = `v_12`; `f_03` = `v_13`"
                .to_string()
        )
    );
}

#[test]
fn raw_pipe_inventory_requires_typed_target_when_sections_are_ambiguous() {
    let document_id = Uuid::now_v7();
    let chunks = [
        runtime_table_chunk(document_id, 0, "heading", "## dataset_17"),
        runtime_table_chunk(document_id, 1, "table_row", "| v_01 | v_02 |"),
        runtime_table_chunk(document_id, 2, "heading", "## dataset_18"),
        runtime_table_chunk(document_id, 3, "table_row", "| v_11 | v_12 |"),
    ];
    let chunk_refs = chunks.iter().collect::<Vec<_>>();
    let mut ir = table_inventory_ir(vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary]);
    ir.retrieval_query = None;

    assert_eq!(build_raw_pipe_table_column_inventory_answer(Some(&ir), &chunk_refs), None);
}

#[test]
fn raw_pipe_inventory_uses_typed_intent_with_arbitrary_heading_text() {
    let document_id = Uuid::now_v7();
    let chunks = [
        runtime_table_chunk(document_id, 0, "heading", "## dataset_17"),
        runtime_table_chunk(document_id, 1, "table_row", "| v_01 | v_02 |"),
        runtime_table_chunk(document_id, 2, "table_row", "| v_11 | v_12 |"),
    ];
    let chunk_refs = chunks.iter().collect::<Vec<_>>();
    let mut ir = table_inventory_ir(vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary]);
    ir.retrieval_query = Some("dataset_17".to_string());

    assert_eq!(
        build_raw_pipe_table_column_inventory_answer(Some(&ir), &chunk_refs),
        Some("`dataset_17`:\n- `v_01`; `v_02`\n- `v_11`; `v_12`".to_string())
    );
}

#[test]
fn structural_section_label_preserves_identifier_punctuation() {
    assert_eq!(
        extract_structural_section_label("## _dataset_17-"),
        Some("_dataset_17-".to_string())
    );
}

#[test]
fn semantic_table_row_protocol_round_trips_its_canonical_emitter() {
    let source_text = build_semantic_table_row_text(
        Some("sheet_01"),
        Some("dataset_17"),
        2,
        &["f_01".to_string(), "f_02".to_string()],
        &["v_01".to_string(), "v_02".to_string()],
    );
    let chunk = runtime_table_chunk(Uuid::now_v7(), 2, "table_row", &source_text);

    let parsed = parse_table_row_chunk(&chunk).expect("canonical protocol should parse");

    assert_eq!(parsed.sheet_name, "sheet_01");
    assert_eq!(parsed.table_name.as_deref(), Some("dataset_17"));
    assert_eq!(parsed.row_number, 3);
    assert_eq!(
        parsed.fields,
        vec![("f_01".to_string(), "v_01".to_string()), ("f_02".to_string(), "v_02".to_string())]
    );
}
