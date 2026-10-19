use super::*;

use crate::shared::admission::acquire_permit_within;

pub(super) async fn acquire_docling_permit() -> Result<OwnedSemaphorePermit, DoclingExtractionError>
{
    acquire_permit_within(
        DOCLING_CONCURRENCY.clone(),
        Duration::from_secs(docling_acquire_timeout_secs()),
        "docling",
    )
    .await
    .map_err(DoclingExtractionError::from)
}

/// Bounded wait for one Docling slot. Override with
/// `IRONRAG_DOCLING_ACQUIRE_TIMEOUT_SECS`; the default outlasts a full queue of
/// maximum-length extractions.
fn docling_acquire_timeout_secs() -> u64 {
    std::env::var("IRONRAG_DOCLING_ACQUIRE_TIMEOUT_SECS")
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(default_docling_acquire_timeout_secs)
}

/// Default bounded wait for one Docling slot: `DOCLING_WAITERS_PER_PROCESS`
/// maximum-length extractions (2 × 900 s = 1800 s at the defaults). The ingest
/// worker reads this same default for its heavy-revision pipeline slot.
pub(crate) fn default_docling_acquire_timeout_secs() -> u64 {
    default_docling_acquire_timeout_secs_for(docling_timeout_secs())
}

/// Pure form of [`default_docling_acquire_timeout_secs`] for a given extractor
/// timeout, so the queue multiple is testable without the environment.
pub(crate) fn default_docling_acquire_timeout_secs_for(timeout_secs: u64) -> u64 {
    u64::try_from(DOCLING_WAITERS_PER_PROCESS).unwrap_or(u64::MAX).saturating_mul(timeout_secs)
}

pub(super) fn write_input_file(
    temp_dir: &TempDir,
    source: DoclingSource<'_>,
    file_bytes: &[u8],
) -> Result<PathBuf, DoclingExtractionError> {
    let DoclingSource { file_name, mime_type, source_format } = source;
    let file_name = normalized_input_file_name(file_name, mime_type, source_format);
    let input_path = temp_dir.path().join(file_name);
    std::fs::write(&input_path, file_bytes).map_err(DoclingExtractionError::WriteInput)?;
    Ok(input_path)
}

pub(super) async fn run_docling(
    input_path: &Path,
    extra_args: &[&str],
) -> Result<DoclingExtractionPayload, DoclingExtractionError> {
    prepare_docling_invocation()?;

    let output = run_docling_raw(input_path, extra_args).await?;
    serde_json::from_slice(&output.stdout).map_err(DoclingExtractionError::InvalidJson)
}

/// Runs the pre-extraction memory gate and then logs the current process RSS so
/// operators can correlate OOM kills with pre-extraction baseline memory usage.
fn prepare_docling_invocation() -> Result<(), DoclingExtractionError> {
    ensure_docling_process_memory_headroom()?;
    log_docling_rss_before_extraction();
    Ok(())
}

fn log_docling_rss_before_extraction() {
    if let Some(rss_mb) = current_rss_mb() {
        if rss_mb > 3000 {
            warn_docling_rss_high(rss_mb);
        } else {
            info_docling_rss(rss_mb);
        }
    }
}

fn warn_docling_rss_high(rss_mb: u64) {
    tracing::warn!(rss_mb, "docling: worker RSS is high before extraction — OOM likely");
}

fn info_docling_rss(rss_mb: u64) {
    tracing::info!(rss_mb, "docling: worker RSS before extraction");
}

/// Queries the page count of a PDF via `--page-count`. Returns `None`
/// when the extractor reports that page counting is unsupported.
pub(super) async fn run_docling_page_count(
    input_path: &Path,
) -> Result<Option<u32>, DoclingExtractionError> {
    ensure_docling_process_memory_headroom()?;
    let output = run_docling_raw(input_path, &["--page-count"]).await?;
    let payload: DoclingPageCountPayload =
        serde_json::from_slice(&output.stdout).map_err(DoclingExtractionError::InvalidJson)?;
    Ok(payload.page_count)
}

