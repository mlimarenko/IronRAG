//! Native, model-free byte extraction. Product projections remain in the library.
use ironrag_backend::integrations::extraction_protocol::{Extraction, Operation, Request, Unit};
use xberg::{ExtractInput, ExtractionConfig};

pub(super) fn mime(request: &Request, extension: &str) -> anyhow::Result<&'static str> {
    // Literal source must never reach XML tag stripping or a grammar downloader.
    match extension {
        "xml" | "svg" => return Ok("text/plain"),
        "md" | "markdown" => return Ok("text/markdown"),
        "rst" => return Ok("text/x-rst"),
        _ => {}
    }
    let essence = request.mime.split(';').next().unwrap_or_default().trim();
    Ok(match essence {
        "application/pdf" => "application/pdf",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation"
        }
        "image/png" => "image/png",
        "image/jpeg" => "image/jpeg",
        "image/gif" => "image/gif",
        "image/tiff" => "image/tiff",
        "image/bmp" => "image/bmp",
        "image/webp" => "image/webp",
        "text/html" | "application/xhtml+xml" => "text/html",
        "text/markdown" | "text/x-markdown" => "text/markdown",
        "text/x-rst" | "text/prs.fallenstein.rst" => "text/x-rst",
        "text/csv" | "application/csv" => "text/csv",
        "text/tab-separated-values" => "text/tab-separated-values",
        "application/vnd.ms-excel" => "application/vnd.ms-excel",
        "application/vnd.ms-excel.sheet.binary.macroEnabled.12" => {
            "application/vnd.ms-excel.sheet.binary.macroEnabled.12"
        }
        "application/vnd.oasis.opendocument.spreadsheet" => {
            "application/vnd.oasis.opendocument.spreadsheet"
        }
        "text/plain"
        | "application/xml"
        | "text/xml"
        | "image/svg+xml"
        | "application/x-source" => "text/plain",
        mime if mime.starts_with("text/") => "text/plain",
        _ => anyhow::bail!("unsupported_format"),
    })
}

pub(super) fn security_limits(request: &Request) -> xberg::extractors::security::SecurityLimits {
    xberg::extractors::security::SecurityLimits {
        max_content_size: request.limits.text_bytes,
        max_entity_length: request.limits.text_bytes,
        max_pages: Some(request.limits.units as usize),
        // Even empty cells consume serialized output and parsing work.
        max_table_cells: request.limits.output_bytes / 3,
        ..Default::default()
    }
}

pub(super) async fn extract(
    request: &Request,
    bytes: &[u8],
    mime: &str,
) -> anyhow::Result<Extraction> {
    let config = ExtractionConfig {
        use_cache: false,
        ocr: None,
        disable_ocr: true,
        enable_quality_processing: false,
        output_format: if mime == "text/plain" {
            xberg::OutputFormat::Plain
        } else {
            xberg::OutputFormat::Markdown
        },
        pages: Some(xberg::core::config::PageConfig { extract_pages: true, ..Default::default() }),
        security_limits: Some(security_limits(request)),
        ..Default::default()
    };
    // Fail invalid UTF-8 at the boundary instead of upstream's lossy decoding.
    if matches!(mime, "text/plain" | "text/csv" | "text/tab-separated-values") {
        std::str::from_utf8(bytes)?;
    }
    let html = if mime == "text/html" {
        Some(ironrag_backend::shared::extraction::html_main_content::prepare_html_main_content(
            bytes,
            Some(&request.mime),
        )?)
    } else {
        None
    };
    let input = html.as_ref().map_or(bytes, |html| html.source.as_bytes());
    let mut result = Box::pin(
        xberg::engine::Engine::new_default()
            .extract(ExtractInput::from_bytes(input.to_vec(), mime, None), &config),
    )
    .await?;
    anyhow::ensure!(result.errors.is_empty() && result.results.len() == 1, "native_engine_result");
    let document = result.results.pop().ok_or_else(|| anyhow::anyhow!("native_engine_empty"))?;
    // Excel can skip unreadable sheets and warn. Partial workbooks must fail,
    // never masquerade as complete successful extraction.
    for warning in &document.processing_warnings {
        let message = warning.message.to_ascii_lowercase();
        anyhow::ensure!(
            !message.contains("truncat")
                && !message.contains("skipped")
                && !message.contains("could not")
                && !message.contains("failed"),
            "native_incomplete: {}",
            warning.message
        );
    }
    let spreadsheet = mime.starts_with("application/vnd.");
    let mut units = if spreadsheet {
        let pages =
            document.pages.as_ref().ok_or_else(|| anyhow::anyhow!("sheet_pages_missing"))?;
        anyhow::ensure!(!pages.is_empty(), "workbook_has_no_sheets");
        pages
            .iter()
            .map(|page| {
                Ok(Unit {
                    ordinal: page.page_number,
                    sheet_name: page.sheet_name.clone(),
                    content: page.content.clone(),
                    tables: serde_json::to_value(&page.tables)?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?
    } else {
        // Literal source projection preserves whitespace and tags that upstream's
        // paragraph model intentionally normalizes. It performs no second parse.
        let content = if mime == "text/plain" {
            std::str::from_utf8(bytes)?.to_owned()
        } else {
            document.content.clone()
        };
        vec![Unit {
            ordinal: 1,
            sheet_name: None,
            content,
            tables: serde_json::to_value(&document.tables)?,
        }]
    };
    let total = u32::try_from(units.len())?;
    anyhow::ensure!(total <= request.limits.units, "unit_count_limit");
    anyhow::ensure!(units.iter().map(|u| u.ordinal).eq(1..=total), "native_unit_ordinals");
    let start = request.start.unwrap_or(1);
    let end = request.end.unwrap_or(total);
    anyhow::ensure!(start <= total && end <= total, "range_out_of_bounds");
    units.retain(|unit| (start..=end).contains(&unit.ordinal));
    let size = units
        .iter()
        .try_fold(0usize, |size, unit| {
            size.checked_add(unit.content.len()).and_then(|size| size.checked_add(2))
        })
        .ok_or_else(|| anyhow::anyhow!("content_limit"))?;
    anyhow::ensure!(size.saturating_sub(2) <= request.limits.text_bytes, "content_limit");
    let content = units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    Ok(Extraction {
        source_sha256: request.source_sha256.clone(),
        text_byte_limit: request.limits.text_bytes,
        source_unit_count: total,
        unit_kind: if spreadsheet { "sheet" } else { "document" }.into(),
        selected_start: start,
        selected_end: end,
        content: if request.operation == Operation::Count { String::new() } else { content },
        units: if request.operation == Operation::Count { Vec::new() } else { units },
        visual_inputs: Vec::new(),
        warnings: document
            .processing_warnings
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?,
    })
}
