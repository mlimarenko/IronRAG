use std::{fs, path::Path, process::Command};

use anyhow::{Context, Result, anyhow};
use lopdf::{Document, Object, ObjectId};

use crate::shared::extraction::{
    ExtractedImage, ExtractionOutput, ExtractionSourceMetadata, RawExtractionPage,
    build_text_layout,
};

/// Minimum consecutive spaces to consider as a column separator in table detection.
const TABLE_DETECTION_MIN_SPACES: usize = 3;
/// Minimum rows required to recognize a table.
const TABLE_MIN_ROWS: usize = 2;
/// Minimum columns required to recognize a table.
const TABLE_MIN_COLUMNS: usize = 2;

#[derive(Debug)]
struct PopplerPdfExtraction {
    pages: Vec<RawExtractionPage>,
    page_count: Option<u32>,
}

pub fn extract_pdf(file_bytes: &[u8]) -> Result<ExtractionOutput> {
    let mut warnings = Vec::new();
    let (pages, page_numbers, page_count) = match Document::load_mem(file_bytes) {
        Ok(document) => {
            let pages = document.get_pages();
            let page_numbers = pages.keys().copied().collect::<Vec<_>>();
            let page_count = Some(u32::try_from(page_numbers.len()).unwrap_or(u32::MAX));
            if page_numbers.is_empty() {
                (Vec::new(), Vec::new(), page_count)
            } else {
                match extract_pdf_pages_with_lopdf(&document, &page_numbers) {
                    Ok(extracted_pages) => (extracted_pages, page_numbers, page_count),
                    Err(primary_error) => {
                        let fallback = extract_pdf_with_poppler(file_bytes).with_context(|| {
                            format!(
                                "failed to extract pdf text with lopdf and poppler fallback: {primary_error:#}",
                            )
                        })?;
                        warnings.push(format!(
                            "lopdf extraction failed; used pdftotext fallback ({primary_error})"
                        ));
                        (fallback.pages, page_numbers, fallback.page_count.or(page_count))
                    }
                }
            }
        }
        Err(load_error) => {
            let fallback = extract_pdf_with_poppler(file_bytes).with_context(|| {
                format!(
                    "failed to extract pdf text with lopdf and poppler fallback: {load_error:#}"
                )
            })?;
            let page_numbers = fallback
                .page_count
                .map(|count| (1..=count).collect::<Vec<_>>())
                .unwrap_or_default();
            warnings.push(format!(
                "lopdf could not parse the pdf structure; used pdftotext fallback ({load_error})"
            ));
            (fallback.pages, page_numbers, fallback.page_count)
        }
    };
    let pages = apply_table_heuristics(pages);
    let layout = build_text_layout(&pages);

    let extracted_images = match Document::load_mem(file_bytes) {
        Ok(document) => {
            let images = extract_pdf_images(&document);
            if !images.is_empty() {
                tracing::info!(count = images.len(), "extracted images from pdf");
            }
            images
        }
        Err(_) => Vec::new(),
    };

    Ok(ExtractionOutput {
        extraction_kind: "pdf_text".into(),
        content_text: layout.content_text,
        page_count,
        warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: "pdf".to_string(),
            page_count,
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "pages": page_numbers,
            "extracted_image_count": extracted_images.len(),
        }),
        provider_kind: None,
        model_name: None,
        extracted_images,
    })
}

fn extract_pdf_pages_with_lopdf(
    document: &Document,
    page_numbers: &[u32],
) -> Result<Vec<RawExtractionPage>> {
    page_numbers
        .iter()
        .copied()
        .map(|page_number| {
            let page_text = document
                .extract_text(&[page_number])
                .with_context(|| format!("failed to extract pdf page {page_number}"))?;
            Ok(RawExtractionPage {
                page_number: Some(i32::try_from(page_number).unwrap_or(i32::MAX)),
                lines: split_pdf_page_lines(&page_text),
            })
        })
        .collect()
}

fn extract_pdf_with_poppler(file_bytes: &[u8]) -> Result<PopplerPdfExtraction> {
    let tempdir = tempfile::tempdir().context("failed to create tempdir for pdftotext")?;
    let pdf_path = tempdir.path().join("document.pdf");
    fs::write(&pdf_path, file_bytes).context("failed to write temp pdf for pdftotext")?;
    let page_count = extract_pdf_page_count_with_pdfinfo(&pdf_path);
    let pages = extract_pdf_pages_with_poppler(&pdf_path, page_count)?;

    Ok(PopplerPdfExtraction { pages, page_count })
}