/// Calls `--pages START-END` to extract a range of pages in a single
/// Python process, reusing the loaded `RapidOCR` model across pages.
pub(super) async fn run_docling_batch(
    input_path: &Path,
    start_page: u32,
    end_page: u32,
) -> Result<DoclingBatchPayload, DoclingExtractionError> {
    ensure_docling_process_memory_headroom()?;
    let range = format!("{start_page}-{end_page}");
    let output = run_docling_raw(input_path, &["--pages", &range]).await?;
    serde_json::from_slice(&output.stdout).map_err(DoclingExtractionError::InvalidJson)
}

pub(super) async fn run_docling_page_batches<F, Fut>(
    input_path: &Path,
    source: DoclingSource<'_>,
    start_page: u32,
    end_page: u32,
    batch_size: u32,
    mut on_batch: F,
) -> Result<(), DoclingBatchStreamError>
where
    F: FnMut(DoclingPageRangeExtraction) -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    ensure_docling_process_memory_headroom()?;

    let DoclingSource { file_name, mime_type, source_format } = source;
    let timeout_secs = docling_timeout_secs();
    let range = format!("{start_page}-{end_page}");
    let mut command = Command::new(docling_extract_bin());
    command
        .arg("--page-batches")
        .arg(batch_size.to_string())
        .arg(range)
        .arg(input_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(DoclingExtractionError::Spawn)?;
    let stdout = child.stdout.take().ok_or_else(|| {
        DoclingExtractionError::Spawn(std::io::Error::other("docling batch stdout was not piped"))
    })?;
    let stderr_task = child.stderr.take().map(|mut stderr| {
        tokio::spawn(async move {
            let mut bytes = Vec::new();
            let _ = stderr.read_to_end(&mut bytes).await;
            bytes
        })
    });

    let mut lines = BufReader::new(stdout).lines();
    while let Some(line) = timeout(Duration::from_secs(timeout_secs), lines.next_line())
        .await
        .map_err(|_| {
            let _ = child.start_kill();
            DoclingExtractionError::Timeout(timeout_secs)
        })?
        .map_err(DoclingExtractionError::Spawn)?
    {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut payload: DoclingBatchPayload =
            serde_json::from_str(line).map_err(DoclingExtractionError::InvalidJson)?;
        let (range_start, range_end) =
            parse_page_range(payload.page_range.as_deref()).map_err(|message| {
                DoclingExtractionError::PaginatedMergeFailed(format!(
                    "docling streamed batch has invalid pageRange: {message}"
                ))
            })?;
        let merged_payload = merge_batch_payload(&mut payload, range_start, range_end);
        let elapsed_ms = merged_payload
            .timings
            .get("totalSeconds")
            .and_then(serde_json::Value::as_f64)
            .map(|seconds| (seconds * 1000.0).round() as i64)
            .unwrap_or_default();
        let output = build_output(merged_payload, file_name, mime_type, source_format)?;
        on_batch(DoclingPageRangeExtraction {
            start_page: range_start,
            end_page: range_end,
            elapsed_ms,
            output,
        })
        .await
        .map_err(DoclingBatchStreamError::Batch)?;
    }

    let status = child.wait().await.map_err(DoclingExtractionError::Spawn)?;
    let stderr_bytes = match stderr_task {
        Some(task) => task.await.unwrap_or_default(),
        None => Vec::new(),
    };
    if !status.success() {
        let stderr =
            String::from_utf8(stderr_bytes).map_err(DoclingExtractionError::InvalidUtf8)?;
        return Err(DoclingExtractionError::ProcessFailed {
            status,
            stderr: truncate_for_error(&stderr),
        }
        .into());
    }

    Ok(())
}

