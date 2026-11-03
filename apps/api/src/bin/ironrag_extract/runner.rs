//! Native structure and bounded visual bytes. Recognition belongs to parent Vision.
use std::io::{Cursor, Read};

use base64::Engine as _;
use sha2::{Digest, Sha256};
use xberg::core::config::{ConcurrencyConfig, ImageExtractionConfig, PageConfig, PdfConfig};
use xberg::{ExtractInput, ExtractionConfig};

use super::input;
use ironrag_backend::integrations::extraction_protocol::{
    DisplayExtent, Extraction, Operation, Request, Unit, VisualInput, VisualInputKind,
};

pub(super) fn extract_native(
    request: Request,
) -> impl std::future::Future<Output = anyhow::Result<Extraction>> {
    // Keep native parsing state off callers' async stacks, including unit tests.
    Box::pin(extract_units(request))
}

async fn extract_units(request: Request) -> anyhow::Result<Extraction> {
    request.validate()?;
    let bytes = input::read(&request.path, request.limits.input_bytes as usize)?;
    anyhow::ensure!(
        hex::encode(Sha256::digest(&bytes)) == request.source_sha256,
        "source_digest_mismatch"
    );
    let extension =
        request.path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let mime = super::general::mime(&request, &extension)?;
    let unit_kind = match mime {
        "application/pdf" => "page",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "document",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "slide",
        mime if mime.starts_with("image/") => "frame",
        _ => return Box::pin(super::general::extract(&request, &bytes, mime)).await,
    };
    let total = source_unit_count(&request, &bytes, mime, unit_kind)?;
    anyhow::ensure!((1..=request.limits.units).contains(&total), "unit_count_limit");
    let start = request.start.unwrap_or(1);
    let end = request.end.unwrap_or(total);
    anyhow::ensure!(start <= total && end <= total, "range_out_of_bounds");
    let sliced = unit_kind == "page" || mime == "image/tiff";
    anyhow::ensure!(
        sliced || (start == 1 && end == total),
        "selected_range_unsupported_for_format"
    );
    let mut output = Extraction {
        source_sha256: request.source_sha256.clone(),
        text_byte_limit: request.limits.text_bytes,
        source_unit_count: total,
        unit_kind: unit_kind.into(),
        selected_start: start,
        selected_end: end,
        content: String::new(),
        units: Vec::new(),
        visual_inputs: Vec::new(),
        warnings: Vec::new(),
    };
    // Inspection only: no raster decode, rendering session, assets or inference.
    if request.operation == Operation::Count {
        return Ok(output);
    }
    if unit_kind == "frame" {
        return extract_frames(&request, &bytes, mime, output);
    }
    let mut visual_bytes = 0usize;
    let pdf = if unit_kind == "page" { Some(input::pdf(&bytes)?) } else { None };
    // Open the public pinned renderer lazily, only when native structure has no text.
    let mut renderer = None;
    let config = ExtractionConfig {
        security_limits: Some(super::general::security_limits(&request)),
        use_cache: false,
        ocr: None,
        disable_ocr: true,
        enable_quality_processing: false,
        include_document_structure: true,
        output_format: xberg::OutputFormat::Markdown,
        images: Some(ImageExtractionConfig {
            extract_images: true,
            inject_placeholders: false,
            target_dpi: 72,
            auto_adjust_dpi: false,
            run_ocr_on_images: false,
            ..Default::default()
        }),
        pdf_options: Some(PdfConfig {
            extract_images: true,
            extract_tables: true,
            ocr_inline_images: false,
            ..Default::default()
        }),
        pages: Some(PageConfig { extract_pages: true, ..Default::default() }),
        concurrency: Some(ConcurrencyConfig { max_threads: Some(1), max_concurrent_ocr: Some(1) }),
        ..Default::default()
    };
    config.validate()?;
    let engine = xberg::engine::Engine::new_default();
    for ordinal in if sliced { start..=end } else { 1..=1 } {
        let data = input::unit_bytes(&bytes, pdf.as_ref(), total, mime, ordinal)?;
        let mut result =
            Box::pin(engine.extract(ExtractInput::from_bytes(data, mime, None), &config)).await?;
        anyhow::ensure!(
            result.errors.is_empty() && result.results.len() == 1,
            "native_engine_result"
        );
        let mut document =
            result.results.pop().ok_or_else(|| anyhow::anyhow!("native_engine_empty"))?;
        append_native_units(&mut document, &mut output, ordinal)?;
        if unit_kind == "page"
            && output.units.last().is_some_and(|unit| unit.content.trim().is_empty())
        {
            input::pdf_render_pixels(
                pdf.as_ref().ok_or_else(|| anyhow::anyhow!("pdf_missing"))?,
                ordinal,
                96,
            )?;
            if renderer.is_none() {
                let session = xberg::pdf::PdfRenderSession::open(&bytes, None)?;
                anyhow::ensure!(
                    session.page_count() == total as usize,
                    "render_page_count_mismatch"
                );
                renderer = Some(session);
            }
            let data = renderer
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("renderer_missing"))?
                .render_page_to_png((ordinal - 1) as usize, Some(96))?;
            // No placeholder content and no embedded inputs alongside the same full page.
            output
                .units
                .last_mut()
                .ok_or_else(|| anyhow::anyhow!("page_unit_missing"))?
                .content
                .clear();
            append_visual(
                data,
                NativeVisual {
                    kind: VisualInputKind::PdfPage,
                    native_index: ordinal - 1,
                    source_unit: ordinal,
                    source_path: None,
                    mime: "image/png".into(),
                    display_extent_96_dpi: None,
                },
                &request,
                &mut visual_bytes,
                &mut output,
            )?;
            continue;
        }
        let mut office_package = if unit_kind == "document" {
            Some(zip::ZipArchive::new(Cursor::new(bytes.as_slice()))?)
        } else {
            None
        };
        for image in document.images.take().unwrap_or_default() {
            let source_unit = if unit_kind == "slide" {
                image
                    .page_number
                    .filter(|page| (1..=total).contains(page))
                    .ok_or_else(|| anyhow::anyhow!("picture_slide_out_of_bounds"))?
            } else {
                ordinal
            };
            let source_path = match office_package.as_mut() {
                Some(package) => Some(docx_source_path(
                    package,
                    image.source_path.as_deref(),
                    image.data.as_ref(),
                    request.limits.output_bytes,
                )?),
                None => image.source_path,
            };
            let extent = (unit_kind != "page")
                .then_some(image.width.zip(image.height))
                .flatten()
                .filter(|(width, height)| *width > 0 && *height > 0)
                .map(|(width, height)| DisplayExtent { width, height });
            append_visual(
                image.data,
                NativeVisual {
                    kind: VisualInputKind::EmbeddedPicture,
                    native_index: image.image_index,
                    source_unit,
                    source_path,
                    mime: format!(
                        "image/{}",
                        image.format.to_ascii_lowercase().replace("jpg", "jpeg")
                    ),
                    display_extent_96_dpi: extent,
                },
                &request,
                &mut visual_bytes,
                &mut output,
            )?;
        }
    }
    // Stable source-unit ordering also covers Office image inventories.
    output.visual_inputs.sort_by_key(|visual| (visual.source_unit, visual.native_index));
    for (ordinal, visual) in output.visual_inputs.iter_mut().enumerate() {
        visual.ordinal = ordinal;
    }
    finalize(output, &request)
}

