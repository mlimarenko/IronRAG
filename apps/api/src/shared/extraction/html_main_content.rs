mod decoding;
mod media;
mod metadata;
mod rendering;

use anyhow::Result;
use reqwest::Url;
use scraper::Html;

use self::decoding::{decode_html, safe_html_truncation_boundary};
use self::media::collect_outbound_resources;
use self::metadata::{collect_outbound_links, extract_title};
use self::rendering::{parse_selector, render_markdownish_text, select_content_root};
use crate::shared::extraction::{
    ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content,
};
use crate::shared::web::url_identity::normalize_absolute_url;

const HTML_LINK_LIMIT: usize = 512;
const HTML_RESOURCE_LIMIT: usize = 128;
const HTML_CHARSET_SCAN_BYTES: usize = 4_096;
const MIN_CRAWLABLE_IMAGE_DIMENSION: u32 = 48;
/// Hard cap on the size of HTML we hand to `scraper::Html::parse_document`.
/// The DOM arena built by `html5ever` is dense: a 20 MB HTML source with
/// heavy inline markup or base64-encoded images can allocate 1.5–2 GB of
/// nested `Node` structures and OOM-kill the worker. Above this cap we
/// truncate the decoded source at a tag boundary, parse whatever fits, and
/// warn. The pipeline still produces a usable extraction for any input size;
/// only the tail of very large pages is dropped from graph extraction.
const HTML_PARSE_SOFT_CAP_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
struct DecodedHtml {
    content: String,
    charset: String,
    had_errors: bool,
}

pub fn extract_html_main_content(
    file_bytes: &[u8],
    mime_type: Option<&str>,
) -> Result<ExtractionOutput> {
    let decoded = decode_html(file_bytes, mime_type);
    let original_len = decoded.content.len();
    let (parse_source, truncated) = if original_len > HTML_PARSE_SOFT_CAP_BYTES {
        let boundary = safe_html_truncation_boundary(&decoded.content, HTML_PARSE_SOFT_CAP_BYTES);
        tracing::warn!(
            stage = "html_extract",
            original_bytes = original_len,
            kept_bytes = boundary,
            cap_bytes = HTML_PARSE_SOFT_CAP_BYTES,
            "HTML source exceeds parse soft cap; truncating before DOM build to protect worker memory"
        );
        (&decoded.content[..boundary], true)
    } else {
        (decoded.content.as_str(), false)
    };
    let document = Html::parse_document(parse_source);
    let title = extract_title(&document);
    let content_root = select_content_root(&document);
    let root = content_root.unwrap_or_else(|| document.root_element());
    let rendered_text = render_markdownish_text(root, title.as_deref());
    let layout = build_text_layout_from_content(&rendered_text);
    let outbound_links = collect_outbound_links(root);
    let outbound_resources = collect_outbound_resources(root);
    let mut warnings = Vec::new();
    if rendered_text.trim().is_empty() {
        warnings.push("html page did not yield readable main content".to_string());
    }
    if decoded.had_errors {
        warnings.push(format!(
            "html payload required replacement characters while decoding with {}",
            decoded.charset
        ));
    }
    if outbound_links.len() == HTML_LINK_LIMIT {
        warnings.push("outbound link collection reached the configured limit".to_string());
    }
    if outbound_resources.len() == HTML_RESOURCE_LIMIT {
        warnings.push("outbound resource collection reached the configured limit".to_string());
    }
    if truncated {
        warnings.push(format!(
            "html source was truncated from {} to {} bytes before DOM parsing to stay within worker memory budget",
            original_len,
            parse_source.len(),
        ));
    }

    Ok(ExtractionOutput {
        extraction_kind: "html_main_content".to_string(),
        content_text: layout.content_text,
        page_count: Some(1),
        warnings,
        source_metadata: ExtractionSourceMetadata {
            source_format: "html_main_content".to_string(),
            page_count: Some(1),
            line_count: i32::try_from(layout.structure_hints.lines.len()).unwrap_or(i32::MAX),
        },
        structure_hints: layout.structure_hints,
        source_map: serde_json::json!({
            "title": title,
            "outboundLinks": outbound_links,
            "outboundResources": outbound_resources,
            "contentRootTag": root.value().name(),
            "charset": decoded.charset,
        }),
        provider_kind: None,
        model_name: None,
        usage_json: serde_json::json!({}),
        extracted_images: Vec::new(),
    })
}

#[must_use]
pub fn extract_html_preferred_url(
    file_bytes: &[u8],
    mime_type: Option<&str>,
    base_url: &str,
) -> Option<String> {
    let decoded = decode_html(file_bytes, mime_type);
    let document = Html::parse_document(&decoded.content);
    let selector = parse_selector("link[href]")?;
    let base = Url::parse(base_url).ok()?;

    document.select(&selector).find_map(|element| {
        let rel = element.value().attr("rel")?;
        const PREFERRED_URL_REL: &str = "canonical";
        if !rel.split_ascii_whitespace().any(|part| part.eq_ignore_ascii_case(PREFERRED_URL_REL)) {
            return None;
        }
        let href = element.value().attr("href")?.trim();
        if href.is_empty() {
            return None;
        }
        let resolved = base.join(href).ok()?;
        normalize_absolute_url(resolved.as_str()).ok()
    })
}

#[must_use]
pub fn payload_looks_like_html_document(text: &str) -> bool {
    let prefix = text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    prefix.starts_with("<!doctype html")
        || prefix.starts_with("<html")
        || prefix.starts_with("<head")
        || prefix.starts_with("<body")
        || prefix.starts_with("<main")
        || prefix.starts_with("<article")
        || prefix.contains("<html")
        || prefix.contains("<body")
}

#[cfg(test)]
mod tests;
