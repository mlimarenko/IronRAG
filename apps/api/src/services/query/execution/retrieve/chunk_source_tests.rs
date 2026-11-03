use super::test_prelude::*;

#[test]
fn chunk_answer_source_text_preserves_prose_conditions_in_containing_window() {
    let core = "Повторный запрос допустим.";
    let condition = "После подтверждения записи повтор запрещён.";
    let window = format!("{core} {condition}");
    let mut row = answer_source_chunk(0, "paragraph", core, core);
    row.window_text = Some(window.clone());

    let source_text = chunk_answer_source_text(&row);

    assert!(source_text.contains(condition), "Missing condition: {source_text}");
    assert_eq!(source_text, window);
}

#[test]
fn chunk_answer_source_text_preserves_content_outside_window() {
    let workspace_id = Uuid::now_v7();
    let library_id = Uuid::now_v7();
    let document_id = Uuid::now_v7();
    let revision_id = Uuid::now_v7();
    let mut row = chunk_row(
        workspace_id,
        library_id,
        document_id,
        revision_id,
        0,
        "Overview line.\n1. Theta marker changed from state K to state L.",
    );
    row.chunk_kind = Some("paragraph".to_string());
    row.window_text = Some("Overview line.".to_string());

    let source_text = chunk_answer_source_text(&row);

    assert!(source_text.contains("Overview line."), "{source_text}");
    assert!(source_text.contains("Theta marker changed from state K to state L"), "{source_text}");
}

#[test]
fn chunk_answer_source_text_deduplicates_windows_without_reordering_disjoint_text() {
    let core = "Requests are allowed.\nApproval is required.";
    for (window, expected) in [
        (None, core.to_string()),
        (Some("  \n  "), core.to_string()),
        (Some(core), core.to_string()),
        (Some("Requests are allowed. Approval is required."), core.to_string()),
        (Some("Requests are allowed."), core.to_string()),
        (Some("Contact the operator."), format!("{core}\nContact the operator.")),
        (
            Some("Before starting, obtain approval.\nRequests are allowed.\nApproval is required."),
            "Before starting, obtain approval.\nRequests are allowed.\nApproval is required."
                .to_string(),
        ),
    ] {
        let mut row = answer_source_chunk(0, "paragraph", core, "Unrelated normalized prose.");
        row.window_text = window.map(str::to_string);

        assert_eq!(chunk_answer_source_text(&row), expected, "window={window:?}");
    }
}

#[test]
fn chunk_answer_source_text_uses_window_and_normalized_text_without_content() {
    let core = "Requests are limited to five per hour.";
    for (window, expected) in [
        (None, core.to_string()),
        (Some("Requests are limited."), format!("Requests are limited.\n{core}")),
        (
            Some("Requests are limited to five per hour. Wait for the next hour before retrying."),
            "Requests are limited to five per hour. Wait for the next hour before retrying."
                .to_string(),
        ),
    ] {
        let mut row = answer_source_chunk(0, "paragraph", "", core);
        row.window_text = window.map(str::to_string);

        assert_eq!(chunk_answer_source_text(&row), expected);
    }
}

#[test]
fn chunk_answer_source_text_preserves_core_in_oversized_windows() {
    let core = "Повтор возможен только после согласования.";
    for prefix_chars in [0, 120, 15_990, 16_100] {
        let window = format!("{}{core} {}", "я".repeat(prefix_chars), "я".repeat(16_100));
        let mut row = answer_source_chunk(0, "paragraph", core, core);
        row.window_text = Some(window);

        let source_text = chunk_answer_source_text(&row);

        assert!(source_text.contains(core), "Core lost with prefix={prefix_chars}");
        assert_eq!(source_text.matches(core).count(), 1, "prefix={prefix_chars}");
        assert_eq!(source_text.chars().count(), 16_003);
        assert!(source_text.ends_with("..."));
    }
}

#[test]
fn chunk_answer_source_text_keeps_existing_bound_for_oversized_core() {
    let core = "Ordinary prose. ".repeat(1_100);
    let mut row = answer_source_chunk(0, "paragraph", &core, &core);
    row.window_text = Some("Ordinary prose.".to_string());

    let source_text = chunk_answer_source_text(&row);

    assert_eq!(source_text, format!("{}...", core.chars().take(16_000).collect::<String>()));
}

#[test]
fn map_chunk_hit_preserves_window_conditions_identity_and_readable_revision() {
    let document = document_row("request-policy.txt", "Request policy");
    let revision_id = document.readable_revision_id.unwrap();
    let core = "Requests are limited to five per hour.";
    let condition = "Wait for the next hour before retrying.";
    let window = format!("{core} {condition}");
    let mut row = chunk_row(
        document.workspace_id,
        document.library_id,
        document.document_id,
        revision_id,
        7,
        core,
    );
    row.chunk_kind = Some("paragraph".to_string());
    row.window_text = Some(window.clone());
    let chunk_id = row.chunk_id;
    let document_id = row.document_id;
    let document_index = HashMap::from([(document_id, document)]);

    let mapped = map_chunk_hit(row.clone(), 0.75, &document_index, &[]).unwrap();
    let context = crate::services::query::execution::context::assemble_bounded_context(
        &[],
        &[],
        std::slice::from_ref(&mapped),
        2_048,
    );

    assert_eq!(mapped.source_text, window);
    assert_eq!(
        (mapped.chunk_id, mapped.document_id, mapped.revision_id),
        (chunk_id, document_id, revision_id)
    );
    assert_eq!(mapped.chunk_index, 7);
    assert_eq!(mapped.chunk_kind.as_deref(), Some("paragraph"));
    assert_eq!(mapped.score, Some(0.75));
    assert!(context.contains(core), "{context}");
    assert!(context.contains(condition), "{context}");
    assert!(context.len() <= 2_048);

    row.revision_id = Uuid::now_v7();
    assert!(map_chunk_hit(row, 0.75, &document_index, &[]).is_none());
}

#[test]
fn chunk_answer_source_text_preserves_specialized_kinds_with_containing_windows() {
    for (kind, content, normalized, expected) in [
        ("table_row", "| five |", "Requests | Limit: five", "Requests | Limit: five"),
        (
            "source_unit",
            "Original record prose.",
            "Normalized record prose.",
            "Original record prose.",
        ),
    ] {
        let mut row = answer_source_chunk(0, kind, content, normalized);
        row.window_text = Some(format!("Before. {content} After."));

        assert_eq!(chunk_answer_source_text(&row), expected);
    }
}
