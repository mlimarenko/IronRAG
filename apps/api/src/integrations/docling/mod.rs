//! Embedded Docling extraction adapter.
//!
//! Docling is installed into the backend image and executed locally. This
//! module owns the process boundary and converts Docling Markdown output into
//! the canonical [`ExtractionOutput`] shape consumed by the ingest pipeline.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};

use serde::Deserialize;
use tempfile::TempDir;
use thiserror::Error;
use tokio::{
    process::Command,
    sync::{OwnedSemaphorePermit, Semaphore},
    time::timeout,
};

use crate::shared::extraction::{
    ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content,
};

const DEFAULT_EXTRACT_BIN: &str = "ironrag-docling-extract";
const DEFAULT_TIMEOUT_SECS: u64 = 300;
const DEFAULT_MAX_CONCURRENCY: usize = 1;
const STDERR_PREVIEW_LIMIT: usize = 4_000;

static DOCLING_CONCURRENCY: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(docling_max_concurrency())));

#[derive(Debug, Error)]
pub enum DoclingExtractionError {
    #[error("failed to create docling workspace: {0}")]
    TempDir(std::io::Error),

    #[error("failed to write docling input: {0}")]
    WriteInput(std::io::Error),

    #[error("docling extractor is unavailable: {0}")]
    Spawn(std::io::Error),

    #[error("docling extraction timed out after {0}s")]
    Timeout(u64),

    #[error("docling extractor failed with status {status}: {stderr}")]
    ProcessFailed { status: String, stderr: String },

    #[error("docling extractor returned invalid utf-8: {0}")]
    InvalidUtf8(std::string::FromUtf8Error),

    #[error("docling extractor returned invalid json: {0}")]
    InvalidJson(serde_json::Error),

    #[error("docling extracted no text")]
    EmptyOutput,

    #[error("docling concurrency limiter is closed")]
    LimiterClosed,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DoclingExtractionPayload {
    markdown: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    page_count: Option<u32>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    input_format: Option<String>,
    #[serde(default)]
    docling_version: Option<String>,
    #[serde(default)]
    warnings: Vec<String>,
    #[serde(default)]
    timings: serde_json::Value,
}

/// Extracts document text with the local Docling runtime.
///
/// # Errors
///
/// Returns [`DoclingExtractionError`] when the embedded extractor is missing,
/// fails, times out, or returns empty/invalid output.
pub async fn extract_document(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
    file_bytes: Vec<u8>,
) -> Result<ExtractionOutput, DoclingExtractionError> {
    let _permit = acquire_docling_permit().await?;
    let temp_dir = tempfile::tempdir().map_err(DoclingExtractionError::TempDir)?;
    let input_path = write_input_file(&temp_dir, file_name, mime_type, source_format, &file_bytes)?;
    let payload = run_docling(&input_path).await?;
    build_output(payload, file_name, mime_type, source_format)
}

async fn acquire_docling_permit() -> Result<OwnedSemaphorePermit, DoclingExtractionError> {
    DOCLING_CONCURRENCY
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| DoclingExtractionError::LimiterClosed)
}

fn write_input_file(
    temp_dir: &TempDir,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
    file_bytes: &[u8],
) -> Result<PathBuf, DoclingExtractionError> {
    let file_name = normalized_input_file_name(file_name, mime_type, source_format);
    let input_path = temp_dir.path().join(file_name);
    std::fs::write(&input_path, file_bytes).map_err(DoclingExtractionError::WriteInput)?;
    Ok(input_path)
}

async fn run_docling(
    input_path: &Path,
) -> Result<DoclingExtractionPayload, DoclingExtractionError> {
    let timeout_secs = docling_timeout_secs();
    let mut command = Command::new(docling_extract_bin());
    command.arg(input_path).kill_on_drop(true);

    let output = timeout(Duration::from_secs(timeout_secs), command.output())
        .await
        .map_err(|_| DoclingExtractionError::Timeout(timeout_secs))?
        .map_err(DoclingExtractionError::Spawn)?;

    if !output.status.success() {
        let status =
            output.status.code().map_or_else(|| "signal".to_string(), |code| code.to_string());
        let stderr =
            String::from_utf8(output.stderr).map_err(DoclingExtractionError::InvalidUtf8)?;
        return Err(DoclingExtractionError::ProcessFailed {
            status,
            stderr: truncate_for_error(&stderr),
        });
    }

    serde_json::from_slice(&output.stdout).map_err(DoclingExtractionError::InvalidJson)
}

