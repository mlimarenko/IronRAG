use super::{CONTRACT, ENGINE, PRODUCER, PROFILE, XbergExtractionError as Error, XbergSource};
use crate::{
    integrations::extraction_protocol::{
        Extraction, MAX_PIXELS, Operation, Request, Response, VisualInputKind,
    },
    shared::extraction::{
        ExtractionLineSignal, ExtractionOutput, ExtractionSourceMetadata, RawExtractionPage,
        build_text_layout, infer_line_signals,
    },
};
use base64::Engine as _;
use sha2::{Digest, Sha256};

fn require(condition: bool, message: &str) -> Result<(), Error> {
    if !condition {
        return Err(Error::Contract(message.to_owned()));
    }
    Ok(())
}

fn validate_response_identity(response: &Response) -> Result<(), Error> {
    require(
        response.contract == CONTRACT
            && response.producer == PRODUCER
            && response.engine == ENGINE
            && response.profile == PROFILE,
        "producer contract/engine/profile identity mismatch",
    )
}

pub(super) fn response_failure(response: Response) -> Error {
    if let Err(error) = validate_response_identity(&response) {
        return error;
    }
    let Some(message) = response.error else {
        return Error::Contract("failed process omitted its error".into());
    };
    if response.status != "error"
        || response.result.is_some()
        || message.len() > 16384
        || message.chars().count() > 4096
        || response.error_code.as_ref().is_some_and(|code| {
            code.is_empty()
                || code.len() > 128
                || !code.is_ascii()
                || code.chars().any(char::is_control)
        })
    {
        return Error::Contract("failed process returned an invalid error envelope".into());
    }
    Error::ProducerRejected { error_code: response.error_code, message }
}

/// Validates child identity, source association, visual inventory and byte budgets.
///
/// # Errors
/// Returns an extraction contract error for mismatched or malformed producer
/// output. This does not classify the original source bytes as invalid.
pub fn validate_response(response: Response, request: &Request) -> Result<Extraction, Error> {
    request.validate().map_err(|error| Error::Contract(error.to_string()))?;
    validate_response_identity(&response)?;
    require(
        serde_json::to_vec(&response).map_err(Error::InvalidJson)?.len()
            <= request.limits.output_bytes,
        "response byte budget exceeded",
    )?;
    require(
        response.status == "success" && response.error.is_none() && response.error_code.is_none(),
        "unsuccessful response",
    )?;
    let mut output = response.result.ok_or_else(|| Error::Contract("missing result".into()))?;
    require(output.source_sha256 == request.source_sha256, "source digest mismatch")?;
    require(output.text_byte_limit == request.limits.text_bytes, "admitted text budget mismatch")?;
    require(
        output.source_unit_count > 0 && output.source_unit_count <= request.limits.units,
        "source unit count exceeds admission budget",
    )?;
    let start = request.start.unwrap_or(1);
    let end = request.end.unwrap_or(output.source_unit_count);
    require(
        start > 0
            && end >= start
            && end <= output.source_unit_count
            && output.selected_start == start
            && output.selected_end == end,
        "selected range mismatch",
    )?;
    require(
        matches!(output.unit_kind.as_str(), "page" | "slide" | "frame" | "document" | "sheet"),
        "unknown source unit kind",
    )?;
    let expected_kind = match request.mime.split(';').next().unwrap_or_default().trim() {
        "application/pdf" => Some("page"),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some("document")
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some("slide")
        }
        "image/svg+xml" => Some("document"),
        mime if mime.starts_with("image/") => Some("frame"),
        _ => None,
    };
    require(
        expected_kind.is_none_or(|kind| output.unit_kind == kind),
        "source unit kind differs from requested MIME",
    )?;
    if request.operation == Operation::Count {
        require(
            output.units.is_empty() && output.content.is_empty() && output.visual_inputs.is_empty(),
            "count returned extraction content",
        )?;
        return Ok(output);
    }
    require(
        end - start < request.limits.units && output.units.len() == (end - start + 1) as usize,
        "unit cardinality exceeds budget or selected range",
    )?;
    require(
        output.units.iter().map(|unit| unit.ordinal).eq(start..=end),
        "unit ordering mismatch",
    )?;
    if output.unit_kind == "sheet" {
        let mut names = std::collections::HashSet::new();
        require(
            output.units.iter().all(|unit| {
                unit.sheet_name.as_deref().is_some_and(|name| {
                    !name.trim().is_empty() && name.len() <= 1024 && names.insert(name)
                })
            }),
            "sheet identity missing or duplicated",
        )?;
    }
    let length = output
        .units
        .iter()
        .try_fold(0usize, |size, unit| size.checked_add(unit.content.len()))
        .and_then(|size| size.checked_add(output.units.len().saturating_sub(1) * 2))
        .ok_or_else(|| Error::Contract("content length overflow".into()))?;
    require(
        length <= request.limits.text_bytes && output.content.len() <= request.limits.text_bytes,
        "content budget exceeded",
    )?;
    let content =
        output.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    require(output.content == content, "canonical content differs from ordered units")?;
    validate_visual_inputs(&output, request)?;
    if matches!(
        request.mime.split(';').next().unwrap_or_default().trim(),
        "application/pdf"
            | "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            | "application/vnd.openxmlformats-officedocument.presentationml.presentation"
    ) {
        // Only the producer's native Markdown is transport-encoded. Decode it
        // before raw Vision transcriptions are joined into these units.
        for unit in &mut output.units {
            unit.content = decode_native_markdown_transport(&unit.content);
        }
        output.content =
            output.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    }
    Ok(output)
}

