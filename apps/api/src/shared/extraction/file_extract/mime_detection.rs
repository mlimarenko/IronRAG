use std::path::Path;

use crate::shared::extraction;

use super::FileExtractError;
use super::kind::{
    DOCX_EXTENSIONS, DOCX_MIME_TYPES, HTML_EXTENSIONS, HTML_MIME_TYPES, IMAGE_EXTENSIONS,
    IMAGE_MIME_TYPES, PPTX_EXTENSIONS, PPTX_MIME_TYPES, TEXT_LIKE_EXTENSIONS, TEXT_LIKE_MIME_TYPES,
    UploadFileKind,
};

/// Shared structural media-class classifier from declared inputs only
/// (`file_name` extension + `mime_type`), no byte sniffing. This is the
/// no-payload variant of [`detect_upload_file_kind`], used by callers that
/// only hold persisted document/revision metadata (admission resolution,
/// document-parentage backfill) and never see the original bytes.
#[must_use]
pub fn detect_declared_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Option<UploadFileKind> {
    let normalized_mime = normalized_upload_mime_essence(mime_type);
    let extension = normalized_upload_extension(file_name);

    if normalized_mime.as_deref() == Some("application/pdf") || extension.as_deref() == Some("pdf")
    {
        return Some(UploadFileKind::Pdf);
    }
    if normalized_mime.as_deref().is_some_and(|value| IMAGE_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| IMAGE_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Image);
    }
    if normalized_mime.as_deref().is_some_and(|value| DOCX_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| DOCX_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Docx);
    }
    if extraction::tabular::is_tabular_mime_type(normalized_mime.as_deref())
        || extraction::tabular::is_tabular_extension(file_name)
    {
        return Some(UploadFileKind::Spreadsheet);
    }
    if normalized_mime.as_deref().is_some_and(|value| PPTX_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| PPTX_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Pptx);
    }
    if normalized_mime
        .as_deref()
        .is_some_and(|value| value.starts_with("text/") || TEXT_LIKE_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| {
            TEXT_LIKE_EXTENSIONS.contains(&value) || HTML_EXTENSIONS.contains(&value)
        })
    {
        return Some(UploadFileKind::TextLike);
    }
    None
}

fn normalized_upload_extension(file_name: Option<&str>) -> Option<String> {
    file_name
        .and_then(|value| Path::new(value).extension().and_then(|ext| ext.to_str()))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_ascii_lowercase)
}

fn normalized_upload_mime_type(mime_type: Option<&str>) -> Option<String> {
    mime_type.map(str::trim).filter(|value| !value.is_empty()).map(str::to_ascii_lowercase)
}

fn normalized_upload_mime_essence(mime_type: Option<&str>) -> Option<String> {
    normalized_upload_mime_type(mime_type)
        .and_then(|value| value.split(';').next().map(str::trim).map(str::to_string))
}

pub(super) fn declared_payload_is_html(file_name: Option<&str>, mime_type: Option<&str>) -> bool {
    let normalized_mime = normalized_upload_mime_essence(mime_type);
    let extension = normalized_upload_extension(file_name);
    normalized_mime.as_deref().is_some_and(|value| HTML_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| HTML_EXTENSIONS.contains(&value))
}

pub(super) fn payload_looks_like_html(file_bytes: &[u8]) -> bool {
    std::str::from_utf8(file_bytes)
        .is_ok_and(extraction::html_main_content::payload_looks_like_html_document)
}

/// `%PDF-` header every PDF begins with, used as byte evidence when a payload
/// was declared text-like (or not declared at all).
const PDF_MAGIC: &[u8] = b"%PDF-";

#[must_use]
pub fn detect_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> UploadFileKind {
    let declared = detect_declared_upload_file_kind(file_name, mime_type);
    // A declared dedicated kind (pdf, image, docx, pptx, spreadsheet) is
    // authoritative even when the bytes are a ZIP archive shared with other
    // OOXML/ODF kinds, so an explicitly declared DOCX/PPTX keeps its route. A
    // declared *text-like* kind is not authoritative: a server may serve a real
    // workbook, PDF or image under `text/plain` or `application/octet-stream`,
    // and decoding those bytes as UTF-8 would fabricate a `ContentInvalid`
    // verdict.
    if let Some(file_kind) = declared
        && file_kind != UploadFileKind::TextLike
    {
        return file_kind;
    }
    // A declared text-like kind (or no declaration at all) is trusted only when
    // the bytes are text by the existing heuristic. This is checked before any
    // format signature because the ZIP/OLE2 container prefix is shared with a
    // text payload that merely begins with it (`PK\x03\x04hello`): such bytes
    // are text and must not be decoded as a workbook.
    if utf8_payload_is_text(file_bytes) {
        return UploadFileKind::TextLike;
    }
    // The bytes are not text: only a recognised format signature may claim
    // them. Each route's local failure is operational, so a recognised binary
    // is never reported as unsupported content.
    if let Some(file_kind) = byte_evidence_upload_file_kind(file_bytes) {
        return file_kind;
    }
    // Declared text whose bytes are neither text nor a known format keeps its
    // declared kind, so the upload gate applies `InvalidUtf8`; with no
    // declaration at all the bytes are raw binary. Both are legitimate content
    // rejections, not an operational fault.
    declared.unwrap_or(UploadFileKind::Binary)
}

/// The upload kind proven by the bytes alone, used once the payload is known
/// not to be text. Reuses the workbook-container test from `tabular` and the
/// decoder crate's own raster-image magic scan; no second signature table.
fn byte_evidence_upload_file_kind(file_bytes: &[u8]) -> Option<UploadFileKind> {
    if file_bytes.starts_with(PDF_MAGIC) {
        return Some(UploadFileKind::Pdf);
    }
    if extraction::image::bytes_look_like_raster_image(file_bytes) {
        return Some(UploadFileKind::Image);
    }
    if extraction::tabular::has_workbook_container_signature(file_bytes) {
        return Some(UploadFileKind::Spreadsheet);
    }
    None
}

fn utf8_payload_is_text(file_bytes: &[u8]) -> bool {
    std::str::from_utf8(file_bytes).is_ok_and(|decoded| !utf8_payload_looks_binary(decoded))
}

pub fn validate_upload_file_admission(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> Result<UploadFileKind, FileExtractError> {
    let file_kind = detect_upload_file_kind(file_name, mime_type, file_bytes);
    match file_kind {
        UploadFileKind::Binary => Err(FileExtractError::UnsupportedBinary),
        UploadFileKind::TextLike => {
            if !declared_payload_is_html(file_name, mime_type) {
                std::str::from_utf8(file_bytes).map_err(|_| FileExtractError::InvalidUtf8)?;
            }
            Ok(file_kind)
        }
        UploadFileKind::Pdf
        | UploadFileKind::Image
        | UploadFileKind::Docx
        | UploadFileKind::Spreadsheet
        | UploadFileKind::Pptx => Ok(file_kind),
    }
}

fn utf8_payload_looks_binary(decoded_text: &str) -> bool {
    if decoded_text.chars().any(|ch| ch == '\0') {
        return true;
    }
    let non_whitespace_control_count = decoded_text
        .chars()
        .filter(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t' | '\u{000C}'))
        .count();
    let total_char_count = decoded_text.chars().count();
    if total_char_count == 0 {
        return false;
    }
    non_whitespace_control_count.saturating_mul(5) >= total_char_count
}