fn build_output(
    payload: DoclingExtractionPayload,
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
) -> Result<ExtractionOutput, DoclingExtractionError> {
    let content = if payload.markdown.trim().is_empty() {
        payload.text.unwrap_or_default()
    } else {
        payload.markdown
    };
    if content.trim().is_empty() {
        return Err(DoclingExtractionError::EmptyOutput);
    }

    let layout = build_text_layout_from_content(content.trim());
    let line_count = i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX);
    let input_format = payload.input_format.unwrap_or_else(|| source_format.to_string());
    let page_count = payload.page_count;

    Ok(ExtractionOutput {
        extraction_kind: "docling_markdown".to_string(),
        content_text: layout.content_text,
        page_count,
        warnings: payload.warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: input_format.clone(),
            page_count,
            line_count,
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "adapter": "docling",
            "input_file_name": file_name,
            "mime_type": mime_type,
            "source_format": source_format,
            "docling_input_format": input_format,
            "docling_status": payload.status,
            "docling_version": payload.docling_version,
            "timings": payload.timings,
        }),
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images: Vec::new(),
    })
}

fn docling_extract_bin() -> String {
    std::env::var("IRONRAG_DOCLING_EXTRACT_BIN").unwrap_or_else(|_| DEFAULT_EXTRACT_BIN.to_string())
}

fn docling_timeout_secs() -> u64 {
    std::env::var("IRONRAG_DOCLING_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
}

fn docling_max_concurrency() -> usize {
    std::env::var("IRONRAG_DOCLING_MAX_CONCURRENCY")
        .ok()
        .and_then(|raw| raw.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_CONCURRENCY)
}

fn normalized_input_file_name(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
) -> String {
    let extension = file_extension_for_docling(file_name, mime_type, source_format);
    let base = file_name
        .and_then(|value| Path::new(value).file_name())
        .and_then(OsStr::to_str)
        .map(|value| {
            value
                .chars()
                .map(|ch| if ch == '/' || ch == '\\' || ch == '\0' { '_' } else { ch })
                .collect::<String>()
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "upload".to_string());

    if Path::new(&base).extension().is_some() { base } else { format!("{base}.{extension}") }
}

fn file_extension_for_docling(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    source_format: &str,
) -> &'static str {
    if let Some(ext) = file_name
        .and_then(|value| Path::new(value).extension())
        .and_then(OsStr::to_str)
        .map(str::to_ascii_lowercase)
    {
        return match ext.as_str() {
            "pdf" => "pdf",
            "docx" => "docx",
            "pptx" => "pptx",
            "png" => "png",
            "jpg" | "jpeg" => "jpg",
            "tif" | "tiff" => "tiff",
            "bmp" => "bmp",
            "webp" => "webp",
            _ => extension_from_source_format(source_format),
        };
    }

    match mime_type.map(str::to_ascii_lowercase).as_deref() {
        Some("application/pdf") => "pdf",
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document") => "docx",
        Some("application/vnd.openxmlformats-officedocument.presentationml.presentation") => "pptx",
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some("image/tiff") => "tiff",
        Some("image/bmp") => "bmp",
        Some("image/webp") => "webp",
        _ => extension_from_source_format(source_format),
    }
}

fn extension_from_source_format(source_format: &str) -> &'static str {
    match source_format {
        "pdf" => "pdf",
        "docx" => "docx",
        "pptx" => "pptx",
        "image" | "png" => "png",
        "jpeg" | "jpg" => "jpg",
        "tiff" => "tiff",
        "bmp" => "bmp",
        "webp" => "webp",
        _ => "bin",
    }
}

fn truncate_for_error(value: &str) -> String {
    let mut output = value.chars().take(STDERR_PREVIEW_LIMIT).collect::<String>();
    if value.chars().count() > STDERR_PREVIEW_LIMIT {
        output.push_str("...");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_extraction_output_from_docling_markdown() {
        let payload = DoclingExtractionPayload {
            markdown: "# Operations Report\n\n| Region | Amount |\n|---|---:|\n| West | 42 |"
                .to_string(),
            text: None,
            page_count: Some(2),
            status: Some("success".to_string()),
            input_format: Some("pdf".to_string()),
            docling_version: Some("2.91.0".to_string()),
            warnings: Vec::new(),
            timings: serde_json::json!({"total": 1.5}),
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
    fn normalized_input_file_name_adds_extension_when_missing() {
        assert_eq!(
            normalized_input_file_name(Some("upload"), Some("application/pdf"), "pdf"),
            "upload.pdf"
        );
        assert_eq!(
            normalized_input_file_name(Some("../unsafe/report"), None, "docx"),
            "report.docx"
        );
    }
}
