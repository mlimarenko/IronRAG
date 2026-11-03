use super::*;
use crate::integrations::extraction_protocol::{Limits, Request, Response, Unit};
use std::time::Duration;

fn fixture_request() -> Request {
    Request {
        path: "/var/tmp/source.pdf".into(),
        operation: Operation::Extract,
        mime: "application/pdf".into(),
        source_sha256: "a".repeat(64),
        limits: Limits::default(),
        start: Some(2),
        end: Some(3),
    }
}

fn extraction(request: &Request) -> Extraction {
    Extraction {
        source_sha256: request.source_sha256.clone(),
        text_byte_limit: request.limits.text_bytes,
        source_unit_count: 3,
        unit_kind: "page".into(),
        selected_start: 2,
        selected_end: 3,
        content: "second\n\nthird".into(),
        units: vec![
            Unit {
                ordinal: 2,
                sheet_name: None,
                content: "second".into(),
                tables: serde_json::json!([]),
            },
            Unit {
                ordinal: 3,
                sheet_name: None,
                content: "third".into(),
                tables: serde_json::json!([]),
            },
        ],
        visual_inputs: vec![],
        warnings: vec![],
    }
}

#[test]
fn accepts_only_canonical_source_bound_range_and_ordered_content() {
    let request = fixture_request();
    let response = Response::new(Ok(extraction(&request)), Duration::ZERO, None);
    let output = output::validate_response(response, &request).expect("valid canonical response");
    let built = output::build_output(
        output,
        XbergSource {
            file_name: Some("source.pdf"),
            mime_type: Some("application/pdf"),
            source_format: "pdf",
        },
    )
    .expect("valid output");
    assert_eq!(built.content_text, "second\n\nthird");
    assert_eq!(
        built.structure_hints.lines.iter().map(|line| line.page_number).collect::<Vec<_>>(),
        [Some(2), Some(3)]
    );
    assert_eq!(
        built
            .structure_hints
            .lines
            .iter()
            .map(|line| (line.start_offset, line.end_offset))
            .collect::<Vec<_>>(),
        [(Some(0), Some(6)), (Some(8), Some(13))]
    );
    assert_eq!(built.source_map["engine"], ENGINE);
    assert_eq!(built.source_map["text_semantics"], TEXT_SEMANTICS);
}

#[test]
fn admitted_text_budget_cannot_be_replaced_by_the_producer() {
    let request = fixture_request();
    let mut payload = extraction(&request);
    payload.text_byte_limit = crate::integrations::extraction_protocol::MAX_TEXT;
    let response = Response::new(Ok(payload), Duration::ZERO, None);
    assert!(validate_response(response, &request).is_err());
}

#[test]
fn native_markdown_is_decoded_once_before_literal_vision_text_can_be_joined() {
    let request = fixture_request();
    let mut payload = extraction(&request);
    payload.units[0].content = r"ALPHA\_TIMEOUT -&gt; &amp;quot; &amp;lt;".into();
    payload.content =
        payload.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    let output =
        validate_response(Response::new(Ok(payload), Duration::ZERO, None), &request).unwrap();
    assert_eq!(output.units[0].content, "ALPHA_TIMEOUT -> &quot; &lt;");
    assert_eq!(output.content, "ALPHA_TIMEOUT -> &quot; &lt;\n\nthird");
    assert_eq!(output.text_byte_limit, request.limits.text_bytes);
}

#[test]
fn projection_refuses_a_stale_aggregate_after_unit_recognition() {
    let mut native = extraction(&fixture_request());
    native.units[0].content.push_str("\nrecognized image text");
    assert!(
        project_native(
            native,
            XbergSource {
                file_name: Some("source.pdf"),
                mime_type: Some("application/pdf"),
                source_format: "pdf",
            }
        )
        .is_err()
    );
}

