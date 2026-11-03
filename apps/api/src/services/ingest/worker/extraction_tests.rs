use super::extraction::{
    PdfExtractUnit, PdfExtractUnitMergeFragment, await_with_extraction_cancellation,
    contiguous_pdf_extract_unit_runs, merge_pdf_unit_outputs, pdf_xberg_failure,
};
use crate::shared::extraction::{
    ExtractedImage, ExtractionOutput, ExtractionSourceMetadata, ExtractionStructureHints,
};
use crate::{domains::ops::OpsFailureDisposition, integrations::xberg::XbergExtractionError};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[test]
fn resumable_pdf_xberg_output_faults_are_terminal_and_timeouts_retry() {
    let invalid_json = XbergExtractionError::InvalidJson(
        serde_json::from_str::<serde_json::Value>("{").expect_err("invalid JSON fixture"),
    );
    for (error, code, expected) in [
        (invalid_json, "pdf_page_count_failed", OpsFailureDisposition::OperationalTerminal),
        (
            XbergExtractionError::PageExtractionFailed {
                page: 1,
                source: Box::new(XbergExtractionError::EmptyOutput),
            },
            "pdf_page_range_extract_failed",
            OpsFailureDisposition::OperationalTerminal,
        ),
        (
            XbergExtractionError::Timeout(1),
            "pdf_page_count_failed",
            OpsFailureDisposition::RetryExhausted,
        ),
        (
            XbergExtractionError::Timeout(1),
            "pdf_page_range_extract_failed",
            OpsFailureDisposition::RetryExhausted,
        ),
    ] {
        let result = pdf_xberg_failure(&error, code, error.to_string());
        assert_eq!(result.failure_code, code);
        assert_eq!(result.failure_disposition, expected, "{error}");
    }
    let memory = pdf_xberg_failure(
        &XbergExtractionError::InsufficientMemory {
            memory_limit_mib: 1024,
            current_rss_mib: 960,
            available_mib: 64,
            required_mib: 512,
            safety_margin_mib: 256,
        },
        "pdf_page_count_failed",
        "out of memory".to_string(),
    );
    assert_eq!(memory.failure_code, "xberg_insufficient_memory");
    assert_eq!(memory.failure_disposition, OpsFailureDisposition::OperationalTerminal);
}

#[tokio::test]
async fn cancellation_aborts_an_in_flight_extraction_future() {
    let cancellation_token = CancellationToken::new();
    let cancellation_for_task = cancellation_token.clone();
    let revision_id = Uuid::now_v7();
    let (started_sender, started_receiver) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        await_with_extraction_cancellation(&cancellation_for_task, revision_id, async move {
            let _ = started_sender.send(());
            std::future::pending::<()>().await;
        })
        .await
    });

    started_receiver.await.expect("extraction future must start");
    cancellation_token.cancel();

    let result = tokio::time::timeout(std::time::Duration::from_millis(100), task)
        .await
        .expect("cancellation must abort extraction promptly")
        .expect("cancellation task must not panic");
    let error = result.expect_err("canceled extraction must fail");
    assert_eq!(error.failure_code, "extract_canceled");
}

fn unit(unit_ordinal: i32, start_page: u32, end_page: u32) -> PdfExtractUnit {
    PdfExtractUnit {
        unit_ordinal,
        start_page,
        end_page,
        range_start: i32::try_from(start_page).unwrap(),
        range_end: i32::try_from(end_page).unwrap(),
    }
}

#[test]
fn splits_contiguous_pdf_extract_runs_by_memory_window() {
    let units =
        vec![unit(1, 1, 10), unit(2, 11, 20), unit(3, 21, 30), unit(4, 31, 40), unit(5, 41, 50)];

    let runs = contiguous_pdf_extract_unit_runs(&units, 25);

    assert_eq!(runs.len(), 3);
    assert_eq!((runs[0].start_page, runs[0].end_page), (1, 20));
    assert_eq!((runs[1].start_page, runs[1].end_page), (21, 40));
    assert_eq!((runs[2].start_page, runs[2].end_page), (41, 50));
}

#[test]
fn keeps_pdf_extract_run_boundaries_at_existing_gaps() {
    let units = vec![unit(1, 1, 10), unit(3, 31, 40), unit(4, 41, 50)];

    let runs = contiguous_pdf_extract_unit_runs(&units, 100);

    assert_eq!(runs.len(), 2);
    assert_eq!((runs[0].start_page, runs[0].end_page), (1, 10));
    assert_eq!((runs[1].start_page, runs[1].end_page), (31, 50));
}

#[test]
fn pdf_extract_merge_fragment_keeps_structure_and_drops_only_image_bytes() {
    let fragment = PdfExtractUnitMergeFragment::from_output(ExtractionOutput {
        extraction_kind: "xberg_markdown".to_string(),
        content_text: "Alpha section".to_string(),
        page_count: Some(1),
        warnings: vec!["partial table".to_string()],
        source_metadata: ExtractionSourceMetadata {
            source_format: "pdf".to_string(),
            page_count: Some(1),
            line_count: 1,
        },
        structure_hints: ExtractionStructureHints::default(),
        source_map: serde_json::json!({"heavy": true}),
        provider_kind: Some("provider".to_string()),
        model_name: Some("model".to_string()),
        usage_json: serde_json::json!({"tokens": 42}),
        extracted_images: vec![ExtractedImage {
            page: 1,
            image_bytes: vec![7_u8; 1024 * 1024],
            mime_type: "image/png".to_string(),
            width: 128,
            height: 128,
        }],
    });

    assert_eq!(fragment.content_text, "Alpha section");
    assert_eq!(fragment.warnings, vec!["partial table"]);
    assert_eq!(fragment.provider_kind.as_deref(), Some("provider"));
    assert_eq!(fragment.model_name.as_deref(), Some("model"));
    assert_eq!(fragment.usage_json, serde_json::json!({"tokens": 42}));
    assert_eq!(fragment.source_map["heavy"], true);
}

