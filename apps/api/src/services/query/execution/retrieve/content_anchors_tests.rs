use super::test_prelude::*;

#[test]
fn sequential_record_detection_requires_semver_in_the_first_field() {
    assert!(ordered_step_looks_like_sequential_record("1. 2.4.0 | change summary"));
    assert!(ordered_step_looks_like_sequential_record("1. v2.4.0 | change summary"));
    assert!(!ordered_step_looks_like_sequential_record("1. 2..4 | change summary"));
    assert!(!ordered_step_looks_like_sequential_record("1. Label 2.4.0 | change summary"));
    assert!(!ordered_step_looks_like_sequential_record("1. Version 2.4.0 | change summary"));
}

#[test]
fn command_dense_excerpt_preserves_multiline_shell_procedure() {
    let excerpt = command_dense_excerpt_for(
        concat!(
            "Procedure\n",
            "1. runner-shell --scope=elevated\n",
            "2. sample-transfer https://example.invalid/alpha/runner.sh --output=/work/sample-runner.sh\n",
            "3. sample-prepare +x /work/sample-runner.sh\n",
            "4. /work/sample-runner.sh\n"
        ),
        4_000,
    )
    .expect("command-dense excerpt");

    assert!(excerpt.contains("runner-shell --scope=elevated\n"));
    assert!(excerpt.contains(
        "sample-transfer https://example.invalid/alpha/runner.sh --output=/work/sample-runner.sh\n"
    ));
    assert!(excerpt.contains("sample-prepare +x /work/sample-runner.sh\n"));
    assert!(excerpt.contains("/work/sample-runner.sh"));
}

pub(super) fn release_query_ir(count: Option<&str>, entity: Option<&str>) -> QueryIR {
    QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::MultiDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Version],
        target_entities: entity
            .map(|label| {
                vec![EntityMention { label: label.to_string(), role: EntityRole::Subject }]
            })
            .unwrap_or_default(),
        literal_constraints: count
            .map(|text| {
                vec![LiteralSpan { text: text.to_string(), kind: LiteralKind::NumericCode }]
            })
            .unwrap_or_default(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

pub(super) fn release_query_ir_with_source_slice_count(count: Option<u16>) -> QueryIR {
    release_query_ir_with_source_slice(SourceSliceDirection::Tail, count)
}

pub(super) fn release_query_ir_with_source_slice(
    direction: SourceSliceDirection,
    count: Option<u16>,
) -> QueryIR {
    let mut ir = release_query_ir(None, None);
    ir.source_slice =
        Some(SourceSliceSpec { direction, count, filter: SourceSliceFilter::ReleaseMarker });
    ir
}

pub(super) fn target_entities_query_ir(target_labels: &[&str]) -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::Event],
        target_entities: target_labels
            .iter()
            .map(|label| EntityMention { label: (*label).to_string(), role: EntityRole::Subject })
            .collect(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

pub(super) fn comparison_query_ir(target_labels: &[&str]) -> QueryIR {
    let mut query_ir = target_entities_query_ir(target_labels);
    query_ir.act = QueryAct::Compare;
    query_ir.scope = QueryScope::MultiDocument;
    query_ir.comparison = Some(ComparisonSpec {
        a: target_labels.first().map(|label| (*label).to_string()),
        b: target_labels.get(1).map(|label| (*label).to_string()),
        dimension: "failure behavior".to_string(),
    });
    query_ir
}

#[test]
fn content_anchor_model_builds_adjacent_sequences_for_unquoted_questions() {
    let mut ir = target_entities_query_ir(&[]);
    ir.retrieval_query = Some("which service plans are available".to_string());

    let model = RetrievalContentAnchorModel::new("which service plans are available?", Some(&ir));

    assert!(model.search_terms.iter().any(|term| term == "service"));
    assert!(model.search_terms.iter().any(|term| term == "plans"));
    assert!(
        model
            .phrase_sequences
            .iter()
            .any(|sequence| { sequence == &vec!["service".to_string(), "plans".to_string()] })
    );
}

#[test]
fn content_anchor_model_adds_prefix_search_terms_for_inflected_matches() {
    let model = RetrievalContentAnchorModel::new("which service documents are available?", None);

    assert!(model.search_terms.iter().any(|term| term == "documents"));
    assert!(
        model.search_terms.iter().any(|term| term == "docum"),
        "content-anchor candidate lookup should include a stable prefix term: {model:#?}"
    );
}

#[test]
fn content_anchor_row_score_prefers_body_section_over_navigation_overlap() {
    let document = sample_document_row("overview.md", "Product overview");
    let nav = knowledge_chunk_row(
        &document,
        0,
        Some("paragraph"),
        "Service overview image loading help link support available",
    );
    let body = knowledge_chunk_row(
        &document,
        1,
        Some("paragraph"),
        "Pricing policy: service plans. Free tier. Personal tier. Business tier.",
    );
    let model = RetrievalContentAnchorModel::new("which service plans are available?", None);

    let nav_score = content_anchor_row_score(&nav, &model);
    let body_score = content_anchor_row_score(&body, &model);

    assert!(body_score > nav_score);
    assert!(body_score > 0);
}

#[test]
fn content_anchor_row_score_preserves_order_for_inflected_action_target_phrases() {
    let document = sample_document_row("runbook.md", "Operations handbook");
    let ordered = knowledge_chunk_row(
        &document,
        0,
        Some("paragraph"),
        "Updating Sample Servers requires a backup and a release bundle.",
    );
    let reversed = knowledge_chunk_row(
        &document,
        1,
        Some("paragraph"),
        "Sample Servers have a maintenance note about updating integrations.",
    );
    let model = RetrievalContentAnchorModel::new("How do I update Sample Server?", None);

    let ordered_score = content_anchor_row_score(&ordered, &model);
    let reversed_score = content_anchor_row_score(&reversed, &model);

    assert!(ordered_score > reversed_score, "ordered={ordered_score}, reversed={reversed_score}");
    assert!(ordered_score >= 1024, "an ordered inflected phrase must earn a phrase hit");
}

#[test]
fn content_anchor_revision_ids_respect_target_scope() {
    let first = sample_document_row("first.md", "First");
    let second = sample_document_row("second.md", "Second");
    let expected_revision = current_document_revision_id(&second).expect("revision id");
    let mut document_index = HashMap::new();
    document_index.insert(first.document_id, first);
    document_index.insert(second.document_id, second.clone());
    let targeted_document_ids = BTreeSet::from([second.document_id]);

    let revision_ids = content_anchor_revision_ids(&document_index, &targeted_document_ids);

    assert_eq!(revision_ids, vec![expected_revision]);
}

pub(super) fn exact_error_code_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::ErrorCode],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

