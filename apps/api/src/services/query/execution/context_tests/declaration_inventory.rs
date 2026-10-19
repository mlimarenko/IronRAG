use super::*;

#[test]
fn focused_declaration_context_preserves_the_complete_baseline_context() {
    let query_ir = general_ir();
    let baseline = assemble_bounded_context_for_query(BoundedContextQuery {
        query_ir: &query_ir,
        question: "Which LedgerBoard state is reported?",
        entities: &[],
        relationships: &[],
        chunks: &[ordinary_chunk("baseline evidence", "baseline evidence remains visible")],
        graph_evidence_lines: &[],
        budget_chars: 8192,
    });
    let mut declaration = ordinary_chunk(
        "LedgerBoard declaration",
        "function LedgerBoard() {\n  const state = loadEntries();\n}",
    );
    declaration.score_kind = RuntimeChunkScoreKind::SourceContext;

    assert_eq!(
        prepend_focused_declaration_context(
            &query_ir,
            "Which LedgerBoard state is reported?",
            baseline.clone(),
            &[],
        ),
        baseline
    );
    let augmented = prepend_focused_declaration_context(
        &query_ir,
        "Which LedgerBoard state is reported?",
        baseline.clone(),
        &[declaration],
    );

    assert!(augmented.contains("function LedgerBoard()"));
    assert!(
        augmented.ends_with(baseline.strip_prefix("Context\n").unwrap_or(baseline.as_str())),
        "{augmented}"
    );
}

#[test]
fn document_scoped_enumeration_adds_a_bounded_declaration_inventory_in_document_order() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = general_ir();
    query_ir.document_focus =
        Some(crate::domains::query_ir::DocumentHint { hint: "sample component".to_string() });
    let mut baseline_chunk = ordinary_chunk("baseline evidence", "baseline evidence");
    baseline_chunk.document_id = document_id;
    baseline_chunk.revision_id = revision_id;
    baseline_chunk.document_label = "sample_component.tsx".to_string();
    let mut later_declarations = ordinary_chunk(
        "later declarations",
        "function useTheme(): ThemeState {\n  const state = readTheme();\n  return state;\n}\n\nfunction useAuth(): AuthState {\n  const state = readAuth();\n  return state;\n}",
    );
    later_declarations.document_id = document_id;
    later_declarations.revision_id = revision_id;
    later_declarations.document_label = "sample_component.tsx".to_string();
    later_declarations.chunk_index = 42;
    let mut unrelated_declaration = ordinary_chunk(
        "unrelated declaration",
        "function mustNotLeakFromAnotherDocument() {\n  return true;\n}",
    );
    unrelated_declaration.document_id = Uuid::now_v7();
    unrelated_declaration.document_label = "unrelated.ts".to_string();
    let inventory_candidates = vec![unrelated_declaration, later_declarations];
    let budget_chars = 420;

    let context = assemble_bounded_context_for_query_with_inventory_candidates(
        BoundedContextInventoryQuery {
            query_ir: &query_ir,
            question: "Which declarations are used by the component?",
            entities: &[],
            relationships: &[],
            chunks: std::slice::from_ref(&baseline_chunk),
            graph_evidence_lines: &[],
            inventory_candidates: &inventory_candidates,
            focused_document_id: Some(document_id),
            fallback_declaration_chunks: &[],
            budget_chars,
        },
    );

    assert!(context.contains("declaration_inventory"), "{context}");
    assert!(
        context.find("function useTheme").unwrap() < context.find("function useAuth").unwrap(),
        "{context}"
    );
    assert!(!context.contains("readTheme"), "{context}");
    assert!(!context.contains("readAuth"), "{context}");
    assert!(!context.contains("mustNotLeakFromAnotherDocument"), "{context}");
    assert!(context.len() <= budget_chars, "{} > {budget_chars}: {context}", context.len());
}

#[test]
fn declaration_inventory_exposes_custom_declarations_unnamed_by_the_question() {
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut query_ir = general_ir();
    query_ir.document_focus =
        Some(crate::domains::query_ir::DocumentHint { hint: "react_dashboard.txt".to_string() });
    let mut candidate = ordinary_chunk(
        "React hooks useState useCallback useEffect useReducer dashboard state",
        include_str!("../../../../../benchmarks/grounded_query/corpus/code/react_dashboard.txt"),
    );
    candidate.document_id = document_id;
    candidate.revision_id = revision_id;
    candidate.document_label = "react_dashboard.txt".to_string();
    let budget_chars = DECLARATION_INVENTORY_CONTEXT_CHARS;

    let context = assemble_bounded_context_for_query_with_inventory_candidates(
        BoundedContextInventoryQuery {
            query_ir: &query_ir,
            question: "What hooks are used in the dashboard component and what state do they manage?",
            entities: &[],
            relationships: &[],
            chunks: &[],
            graph_evidence_lines: &[],
            inventory_candidates: std::slice::from_ref(&candidate),
            focused_document_id: Some(document_id),
            fallback_declaration_chunks: &[],
            budget_chars,
        },
    );

    assert!(context.contains("function useAuth(): AuthContextValue"), "{context}");
    assert!(context.contains("function useTheme(): ThemeContextValue"), "{context}");
    assert!(context.contains("function useDashboardData()"), "{context}");
    assert!(context.contains("function Dashboard()"), "{context}");
    assert!(context.len() <= budget_chars, "{} > {budget_chars}", context.len());
}