#[test]
fn native_pdf_projection_is_source_scoped_and_keeps_code_and_canonical_roles() {
    let markdown = "| Name | Value |\n| --- | --- |\n| alpha | 7 |";
    let mut payload = extraction(&fixture_request());
    payload.units[0].content = markdown.into();
    payload.content =
        payload.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    let built = project_native(
        payload,
        XbergSource { file_name: Some("source.docx"), mime_type: None, source_format: "docx" },
    )
    .unwrap();
    assert!(
        built.content_text.starts_with(markdown),
        "authored/non-PDF Markdown keeps its declared header contract"
    );
    let canonical = crate::shared::extraction::table_logical::LogicalTable::from_cells(
        2,
        1,
        vec![
            crate::shared::extraction::table_logical::LogicalCellDraft::new(0, 0, "Name"),
            crate::shared::extraction::table_logical::LogicalCellDraft::new(1, 0, "alpha"),
        ],
    )
    .unwrap()
    .render_canonical();
    let content = format!("```text\n{markdown}\n```\n\n{canonical}");
    let projected = tables::project_pdf_table_lines(&content, 1024 * 1024).unwrap().join("\n");
    assert_eq!(
        projected, content,
        "literal code and an explicit anchor-band contract are untouched"
    );
    let parsed = crate::shared::extraction::table_logical::LogicalTable::parse_canonical_lines(
        &canonical.lines().collect::<Vec<_>>(),
    )
    .unwrap();
    let projection = parsed
        .project(crate::shared::extraction::table_markdown::SemanticTableContext::default())
        .unwrap();
    assert_eq!(projection.header_row_count, 1);
    assert_eq!(projection.header.paths[0].display, "Name");
}

#[test]
fn native_docx_table_transport_defers_literal_cells_and_keeps_declared_headers() {
    use crate::shared::extraction::{
        table_logical::LogicalTable,
        table_markdown::{SemanticTableContext, parse_markdown_table_rows},
    };
    let request = Request {
        path: "/var/tmp/source.docx".into(),
        mime: "application/vnd.openxmlformats-officedocument.wordprocessingml.document".into(),
        start: Some(1),
        end: Some(1),
        ..fixture_request()
    };
    let table =
        "| key | value |\n| --- | --- |\n| literal | \\<br\\> |\n| entity | \\&amp; |\n| - | - |";
    let mut payload = extraction(&request);
    payload.source_unit_count = 1;
    payload.unit_kind = "document".into();
    payload.selected_start = 1;
    payload.selected_end = 1;
    payload.content = table.into();
    payload.units = vec![Unit {
        ordinal: 1,
        sheet_name: None,
        content: table.into(),
        tables: serde_json::json!([]),
    }];
    let native =
        validate_response(Response::new(Ok(payload), Duration::ZERO, None), &request).unwrap();
    assert_eq!(native.units[0].content, table);
    let projected = project_native(
        native,
        XbergSource {
            file_name: Some("source.docx"),
            mime_type: Some(&request.mime),
            source_format: "docx",
        },
    )
    .unwrap();
    assert_eq!(projected.content_text, table);
    let rows = parse_markdown_table_rows(&projected.content_text);
    assert_eq!(rows[1], ["literal", "<br>"]);
    assert_eq!(rows[2], ["entity", "&amp;"]);
    assert_eq!(rows[3], ["-", "-"]);
    let table = LogicalTable::from_markdown_rows(&rows).unwrap();
    let projection = table.project(SemanticTableContext::default()).unwrap();
    assert_eq!(projection.header_row_count, 1);
    assert_eq!(projection.header.paths[0].display, "key");
    assert_eq!(projection.body_rows.len(), 3);
}

#[test]
fn native_pdf_projection_cannot_expand_beyond_the_admitted_text_budget() {
    let mut payload = extraction(&fixture_request());
    payload.units[0].content = "| a | b |\n| --- | --- |\n| c | d |".into();
    payload.content =
        payload.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    payload.text_byte_limit = payload.content.len();
    let result = project_native(
        payload,
        XbergSource { file_name: Some("source.pdf"), mime_type: None, source_format: "pdf" },
    );
    assert!(
        matches!(result, Err(XbergExtractionError::Contract(message)) if message == "projected content budget exceeded")
    );
}

