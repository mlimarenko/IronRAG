//! Generated OOXML regressions for physical slide table ownership; no providers.
use std::io::{Cursor, Write};

use ironrag_backend::integrations::{
    extraction_protocol::{Limits, Operation, Request, Response},
    xberg::{XbergSource, project_native, validate_response},
};
use sha2::{Digest, Sha256};

const MIME: &str = "application/vnd.openxmlformats-officedocument.presentationml.presentation";

fn title(text: &str) -> String {
    format!(
        r#"<p:sp><p:nvSpPr><p:cNvPr id="1" name="Title"/><p:cNvSpPr/><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp>"#
    )
}

fn table(material: &str, stock: &str, code: &str) -> String {
    let rows = [["Material", "Stock", "Bin"], [material, stock, code]]
        .into_iter()
        .map(|row| {
            let cells = row
                .into_iter()
                .map(|value| {
                    format!(
                        "<a:tc><a:txBody><a:p><a:r><a:t>{value}</a:t></a:r></a:p></a:txBody></a:tc>"
                    )
                })
                .collect::<String>();
            format!("<a:tr>{cells}</a:tr>")
        })
        .collect::<String>();
    format!(
        "<p:graphicFrame><a:graphic><a:graphicData uri=\"http://schemas.openxmlformats.org/drawingml/2006/table\"><a:tbl>{rows}</a:tbl></a:graphicData></a:graphic></p:graphicFrame>"
    )
}

