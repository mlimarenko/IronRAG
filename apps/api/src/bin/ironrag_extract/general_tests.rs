//! Deterministic native-route regressions; no model paths, registry, or downloads.
use std::io::{Cursor, Write};

use ironrag_backend::integrations::extraction_protocol::{Limits, Operation, Request};
use sha2::{Digest, Sha256};

fn request(path: std::path::PathBuf, mime: &str, bytes: &[u8]) -> Request {
    Request {
        path,
        mime: mime.into(),
        source_sha256: hex::encode(Sha256::digest(bytes)),
        operation: Operation::Extract,
        start: None,
        end: None,
        limits: Limits::default(),
    }
}

#[test]
fn native_child_cli_does_not_require_ocr_assets() -> anyhow::Result<()> {
    use clap::Parser as _;
    super::Arguments::try_parse_from(["ironrag-extract"])?;
    Ok(())
}

#[tokio::test]
async fn gif_preserves_original_picture_without_loading_models() -> anyhow::Result<()> {
    use base64::Engine as _;

    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.gif");
    let image = image::DynamicImage::ImageRgb8(image::RgbImage::new(2, 2));
    let mut encoded = Cursor::new(Vec::new());
    image.write_to(&mut encoded, image::ImageFormat::Gif)?;
    let bytes = encoded.into_inner();
    std::fs::write(&path, &bytes)?;
    let output = super::runner::extract_native(request(path, "image/gif", &bytes)).await?;
    assert_eq!(output.source_unit_count, 1);
    assert_eq!(output.visual_inputs.len(), 1);
    assert_eq!(output.visual_inputs[0].mime, "image/gif");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&output.visual_inputs[0].content_base64)?,
        bytes
    );
    Ok(())
}

#[tokio::test]
async fn literal_source_retains_tags_comments_and_whitespace_without_models() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    for (name, mime, source) in [
        ("source.rs", "text/plain", "// keep comment\nfn main() {\n    let x = \"<tag>\";\n}\n"),
        (
            "source.xml",
            "application/xml",
            "<?xml version=\"1.0\"?>\n<!-- comment -->\n<root a=\"1\">  literal </root>\n",
        ),
        ("source.svg", "image/svg+xml", "<svg><!-- source --><text>literal</text></svg>\n"),
    ] {
        let path = directory.path().join(name);
        std::fs::write(&path, source)?;
        let input = request(path, mime, source.as_bytes());
        let output = super::runner::extract_native(input).await?;
        assert_eq!(output.content, source);
    }
    Ok(())
}

#[tokio::test]
async fn markdown_rst_and_selected_html_use_native_conversion() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, mime, source) in [
        ("source.md", "text/markdown", "# Native heading\n\nNative **body**.\n"),
        ("source.rst", "text/x-rst", "Native heading\n==============\n\nNative body.\n"),
        (
            "source.html",
            "text/html",
            "<nav>FOREIGN_NAV</nav><main><h1>Native heading</h1><p>Native body.</p></main>",
        ),
    ] {
        let path = directory.path().join(name);
        std::fs::write(&path, source)?;
        let output = super::runner::extract_native(request(path, mime, source.as_bytes())).await?;
        assert!(output.content.contains("Native heading"));
        assert!(output.content.contains("body"));
        assert!(!output.content.contains("FOREIGN_NAV"));
    }
    Ok(())
}

#[tokio::test]
async fn public_delimited_cells_keep_headers_quotes_and_empty_cells() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    for (name, mime, bytes, expected) in [
        (
            "source.csv",
            "text/csv",
            b"name,empty,note\n42,,\"quoted, value\"\n".as_slice(),
            "quoted, value",
        ),
        (
            "source.tsv",
            "text/tab-separated-values",
            b"name\tempty\tnote\n42\t\tvalue\n".as_slice(),
            "value",
        ),
    ] {
        let path = directory.path().join(name);
        std::fs::write(&path, bytes)?;
        let output = super::runner::extract_native(request(path, mime, bytes)).await?;
        let tables: Vec<xberg::types::Table> =
            serde_json::from_value(output.units[0].tables.clone())?;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].cells[0], ["name", "empty", "note"]);
        assert_eq!(tables[0].cells[1], ["42", "", expected]);
        let projected = ironrag_backend::shared::extraction::tabular::extract_tabular(
            Some(name),
            Some(mime),
            bytes,
            Some(&output),
        )?;
        assert_eq!(projected.source_map["tables"][0]["headerPolicy"], "inferred_first_row_header");
        assert_eq!(projected.source_map["tables"][0]["columnCount"], 3);
    }
    for bytes in [b"".as_slice(), b"a,b\n".as_slice(), b"1,2\n3,4\n".as_slice()] {
        let path = directory.path().join("empty-or-headerless.csv");
        std::fs::write(&path, bytes)?;
        let output = super::runner::extract_native(request(path, "text/csv", bytes)).await?;
        let tables: Vec<xberg::types::Table> =
            serde_json::from_value(output.units[0].tables.clone())?;
        assert_eq!(
            tables.iter().map(|table| table.cells.len()).sum::<usize>(),
            bytes.iter().filter(|&&byte| byte == b'\n').count()
        );
    }
    Ok(())
}