#[test]
fn empty_pdf_range_keeps_contract_validation_and_defers_document_emptiness() {
    let request = fixture_request();
    let mut payload = extraction(&request);
    for unit in &mut payload.units {
        unit.content.clear();
    }
    for source_unit in 2..=3 {
        let mut page = visual(crate::integrations::extraction_protocol::VisualInputKind::PdfPage);
        page.ordinal = (source_unit - 2) as usize;
        page.source_unit = source_unit;
        page.native_index = source_unit - 1;
        payload.visual_inputs.push(page);
    }
    payload.content = "\n\n".into();
    let response = Response::new(Ok(payload), Duration::ZERO, None);
    let native = output::validate_response(response, &request)
        .expect("a visual-only PDF range is valid before parent Vision");
    let output = project_native(
        native,
        XbergSource {
            file_name: Some("source.pdf"),
            mime_type: Some("application/pdf"),
            source_format: "pdf",
        },
    )
    .expect("parent projection of blank range");
    assert!(output.content_text.trim().is_empty());
    assert_eq!(output.page_count, Some(3));

    let mut missing = extraction(&request);
    missing.content.clear();
    missing.units.clear();
    assert!(
        output::validate_response(Response::new(Ok(missing), Duration::ZERO, None), &request)
            .is_err(),
        "empty content does not waive selected-unit cardinality"
    );
}

#[test]
fn structured_producer_refusal_is_distinct_from_input_invalidity_and_identity_tampering() {
    let error =
        Response::new(Err(anyhow::anyhow!("native workbook refused")), Duration::ZERO, None);
    assert!(matches!(
        output::response_failure(error),
        XbergExtractionError::ProducerRejected { .. }
    ));
    let mut impostor =
        Response::new(Err(anyhow::anyhow!("native workbook refused")), Duration::ZERO, None);
    impostor.engine.push_str("-other");
    assert!(matches!(output::response_failure(impostor), XbergExtractionError::Contract(_)));
    let success = Response::new(Ok(extraction(&fixture_request())), Duration::ZERO, None);
    assert!(matches!(output::response_failure(success), XbergExtractionError::Contract(_)));
}

#[test]
fn rejects_identity_tampering_unit_reordering_and_oversized_content() {
    let request = fixture_request();
    for fault in 0..8 {
        let mut response = Response::new(Ok(extraction(&request)), Duration::ZERO, None);
        match fault {
            0 => response.contract.push_str("-other"),
            1 => response.producer.push_str("-other"),
            2 => response.engine.push_str("-other"),
            3 => response.profile.push_str("-other"),
            4 => response.result.as_mut().unwrap().source_sha256 = "b".repeat(64),
            5 => response.result.as_mut().unwrap().selected_start = 1,
            6 => response.result.as_mut().unwrap().units.reverse(),
            _ => response.result.as_mut().unwrap().content.push_str(" tampered"),
        }
        assert!(output::validate_response(response, &request).is_err(), "fault {fault}");
    }
    let mut tiny = fixture_request();
    tiny.limits.text_bytes = 4;
    let response = Response::new(Ok(extraction(&tiny)), Duration::ZERO, None);
    assert!(output::validate_response(response, &tiny).is_err());
}

#[test]
fn count_cannot_smuggle_extraction_payload() {
    let mut request = fixture_request();
    request.operation = Operation::Count;
    request.start = None;
    request.end = None;
    let mut payload = extraction(&request);
    payload.selected_start = 1;
    let response = Response::new(Ok(payload), Duration::ZERO, None);
    assert!(output::validate_response(response, &request).is_err());
}

