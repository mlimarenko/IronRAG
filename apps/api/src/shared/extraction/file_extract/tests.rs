use std::{
    io::{Cursor, Write},
    sync::atomic::{AtomicUsize, Ordering},
};

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use image::{DynamicImage, ImageFormat};
use zip::write::SimpleFileOptions;

use super::*;

// General-format fixtures exercise the same native child route as uploads.
// The root check runner supplies IRONRAG_XBERG_EXTRACT_BIN for its built binary.
// A separate thread keeps this synchronous fixture API safe inside Tokio tests.
fn build_file_extraction_plan(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    bytes: Vec<u8>,
) -> Result<FileExtractionPlan, FileExtractError> {
    let kind = detect_upload_file_kind(file_name, mime_type, &bytes);
    if !matches!(kind, UploadFileKind::TextLike | UploadFileKind::Spreadsheet) {
        return super::build_file_extraction_plan(file_name, mime_type, bytes);
    }
    let file_name = file_name.map(str::to_owned);
    let mime_type = mime_type.map(str::to_owned);
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("native fixture runtime");
        runtime.block_on(super::build_model_free_file_extraction_plan(
            file_name.as_deref(),
            mime_type.as_deref(),
            bytes,
        ))
    })
    .join()
    .expect("native fixture thread")
}
use crate::integrations::llm::{
    ChatRequest, ChatResponse, EmbeddingBatchRequest, EmbeddingBatchResponse, EmbeddingRequest,
    EmbeddingResponse, VisionRequest, VisionResponse,
};

#[test]
fn xberg_output_and_resource_faults_never_reject_the_input_bytes() {
    use crate::integrations::xberg::XbergExtractionError;

    let invalid_json = XbergExtractionError::InvalidJson(
        serde_json::from_str::<serde_json::Value>("{").expect_err("invalid JSON fixture"),
    );
    let invalid_utf8 = XbergExtractionError::InvalidUtf8(
        String::from_utf8(vec![0xff]).expect_err("invalid UTF-8 fixture"),
    );
    for fault in [
        invalid_json,
        invalid_utf8,
        XbergExtractionError::EmptyOutput,
        XbergExtractionError::ProducerRejected {
            error_code: None,
            message: "native workbook refused".into(),
        },
    ] {
        assert_eq!(xberg_failure_class(&fault), ExtractionFailureClass::OperationalTerminal);
        assert_eq!(
            xberg_failure_class(&XbergExtractionError::PageExtractionFailed {
                page: 1,
                source: Box::new(fault),
            }),
            ExtractionFailureClass::OperationalTerminal
        );
    }
    assert_eq!(
        xberg_failure_class(&XbergExtractionError::Timeout(10)),
        ExtractionFailureClass::OperationalRetryable
    );
    assert_eq!(
        xberg_failure_class(&XbergExtractionError::InsufficientMemory {
            memory_limit_mib: 1024,
            current_rss_mib: 960,
            available_mib: 64,
            required_mib: 512,
            safety_margin_mib: 256,
        }),
        ExtractionFailureClass::OperationalTerminal
    );
}

#[tokio::test]
async fn runtime_html_dispatch_preserves_declared_cp1251_without_a_meta_tag() {
    let html = "<html><body><main><h1>Отчёт</h1><p>Проверка кодировки без метатега: Привет, мир.</p></main></body></html>";
    let (bytes, _, had_errors) = encoding_rs::WINDOWS_1251.encode(html);
    assert!(!had_errors);
    let policy = LibraryRecognitionPolicy::default();
    let plan = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &FakeGateway,
        vision_provider: None,
        vision_api_key: None,
        vision_base_url: None,
        vision_extra_parameters_json: None,
        file_name: Some("report.html"),
        mime_type: Some("TEXT/HTML; charset=windows-1251"),
        file_bytes: bytes.into_owned(),
        recognition_policy: &policy,
    })
    .await
    .expect("production dispatch through the actual model-free native child");
    let text = plan.normalized_text.as_deref().expect("decoded HTML text");
    assert!(text.contains("Отчёт"));
    assert!(text.contains("Проверка кодировки без метатега: Привет, мир."));
    assert!(!text.contains('\u{fffd}'));
    assert_eq!(
        plan.source_map["native_extraction"]["mime_type"],
        "text/html; charset=windows-1251"
    );
    assert_eq!(plan.source_map["native_extraction"]["producer"], xberg::PRODUCER);
}

fn valid_png_bytes() -> Vec<u8> {
    let image = DynamicImage::new_rgba8(2, 2);
    let mut cursor = Cursor::new(Vec::new());
    if let Err(error) = image.write_to(&mut cursor, ImageFormat::Png) {
        panic!("encode generated png fixture: {error}");
    }
    cursor.into_inner()
}

fn valid_xlsx_bytes() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(&mut cursor);
    let options = SimpleFileOptions::default();

    writer.start_file("[Content_Types].xml", options).expect("content types");
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
  <Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
</Types>"#,
        )
        .expect("write content types");

    writer.start_file("_rels/.rels", options).expect("root rels");
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#,
        )
        .expect("write root rels");

    writer.start_file("xl/workbook.xml", options).expect("workbook");
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
  <sheets><sheet name="Sheet1" sheetId="1" r:id="rId1"/></sheets>
</workbook>"#,
        )
        .expect("write workbook");

    writer.start_file("xl/_rels/workbook.xml.rels", options).expect("workbook rels");
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
</Relationships>"#,
        )
        .expect("write workbook rels");

    writer.start_file("xl/worksheets/sheet1.xml", options).expect("sheet1");
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
  <sheetData>
<row r="1">
  <c r="A1" t="inlineStr"><is><t>Name</t></is></c>
  <c r="B1" t="inlineStr"><is><t>Value</t></is></c>
</row>
<row r="2">
  <c r="A2" t="inlineStr"><is><t>Acme</t></is></c>
  <c r="B2"><v>42</v></c>
</row>
  </sheetData>
</worksheet>"#,
        )
        .expect("write sheet1");

    writer.finish().expect("finish xlsx");
    cursor.into_inner()
}

/// A ZIP that is a valid OOXML document but not a workbook (a DOCX-shaped
/// archive). The workbook backend cannot open it, which is the point of the
/// routing tests: a non-workbook ZIP must never be read as UTF-8 text nor be a
/// permanent content rejection.
fn non_workbook_zip_bytes() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(&mut cursor);
    let options = SimpleFileOptions::default();

    writer.start_file("_rels/.rels", options).expect("root rels");
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#,
        )
        .expect("write root rels");

    writer.start_file("word/document.xml", options).expect("document");
    writer
        .write_all(
            br#"<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p><w:r><w:t>Not a workbook</w:t></w:r></w:p></w:body>
</w:document>"#,
        )
        .expect("write document");

    writer.finish().expect("finish docx-shaped zip");
    cursor.into_inner()
}

const WORKBOOK_ROOT_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"#;
const EMPTY_WORKBOOK_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"></Relationships>"#;
const WORKBOOK_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
  <Default Extension="xml" ContentType="application/xml"/>
  <Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
</Types>"#;
const WORKBOOK_WITHOUT_SHEET_ENTRIES_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets/></workbook>"#;

/// The relationships every workbook-shaped fixture needs, so the variants below
/// differ only in the parts that decide their behaviour.
fn write_workbook_container_rels(writer: &mut zip::ZipWriter<&mut Cursor<Vec<u8>>>) {
    let options = SimpleFileOptions::default();
    writer.start_file("_rels/.rels", options).expect("root rels");
    writer.write_all(WORKBOOK_ROOT_RELS.as_bytes()).expect("write root rels");
    writer.start_file("xl/_rels/workbook.xml.rels", options).expect("workbook rels");
    writer.write_all(EMPTY_WORKBOOK_RELS.as_bytes()).expect("write workbook rels");
}

/// A ZIP that calamine opens as a workbook with zero sheets: it has the write
/// package relationships a workbook needs but no `xl/workbook.xml`. This is the
/// `Ok`-empty case the workbook backend returns for some non-workbook archives.
fn zero_sheet_workbook_zip_bytes() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(&mut cursor);
    write_workbook_container_rels(&mut writer);
    writer.finish().expect("finish zero-sheet zip");
    cursor.into_inner()
}

/// A workbook container that does carry `xl/workbook.xml`, but whose workbook
/// declares no sheets at all, so the backend opens it and reports zero
/// worksheets.
fn workbook_without_sheet_entries_zip_bytes() -> Vec<u8> {
    let mut cursor = Cursor::new(Vec::<u8>::new());
    let mut writer = zip::ZipWriter::new(&mut cursor);
    let options = SimpleFileOptions::default();

    writer.start_file("[Content_Types].xml", options).expect("content types");
    writer.write_all(WORKBOOK_CONTENT_TYPES.as_bytes()).expect("write content types");

    write_workbook_container_rels(&mut writer);

    writer.start_file("xl/workbook.xml", options).expect("workbook");
    writer.write_all(WORKBOOK_WITHOUT_SHEET_ENTRIES_XML.as_bytes()).expect("write workbook");

    writer.finish().expect("finish empty-sheet workbook zip");
    cursor.into_inner()
}

macro_rules! vision_only_gateway {
    ($ty:ty, $vision:expr) => {
        #[async_trait]
        impl LlmGateway for $ty {
            async fn generate(&self, _request: ChatRequest) -> Result<ChatResponse> {
                unreachable!("generate is not used in file extraction tests")
            }

            async fn embed(&self, _request: EmbeddingRequest) -> Result<EmbeddingResponse> {
                unreachable!("embed is not used in file extraction tests")
            }

            async fn embed_many(
                &self,
                _request: EmbeddingBatchRequest,
            ) -> Result<EmbeddingBatchResponse> {
                unreachable!("embed_many is not used in file extraction tests")
            }

            async fn vision_extract(&self, request: VisionRequest) -> Result<VisionResponse> {
                ($vision)(self, request)
            }
        }
    };
}

