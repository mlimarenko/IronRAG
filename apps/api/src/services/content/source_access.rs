use uuid::Uuid;

use crate::domains::content::{ContentSourceAccess, ContentSourceAccessKind};

const DOCUMENT_SOURCE_ROUTE_PREFIX: &str = "/v1/content/documents";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentSourceDescriptor {
    pub access: Option<ContentSourceAccess>,
    pub file_name: String,
}

#[must_use]
pub fn describe_content_source(
    document_id: Uuid,
    revision_id: Option<Uuid>,
    content_source_kind: &str,
    source_uri: Option<&str>,
    storage_key: Option<&str>,
    title: Option<&str>,
    external_key: &str,
) -> ContentSourceDescriptor {
    let file_name =
        derive_revision_source_file_name(content_source_kind, source_uri, title, external_key);
    let access = if content_source_kind.trim() == "web_page" {
        normalize_external_source_uri(source_uri)
            .map(|href| ContentSourceAccess { kind: ContentSourceAccessKind::ExternalUrl, href })
    } else if has_stored_document_source(content_source_kind, storage_key) {
        Some(ContentSourceAccess {
            kind: ContentSourceAccessKind::StoredDocument,
            href: document_source_download_path(document_id, revision_id),
        })
    } else {
        None
    };

    ContentSourceDescriptor { access, file_name }
}

#[must_use]
pub fn derive_content_source_file_name(
    source_uri: Option<&str>,
    title: Option<&str>,
    fallback: &str,
) -> String {
    preferred_content_source_file_name(source_uri, title)
        .unwrap_or_else(|| sanitize_download_file_name(fallback, "download"))
}

#[must_use]
pub fn derive_storage_backed_content_file_name(
    content_source_kind: &str,
    source_uri: Option<&str>,
    title: Option<&str>,
) -> Option<String> {
    match content_source_kind.trim() {
        "upload" | "replace" | "append" => preferred_content_source_file_name(source_uri, title),
        "edit" => preferred_edited_markdown_file_name(source_uri, title),
        _ => None,
    }
}

fn sanitize_download_file_name(value: &str, fallback: &str) -> String {
    let sanitized = value
        .split(['/', '\\'])
        .next_back()
        .unwrap_or(value)
        .chars()
        .map(|character| if character.is_ascii_control() { '_' } else { character })
        .collect::<String>()
        .replace('"', "")
        .trim()
        .trim_matches('.')
        .to_string();
    if sanitized.is_empty() {
        fallback
            .split(['/', '\\'])
            .next_back()
            .unwrap_or(fallback)
            .trim()
            .trim_matches('.')
            .to_string()
    } else {
        sanitized
    }
}

fn preferred_content_source_file_name(
    source_uri: Option<&str>,
    title: Option<&str>,
) -> Option<String> {
    file_name_from_source_uri(source_uri)
        .or_else(|| normalized_non_empty(title))
        .and_then(|value| sanitized_candidate_file_name(&value))
}

fn derive_revision_source_file_name(
    content_source_kind: &str,
    source_uri: Option<&str>,
    title: Option<&str>,
    fallback: &str,
) -> String {
    derive_storage_backed_content_file_name(content_source_kind, source_uri, title)
        .unwrap_or_else(|| derive_content_source_file_name(source_uri, title, fallback))
}

fn preferred_edited_markdown_file_name(
    source_uri: Option<&str>,
    title: Option<&str>,
) -> Option<String> {
    let base_name = file_name_from_source_uri(source_uri)
        .or_else(|| normalized_non_empty(title))
        .and_then(|value| sanitized_candidate_file_name(&value))?;
    Some(ensure_markdown_extension(&base_name))
}

fn ensure_markdown_extension(file_name: &str) -> String {
    let stem = file_name.rsplit_once('.').map_or(file_name, |(stem, _)| stem.trim());
    let normalized_stem = if stem.is_empty() { "document" } else { stem };
    format!("{normalized_stem}.md")
}

fn sanitized_candidate_file_name(value: &str) -> Option<String> {
    let sanitized = sanitize_download_file_name(value, "");
    (!sanitized.is_empty()).then_some(sanitized)
}

fn file_name_from_source_uri(source_uri: Option<&str>) -> Option<String> {
    source_uri
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|value| {
            reqwest::Url::parse(value)
                .ok()
                .and_then(|url| {
                    url.path_segments()
                        .and_then(|segments| segments.last())
                        .map(str::trim)
                        .filter(|segment| !segment.is_empty())
                        .map(std::string::ToString::to_string)
                })
                .or_else(|| value.split_once("://").map(|(_, rest)| rest.to_string()))
                .or_else(|| Some(value.to_string()))
        })
        .and_then(|value| {
            value
                .rsplit('/')
                .next()
                .map(str::trim)
                .filter(|item| !item.is_empty() && *item != "inline")
                .map(std::string::ToString::to_string)
        })
}

fn normalized_non_empty(value: Option<&str>) -> Option<String> {
    value.map(str::trim).filter(|item| !item.is_empty()).map(std::string::ToString::to_string)
}

