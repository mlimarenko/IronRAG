use super::*;

fn chunk_hit(text: &str) -> KnowledgeChunkSearchRow {
    KnowledgeChunkSearchRow {
        chunk_id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        revision_id: Uuid::nil(),
        content_text: text.to_string(),
        normalized_text: text.to_string(),
        section_path: Vec::new(),
        heading_trail: Vec::new(),
        score: 1.0,
        exact_identifier_rarity: 0.0,
        exact_identifier_terms: Vec::new(),
        rare_exact_identifier_terms: Vec::new(),
        quality_score: None,
    }
}

#[test]
fn search_document_summary_serializes_camel_case_without_workspace_id() {
    let row = document_row("orion-status.md");
    let summary = KnowledgeSearchDocumentSummary::from(row);
    let value = serde_json::to_value(&summary).expect("summary must serialize");
    let object = value.as_object().expect("summary must serialize as an object");

    assert!(object.contains_key("documentId"));
    assert!(object.contains_key("libraryId"));
    assert!(!object.contains_key("document_id"));
    assert!(!object.contains_key("workspace_id"));
    assert!(!object.contains_key("workspaceId"));
}

#[test]
fn search_chunk_hit_serializes_camel_case_without_workspace_id() {
    let hit = KnowledgeSearchChunkHit::from(chunk_hit("orion status endpoint"));
    let value = serde_json::to_value(&hit).expect("chunk hit must serialize");
    let object = value.as_object().expect("chunk hit must serialize as an object");

    assert!(object.contains_key("chunkId"));
    assert!(!object.contains_key("chunk_id"));
    assert!(!object.contains_key("workspace_id"));
    assert!(!object.contains_key("workspaceId"));
}

#[test]
fn search_entity_hit_serializes_camel_case_without_workspace_id() {
    let row = KnowledgeEntitySearchRow {
        entity_id: Uuid::nil(),
        workspace_id: Uuid::nil(),
        library_id: Uuid::nil(),
        primary_label: "Orion Signal".to_string(),
        entity_type: "system".to_string(),
        summary: None,
        score: 1.0,
    };
    let hit = KnowledgeSearchEntityHit::from(row);
    let value = serde_json::to_value(&hit).expect("entity hit must serialize");
    let object = value.as_object().expect("entity hit must serialize as an object");

    assert!(object.contains_key("entityId"));
    assert!(object.contains_key("\u{63}anonicalLabel"));
    assert!(!object.contains_key("entity_id"));
    assert!(!object.contains_key("\u{63}anonical_label"));
    assert!(!object.contains_key("workspace_id"));
    assert!(!object.contains_key("workspaceId"));
}

#[test]
fn document_keyword_coverage_bonus_rewards_concentrated_evidence() {
    let keywords = document_search_keywords("Alpha matrix beta gamma delta epsilon zeta");
    let weak = chunk_hit("Alpha matrix mentions beta once.");
    let strong = chunk_hit("Alpha matrix beta, gamma, delta, epsilon, and zeta.");

    let weak_bonus = document_keyword_coverage_bonus(&[weak], &keywords);
    let strong_bonus = document_keyword_coverage_bonus(&[strong], &keywords);

    assert!(strong_bonus > weak_bonus + 0.2);
}

#[test]
fn document_identity_bonus_rewards_specific_title_tokens() {
    let specific = document_row("alpha_matrix_delta_index.yaml");
    let generic = document_row("alpha_matrix.yaml");

    let query = "Alpha Matrix delta index";
    assert!(document_identity_bonus(&specific, query) > document_identity_bonus(&generic, query));
}

#[test]
fn document_identity_bonus_rewards_structured_identity_match() {
    let exact = document_row("segment-alpha-control-plane.md");
    let generic = document_row("segment-alpha-overview.md");

    let query = "segment alpha control plane calibration";

    assert!(document_identity_bonus(&exact, query) > document_identity_bonus(&generic, query));
}

#[test]
fn document_identity_bonus_rewards_multi_token_source_phrase() {
    let source = document_row("north-region-review-cycle.txt");
    let generic = document_row("north-region-review-notes.txt");

    let query = "north region review cycle threshold";

    assert!(document_identity_bonus(&source, query) > document_identity_bonus(&generic, query));
}

