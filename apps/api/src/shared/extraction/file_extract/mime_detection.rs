use std::path::Path;

use crate::shared::extraction;

use super::{
    DOCX_EXTENSIONS, DOCX_MIME_TYPES, FileExtractError, GENERIC_BINARY_MIME_TYPES, HTML_EXTENSIONS,
    HTML_MIME_TYPES, IMAGE_EXTENSIONS, PPTX_EXTENSIONS, PPTX_MIME_TYPES, SPREADSHEET_EXTENSIONS,
    SPREADSHEET_MIME_TYPES, TEXT_LIKE_EXTENSIONS, TEXT_LIKE_MIME_TYPES, UploadFileKind,
};

pub(super) fn detect_declared_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
) -> Option<UploadFileKind> {
    let normalized_mime = normalized_upload_mime_essence(mime_type);
    let extension = normalized_upload_extension(file_name);

    if normalized_mime.as_deref() == Some("application/pdf") || extension.as_deref() == Some("pdf")
    {
        return Some(UploadFileKind::Pdf);
    }
    if normalized_mime.as_deref().is_some_and(|value| value.starts_with("image/"))
        || extension.as_deref().is_some_and(|value| IMAGE_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Image);
    }
    if normalized_mime.as_deref().is_some_and(|value| DOCX_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| DOCX_EXTENSIONS.contains(&value))
    {
        return Some(UploadFileKind::Docx);
    }
    if normalized_mime.as_deref().is_some_and(|value| SPREADSHEET_MIME_TYPES.contains(&value))
        || extension.as_deref().is_some_and(|value| SPREADSHEET_EXTENSIONS.contains(&value))
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

fn is_supported_upload_mime_type(mime_type: &str) -> bool {
    mime_type == "application/pdf"
        || mime_type.starts_with("image/")
        || HTML_MIME_TYPES.contains(&mime_type)
        || TEXT_LIKE_MIME_TYPES.contains(&mime_type)
        || mime_type.starts_with("text/")
        || DOCX_MIME_TYPES.contains(&mime_type)
        || SPREADSHEET_MIME_TYPES.contains(&mime_type)
        || PPTX_MIME_TYPES.contains(&mime_type)
}

fn mime_type_is_generic_binary(mime_type: &str) -> bool {
    GENERIC_BINARY_MIME_TYPES.contains(&mime_type)
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

fn declares_unsupported_mime_type(mime_type: Option<&str>) -> bool {
    // Unknown extensions alone never reject — foldered uploads routinely include
    // files like `.env.backup.20260116-162838` whose extension is meaningless.
    // Content sniffing (`utf8_payload_looks_binary`) is the authority for text
    // admission. An explicit non-generic MIME type that names an unsupported
    // format still short-circuits, so declared binaries like `video/mp4` are
    // still rejected without a UTF-8 decode pass.
    if let Some(mime_type) = normalized_upload_mime_essence(mime_type)
        && !mime_type_is_generic_binary(&mime_type)
        && !is_supported_upload_mime_type(&mime_type)
    {
        return true;
    }
    false
}

pub fn detect_upload_file_kind(
    file_name: Option<&str>,
    mime_type: Option<&str>,
    file_bytes: &[u8],
) -> UploadFileKind {
    if let Some(file_kind) = detect_declared_upload_file_kind(file_name, mime_type) {
        return file_kind;
    }
    if declares_unsupported_mime_type(mime_type) {
        return UploadFileKind::Binary;
    }
    if let Ok(decoded_text) = std::str::from_utf8(file_bytes)
        && !utf8_payload_looks_binary(decoded_text)
    {
        return UploadFileKind::TextLike;
    }
    UploadFileKind::Binary
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