fn has_stored_document_source(content_source_kind: &str, storage_key: Option<&str>) -> bool {
    storage_key.map(str::trim).is_some_and(|value| !value.is_empty())
        || matches!(content_source_kind.trim(), "upload" | "replace" | "edit" | "append")
}

fn normalize_external_source_uri(source_uri: Option<&str>) -> Option<String> {
    let value = source_uri.map(str::trim).filter(|item| !item.is_empty())?;
    let parsed = reqwest::Url::parse(value).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

#[must_use]
pub fn document_source_download_path(document_id: Uuid, revision_id: Option<Uuid>) -> String {
    match revision_id {
        Some(revision_id) => {
            format!("{DOCUMENT_SOURCE_ROUTE_PREFIX}/{document_id}/source?revisionId={revision_id}")
        }
        None => format!("{DOCUMENT_SOURCE_ROUTE_PREFIX}/{document_id}/source"),
    }
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use crate::domains::content::ContentSourceAccessKind;

    use super::{derive_storage_backed_content_file_name, describe_content_source};

    #[test]
    fn upload_documents_use_stable_application_download_path() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let descriptor = describe_content_source(
            document_id,
            Some(revision_id),
            "upload",
            Some("upload://runtime-upload-check.pdf"),
            None,
            Some("Ignored title"),
            "fallback.bin",
        );

        assert_eq!(descriptor.file_name, "runtime-upload-check.pdf");
        let access = descriptor.access.expect("stored uploads should expose source access");
        assert_eq!(access.kind, ContentSourceAccessKind::StoredDocument);
        assert_eq!(
            access.href,
            format!("/v1/content/documents/{document_id}/source?revisionId={revision_id}")
        );
    }

    #[test]
    fn web_pages_use_external_source_url() {
        let document_id = Uuid::now_v7();
        let descriptor = describe_content_source(
            document_id,
            Some(Uuid::now_v7()),
            "web_page",
            Some("https://example.com/guide"),
            Some("content/demo"),
            Some("Guide"),
            "fallback.bin",
        );

        assert_eq!(descriptor.file_name, "guide");
        let access = descriptor.access.expect("web pages should expose their external URL");
        assert_eq!(access.kind, ContentSourceAccessKind::ExternalUrl);
        assert_eq!(access.href, "https://example.com/guide");
    }

    #[test]
    fn append_revisions_expose_generated_text_source_link() {
        let document_id = Uuid::now_v7();
        let revision_id = Uuid::now_v7();
        let descriptor = describe_content_source(
            document_id,
            Some(revision_id),
            "append",
            Some("append://inline"),
            None,
            Some("Inline notes"),
            "fallback.bin",
        );

        assert_eq!(descriptor.file_name, "Inline notes");
        let access = descriptor.access.expect("append revisions should expose generated source");
        assert_eq!(access.kind, ContentSourceAccessKind::StoredDocument);
        assert_eq!(
            access.href,
            format!("/v1/content/documents/{document_id}/source?revisionId={revision_id}")
        );
    }

    #[test]
    fn descriptor_sanitizes_download_file_name() {
        let descriptor = describe_content_source(
            Uuid::now_v7(),
            Some(Uuid::now_v7()),
            "upload",
            Some("upload://../../quarterly-report.pdf"),
            Some("content/demo"),
            None,
            "fallback.bin",
        );

        assert_eq!(descriptor.file_name, "quarterly-report.pdf");
    }

    #[test]
    fn storage_backed_file_name_uses_same_canonical_derivation() {
        let file_name = derive_storage_backed_content_file_name(
            "replace",
            Some("upload://../../quarterly-report.pdf"),
            Some("Ignored"),
        );

        assert_eq!(file_name.as_deref(), Some("quarterly-report.pdf"));
    }

    #[test]
    fn edited_documents_keep_download_access() {
        let descriptor = describe_content_source(
            Uuid::now_v7(),
            Some(Uuid::now_v7()),
            "edit",
            Some("edit://Quarterly report.md"),
            Some("content/demo"),
            Some("Quarterly report"),
            "fallback.bin",
        );

        assert_eq!(descriptor.file_name, "Quarterly report.md");
        assert_eq!(
            descriptor.access.expect("edited documents should expose stored download access").kind,
            ContentSourceAccessKind::StoredDocument
        );
    }

    #[test]
    fn edited_documents_rebuild_markdown_storage_file_name_from_original_title() {
        let file_name = derive_storage_backed_content_file_name(
            "edit",
            Some("edit://inline"),
            Some("Inventory.xlsx"),
        );

        assert_eq!(file_name.as_deref(), Some("Inventory.md"));
    }

    #[test]
    fn appended_documents_rebuild_storage_file_name_from_title() {
        let file_name = derive_storage_backed_content_file_name(
            "append",
            Some("append://inline"),
            Some("conversation-log.jsonl"),
        );

        assert_eq!(file_name.as_deref(), Some("conversation-log.jsonl"));
    }
}
