use super::*;

use crate::shared::extraction::file_extract::{ExtractionFailureClass, UploadAdmissionError};

#[derive(Default)]
pub(super) struct MappedCounts {
    pub(super) counts: WebRunCounts,
    pub(super) last_activity_at: Option<DateTime<Utc>>,
}

pub(super) const fn map_web_run_counts_row(row: WebRunCountsRow) -> MappedCounts {
    MappedCounts {
        counts: WebRunCounts {
            discovered: row.discovered,
            eligible: row.eligible,
            processed: row.processed,
            queued: row.queued,
            processing: row.processing,
            duplicates: row.duplicates,
            excluded: row.excluded,
            blocked: row.blocked,
            failed: row.failed,
            canceled: row.canceled,
        },
        last_activity_at: row.last_activity_at,
    }
}

pub(super) fn map_web_run_receipt(run: WebIngestRun) -> WebIngestRunReceipt {
    WebIngestRunReceipt {
        run_id: run.run_id,
        library_id: run.library_id,
        mode: run.mode,
        run_state: run.run_state,
        async_operation_id: run.async_operation_id,
        counts: run.counts,
        failure_code: run.failure_code,
        cancel_requested_at: run.cancel_requested_at,
    }
}

/// Crawl and materialization filters included in the source-identity fingerprint.
pub(super) struct WebRunFilterPatterns<'a> {
    pub(super) crawl_allow: &'a serde_json::Value,
    pub(super) crawl_block: &'a serde_json::Value,
    pub(super) materialization_allow: &'a serde_json::Value,
    pub(super) materialization_block: &'a serde_json::Value,
}

pub(super) fn web_run_source_identity(
    normalized_seed_url: &str,
    mode: &str,
    boundary_policy: &str,
    max_depth: i32,
    max_pages: i32,
    patterns: WebRunFilterPatterns<'_>,
) -> String {
    let WebRunFilterPatterns {
        crawl_allow: crawl_allow_patterns_json,
        crawl_block: crawl_block_patterns_json,
        materialization_allow: materialization_allow_patterns_json,
        materialization_block: materialization_block_patterns_json,
    } = patterns;
    let payload = serde_json::json!({
        "normalizedSeedUrl": normalized_seed_url,
        "mode": mode,
        "boundaryPolicy": boundary_policy,
        "maxDepth": max_depth,
        "maxPages": max_pages,
        "crawlFilter": {
            "allowPatterns": crawl_allow_patterns_json,
            "blockPatterns": crawl_block_patterns_json,
        },
        "materializationFilter": {
            "allowPatterns": materialization_allow_patterns_json,
            "blockPatterns": materialization_block_patterns_json,
        },
    });
    let bytes = serde_json::to_vec(&payload).unwrap_or_default();
    format!("web_capture:sha256:{}", hex::encode(Sha256::digest(bytes)))
}

pub(super) fn parse_run_filter_patterns(
    value: serde_json::Value,
) -> Result<Vec<WebIngestPattern>, ApiError> {
    serde_json::from_value(value).map_err(|_| ApiError::Internal)
}

pub(super) fn parse_run_url_filter(
    allow_patterns: serde_json::Value,
    block_patterns: serde_json::Value,
) -> Result<WebIngestUrlFilter, ApiError> {
    Ok(WebIngestUrlFilter {
        allow_patterns: parse_run_filter_patterns(allow_patterns)?,
        block_patterns: parse_run_filter_patterns(block_patterns)?,
    })
}

pub(super) fn map_web_page_row(row: WebDiscoveredPageRow) -> WebDiscoveredPage {
    WebDiscoveredPage {
        candidate_id: row.id,
        run_id: row.run_id,
        discovered_url: row.discovered_url,
        normalized_url: row.normalized_url,
        final_url: row.final_url,
        preferred_url: row.preferred_url,
        depth: row.depth,
        referrer_candidate_id: row.referrer_candidate_id,
        host_classification: row.host_classification,
        candidate_state: row.candidate_state,
        classification_reason: row.classification_reason,
        classification_detail: row.classification_detail,
        content_type: row.content_type,
        http_status: row.http_status,
        discovered_at: row.discovered_at,
        updated_at: row.updated_at,
        document_id: row.document_id,
        result_revision_id: row.result_revision_id,
        mutation_item_id: row.mutation_item_id,
    }
}

impl WebRunFailure {
    pub(super) fn from_fetch_error(error: PublicHttpUrlError) -> Self {
        if error.is_terminal_policy_rejection() {
            Self::invalid_url()
        } else {
            Self::inaccessible()
        }
    }

    pub(super) fn inaccessible() -> Self {
        Self {
            failure_code: WebRunFailureCode::Inaccessible.as_str().to_string(),
            candidate_reason: Some(WebClassificationReason::Inaccessible.as_str().to_string()),
            final_url: None,
            content_type: None,
            http_status: None,
        }
    }

    pub(super) fn inaccessible_with_response(
        final_url: Option<String>,
        content_type: Option<String>,
        http_status: Option<i32>,
    ) -> Self {
        Self {
            failure_code: WebRunFailureCode::Inaccessible.as_str().to_string(),
            candidate_reason: Some(WebClassificationReason::Inaccessible.as_str().to_string()),
            final_url,
            content_type,
            http_status,
        }
    }

    pub(super) fn invalid_url() -> Self {
        Self {
            failure_code: WebRunFailureCode::InvalidUrl.as_str().to_string(),
            candidate_reason: Some(WebClassificationReason::InvalidUrl.as_str().to_string()),
            final_url: None,
            content_type: None,
            http_status: None,
        }
    }