struct FakeGateway;
struct EmptyVisionGateway;
struct FlakyVisionGateway {
    calls: AtomicUsize,
}
struct CountingEmptyVisionGateway {
    calls: AtomicUsize,
}
struct CountingTextVisionGateway {
    calls: AtomicUsize,
}
struct ErroringVisionGateway {
    calls: AtomicUsize,
}

vision_only_gateway!(FakeGateway, |_gateway: &FakeGateway, mut request: VisionRequest| {
    Ok(VisionResponse {
        provider_kind: std::mem::take(&mut request.provider_kind),
        model_name: std::mem::take(&mut request.model_name),
        output_text: serde_json::json!({"outcome":"text","text":"Acme Corp\nBudget 2026"})
            .to_string(),
        usage_json: serde_json::json!({
            "prompt_tokens": 11,
            "completion_tokens": 3,
            "total_tokens": 14,
        }),
    })
});

vision_only_gateway!(
    EmptyVisionGateway,
    |_gateway: &EmptyVisionGateway, mut request: VisionRequest| {
        Ok(VisionResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text: String::new(),
            usage_json: serde_json::json!({}),
        })
    }
);

vision_only_gateway!(
    FlakyVisionGateway,
    |gateway: &FlakyVisionGateway, mut request: VisionRequest| {
        let call = gateway.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VisionResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text: if call == 0 {
                String::new()
            } else {
                serde_json::json!({"outcome":"text","text":"Recovered OCR text"}).to_string()
            },
            usage_json: if call == 0 {
                serde_json::json!({
                    "prompt_tokens": 100,
                    "completion_tokens": 0,
                    "total_tokens": 100,
                })
            } else {
                serde_json::json!({
                    "prompt_tokens": 100,
                    "completion_tokens": 5,
                    "total_tokens": 105,
                })
            },
        })
    }
);

vision_only_gateway!(
    CountingTextVisionGateway,
    |gateway: &CountingTextVisionGateway, mut request: VisionRequest| {
        gateway.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VisionResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text: serde_json::json!({"outcome":"text","text":"Counted OCR text"})
                .to_string(),
            usage_json: serde_json::json!({}),
        })
    }
);

vision_only_gateway!(
    ErroringVisionGateway,
    |gateway: &ErroringVisionGateway, _request: VisionRequest| {
        gateway.calls.fetch_add(1, Ordering::SeqCst);
        Err(anyhow!("provider exploded"))
    }
);

vision_only_gateway!(
    CountingEmptyVisionGateway,
    |gateway: &CountingEmptyVisionGateway, mut request: VisionRequest| {
        gateway.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VisionResponse {
            provider_kind: std::mem::take(&mut request.provider_kind),
            model_name: std::mem::take(&mut request.model_name),
            output_text: String::new(),
            usage_json: serde_json::json!({}),
        })
    }
);

#[test]
fn detects_pdf_by_extension() {
    assert_eq!(detect_upload_file_kind(Some("manual.pdf"), None, b"%PDF-1.7"), UploadFileKind::Pdf);
}

#[test]
fn detects_docx_by_extension() {
    assert_eq!(detect_upload_file_kind(Some("notes.docx"), None, b"binary"), UploadFileKind::Docx);
}

#[test]
fn detects_spreadsheet_by_xlsx_extension() {
    assert_eq!(
        detect_upload_file_kind(Some("sheet.xlsx"), None, b"binary"),
        UploadFileKind::Spreadsheet
    );
}

#[test]
fn detects_spreadsheet_by_xls_extension() {
    assert_eq!(
        detect_upload_file_kind(Some("sheet.xls"), None, b"binary"),
        UploadFileKind::Spreadsheet
    );
}

#[test]
fn detects_tabular_csv_by_extension() {
    assert_eq!(
        detect_upload_file_kind(Some("sheet.csv"), None, b"name,value\nacme,42\n"),
        UploadFileKind::Spreadsheet
    );
}

#[test]
fn detects_spreadsheet_by_ods_extension() {
    assert_eq!(
        detect_upload_file_kind(Some("sheet.ods"), None, b"binary"),
        UploadFileKind::Spreadsheet
    );
}

#[test]
fn detects_pptx_by_extension() {
    assert_eq!(detect_upload_file_kind(Some("deck.pptx"), None, b"binary"), UploadFileKind::Pptx);
}

#[test]
fn detects_image_by_mime_type() {
    assert_eq!(
        detect_upload_file_kind(Some("photo.bin"), Some("image/png"), &[0x89, 0x50, 0x4e]),
        UploadFileKind::Image
    );
}

#[test]
fn treats_svg_as_text_like_xml_not_raster_image() {
    assert_eq!(
        detect_upload_file_kind(
            Some("diagram.svg"),
            Some("image/svg+xml"),
            br#"<svg xmlns="http://www.w3.org/2000/svg"><text>Acme</text></svg>"#,
        ),
        UploadFileKind::TextLike
    );
}

#[test]
fn rejects_declared_heic_binary_bytes_as_unsupported() {
    // The declared MIME is unsupported, but the verdict comes from the bytes:
    // real HEIC content is binary, so it stays a content rejection.
    let heic = b"\x00\x00\x00\x18ftypheic\x00\x00\x00\x00heicmif1".to_vec();
    assert_eq!(
        validate_upload_file_admission(Some("photo.heic"), Some("image/heic"), &heic).unwrap_err(),
        FileExtractError::UnsupportedBinary
    );
}

#[test]
fn accepts_extensionless_utf8_text() {
    assert_eq!(
        detect_upload_file_kind(Some("Dockerfile"), None, b"FROM rust:1.86"),
        UploadFileKind::TextLike
    );
}

#[test]
fn accepts_spreadsheet_declared_extension_before_utf8_sniffing() {
    assert_eq!(
        detect_upload_file_kind(Some("sheet.xlsx"), None, br"name,value\nacme,42"),
        UploadFileKind::Spreadsheet
    );
}

#[test]
fn accepts_spreadsheet_declared_mime_type_before_utf8_sniffing() {
    assert_eq!(
        detect_upload_file_kind(
            Some("spreadsheet"),
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            br"name,value\nacme,42",
        ),
        UploadFileKind::Spreadsheet
    );
}

#[test]
fn rejects_extensionless_utf8_payloads_with_nul_bytes_as_binary() {
    assert_eq!(
        detect_upload_file_kind(Some("payload.bin"), None, b"\0\x01\x02\x03\n"),
        UploadFileKind::Binary
    );
}

#[test]
fn accepts_unknown_extension_text_payload_via_content_sniffing() {
    assert_eq!(
        detect_upload_file_kind(
            Some(".env.backup.20260116-162838"),
            Some("application/octet-stream"),
            b"DATABASE_URL=postgres://localhost:5432/app\nAPI_KEY=redacted\n",
        ),
        UploadFileKind::TextLike
    );
}

#[test]
fn extracts_record_jsonl_by_extension() {
    let plan = build_file_extraction_plan(
        Some("events.jsonl"),
        Some("application/x-ndjson"),
        br#"{"id":"event-1","kind":"event","occurredAt":"2026-04-28T09:00:00Z","text":"created item"}"#.to_vec(),
    )
    .expect("record jsonl extraction");

    assert_eq!(plan.source_format_metadata.source_format, "record_jsonl");
    assert_eq!(plan.extraction_kind, "record_jsonl");
    let source_text = plan.source_text.as_deref().expect("source text");
    assert!(source_text.contains("[source_profile source_format=record_jsonl"));
    assert!(source_text.contains("unit_count=1"));
    // Generic rendering: the value-shaped timestamp stamps the record and
    // every leaf value stays searchable text.
    assert!(source_text.contains("[unit_ordinal=0 occurred_at=2026-04-28T09:00:00+00:00]"));
    assert!(source_text.contains("id=event-1"));
    assert!(source_text.contains("kind=event"));
    assert!(source_text.contains("text=created item"));
}

#[test]
fn rejects_invalid_record_jsonl_by_explicit_format() {
    let error = build_file_extraction_plan(
        Some("events.ndjson"),
        Some("application/x-ndjson"),
        b"{\"id\":1}\nnot-json\n".to_vec(),
    )
    .unwrap_err();

    assert!(matches!(error, FileExtractError::ExtractionFailed { .. }));
    assert_eq!(error.failure_class(), ExtractionFailureClass::ContentInvalid);
}

#[test]
fn malformed_csv_bytes_are_rejected_by_the_tabular_parser() {
    let error = build_file_extraction_plan(
        Some("sheet.csv"),
        Some("text/csv"),
        b"id,name\n1,\xff".to_vec(),
    )
    .expect_err("CSV parser cannot decode invalid UTF-8");
    assert!(matches!(error, FileExtractError::ExtractionFailed { .. }));
    assert_eq!(error.failure_class(), ExtractionFailureClass::ContentInvalid);
}