#[test]
fn document_identity_bonus_rewards_source_phrase_with_separators() {
    let source = document_row("alpha_data_sequence.md");
    let generic = document_row("alpha_reference_notes.md");

    let query = "alpha data sequence retry policy exponential backoff max attempts delay";

    assert!(document_identity_bonus(&source, query) > document_identity_bonus(&generic, query));
}

#[test]
fn document_best_chunk_focus_bonus_rewards_concentrated_evidence() {
    let keywords = document_search_keywords("alpha beta gamma delta epsilon");
    let diffuse = vec![
        chunk_hit("alpha beta notes"),
        chunk_hit("gamma delta notes"),
        chunk_hit("epsilon notes"),
    ];
    let focused = vec![chunk_hit("alpha beta gamma delta epsilon")];

    let query = "alpha beta gamma delta epsilon";

    assert!(
        document_best_chunk_focus_bonus(&focused, &keywords, query)
            > document_best_chunk_focus_bonus(&diffuse, &keywords, query)
    );
}

#[test]
fn document_best_chunk_evidence_coverage_rewards_single_focused_chunk() {
    let keywords = document_search_keywords("alpha beta gamma delta epsilon");
    let diffuse = vec![
        chunk_hit("alpha beta notes"),
        chunk_hit("gamma delta notes"),
        chunk_hit("epsilon notes"),
    ];
    let focused = vec![chunk_hit("alpha beta gamma delta epsilon")];
    let query = "alpha beta gamma delta epsilon";

    assert!(
        document_best_chunk_evidence_coverage(&focused, &keywords, query)
            > document_best_chunk_evidence_coverage(&diffuse, &keywords, query) + 0.3
    );
}

#[test]
fn document_lexical_signal_dampens_soft_title_without_evidence_coverage() {
    let weak = document_lexical_signal(Some(50.0), 0.0, 0.2);
    let strong = document_lexical_signal(Some(50.0), 0.0, 0.9);
    let identity = document_lexical_signal(Some(50.0), 1.25, 0.2);
    let exact_identity = document_lexical_signal(Some(1_000_000.0), 0.0, 0.0);

    assert!(strong > weak + 2.0);
    assert_eq!(identity, 50.0_f64.ln_1p());
    assert_eq!(exact_identity, 1_000_000.0_f64.ln_1p());
}

#[test]
fn document_vector_raw_score_bonus_preserves_similarity_margin() {
    assert!(
        document_vector_raw_score_bonus(Some(0.91)) > document_vector_raw_score_bonus(Some(0.73))
    );
    assert_eq!(document_vector_raw_score_bonus(Some(-0.5)), 0.0);
    assert_eq!(document_vector_raw_score_bonus(Some(2.0)), DOCUMENT_VECTOR_RAW_SCORE_WEIGHT);
}

#[test]
fn document_anchor_match_bonus_rewards_exact_config_keys() {
    let generic =
        chunk_hit("Configuration values include OTHER_PORT, OTHER_DATABASE_URL, and OTHER_SECRET.");
    let exact =
        chunk_hit("Configuration values include APP_PORT, APP_DATABASE_URL, and APP_SECRET.");

    let query = "Which settings configure APP_PORT APP_DATABASE_URL APP_SECRET?";

    assert!(
        document_anchor_match_bonus(&[exact], query)
            > document_anchor_match_bonus(&[generic], query) + 1.0
    );
}

#[test]
fn document_anchor_match_bonus_treats_structural_literals_as_strong_anchors() {
    let generic = chunk_hit("Trace limit window controls the request budget.");
    let exact = chunk_hit("TRACE_LIMIT_WINDOW controls the request budget.");

    let query = "trace limit TRACE_LIMIT_WINDOW";

    assert!(
        document_anchor_match_bonus(&[exact], query)
            > document_anchor_match_bonus(&[generic], query) + 20.0
    );
}

#[test]
fn document_anchor_match_bonus_rewards_camel_case_identifiers() {
    let generic = chunk_hit("The preprocessor lowercases text and normalizes addresses.");
    let exact = chunk_hit("TextPreprocessor.lowercase, TextPreprocessor.normalize_address");

    let query = "What steps does TextPreprocessor apply?";

    assert!(
        document_anchor_match_bonus(&[exact], query)
            > document_anchor_match_bonus(&[generic], query)
    );
}