fn visual(
    kind: crate::integrations::extraction_protocol::VisualInputKind,
) -> crate::integrations::extraction_protocol::VisualInput {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 3).write_to(&mut png, image::ImageFormat::Png).unwrap();
    let bytes = png.into_inner();
    crate::integrations::extraction_protocol::VisualInput {
        kind,
        ordinal: 0,
        native_index: 0,
        source_unit: 2,
        source_path: None,
        mime: "image/png".into(),
        width: 2,
        height: 3,
        display_extent_96_dpi: None,
        payload_sha256: hex::encode(Sha256::digest(&bytes)),
        content_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
    }
}

#[test]
fn validates_visual_bytes_digest_dimensions_association_and_inventory() {
    use crate::integrations::extraction_protocol::VisualInputKind;
    let request = fixture_request();
    for fault in 0..10 {
        let mut payload = extraction(&request);
        let mut input = visual(VisualInputKind::EmbeddedPicture);
        match fault {
            0 => input.source_unit = 1,
            1 => input.content_base64 = "invalid base64".into(),
            2 => input.width = 1,
            3 => input.ordinal = 9,
            4 => input.payload_sha256 = "0".repeat(64),
            5 => input.mime = "image/jpeg".into(),
            6 => input.kind = VisualInputKind::RasterFrame,
            7 => input.source_path = Some("../escape".into()),
            8 => {
                let mut duplicate = visual(VisualInputKind::EmbeddedPicture);
                duplicate.ordinal = 1;
                payload.visual_inputs.push(input);
                input = duplicate;
            }
            _ => {}
        }
        payload.visual_inputs.push(input);
        let result =
            output::validate_response(Response::new(Ok(payload), Duration::ZERO, None), &request);
        assert_eq!(result.is_ok(), fault == 9, "visual fault {fault}");
    }
}

#[test]
fn visual_only_pdf_keeps_page_identity_and_excludes_embedded_duplicate_route() {
    use crate::integrations::extraction_protocol::VisualInputKind;
    let request = fixture_request();
    for duplicate in [false, true] {
        let mut payload = extraction(&request);
        payload.units[0].content.clear();
        payload.content = "\n\nthird".into();
        let mut page = visual(VisualInputKind::PdfPage);
        page.native_index = 1;
        payload.visual_inputs.push(page);
        if duplicate {
            let mut picture = visual(VisualInputKind::EmbeddedPicture);
            picture.ordinal = 1;
            picture.native_index = 2;
            payload.visual_inputs.push(picture);
        }
        let result =
            output::validate_response(Response::new(Ok(payload), Duration::ZERO, None), &request);
        if duplicate {
            assert!(result.is_err());
            continue;
        }
        let projected = project_native(
            result.unwrap(),
            XbergSource {
                file_name: Some("scan.pdf"),
                mime_type: Some("application/pdf"),
                source_format: "pdf",
            },
        )
        .unwrap();
        assert_eq!(projected.source_map["visual_inputs"][0]["source_unit"], 2);
        assert!(projected.source_map["visual_inputs"][0].get("content_base64").is_none());
        assert_eq!(
            projected.source_map["visual_inputs"][0]["payload_sha256"].as_str().unwrap().len(),
            64
        );
        assert!(projected.extracted_images.is_empty());
    }
}

#[test]
fn child_cannot_smuggle_model_fields_or_transcripts() {
    let mut payload = serde_json::to_value(extraction(&fixture_request())).unwrap();
    payload["ocrIdentity"] = serde_json::json!({"model":"retired"});
    assert!(serde_json::from_value::<Extraction>(payload).is_err());
    let mut input = serde_json::to_value(visual(
        crate::integrations::extraction_protocol::VisualInputKind::EmbeddedPicture,
    ))
    .unwrap();
    input["ocrText"] = serde_json::json!("untrusted transcript");
    assert!(
        serde_json::from_value::<crate::integrations::extraction_protocol::VisualInput>(input)
            .is_err()
    );
}
