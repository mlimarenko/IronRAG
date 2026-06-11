use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub(super) struct AppendableDocumentSource {
    pub(super) raw_bytes: Vec<u8>,
    pub(super) mime_type: String,
    pub(super) title: Option<String>,
    pub(super) language_code: Option<String>,
}

/// Returns true when `mime_type` is safe for source-format append.
pub(super) fn is_appendable_text_mime(mime_type: &str) -> bool {
    let normalized = mime_type.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return false;
    }
    if normalized.starts_with("text/") {
        return true;
    }
    matches!(normalized.as_str(), "application/jsonl" | "application/x-ndjson")
}

fn append_separator_for_mime(mime_type: &str) -> &'static [u8] {
    match mime_type.trim().to_ascii_lowercase().as_str() {
        "application/jsonl" | "application/x-ndjson" | "text/csv" | "text/tab-separated-values" => {
            b"\n"
        }
        "text/markdown" | "text/plain" => b"\n\n",
        _ => b"\n",
    }
}

pub(super) fn source_uri_for_inline_payload(
    operation_kind: &str,
    source_identity: Option<&str>,
    file_name: Option<&str>,
) -> String {
    if let Some(source_identity) = source_identity {
        return format!("mcp://payload/{source_identity}");
    }

    match file_name {
        Some(file_name) => format!("{operation_kind}://{file_name}"),
        None => format!("{operation_kind}://inline"),
    }
}