fn append_native_units(
    document: &mut xberg::ExtractedDocument,
    output: &mut Extraction,
    ordinal: u32,
) -> anyhow::Result<()> {
    let native_text_units = native_text_units(
        document.document.as_ref().ok_or_else(|| anyhow::anyhow!("native_structure_missing"))?,
        &output.unit_kind,
        ordinal,
        output.source_unit_count,
    )?;
    anyhow::ensure!(document.content.len() <= output.text_byte_limit, "content_limit");
    output.warnings.extend(
        document
            .processing_warnings
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<Vec<_>, _>>()?,
    );
    if output.unit_kind == "slide" {
        let pages =
            document.pages.as_ref().ok_or_else(|| anyhow::anyhow!("slide_pages_missing"))?;
        anyhow::ensure!(
            pages.len() == output.source_unit_count as usize
                && pages.iter().map(|page| page.page_number).eq(1..=output.source_unit_count),
            "slide_count_or_ordinal_mismatch"
        );
        let mut tables_by_slide = std::collections::BTreeMap::<_, Vec<_>>::new();
        for table in &document.tables {
            anyhow::ensure!(
                (1..=output.source_unit_count).contains(&table.page_number),
                "table_slide_out_of_bounds"
            );
            tables_by_slide.entry(table.page_number).or_default().push(table);
        }
        for page in pages {
            output.units.push(Unit {
                ordinal: page.page_number,
                sheet_name: None,
                content: if native_text_units.contains(&page.page_number) {
                    page.content.clone()
                } else {
                    String::new()
                },
                // Native PPTX tables live on the document, with archive-derived
                // physical slide identities even when a slide has no title.
                tables: serde_json::to_value(
                    tables_by_slide.remove(&page.page_number).unwrap_or_default(),
                )?,
            });
        }
    } else {
        anyhow::ensure!(
            output.unit_kind != "page" || document.counts.pages == 1,
            "single_unit_engine_count_mismatch"
        );
        for table in &mut document.tables {
            table.page_number = ordinal;
        }
        let native_content = native_text_units.contains(&ordinal)
            || document
                .tables
                .iter()
                .any(|table| table.cells.iter().flatten().any(|cell| !cell.trim().is_empty()));
        output.units.push(Unit {
            ordinal,
            sheet_name: None,
            content: if native_content { document.content.clone() } else { String::new() },
            tables: serde_json::to_value(&document.tables)?,
        });
    }
    Ok(())
}

