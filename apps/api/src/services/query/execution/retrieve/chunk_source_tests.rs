use super::test_prelude::*;

#[test]
fn chunk_own_source_text_excludes_containing_window() {
    let core = "Автоматическая рассылка писем в изменение не входит.";
    let neighbor = "Показ сообщения в том же чате заявку не создаёт.";
    let mut row = answer_source_chunk(0, "paragraph", core, core);
    row.window_text = Some(format!("{core} {neighbor}"));

    let own = chunk_own_source_text(&row);

    assert_eq!(own, core);
    assert!(!own.contains(neighbor), "neighboring window text leaked into the chunk source: {own}");
    assert_eq!(
        chunk_answer_source_text(&row),
        own,
        "answer context and verifier must read the exact same chunk-own text"
    );
}

#[test]
fn chunk_own_source_text_keeps_own_projection_for_specialized_kinds() {
    for (kind, content, normalized, expected) in [
        ("table_row", "| five |", "Requests | Limit: five", "Requests | Limit: five"),
        ("source_unit", "Recorded unit body.", "Recorded unit body.", "Recorded unit body."),
        ("paragraph", "", "Only the normalized projection.", "Only the normalized projection."),
    ] {
        let mut row = answer_source_chunk(0, kind, content, normalized);
        row.window_text = Some("Neighboring window paragraph.".to_string());

        assert_eq!(chunk_own_source_text(&row), expected, "kind={kind}");
    }
}

#[test]
fn chunk_answer_source_text_excludes_neighbor_window_text_when_chunk_has_own_content() {
    let core = "Повторный запрос допустим.";
    let condition = "После подтверждения записи повтор запрещён.";
    let window = format!("{core} {condition}");
    let mut row = answer_source_chunk(0, "paragraph", core, core);
    row.window_text = Some(window);

    let source_text = chunk_answer_source_text(&row);

    assert_eq!(source_text, core);
    assert!(!source_text.contains(condition), "Neighbor window text leaked: {source_text}");
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
fn chunk_answer_source_text_keeps_own_content_and_normalized_without_window_bleed() {
    let core = "Requests are allowed.\nApproval is required.";
    for window in [
        None,
        Some("  \n  "),
        Some(core),
        Some("Requests are allowed. Approval is required."),
        Some("Requests are allowed."),
        Some("Contact the operator."),
        Some("Before starting, obtain approval.\nRequests are allowed.\nApproval is required."),
    ] {
        let mut row = answer_source_chunk(0, "paragraph", core, "Unrelated normalized prose.");
        row.window_text = window.map(str::to_string);

        assert_eq!(chunk_answer_source_text(&row), core, "window={window:?}");
    }
}

#[test]
fn chunk_answer_source_text_uses_normalized_text_without_content_or_window_bleed() {
    let core = "Requests are limited to five per hour.";
    for window in [
        None,
        Some("Requests are limited."),
        Some("Requests are limited to five per hour. Wait for the next hour before retrying."),
    ] {
        let mut row = answer_source_chunk(0, "paragraph", "", core);
        row.window_text = window.map(str::to_string);

        assert_eq!(chunk_answer_source_text(&row), core);
    }
}

#[test]
fn chunk_answer_source_text_preserves_core_in_oversized_windows() {
    let core = "Повтор возможен только после согласования.";
    for prefix_chars in [0, 120, 15_990, 16_100] {
        let window = format!("{}{core} {}", "я".repeat(prefix_chars), "я".repeat(16_100));
        let mut row = answer_source_chunk(0, "paragraph", core, "Unrelated normalized prose.");
        row.window_text = Some(window);

        let source_text = chunk_answer_source_text(&row);

        assert!(source_text.contains(core), "Core lost with prefix={prefix_chars}");
        assert_eq!(source_text.matches(core).count(), 1, "prefix={prefix_chars}");
        assert!(
            source_text.len()
                <= crate::services::query::execution::chunk_support::MAX_EVIDENCE_SOURCE_UTF8_BYTES
        );
        assert!(!source_text.ends_with("..."));
    }
}

#[test]
fn chunk_answer_source_text_keeps_existing_bound_for_oversized_core() {
    let core = "Ordinary prose. ".repeat(1_100);
    let mut row = answer_source_chunk(0, "paragraph", &core, &core);
    row.window_text = Some("Ordinary prose.".to_string());

    let source_text = chunk_answer_source_text(&row);

    let expected_prefix = core.chars().take(16_000).collect::<String>();
    assert_eq!(source_text, expected_prefix.trim_end());
    assert!(source_text.len() <= 16_000);
}

#[test]
fn oversized_cyrillic_source_is_bounded_in_utf8_bytes() {
    let core = "я".repeat(9_000);
    let row = answer_source_chunk(0, "paragraph", &core, &core);
    let source_text = chunk_answer_source_text(&row);
    assert_eq!(source_text.len(), 16_000);
    assert_eq!(source_text.chars().count(), 8_000);
    assert!(!source_text.ends_with("..."));
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

    assert_eq!(mapped.source_text, core);
    assert_eq!(
        (mapped.chunk_id, mapped.document_id, mapped.revision_id),
        (chunk_id, document_id, revision_id)
    );
    assert_eq!(mapped.chunk_index, 7);
    assert_eq!(mapped.chunk_kind.as_deref(), Some("paragraph"));
    assert_eq!(mapped.score, Some(0.75));
    assert!(context.contains(core), "{context}");
    assert!(!context.contains(condition), "neighbor window leaked into context: {context}");
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