#[test]
fn declaration_inventory_election_follows_rank_before_candidate_count() {
    let first_ranked_document_id = Uuid::now_v7();
    let runner_up_document_id = Uuid::now_v7();
    let mut first_ranked =
        ordinary_chunk("primary.rs declaration", "enum PrimaryState {\n    Ready,\n}");
    first_ranked.document_id = first_ranked_document_id;
    let runner_up_chunks = (0..3)
        .map(|index| {
            let mut chunk = ordinary_chunk(
                &format!("secondary.rs declaration {index}"),
                "enum SecondaryState {\n    Waiting,\n}",
            );
            chunk.document_id = runner_up_document_id;
            chunk
        })
        .collect::<Vec<_>>();
    let candidates = std::iter::once(first_ranked).chain(runner_up_chunks).collect::<Vec<_>>();

    assert_eq!(
        declaration_inventory_document_id(&general_ir(), &candidates, None),
        Some(first_ranked_document_id),
        "the first-ranked candidate document must win even when a runner-up contributes more chunks"
    );
}

#[test]
fn declaration_inventory_enforces_its_entry_and_character_caps() {
    let document_id = Uuid::now_v7();
    let short_declarations = (0..DECLARATION_INVENTORY_ENTRY_LIMIT + 12)
        .map(|index| format!("function item_{index}() {{\n  return {index};\n}}\n"))
        .collect::<String>();
    let mut candidate = ordinary_chunk("declarations", &short_declarations);
    candidate.document_id = document_id;
    candidate.document_label = "bounded.ts".to_string();
    let block =
        declaration_inventory_context_block(std::slice::from_ref(&candidate), document_id, 22_000)
            .expect("bounded declaration inventory");

    assert_eq!(
        block.lines().filter(|line| line.starts_with("- ")).count(),
        DECLARATION_INVENTORY_ENTRY_LIMIT
    );

    let long_declarations = (0..DECLARATION_INVENTORY_ENTRY_LIMIT)
        .map(|index| {
            format!(
                "function item_{index}_{}() {{\n  return {index};\n}}\n",
                "long_signature_segment_".repeat(5)
            )
        })
        .collect::<String>();
    candidate.source_text = long_declarations;
    let block =
        declaration_inventory_context_block(std::slice::from_ref(&candidate), document_id, 22_000)
            .expect("character-bounded declaration inventory");

    assert!(
        block.len() <= DECLARATION_INVENTORY_CONTEXT_CHARS,
        "{} > {}",
        block.len(),
        DECLARATION_INVENTORY_CONTEXT_CHARS
    );
}

#[test]
fn non_enumerative_or_multi_document_inventory_keeps_exactly_the_existing_context() {
    let first_document_id = Uuid::now_v7();
    let second_document_id = Uuid::now_v7();
    let mut baseline_chunk = ordinary_chunk("baseline evidence", "baseline evidence");
    baseline_chunk.document_id = first_document_id;
    let mut declaration =
        ordinary_chunk("declaration", "function hiddenDeclaration() {\n  return true;\n}");
    declaration.document_id = first_document_id;
    let mut other_document = declaration.clone();
    other_document.document_id = second_document_id;
    other_document.document_label = "other.ts".to_string();
    let fallback_declaration = declaration.clone();
    let inventory_candidates = vec![declaration, other_document];
    let question = "Describe the selected component.";
    let budget_chars = 512;
    let mut non_enumerative = general_ir();
    // Describe now triggers the inventory alongside Enumerate, so the
    // non-triggering arm uses a procedural act instead.
    non_enumerative.act = QueryAct::ConfigureHow;
    non_enumerative.document_focus =
        Some(crate::domains::query_ir::DocumentHint { hint: "guide.md".to_string() });
    let baseline = prepend_focused_declaration_context(
        &non_enumerative,
        question,
        assemble_bounded_context_for_query(BoundedContextQuery {
            query_ir: &non_enumerative,
            question,
            entities: &[],
            relationships: &[],
            chunks: std::slice::from_ref(&baseline_chunk),
            graph_evidence_lines: &[],
            budget_chars,
        }),
        std::slice::from_ref(&fallback_declaration),
    );

    assert_eq!(
        assemble_bounded_context_for_query_with_inventory_candidates(
            BoundedContextInventoryQuery {
                query_ir: &non_enumerative,
                question,
                entities: &[],
                relationships: &[],
                chunks: std::slice::from_ref(&baseline_chunk),
                graph_evidence_lines: &[],
                inventory_candidates: &inventory_candidates,
                focused_document_id: Some(first_document_id),
                fallback_declaration_chunks: std::slice::from_ref(&fallback_declaration),
                budget_chars,
            }
        ),
        baseline
    );

    let mut multi_document = general_ir();
    multi_document.scope = QueryScope::MultiDocument;
    let baseline = prepend_focused_declaration_context(
        &multi_document,
        question,
        assemble_bounded_context_for_query(BoundedContextQuery {
            query_ir: &multi_document,
            question,
            entities: &[],
            relationships: &[],
            chunks: std::slice::from_ref(&baseline_chunk),
            graph_evidence_lines: &[],
            budget_chars,
        }),
        std::slice::from_ref(&fallback_declaration),
    );
    assert_eq!(
        assemble_bounded_context_for_query_with_inventory_candidates(
            BoundedContextInventoryQuery {
                query_ir: &multi_document,
                question,
                entities: &[],
                relationships: &[],
                chunks: std::slice::from_ref(&baseline_chunk),
                graph_evidence_lines: &[],
                inventory_candidates: &inventory_candidates,
                focused_document_id: None,
                fallback_declaration_chunks: std::slice::from_ref(&fallback_declaration),
                budget_chars,
            }
        ),
        baseline
    );
}