/// DOCX paths identify verified archive-root members resolved from native relationship
/// targets relative to word/document.xml. PPTX has no native path and retains None;
/// its identity is source_unit, native_index and the exact payload digest, not a guessed member.
fn docx_source_path(
    package: &mut zip::ZipArchive<Cursor<&[u8]>>,
    native_target: Option<&str>,
    payload: &[u8],
    byte_limit: usize,
) -> anyhow::Result<String> {
    let name = office_member_path(
        "word",
        native_target
            .filter(|path| !path.is_empty())
            .ok_or_else(|| anyhow::anyhow!("office_picture_path_missing"))?,
    )?;
    anyhow::ensure!(payload.len() <= byte_limit, "office_member_bytes_limit");
    let mut member = package.by_name(&name)?;
    anyhow::ensure!(member.size() == payload.len() as u64, "office_picture_member_bytes_mismatch");
    let mut bytes = Vec::new();
    (&mut member).take(payload.len() as u64 + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes == payload, "office_picture_member_bytes_mismatch");
    Ok(name)
}

fn office_member_path(base: &str, target: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        target.len() <= 4096 && !target.chars().any(char::is_control),
        "invalid_office_relationship_target"
    );
    // Match pinned Xberg's OPC resolution: slash-rooted targets use the package
    // root; in-bounds parent components are valid; percent escapes are not decoded.
    let target = target.replace('\\', "/");
    anyhow::ensure!(
        !target.starts_with("//") && !target.contains(':'),
        "invalid_office_relationship_target"
    );
    let mut components = if target.starts_with('/') {
        Vec::new()
    } else {
        base.split('/').filter(|component| !component.is_empty()).collect::<Vec<_>>()
    };
    for component in target.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                anyhow::ensure!(components.pop().is_some(), "office_relationship_escapes_package");
            }
            _ => components.push(component),
        }
    }
    anyhow::ensure!(!components.is_empty(), "office_relationship_has_no_member");
    Ok(components.join("/"))
}