#[test]
fn workbook_container_beyond_the_decompression_limit_is_operational_not_content_invalid() {
    let error = build_file_extraction_plan(
        Some("ledger.xlsx"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        crate::shared::extraction::tabular::declared_oversized_workbook_zip_bytes(),
    )
    .expect_err("a container above the decompression limit is refused");
    assert!(matches!(error, FileExtractError::ExtractionFailed { .. }));
    assert_eq!(
        error.failure_class(),
        ExtractionFailureClass::OperationalTerminal,
        "a decompression bound is an attempt limit, not a byte verdict"
    );
}

#[test]
fn irg173_csv_mime_with_non_tabular_extension_extracts_without_content_rejection() {
    let plan =
        build_file_extraction_plan(Some("report.txt"), Some("text/csv"), b"a,b\n1,2\n".to_vec())
            .expect("declared CSV must be extracted even with a .txt extension");
    assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(plan.source_format_metadata.source_format, "csv");
    assert!(plan.source_text.as_deref().is_some_and(|text| text.contains("| 1 | 2 |")));
}

#[test]
fn irg173_workbook_backend_failure_is_not_content_rejection() {
    let error = build_file_extraction_plan(
        Some("report.xlsx"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        b"not a workbook".to_vec(),
    )
    .expect_err("workbook backend must reject non-workbook bytes");
    assert_eq!(error.failure_class(), ExtractionFailureClass::OperationalTerminal);
    assert_eq!(
        UploadAdmissionError::from_file_extract_error(
            "report.xlsx",
            Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
            14,
            &error,
        )
        .extraction_failure_class(),
        Some(ExtractionFailureClass::OperationalTerminal),
    );
}

#[test]
fn irg173_csv_decode_failure_remains_content_rejection_with_mime_routing() {
    let error =
        build_file_extraction_plan(Some("report.txt"), Some("text/csv"), b"a,b\n1,\xff\n".to_vec())
            .expect_err("CSV UTF-8 decoding must reject malformed bytes");
    assert_eq!(error.failure_class(), ExtractionFailureClass::ContentInvalid);
}

#[test]
fn irg173_known_tabular_extension_precedes_conflicting_mime() {
    let plan = build_file_extraction_plan(
        Some("report.csv"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        b"a,b\n1,2\n".to_vec(),
    )
    .expect("known CSV extension takes precedence over conflicting spreadsheet MIME");
    assert_eq!(plan.source_format_metadata.source_format, "csv");
}

#[test]
fn irg173_csv_mime_with_workbook_container_bytes_extracts_as_workbook() {
    // A name without a usable extension and a `text/csv` MIME: only the bytes
    // can prove the payload is really a workbook archive, so the container
    // signature must win over the CSV claim instead of failing UTF-8 decode.
    let plan = build_file_extraction_plan(Some("report"), Some("text/csv"), valid_xlsx_bytes())
        .expect("workbook container bytes must extract despite the text/csv claim");
    assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Name | Value |")));
}

#[test]
fn irg173_csv_extension_with_workbook_container_bytes_extracts_as_workbook() {
    // Web ingest synthesizes `table.csv` from `Content-Type: text/csv`
    // (web/support.rs). A server can serve real workbook bytes under that
    // label; the container signature, not the name, decides the parser.
    let plan = build_file_extraction_plan(Some("table.csv"), Some("text/csv"), valid_xlsx_bytes())
        .expect("a real workbook named .csv must extract, not be decoded as UTF-8");
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Name | Value |")));
}

#[test]
fn irg173_tsv_extension_with_workbook_container_bytes_extracts_as_workbook() {
    let plan = build_file_extraction_plan(
        Some("table.tsv"),
        Some("text/tab-separated-values"),
        valid_xlsx_bytes(),
    )
    .expect("a real workbook named .tsv must extract, not be decoded as UTF-8");
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Name | Value |")));
}

#[test]
fn irg173_ole_workbook_container_with_csv_claim_is_not_a_content_rejection() {
    // An OLE2 (`xls`) container on a `text/csv` claim must not be decoded as
    // UTF-8 text, which would fabricate a `ContentInvalid` verdict. These
    // truncated bytes cannot yield a workbook, an operational fault.
    let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    bytes.extend_from_slice(b"truncated ole payload");
    let error = build_file_extraction_plan(Some("report.csv"), Some("text/csv"), bytes)
        .expect_err("a truncated OLE container cannot yield a workbook");
    assert_eq!(error.failure_class(), ExtractionFailureClass::OperationalTerminal);
}

#[test]
fn irg173_declared_text_plain_with_workbook_container_bytes_extracts() {
    // A server can label a real workbook `text/plain` and omit the extension.
    // The declared text MIME must not force a UTF-8 decode of archive bytes and
    // must not produce a `ContentInvalid` verdict.
    let plan = build_file_extraction_plan(Some("export"), Some("text/plain"), valid_xlsx_bytes())
        .expect("a workbook served as text/plain must extract, not be decoded as UTF-8");
    assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert_eq!(plan.source_format_metadata.source_format, "xlsx");
    assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Name | Value |")));
}

#[test]
fn irg173_text_extension_with_workbook_container_bytes_extracts() {
    // A `.txt` name with a `text/plain` MIME carrying workbook bytes: the
    // container signature, not the declared name, selects the parser.
    let plan =
        build_file_extraction_plan(Some("report.txt"), Some("text/plain"), valid_xlsx_bytes())
            .expect("a workbook named .txt must extract, not be decoded as UTF-8");
    assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Name | Value |")));
}

#[test]
fn irg173_octet_stream_with_workbook_container_bytes_extracts() {
    let plan = build_file_extraction_plan(
        Some("export"),
        Some("application/octet-stream"),
        valid_xlsx_bytes(),
    )
    .expect("a workbook served as application/octet-stream must extract");
    assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Name | Value |")));
}

#[test]
fn irg173_ole_container_on_octet_stream_is_not_a_content_rejection() {
    // An OLE2 (`xls`) container on a generic binary MIME must route to the
    // workbook backend, whose refusal is operational, never `ContentInvalid`.
    let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    bytes.extend_from_slice(b"truncated ole payload");
    let error = build_file_extraction_plan(Some("export"), Some("application/octet-stream"), bytes)
        .expect_err("a truncated OLE container cannot yield a workbook");
    assert_eq!(error.failure_class(), ExtractionFailureClass::OperationalTerminal);
    assert_ne!(error.failure_class(), ExtractionFailureClass::ContentInvalid);
}

#[test]
fn irg173_non_workbook_zip_container_is_not_a_content_rejection() {
    // A DOCX-shaped ZIP served without a declared document kind is routed to
    // the workbook backend by its container signature. calamine refuses it; the
    // failure is operational, never a permanent content rejection.
    let error = build_file_extraction_plan(
        Some("export"),
        Some("application/octet-stream"),
        non_workbook_zip_bytes(),
    )
    .expect_err("a DOCX-shaped zip is not a workbook");
    assert_eq!(error.failure_class(), ExtractionFailureClass::OperationalTerminal);
}

#[test]
fn irg173_zip_container_without_a_workbook_part_is_operational_not_content_invalid() {
    // A DOCX-shaped archive without `xl/workbook.xml` is refused by the
    // container guard before calamine: no workbook part means no extractable
    // units, and the refusal must never be a permanent content rejection.
    let error = build_file_extraction_plan(
        Some("export"),
        Some("application/octet-stream"),
        zero_sheet_workbook_zip_bytes(),
    )
    .expect_err("a container without a workbook part yields no content");
    assert_eq!(error.failure_class(), ExtractionFailureClass::OperationalTerminal);
    assert!(error.to_string().contains("no workbook part"), "{error}");
}

#[test]
fn irg173_workbook_container_without_sheet_entries_is_operational_not_content_invalid() {
    // A real workbook part with no sheet entries still reaches the backend and
    // must stay operational: calamine opens it, our backend bail reports it,
    // and the bytes are never called unextractable content.
    let error = build_file_extraction_plan(
        Some("export"),
        Some("application/octet-stream"),
        workbook_without_sheet_entries_zip_bytes(),
    )
    .expect_err("a workbook with no worksheets yields no content");
    assert_eq!(error.failure_class(), ExtractionFailureClass::OperationalTerminal);
    assert!(error.to_string().contains("no workbook worksheets"), "{error}");
}

#[test]
fn irg173_declared_document_kinds_keep_their_route_over_a_zip_container() {
    // The ZIP signature is shared by DOCX/PPTX and workbooks. An explicitly
    // declared document kind must stay on the xberg route; only a text-like,
    // generic-binary or absent declaration yields to the container signature.
    assert_eq!(
        detect_upload_file_kind(
            Some("report.docx"),
            Some("application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
            b"PK\x03\x04docx",
        ),
        UploadFileKind::Docx
    );
    assert_eq!(
        detect_upload_file_kind(
            Some("deck.pptx"),
            Some("application/vnd.openxmlformats-officedocument.presentationml.presentation"),
            b"PK\x03\x04pptx",
        ),
        UploadFileKind::Pptx
    );
}

#[test]
fn irg173_plain_text_declaration_stays_text_like_without_a_container() {
    assert_eq!(
        detect_upload_file_kind(Some("notes.txt"), Some("text/plain"), b"hello, world\n"),
        UploadFileKind::TextLike
    );
    let plan = build_file_extraction_plan(
        Some("notes.txt"),
        Some("text/plain"),
        b"hello, world\n".to_vec(),
    )
    .expect("plain text stays on the text route");
    assert_eq!(plan.file_kind, UploadFileKind::TextLike);
    assert_eq!(plan.extraction_kind, "text_like");
}

#[test]
fn irg173_declared_text_plain_with_pdf_bytes_routes_to_xberg() {
    // A URL without an extension served with `Content-Type: text/plain` can be
    // a real PDF. Declared text is trusted only when the bytes are text; a
    // `%PDF-` header proves otherwise and must reach the xberg route instead
    // of a `ContentInvalid` verdict fabricated by the UTF-8 decoder.
    let pdf = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n1 0 obj\n<< /Length 4 >>\nstream\n\x00\x01\x02\x03\nendstream\nendobj\n%%EOF\n"
        .to_vec();
    assert_eq!(detect_upload_file_kind(None, Some("text/plain"), &pdf), UploadFileKind::Pdf);
    let error = build_file_extraction_plan(None, Some("text/plain"), pdf)
        .expect_err("a PDF requires the xberg runtime, not the local text parser");
    assert_ne!(error.failure_class(), ExtractionFailureClass::ContentInvalid);
}

#[test]
fn irg173_declared_text_plain_with_png_bytes_routes_to_image() {
    // Same for an image served as `text/plain`: the PNG magic, not the declared
    // text MIME, selects the route, so the vision runtime is reached.
    let png = valid_png_bytes();
    assert_eq!(detect_upload_file_kind(None, Some("text/plain"), &png), UploadFileKind::Image);
    let error = build_file_extraction_plan(None, Some("text/plain"), png)
        .expect_err("an image requires the vision runtime, not the local text parser");
    assert_ne!(error.failure_class(), ExtractionFailureClass::ContentInvalid);
}

#[test]
fn irg173_text_bytes_starting_with_zip_magic_stay_text_like() {
    // The ZIP local-file-header prefix is only four bytes and a text payload
    // may legitimately begin with them. When the bytes decode as text the
    // declared text kind wins; the shared archive prefix must not route the
    // payload to the workbook backend, whose refusal would be a false
    // operational fault.
    let payload = b"PK\x03\x04hello, world\n".to_vec();
    assert_eq!(
        detect_upload_file_kind(Some("notes.txt"), Some("text/plain"), &payload),
        UploadFileKind::TextLike
    );
    let plan = build_file_extraction_plan(Some("notes.txt"), Some("text/plain"), payload)
        .expect("zip-prefixed text must extract as text");
    assert_eq!(plan.file_kind, UploadFileKind::TextLike);
    assert_eq!(plan.extraction_kind, "text_like");
}

#[test]
fn irg173_declared_text_plain_with_unrecognized_binary_is_content_rejection() {
    // Declared text whose bytes are neither text nor a recognised format: the
    // content rejection is legitimate, not an operational fault.
    let error =
        build_file_extraction_plan(None, Some("text/plain"), b"\xff\xfe\xfd\xfc\x00\x01".to_vec())
            .expect_err("bytes that are neither text nor a known format stay unsupported");
    assert_eq!(error.failure_class(), ExtractionFailureClass::ContentInvalid);
}

#[test]
fn irg173_truly_binary_non_container_stays_a_content_rejection() {
    let error = build_file_extraction_plan(
        Some("blob.bin"),
        Some("application/octet-stream"),
        b"\x00\x01\x02\x03".to_vec(),
    )
    .expect_err("binary bytes without a container are a content rejection");
    assert_eq!(error.failure_class(), ExtractionFailureClass::ContentInvalid);
}

#[test]
fn irg173_known_tsv_extension_precedes_conflicting_csv_mime() {
    let plan = build_file_extraction_plan(
        Some("report.tsv"),
        Some("text/csv"),
        b"name\tvalue\nacme\t42\n".to_vec(),
    )
    .expect("known TSV extension takes precedence over a conflicting CSV MIME");
    assert_eq!(plan.source_format_metadata.source_format, "tsv");
}

#[test]
fn empty_inline_text_stays_on_the_empty_plan_contract() {
    let plan = build_file_extraction_plan(Some("empty.txt"), Some("text/plain"), b"  \n".to_vec())
        .expect("empty inline text is accepted, not mapped to a content rejection");
    assert_eq!(plan.adapter_status, "ready");
    assert!(plan.source_text.is_none());
    assert!(plan.normalized_text.is_none());
}

#[test]
fn empty_html_stays_on_the_empty_plan_contract() {
    // html5ever is lenient: tag-only markup parses without error and yields no
    // readable text, so the inline contract keeps it as an empty plan rather
    // than a content rejection. Only genuinely malformed parser bytes
    // (invalid JSONL lines, undecodable tabular bytes) are `ContentInvalid`.
    let plan = build_file_extraction_plan(
        Some("empty.html"),
        Some("text/html"),
        b"<html><body><main><broken></main></body></html>".to_vec(),
    )
    .expect("tag-only HTML yields an empty plan, not a content rejection");
    assert_eq!(plan.extraction_kind, "html_main_content");
    assert!(plan.source_text.is_none());
}

#[test]
fn declared_unsupported_mime_type_does_not_override_text_bytes() {
    // `application/x-sh` is not a supported MIME, and `deploy` has no known
    // extension, but the bytes are valid UTF-8 script text. A declared MIME
    // alone cannot reject them as content.
    assert_eq!(
        detect_upload_file_kind(Some("deploy"), Some("application/x-sh"), b"#!/bin/sh\necho ok\n"),
        UploadFileKind::TextLike
    );
}

#[test]
fn declared_unsupported_mime_type_with_binary_bytes_stays_binary() {
    // The bytes are genuinely binary, so an unsupported declared MIME still
    // yields the content rejection the upload gate must apply.
    assert_eq!(
        detect_upload_file_kind(
            Some("clip.mp4"),
            Some("video/mp4"),
            b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom",
        ),
        UploadFileKind::Binary
    );
}

#[test]
fn rejects_invalid_utf8_when_file_is_text_like() {
    let result =
        build_file_extraction_plan(Some("notes.txt"), Some("text/plain"), vec![0xff, 0xfe]);

    assert!(matches!(result, Err(FileExtractError::InvalidUtf8)));
}

#[test]
fn converts_invalid_utf8_into_structured_upload_rejection() {
    let rejection = UploadAdmissionError::from_file_extract_error(
        "notes.txt",
        Some("text/plain"),
        2,
        &FileExtractError::InvalidUtf8,
    );

    assert_eq!(rejection.error_kind(), "invalid_text_encoding");
    assert_eq!(rejection.details().file_name.as_deref(), Some("notes.txt"));
    assert_eq!(rejection.details().rejection_kind.as_deref(), Some("invalid_text_encoding"));
    assert_eq!(rejection.details().detected_format.as_deref(), Some("Text"));
    assert_eq!(rejection.details().file_size_bytes, Some(2));
}

#[test]
fn creates_structured_limit_rejection() {
    let rejection =
        UploadAdmissionError::file_too_large("manual.pdf", Some("application/pdf"), 1024, 1);

    assert_eq!(rejection.error_kind(), "upload_limit_exceeded");
    assert_eq!(rejection.details().rejection_kind.as_deref(), Some("upload_limit_exceeded"));
    assert_eq!(rejection.details().detected_format.as_deref(), Some("PDF"));
    assert_eq!(rejection.details().upload_limit_mb, Some(1));
}

#[test]
fn typed_stream_limit_body_errors_map_to_upload_limit_exceeded() {
    let rejection = multipart_file_body_error(
        Some("large.pdf"),
        Some("application/pdf"),
        4,
        MultipartFileBodyErrorKind::SizeLimit,
        "field size exceeded",
    );

    assert_eq!(rejection.error_kind(), "upload_limit_exceeded");
    assert_eq!(rejection.details().rejection_kind.as_deref(), Some("upload_limit_exceeded"));
    assert_eq!(rejection.details().upload_limit_mb, Some(4));
}

#[test]
fn typed_stream_failures_map_to_multipart_stream_failure() {
    let rejection = multipart_file_body_error(
        Some("report.pdf"),
        Some("application/pdf"),
        4,
        MultipartFileBodyErrorKind::StreamFailure,
        "failed to read stream to end",
    );

    assert_eq!(rejection.error_kind(), "multipart_stream_failure");
    assert_eq!(rejection.details().rejection_kind.as_deref(), Some("multipart_stream_failure"));
}

#[test]
fn untyped_error_text_cannot_override_multipart_failure_kind() {
    let rejection = multipart_file_body_error(
        Some("payload.bin"),
        Some("application/octet-stream"),
        4,
        MultipartFileBodyErrorKind::InvalidBody,
        "field size exceeded while retrying a blocked stream",
    );

    assert_eq!(rejection.error_kind(), "invalid_file_body");
}

#[test]
fn request_level_upload_rejections_carry_no_content_class() {
    // A truncated multipart stream may have incomplete bytes, so the server
    // holds no content verdict and must not claim one.
    let truncated = multipart_file_body_error(
        Some("report.pdf"),
        Some("application/pdf"),
        4,
        MultipartFileBodyErrorKind::StreamFailure,
        "unexpected end of multipart stream",
    );
    assert_eq!(truncated.extraction_failure_class(), None);

    assert_eq!(UploadAdmissionError::invalid_multipart_payload().extraction_failure_class(), None,);
    assert_eq!(
        UploadAdmissionError::missing_upload_file("no file field").extraction_failure_class(),
        None,
    );
    assert_eq!(
        UploadAdmissionError::file_too_large("big.pdf", Some("application/pdf"), 10, 1)
            .extraction_failure_class(),
        None,
        "a size limit is a request-level rejection, not a content verdict",
    );
}

#[test]
fn malformed_local_bytes_carry_the_content_class() {
    assert_eq!(
        UploadAdmissionError::from_file_extract_error(
            "notes.txt",
            Some("text/plain"),
            2,
            &FileExtractError::InvalidUtf8,
        )
        .extraction_failure_class(),
        Some(ExtractionFailureClass::ContentInvalid),
        "malformed text bytes are a deterministic content rejection",
    );
    assert_eq!(
        UploadAdmissionError::from_file_extract_error(
            "blob.bin",
            None,
            4,
            &FileExtractError::UnsupportedBinary,
        )
        .extraction_failure_class(),
        Some(ExtractionFailureClass::ContentInvalid),
        "an unsupported binary payload is a deterministic content rejection",
    );
}

#[test]
fn accepts_large_utf8_text_upload_plan() {
    let large_text = "IronRAG bulk ingest line.\n".repeat(32 * 1024);
    let plan = match build_file_extraction_plan(
        Some("large-notes.txt"),
        Some("text/plain"),
        large_text.clone().into_bytes(),
    ) {
        Ok(plan) => plan,
        Err(error) => panic!("large text extraction plan: {error}"),
    };

    assert_eq!(plan.file_kind, UploadFileKind::TextLike);
    assert_eq!(plan.extraction_kind, "text_like");
    assert_eq!(plan.normalized_text.as_deref(), Some(large_text.as_str()));
    assert_eq!(plan.source_format_metadata.source_format, "text_like");
}

#[test]
fn routes_html_uploads_through_html_main_content_extractor() {
    let html = r"
        <html>
            <head><title>Ingest page</title></head>
            <body><main><h1>Docs</h1><p>Primary path only.</p></main></body>
        </html>
    ";

    let plan = match build_file_extraction_plan(
        Some("index.html"),
        Some("text/html; charset=utf-8"),
        html.as_bytes().to_vec(),
    ) {
        Ok(plan) => plan,
        Err(error) => panic!("html extraction plan: {error}"),
    };

    assert_eq!(plan.file_kind, UploadFileKind::TextLike);
    assert_eq!(plan.extraction_kind, "html_main_content");
    assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("# Docs")));
    assert_eq!(plan.source_format_metadata.source_format, "html_main_content");
}

#[test]
fn local_plan_rejects_xberg_owned_pdf_upload() {
    let error = match build_file_extraction_plan(
        Some("manual.pdf"),
        Some("application/pdf"),
        b"%PDF-1.7\n".to_vec(),
    ) {
        Ok(plan) => panic!("pdf upload should require runtime xberg: {:?}", plan.file_kind),
        Err(error) => error,
    };

    assert!(matches!(
        error,
        FileExtractError::ExtractionFailed { file_kind: UploadFileKind::Pdf, .. }
    ));
    assert!(error.to_string().contains("xberg runtime extraction is required"));
}

#[test]
fn builds_tabular_extraction_plan_for_xlsx_upload() {
    let plan = match build_file_extraction_plan(
        Some("inventory.xlsx"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        valid_xlsx_bytes(),
    ) {
        Ok(plan) => plan,
        Err(error) => panic!("xlsx extraction plan: {error}"),
    };

    assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert_eq!(plan.source_format_metadata.source_format, "xlsx");
    assert_eq!(plan.source_map["recognition"]["engine"], serde_json::json!("native"));
    assert!(plan.normalized_text.as_deref().is_some_and(|text| text.contains("| Acme | 42 |")));
}

#[test]
fn builds_tabular_extraction_plan_for_csv_upload() {
    let plan = match build_file_extraction_plan(
        Some("people.csv"),
        Some("text/csv"),
        b"Name,Email\nAlice,alice@example.com\n".to_vec(),
    ) {
        Ok(plan) => plan,
        Err(error) => panic!("csv extraction plan: {error}"),
    };

    assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert_eq!(plan.source_format_metadata.source_format, "csv");
    assert!(
        plan.normalized_text
            .as_deref()
            .is_some_and(|text| text.contains("| Alice | alice@example.com |"))
    );
}

#[test]
fn builds_tabular_extraction_plan_for_tsv_upload() {
    let plan = match build_file_extraction_plan(
        Some("people.tsv"),
        Some("text/tab-separated-values"),
        b"Name\tEmail\nAlice\talice@example.com\n".to_vec(),
    ) {
        Ok(plan) => plan,
        Err(error) => panic!("tsv extraction plan: {error}"),
    };

    assert_eq!(plan.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(plan.extraction_kind, "tabular_text");
    assert_eq!(plan.source_format_metadata.source_format, "tsv");
    assert!(
        plan.normalized_text
            .as_deref()
            .is_some_and(|text| text.contains("| Alice | alice@example.com |"))
    );
}

#[tokio::test]
async fn runtime_plan_uses_native_tabular_parser_for_xlsx_upload() {
    let policy = LibraryRecognitionPolicy::default();
    let result = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &FakeGateway,
        vision_provider: None,
        vision_api_key: None,
        vision_base_url: None,
        vision_extra_parameters_json: None,
        file_name: Some("inventory.xlsx"),
        mime_type: Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        file_bytes: valid_xlsx_bytes(),
        recognition_policy: &policy,
    })
    .await
    .expect("runtime xlsx extraction");

    assert_eq!(result.file_kind, UploadFileKind::Spreadsheet);
    assert_eq!(result.extraction_kind, "tabular_text");
    assert_eq!(result.source_format_metadata.source_format, "xlsx");
    assert_eq!(result.source_map["recognition"]["engine"], serde_json::json!("native"));
    assert!(
        result.source_map.get("native_extraction").is_none(),
        "specialized XLSX Table projection must not claim a native child invocation"
    );
    assert_eq!(result.source_map["recognition"]["capability"], serde_json::json!("tabular_parse"));
    assert!(result.provider_kind.is_none());
    assert!(
        result.normalized_text.as_deref().is_some_and(|text| text.contains("| Name | Value |"))
    );
}

#[test]
fn rejects_binary_like_utf8_payloads_with_structured_unsupported_type() {
    let extraction_error = match build_file_extraction_plan(
        Some("unsupported.bin"),
        Some("application/octet-stream"),
        b"\0\x01\x02\x03\n".to_vec(),
    ) {
        Ok(plan) => panic!("binary-ish utf8 payload should be rejected: {:?}", plan.file_kind),
        Err(error) => error,
    };
    let rejection = UploadAdmissionError::from_file_extract_error(
        "unsupported.bin",
        Some("application/octet-stream"),
        5,
        &extraction_error,
    );

    assert_eq!(rejection.error_kind(), "unsupported_upload_type");
    assert_eq!(rejection.details().file_name.as_deref(), Some("unsupported.bin"));
    assert_eq!(rejection.details().detected_format.as_deref(), Some("Binary"));
}

#[test]
fn upload_admission_accepts_spreadsheet_before_persistence() {
    let result = validate_upload_file_admission(
        Some("sheet.xlsx"),
        Some("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
        b"binary xlsx payload",
    );

    assert_eq!(result.expect("spreadsheet upload should be admitted"), UploadFileKind::Spreadsheet);
}

#[tokio::test]
async fn runtime_plan_uses_vision_provider_for_non_xberg_images() {
    let policy = LibraryRecognitionPolicy::default();
    let provider = ProviderModelSelection {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-mini".to_string(),
    };
    let runtime_profile = serde_json::json!({
        "_providerProfile": {"runtime": {"kind": "openai_compatible"}}
    });

    let result = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &FakeGateway,
        vision_provider: Some(&provider),
        vision_api_key: Some("test-key"),
        vision_base_url: None,
        vision_extra_parameters_json: Some(&runtime_profile),
        file_name: Some("diagram.gif"),
        mime_type: Some("image/gif"),
        file_bytes: valid_png_bytes(),
        recognition_policy: &policy,
    })
    .await;
    let result = match result {
        Ok(plan) => plan,
        Err(error) => panic!("runtime image extraction: {error}"),
    };

    assert_eq!(result.file_kind, UploadFileKind::Image);
    assert_eq!(result.extraction_kind, "vision_image");
    assert_eq!(result.provider_kind.as_deref(), Some("openai"));
    assert_eq!(result.model_name.as_deref(), Some("gpt-5.4-mini"));
    assert_eq!(result.normalized_text.as_deref(), Some("Acme Corp\nBudget 2026"));
    assert_eq!(result.source_format_metadata.source_format, "image");
    let quality = extraction_quality_from_source_map(
        &result.source_map,
        &result.extraction_kind,
        result.extraction_warnings.len(),
    );
    assert_eq!(quality.normalization_status, ExtractionNormalizationStatus::Verbatim);
    assert_eq!(quality.ocr_source.as_deref(), Some("vision_llm"));
    assert_eq!(result.source_map["recognition"]["engine"], serde_json::json!("vision"));
    assert_eq!(result.source_map["recognition"]["capability"], serde_json::json!("image_ocr"));
    assert_eq!(result.source_map["recognition"]["structure_tier"], serde_json::json!("flat"));
}

#[tokio::test]
async fn runtime_plan_uses_vision_policy_for_static_raster_images() {
    let policy = LibraryRecognitionPolicy::default();
    let provider = ProviderModelSelection {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-mini".to_string(),
    };
    let runtime_profile = serde_json::json!({
        "_providerProfile": {"runtime": {"kind": "openai_compatible"}}
    });

    let result = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &FakeGateway,
        vision_provider: Some(&provider),
        vision_api_key: Some("test-key"),
        vision_base_url: None,
        vision_extra_parameters_json: Some(&runtime_profile),
        file_name: Some("scan.png"),
        mime_type: Some("image/png"),
        file_bytes: valid_png_bytes(),
        recognition_policy: &policy,
    })
    .await
    .expect("vision policy should route static raster image to vision");

    assert!(result.normalized_text.as_deref().is_some_and(|text| text.contains("Acme Corp")));
    assert_eq!(result.provider_kind.as_deref(), Some("openai"));
    assert_eq!(result.source_map["recognition"]["engine"], serde_json::json!("vision"));
}

