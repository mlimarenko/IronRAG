#[must_use]
pub fn resolve_document_hint(
    revision_kind: &str,
    revision_source_uri: Option<&str>,
    revision_document_hint: Option<&str>,
    document_title: Option<&str>,
    library_setting: bool,
) -> Option<String> {
    if !library_setting {
        return None;
    }
    if let Some(h) = revision_document_hint {
        return Some(h.to_string());
    }
    if revision_kind == "web_page"
        && let Some(uri) =
            revision_source_uri.filter(|u| u.starts_with("http://") || u.starts_with("https://"))
    {
        return Some(uri.to_string());
    }
    document_title.map(std::string::ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::resolve_document_hint;

    #[test]
    fn explicit_hint_wins() {
        assert_eq!(
            resolve_document_hint(
                "web_page",
                Some("https://example.com/source"),
                Some("Release notes"),
                Some("Fallback title"),
                true,
            ),
            Some("Release notes".to_string())
        );
    }

    #[test]
    fn web_page_http_source_falls_back() {
        assert_eq!(
            resolve_document_hint("web_page", Some("https://example.com/source"), None, None, true),
            Some("https://example.com/source".to_string())
        );
    }

    #[test]
    fn web_page_non_http_source_is_not_used() {
        assert_eq!(
            resolve_document_hint("web_page", Some("upload://blob"), None, Some("Upload"), true),
            Some("Upload".to_string())
        );
    }

    #[test]
    fn title_falls_back() {
        assert_eq!(
            resolve_document_hint("upload", Some("upload://blob"), None, Some("Upload"), true),
            Some("Upload".to_string())
        );
    }

    #[test]
    fn flag_off_returns_none() {
        assert_eq!(
            resolve_document_hint(
                "web_page",
                Some("https://example.com/source"),
                Some("Release notes"),
                Some("Fallback title"),
                false,
            ),
            None
        );
    }
}