pub(super) fn infer_inline_mime_type(
    requested_mime_type: Option<&str>,
    file_name: Option<&str>,
    fallback_kind: &str,
) -> String {
    if let Some(mime_type) = requested_mime_type
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter(|value| !value.eq_ignore_ascii_case("application/octet-stream"))
    {
        return mime_type.to_string();
    }

    match file_name.and_then(file_extension) {
        Some(extension) if extension == "pdf" => "application/pdf".to_string(),
        Some(extension) if extension == "docx" => {
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document".to_string()
        }
        Some(extension) if extension == "xls" => "application/vnd.ms-excel".to_string(),
        Some(extension) if extension == "xlsx" => {
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".to_string()
        }
        Some(extension) if extension == "xlsb" => {
            "application/vnd.ms-excel.sheet.binary.macroenabled.12".to_string()
        }
        Some(extension) if extension == "ods" => {
            "application/vnd.oasis.opendocument.spreadsheet".to_string()
        }
        Some(extension) if extension == "csv" => "text/csv".to_string(),
        Some(extension) if extension == "tsv" => "text/tab-separated-values".to_string(),
        Some(extension) if extension == "pptx" => {
            "application/vnd.openxmlformats-officedocument.presentationml.presentation".to_string()
        }
        Some(extension) if extension == "md" => "text/markdown".to_string(),
        Some(extension) if extension == "txt" => "text/plain".to_string(),
        Some(extension) if extension == "json" => "application/json".to_string(),
        Some(extension) if extension == "png" => "image/png".to_string(),
        Some(extension) if extension == "jpg" || extension == "jpeg" => "image/jpeg".to_string(),
        Some(extension) if extension == "gif" => "image/gif".to_string(),
        Some(extension) if extension == "bmp" => "image/bmp".to_string(),
        Some(extension) if extension == "webp" => "image/webp".to_string(),
        Some(extension) if extension == "svg" => "image/svg+xml".to_string(),
        Some(extension) if extension == "tif" || extension == "tiff" => "image/tiff".to_string(),
        _ if fallback_kind == "append" => "text/plain".to_string(),
        _ if fallback_kind == "edit" => "text/markdown".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn file_extension(file_name: &str) -> Option<String> {
    let (_, extension) = file_name.rsplit_once('.')?;
    Some(extension.trim().to_ascii_lowercase())
}

pub(super) fn edited_markdown_file_name(title: Option<&str>, document_id: Uuid) -> String {
    let base = title.map(str::trim).filter(|value| !value.is_empty()).unwrap_or("document");
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem.trim());
    let normalized_stem =
        if stem.is_empty() { format!("document-{document_id}") } else { stem.to_string() };
    format!("{normalized_stem}.md")
}

pub(super) fn sha256_hex_text(value: &str) -> String {
    sha256_hex_bytes(value.as_bytes())
}

pub(super) fn sha256_hex_bytes(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    hex::encode(hasher.finalize())
}

/// Canonical byte-level merge for append.
pub(super) fn merge_appended_bytes(
    existing_bytes: &[u8],
    appended_text: &str,
    mime_type: &str,
) -> Vec<u8> {
    let existing_end = existing_bytes
        .iter()
        .rposition(|byte| !matches!(byte, b'\r' | b'\n' | b' ' | b'\t'))
        .map_or(0, |index| index + 1);
    let trimmed_existing = &existing_bytes[..existing_end];
    let trimmed_appended = appended_text.trim_start_matches(['\r', '\n', ' ', '\t']);
    if trimmed_existing.is_empty() {
        return trimmed_appended.as_bytes().to_vec();
    }
    if trimmed_appended.is_empty() {
        return trimmed_existing.to_vec();
    }
    let mut merged = Vec::with_capacity(trimmed_existing.len() + 2 + trimmed_appended.len());
    merged.extend_from_slice(trimmed_existing);
    merged.extend_from_slice(append_separator_for_mime(mime_type));
    merged.extend_from_slice(trimmed_appended.as_bytes());
    merged
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::{
        edited_markdown_file_name, infer_inline_mime_type, is_appendable_text_mime,
        merge_appended_bytes, source_uri_for_inline_payload,
    };
    use crate::shared::extraction::record_jsonl::extract_record_jsonl;

    #[test]
    fn infers_spreadsheet_inline_mime_type_from_file_name() {
        assert_eq!(
            infer_inline_mime_type(None, Some("inventory.xlsx"), "replace"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(
            infer_inline_mime_type(None, Some("inventory.xls"), "replace"),
            "application/vnd.ms-excel"
        );
        assert_eq!(
            infer_inline_mime_type(None, Some("inventory.ods"), "replace"),
            "application/vnd.oasis.opendocument.spreadsheet"
        );
        assert_eq!(infer_inline_mime_type(None, Some("inventory.csv"), "replace"), "text/csv");
        assert_eq!(
            infer_inline_mime_type(None, Some("inventory.tsv"), "replace"),
            "text/tab-separated-values"
        );
    }

    #[test]
    fn builds_canonical_markdown_file_name_for_edited_sources() {
        assert_eq!(edited_markdown_file_name(Some("Inventory.xlsx"), Uuid::nil()), "Inventory.md");
        assert_eq!(
            edited_markdown_file_name(Some("Quarterly report"), Uuid::nil()),
            "Quarterly report.md"
        );
    }

    #[test]
    fn edit_inline_sources_use_canonical_inline_uri() {
        assert_eq!(source_uri_for_inline_payload("edit", None, None), "edit://inline");
    }

    #[test]
    fn append_merges_raw_bytes_with_single_newline() {
        let existing = b"{\"id\":\"msg-1\",\"text\":\"hello\"}\n";
        let appended = "\n{\"id\":\"msg-2\",\"text\":\"world\"}";
        let merged = merge_appended_bytes(existing, appended, "application/x-ndjson");
        assert_eq!(
            std::str::from_utf8(&merged).unwrap(),
            "{\"id\":\"msg-1\",\"text\":\"hello\"}\n{\"id\":\"msg-2\",\"text\":\"world\"}"
        );
    }

    #[test]
    fn append_keeps_record_jsonl_parseable_after_two_appends() {
        // Byte-level merge keeps the JSONL source parseable across
        // repeated appends instead of feeding rendered extraction headers
        // back into the next extraction pass.
        let initial = br#"{"id":"msg-1","kind":"message","text":"hello"}"#.to_vec();
        let after_first = merge_appended_bytes(
            &initial,
            "\n{\"id\":\"msg-2\",\"kind\":\"message\",\"text\":\"world\"}",
            "application/x-ndjson",
        );
        let after_second = merge_appended_bytes(
            &after_first,
            "\n{\"id\":\"msg-3\",\"kind\":\"message\",\"text\":\"again\"}",
            "application/x-ndjson",
        );

        let extracted = extract_record_jsonl(&after_second).expect("merged JSONL must parse");
        assert_eq!(extracted.source_metadata.line_count, 4); // header + 3 records
        assert!(extracted.content_text.contains("unit_count=3"));
        assert!(extracted.content_text.contains("id=msg-1"));
        assert!(extracted.content_text.contains("id=msg-2"));
        assert!(extracted.content_text.contains("id=msg-3"));
    }

    #[test]
    fn append_preserves_markdown_paragraph_spacing() {
        let merged =
            merge_appended_bytes(b"# Existing\n\nBody\n", "Next paragraph", "text/markdown");
        assert_eq!(std::str::from_utf8(&merged).unwrap(), "# Existing\n\nBody\n\nNext paragraph");
    }

    #[test]
    fn append_handles_empty_existing_or_appended_input() {
        assert_eq!(merge_appended_bytes(b"", "first record", "text/plain"), b"first record");
        assert_eq!(
            merge_appended_bytes(b"only existing", "  \n  ", "text/plain"),
            b"only existing"
        );
        assert!(merge_appended_bytes(b"", "  ", "text/plain").is_empty());
    }

    #[test]
    fn is_appendable_text_mime_accepts_text_like_sources() {
        assert!(is_appendable_text_mime("text/plain"));
        assert!(is_appendable_text_mime("text/markdown"));
        assert!(is_appendable_text_mime("text/csv"));
        assert!(is_appendable_text_mime("application/x-ndjson"));
        assert!(is_appendable_text_mime("APPLICATION/X-NDJSON"));
    }

    #[test]
    fn is_appendable_text_mime_rejects_binary_sources() {
        assert!(!is_appendable_text_mime("application/pdf"));
        assert!(!is_appendable_text_mime(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        ));
        assert!(!is_appendable_text_mime("image/png"));
        assert!(!is_appendable_text_mime("application/octet-stream"));
        assert!(!is_appendable_text_mime("application/json"));
        assert!(!is_appendable_text_mime("application/xml"));
        assert!(!is_appendable_text_mime("application/yaml"));
        assert!(!is_appendable_text_mime(""));
    }
}