fn source_unit_count(
    request: &Request,
    bytes: &[u8],
    mime: &str,
    unit_kind: &str,
) -> anyhow::Result<u32> {
    match unit_kind {
        "page" => Ok(u32::try_from(xberg::pdf_page_count(bytes, None)?)?),
        "slide" => slide_count(bytes),
        "frame" if mime == "image/tiff" => input::tiff_count(bytes, request.limits.units),
        "frame" => {
            input::inspect_raster(bytes)?;
            Ok(1)
        }
        _ => {
            let archive = zip::ZipArchive::new(Cursor::new(bytes))?;
            anyhow::ensure!(archive.len() <= 4096, "office_entry_limit");
            anyhow::ensure!(
                archive.file_names().any(|name| name.is_ok_and(|name| name == "word/document.xml")),
                "office_document_missing"
            );
            Ok(1)
        }
    }
}

fn extract_frames(
    request: &Request,
    bytes: &[u8],
    mime: &str,
    mut output: Extraction,
) -> anyhow::Result<Extraction> {
    let mut visual_bytes = 0usize;
    for ordinal in output.selected_start..=output.selected_end {
        let data = input::unit_bytes(bytes, None, output.source_unit_count, mime, ordinal)?;
        output.units.push(Unit {
            ordinal,
            sheet_name: None,
            content: String::new(),
            tables: serde_json::json!([]),
        });
        append_visual(
            data,
            NativeVisual {
                kind: VisualInputKind::RasterFrame,
                native_index: ordinal - 1,
                source_unit: ordinal,
                source_path: None,
                mime: if mime == "image/tiff" { "image/png" } else { mime }.into(),
                display_extent_96_dpi: None,
            },
            request,
            &mut visual_bytes,
            &mut output,
        )?;
    }
    finalize(output, request)
}

fn native_text_units(
    structure: &xberg::types::DocumentStructure,
    unit_kind: &str,
    ordinal: u32,
    total: u32,
) -> anyhow::Result<std::collections::HashSet<u32>> {
    let mut units = std::collections::HashSet::new();
    for node in &structure.nodes {
        let has_text = node.content.text().is_some_and(|text| !text.trim().is_empty())
            || matches!(&node.content, xberg::types::NodeContent::Table { grid }
                if grid.cells.iter().any(|cell| !cell.content.trim().is_empty()));
        if has_text {
            let source_unit = if unit_kind == "slide" {
                node.page
                    .filter(|page| (1..=total).contains(page))
                    .ok_or_else(|| anyhow::anyhow!("native_text_slide_association_missing"))?
            } else {
                ordinal
            };
            units.insert(source_unit);
        }
    }
    Ok(units)
}

struct NativeVisual {
    kind: VisualInputKind,
    native_index: u32,
    source_unit: u32,
    source_path: Option<String>,
    mime: String,
    display_extent_96_dpi: Option<DisplayExtent>,
}

fn append_visual(
    data: impl AsRef<[u8]>,
    source: NativeVisual,
    request: &Request,
    visual_bytes: &mut usize,
    output: &mut Extraction,
) -> anyhow::Result<()> {
    let NativeVisual { kind, native_index, source_unit, source_path, mime, display_extent_96_dpi } =
        source;
    let data = data.as_ref();
    anyhow::ensure!(output.visual_inputs.len() < 128, "visual_count_limit");
    *visual_bytes = visual_bytes
        .checked_add(data.len())
        .ok_or_else(|| anyhow::anyhow!("visual_bytes_overflow"))?;
    anyhow::ensure!(*visual_bytes <= request.limits.output_bytes, "visual_bytes_limit");
    let raster = input::raster(data)?;
    output.visual_inputs.push(VisualInput {
        kind,
        ordinal: output.visual_inputs.len(),
        native_index,
        source_unit,
        source_path,
        mime,
        width: raster.width(),
        height: raster.height(),
        display_extent_96_dpi,
        payload_sha256: hex::encode(Sha256::digest(data)),
        content_base64: base64::engine::general_purpose::STANDARD.encode(data),
    });
    Ok(())
}

