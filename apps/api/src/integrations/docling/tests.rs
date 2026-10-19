use super::*;
use super::{
    output::build_output,
    runtime::{
        auto_docling_max_concurrency_for_limits, default_docling_acquire_timeout_secs_for,
        docling_process_memory_headroom_for_limits, normalized_input_file_name,
    },
};

#[test]
fn docling_acquire_timeout_default_outlasts_a_full_pipeline_queue() {
    // One Docling slot serves DOCLING_WAITERS_PER_PROCESS (2) heavy-pipeline
    // jobs, so the default wait covers two maximum-length extractions at the
    // default 900 s extractor timeout.
    assert_eq!(default_docling_acquire_timeout_secs_for(900), 1_800);
}

#[test]
fn builds_extraction_output_from_docling_markdown() {
    let payload = DoclingExtractionPayload {
        markdown: "# Operations Report\n\n| Region | Amount |\n|---|---:|\n| West | 42 |"
            .to_string(),
        text: None,
        picture_ocr_text: Vec::new(),
        page_count: Some(2),
        status: Some("success".to_string()),
        input_format: Some("pdf".to_string()),
        docling_version: Some("2.91.0".to_string()),
        warnings: Vec::new(),
        timings: serde_json::json!({"total": 1.5}),
        pictures: Vec::new(),
    };

    let output =
        build_output(payload, Some("operations-report.pdf"), Some("application/pdf"), "pdf")
            .expect("docling output");

    assert_eq!(output.extraction_kind, "docling_markdown");
    assert_eq!(output.page_count, Some(2));
    assert_eq!(output.source_metadata.source_format, "pdf");
    assert!(output.content_text.contains("| West | 42 |"));
    assert!(output.structure_hints.lines.iter().any(|line| {
        line.signals.contains(&crate::shared::extraction::ExtractionLineSignal::TableRow)
    }));
    assert_eq!(output.source_map["adapter"], "docling");
}

#[test]
fn builds_extraction_output_from_docling_text_when_markdown_is_only_image_placeholder() {
    let payload = DoclingExtractionPayload {
        markdown: "<!-- image -->".to_string(),
        text: Some("Formats PDF / DOCX / PPTX / PNG / JPG".to_string()),
        picture_ocr_text: Vec::new(),
        page_count: Some(1),
        status: Some("success".to_string()),
        input_format: Some("pdf".to_string()),
        docling_version: Some("2.91.0".to_string()),
        warnings: Vec::new(),
        timings: serde_json::json!({"total": 1.5}),
        pictures: Vec::new(),
    };

    let output = build_output(payload, Some("formats.pdf"), Some("application/pdf"), "pdf")
        .expect("docling output");

    assert_eq!(output.content_text, "Formats PDF / DOCX / PPTX / PNG / JPG");
}

#[test]
fn rejects_docling_output_when_markdown_is_only_image_placeholder_without_text() {
    let payload = DoclingExtractionPayload {
        markdown: "<!-- image -->".to_string(),
        text: None,
        picture_ocr_text: Vec::new(),
        page_count: Some(1),
        status: Some("success".to_string()),
        input_format: Some("pdf".to_string()),
        docling_version: Some("2.91.0".to_string()),
        warnings: Vec::new(),
        timings: serde_json::json!({"total": 1.5}),
        pictures: Vec::new(),
    };

    let error = build_output(payload, Some("empty.pdf"), Some("application/pdf"), "pdf")
        .expect_err("placeholder-only output should be rejected");

    assert!(matches!(error, DoclingExtractionError::EmptyOutput));
}

#[test]
fn normalized_input_file_name_adds_extension_when_missing() {
    assert_eq!(
        normalized_input_file_name(Some("upload"), Some("application/pdf"), "pdf"),
        "upload.pdf"
    );
    assert_eq!(normalized_input_file_name(Some("../unsafe/report"), None, "docx"), "report.docx");
}

#[test]
fn auto_docling_concurrency_uses_cpu_and_memory_bounds() {
    assert_eq!(auto_docling_max_concurrency_for_limits(4, Some(8 * 1024 * 1024 * 1024)), 2);
    assert_eq!(auto_docling_max_concurrency_for_limits(8, Some(16 * 1024 * 1024 * 1024)), 4);
    assert_eq!(auto_docling_max_concurrency_for_limits(8, Some(4 * 1024 * 1024 * 1024)), 1);
    assert_eq!(auto_docling_max_concurrency_for_limits(8, None), 1);
}

#[test]
fn docling_process_memory_gate_rejects_limits_below_one_process_floor() {
    let error = docling_process_memory_headroom_for_limits(768 * 1024 * 1024, Some(64))
        .expect_err("tiny worker cap cannot safely run a docling process");

    assert_eq!(error.memory_failure_code(), Some("docling_insufficient_memory"));
}

#[cfg(target_family = "unix")]
#[test]
fn docling_process_memory_failure_uses_typed_sigkill_status() {
    let error = DoclingExtractionError::ProcessFailed {
        status: std::process::ExitStatus::from_raw(9),
        stderr: String::new(),
    };

    assert_eq!(error.memory_failure_code(), Some("docling_process_oom"));
}

#[cfg(target_family = "unix")]
#[test]
fn docling_process_memory_failure_ignores_stderr_prose() {
    let error = DoclingExtractionError::ProcessFailed {
        status: std::process::ExitStatus::from_raw(9 << 8),
        stderr: "SIGKILL likely out of memory".to_string(),
    };

    assert_eq!(error.memory_failure_code(), None);
}

#[test]
fn docling_process_memory_failure_ignores_nested_error_prose() {
    let error = DoclingExtractionError::PageExtractionFailed {
        page: 1,
        source: Box::new(DoclingExtractionError::PaginatedMergeFailed(
            "SIGKILL likely out of memory".to_string(),
        )),
    };

    assert_eq!(error.memory_failure_code(), None);
}

#[test]
fn docling_process_memory_gate_allows_single_process_when_headroom_exists() {
    let available = docling_process_memory_headroom_for_limits(4 * 1024 * 1024 * 1024, Some(256))
        .expect("4 GiB worker cap has enough headroom for one docling process");

    assert!(available >= DOCLING_AUTO_MEMORY_PER_PROCESS_MIB);
}

#[test]
fn page_batch_range_covers_every_page_once_in_order() {
    assert_eq!(page_batch_range(0, 10, 25), (1, 10));
    assert_eq!(page_batch_range(1, 10, 25), (11, 20));
    assert_eq!(page_batch_range(2, 10, 25), (21, 25));
}

#[test]
fn page_batch_range_handles_a_single_short_batch() {
    assert_eq!(page_batch_range(0, 10, 3), (1, 3));
    assert_eq!(page_batch_range(0, 1, 1), (1, 1));
}
