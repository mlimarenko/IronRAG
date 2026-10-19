use super::test_prelude::*;

#[test]
fn config_queries_reserve_source_context_chunks_during_truncation() {
    let document_id = Uuid::now_v7();
    let mut query_ir = setup_query_ir("Subject Alpha");
    query_ir.target_types = vec![QueryTargetKind::ConfigurationFile, QueryTargetKind::Parameter];

    let mut bundle = setup_manual_source_context_bundle(
        document_id,
        "[Main] url = https://alpha.example.invalid/api",
        "printSlip = true",
    );

    assert_source_context_retained(
        &mut bundle,
        8,
        &query_ir,
        &["fillDetails", "visible = true", "credentialToken"],
    );
}

#[test]
fn table_inventory_queries_reserve_source_context_chunks_during_truncation() {
    let document_id = Uuid::now_v7();
    let mut query_ir = setup_query_ir("Accounts schema");
    query_ir.act = QueryAct::Enumerate;
    query_ir.document_focus = None;
    query_ir.target_types = vec![QueryTargetKind::TableRow, QueryTargetKind::TableSummary];
    query_ir.target_entities =
        vec![EntityMention { label: "accounts".to_string(), role: EntityRole::Subject }];

    let mut bundle = source_context_bundle(
        document_id,
        8,
        100.0,
        1.0,
        "Accounts schema",
        &[
            "| account_id | UUID | PRIMARY KEY | Unique account identifier |",
            "| email | VARCHAR(255) | UNIQUE, NOT NULL | Login email |",
            "| status | VARCHAR(20) | NOT NULL | Account state |",
            "| created_at | TIMESTAMPTZ | NOT NULL | Creation timestamp |",
            "| updated_at | TIMESTAMPTZ | NOT NULL | Last update timestamp |",
            "| archived_at | TIMESTAMPTZ | nullable | Archive timestamp |",
        ],
    );

    assert_source_context_retained(&mut bundle, 10, &query_ir, &["account_id", "updated_at"]);
}

#[test]
fn low_confidence_structured_fallback_reserves_source_context_chunks() {
    let document_id = Uuid::now_v7();
    let query_ir = low_confidence_untyped_query_ir();

    let mut bundle = setup_manual_source_context_bundle(
        document_id,
        "[Main] endpointUrl = https://alpha.example.invalid/api",
        "printSlip = false",
    );

    assert_source_context_retained(
        &mut bundle,
        8,
        &query_ir,
        &["endpointUrl", "visible = true", "printSlip = false"],
    );
}

#[test]
fn low_confidence_structured_fallback_keeps_late_source_context_examples() {
    let document_id = Uuid::now_v7();
    let query_ir = low_confidence_untyped_query_ir();

    let mut chunks = (0..8)
        .map(|index| setup_focus_runtime_chunk(Uuid::now_v7(), index, 1.0))
        .collect::<Vec<_>>();
    chunks.extend((0..28).map(|offset| {
        let text = match offset {
            24 => "[Display] visible = true".to_string(),
            26 => "[Receipt] printSlip = false".to_string(),
            _ => format!("tableRow{offset} = value"),
        };
        let mut chunk = setup_focus_runtime_chunk(document_id, 20 + offset, 0.1);
        chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
        chunk.document_label = "Subject Alpha setup manual".to_string();
        chunk.excerpt = text.clone();
        chunk.source_text = text;
        chunk
    }));
    let mut bundle = RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks };

    truncate_bundle(&mut bundle, 32, Some(&query_ir), &std::collections::HashSet::new());

    let retained_source_text =
        bundle.chunks.iter().map(|chunk| chunk.source_text.as_str()).collect::<Vec<_>>();
    assert!(
        bundle
            .chunks
            .iter()
            .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
            .count()
            >= 28,
        "{retained_source_text:?}"
    );
    assert!(retained_source_text.iter().any(|text| text.contains("visible = true")));
    assert!(retained_source_text.iter().any(|text| text.contains("printSlip = false")));
}

fn source_context_bundle(
    document_id: Uuid,
    noise_count: i32,
    noise_score: f32,
    source_score: f32,
    document_label: &str,
    source_texts: &[&str],
) -> RetrievalBundle {
    let mut chunks = (0..noise_count)
        .map(|index| setup_focus_runtime_chunk(Uuid::now_v7(), index, noise_score))
        .collect::<Vec<_>>();
    chunks.extend(source_texts.iter().enumerate().map(|(offset, text)| {
        let mut chunk = setup_focus_runtime_chunk(document_id, 20 + offset as i32, source_score);
        chunk.score_kind = RuntimeChunkScoreKind::SourceContext;
        chunk.document_label = document_label.to_string();
        chunk.excerpt = (*text).to_string();
        chunk.source_text = (*text).to_string();
        chunk
    }));
    RetrievalBundle { entities: Vec::new(), relationships: Vec::new(), chunks }
}

fn setup_manual_source_context_bundle(
    document_id: Uuid,
    endpoint: &str,
    print_slip: &str,
) -> RetrievalBundle {
    source_context_bundle(
        document_id,
        6,
        1.0,
        0.1,
        "Subject Alpha setup manual",
        &[
            endpoint,
            "| fillDetails | boolean | true false | Fill detail fields |",
            "[Display] visible = true",
            "| visible | boolean | true false | Display code |",
            print_slip,
            "credentialToken = value",
        ],
    )
}

fn assert_source_context_retained(
    bundle: &mut RetrievalBundle,
    top_k: usize,
    query_ir: &QueryIR,
    expected_fragments: &[&str],
) {
    truncate_bundle(bundle, top_k, Some(query_ir), &std::collections::HashSet::new());

    let retained_source_text =
        bundle.chunks.iter().map(|chunk| chunk.source_text.as_str()).collect::<Vec<_>>();
    assert!(
        bundle
            .chunks
            .iter()
            .filter(|chunk| chunk.score_kind == RuntimeChunkScoreKind::SourceContext)
            .count()
            >= 6,
        "{retained_source_text:?}"
    );
    for expected_fragment in expected_fragments {
        assert!(retained_source_text.iter().any(|text| text.contains(expected_fragment)));
    }
}
