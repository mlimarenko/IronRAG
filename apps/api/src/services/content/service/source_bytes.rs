use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub(super) struct AppendableDocumentSource {
    pub(super) raw_bytes: Vec<u8>,
    pub(super) replacement: ironrag_contracts::documents::ReplacementIdentity,
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

/// Canonical MIME type for a lowercased file extension, or `None` when the
/// extension is not one the inline surfaces recognise. Pure: the fallback
/// kind decision below stays separate from the extension table.
fn canonical_mime_for_extension(extension: &str) -> Option<&'static str> {
    Some(match extension {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsb" => "application/vnd.ms-excel.sheet.binary.macroenabled.12",
        "ods" => "application/vnd.oasis.opendocument.spreadsheet",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "md" => "text/markdown",
        "txt" => "text/plain",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "tif" | "tiff" => "image/tiff",
        _ => return None,
    })
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

    if let Some(mime_type) = file_name
        .and_then(file_extension)
        .and_then(|extension| canonical_mime_for_extension(&extension))
    {
        return mime_type.to_string();
    }

    match fallback_kind {
        "append" => "text/plain".to_string(),
        "edit" => "text/markdown".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

fn file_extension(file_name: &str) -> Option<String> {
    let (_, extension) = file_name.rsplit_once('.')?;
    Some(extension.trim().to_ascii_lowercase())
}

pub(super) fn sha256_hex_text(value: &str) -> String {
    sha256_hex_bytes(value.as_bytes())
}

pub(super) fn sha256_hex_bytes(value: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value);
    hex::encode(hasher.finalize())
}

/// Byte-level merge for append.
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
    use super::{
        canonical_mime_for_extension, infer_inline_mime_type, is_appendable_text_mime,
        merge_appended_bytes, source_uri_for_inline_payload,
    };
    use crate::shared::extraction::record_jsonl::extract_record_jsonl;

    #[test]
    fn canonical_mime_table_covers_aliases_and_unknowns() {
        assert_eq!(canonical_mime_for_extension("jpeg"), Some("image/jpeg"));
        assert_eq!(canonical_mime_for_extension("jpg"), Some("image/jpeg"));
        assert_eq!(canonical_mime_for_extension("tiff"), Some("image/tiff"));
        assert_eq!(canonical_mime_for_extension("bin"), None);
        assert_eq!(canonical_mime_for_extension(""), None);
    }

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
    fn replacement_filename_is_explicit_not_derived_from_title() {
        let replacement = ironrag_contracts::documents::ReplacementIdentity {
            checksum: format!("sha256:{}", "a".repeat(64)),
            byte_size: 4,
            mime_type: "text/markdown".to_owned(),
            file_name: "replacement.md".to_owned(),
            title: Some("Inventory.xlsx".to_owned()),
            language_code: None,
            source_uri: None,
            document_hint: None,
        }
        .normalize()
        .expect("valid explicit replacement");
        assert_eq!(replacement.file_name, "replacement.md");
        assert_eq!(replacement.title.as_deref(), Some("Inventory.xlsx"));
    }

    #[test]
    fn edit_inline_sources_use_normalized_inline_uri() {
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