fn decode_native_markdown_transport(content: &str) -> String {
    if !content.contains('\\') && !content.contains('&') {
        return content.to_owned();
    }
    content
        .split_inclusive('\n')
        .map(|line| {
            if infer_line_signals(line).contains(&ExtractionLineSignal::TableRow) {
                // The shared cell decoder owns table escapes and entities once.
                line.to_owned()
            } else {
                decode_native_markdown_text(line)
            }
        })
        .collect()
}

fn decode_native_markdown_text(content: &str) -> String {
    let mut unescaped = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\\'
            && let Some(next) = chars.peek().copied()
            && matches!(
                next,
                '\\' | '`'
                    | '*'
                    | '_'
                    | '{'
                    | '}'
                    | '['
                    | ']'
                    | '('
                    | ')'
                    | '#'
                    | '+'
                    | '-'
                    | '.'
                    | '!'
                    | '|'
                    | '<'
                    | '>'
            )
        {
            unescaped.push(next);
            chars.next();
        } else {
            unescaped.push(character);
        }
    }
    // Decode once: &amp;quot; is literal &quot;, not a quotation mark.
    if !unescaped.contains('&') {
        return unescaped;
    }
    let mut output = String::with_capacity(unescaped.len());
    let mut remaining = unescaped.as_str();
    while let Some(index) = remaining.find('&') {
        output.push_str(&remaining[..index]);
        remaining = &remaining[index..];
        let entity =
            [("&gt;", '>'), ("&lt;", '<'), ("&amp;", '&'), ("&quot;", '"'), ("&#39;", '\'')]
                .into_iter()
                .find(|(encoded, _)| remaining.starts_with(encoded));
        if let Some((encoded, decoded)) = entity {
            output.push(decoded);
            remaining = &remaining[encoded.len()..];
        } else {
            output.push('&');
            remaining = &remaining[1..];
        }
    }
    output.push_str(remaining);
    output
}