fn pptx(slides: &[String]) -> anyhow::Result<Vec<u8>> {
    let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    let mut members = vec![
        ("[Content_Types].xml".to_owned(), format!(
            r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>{}</Types>"#,
            (1..=slides.len()).map(|number| format!(r#"<Override PartName="/ppt/slides/slide{number}.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>"#)).collect::<String>()
        )),
        ("_rels/.rels".to_owned(), r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/></Relationships>"#.to_owned()),
        ("ppt/presentation.xml".to_owned(), format!(
            r#"<p:presentation xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><p:sldIdLst>{}</p:sldIdLst></p:presentation>"#,
            (1..=slides.len()).map(|number| format!(r#"<p:sldId id="{}" r:id="rId{number}"/>"#, number + 255)).collect::<String>()
        )),
        ("ppt/_rels/presentation.xml.rels".to_owned(), format!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{}</Relationships>"#,
            (1..=slides.len()).map(|number| format!(r#"<Relationship Id="rId{number}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{number}.xml"/>"#)).collect::<String>()
        )),
    ];
    for (index, body) in slides.iter().enumerate() {
        members.push((format!("ppt/slides/slide{}.xml", index + 1), format!(
            r#"<p:sld xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main"><p:cSld><p:spTree>{body}</p:spTree></p:cSld></p:sld>"#
        )));
    }
    for (name, body) in members {
        archive.start_file(name, options)?;
        archive.write_all(body.as_bytes())?;
    }
    Ok(archive.finish()?.into_inner())
}

fn request(path: std::path::PathBuf, bytes: &[u8]) -> Request {
    Request {
        path,
        mime: MIME.into(),
        source_sha256: hex::encode(Sha256::digest(bytes)),
        operation: Operation::Extract,
        start: None,
        end: None,
        limits: Limits::default(),
    }
}

#[tokio::test]
async fn pptx_native_tables_reach_parent_source_map_without_cross_slide_duplication()
-> anyhow::Result<()> {
    let bytes = pptx(&[
        title("First inventory") + &table("Copper", "7", "AB-0042"),
        title("Second inventory") + &table("Silver", "19", "ZX-0019"),
    ])?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("inventory.pptx");
    std::fs::write(&path, &bytes)?;
    let input = request(path.clone(), &bytes);
    let output = super::extract_native(request(path, &bytes)).await?;
    assert_eq!(output.source_unit_count, 2);
    assert_eq!(output.unit_kind, "slide");
    assert_eq!((output.selected_start, output.selected_end), (1, 2));
    assert!(output.visual_inputs.is_empty());
    let validated =
        validate_response(Response::new(Ok(output), std::time::Duration::ZERO, None), &input)?;
    for (index, (marker, code, foreign)) in
        [("Copper", "AB-0042", "Silver"), ("Silver", "ZX-0019", "Copper")].into_iter().enumerate()
    {
        let unit = &validated.units[index];
        assert_eq!(unit.ordinal, index as u32 + 1);
        let tables: Vec<xberg::types::Table> = serde_json::from_value(unit.tables.clone())?;
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].page_number, unit.ordinal);
        let stock = if index == 0 { "7" } else { "19" };
        assert_eq!(tables[0].cells, [vec!["Material", "Stock", "Bin"], vec![marker, stock, code]]);
        assert!(unit.content.contains(marker));
        assert!(!unit.content.contains(foreign));
    }
    let expected_tables =
        validated.units.iter().map(|unit| unit.tables.clone()).collect::<Vec<_>>();
    let projected = project_native(
        validated,
        XbergSource {
            file_name: Some("inventory.pptx"),
            mime_type: Some(MIME),
            source_format: "pptx",
        },
    )?;
    assert_eq!(projected.page_count, Some(2));
    for (index, tables) in expected_tables.into_iter().enumerate() {
        assert_eq!(projected.source_map["units"][index]["tables"], tables);
    }
    for marker in ["Copper", "AB-0042", "Silver", "ZX-0019"] {
        assert_eq!(projected.content_text.matches(marker).count(), 1);
    }
    Ok(())
}

#[tokio::test]
async fn pptx_empty_and_titleless_slides_keep_physical_table_identity_and_order()
-> anyhow::Result<()> {
    let bytes = pptx(&[
        String::new(),
        table("Copper", "7", "AB-0042") + &table("Silver", "19", "ZX-0019"),
        String::new(),
    ])?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("titleless.pptx");
    std::fs::write(&path, &bytes)?;
    let output = super::extract_native(request(path, &bytes)).await?;
    assert_eq!(output.source_unit_count, 3);
    assert_eq!(output.units.iter().map(|unit| unit.ordinal).collect::<Vec<_>>(), [1, 2, 3]);
    for index in [0, 2] {
        assert_eq!(output.units[index].tables, serde_json::json!([]));
        assert!(output.units[index].content.trim().is_empty());
    }
    let tables: Vec<xberg::types::Table> = serde_json::from_value(output.units[1].tables.clone())?;
    assert_eq!(tables.len(), 2);
    assert_eq!(tables.iter().map(|table| table.page_number).collect::<Vec<_>>(), [2, 2]);
    assert_eq!(tables[0].cells[1], ["Copper", "7", "AB-0042"]);
    assert_eq!(tables[1].cells[1], ["Silver", "19", "ZX-0019"]);
    Ok(())
}

async fn native_document() -> anyhow::Result<xberg::ExtractedDocument> {
    let bytes = pptx(&[table("Copper", "7", "AB-0042"), String::new()])?;
    let config = xberg::ExtractionConfig {
        use_cache: false,
        disable_ocr: true,
        enable_quality_processing: false,
        include_document_structure: true,
        output_format: xberg::OutputFormat::Markdown,
        pages: Some(xberg::core::config::PageConfig { extract_pages: true, ..Default::default() }),
        ..Default::default()
    };
    let mut result = Box::pin(
        xberg::engine::Engine::new_default()
            .extract(xberg::ExtractInput::from_bytes(bytes, MIME, None), &config),
    )
    .await?;
    anyhow::ensure!(result.errors.is_empty() && result.results.len() == 1);
    result.results.pop().ok_or_else(|| anyhow::anyhow!("native_engine_empty"))
}

fn projection() -> ironrag_backend::integrations::extraction_protocol::Extraction {
    ironrag_backend::integrations::extraction_protocol::Extraction {
        source_sha256: "0".repeat(64),
        text_byte_limit: Limits::default().text_bytes,
        source_unit_count: 2,
        unit_kind: "slide".into(),
        selected_start: 1,
        selected_end: 2,
        content: String::new(),
        units: Vec::new(),
        visual_inputs: Vec::new(),
        warnings: Vec::new(),
    }
}

#[tokio::test]
async fn pptx_projection_rejects_invalid_table_physical_identity() -> anyhow::Result<()> {
    let native = native_document().await?;
    assert_eq!(native.tables.len(), 1);
    for page_number in [0, 3, u32::MAX] {
        let mut document = native.clone();
        document.tables[0].page_number = page_number;
        let mut output = projection();
        assert!(
            super::append_native_units(&mut document, &mut output, 1)
                .unwrap_err()
                .to_string()
                .contains("table_slide_out_of_bounds")
        );
        assert!(output.units.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn pptx_projection_preserves_native_table_fields_and_rejects_bad_page_inventory()
-> anyhow::Result<()> {
    let native = native_document().await?;
    let expected = serde_json::to_value(&native.tables)?;
    let mut document = native.clone();
    let mut output = projection();
    super::append_native_units(&mut document, &mut output, 1)?;
    assert_eq!(output.units[0].tables, expected);
    assert_eq!(output.units[1].tables, serde_json::json!([]));
    for ordinals in [vec![1], vec![1, 1], vec![2, 1], vec![1, 3]] {
        let mut document = native.clone();
        let pages = document.pages.as_mut().ok_or_else(|| anyhow::anyhow!("pages missing"))?;
        pages.truncate(ordinals.len());
        for (page, ordinal) in pages.iter_mut().zip(ordinals) {
            page.page_number = ordinal;
        }
        let mut output = projection();
        assert!(
            super::append_native_units(&mut document, &mut output, 1)
                .unwrap_err()
                .to_string()
                .contains("slide_count_or_ordinal_mismatch")
        );
        assert!(output.units.is_empty());
    }
    Ok(())
}

#[tokio::test]
async fn pptx_partial_slide_selection_is_still_refused() -> anyhow::Result<()> {
    let bytes = pptx(&[table("Copper", "7", "AB-0042"), table("Silver", "19", "ZX-0019")])?;
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("selected.pptx");
    std::fs::write(&path, &bytes)?;
    let mut input = request(path, &bytes);
    input.start = Some(2);
    input.end = Some(2);
    assert!(
        super::extract_native(input)
            .await
            .unwrap_err()
            .to_string()
            .contains("selected_range_unsupported_for_format")
    );
    Ok(())
}