#[tokio::test]
async fn digest_input_text_and_unit_limits_refuse_without_models() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("source.txt");
    let bytes = b"literal text";
    std::fs::write(&path, bytes)?;
    let mut input = request(path.clone(), "text/plain", bytes);
    input.source_sha256 = "0".repeat(64);
    assert!(
        super::runner::extract_native(input)
            .await
            .unwrap_err()
            .to_string()
            .contains("source_digest_mismatch")
    );
    let mut input = request(path.clone(), "text/plain", bytes);
    input.limits.input_bytes = 2;
    assert!(
        super::runner::extract_native(input).await.unwrap_err().to_string().contains("file_limit")
    );
    let mut input = request(path, "text/plain", bytes);
    input.limits.text_bytes = 2;
    assert!(super::runner::extract_native(input).await.is_err());
    let limits = Limits { units: 0, ..Limits::default() };
    assert!(limits.validate().is_err());
    let limits = Limits { input_bytes: u64::MAX, ..Limits::default() };
    assert!(limits.validate().is_err());
    Ok(())
}

fn pdf(pages: u32) -> anyhow::Result<Vec<u8>> {
    use lopdf::{Object, dictionary};
    let mut document = lopdf::Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let kids = (0..pages).map(|_| {
        Object::Reference(document.add_object(dictionary! { "Type" => "Page", "Parent" => pages_id, "MediaBox" => vec![Object::Integer(0), Object::Integer(0), Object::Integer(100), Object::Integer(100)] }))
    }).collect::<Vec<_>>();
    document.objects.insert(
        pages_id,
        dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => pages }.into(),
    );
    let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    document.trailer.set("Root", catalog);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes)?;
    Ok(bytes)
}

#[tokio::test]
async fn pdf_count_uses_public_counter_above_pilot_limit_without_models() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let bytes = pdf(65)?;
    let path = directory.path().join("document.bin"); // MIME is authoritative.
    std::fs::write(&path, &bytes)?;
    let mut input = request(path.clone(), "application/pdf", &bytes);
    input.operation = Operation::Count;
    let output = super::runner::extract_native(input).await?;
    assert_eq!(output.source_unit_count, 65);
    assert!(output.units.is_empty());
    let mut native = request(path.clone(), "application/pdf", &bytes);
    native.start = Some(1);
    native.end = Some(1);
    let parent_request = serde_json::from_slice(&serde_json::to_vec(&native)?)?;
    let output = super::runner::extract_native(native).await?;
    let output = ironrag_backend::integrations::xberg::validate_response(
        ironrag_backend::integrations::extraction_protocol::Response::new(
            Ok(output),
            std::time::Duration::ZERO,
            None,
        ),
        &parent_request,
    )?;
    assert_eq!(output.source_unit_count, 65);
    assert_eq!(output.units.len(), 1);
    assert_eq!(output.units[0].ordinal, 1);
    assert_eq!(output.visual_inputs.len(), 1);
    assert_eq!(
        output.visual_inputs[0].kind,
        ironrag_backend::integrations::extraction_protocol::VisualInputKind::PdfPage
    );
    let mut input = request(path, "application/pdf", &bytes);
    input.operation = Operation::Count;
    input.limits.units = 64;
    assert!(super::runner::extract_native(input).await.is_err());
    Ok(())
}

#[tokio::test]
async fn native_image_preserves_vision_payload() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut cursor = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(4, 3).write_to(&mut cursor, image::ImageFormat::Png)?;
    let bytes = cursor.into_inner();
    let path = directory.path().join("image.png");
    std::fs::write(&path, &bytes)?;
    let output = super::runner::extract_native(request(path, "image/png", &bytes)).await?;
    assert!(output.content.is_empty());
    assert_eq!(output.visual_inputs.len(), 1);
    assert_eq!((output.visual_inputs[0].width, output.visual_inputs[0].height), (4, 3));
    assert!(!output.visual_inputs[0].content_base64.is_empty());
    Ok(())
}

fn two_frame_tiff() -> anyhow::Result<Vec<u8>> {
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut encoder = tiff::encoder::TiffEncoder::new(&mut cursor)?;
        encoder.write_image::<tiff::encoder::colortype::Gray8>(1, 1, &[0])?;
        encoder.write_image::<tiff::encoder::colortype::Gray8>(1, 1, &[255])?;
    }
    Ok(cursor.into_inner())
}