#[tokio::test]
async fn explicit_vision_image_policy_without_binding_is_refused() {
    let mut native = native_visual_fixture(true);
    let error = recognize_native_visuals(
        &mut native,
        UploadFileKind::Pdf,
        visual_input(&FakeGateway, None),
    )
    .await
    .err()
    .expect("binding required for actual visual input");
    assert_eq!(error.failure_class(), ExtractionFailureClass::OperationalTerminal);
}

#[tokio::test]
async fn runtime_plan_rejects_empty_vision_image_text() {
    let policy = LibraryRecognitionPolicy::default();
    let provider = ProviderModelSelection {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-mini".to_string(),
    };

    let error = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &EmptyVisionGateway,
        vision_provider: Some(&provider),
        vision_api_key: Some("test-key"),
        vision_base_url: None,
        vision_extra_parameters_json: None,
        file_name: Some("scan.png"),
        mime_type: Some("image/png"),
        file_bytes: valid_png_bytes(),
        recognition_policy: &policy,
    })
    .await
    .expect_err("empty image OCR must fail loudly");

    assert!(error.to_string().contains("invalid recognition outcome"));
    assert_eq!(error.vision_calls().len(), 2);
}

#[tokio::test]
async fn runtime_plan_retries_once_when_vision_returns_empty_then_text() {
    let policy = LibraryRecognitionPolicy::default();
    let provider = ProviderModelSelection {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-mini".to_string(),
    };
    let gateway = FlakyVisionGateway { calls: AtomicUsize::new(0) };

    let result = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &gateway,
        vision_provider: Some(&provider),
        vision_api_key: Some("test-key"),
        vision_base_url: None,
        vision_extra_parameters_json: None,
        file_name: Some("scan.png"),
        mime_type: Some("image/png"),
        file_bytes: valid_png_bytes(),
        recognition_policy: &policy,
    })
    .await
    .expect("empty first vision response should be recovered by one retry");

    assert!(
        result.normalized_text.as_deref().is_some_and(|text| text.contains("Recovered OCR text")),
        "normalized_text should contain the recovered OCR text: {:?}",
        result.normalized_text
    );
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
    let calls = result.usage_json["vision_calls"].as_array().expect("raw call receipts");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0]["usage_json"]["total_tokens"], 100);
    assert_eq!(calls[1]["usage_json"]["total_tokens"], 105);
}