/// Low-level docling invocation returning raw output for custom parsing.
async fn run_docling_raw(
    input_path: &Path,
    extra_args: &[&str],
) -> Result<std::process::Output, DoclingExtractionError> {
    let timeout_secs = docling_timeout_secs();
    let mut command = Command::new(docling_extract_bin());
    for arg in extra_args {
        command.arg(arg);
    }
    command.arg(input_path).kill_on_drop(true);

    let output = timeout(Duration::from_secs(timeout_secs), command.output())
        .await
        .map_err(|_| DoclingExtractionError::Timeout(timeout_secs))?
        .map_err(DoclingExtractionError::Spawn)?;

    if !output.status.success() {
        let stderr =
            String::from_utf8(output.stderr).map_err(DoclingExtractionError::InvalidUtf8)?;
        return Err(DoclingExtractionError::ProcessFailed {
            status: output.status,
            stderr: truncate_for_error(&stderr),
        });
    }

    Ok(output)
}

fn parse_page_range(raw: Option<&str>) -> Result<(u32, u32), String> {
    let raw = raw.ok_or_else(|| "missing pageRange".to_string())?;
    let (start, end) =
        raw.split_once('-').ok_or_else(|| format!("expected START-END, got {raw}"))?;
    let start_page = start.parse::<u32>().map_err(|_| format!("invalid start page {start}"))?;
    let end_page = end.parse::<u32>().map_err(|_| format!("invalid end page {end}"))?;
    if start_page == 0 || end_page < start_page {
        return Err(format!("invalid page range {raw}"));
    }
    Ok((start_page, end_page))
}

pub(super) fn page_batch_size() -> u32 {
    std::env::var("IRONRAG_DOCLING_PAGE_BATCH_SIZE")
        .ok()
        .and_then(|raw| raw.parse::<u32>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_PAGE_BATCH_SIZE)
}

/// Returns the current process RSS in megabytes, or `None` if the platform
/// does not support `/proc/self/statm` (non-Linux).
fn current_rss_mb() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
        let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
        // statm reports pages; typical page size is 4 KiB.
        Some(pages * 4 / 1024)
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn ensure_docling_process_memory_headroom() -> Result<(), DoclingExtractionError> {
    let Some(memory_limit_bytes) = telemetry::detect_container_memory_limit_bytes() else {
        return Ok(());
    };
    docling_process_memory_headroom_for_limits(memory_limit_bytes, current_rss_mb()).map(|_| ())
}

pub(super) fn docling_process_memory_headroom_for_limits(
    memory_limit_bytes: u64,
    current_rss_mib: Option<u64>,
) -> Result<u64, DoclingExtractionError> {
    let memory_limit_mib = memory_limit_bytes / (1024 * 1024);
    let current_rss_mib = current_rss_mib.unwrap_or_default();
    let available_mib = memory_limit_mib.saturating_sub(current_rss_mib);
    let required_with_margin =
        DOCLING_AUTO_MEMORY_PER_PROCESS_MIB.saturating_add(DOCLING_PROCESS_SAFETY_MARGIN_MIB);
    if available_mib < required_with_margin {
        return Err(DoclingExtractionError::InsufficientMemory {
            memory_limit_mib,
            current_rss_mib,
            available_mib,
            required_mib: DOCLING_AUTO_MEMORY_PER_PROCESS_MIB,
            safety_margin_mib: DOCLING_PROCESS_SAFETY_MARGIN_MIB,
        });
    }
    Ok(available_mib)
}