fn merge_fragment(text: &str, page: i32, calls: serde_json::Value) -> PdfExtractUnitMergeFragment {
    let layout = crate::shared::extraction::build_text_layout(&[
        crate::shared::extraction::RawExtractionPage {
            page_number: Some(page),
            lines: vec![text.into()],
        },
    ]);
    PdfExtractUnitMergeFragment {
        content_text: layout.content_text,
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({"units":[{"ordinal":page,"tables":[{"cells":["preserved"]}]}],
            "visual_recognition":[{"source_unit":page,"outcome":{"outcome":"text","text":text}}]}),
        warnings: Vec::new(),
        provider_kind: Some("provider".into()),
        model_name: Some("model".into()),
        usage_json: serde_json::json!({"vision_calls":calls}),
    }
}

#[test]
fn merge_pdf_unit_outputs_preserves_page_hints_offsets_tables_and_visual_outcomes() {
    let output = merge_pdf_unit_outputs(
        vec![
            merge_fragment("Alpha section", 1, serde_json::json!([])),
            merge_fragment("Beta section", 2, serde_json::json!([])),
        ],
        2,
        "input.pdf",
        "application/pdf",
    )
    .expect("readable PDF aggregate");
    assert_eq!(output.content_text, "Alpha section\n\nBeta section");
    assert_eq!(output.structure_hints.lines.len(), 2);
    let second = &output.structure_hints.lines[1];
    assert_eq!(second.page_number, Some(2));
    assert_eq!(second.ordinal, 1);
    assert_eq!(second.start_offset, Some(15));
    assert_eq!(second.end_offset, Some(27));
    assert_eq!(output.source_map["units"][1]["tables"][0]["cells"][0], "preserved");
    assert_eq!(output.source_map["visual_recognition"][1]["source_unit"], 2);
    assert_eq!(output.source_map["native_extractions"].as_array().unwrap().len(), 2);
}

#[test]
fn merge_pdf_unit_outputs_keeps_each_raw_call_without_an_extra_aggregate() {
    let output = merge_pdf_unit_outputs(vec![
        merge_fragment("Alpha section", 1, serde_json::json!([
            {"provider_kind":"provider","model_name":"model","usage_json":{"total_tokens":12}},
            {"provider_kind":"provider","model_name":"model","usage_json":{"total_tokens":24}},
        ])),
        merge_fragment("Beta section", 2, serde_json::json!([
            {"provider_kind":"provider","model_name":"model","usage_json":{"total_tokens":36}},
        ])),
    ], 2, "input.pdf", "application/pdf").unwrap();
    let calls = output.usage_json["vision_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[0]["usage_json"]["total_tokens"], 12);
    assert_eq!(calls[2]["usage_json"]["total_tokens"], 36);
    assert!(output.usage_json.get("total_tokens").is_none());
}

#[test]
fn blank_pdf_ranges_after_readable_ranges_do_not_erase_or_reject_the_document() {
    let fragment = |text: &str, page: i32| {
        let layout = crate::shared::extraction::build_text_layout(&[
            crate::shared::extraction::RawExtractionPage {
                page_number: Some(page),
                lines: text.split('\n').map(str::to_owned).collect(),
            },
        ]);
        PdfExtractUnitMergeFragment {
            structure_hints: layout.structure_hints,
            source_map: serde_json::json!({}),
            content_text: layout.content_text,
            warnings: vec![],
            provider_kind: None,
            model_name: None,
            usage_json: serde_json::json!({}),
        }
    };
    let output = merge_pdf_unit_outputs(
        vec![fragment("Earlier readable page", 1), fragment("\n\n", 2)],
        2,
        "mixed.pdf",
        "application/pdf",
    )
    .expect("blank later ranges must not reject readable content");
    assert_eq!(output.content_text.trim(), "Earlier readable page");
    assert_eq!(output.page_count, Some(2));
    assert_eq!(output.structure_hints.lines[0].page_number, Some(1));
    assert!(
        output.structure_hints.lines[1..]
            .iter()
            .all(|line| { line.page_number == Some(2) && line.text.is_empty() })
    );
    let plan = crate::shared::extraction::file_extract::build_xberg_pdf_extraction_plan(output);
    assert_eq!(plan.normalized_text.as_deref().map(str::trim), Some("Earlier readable page"));
    assert_eq!(plan.source_format_metadata.page_count, Some(2));

    let error = merge_pdf_unit_outputs(
        vec![fragment("", 1), fragment("\n\n", 2)],
        2,
        "blank.pdf",
        "application/pdf",
    )
    .expect_err("only the complete all-blank document is refused");
    assert_eq!(error.failure_code, "pdf_no_usable_content");
    assert_eq!(error.failure_disposition, OpsFailureDisposition::OperationalTerminal);
}
