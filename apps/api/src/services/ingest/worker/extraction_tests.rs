use super::extraction::{
    PdfExtractUnit, PdfExtractUnitMergeFragment, await_with_extraction_cancellation,
    contiguous_pdf_extract_unit_runs, merge_pdf_unit_outputs, pdf_docling_failure,
};
use crate::shared::extraction::{
    ExtractedImage, ExtractionOutput, ExtractionSourceMetadata, ExtractionStructureHints,
};
use crate::{domains::ops::OpsFailureDisposition, integrations::docling::DoclingExtractionError};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[test]
fn resumable_pdf_docling_output_faults_are_terminal_and_timeouts_retry() {
    let invalid_json = DoclingExtractionError::InvalidJson(
        serde_json::from_str::<serde_json::Value>("{").expect_err("invalid JSON fixture"),
    );
    for (error, code, expected) in [
        (invalid_json, "pdf_page_count_failed", OpsFailureDisposition::OperationalTerminal),
        (
            DoclingExtractionError::PageExtractionFailed {
                page: 1,
                source: Box::new(DoclingExtractionError::EmptyOutput),
            },
            "pdf_page_range_extract_failed",
            OpsFailureDisposition::OperationalTerminal,
        ),
        (
            DoclingExtractionError::Timeout(1),
            "pdf_page_count_failed",
            OpsFailureDisposition::RetryExhausted,
        ),
        (
            DoclingExtractionError::Timeout(1),
            "pdf_page_range_extract_failed",
            OpsFailureDisposition::RetryExhausted,
        ),
    ] {
        let result = pdf_docling_failure(&error, code, error.to_string());
        assert_eq!(result.failure_code, code);
        assert_eq!(result.failure_disposition, expected, "{error}");
    }
    let memory = pdf_docling_failure(
        &DoclingExtractionError::InsufficientMemory {
            memory_limit_mib: 1024,
            current_rss_mib: 960,
            available_mib: 64,
            required_mib: 512,
            safety_margin_mib: 256,
        },
        "pdf_page_count_failed",
        "out of memory".to_string(),
    );
    assert_eq!(memory.failure_code, "docling_insufficient_memory");
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
fn pdf_extract_merge_fragment_drops_heavy_extraction_state() {
    let fragment = PdfExtractUnitMergeFragment::from_output(ExtractionOutput {
        extraction_kind: "docling_markdown".to_string(),
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
}

#[test]
fn merge_pdf_unit_outputs_rebuilds_final_layout_from_fragments() {
    let output = merge_pdf_unit_outputs(
        vec![
            PdfExtractUnitMergeFragment {
                content_text: "Alpha section".to_string(),
                warnings: vec!["first warning".to_string()],
                provider_kind: Some("provider".to_string()),
                model_name: Some("model".to_string()),
                usage_json: serde_json::json!({"tokens": 7}),
            },
            PdfExtractUnitMergeFragment {
                content_text: "Beta section".to_string(),
                warnings: vec!["second warning".to_string()],
                provider_kind: None,
                model_name: None,
                usage_json: serde_json::json!({}),
            },
        ],
        2,
        "input.pdf",
        "application/pdf",
    );

    assert_eq!(output.content_text, "Alpha section\n\nBeta section");
    assert_eq!(output.page_count, Some(2));
    assert_eq!(output.warnings, vec!["first warning", "second warning"]);
    assert_eq!(output.provider_kind.as_deref(), Some("provider"));
    assert_eq!(output.model_name.as_deref(), Some("model"));
    assert_eq!(output.usage_json, serde_json::json!({"tokens": 7}));
    assert!(output.extracted_images.is_empty());
    assert_eq!(output.structure_hints.lines.len(), 3);
}

#[test]
fn merge_pdf_unit_outputs_preserves_all_embedded_picture_usage() {
    let output = merge_pdf_unit_outputs(
        vec![
            PdfExtractUnitMergeFragment {
                content_text: "Alpha section".to_string(),
                warnings: Vec::new(),
                provider_kind: Some("provider".to_string()),
                model_name: Some("model".to_string()),
                usage_json: serde_json::json!({
                    "embedded_picture_ocr_usage": [
                        {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12},
                        {"prompt_tokens": 20, "completion_tokens": 4, "total_tokens": 24}
                    ],
                }),
            },
            PdfExtractUnitMergeFragment {
                content_text: "Beta section".to_string(),
                warnings: Vec::new(),
                provider_kind: Some("provider".to_string()),
                model_name: Some("model".to_string()),
                usage_json: serde_json::json!({
                    "embedded_picture_ocr_usage": [
                        {"prompt_tokens": 30, "completion_tokens": 6, "total_tokens": 36}
                    ],
                }),
            },
        ],
        2,
        "input.pdf",
        "application/pdf",
    );

    assert_eq!(output.usage_json["embedded_picture_ocr_call_count"], serde_json::json!(3));
    assert_eq!(output.usage_json["prompt_tokens"], serde_json::json!(60));
    assert_eq!(output.usage_json["completion_tokens"], serde_json::json!(12));
    assert_eq!(output.usage_json["total_tokens"], serde_json::json!(72));
    assert_eq!(output.usage_json["embedded_picture_ocr_usage"].as_array().map(Vec::len), Some(3));
}