    pub(super) fn unsupported_content(
        final_url: Option<String>,
        content_type: Option<String>,
        http_status: Option<i32>,
    ) -> Self {
        Self {
            failure_code: WebRunFailureCode::UnsupportedContent.as_str().to_string(),
            candidate_reason: Some(
                WebClassificationReason::UnsupportedContent.as_str().to_string(),
            ),
            final_url,
            content_type,
            http_status,
        }
    }

    /// The one mapping from a typed extraction rejection to a web-run outcome.
    /// Only a deterministic content rejection (`ContentInvalid`) is
    /// `unsupported_content`; an operational/provider/configuration fault — and a
    /// request-level rejection carrying no content class (`None`) — is an
    /// internal capture failure, never a statement about the candidate's content.
    pub(super) fn from_extraction_rejection(
        rejection: &UploadAdmissionError,
        final_url: Option<String>,
        content_type: Option<String>,
        http_status: Option<i32>,
    ) -> Self {
        match rejection.extraction_failure_class() {
            Some(ExtractionFailureClass::ContentInvalid) => {
                Self::unsupported_content(final_url, content_type, http_status)
            }
            Some(ExtractionFailureClass::OperationalRetryable)
            | Some(ExtractionFailureClass::OperationalTerminal)
            | None => Self::internal(
                WebRunFailureCode::WebCaptureMaterializationFailed.as_str(),
                final_url,
                content_type,
                http_status,
            ),
        }
    }

    pub(super) fn internal(
        failure_code: &str,
        final_url: Option<String>,
        content_type: Option<String>,
        http_status: Option<i32>,
    ) -> Self {
        Self {
            failure_code: failure_code.to_string(),
            candidate_reason: None,
            final_url,
            content_type,
            http_status,
        }
    }
}

pub(super) fn extraction_title(source_map: &serde_json::Value) -> Option<String> {
    source_map
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

pub(super) fn resolved_web_mime_type(
    content_type: Option<&str>,
    extraction_plan: &crate::shared::extraction::file_extract::FileExtractionPlan,
) -> String {
    content_type.map_or_else(
        || match extraction_plan.extraction_kind.as_str() {
            "html_main_content" => "text/html".to_string(),
            "pdf_text" => "application/pdf".to_string(),
            "docx_text" => {
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                    .to_string()
            }
            "tabular_text" => {
                spreadsheet_mime_type(extraction_plan.source_format_metadata.source_format.as_str())
                    .to_string()
            }
            "pptx_text" => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
                    .to_string()
            }
            _ => "text/plain".to_string(),
        },
        |content_type| content_type.trim().to_string(),
    )
}

pub(super) fn source_file_name_from_url(final_url: &str, content_type: Option<&str>) -> String {
    let fallback = match content_type {
        Some(value) if value.starts_with("text/html") || value == "application/xhtml+xml" => {
            "index.html"
        }
        Some("application/pdf") => "download.pdf",
        Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document") => {
            "document.docx"
        }
        Some("text/csv" | "application/csv") => "table.csv",
        Some("text/tab-separated-values") => "table.tsv",
        Some("application/vnd.ms-excel") => "workbook.xls",
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet") => {
            "workbook.xlsx"
        }
        Some("application/vnd.ms-excel.sheet.binary.macroenabled.12") => "workbook.xlsb",
        Some("application/vnd.oasis.opendocument.spreadsheet") => "workbook.ods",
        Some("application/vnd.openxmlformats-officedocument.presentationml.presentation") => {
            "slides.pptx"
        }
        _ => "download.bin",
    };
    reqwest::Url::parse(final_url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_else(|| fallback.to_string())
}

pub(super) fn is_direct_image_web_resource(final_url: &str, content_type: Option<&str>) -> bool {
    content_type
        .and_then(normalized_mime_type)
        .is_some_and(|mime_type| mime_type.starts_with("image/"))
        || url_extension(final_url).is_some_and(|extension| {
            matches!(
                extension.as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "bmp"
                    | "webp"
                    | "svg"
                    | "tif"
                    | "tiff"
                    | "heic"
                    | "heif"
            )
        })
}

fn normalized_mime_type(content_type: &str) -> Option<String> {
    let mime_type = content_type.split(';').next()?.trim().to_ascii_lowercase();
    (!mime_type.is_empty()).then_some(mime_type)
}

fn url_extension(final_url: &str) -> Option<String> {
    reqwest::Url::parse(final_url)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
                .and_then(|segment| segment.rsplit_once('.').map(|(_, extension)| extension))
                .map(str::trim)
                .filter(|extension| !extension.is_empty())
                .map(str::to_ascii_lowercase)
        })
        .or_else(|| {
            final_url
                .split(['?', '#'])
                .next()
                .and_then(|path| path.rsplit('/').next())
                .and_then(|segment| segment.rsplit_once('.').map(|(_, extension)| extension))
                .map(str::trim)
                .filter(|extension| !extension.is_empty())
                .map(str::to_ascii_lowercase)
        })
}

fn spreadsheet_mime_type(source_format: &str) -> &'static str {
    match source_format {
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "xls" => "application/vnd.ms-excel",
        "xlsb" => "application/vnd.ms-excel.sheet.binary.macroenabled.12",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        _ => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    }
}

pub(super) fn fallback_title_from_url(final_url: &str) -> Option<String> {
    reqwest::Url::parse(final_url).ok().and_then(|url| {
        let path_title = url
            .path_segments()
            .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "index.html")
            .map(ToString::to_string);
        path_title.or_else(|| url.host_str().map(ToString::to_string))
    })
}
