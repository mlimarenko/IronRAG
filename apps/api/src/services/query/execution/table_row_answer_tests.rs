use super::*;
use crate::domains::query_ir::{
    EntityMention, EntityRole, LiteralKind, LiteralSpan, QueryAct, QueryIR, QueryLanguage,
    QueryScope, QueryTargetKind,
};
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

fn row_lookup_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![QueryTargetKind::TableRow],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 0.95,
    }
}

fn focused_lookup_chunks(document_id: Uuid) -> Vec<RuntimeMatchedChunk> {
    vec![
        runtime_table_chunk(
            document_id,
            0,
            "table_row",
            "Sheet: sheet_01 | Table: status_table | Row 1 | Account: Sable Ridge | Ticket: SR-A | Code: 403 | Description: insufficient permissions",
        ),
        runtime_table_chunk(
            document_id,
            1,
            "table_row",
            "Sheet: sheet_01 | Table: status_table | Row 2 | Account: Alpha | Ticket: SR-B | Code: 404 | Description: missing resource",
        ),
    ]
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
fn semantic_table_row_protocol_round_trips_its_shared_emitter() {
    let source_text = build_semantic_table_row_text(
        Some("sheet_01"),
        Some("dataset_17"),
        2,
        &["f_01".to_string(), "f_02".to_string()],
        &["v_01".to_string(), "v_02".to_string()],
    );
    let chunk = runtime_table_chunk(Uuid::now_v7(), 2, "table_row", &source_text);

    let parsed = parse_table_row_chunk(&chunk).expect("normalized protocol should parse");

    assert_eq!(parsed.sheet_name, "sheet_01");
    assert_eq!(parsed.table_name.as_deref(), Some("dataset_17"));
    assert_eq!(parsed.row_number, 3);
    assert_eq!(
        parsed.fields,
        vec![("f_01".to_string(), "v_01".to_string()), ("f_02".to_string(), "v_02".to_string())]
    );
}

#[test]
fn focused_table_row_rejects_identity_tokens_shared_by_every_row_in_the_table() {
    let document_id = Uuid::now_v7();
    let rows = vec![
        parsed_inventory_row(
            document_id,
            "status_table",
            1,
            &[("Code", "403"), ("Description", "shared marker"), ("Context", "shared context")],
        ),
        parsed_inventory_row(
            document_id,
            "status_table",
            2,
            &[("Code", "404"), ("Description", "shared marker"), ("Context", "other context")],
        ),
    ];

    let answer = build_focused_table_row_field_answer(
        "What Code is specified for the shared context?",
        None,
        &rows,
        &[],
    );

    assert!(
        answer.is_none(),
        "shared value tokens cannot prove which table row answers the question: {answer:?}"
    );
}

#[test]
fn focused_table_row_accepts_a_question_matched_value_unique_to_its_table_row() {
    let document_id = Uuid::now_v7();
    let rows = vec![
        parsed_inventory_row(
            document_id,
            "status_table",
            1,
            &[("Code", "403"), ("Description", "insufficient permissions")],
        ),
        parsed_inventory_row(
            document_id,
            "status_table",
            2,
            &[("Code", "404"), ("Description", "missing resource")],
        ),
    ];

    assert_eq!(
        build_focused_table_row_field_answer(
            "What Description is listed for 403?",
            None,
            &rows,
            &[],
        ),
        Some("Code: `403`; Description: `insufficient permissions`".to_string())
    );
}

#[test]
fn focused_table_row_does_not_bypass_the_identity_guard_for_a_plural_header() {
    let question = "What HTTP methods should be used for CRUD operations?";
    let row = parsed_inventory_row(
        Uuid::now_v7(),
        "method_table",
        1,
        &[("Method", "GET"), ("CRUD", "Read")],
    );
    assert!(build_focused_table_row_field_answer(question, None, &[row], &[]).is_none());
}

#[test]
fn focused_table_row_keeps_rendering_when_selected_value_carries_structured_focus() {
    let document_id = Uuid::now_v7();
    let chunks = focused_lookup_chunks(document_id);
    let question = "What Description is listed for Code 403?";
    let expected = Some("Code: `403`; Description: `insufficient permissions`".to_string());

    let mut entity_ir = row_lookup_ir();
    entity_ir.target_entities =
        vec![EntityMention { label: "Code 403".to_string(), role: EntityRole::Subject }];

    let mut literal_ir = row_lookup_ir();
    literal_ir.literal_constraints =
        vec![LiteralSpan { text: "403".to_string(), kind: LiteralKind::NumericCode }];

    let mut retrieval_ir = row_lookup_ir();
    retrieval_ir.retrieval_query = Some("Code 403".to_string());

    for ir in [&entity_ir, &literal_ir, &retrieval_ir] {
        assert_eq!(build_table_row_grounded_answer(question, Some(ir), &chunks), expected,);
    }
}

#[test]
fn focused_table_row_keeps_rendering_when_focus_is_shared_by_peer_rows() {
    let document_id = Uuid::now_v7();
    let mut chunks = focused_lookup_chunks(document_id);
    chunks[1].source_text = chunks[1].source_text.replace("Alpha", "Sable Ridge");
    let question = "What Description is listed for Code 403 in Sable Ridge?";
    let expected = Some(
        "Account: `Sable Ridge`; Code: `403`; Description: `insufficient permissions`".to_string(),
    );
    let mut ir = row_lookup_ir();
    ir.target_entities =
        vec![EntityMention { label: "Sable Ridge".to_string(), role: EntityRole::Subject }];
    ir.retrieval_query = Some(question.to_string());

    assert_eq!(build_table_row_grounded_answer(question, None, &chunks), expected);
    assert_eq!(build_table_row_grounded_answer(question, Some(&ir), &chunks), expected);
}

#[test]
fn focused_table_row_ignores_semantic_row_protocol_metadata_when_proving_focus() {
    let document_id = Uuid::now_v7();
    let chunks = focused_lookup_chunks(document_id);
    let question = "What Description is listed for Code 403 in the status_table row?";
    let expected = Some("Code: `403`; Description: `insufficient permissions`".to_string());
    let mut ir = row_lookup_ir();
    ir.retrieval_query = Some(question.to_string());

    assert_eq!(build_table_row_grounded_answer(question, None, &chunks), expected);
    assert_eq!(build_table_row_grounded_answer(question, Some(&ir), &chunks), expected);
}

#[test]
fn focused_table_row_declines_when_structured_focus_matches_another_row_only() {
    let document_id = Uuid::now_v7();
    let mut chunks = focused_lookup_chunks(document_id);
    chunks[0].source_text = chunks[0]
        .source_text
        .replace("Sable Ridge", "Alphabet Holdings")
        .replace("insufficient permissions", "shared context");
    let question = "What Description is listed for Alpha in the shared context?";
    let mut ir = row_lookup_ir();
    ir.target_entities =
        vec![EntityMention { label: "Alpha".to_string(), role: EntityRole::Subject }];
    ir.retrieval_query = Some(question.to_string());

    assert_eq!(
        build_table_row_grounded_answer(question, None, &chunks),
        Some("Description: `shared context`".to_string()),
    );
    assert_eq!(build_table_row_grounded_answer(question, Some(&ir), &chunks), None);
}

#[test]
fn focused_table_row_declines_when_typed_focus_is_absent_from_the_table() {
    let document_id = Uuid::now_v7();
    let mut chunks = focused_lookup_chunks(document_id);
    chunks[0].source_text = chunks[0]
        .source_text
        .replace("Sable Ridge", "Alphabet Holdings")
        .replace("insufficient permissions", "shared context");
    let question = "What Description is listed for Omega in the shared context?";
    let mut ir = row_lookup_ir();
    ir.target_entities =
        vec![EntityMention { label: "Omega".to_string(), role: EntityRole::Subject }];
    ir.retrieval_query = Some(question.to_string());

    assert_eq!(
        build_table_row_grounded_answer(question, None, &chunks),
        Some("Description: `shared context`".to_string()),
    );
    assert_eq!(build_table_row_grounded_answer(question, Some(&ir), &chunks), None);
}

#[test]
fn focused_table_row_declines_when_repeated_retrieval_focus_is_absent_from_the_table() {
    let document_id = Uuid::now_v7();
    let mut chunks = focused_lookup_chunks(document_id);
    chunks[0].source_text = chunks[0]
        .source_text
        .replace("Sable Ridge", "Alphabet Holdings")
        .replace("insufficient permissions", "shared context");
    chunks.push(runtime_table_chunk(
        document_id,
        2,
        "paragraph",
        "Omega is documented outside the table.",
    ));
    chunks.push(runtime_table_chunk(
        document_id,
        3,
        "paragraph",
        "Additional material also describes Omega.",
    ));
    let question = "What Description is listed for Omega in the shared context?";
    let mut ir = row_lookup_ir();
    ir.retrieval_query = Some(question.to_string());

    assert_eq!(
        build_table_row_grounded_answer(question, None, &chunks),
        Some("Description: `shared context`".to_string()),
    );
    assert_eq!(build_table_row_grounded_answer(question, Some(&ir), &chunks), None);
}

#[test]
fn focused_table_row_declines_when_retrieval_focus_is_in_another_table() {
    let document_id = Uuid::now_v7();
    let chunks = vec![
        runtime_table_chunk(
            document_id,
            0,
            "table_row",
            "Sheet: sheet_01 | Table: status_table | Row 1 | Code: 403 | Ticket: VX-7 | Description: shared context",
        ),
        runtime_table_chunk(
            document_id,
            1,
            "table_row",
            "Sheet: sheet_01 | Table: status_table | Row 2 | Code: 404 | Description: other context",
        ),
        runtime_table_chunk(
            document_id,
            2,
            "table_row",
            "Sheet: sheet_01 | Table: subject_table | Row 1 | Subject: Omega | Detail: secondary material",
        ),
    ];
    let question = "What Description is listed for Omega in the VX-7 shared context?";
    let mut ir = row_lookup_ir();
    ir.retrieval_query = Some(question.to_string());

    assert_eq!(
        build_table_row_grounded_answer(question, None, &chunks),
        Some("Ticket: `VX-7`; Description: `shared context`".to_string()),
    );
    assert_eq!(build_table_row_grounded_answer(question, Some(&ir), &chunks), None);
}

#[test]
fn focused_raw_pipe_row_ignores_document_scope_label_when_proving_focus() {
    let document_id = Uuid::now_v7();
    let chunks = vec![runtime_table_chunk(document_id, 0, "table_row", "| VX-7 | enabled |")];
    let question = "What is listed for VX-7 in document_01?";
    let expected = Some("`VX-7`; `enabled`".to_string());
    let mut ir = row_lookup_ir();
    ir.literal_constraints =
        vec![LiteralSpan { text: "VX-7".to_string(), kind: LiteralKind::Identifier }];
    ir.retrieval_query = Some(question.to_string());

    assert_eq!(build_table_row_grounded_answer(question, None, &chunks), expected);
    assert_eq!(build_table_row_grounded_answer(question, Some(&ir), &chunks), expected);
}

#[test]
fn focused_table_row_does_not_match_a_structural_literal_prefix() {
    let document_id = Uuid::now_v7();
    let mut chunks = focused_lookup_chunks(document_id);
    chunks[0].source_text = chunks[0]
        .source_text
        .replace("insufficient permissions", "shared context")
        .replace("SR-A", "SR-B");
    chunks[1].source_text = chunks[1].source_text.replace("SR-B", "SR-A");
    let question = "What Description is listed for SR-A in the shared context?";
    let mut ir = row_lookup_ir();
    ir.literal_constraints =
        vec![LiteralSpan { text: "SR-A".to_string(), kind: LiteralKind::Identifier }];
    ir.retrieval_query = Some(question.to_string());

    assert_eq!(
        build_table_row_grounded_answer(question, None, &chunks),
        Some("Description: `shared context`".to_string()),
    );
    assert_eq!(build_table_row_grounded_answer(question, Some(&ir), &chunks), None);
}

#[test]
fn focused_table_row_keeps_prior_answer_when_structured_focus_is_empty() {
    let document_id = Uuid::now_v7();
    let chunks = focused_lookup_chunks(document_id);
    let ir = row_lookup_ir();

    assert_eq!(
        build_table_row_grounded_answer(
            "What Description is listed for Code 403?",
            Some(&ir),
            &chunks,
        ),
        Some("Code: `403`; Description: `insufficient permissions`".to_string()),
    );
}