pub(super) fn transport_inventory_query_ir() -> QueryIR {
    QueryIR {
        act: QueryAct::RetrieveValue,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![
            crate::domains::query_ir::QueryTargetKind::Port,
            crate::domains::query_ir::QueryTargetKind::Protocol,
            crate::domains::query_ir::QueryTargetKind::Connection,
        ],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: None,
        retrieval_query: None,
        confidence: 1.0,
    }
}

#[test]
fn document_identity_focus_terms_include_structural_prefixes() {
    let mut query_ir = target_entities_query_ir(&["integrated connector", "connection variants"]);
    query_ir.document_focus = Some(DocumentHint { hint: "Sample Subject setup".to_string() });

    let terms = document_identity_focus_terms(&["setup".to_string()], Some(&query_ir));

    assert!(terms.contains(&"Sample Subject setup".to_string()));
    assert!(terms.contains(&"integrated connector".to_string()));
    assert!(terms.contains(&"integr".to_string()));
    assert!(terms.contains(&"connec".to_string()));
}

#[test]
fn linked_anchor_focus_queries_follow_relevant_source_links() {
    let source_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        document_label: "Sample Subject overview".to_string(),
        excerpt: "See [Integrated connector catalog](/connectors) for details.".to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text:
            "See [Integrated connector catalog](/connectors) for details. Also [Support](/support)."
                .to_string(),
    };
    let queries = linked_anchor_focus_queries(
        "Which integration connectors are available?",
        None,
        &[],
        &[source_chunk],
    );

    assert_eq!(
        queries,
        vec![
            "Integrated connector catalog".to_string(),
            "integra connector catalog".to_string(),
            "integrated connect catalog".to_string(),
        ]
    );
}

#[test]
fn linked_anchor_focus_queries_add_numeric_free_prefix_variants() {
    let source_chunk = RuntimeMatchedChunk {
        chunk_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: Uuid::now_v7(),
        chunk_index: 0,
        chunk_kind: Some("paragraph".to_string()),
        document_label: "Sample Subject overview".to_string(),
        excerpt: "See [15 integrated connectors](/connectors) for details.".to_string(),
        score_kind: RuntimeChunkScoreKind::Relevance,
        score: Some(1.0),
        source_text: "See [15 integrated connectors](/connectors) for details.".to_string(),
    };
    let queries = linked_anchor_focus_queries(
        "Which integration connectors are available?",
        None,
        &[],
        &[source_chunk],
    );

    assert!(queries.contains(&"15 integrated connectors".to_string()));
    assert!(queries.contains(&"integrated connectors".to_string()));
    assert!(queries.contains(&"integra connectors".to_string()));
}

#[test]
fn linked_anchor_hydration_is_cross_document() {
    assert!(
        linked_anchor_hydration_target_filter().is_empty(),
        "linked markdown anchors point at related documents, so hydration must not inherit the source document scope filter"
    );
}