#[tokio::test]
async fn runtime_plan_rejects_empty_vision_image_text_after_one_retry() {
    let policy = LibraryRecognitionPolicy::default();
    let provider = ProviderModelSelection {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-mini".to_string(),
    };
    let gateway = CountingEmptyVisionGateway { calls: AtomicUsize::new(0) };

    let error = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &gateway,
        vision_provider: Some(&provider),
        vision_api_key: Some("test-key"),
        vision_base_url: None,
        vision_extra_parameters_json: None,
        file_name: Some("scan.png"),
        mime_type: Some("image/png"),
        file_bytes: valid_png_bytes(),
        recognition_policy: &policy,
    })
    .await
    .expect_err("empty image OCR on both attempts must fail loudly");

    assert!(error.to_string().contains("invalid recognition outcome"));
    assert_eq!(error.vision_calls().len(), 2);
    assert_eq!(
        error.failure_class(),
        ExtractionFailureClass::OperationalRetryable,
        "two empty HTTP 200 model outputs are a provider fault, not a content verdict"
    );
    assert_ne!(
        error.failure_class(),
        ExtractionFailureClass::ContentInvalid,
        "the empty-provider path must never publish a content rejection"
    );
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 2, "exactly one retry, no loop");
}

#[tokio::test]
async fn runtime_plan_does_not_retry_when_vision_returns_text() {
    let policy = LibraryRecognitionPolicy::default();
    let provider = ProviderModelSelection {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-mini".to_string(),
    };
    let gateway = CountingTextVisionGateway { calls: AtomicUsize::new(0) };

    let result = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &gateway,
        vision_provider: Some(&provider),
        vision_api_key: Some("test-key"),
        vision_base_url: None,
        vision_extra_parameters_json: None,
        file_name: Some("scan.png"),
        mime_type: Some("image/png"),
        file_bytes: valid_png_bytes(),
        recognition_policy: &policy,
    })
    .await
    .expect("non-empty vision response should be accepted on the first attempt");

    assert!(result.normalized_text.is_some(), "text response should produce a plan");
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1, "no retry for a non-empty response");
}

