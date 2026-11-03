mod decoding;
mod media;
mod metadata;
mod rendering;

use anyhow::Result;
use reqwest::Url;
use scraper::Html;

use self::decoding::decode_html;
use self::media::collect_outbound_resources;
use self::metadata::{collect_outbound_links, extract_title};
use self::rendering::{media_blocks, parse_selector, select_content_root, selected_html};
use crate::shared::extraction::{
    ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content,
};
use crate::shared::web::url_identity::normalize_absolute_url;

const HTML_LINK_LIMIT: usize = 512;
const HTML_RESOURCE_LIMIT: usize = 128;
const HTML_CHARSET_SCAN_BYTES: usize = 4_096;
const MIN_CRAWLABLE_IMAGE_DIMENSION: u32 = 48;
// The existing DOM allocation budget is a hard refusal, never success with a
// truncated tail: dense HTML can expand to hundreds of times its source size.
const HTML_PARSE_LIMIT_BYTES: usize = 4 * 1024 * 1024;

pub struct PreparedHtml {
    pub source: String,
    pub source_map: serde_json::Value,
    pub warnings: Vec<String>,
    title: Option<String>,
    media: Vec<String>,
}

#[derive(Debug, Clone)]
struct DecodedHtml {
    content: String,
    charset: String,
    had_errors: bool,
}

/// Select the product's main root and media without converting document text.
pub fn prepare_html_main_content(
    file_bytes: &[u8],
    mime_type: Option<&str>,
) -> Result<PreparedHtml> {
    anyhow::ensure!(file_bytes.len() <= HTML_PARSE_LIMIT_BYTES, "html_parse_limit");
    let decoded = decode_html(file_bytes, mime_type);
    anyhow::ensure!(decoded.content.len() <= HTML_PARSE_LIMIT_BYTES, "html_parse_limit");
    let document = Html::parse_document(&decoded.content);
    let title = extract_title(&document);
    let root = select_content_root(&document).unwrap_or_else(|| document.root_element());
    let outbound_links = collect_outbound_links(root);
    let outbound_resources = collect_outbound_resources(root);
    let mut warnings = Vec::new();
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
    Ok(PreparedHtml {
        source: selected_html(root),
        source_map: serde_json::json!({
            "title": title, "outboundLinks": outbound_links, "outboundResources": outbound_resources,
            "contentRootTag": root.value().name(), "charset": decoded.charset,
        }),
        warnings,
        title,
        media: media_blocks(root),
    })
}

/// Project child-produced native text into crawl metadata and media semantics.
pub fn extract_html_main_content(
    file_bytes: &[u8],
    mime_type: Option<&str>,
    native_content: &str,
) -> Result<ExtractionOutput> {
    let prepared = prepare_html_main_content(file_bytes, mime_type)?;
    let mut blocks = Vec::new();
    if let Some(title) =
        prepared.title.filter(|_| !native_content.trim().is_empty() || !prepared.media.is_empty())
    {
        blocks.push(format!("# {title}"));
    }
    blocks.push(native_content.to_owned());
    blocks.extend(prepared.media);
    let layout = build_text_layout_from_content(&blocks.join("\n\n"));
    let mut warnings = prepared.warnings;
    if layout.content_text.trim().is_empty() {
        warnings.push("html page did not yield readable main content".into());
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
        source_map: prepared.source_map,
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