#[test]
fn source_profile_rows_are_not_answer_driving_search_hits() {
    let document = sample_document_row("records.jsonl", "Records");
    let row = knowledge_chunk_row(
        &document,
        0,
        Some("source_profile"),
        "[source_profile source_format=record_jsonl unit_count=42]",
    );
    let document_index = HashMap::from([(document.document_id, document)]);

    assert!(!is_answer_driving_search_chunk_row(&row));
    assert!(
        map_chunk_hit(row, 1.0, &document_index, &[]).is_some(),
        "specialized source-context paths still map source profiles explicitly"
    );
}

#[test]
fn ordinary_content_rows_remain_answer_driving_search_hits() {
    let document = sample_document_row("records.jsonl", "Records");
    let row = knowledge_chunk_row(
        &document,
        1,
        Some("source_unit"),
        "[unit_id=alpha] service-a listens on port 8080",
    );

    assert!(is_answer_driving_search_chunk_row(&row));
}

#[test]
fn table_row_answer_context_uses_semantic_row_text() {
    let mut chunk = answer_source_chunk(1, "table_row", "| 1 |", "Sheet: test1 | Row 1 | col_1: 1");
    chunk.span_start = Some(0);
    chunk.span_end = Some(5);
    chunk.token_count = Some(4);
    chunk.section_path = vec!["test1".to_string()];
    chunk.heading_trail = vec!["test1".to_string()];

    assert_eq!(chunk_answer_source_text(&chunk), "Sheet: test1 | Row 1 | col_1: 1");
}

#[test]
fn metadata_summary_answer_context_uses_normalized_text_when_content_is_empty() {
    let mut chunk = answer_source_chunk(
        1,
        "metadata_block",
        "",
        "Table Summary | Sheet: products | Column: Stock | Value Kind: numeric | Value Shape: label | Aggregation Priority: 3 | Row Count: 3 | Non-empty Count: 3 | Distinct Count: 3 | Average: 20 | Min: 10 | Max: 30",
    );
    chunk.token_count = Some(16);
    chunk.section_path = vec!["products".to_string()];
    chunk.heading_trail = vec!["products".to_string()];

    assert!(chunk_answer_source_text(&chunk).starts_with("Table Summary |"));
}

#[test]
fn source_unit_answer_context_prefers_full_record_over_window_excerpt() {
    let text = "[unit_ordinal=0] fields: resources.alpha.events.type=array; resources.alpha.secret.description=HMAC-SHA256 signing secret";
    let mut chunk = answer_source_chunk(1, "source_unit", text, text);
    chunk.token_count = Some(16);
    chunk.section_path = vec!["resources".to_string()];
    chunk.heading_trail = vec!["resources".to_string()];
    chunk.window_text =
        Some("[unit_ordinal=0] fields: resources.alpha.events.type=array".to_string());

    let source_text = chunk_answer_source_text(&chunk);

    assert!(source_text.contains("events.type=array"));
    assert!(source_text.contains("HMAC-SHA256"));
}

#[test]
fn non_table_chunk_answer_context_preserves_raw_content_text() {
    let mut chunk = answer_source_chunk(0, "heading", "test1", "test1");
    chunk.span_start = Some(0);
    chunk.span_end = Some(5);
    chunk.token_count = Some(1);
    chunk.section_path = vec!["test1".to_string()];
    chunk.heading_trail = vec!["test1".to_string()];

    assert_eq!(chunk_answer_source_text(&chunk), "test1");
}

#[test]
fn document_target_candidates_include_extensionless_stem() {
    let document = sample_document_row("sample-heavy-1.xls", "sample-heavy-1.xls");

    let candidates = normalized_document_target_candidates(
        [
            document.file_name.as_deref(),
            document.title.as_deref(),
            Some(document.external_key.as_str()),
        ]
        .into_iter()
        .flatten(),
    );

    assert!(candidates.contains(&"sample-heavy-1.xls".to_string()));
    assert!(candidates.contains(&"sample-heavy-1".to_string()));
}

#[test]
fn requested_initial_table_row_count_uses_typed_source_slice() {
    let mut ir = QueryIR {
        act: QueryAct::Enumerate,
        scope: QueryScope::SingleDocument,
        language: QueryLanguage::Auto,
        target_types: vec![crate::domains::query_ir::QueryTargetKind::TableRow],
        target_entities: Vec::new(),
        literal_constraints: Vec::new(),
        temporal_constraints: Vec::new(),
        comparison: None,
        document_focus: None,
        conversation_refs: Vec::new(),
        needs_clarification: None,
        source_slice: Some(SourceSliceSpec {
            direction: SourceSliceDirection::Head,
            count: Some(7),
            filter: SourceSliceFilter::None,
        }),
        retrieval_query: None,
        confidence: 1.0,
    };

    assert_eq!(requested_initial_table_row_count(Some(&ir)), Some(7));

    ir.target_types = vec![crate::domains::query_ir::QueryTargetKind::Document];
    assert_eq!(requested_initial_table_row_count(Some(&ir)), None);
}