#[tokio::test]
async fn native_two_frame_tiff_passes_real_parent_validation() -> anyhow::Result<()> {
    use base64::Engine as _;
    use ironrag_backend::integrations::extraction_protocol::Response;
    use ironrag_backend::integrations::xberg::validate_response;

    let directory = tempfile::tempdir()?;
    let bytes = two_frame_tiff()?;
    let path = directory.path().join("two-frames.tiff");
    std::fs::write(&path, &bytes)?;
    let input = request(path, "image/tiff", &bytes);
    let child_request = serde_json::from_slice(&serde_json::to_vec(&input)?)?;
    let output = super::runner::extract_native(child_request).await?;
    // This is the real parent validator, not a local copy of its invariants.
    let validated =
        validate_response(Response::new(Ok(output), std::time::Duration::ZERO, None), &input)?;
    assert_eq!(validated.source_unit_count, 2);
    assert_eq!(validated.content, "\n\n");
    assert_eq!(validated.units.iter().map(|unit| unit.ordinal).collect::<Vec<_>>(), [1, 2]);
    assert!(validated.units.iter().all(|unit| unit.content.is_empty()));
    assert_eq!(validated.visual_inputs.len(), 2);
    for (ordinal, picture) in validated.visual_inputs.iter().enumerate() {
        assert_eq!(picture.ordinal, ordinal);
        assert_eq!(picture.native_index, ordinal as u32);
        assert_eq!(picture.source_unit, ordinal as u32 + 1);
        assert_eq!(picture.mime, "image/png");
        let raster = super::input::raster(
            &base64::engine::general_purpose::STANDARD.decode(&picture.content_base64)?,
        )?;
        assert_eq!(raster.to_luma8().as_raw(), &[if ordinal == 0 { 0 } else { 255 }]);
    }
    let mut broken = validated;
    broken.content.clear();
    assert!(
        validate_response(Response::new(Ok(broken), std::time::Duration::ZERO, None), &input,)
            .is_err(),
        "the parent must still reject the previously broken two-frame aggregate"
    );
    Ok(())
}

#[tokio::test]
async fn picture_only_tiff_join_counts_toward_text_budget() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let bytes = two_frame_tiff()?;
    let path = directory.path().join("two-frames.tiff");
    std::fs::write(&path, &bytes)?;
    let mut input = request(path, "image/tiff", &bytes);
    input.limits.text_bytes = 1; // The two empty units still require a two-byte separator.
    assert!(
        super::runner::extract_native(input)
            .await
            .unwrap_err()
            .to_string()
            .contains("aggregate_content_limit")
    );
    Ok(())
}

fn ods() -> anyhow::Result<Vec<u8>> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, content) in [
        ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
        (
            "META-INF/manifest.xml",
            r#"<?xml version="1.0"?><manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="application/vnd.oasis.opendocument.spreadsheet"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#,
        ),
        (
            "styles.xml",
            r#"<?xml version="1.0"?><office:document-styles xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0"><office:styles/></office:document-styles>"#,
        ),
        (
            "content.xml",
            r#"<?xml version="1.0"?><office:document-content xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0"><office:body><office:spreadsheet><table:table table:name="Dates"><table:table-row><table:table-cell office:value-type="string"><text:p>When</text:p></table:table-cell></table:table-row><table:table-row><table:table-cell office:value-type="date" office:date-value="2026-10-22"><text:p>2026-10-22</text:p></table:table-cell></table:table-row></table:table><table:table table:name="Empty"/></office:spreadsheet></office:body></office:document-content>"#,
        ),
    ] {
        zip.start_file(name, options)?;
        zip.write_all(content.as_bytes())?;
    }
    Ok(zip.finish()?.into_inner())
}

#[tokio::test]
async fn spreadsheet_keeps_sheet_identity_dates_and_empty_sheets() -> anyhow::Result<()> {
    let bytes = ods()?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("sheets.ods");
    std::fs::write(&path, &bytes)?;
    let output = super::runner::extract_native(request(
        path,
        "application/vnd.oasis.opendocument.spreadsheet",
        &bytes,
    ))
    .await?;
    assert_eq!(output.source_unit_count, 2);
    assert_eq!(output.units[0].sheet_name.as_deref(), Some("Dates"));
    assert_eq!(output.units[1].sheet_name.as_deref(), Some("Empty"));
    assert!(output.content.contains("2026-10-22"));
    let projected = ironrag_backend::shared::extraction::tabular::extract_tabular(
        Some("sheets.ods"),
        None,
        &bytes,
        Some(&output),
    )?;
    assert_eq!(projected.source_map["sheet_count"], 2);
    assert_eq!(projected.source_map["tables"][1]["headerPolicy"], "empty_sheet");
    let path = directory.path().join("corrupt.ods");
    let corrupt = b"not a workbook";
    std::fs::write(&path, corrupt)?;
    assert!(
        super::runner::extract_native(request(
            path,
            "application/vnd.oasis.opendocument.spreadsheet",
            corrupt
        ))
        .await
        .is_err()
    );
    Ok(())
}