fn validate_visual_inputs(output: &Extraction, request: &Request) -> Result<(), Error> {
    require(output.visual_inputs.len() <= 128, "visual input count exceeded")?;
    let mut bytes_total = 0usize;
    let mut identities = std::collections::HashSet::new();
    let mut full_pages = std::collections::HashSet::new();
    let mut picture_pages = std::collections::HashSet::new();
    let mut previous = None;
    for (ordinal, visual) in output.visual_inputs.iter().enumerate() {
        require(visual.ordinal == ordinal, "visual input ordering mismatch")?;
        require(
            (output.selected_start..=output.selected_end).contains(&visual.source_unit),
            "visual association outside selected source units",
        )?;
        let key = (visual.source_unit, visual.native_index);
        require(
            previous.is_none_or(|previous| previous <= key) && identities.insert(key),
            "visual inventory is reordered or duplicated",
        )?;
        previous = Some(key);
        require(
            visual.width > 0
                && visual.height > 0
                && u64::from(visual.width) * u64::from(visual.height) <= MAX_PIXELS,
            "visual pixel budget exceeded",
        )?;
        match visual.kind {
            VisualInputKind::PdfPage => {
                require(
                    output.unit_kind == "page"
                        && visual.native_index == visual.source_unit - 1
                        && visual.source_path.is_none()
                        && visual.mime == "image/png"
                        && visual.display_extent_96_dpi.is_none()
                        && full_pages.insert(visual.source_unit),
                    "invalid full-page visual identity",
                )?;
                let unit = output.units.iter().find(|unit| unit.ordinal == visual.source_unit);
                require(
                    unit.is_some_and(|unit| unit.content.trim().is_empty()),
                    "full-page visual duplicates native text",
                )?;
            }
            VisualInputKind::RasterFrame => {
                require(
                    output.unit_kind == "frame"
                        && visual.native_index == visual.source_unit - 1
                        && visual.source_path.is_none()
                        && visual.display_extent_96_dpi.is_none()
                        && full_pages.insert(visual.source_unit),
                    "invalid raster-frame identity",
                )?;
            }
            VisualInputKind::EmbeddedPicture => {
                require(
                    matches!(output.unit_kind.as_str(), "page" | "document" | "slide"),
                    "embedded picture has invalid source kind",
                )?;
                picture_pages.insert(visual.source_unit);
                if output.unit_kind == "document" {
                    require(
                        visual.source_unit == 1 && visual.source_path.is_some(),
                        "Office picture has no source path",
                    )?;
                }
                if output.unit_kind == "page" {
                    require(
                        visual.display_extent_96_dpi.is_none(),
                        "PDF picture has Office extent",
                    )?;
                }
            }
        }
        if let Some(path) = &visual.source_path {
            require(
                !path.is_empty()
                    && path.len() <= 4096
                    && !path.chars().any(char::is_control)
                    && !path.starts_with('/')
                    && !path.contains('\\')
                    && path.split('/').all(|part| !matches!(part, "" | "." | "..")),
                "invalid visual source path",
            )?;
        }
        if let Some(extent) = &visual.display_extent_96_dpi {
            require(extent.width > 0 && extent.height > 0, "invalid display extent")?;
        }
        require(
            matches!(
                visual.mime.as_str(),
                "image/png"
                    | "image/jpeg"
                    | "image/webp"
                    | "image/bmp"
                    | "image/tiff"
                    | "image/gif"
            ),
            "invalid visual MIME",
        )?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&visual.content_base64)
            .map_err(|_| Error::Contract("invalid visual base64".into()))?;
        require(
            base64::engine::general_purpose::STANDARD.encode(&bytes) == visual.content_base64,
            "noncanonical visual base64",
        )?;
        require(
            visual.payload_sha256 == hex::encode(Sha256::digest(&bytes)),
            "visual payload digest mismatch",
        )?;
        bytes_total = bytes_total
            .checked_add(bytes.len())
            .ok_or_else(|| Error::Contract("visual length overflow".into()))?;
        require(bytes_total <= request.limits.output_bytes, "visual byte budget exceeded")?;
        let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .map_err(|_| Error::Contract("invalid visual encoding".into()))?;
        require(
            reader.format().is_some_and(|format| format.to_mime_type() == visual.mime),
            "visual MIME differs from encoded bytes",
        )?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(visual.width);
        limits.max_image_height = Some(visual.height);
        limits.max_alloc = Some(MAX_PIXELS * 8);
        reader.limits(limits);
        let raster =
            reader.decode().map_err(|_| Error::Contract("invalid visual raster".into()))?;
        require(
            (raster.width(), raster.height()) == (visual.width, visual.height),
            "visual dimensions differ from encoded bytes",
        )?;
    }
    require(full_pages.is_disjoint(&picture_pages), "full page and embedded pictures overlap")?;
    if output.unit_kind == "page" {
        require(
            output
                .units
                .iter()
                .all(|unit| !unit.content.trim().is_empty() || full_pages.contains(&unit.ordinal)),
            "textless PDF page has no full-page visual",
        )?;
    }
    if output.unit_kind == "frame" {
        require(full_pages.len() == output.units.len(), "raster frame inventory incomplete")?;
    }
    Ok(())
}