fn extract_pdf_pages_with_poppler(
    pdf_path: &Path,
    page_count: Option<u32>,
) -> Result<Vec<RawExtractionPage>> {
    if let Some(page_count) = page_count {
        let mut pages = Vec::with_capacity(usize::try_from(page_count).unwrap_or(0));
        for page_number in 1..=page_count {
            let output = Command::new("pdftotext")
                .arg("-layout")
                .arg("-f")
                .arg(page_number.to_string())
                .arg("-l")
                .arg(page_number.to_string())
                .arg(pdf_path)
                .arg("-")
                .output()
                .with_context(|| format!("failed to spawn pdftotext for page {page_number}"))?;
            if !output.status.success() {
                return Err(anyhow!(
                    "pdftotext exited with status {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
            pages.push(RawExtractionPage {
                page_number: Some(i32::try_from(page_number).unwrap_or(i32::MAX)),
                lines: split_pdf_page_lines(&String::from_utf8_lossy(&output.stdout)),
            });
        }
        return Ok(pages);
    }

    let output = Command::new("pdftotext")
        .arg("-layout")
        .arg(pdf_path)
        .arg("-")
        .output()
        .context("failed to spawn pdftotext")?;
    if !output.status.success() {
        return Err(anyhow!(
            "pdftotext exited with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(vec![RawExtractionPage {
        page_number: Some(1),
        lines: split_pdf_page_lines(&String::from_utf8_lossy(&output.stdout)),
    }])
}

fn extract_pdf_page_count_with_pdfinfo(pdf_path: &Path) -> Option<u32> {
    let output = Command::new("pdfinfo").arg(pdf_path).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).lines().find_map(|line| {
        let (label, value) = line.split_once(':')?;
        (label.trim() == "Pages").then(|| value.trim().parse::<u32>().ok()).flatten()
    })
}

/// Extracts embedded images from a PDF document by traversing page XObject resources.
fn extract_pdf_images(document: &Document) -> Vec<ExtractedImage> {
    let mut images = Vec::new();
    let pages = document.get_pages();
    let mut skipped_count: usize = 0;

    for (&page_number, &page_id) in &pages {
        match extract_images_from_page(
            document,
            page_id,
            page_number,
            &mut images,
            &mut skipped_count,
        ) {
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(stage = "pdf_images", page = page_number, error = %error, "page image extraction failed, continuing");
            }
        }
    }

    tracing::info!(
        stage = "pdf_images",
        images_extracted = images.len(),
        images_skipped = skipped_count,
        "PDF image extraction complete"
    );

    images
}

fn extract_images_from_page(
    document: &Document,
    page_id: ObjectId,
    page_number: u32,
    images: &mut Vec<ExtractedImage>,
    skipped_count: &mut usize,
) -> Result<()> {
    let page_obj = document.get_object(page_id).context("page object not found")?;
    let page_dict = page_obj.as_dict().map_err(|_| anyhow!("page object is not a dictionary"))?;

    let resources_ref = match page_dict.get(b"Resources") {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let (_, resources_obj) = match document.dereference(resources_ref) {
        Ok(pair) => pair,
        Err(_) => return Ok(()),
    };
    let resources = match resources_obj.as_dict() {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };

    let xobj_ref = match resources.get(b"XObject") {
        Ok(r) => r,
        Err(_) => return Ok(()),
    };
    let (_, xobj_obj) = match document.dereference(xobj_ref) {
        Ok(pair) => pair,
        Err(_) => return Ok(()),
    };
    let xobjects = match xobj_obj.as_dict() {
        Ok(d) => d,
        Err(_) => return Ok(()),
    };

    for (_name, xobj_ref) in xobjects.iter() {
        let resolved = match document.dereference(xobj_ref) {
            Ok((_, obj)) => obj,
            Err(_) => continue,
        };
        let stream = match resolved.as_stream() {
            Ok(stream) => stream,
            Err(_) => continue,
        };

        let subtype = stream
            .dict
            .get(b"Subtype")
            .ok()
            .and_then(|v| v.as_name().ok())
            .map(|n| std::str::from_utf8(n).unwrap_or_default().to_string())
            .unwrap_or_default();

        if subtype != "Image" {
            continue;
        }

        let width =
            stream.dict.get(b"Width").ok().and_then(|v| v.as_i64().ok()).unwrap_or(0) as u32;
        let height =
            stream.dict.get(b"Height").ok().and_then(|v| v.as_i64().ok()).unwrap_or(0) as u32;

        if width == 0 || height == 0 {
            tracing::debug!(
                stage = "pdf_images",
                page = page_number,
                reason = "zero_dimensions",
                "image skipped"
            );
            *skipped_count += 1;
            continue;
        }

        let filter = stream
            .dict
            .get(b"Filter")
            .ok()
            .and_then(|v| match v {
                Object::Name(name) => std::str::from_utf8(name).ok().map(str::to_string),
                Object::Array(arr) => arr.first().and_then(|item| {
                    if let Object::Name(name) = item {
                        std::str::from_utf8(name).ok().map(str::to_string)
                    } else {
                        None
                    }
                }),
                _ => None,
            })
            .unwrap_or_default();

        let raw_bytes = match stream.decompressed_content() {
            Ok(bytes) => bytes,
            Err(_) => stream.content.clone(),
        };

        if raw_bytes.is_empty() {
            continue;
        }

        let (image_bytes, mime_type) = match filter.as_str() {
            "DCTDecode" => (raw_bytes, "image/jpeg".to_string()),
            "JPXDecode" => (raw_bytes, "image/jp2".to_string()),
            _ => {
                let bits_per_component = stream
                    .dict
                    .get(b"BitsPerComponent")
                    .ok()
                    .and_then(|v| v.as_i64().ok())
                    .unwrap_or(8) as u32;
                let color_space = stream
                    .dict
                    .get(b"ColorSpace")
                    .ok()
                    .and_then(|v| match v {
                        Object::Name(name) => std::str::from_utf8(name).ok().map(str::to_string),
                        Object::Array(arr) => arr.first().and_then(|item| {
                            if let Object::Name(name) = item {
                                std::str::from_utf8(name).ok().map(str::to_string)
                            } else {
                                None
                            }
                        }),
                        _ => None,
                    })
                    .unwrap_or_else(|| "DeviceRGB".to_string());

                match reconstruct_png_from_raw(
                    &raw_bytes,
                    width,
                    height,
                    bits_per_component,
                    &color_space,
                ) {
                    Some(png_bytes) => (png_bytes, "image/png".to_string()),
                    None => {
                        tracing::debug!(
                            stage = "pdf_images",
                            page = page_number,
                            reason = "unsupported_colorspace",
                            "image skipped"
                        );
                        *skipped_count += 1;
                        continue;
                    }
                }
            }
        };

        images.push(ExtractedImage {
            page: usize::try_from(page_number).unwrap_or(0),
            image_bytes,
            mime_type,
            width,
            height,
        });
    }

    Ok(())
}

fn reconstruct_png_from_raw(
    raw_bytes: &[u8],
    width: u32,
    height: u32,
    bits_per_component: u32,
    color_space: &str,
) -> Option<Vec<u8>> {
    let channels: u32 = match color_space {
        "DeviceGray" | "CalGray" => 1,
        "DeviceRGB" | "CalRGB" => 3,
        "DeviceCMYK" => 4,
        _ => 3,
    };

    if bits_per_component != 8 {
        return None;
    }

    let expected_len = (width * height * channels) as usize;
    if raw_bytes.len() < expected_len {
        return None;
    }

    let pixel_bytes = if color_space == "DeviceCMYK" {
        let mut rgb = Vec::with_capacity((width * height * 3) as usize);
        for pixel in raw_bytes[..expected_len].chunks_exact(4) {
            let c = f32::from(pixel[0]) / 255.0;
            let m = f32::from(pixel[1]) / 255.0;
            let y = f32::from(pixel[2]) / 255.0;
            let k = f32::from(pixel[3]) / 255.0;
            rgb.push(((1.0 - c) * (1.0 - k) * 255.0) as u8);
            rgb.push(((1.0 - m) * (1.0 - k) * 255.0) as u8);
            rgb.push(((1.0 - y) * (1.0 - k) * 255.0) as u8);
        }
        rgb
    } else {
        raw_bytes[..expected_len].to_vec()
    };

    let color_type = match color_space {
        "DeviceGray" | "CalGray" => image::ColorType::L8,
        _ => image::ColorType::Rgb8,
    };

    let mut png_buf = std::io::Cursor::new(Vec::new());
    image::write_buffer_with_format(
        &mut png_buf,
        &pixel_bytes,
        width,
        height,
        color_type,
        image::ImageFormat::Png,
    )
    .ok()?;
    Some(png_buf.into_inner())
}

/// Applies table-detection heuristics to extracted PDF pages.
///
/// If a sequence of lines shows consistent multi-column alignment (text segments
/// separated by 3+ spaces), the lines are wrapped in a markdown table.
fn apply_table_heuristics(pages: Vec<RawExtractionPage>) -> Vec<RawExtractionPage> {
    pages
        .into_iter()
        .map(|page| RawExtractionPage {
            page_number: page.page_number,
            lines: detect_and_format_tables(page.lines),
        })
        .collect()
}

fn detect_and_format_tables(lines: Vec<String>) -> Vec<String> {
    let mut result = Vec::with_capacity(lines.len());
    let mut table_buffer: Vec<Vec<String>> = Vec::new();
    let mut in_table = false;

    for line in &lines {
        let columns = split_tabular_columns(line);
        let is_tabular = columns.len() >= 2;

        if is_tabular {
            if !in_table {
                in_table = true;
                table_buffer.clear();
            }
            table_buffer.push(columns);
        } else {
            if in_table {
                flush_table_buffer(&table_buffer, &mut result);
                table_buffer.clear();
                in_table = false;
            }
            result.push(line.clone());
        }
    }

    if in_table {
        flush_table_buffer(&table_buffer, &mut result);
    }

    result
}

/// Splits a line into columns based on runs of 3+ whitespace characters.
fn split_tabular_columns(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut columns = Vec::new();
    let mut current = String::new();
    let mut space_run = 0usize;

    for ch in trimmed.chars() {
        if ch == ' ' {
            space_run += 1;
        } else {
            if space_run >= TABLE_DETECTION_MIN_SPACES && !current.trim().is_empty() {
                columns.push(current.trim().to_string());
                current = String::new();
            } else {
                for _ in 0..space_run {
                    current.push(' ');
                }
            }
            space_run = 0;
            current.push(ch);
        }
    }
    if !current.trim().is_empty() {
        columns.push(current.trim().to_string());
    }

    columns
}

fn flush_table_buffer(rows: &[Vec<String>], output: &mut Vec<String>) {
    if rows.len() < TABLE_MIN_ROWS {
        for row in rows {
            output.push(row.join("   "));
        }
        return;
    }

    let max_cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    if max_cols < TABLE_MIN_COLUMNS {
        for row in rows {
            output.push(row.join("   "));
        }
        return;
    }

    for (row_index, row) in rows.iter().enumerate() {
        let mut padded = row.clone();
        padded.resize(max_cols, String::new());
        output.push(format!("| {} |", padded.join(" | ")));
        if row_index == 0 {
            let separator = (0..max_cols).map(|_| "---").collect::<Vec<_>>();
            output.push(format!("| {} |", separator.join(" | ")));
        }
    }
}

fn split_pdf_page_lines(content: &str) -> Vec<String> {
    content
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| line.trim_end().to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use lopdf::{
        Document, Object, Stream,
        content::{Content, Operation},
        dictionary,
    };

    use super::*;

    const POPPLER_FALLBACK_PDF: &[u8] = br"%PDF-1.4
1 0 obj
<< /Type /Catalog /Pages 2 0 R >>
endobj
2 0 obj
<< /Type /Pages /Kids [3 0 R] /Count 1 >>
endobj
3 0 obj
<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>
endobj
4 0 obj
<< /Length 67 >>
stream
BT
/F1 18 Tf
72 720 Td
(Runtime PDF upload check) Tj
0 -24 Td
(Quarterly graph report) Tj
ET
endstream
endobj
5 0 obj
<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>
endobj
xref
0 6
0000000000 65535 f 
0000000009 00000 n 
0000000058 00000 n 
0000000115 00000 n 
0000000241 00000 n 
0000000359 00000 n 
trailer
<< /Size 6 /Root 1 0 R >>
startxref
429
%%EOF
";

    fn build_minimal_pdf_bytes() -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages_id = document.new_object_id();
        let single_page_id = document.new_object_id();
        let font_id = document.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources_id = document.add_object(dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        });
        let content = Content {
            operations: vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), Object::Integer(14)]),
                Operation::new("Td", vec![Object::Integer(72), Object::Integer(720)]),
                Operation::new("Tj", vec![Object::string_literal("Quarterly graph report")]),
                Operation::new("ET", vec![]),
            ],
        };
        let encoded = content.encode().expect("encode pdf stream");
        let content_id = document.add_object(Stream::new(dictionary! {}, encoded));
        document.objects.insert(
            single_page_id,
            Object::Dictionary(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
                "Resources" => resources_id,
                "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
            }),
        );
        document.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Kids" => vec![single_page_id.into()],
                "Count" => 1,
            }),
        );
        let catalog_id = document.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        document.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).expect("save pdf");
        bytes
    }

    #[test]
    fn extracts_text_and_page_map_from_minimal_pdf() {
        let output = extract_pdf(&build_minimal_pdf_bytes()).expect("pdf extraction");

        assert_eq!(output.extraction_kind, "pdf_text");
        assert_eq!(output.page_count, Some(1));
        assert!(output.content_text.contains("Quarterly graph report"));
        assert_eq!(output.source_map["pages"], serde_json::json!([1]));
        assert!(output.structure_hints.lines.iter().any(|line| line.page_number == Some(1)));
    }

    #[test]
    fn falls_back_to_poppler_for_readable_non_lopdf_pdf() {
        let output =
            extract_pdf(POPPLER_FALLBACK_PDF).expect("pdf extraction with poppler fallback");

        assert_eq!(output.page_count, Some(1));
        assert!(output.content_text.contains("Runtime PDF upload check"));
        assert!(output.warnings.iter().any(|warning| warning.contains("pdftotext fallback")));
        assert_eq!(output.source_map["pages"], serde_json::json!([1]));
    }
}