fn finalize(mut output: Extraction, request: &Request) -> anyhow::Result<Extraction> {
    anyhow::ensure!(
        output.units.len() == (output.selected_end - output.selected_start + 1) as usize,
        "output_unit_count_mismatch"
    );
    anyhow::ensure!(
        output
            .units
            .iter()
            .map(|unit| unit.ordinal)
            .eq(output.selected_start..=output.selected_end),
        "output_unit_order_mismatch"
    );
    let size = output
        .units
        .iter()
        .try_fold(0usize, |size, unit| size.checked_add(unit.content.len()))
        .and_then(|size| size.checked_add(output.units.len().saturating_sub(1) * 2))
        .ok_or_else(|| anyhow::anyhow!("aggregate_content_limit"))?;
    anyhow::ensure!(size <= request.limits.text_bytes, "aggregate_content_limit");
    output.content =
        output.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    Ok(output)
}

fn slide_count(bytes: &[u8]) -> anyhow::Result<u32> {
    let archive = zip::ZipArchive::new(Cursor::new(bytes))?;
    anyhow::ensure!(archive.len() <= 4096, "office_entry_limit");
    let mut numbers = Vec::new();
    for name in archive.file_names() {
        let name = name?;
        if let Some(number) =
            name.strip_prefix("ppt/slides/slide").and_then(|n| n.strip_suffix(".xml"))
        {
            numbers.push(number.parse::<u32>()?);
        }
    }
    numbers.sort_unstable();
    anyhow::ensure!(
        !numbers.is_empty() && numbers.iter().copied().eq(1..=numbers.len() as u32),
        "slide_parts_not_contiguous"
    );
    Ok(u32::try_from(numbers.len())?)
}

#[cfg(test)]
mod office_member_tests {
    use super::*;
    use std::io::Write;

    fn package(entries: &[(&str, &[u8])]) -> anyhow::Result<Vec<u8>> {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in entries {
            archive.start_file(*name, zip::write::SimpleFileOptions::default())?;
            archive.write_all(bytes)?;
        }
        Ok(archive.finish()?.into_inner())
    }

    #[test]
    fn docx_relationship_resolves_to_the_verified_package_member() -> anyhow::Result<()> {
        for (target, member) in [
            ("media/image1.png", "word/media/image1.png"),
            ("../media/image1.png", "media/image1.png"),
            ("/media/image1.png", "media/image1.png"),
            ("media\\image1.png", "word/media/image1.png"),
        ] {
            let bytes = package(&[(member, b"native image bytes")])?;
            let mut archive = zip::ZipArchive::new(Cursor::new(bytes.as_slice()))?;
            assert_eq!(
                docx_source_path(&mut archive, Some(target), b"native image bytes", 4096)?,
                member
            );
            assert!(
                docx_source_path(&mut archive, Some(target), b"forged image bytes", 4096).is_err()
            );
            assert!(
                docx_source_path(&mut archive, Some(target), b"native image bytes", 1).is_err()
            );
        }
        Ok(())
    }

    #[test]
    fn office_relationship_cannot_escape_the_package() -> anyhow::Result<()> {
        let bytes = package(&[("outside.png", b"native image bytes")])?;
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes.as_slice()))?;
        for target in [
            "../../outside.png",
            "/../outside.png",
            "C:/outside.png",
            "//server/outside.png",
            "..\\..\\outside.png",
            "",
        ] {
            assert!(
                docx_source_path(&mut archive, Some(target), b"native image bytes", 4096).is_err(),
                "target {target}"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "pptx_tests.rs"]
mod pptx_tests;
