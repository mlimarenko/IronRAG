//! Application boundary for the pinned native Xberg extraction process.
mod output;
mod runtime;
mod tables;

use std::{future::Future, process::ExitStatus};
use thiserror::Error;

use super::extraction_protocol::{Extraction, Operation};
use crate::shared::extraction::ExtractionOutput;

pub use self::output::validate_response;
pub use super::extraction_protocol::{CONTRACT, ENGINE, PRODUCER, PROFILE};
/// Includes the unknown-header-role projection of plain native PDF tables.
pub(crate) const TEXT_SEMANTICS: &str = "native-decoded/vision-literal/2";
pub(crate) const XBERG_WAITERS_PER_PROCESS: usize = 2;

#[derive(Clone, Copy)]
pub struct XbergSource<'a> {
    pub file_name: Option<&'a str>,
    pub mime_type: Option<&'a str>,
    pub source_format: &'a str,
}

#[derive(Debug, Error)]
pub enum XbergExtractionError {
    #[error("failed to create Xberg private disk workspace: {0}")]
    TempDir(std::io::Error),
    #[error("failed to write Xberg input: {0}")]
    WriteInput(std::io::Error),
    #[error("Xberg extractor is unavailable: {0}")]
    Spawn(std::io::Error),
    #[error("Xberg extraction timed out after {0}s")]
    Timeout(u64),
    #[error("Xberg extractor failed with status {status}: {stderr}")]
    ProcessFailed { status: ExitStatus, stderr: String },
    #[error("Xberg producer refused extraction ({error_code:?}): {message}")]
    ProducerRejected { error_code: Option<String>, message: String },
    #[error(
        "Xberg native extraction requires {required_mib} MiB plus {safety_margin_mib} MiB available; only {available_mib} MiB is available (limit {memory_limit_mib} MiB, used {current_rss_mib} MiB)"
    )]
    InsufficientMemory {
        memory_limit_mib: u64,
        current_rss_mib: u64,
        available_mib: u64,
        required_mib: u64,
        safety_margin_mib: u64,
    },
    #[error("Xberg returned invalid UTF-8: {0}")]
    InvalidUtf8(std::string::FromUtf8Error),
    #[error("Xberg returned invalid JSON: {0}")]
    InvalidJson(serde_json::Error),
    #[error("Xberg extracted no text")]
    EmptyOutput,
    #[error("Xberg concurrency limiter is closed")]
    LimiterClosed,
    #[error("Xberg permit wait timed out after {timeout_ms} ms")]
    AcquireTimeout { timeout_ms: u64 },
    #[error("Xberg page {page} failed: {source}")]
    PageExtractionFailed { page: u32, source: Box<Self> },
    #[error("Xberg contract rejected: {0}")]
    Contract(String),
    #[error("Xberg configuration rejected: {0}")]
    Configuration(String),
    #[error("Xberg supervisor failed: {0}")]
    Supervisor(String),
}

#[derive(Debug, Error)]
pub enum XbergBatchStreamError {
    #[error(transparent)]
    Extraction(#[from] XbergExtractionError),
    #[error("Xberg batch handler failed: {0}")]
    Batch(anyhow::Error),
}

impl XbergExtractionError {
    pub fn memory_failure_code(&self) -> Option<&'static str> {
        match self {
            Self::InsufficientMemory { .. } => Some("xberg_insufficient_memory"),
            Self::ProcessFailed { status, .. }
                if runtime::process_status_is_memory_kill(status) =>
            {
                Some("xberg_process_oom")
            }
            Self::PageExtractionFailed { source, .. } => source.memory_failure_code(),
            _ => None,
        }
    }
}

impl XbergBatchStreamError {
    pub fn memory_failure_code(&self) -> Option<&'static str> {
        match self {
            Self::Extraction(error) => error.memory_failure_code(),
            Self::Batch(_) => None,
        }
    }
}

pub struct XbergPageRangeExtraction {
    pub start_page: u32,
    pub end_page: u32,
    pub elapsed_ms: i64,
    pub extraction: Extraction,
}

pub(crate) fn default_xberg_acquire_timeout_secs() -> u64 {
    600
}
pub fn configured_max_concurrency() -> usize {
    1
}
pub fn configured_page_batch_size() -> u32 {
    runtime::page_batch_size()
}
pub async fn extract_native(
    source: XbergSource<'_>,
    bytes: Vec<u8>,
) -> Result<Extraction, XbergExtractionError> {
    runtime::invoke(source, bytes, Operation::Extract, None).await.map(|(payload, _)| payload)
}

pub fn project_native(
    payload: Extraction,
    source: XbergSource<'_>,
) -> Result<ExtractionOutput, XbergExtractionError> {
    output::build_output(payload, source)
}

pub async fn extract_pdf_page_count(
    source: XbergSource<'_>,
    bytes: &[u8],
) -> Result<Option<u32>, XbergExtractionError> {
    let (payload, _) = runtime::invoke(source, bytes.to_vec(), Operation::Count, None).await?;
    Ok(Some(payload.source_unit_count))
}

pub async fn extract_pdf_page_ranges_streamed<F, Fut>(
    source: XbergSource<'_>,
    bytes: &[u8],
    range: std::ops::RangeInclusive<u32>,
    batch_size: u32,
    mut on_batch: F,
) -> Result<(), XbergBatchStreamError>
where
    F: FnMut(XbergPageRangeExtraction) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let (start, end) = range.into_inner();
    if start == 0 || end < start || batch_size == 0 || batch_size > 64 {
        return Err(
            XbergExtractionError::Contract("invalid requested range/batch size".into()).into()
        );
    }
    // Each bounded range is a real native invocation. No line-delimited Python
    // envelope or fabricated per-page result survives the cutover.
    let mut cursor = start;
    loop {
        let last = cursor.saturating_add(batch_size - 1).min(end);
        let (payload, elapsed) =
            runtime::invoke(source, bytes.to_vec(), Operation::Extract, Some((cursor, last)))
                .await?;
        on_batch(XbergPageRangeExtraction {
            start_page: cursor,
            end_page: last,
            elapsed_ms: elapsed,
            extraction: payload,
        })
        .await
        .map_err(XbergBatchStreamError::Batch)?;
        if last == end {
            break;
        }
        cursor = last + 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
