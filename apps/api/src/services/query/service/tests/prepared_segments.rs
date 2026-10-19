use super::*;

#[test]
fn build_prepared_segment_references_prioritizes_query_matching_headings_and_limits_revision_fanout()
 {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let telegram_revision_id = Uuid::now_v7();
    let control_revision_id = Uuid::now_v7();
    let bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: "hybrid".to_string(),
            requested_mode: "mix".to_string(),
            resolved_mode: "mix".to_string(),
            selected_fact_ids: Vec::new(),
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: json!({}),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    };
    let mut block_rank_refs = HashMap::new();
    let mut blocks = Vec::new();
    for ordinal in 0..12_i32 {
        let block_id = Uuid::now_v7();
        blocks.push(KnowledgeStructuredBlockRow {
            block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: telegram_revision_id,
            ordinal,
            block_kind: if ordinal == 0 { "heading".to_string() } else { "list_item".to_string() },
            text: "telegram".to_string(),
            normalized_text: "telegram".to_string(),
            heading_trail: vec!["Acme Telegram Bot - Example".to_string()],
            section_path: vec!["acme-telegram-bot-example".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        });
        block_rank_refs.insert(
            block_id,
            RankedBundleReference {
                rank: 1,
                score: 100.0 - f64::from(ordinal),
                reasons: BTreeSet::from(["test".to_string()]),
            },
        );
    }
    let control_heading_id = Uuid::now_v7();
    blocks.push(KnowledgeStructuredBlockRow {
        block_id: control_heading_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        document_id: Uuid::now_v7(),
        revision_id: control_revision_id,
        ordinal: 0,
        block_kind: "heading".to_string(),
        text: "sample console".to_string(),
        normalized_text: "sample console".to_string(),
        heading_trail: vec!["Acme Sample Console - Example".to_string()],
        section_path: vec!["acme-control-center-example".to_string()],
        page_number: None,
        span_start: None,
        span_end: None,
        parent_block_id: None,
        table_coordinates_json: None,
        code_language: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    });
    block_rank_refs.insert(
        control_heading_id,
        RankedBundleReference {
            rank: 2,
            score: 90.0,
            reasons: BTreeSet::from(["test".to_string()]),
        },
    );

    let references = build_prepared_segment_references(
        Some(&bundle),
        &blocks,
        &block_rank_refs,
        "What is Acme Sample Console?",
        None,
        &HashMap::new(),
    );

    assert_eq!(
        references.first().and_then(|reference| reference.heading_trail.first().cloned()),
        Some("Acme Sample Console - Example".to_string())
    );
    assert!(
        references.iter().all(|reference| reference.revision_id == control_revision_id),
        "focused query should retain only the best matching revision when focus is explicit"
    );
    assert!(references.len() <= MAX_DETAIL_PREPARED_SEGMENT_REFERENCES);
    assert!(
        references.iter().filter(|reference| reference.revision_id == telegram_revision_id).count()
            <= super::MAX_DETAIL_PREPARED_SEGMENT_REFERENCES_PER_REVISION
    );
}