#[tokio::test]
async fn runtime_plan_does_not_retry_vision_provider_errors() {
    let policy = LibraryRecognitionPolicy::default();
    let provider = ProviderModelSelection {
        provider_kind: "openai".to_string(),
        model_name: "gpt-5.4-mini".to_string(),
    };
    let gateway = ErroringVisionGateway { calls: AtomicUsize::new(0) };

    let error = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &gateway,
        vision_provider: Some(&provider),
        vision_api_key: Some("test-key"),
        vision_base_url: None,
        vision_extra_parameters_json: None,
        file_name: Some("scan.png"),
        mime_type: Some("image/png"),
        file_bytes: valid_png_bytes(),
        recognition_policy: &policy,
    })
    .await
    .expect_err("provider errors must stay terminal, not retried");

    assert!(error.to_string().contains("Vision request failed"));
    assert!(!error.to_string().contains("provider exploded"));
    assert!(error.vision_calls().is_empty());
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1, "provider errors are not retried");
}

fn visual_input<'a>(
    gateway: &'a dyn LlmGateway,
    provider: Option<&'a ProviderModelSelection>,
) -> VisualRecognitionInput<'a> {
    VisualRecognitionInput {
        max_output_tokens_override: None,
        dispatch: None,
        gateway,
        vision_provider: provider,
        api_key: "test-key",
        base_url: None,
        extra_parameters_json: None,
        cancellation_token: None,
    }
}

struct AuthorityVisionGateway {
    calls: AtomicUsize,
    provider_id: uuid::Uuid,
    account_id: uuid::Uuid,
}

struct FailedVisionGateway(u16);

vision_only_gateway!(
    FailedVisionGateway,
    |gateway: &FailedVisionGateway, _request: VisionRequest| {
        use crate::integrations::{provider_budget::ProviderBudgetError, retry::ProviderCallError};
        use crate::shared::provider_http::ProviderDispatchRefusal;
        let error = match gateway.0 {
            0 => ProviderCallError::admission(ProviderBudgetError::Dispatch(
                ProviderDispatchRefusal::Admission,
            )),
            1 => ProviderCallError::local_request_rejected(),
            2 => ProviderCallError::transport(
                "private transport context",
                reqwest::Client::new().get(":invalid").build().expect_err("invalid URL"),
            ),
            status => ProviderCallError::http_status(
                "synthetic",
                reqwest::StatusCode::from_u16(status).unwrap(),
                &reqwest::header::HeaderMap::new(),
                "private provider detail",
            ),
        };
        Err(anyhow::Error::new(error).context("private context"))
    }
);

#[tokio::test]
async fn vision_failure_class_survives_both_extraction_paths_without_leaking_details() {
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    for (status, expected) in [
        (0, ExtractionFailureClass::OperationalTerminal),
        (1, ExtractionFailureClass::OperationalTerminal),
        (2, ExtractionFailureClass::OperationalRetryable),
        (400, ExtractionFailureClass::OperationalTerminal),
        (429, ExtractionFailureClass::OperationalRetryable),
        (503, ExtractionFailureClass::OperationalRetryable),
    ] {
        let gateway = FailedVisionGateway(status);
        let error = recognize_native_visuals(
            &mut native_visual_fixture(true),
            UploadFileKind::Pdf,
            visual_input(&gateway, Some(&provider)),
        )
        .await
        .err()
        .expect("provider failure");
        assert_eq!(error.failure_class(), expected);
        assert!(!error.to_string().contains("private"));
        let policy = LibraryRecognitionPolicy::default();
        let error = build_runtime_file_extraction_plan(FileExtractionRequest {
            max_output_tokens_override: None,
            vision_dispatch: None,
            cancellation_token: None,
            gateway: &gateway,
            vision_provider: Some(&provider),
            vision_api_key: Some("fixture-key"),
            vision_base_url: None,
            vision_extra_parameters_json: None,
            file_name: Some("scan.png"),
            mime_type: Some("image/png"),
            file_bytes: valid_png_bytes(),
            recognition_policy: &policy,
        })
        .await
        .expect_err("provider failure");
        assert_eq!(error.failure_class(), expected);
        assert!(!error.to_string().contains("private"));
    }
}