pub(super) fn build_output(
    payload: Extraction,
    source: XbergSource<'_>,
) -> Result<ExtractionOutput, Error> {
    let aggregate =
        payload.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    require(
        payload.content == aggregate,
        "aggregate content differs from ordered units before projection",
    )?;
    let paged = matches!(payload.unit_kind.as_str(), "page" | "slide" | "frame");
    let mut projected_bytes = 0_usize;
    let pages = payload
        .units
        .iter()
        .map(|unit| {
            let page_number =
                if paged {
                    Some(i32::try_from(unit.ordinal).map_err(|_| {
                        Error::Contract("source unit exceeds page-number range".into())
                    })?)
                } else {
                    None
                };
            let separator = if projected_bytes == 0 { 0 } else { 2 };
            let lines: Vec<String> = if source.source_format == "pdf" {
                super::tables::project_pdf_table_lines(
                    &unit.content,
                    payload
                        .text_byte_limit
                        .saturating_sub(projected_bytes)
                        .saturating_sub(separator),
                )?
            } else {
                unit.content
                    .replace("\r\n", "\n")
                    .replace('\r', "\n")
                    .split('\n')
                    .map(str::to_owned)
                    .collect()
            };
            projected_bytes = projected_bytes
                .saturating_add(separator)
                .saturating_add(lines.len().saturating_sub(1));
            for line in &lines {
                projected_bytes = projected_bytes.saturating_add(line.len());
            }
            require(
                projected_bytes <= payload.text_byte_limit,
                "projected content budget exceeded",
            )?;
            Ok(RawExtractionPage { page_number, lines })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let layout = build_text_layout(&pages);
    require(
        layout.content_text.len() <= payload.text_byte_limit,
        "projected content budget exceeded",
    )?;
    let page_count = matches!(payload.unit_kind.as_str(), "page" | "slide" | "frame")
        .then_some(payload.source_unit_count);
    // Vision has already joined its results into the ordered native units.
    // Persist identity only; encoded bytes never re-enter generic image recognition.
    let source_map = serde_json::json!({
        "adapter":"xberg", "contract":CONTRACT, "producer":PRODUCER, "engine":ENGINE, "profile":PROFILE,
        "source_sha256":payload.source_sha256, "text_byte_limit":payload.text_byte_limit,
        "text_semantics":super::TEXT_SEMANTICS, "input_file_name":source.file_name, "mime_type":source.mime_type,
        "source_format":source.source_format, "unit_kind":payload.unit_kind, "selected_start":payload.selected_start, "selected_end":payload.selected_end,
        "units":payload.units.iter().map(|unit| serde_json::json!({"ordinal":unit.ordinal,"sheet_name":unit.sheet_name,"content_sha256":hex::encode(Sha256::digest(unit.content.as_bytes())),"tables":unit.tables})).collect::<Vec<_>>(),
        "visual_inputs":payload.visual_inputs.iter().map(|p| serde_json::json!({"kind":p.kind,"ordinal":p.ordinal,"native_index":p.native_index,"source_unit":p.source_unit,"source_path":p.source_path,"mime":p.mime,"width":p.width,"height":p.height,"display_extent_96_dpi":p.display_extent_96_dpi,"payload_sha256":p.payload_sha256})).collect::<Vec<_>>()
    });
    Ok(ExtractionOutput {
        extraction_kind: "xberg_markdown".into(),
        content_text: layout.content_text,
        page_count,
        warnings: payload
            .warnings
            .into_iter()
            .map(|warning| {
                warning.as_str().map(str::to_owned).unwrap_or_else(|| warning.to_string())
            })
            .collect(),
        source_metadata: ExtractionSourceMetadata {
            source_format: source.source_format.into(),
            page_count,
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map,
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images: Vec::new(),
    })
}