#[test]
fn build_prepared_segment_references_prefers_rare_query_terms_over_generic_rank() {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let generic_revision_id = Uuid::now_v7();
    let focused_revision_id = Uuid::now_v7();
    let bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: "hybrid".to_string(),
            requested_mode: "mix".to_string(),
            resolved_mode: "mix".to_string(),
            selected_fact_ids: Vec::new(),
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: json!({}),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    };

    let generic_block_id = Uuid::now_v7();
    let focused_block_id = Uuid::now_v7();
    let blocks = vec![
        KnowledgeStructuredBlockRow {
            block_id: generic_block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: generic_revision_id,
            ordinal: 0,
            block_kind: "heading".to_string(),
            text: "Workspace templates and general setup".to_string(),
            normalized_text: "workspace templates and general setup".to_string(),
            heading_trail: vec!["Workspace templates".to_string()],
            section_path: vec!["workspace_templates".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        KnowledgeStructuredBlockRow {
            block_id: focused_block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: focused_revision_id,
            ordinal: 0,
            block_kind: "heading".to_string(),
            text: "Workspace plan options: Free, Standard, Advanced".to_string(),
            normalized_text: "workspace plan options free standard advanced".to_string(),
            heading_trail: vec!["Pricing policy: plan options".to_string()],
            section_path: vec!["pricing_policy_plan_options".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
    ];
    let mut block_rank_refs = HashMap::new();
    block_rank_refs.insert(
        generic_block_id,
        RankedBundleReference {
            rank: 1,
            score: 2_000_000.0,
            reasons: BTreeSet::from(["source_coverage".to_string()]),
        },
    );
    block_rank_refs.insert(
        focused_block_id,
        RankedBundleReference {
            rank: 9,
            score: 10.0,
            reasons: BTreeSet::from(["content_anchor".to_string()]),
        },
    );

    let references = build_prepared_segment_references(
        Some(&bundle),
        &blocks,
        &block_rank_refs,
        "workspace plans",
        None,
        &HashMap::new(),
    );

    assert_eq!(
        references.first().and_then(|reference| reference.heading_trail.first().cloned()),
        Some("Pricing policy: plan options".to_string()),
        "rare query-term overlap should beat a higher-ranked generic companion segment"
    );
}

#[test]
fn build_prepared_segment_references_prefers_answer_supported_blocks_over_generic_titles() {
    let bundle_id = Uuid::now_v7();
    let execution_id = Uuid::now_v7();
    let generic_revision_id = Uuid::now_v7();
    let focused_revision_id = Uuid::now_v7();
    let bundle = KnowledgeContextBundleReferenceSetRow {
        bundle: KnowledgeContextBundleRow {
            bundle_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            query_execution_id: Some(execution_id),
            bundle_state: "ready".to_string(),
            bundle_strategy: "hybrid".to_string(),
            requested_mode: "mix".to_string(),
            resolved_mode: "mix".to_string(),
            selected_fact_ids: Vec::new(),
            verification_state: "not_run".to_string(),
            verification_warnings: json!([]),
            freshness_snapshot: json!({}),
            candidate_summary: json!({}),
            assembly_diagnostics: json!({}),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        chunk_references: Vec::new(),
        entity_references: Vec::new(),
        relation_references: Vec::new(),
        evidence_references: Vec::new(),
    };

    let generic_block_id = Uuid::now_v7();
    let focused_block_id = Uuid::now_v7();
    let blocks = vec![
        KnowledgeStructuredBlockRow {
            block_id: generic_block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: generic_revision_id,
            ordinal: 0,
            block_kind: "heading".to_string(),
            text: "Workspace plans overview".to_string(),
            normalized_text: "workspace plans overview".to_string(),
            heading_trail: vec!["Workspace plans overview".to_string()],
            section_path: vec!["workspace_plans_overview".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
        KnowledgeStructuredBlockRow {
            block_id: focused_block_id,
            workspace_id: Uuid::now_v7(),
            library_id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            revision_id: focused_revision_id,
            ordinal: 0,
            block_kind: "paragraph".to_string(),
            text: "Available options: Free, Standard, Advanced.".to_string(),
            normalized_text: "available options free standard advanced".to_string(),
            heading_trail: vec!["Pricing policy".to_string()],
            section_path: vec!["pricing_policy".to_string()],
            page_number: None,
            span_start: None,
            span_end: None,
            parent_block_id: None,
            table_coordinates_json: None,
            code_language: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        },
    ];
    let mut block_rank_refs = HashMap::new();
    block_rank_refs.insert(
        generic_block_id,
        RankedBundleReference {
            rank: 1,
            score: 2_000_000.0,
            reasons: BTreeSet::from(["source_coverage".to_string()]),
        },
    );
    block_rank_refs.insert(
        focused_block_id,
        RankedBundleReference {
            rank: 20,
            score: 1.0,
            reasons: BTreeSet::from(["content_anchor".to_string()]),
        },
    );

    let references = build_prepared_segment_references(
        Some(&bundle),
        &blocks,
        &block_rank_refs,
        "workspace plans",
        Some("Available options: Free, Standard, Advanced."),
        &HashMap::new(),
    );

    assert_eq!(
        references.first().and_then(|reference| reference.heading_trail.first().cloned()),
        Some("Pricing policy".to_string()),
        "answer-supported source blocks should outrank generic high-rank title matches"
    );
}