#[test]
fn document_anchor_match_bonus_keeps_plain_camelcase_weaker_than_structural_literals() {
    assert!(
        document_anchor_surface_bonus_weight("retry_limit")
            > document_anchor_surface_bonus_weight("configmaps")
    );
    assert_eq!(
        document_anchor_surface_bonus_weight("base64"),
        document_anchor_surface_bonus_weight("configmaps")
    );
    let proper_name = chunk_hit("ConfigMaps are mentioned as a related object.");
    let substantive =
        chunk_hit("Sensitive data should use encryption and base64 encoding where required.");

    let query = "ConfigMaps sensitive data base64 encryption";

    assert!(
        document_anchor_match_bonus(&[substantive], query)
            > document_anchor_match_bonus(&[proper_name], query),
    );
}

#[test]
fn document_anchor_match_bonus_scales_partial_multi_surface_matches() {
    let partial = chunk_hit("The RETRY_LIMIT_REQUESTS setting is available.");
    let complete = chunk_hit(
        "The RETRY_LIMIT_REQUESTS and RETRY_LIMIT_WINDOW_SECONDS settings are available.",
    );

    let query = "RETRY_LIMIT_REQUESTS RETRY_LIMIT_WINDOW_SECONDS";

    assert!(
        document_anchor_match_bonus(&[complete], query)
            > document_anchor_match_bonus(&[partial], query) + 10.0
    );
}

#[test]
fn document_anchor_match_bonus_requires_multi_surface_coverage() {
    let partial = chunk_hit("The ALPHA_LIMIT setting is available.");
    let complete = chunk_hit("The ALPHA_LIMIT and BETA_TIMEOUT settings are available.");
    let query = "ALPHA_LIMIT BETA_TIMEOUT";

    assert!(document_anchor_match_bonus(&[partial], query) < 5.0);
    assert!(document_anchor_match_bonus(&[complete], query) > 40.0);
}

#[derive(Clone, Copy)]
struct IdentifierRankFixture {
    name: &'static str,
    score: f64,
    rarity: f64,
    identifier: Option<&'static str>,
    candidate_chunk: &'static str,
}

fn rank_identifier_fixtures(fixtures: &mut [IdentifierRankFixture]) {
    for fixture in fixtures.iter_mut() {
        fixture.score += document_exact_identifier_relevance_bonus(fixture.score, fixture.rarity);
    }
    fixtures.sort_by(|left, right| right.score.total_cmp(&left.score));
    promote_rare_identifier_declarations(fixtures, |fixture| {
        identifier_rank_fixture_signals(*fixture)
    });
}

fn identifier_rank_fixture_signals(
    fixture: IdentifierRankFixture,
) -> DocumentIdentifierRankSignals {
    let rare_identifier_terms =
        fixture.identifier.into_iter().map(str::to_string).collect::<Vec<_>>();
    let chunk_hits = [chunk_hit(fixture.candidate_chunk)];
    let declares_query_identifier =
        chunk_hits_declare_identifier(&rare_identifier_terms, &chunk_hits);
    let declares_rare_exact_identifier = chunk_hits_declare_rare_exact_identifier(
        fixture.rarity,
        &rare_identifier_terms,
        &chunk_hits,
    );
    DocumentIdentifierRankSignals {
        score: fixture.score,
        exact_identifier_rarity: fixture.rarity,
        declares_query_identifier,
        declares_rare_exact_identifier,
    }
}

#[test]
fn declaration_gated_identifier_reorders_the_recorded_wide_gap() {
    let rarity = exact_identifier_rarity(99, 1);
    let mut fixtures = [
        IdentifierRankFixture {
            name: "topic_overview.md",
            score: 17.25,
            rarity: 0.0,
            identifier: None,
            candidate_chunk: "General failure-handling guidance.",
        },
        IdentifierRankFixture {
            name: "alpha_state.rs",
            score: 6.74,
            rarity,
            identifier: Some("AlphaFault"),
            candidate_chunk: "enum AlphaFault {\n    Missing,\n    Invalid,\n}",
        },
        IdentifierRankFixture {
            name: "beta_pipeline.py",
            score: 5.70,
            rarity,
            identifier: Some("BetaFault"),
            candidate_chunk: "class BetaFault(Exception):\n    pass",
        },
    ];

    rank_identifier_fixtures(&mut fixtures);

    assert_eq!(
        fixtures.iter().map(|fixture| fixture.name).collect::<Vec<_>>(),
        ["alpha_state.rs", "beta_pipeline.py", "topic_overview.md"]
    );
}