vision_only_gateway!(
    AuthorityVisionGateway,
    |gateway: &AuthorityVisionGateway, request: VisionRequest| {
        use crate::shared::provider_http::ProviderDispatchAuthority;
        let authority =
            request.dispatch.as_ref().ok_or_else(|| anyhow::anyhow!("missing authority"))?;
        anyhow::ensure!(
            authority.kind() == ProviderDispatchAuthority::RuntimeBinding
                && authority.provider_catalog_id() == gateway.provider_id
                && authority.account_id() == Some(gateway.account_id)
                && authority.provider_kind() == request.provider_kind
                && authority.model_name() == request.model_name
                && authority.base_url() == request.base_url_override.as_deref().unwrap_or_default()
                && authority.credential_digest()
                    == request
                        .api_key_override
                        .as_deref()
                        .map(crate::shared::provider_http::credential_fingerprint),
            "unattested route"
        );
        anyhow::ensure!(
            request.max_output_tokens_override == Some(128_000),
            "missing output bound"
        );
        let call = gateway.calls.fetch_add(1, Ordering::SeqCst);
        Ok(VisionResponse {
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
            output_text: if call == 0 { String::new() } else { r#"{"outcome":"no_text"}"#.into() },
            usage_json: serde_json::json!({}),
        })
    }
);

#[tokio::test]
async fn native_visual_preserves_trusted_binding_authority() {
    let provider_id = uuid::Uuid::from_u128(41);
    let account_id = uuid::Uuid::from_u128(42);
    let gateway = AuthorityVisionGateway { calls: AtomicUsize::new(0), provider_id, account_id };
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    let authority = crate::shared::provider_http::DispatchAuthority::runtime_binding(
        "openai",
        provider_id,
        Some(account_id),
        "vision",
        "https://example.invalid/v1",
        Some("fixture-key"),
    );
    let mut input = visual_input(&gateway, Some(&provider));
    input.dispatch = Some(&authority);
    input.max_output_tokens_override = Some(128_000);
    input.api_key = "fixture-key";
    input.base_url = Some("https://example.invalid/v1");
    let mut native = native_visual_fixture(true);
    recognize_native_visuals(&mut native, UploadFileKind::Pdf, input)
        .await
        .expect("trusted binding reaches every physical vision attempt");
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
    let mut absent = visual_input(&gateway, Some(&provider));
    absent.api_key = "fixture-key";
    absent.base_url = Some("https://example.invalid/v1");
    assert!(
        recognize_native_visuals(&mut native_visual_fixture(true), UploadFileKind::Pdf, absent)
            .await
            .is_err()
    );
    let mut mismatch = visual_input(&gateway, Some(&provider));
    mismatch.dispatch = Some(&authority);
    mismatch.api_key = "wrong-fixture-key";
    mismatch.base_url = Some("https://example.invalid/v1");
    assert!(
        recognize_native_visuals(&mut native_visual_fixture(true), UploadFileKind::Pdf, mismatch)
            .await
            .is_err()
    );
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
}

fn native_visual_fixture(
    with_visual: bool,
) -> crate::integrations::extraction_protocol::Extraction {
    use crate::integrations::extraction_protocol::{
        Extraction, Unit, VisualInput, VisualInputKind,
    };
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let bytes = valid_png_bytes();
    Extraction {
        source_sha256: "a".repeat(64),
        text_byte_limit: crate::integrations::extraction_protocol::MAX_TEXT,
        source_unit_count: 2,
        unit_kind: "page".into(),
        selected_start: 1,
        selected_end: 2,
        content: "Native page one\n\n".into(),
        warnings: Vec::new(),
        units: vec![
            Unit {
                ordinal: 1,
                sheet_name: None,
                content: "Native page one".into(),
                tables: serde_json::json!([{ "cells":["keep"] }]),
            },
            Unit {
                ordinal: 2,
                sheet_name: None,
                content: String::new(),
                tables: serde_json::json!([]),
            },
        ],
        visual_inputs: if with_visual {
            vec![VisualInput {
                kind: VisualInputKind::PdfPage,
                ordinal: 0,
                native_index: 1,
                source_unit: 2,
                source_path: None,
                mime: "image/png".into(),
                width: 2,
                height: 2,
                display_extent_96_dpi: None,
                content_base64: STANDARD.encode(&bytes),
                payload_sha256: hex::encode(Sha256::digest(&bytes)),
            }]
        } else {
            Vec::new()
        },
    }
}

#[tokio::test]
async fn native_only_requires_no_binding_and_makes_zero_provider_calls() {
    let gateway = ErroringVisionGateway { calls: AtomicUsize::new(0) };
    let mut native = native_visual_fixture(false);
    let receipt =
        recognize_native_visuals(&mut native, UploadFileKind::Pdf, visual_input(&gateway, None))
            .await
            .unwrap();
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    assert!(receipt.calls.is_empty());
    assert_eq!(native.units[0].content, "Native page one");
}

#[tokio::test]
async fn scan_text_merges_only_into_owning_page_and_retains_native_tables() {
    let gateway = CountingTextVisionGateway { calls: AtomicUsize::new(0) };
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    let mut native = native_visual_fixture(true);
    let receipt = recognize_native_visuals(
        &mut native,
        UploadFileKind::Pdf,
        visual_input(&gateway, Some(&provider)),
    )
    .await
    .unwrap();
    assert_eq!(native.units[0].content, "Native page one");
    assert_eq!(native.units[0].tables[0]["cells"][0], "keep");
    assert_eq!(native.units[1].content, "Counted OCR text");
    assert_eq!(receipt.outcomes[0]["source_unit"], 2);
    assert_eq!(receipt.outcomes[0]["outcome"]["outcome"], "text");
    assert!(receipt.outcomes[0].get("content_base64").is_none());
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    // The sole projection persists inventory identity, not the encoded payload.
    let mut output = xberg::project_native(
        native,
        xberg::XbergSource {
            file_name: Some("hybrid.pdf"),
            mime_type: Some("application/pdf"),
            source_format: "pdf",
        },
    )
    .unwrap();
    receipt.apply(&mut output);
    assert!(!output.source_map.to_string().contains("contentBase64"));
    assert!(output.extracted_images.is_empty());
}

struct OutcomeGateway {
    responses: std::sync::Mutex<std::collections::VecDeque<Option<&'static str>>>,
    calls: AtomicUsize,
}

fn admitted_visual_fixture(
    native: crate::integrations::extraction_protocol::Extraction,
    mime: &str,
) -> crate::integrations::extraction_protocol::Extraction {
    use crate::integrations::extraction_protocol::{
        Limits, MAX_OUTPUT, MAX_TEXT, Operation, Request, Response,
    };
    let request = Request {
        path: "/fixture/document".into(),
        operation: Operation::Extract,
        mime: mime.into(),
        source_sha256: native.source_sha256.clone(),
        limits: Limits { output_bytes: MAX_OUTPUT, text_bytes: MAX_TEXT, ..Limits::default() },
        start: None,
        end: None,
    };
    xberg::validate_response(Response::new(Ok(native), std::time::Duration::ZERO, None), &request)
        .expect("native fixture passes the real admitted parent boundary")
}

#[test]
fn pptx_native_tables_survive_plan_and_structured_consumer_without_duplicates() {
    use crate::services::ingest::structured_preparation::{
        PrepareStructuredRevisionCommand, StructuredPreparationService,
    };
    use crate::shared::extraction::{
        structured_document::StructuredBlockKind, table_logical::LogicalTable,
        table_markdown::parse_markdown_table_rows,
    };

    let mime = "application/vnd.openxmlformats-officedocument.presentationml.presentation";
    let mut native = native_visual_fixture(false);
    native.unit_kind = "slide".into();
    for (unit, (material, stock, bin)) in
        native.units.iter_mut().zip([("Copper", "7", "AB-0042"), ("Silver", "19", "ZX-0019")])
    {
        unit.content = format!(
            "## Inventory {}\n\n| Material | Stock | Bin |\n| --- | --- | --- |\n| {material} | {stock} | {bin} |",
            unit.ordinal
        );
        unit.tables = serde_json::json!([{
            "page_number":unit.ordinal,
            "cells":[["Material","Stock","Bin"],[material,stock,bin]],
        }]);
    }
    native.content =
        native.units.iter().map(|u| u.content.as_str()).collect::<Vec<_>>().join("\n\n");
    let expected = native.units.iter().map(|u| u.tables.clone()).collect::<Vec<_>>();
    let output = xberg::project_native(
        admitted_visual_fixture(native, mime),
        xberg::XbergSource {
            file_name: Some("inventory.pptx"),
            mime_type: Some(mime),
            source_format: "pptx",
        },
    )
    .unwrap();
    let plan = build_plan_from_extraction(
        UploadFileKind::Pptx,
        output,
        xberg_owned_recognition_profile(UploadFileKind::Pptx, "pptx").unwrap(),
    );
    for (index, table) in expected.iter().enumerate() {
        assert_eq!(&plan.source_map["units"][index]["tables"], table);
        assert_eq!(plan.source_map["units"][index]["ordinal"], index + 1);
    }
    let normalized = plan.normalized_text.unwrap();
    for marker in ["Copper", "Silver", "AB-0042", "ZX-0019"] {
        assert_eq!(normalized.matches(marker).count(), 1);
    }
    let prepared = StructuredPreparationService::new()
        .prepare_revision(
            PrepareStructuredRevisionCommand {
                revision_id: uuid::Uuid::now_v7(),
                document_id: uuid::Uuid::now_v7(),
                workspace_id: uuid::Uuid::now_v7(),
                library_id: uuid::Uuid::now_v7(),
                preparation_state: "prepared".into(),
                normalization_profile: plan.normalization_profile,
                source_format: "pptx".into(),
                language_code: Some("en".into()),
                source_text: plan.source_text.unwrap(),
                normalized_text: normalized,
                structure_hints: plan.structure_hints,
                typed_fact_count: 0,
                prepared_at: chrono::Utc::now(),
            },
            &tokio_util::sync::CancellationToken::new(),
        )
        .unwrap();
    let tables = prepared
        .ordered_blocks
        .iter()
        .filter(|block| block.block_kind == StructuredBlockKind::Table)
        .collect::<Vec<_>>();
    assert_eq!(tables.len(), 2);
    for (block, material) in tables.into_iter().zip(["Copper", "Silver"]) {
        let rows = parse_markdown_table_rows(&block.text);
        let logical = LogicalTable::from_markdown_rows(&rows).unwrap();
        assert_eq!(logical.cell_at(1, 0).unwrap().text(), material);
        assert_eq!(logical.rows(), 2);
        assert_eq!(logical.columns(), 3);
    }
    assert_eq!(
        prepared
            .chunk_windows
            .iter()
            .filter(|chunk| chunk.chunk_kind == StructuredBlockKind::TableRow)
            .count(),
        2
    );
}

#[tokio::test]
async fn review_vision_literals_survive_native_markdown_and_final_projection() {
    use crate::integrations::extraction_protocol::VisualInputKind;
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    let literal = r"C:\_cache &lt;tag&gt; &quot; &amp;lt;";
    for (kind, mime, format) in [
        (UploadFileKind::Pdf, "application/pdf", "pdf"),
        (
            UploadFileKind::Docx,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "docx",
        ),
        (
            UploadFileKind::Pptx,
            "application/vnd.openxmlformats-officedocument.presentationml.presentation",
            "pptx",
        ),
    ] {
        let gateway = outcome_gateway(vec![Some(
            r#"{"outcome":"text","text":"C:\\_cache &lt;tag&gt; &quot; &amp;lt;"}"#,
        )]);
        let mut native = native_visual_fixture(true);
        native.units[0].content = r"Native\_identifier -&gt; value; literal=&amp;quot;".into();
        if kind == UploadFileKind::Docx {
            native.units.truncate(1);
            native.source_unit_count = 1;
            native.selected_end = 1;
            native.unit_kind = "document".into();
            native.visual_inputs[0].source_unit = 1;
            native.visual_inputs[0].native_index = 0;
            native.visual_inputs[0].source_path = Some("word/media/image1.png".into());
            native.visual_inputs[0].kind = VisualInputKind::EmbeddedPicture;
        } else if kind == UploadFileKind::Pptx {
            native.unit_kind = "slide".into();
            native.visual_inputs[0].native_index = 0;
            native.visual_inputs[0].kind = VisualInputKind::EmbeddedPicture;
        }
        native.content =
            native.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
        let mut native = admitted_visual_fixture(native, mime);
        let receipt =
            recognize_native_visuals(&mut native, kind, visual_input(&gateway, Some(&provider)))
                .await
                .unwrap();
        let mut output = xberg::project_native(
            native,
            xberg::XbergSource {
                file_name: Some("fixture"),
                mime_type: Some(mime),
                source_format: format,
            },
        )
        .unwrap();
        receipt.apply(&mut output);
        let plan = build_plan_from_extraction(
            kind,
            output,
            xberg_owned_recognition_profile(kind, format).unwrap(),
        );
        assert!(plan.source_text.as_deref().unwrap().contains("Native_identifier -> value"));
        assert!(plan.source_text.as_deref().unwrap().contains("literal=&quot;"));
        assert!(
            plan.source_text.as_deref().unwrap().contains(literal),
            "raw Vision source text for {format}"
        );
        assert!(
            plan.normalized_text.as_deref().unwrap().contains(literal),
            "published Vision text for {format}"
        );
        assert!(
            plan.structure_hints.lines.iter().any(|line| line.text == literal),
            "structured preparation text for {format}"
        );
    }
}

#[tokio::test]
async fn review_recognition_retains_the_runtime_admitted_text_budget_for_both_outcomes() {
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    for response in [r#"{"outcome":"text","text":"x"}"#, r#"{"outcome":"no_text"}"#] {
        let gateway = outcome_gateway(vec![Some(response)]);
        let mut native = native_visual_fixture(true);
        native.units[0].content = "n".repeat(40 * 1024 * 1024);
        native.content =
            native.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
        let mut native = admitted_visual_fixture(native, "application/pdf");
        let receipt = recognize_native_visuals(
            &mut native,
            UploadFileKind::Pdf,
            visual_input(&gateway, Some(&provider)),
        )
        .await
        .expect("both outcomes must retain the admitted 64 MiB budget, not substitute 32 MiB");
        assert_eq!(receipt.calls.len(), 1);
        assert!(native.content.len() > 32 * 1024 * 1024);
        assert!(native.content.len() <= crate::integrations::extraction_protocol::MAX_TEXT);
    }
}

#[tokio::test]
async fn recognized_text_cannot_bypass_the_native_text_budget() {
    let gateway = CountingTextVisionGateway { calls: AtomicUsize::new(0) };
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    let mut native = native_visual_fixture(true);
    native.text_byte_limit = 64;
    native.units[0].content = "n".repeat(native.text_byte_limit - 2);
    native.content =
        native.units.iter().map(|unit| unit.content.as_str()).collect::<Vec<_>>().join("\n\n");
    let before = Sha256::digest(native.content.as_bytes());
    let error = recognize_native_visuals(
        &mut native,
        UploadFileKind::Pdf,
        visual_input(&gateway, Some(&provider)),
    )
    .await
    .err()
    .expect("Vision must share the document text budget");
    assert_eq!(error.vision_calls().len(), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(Sha256::digest(native.content.as_bytes()), before);
    assert!(native.units[1].content.is_empty());
    assert!(error.to_string().contains("text budget"));
}

#[tokio::test]
async fn review_over_budget_native_text_is_refused_before_any_provider_call() {
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    for with_visual in [false, true] {
        let gateway = outcome_gateway(vec![Some(r#"{"outcome":"no_text"}"#)]);
        let mut native = native_visual_fixture(with_visual);
        native.text_byte_limit = native.content.len() - 1;
        let error = recognize_native_visuals(
            &mut native,
            UploadFileKind::Pdf,
            visual_input(&gateway, Some(&provider)),
        )
        .await
        .err()
        .expect("an admitted text ceiling cannot depend on a later no_text outcome");
        assert!(error.to_string().contains("admitted extraction text budget"));
        assert!(error.vision_calls().is_empty());
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
        assert!(native.units[1].content.is_empty());
    }
}
vision_only_gateway!(OutcomeGateway, |gateway: &OutcomeGateway, request: VisionRequest| {
    let index = gateway.calls.fetch_add(1, Ordering::SeqCst);
    let text = gateway.responses.lock().unwrap().pop_front().expect("bounded calls");
    match text {
        Some(text) => Ok(VisionResponse {
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
            output_text: text.into(),
            usage_json: serde_json::json!({"total_tokens":index + 10}),
        }),
        None => Err(anyhow!("sensitive provider error")),
    }
});

fn outcome_gateway(responses: Vec<Option<&'static str>>) -> OutcomeGateway {
    OutcomeGateway {
        responses: std::sync::Mutex::new(responses.into()),
        calls: AtomicUsize::new(0),
    }
}

#[tokio::test]
async fn no_text_for_an_image_only_document_fails_with_its_usage_receipt() {
    let gateway = outcome_gateway(vec![Some(r#"{"outcome":"no_text"}"#)]);
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    let policy = LibraryRecognitionPolicy::default();
    let error = build_runtime_file_extraction_plan(FileExtractionRequest {
        max_output_tokens_override: None,
        vision_dispatch: None,
        cancellation_token: None,
        gateway: &gateway,
        vision_provider: Some(&provider),
        vision_api_key: None,
        vision_base_url: None,
        vision_extra_parameters_json: None,
        file_name: Some("blank.gif"),
        mime_type: Some("image/gif"),
        file_bytes: valid_png_bytes(),
        recognition_policy: &policy,
    })
    .await
    .expect_err("a resolved blank image is not a readable document");
    assert_eq!(error.vision_calls().len(), 1);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert!(error.to_string().contains("no readable text"));
}

struct CancellingVisionGateway {
    token: tokio_util::sync::CancellationToken,
    calls: AtomicUsize,
}
vision_only_gateway!(
    CancellingVisionGateway,
    |gateway: &CancellingVisionGateway, request: VisionRequest| {
        gateway.calls.fetch_add(1, Ordering::SeqCst);
        gateway.token.cancel();
        Ok(VisionResponse {
            provider_kind: request.provider_kind.clone(),
            model_name: request.model_name.clone(),
            output_text: String::new(),
            usage_json: serde_json::json!({"total_tokens":17}),
        })
    }
);

#[tokio::test]
async fn cancellation_does_not_retry_or_discard_an_already_returned_paid_receipt() {
    let gateway = CancellingVisionGateway {
        token: tokio_util::sync::CancellationToken::new(),
        calls: AtomicUsize::new(0),
    };
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    let mut native = native_visual_fixture(true);
    let mut input = visual_input(&gateway, Some(&provider));
    input.cancellation_token = Some(&gateway.token);
    let error =
        recognize_native_visuals(&mut native, UploadFileKind::Pdf, input).await.err().unwrap();
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert_eq!(error.vision_calls().len(), 1);
    assert_eq!(error.vision_calls()[0].usage_json["total_tokens"], 17);
    assert!(native.units[1].content.is_empty());
}

#[tokio::test]
async fn explicit_no_text_is_a_resolved_visual_not_an_empty_response_retry() {
    let gateway = outcome_gateway(vec![Some(r#"{"outcome":"no_text"}"#)]);
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    let mut native = native_visual_fixture(true);
    let receipt = recognize_native_visuals(
        &mut native,
        UploadFileKind::Pdf,
        visual_input(&gateway, Some(&provider)),
    )
    .await
    .unwrap();
    assert_eq!(receipt.outcomes[0]["outcome"]["outcome"], "no_text");
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 1);
    assert!(native.units[1].content.is_empty());
    assert_eq!(native.units[0].content, "Native page one");
}

#[tokio::test]
async fn malformed_and_provider_failures_never_publish_partial_recognition() {
    use crate::integrations::extraction_protocol::VisualInputKind;
    for last in [Some("malformed"), Some(r#"{"outcome":"text","text":"  "}"#), None] {
        let gateway =
            outcome_gateway(vec![Some(r#"{"outcome":"text","text":"first image"}"#), last]);
        let provider =
            ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
        let mut native = native_visual_fixture(true);
        native.visual_inputs[0].kind = VisualInputKind::EmbeddedPicture;
        let mut second = serde_json::from_value::<
            crate::integrations::extraction_protocol::VisualInput,
        >(serde_json::to_value(&native.visual_inputs[0]).unwrap())
        .unwrap();
        second.ordinal = 1;
        second.native_index = 3;
        native.visual_inputs.push(second);
        let error = recognize_native_visuals(
            &mut native,
            UploadFileKind::Pdf,
            visual_input(&gateway, Some(&provider)),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.vision_calls().len(), if last.is_some() { 2 } else { 1 });
        assert_eq!(native.units[0].content, "Native page one");
        assert!(native.units[1].content.is_empty());
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
        assert!(!error.to_string().contains("sensitive"));
        let admission = UploadAdmissionError::from_file_extract_error(
            "scan.pdf",
            Some("application/pdf"),
            100,
            &error,
        );
        assert_eq!(admission.vision_calls.len(), error.vision_calls().len());
    }
}

#[tokio::test]
async fn empty_then_provider_failure_preserves_only_the_returned_usage() {
    let gateway = outcome_gateway(vec![Some(""), None]);
    let provider =
        ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
    let mut native = native_visual_fixture(true);
    let error = recognize_native_visuals(
        &mut native,
        UploadFileKind::Pdf,
        visual_input(&gateway, Some(&provider)),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.vision_calls().len(), 1);
    assert_eq!(error.vision_calls()[0].usage_json["total_tokens"], 10);
    assert_eq!(gateway.calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn double_page_picture_or_tampered_payload_is_rejected_before_paid_calls() {
    for fault in 0..3 {
        let gateway = ErroringVisionGateway { calls: AtomicUsize::new(0) };
        let provider =
            ProviderModelSelection { provider_kind: "openai".into(), model_name: "vision".into() };
        let mut native = native_visual_fixture(true);
        match fault {
            0 => native.visual_inputs[0].payload_sha256 = "b".repeat(64),
            1 => native.visual_inputs[0].source_unit = 3,
            _ => {
                let mut picture = serde_json::from_value::<
                    crate::integrations::extraction_protocol::VisualInput,
                >(
                    serde_json::to_value(&native.visual_inputs[0]).unwrap()
                )
                .unwrap();
                picture.kind =
                    crate::integrations::extraction_protocol::VisualInputKind::EmbeddedPicture;
                picture.ordinal = 1;
                native.visual_inputs.push(picture);
            }
        }
        assert!(
            recognize_native_visuals(
                &mut native,
                UploadFileKind::Pdf,
                visual_input(&gateway, Some(&provider))
            )
            .await
            .is_err()
        );
        assert_eq!(gateway.calls.load(Ordering::SeqCst), 0);
    }
}