pub(super) fn process_status_is_memory_kill(status: &ExitStatus) -> bool {
    #[cfg(target_family = "unix")]
    {
        status.signal() == Some(9)
    }
    #[cfg(not(target_family = "unix"))]
    {
        let _ = status;
        false
    }
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

pub(super) fn resolve_docling_max_concurrency() -> usize {
    let raw = std::env::var("IRONRAG_DOCLING_MAX_CONCURRENCY").ok();
    match raw.as_deref().map(str::trim) {
        Some(value) if value.eq_ignore_ascii_case("auto") || value == "0" || value.is_empty() => {
            auto_docling_max_concurrency()
        }
        Some(value) => {
            if let Some(value) = value.parse::<usize>().ok().filter(|value| *value > 0) {
                value
            } else {
                tracing::warn!(
                    raw = value,
                    "invalid IRONRAG_DOCLING_MAX_CONCURRENCY; using automatic docling concurrency"
                );
                auto_docling_max_concurrency()
            }
        }
        None => auto_docling_max_concurrency(),
    }
}

fn auto_docling_max_concurrency() -> usize {
    let cpu_parallelism = telemetry::detect_container_cpu_parallelism().unwrap_or(1);
    let memory_limit_bytes = telemetry::detect_container_memory_limit_bytes();
    let concurrency = auto_docling_max_concurrency_for_limits(cpu_parallelism, memory_limit_bytes);
    let memory_limit_mib = memory_limit_bytes.map(|bytes| bytes / (1024 * 1024));
    let soft_limit_mib = memory_limit_mib.map(|mib| mib.saturating_mul(9) / 10);
    let docling_budget_mib =
        soft_limit_mib.map(|mib| mib.saturating_sub(DOCLING_AUTO_RESERVED_MEMORY_MIB));
    log_auto_docling_concurrency(
        cpu_parallelism,
        memory_limit_mib,
        soft_limit_mib,
        docling_budget_mib,
        concurrency,
    );
    concurrency
}

fn log_auto_docling_concurrency(
    cpu_parallelism: usize,
    memory_limit_mib: Option<u64>,
    soft_limit_mib: Option<u64>,
    docling_budget_mib: Option<u64>,
    concurrency: usize,
) {
    tracing::info!(
        cpu_parallelism,
        ?memory_limit_mib,
        ?soft_limit_mib,
        ?docling_budget_mib,
        reserved_mib = DOCLING_AUTO_RESERVED_MEMORY_MIB,
        per_process_mib = DOCLING_AUTO_MEMORY_PER_PROCESS_MIB,
        max_concurrency = DOCLING_AUTO_MAX_CONCURRENCY,
        concurrency,
        "docling auto concurrency resolved"
    );
    if docling_budget_mib.is_some_and(|budget| budget < DOCLING_AUTO_MEMORY_PER_PROCESS_MIB) {
        warn_auto_docling_concurrency_memory_floor(
            memory_limit_mib,
            soft_limit_mib,
            docling_budget_mib,
        );
    }
}

fn warn_auto_docling_concurrency_memory_floor(
    memory_limit_mib: Option<u64>,
    soft_limit_mib: Option<u64>,
    docling_budget_mib: Option<u64>,
) {
    tracing::warn!(
        ?memory_limit_mib,
        ?soft_limit_mib,
        ?docling_budget_mib,
        required_mib = DOCLING_AUTO_MEMORY_PER_PROCESS_MIB,
        "docling auto concurrency has only enough memory budget for the mandatory single process"
    );
}

pub(super) fn auto_docling_max_concurrency_for_limits(
    cpu_parallelism: usize,
    memory_limit_bytes: Option<u64>,
) -> usize {
    // One Docling process is internally CPU-heavy (Torch/RapidOCR); reserve
    // roughly half of the worker CPU quota for the Rust pipeline, embeddings,
    // health checks, and query traffic while extraction is running.
    let cpu_bound = cpu_parallelism.max(1).saturating_div(2).max(1);
    let memory_bound =
        memory_limit_bytes.map(|bytes| bytes / (1024 * 1024)).map_or(1, |memory_mib| {
            let soft_limit_mib = memory_mib.saturating_mul(9) / 10;
            soft_limit_mib
                .saturating_sub(DOCLING_AUTO_RESERVED_MEMORY_MIB)
                .checked_div(DOCLING_AUTO_MEMORY_PER_PROCESS_MIB)
                .unwrap_or(0) as usize
        });

    cpu_bound.min(memory_bound.max(1)).clamp(1, DOCLING_AUTO_MAX_CONCURRENCY)
}

pub(super) fn normalized_input_file_name(
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