#[test]
fn identifier_relevance_is_bounded_and_requires_strict_rarity() {
    assert_eq!(document_exact_identifier_relevance_bonus(10.0, 0.0), 0.0);
    assert_eq!(document_exact_identifier_relevance_bonus(10.0, 0.949), 0.0);
    assert!(
        document_exact_identifier_relevance_bonus(10.0, 0.95)
            < document_exact_identifier_relevance_bonus(10.0, 0.99)
    );
    assert_eq!(
        document_exact_identifier_relevance_bonus(10.0, 1.0),
        DOCUMENT_EXACT_IDENTIFIER_RELEVANCE_BONUS_MAX
    );
    assert_eq!(
        document_exact_identifier_relevance_bonus(2.0, 1.0),
        2.0 * DOCUMENT_EXACT_IDENTIFIER_RELEVANCE_MAX_FRACTION
    );
    assert_eq!(document_exact_identifier_relevance_bonus(0.0, 1.0), 0.0);
    assert_eq!(document_exact_identifier_relevance_bonus(f64::NAN, 1.0), 0.0);
}

#[test]
fn value_only_identifier_does_not_break_a_near_tie() {
    let rare = exact_identifier_rarity(100, 1);
    let mut fixtures = [
        IdentifierRankFixture {
            name: "topical",
            score: 10.0,
            rarity: 0.0,
            identifier: None,
            candidate_chunk: "Topical prose.",
        },
        IdentifierRankFixture {
            name: "value_match",
            score: 9.99,
            rarity: rare,
            identifier: Some("control-api"),
            candidate_chunk: "component: control-api\n  endpoint: local",
        },
    ];
    promote_rare_identifier_declarations(&mut fixtures, |fixture| {
        identifier_rank_fixture_signals(*fixture)
    });
    assert_eq!(fixtures[0].name, "topical");
}

#[test]
fn leader_declaration_blocks_a_rare_declaration_challenger() {
    let rare = exact_identifier_rarity(100, 1);
    let mut fixtures = [
        IdentifierRankFixture {
            name: "winner",
            score: 10.0,
            rarity: 0.0,
            identifier: Some("AlphaFault"),
            candidate_chunk: "enum AlphaFault {\n    Missing,\n}",
        },
        IdentifierRankFixture {
            name: "challenger",
            score: 9.9,
            rarity: rare,
            identifier: Some("BetaFault"),
            candidate_chunk: "enum BetaFault {\n    Missing,\n}",
        },
    ];

    promote_rare_identifier_declarations(&mut fixtures, |fixture| {
        identifier_rank_fixture_signals(*fixture)
    });

    assert_eq!(fixtures[0].name, "winner");
}

fn assert_recorded_winner_stays_put(
    winner: IdentifierRankFixture,
    challenger: IdentifierRankFixture,
) {
    let expected = winner.name;
    let mut fixtures = [winner, challenger];
    rank_identifier_fixtures(&mut fixtures);
    assert_eq!(fixtures[0].name, expected);
}

#[test]
fn first_recorded_value_only_ordering_stays_put() {
    let rare = exact_identifier_rarity(99, 2);
    assert_recorded_winner_stays_put(
        IdentifierRankFixture {
            name: "focused_guide.md",
            score: 6.10,
            rarity: rare,
            identifier: Some("storage-api"),
            candidate_chunk: "The storage-api value is discussed here.",
        },
        IdentifierRankFixture {
            name: "configuration.md",
            score: 3.40,
            rarity: rare,
            identifier: Some("storage-api"),
            candidate_chunk: "component: storage-api\n  endpoint: local",
        },
    );
}

#[test]
fn prose_overview_stays_ahead_of_value_only_component_matches() {
    let rare = exact_identifier_rarity(99, 1);
    assert_recorded_winner_stays_put(
        IdentifierRankFixture {
            name: "platform_overview.md",
            score: 6.071_515_364_261_858,
            rarity: 0.0,
            identifier: None,
            candidate_chunk: "The platform overview describes each control-plane role.",
        },
        IdentifierRankFixture {
            name: "platform_configuration.md",
            score: 3.80,
            rarity: rare,
            identifier: Some("control-api"),
            candidate_chunk: "component: control-api\n  endpoint: local",
        },
    );
}

#[test]
fn second_recorded_value_only_ordering_stays_put() {
    let rare = exact_identifier_rarity(99, 2);
    assert_recorded_winner_stays_put(
        IdentifierRankFixture {
            name: "operations_dashboard.pdf",
            score: 17.25,
            rarity: rare,
            identifier: Some("AlphaPanel"),
            candidate_chunk: "AlphaPanel operational guidance.",
        },
        IdentifierRankFixture {
            name: "platform_architecture.pptx",
            score: 6.74,
            rarity: rare,
            identifier: Some("AlphaPanel"),
            candidate_chunk: "The AlphaPanel value appears in a table.",
        },
    );
}

#[test]
fn third_recorded_value_only_ordering_stays_put() {
    let rare = exact_identifier_rarity(99, 3);
    assert_recorded_winner_stays_put(
        IdentifierRankFixture {
            name: "platform_architecture.pptx",
            score: 17.25,
            rarity: rare,
            identifier: Some("AlphaRoute"),
            candidate_chunk: "AlphaRoute architecture notes.",
        },
        IdentifierRankFixture {
            name: "interface_guidelines.docx",
            score: 5.70,
            rarity: rare,
            identifier: Some("AlphaRoute"),
            candidate_chunk: "AlphaRoute design guidance.",
        },
    );
}

fn exact_identifier_rarity(document_count: usize, document_frequency: usize) -> f64 {
    1.0 - document_frequency as f64 / document_count as f64
}

#[test]
fn exact_anchor_surfaces_ignore_ordinary_title_case_words() {
    let anchors = document_search_exact_anchor_surfaces("Alpha Beta TextParser RETRY_LIMIT key-v2");

    assert!(!anchors.contains(&"alpha".to_string()));
    assert!(!anchors.contains(&"beta".to_string()));
    assert!(anchors.contains(&"textparser".to_string()));
    assert!(anchors.contains(&"retry_limit".to_string()));
    assert!(anchors.contains(&"key-v2".to_string()));
}

#[test]
fn evidence_sample_limit_preserves_explicit_zero() {
    assert_eq!(resolved_evidence_sample_limit(Some(0)), 0);
    assert_eq!(resolved_evidence_sample_limit(Some(2)), 2);
    assert_eq!(resolved_evidence_sample_limit(None), DEFAULT_EVIDENCE_SAMPLE_LIMIT);
}

#[test]
fn document_search_vector_limits_track_response_surface() {
    assert_eq!(document_search_vector_limits(3, 3), (9, 3));
    assert_eq!(document_search_vector_limits(1, 1), (2, 1));
}

#[test]
fn anchor_phrases_are_generated_from_structure_without_topic_aliases() {
    let queries = expand_document_search_queries("Alpha Matrix delta channel");

    assert_eq!(queries[0], "Alpha Matrix delta channel");
    assert!(queries.iter().any(|query| query == "alpha matrix"));
    assert!(queries.iter().any(|query| query == "matrix delta"));
}

#[test]
fn focused_search_hit_text_centers_late_structural_anchor() {
    let text = format!(
        "alpha matrix summary {}; blocks.alpha.entry.reason_code enum beta gamma delta epsilon zeta status",
        (0..220).map(|index| format!("early_{index}=value")).collect::<Vec<_>>().join("; ")
    );
    let focus_terms = document_search_focus_terms(
        "alpha matrix reason_code beta gamma delta epsilon zeta status",
    );

    let excerpt = focused_search_hit_text(&text, &focus_terms, 220);

    assert!(excerpt.contains("reason_code"), "{excerpt}");
    assert!(excerpt.contains("beta"), "{excerpt}");
    assert!(!excerpt.contains("early_0=value"), "{excerpt}");
}

fn document_row(file_name: &str) -> KnowledgeDocumentRow {
    let document_id = Uuid::now_v7();
    KnowledgeDocumentRow {
        document_id,
        workspace_id: Uuid::now_v7(),
        library_id: Uuid::now_v7(),
        external_key: file_name.to_string(),
        file_name: Some(file_name.to_string()),
        title: Some(file_name.to_string()),
        source_uri: None,
        document_hint: None,
        document_state: "active".to_string(),
        active_revision_id: Some(Uuid::now_v7()),
        readable_revision_id: Some(Uuid::now_v7()),
        latest_revision_no: Some(1),
        parent_document_id: None,
        document_role: crate::domains::content::DOCUMENT_ROLE_PRIMARY.to_string(),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    }
}
