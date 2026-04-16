use std::collections::BTreeSet;

use anyhow::Result;
use encoding_rs::{Encoding, UTF_8};
use reqwest::Url;
use scraper::{ElementRef, Html, Selector};

use crate::shared::extraction::{
    ExtractionOutput, ExtractionSourceMetadata, build_text_layout_from_content,
};
use crate::shared::web::url_identity::normalize_absolute_url;

const HTML_LINK_LIMIT: usize = 512;
const HTML_CHARSET_SCAN_BYTES: usize = 4_096;
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
    let outbound_links = collect_outbound_links(&document);
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
        warnings.push("outbound link collection reached the canonical limit".to_string());
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
pub fn extract_html_canonical_url(
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
        if !rel.split_ascii_whitespace().any(|part| part.eq_ignore_ascii_case("canonical")) {
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

/// Finds a safe byte index at or just before `target_bytes` that lands on a
/// UTF-8 character boundary and, where possible, at a `<` tag boundary so the
/// truncated fragment is still well-structured for the HTML parser to recover.
fn safe_html_truncation_boundary(source: &str, target_bytes: usize) -> usize {
    if source.len() <= target_bytes {
        return source.len();
    }
    let mut boundary = target_bytes;
    while boundary > 0 && !source.is_char_boundary(boundary) {
        boundary -= 1;
    }
    // Walk back up to 4 KiB looking for a `<` so we cut between elements
    // rather than mid-attribute. Keeps the truncated fragment parse-friendly.
    let window_start = boundary.saturating_sub(4096);
    if let Some(offset) = source[window_start..boundary].rfind('<').map(|rel| window_start + rel) {
        boundary = offset;
        while boundary > 0 && !source.is_char_boundary(boundary) {
            boundary -= 1;
        }
    }
    boundary
}

fn decode_html(file_bytes: &[u8], mime_type: Option<&str>) -> DecodedHtml {
    let encoding = charset_from_mime_type(mime_type)
        .or_else(|| sniff_charset_from_html(file_bytes))
        .and_then(|label| Encoding::for_label(label.as_bytes()).map(|encoding| (label, encoding)))
        .unwrap_or_else(|| ("utf-8".to_string(), UTF_8));
    let (decoded, _, had_errors) = encoding.1.decode(file_bytes);
    DecodedHtml { content: decoded.into_owned(), charset: encoding.0, had_errors }
}

fn charset_from_mime_type(mime_type: Option<&str>) -> Option<String> {
    mime_type.and_then(|value| {
        value.split(';').skip(1).find_map(|segment| {
            let (name, raw_value) = segment.split_once('=')?;
            if name.trim().eq_ignore_ascii_case("charset") {
                Some(raw_value.trim().trim_matches('"').to_ascii_lowercase())
            } else {
                None
            }
        })
    })
}

fn sniff_charset_from_html(file_bytes: &[u8]) -> Option<String> {
    let prefix_len = file_bytes.len().min(HTML_CHARSET_SCAN_BYTES);
    let prefix = String::from_utf8_lossy(&file_bytes[..prefix_len]).to_ascii_lowercase();
    find_html_charset_assignment(&prefix)
}

fn find_html_charset_assignment(prefix: &str) -> Option<String> {
    let charset_index = prefix.find("charset=")?;
    let remainder = &prefix[charset_index + "charset=".len()..];
    let trimmed = remainder
        .trim_start_matches(|ch: char| ch.is_ascii_whitespace() || ch == '"' || ch == '\'');
    let value = trimmed
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        .collect::<String>();
    (!value.is_empty()).then_some(value)
}

fn select_content_root(document: &Html) -> Option<ElementRef<'_>> {
    const HIGH_CONFIDENCE_SELECTORS: [&str; 3] =
        ["#main-content.wiki-content", "#main-content", ".wiki-content"];
    const CANDIDATE_SELECTORS: [&str; 11] = [
        "#main-content.wiki-content",
        "#main-content",
        ".wiki-content",
        "main",
        "article",
        "[role='main']",
        "#content",
        "#main",
        ".content",
        ".main-content",
        ".article-content",
    ];

    for query in HIGH_CONFIDENCE_SELECTORS {
        let Some(selector) = parse_selector(query) else {
            continue;
        };
        if let Some(element) = document
            .select(&selector)
            .find(|element| content_root_score(*element, 0, CANDIDATE_SELECTORS.len()) > 0)
        {
            return Some(element);
        }
    }

    let mut best: Option<(usize, ElementRef<'_>)> = None;
    for (priority, query) in CANDIDATE_SELECTORS.iter().enumerate() {
        let Some(selector) = parse_selector(query) else {
            continue;
        };
        for element in document.select(&selector) {
            let score = content_root_score(element, priority, CANDIDATE_SELECTORS.len());
            if score == 0 {
                continue;
            }
            match best {
                Some((best_score, _)) if best_score >= score => {}
                _ => {
                    best = Some((score, element));
                }
            }
        }
    }

    if let Some((_, element)) = best {
        return Some(element);
    }

    parse_selector("body").and_then(|selector| document.select(&selector).next())
}

fn parse_selector(query: &str) -> Option<Selector> {
    Selector::parse(query).ok()
}

fn render_markdownish_text(root: ElementRef<'_>, title: Option<&str>) -> String {
    let mut blocks = Vec::<String>::new();
    if let Some(title) = title {
        push_block(&mut blocks, format!("# {}", normalize_whitespace(title)));
    }
    render_element(root, &mut blocks);
    blocks.join("\n\n")
}

fn render_element(element: ElementRef<'_>, blocks: &mut Vec<String>) {
    let tag_name = element.value().name();
    if is_ignored_tag(tag_name) {
        return;
    }

    match tag_name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
            let level = heading_level(tag_name);
            let text = normalized_text_from_element(element);
            if !text.is_empty() {
                push_block(blocks, format!("{} {}", "#".repeat(level), text));
            }
        }
        "a" => push_link_block(blocks, element),
        "p" => push_text_block(blocks, normalized_text_from_element(element)),
        "pre" => push_preformatted_block(blocks, element),
        "blockquote" => push_blockquote(blocks, element),
        "ul" => push_list_block(blocks, element, false),
        "ol" => push_list_block(blocks, element, true),
        "table" => push_table_block(blocks, element),
        _ => {
            let child_elements = direct_child_elements(element);
            if child_elements.is_empty() {
                push_text_block(blocks, normalized_text_from_element(element));
            } else if child_elements
                .iter()
                .any(|child| is_block_container_or_leaf(child.value().name()))
            {
                for child in child_elements {
                    render_element(child, blocks);
                }
            } else {
                push_text_block(blocks, normalized_text_from_element(element));
            }
        }
    }
}

fn direct_child_elements(element: ElementRef<'_>) -> Vec<ElementRef<'_>> {
    element.children().filter_map(ElementRef::wrap).collect()
}

fn is_ignored_tag(tag_name: &str) -> bool {
    matches!(
        tag_name,
        "script"
            | "style"
            | "noscript"
            | "template"
            | "nav"
            | "footer"
            | "header"
            | "aside"
            | "form"
            | "iframe"
            | "svg"
    )
}

fn is_block_container_or_leaf(tag_name: &str) -> bool {
    matches!(
        tag_name,
        "article"
            | "a"
            | "body"
            | "div"
            | "figure"
            | "figcaption"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "li"
            | "main"
            | "ol"
            | "p"
            | "pre"
            | "section"
            | "summary"
            | "table"
            | "tbody"
            | "thead"
            | "tfoot"
            | "tr"
            | "ul"
            | "blockquote"
    )
}

fn heading_level(tag_name: &str) -> usize {
    tag_name
        .strip_prefix('h')
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .clamp(1, 6)
}

fn content_root_score(element: ElementRef<'_>, priority: usize, selector_count: usize) -> usize {
    let text_len = normalized_text_from_element(element).chars().count();
    let heading_count = count_descendants(element, "h1, h2, h3");
    let link_count = count_descendants(element, "a[href]");
    let image_count = count_descendants(element, "img");
    let layout_count = count_descendants(element, "[data-layout], .contentLayout2, .columnLayout");

    let has_meaningful_content = text_len >= 80
        || heading_count > 0
        || layout_count > 0
        || (link_count >= 3 && image_count >= 1);
    if !has_meaningful_content {
        return 0;
    }

    let priority_bonus = selector_count.saturating_sub(priority) * 240;
    priority_bonus
        + text_len
        + heading_count * 120
        + link_count * 18
        + image_count * 24
        + layout_count * 160
}

fn count_descendants(element: ElementRef<'_>, query: &str) -> usize {
    let Some(selector) = parse_selector(query) else {
        return 0;
    };
    element.select(&selector).count()
}

fn normalized_text_from_element(element: ElementRef<'_>) -> String {
    let joined = element.text().collect::<Vec<_>>().join(" ");
    normalize_whitespace(&joined)
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_text_block(blocks: &mut Vec<String>, value: String) {
    if !value.is_empty() {
        push_block(blocks, value);
    }
}

fn push_preformatted_block(blocks: &mut Vec<String>, element: ElementRef<'_>) {
    let text = element.text().collect::<Vec<_>>().join("");
    let normalized = text.trim();
    if !normalized.is_empty() {
        push_block(blocks, format!("```\n{normalized}\n```"));
    }
}

fn push_blockquote(blocks: &mut Vec<String>, element: ElementRef<'_>) {
    let text = normalized_text_from_element(element);
    if text.is_empty() {
        return;
    }
    let quoted =
        text.lines().map(|line| format!("> {}", line.trim())).collect::<Vec<_>>().join("\n");
    push_block(blocks, quoted);
}

fn push_link_block(blocks: &mut Vec<String>, element: ElementRef<'_>) {
    let Some(href) = element.value().attr("href").map(str::trim) else {
        return;
    };
    if href.is_empty() || href.starts_with('#') || is_ignored_href(href) {
        return;
    }

    let text = normalized_text_from_element(element);
    let label = if text.is_empty() { derive_link_label(element, href) } else { text };
    if label.is_empty() {
        return;
    }

    push_block(blocks, format!("- [{}]({href})", normalize_whitespace(&label)));
}

fn push_list_block(blocks: &mut Vec<String>, element: ElementRef<'_>, ordered: bool) {
    let items = direct_child_elements(element)
        .into_iter()
        .filter(|child| child.value().name() == "li")
        .enumerate()
        .filter_map(|(index, item)| {
            let text = normalized_text_from_element(item);
            if text.is_empty() {
                return None;
            }
            Some(if ordered { format!("{}. {}", index + 1, text) } else { format!("- {text}") })
        })
        .collect::<Vec<_>>();
    if !items.is_empty() {
        push_block(blocks, items.join("\n"));
    }
}

fn derive_link_label(element: ElementRef<'_>, href: &str) -> String {
    for attribute in ["aria-label", "title"] {
        if let Some(value) = element.value().attr(attribute) {
            let normalized = normalize_whitespace(value);
            if !normalized.is_empty() {
                return normalized;
            }
        }
    }

    let Some(image_selector) = parse_selector("img") else {
        return fallback_link_label_from_href(href);
    };
    for image in element.select(&image_selector) {
        if let Some(alt) = image.value().attr("alt") {
            let normalized = normalize_whitespace(alt);
            if !normalized.is_empty() {
                return normalized;
            }
        }
        if let Some(src) = image.value().attr("src") {
            let stem = media_label_from_path(src);
            if !stem.is_empty() {
                return stem;
            }
        }
    }

    fallback_link_label_from_href(href)
}

fn fallback_link_label_from_href(href: &str) -> String {
    if let Ok(url) = Url::parse(href)
        && let Some(segment) = url.path_segments().and_then(Iterator::last)
    {
        let normalized = media_label_from_path(segment);
        if !normalized.is_empty() {
            return normalized;
        }
    }
    media_label_from_path(href)
}

fn media_label_from_path(path: &str) -> String {
    let without_query = path.split('?').next().unwrap_or(path);
    let last_segment = without_query.rsplit('/').next().unwrap_or(without_query);
    let without_extension = last_segment.rsplit_once('.').map_or(last_segment, |(stem, _)| stem);
    let normalized = without_extension
        .replace(['-', '_'], " ")
        .split_whitespace()
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    normalize_whitespace(&normalized)
}

fn push_table_block(blocks: &mut Vec<String>, element: ElementRef<'_>) {
    let Some(row_selector) = parse_selector("tr") else {
        return;
    };
    let Some(cell_selector) = parse_selector("th, td") else {
        return;
    };
    let rows = element
        .select(&row_selector)
        .filter_map(|row| {
            let cells = row
                .select(&cell_selector)
                .map(normalized_text_from_element)
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>();
            (!cells.is_empty()).then_some(format!("| {} |", cells.join(" | ")))
        })
        .collect::<Vec<_>>();
    if !rows.is_empty() {
        push_block(blocks, rows.join("\n"));
    }
}

fn push_block(blocks: &mut Vec<String>, value: String) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if blocks.last().is_some_and(|existing| existing == trimmed) {
        return;
    }
    blocks.push(trimmed.to_string());
}

fn extract_title(document: &Html) -> Option<String> {
    let title_selector = parse_selector("title");
    if let Some(selector) = title_selector
        && let Some(element) = document.select(&selector).next()
    {
        let text = normalized_text_from_element(element);
        if !text.is_empty() {
            return Some(text);
        }
    }

    const META_TITLE_SELECTORS: [&str; 3] =
        [r#"meta[property="og:title"]"#, r#"meta[name="twitter:title"]"#, r#"meta[name="title"]"#];
    for query in META_TITLE_SELECTORS {
        let Some(selector) = parse_selector(query) else {
            continue;
        };
        if let Some(content) =
            document.select(&selector).find_map(|element| element.value().attr("content"))
        {
            let normalized = normalize_whitespace(content);
            if !normalized.is_empty() {
                return Some(normalized);
            }
        }
    }
    None
}

fn collect_outbound_links(document: &Html) -> Vec<String> {
    let Some(selector) = parse_selector("a[href]") else {
        return Vec::new();
    };
    let mut links = BTreeSet::<String>::new();
    for href in document.select(&selector).filter_map(|element| element.value().attr("href")) {
        let trimmed = href.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || is_ignored_href(trimmed) {
            continue;
        }
        links.insert(trimmed.to_string());
        if links.len() >= HTML_LINK_LIMIT {
            break;
        }
    }
    links.into_iter().collect()
}

fn is_ignored_href(href: &str) -> bool {
    if let Ok(url) = Url::parse(href) {
        return !matches!(url.scheme(), "http" | "https");
    }
    matches!(href.split_once(':'), Some((scheme, _)) if !matches!(scheme, "http" | "https"))
}

#[cfg(test)]
mod tests {
    use super::{
        extract_html_canonical_url, extract_html_main_content, payload_looks_like_html_document,
    };

    #[test]
    fn detects_html_payload_by_prefix() {
        assert!(payload_looks_like_html_document("<!DOCTYPE html><html><body>Hello</body></html>"));
        assert!(!payload_looks_like_html_document("plain text payload"));
    }

    #[test]
    fn extracts_main_content_and_links_from_html() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head>
                <title>Docs Home</title>
              </head>
              <body>
                <nav>Top navigation</nav>
                <main>
                  <h1>IronRAG Docs</h1>
                  <p>Ship one canonical ingestion path.</p>
                  <ul><li>Single page</li><li>Recursive crawl</li></ul>
                  <a href="/guide">Guide</a>
                  <a href="https://example.org/outside">Outside</a>
                </main>
                <footer>Footer</footer>
              </body>
            </html>
        "#;

        let output = extract_html_main_content(html.as_bytes(), Some("text/html; charset=utf-8"))
            .expect("html extraction");

        assert_eq!(output.extraction_kind, "html_main_content");
        assert_eq!(
            output.source_map.get("title").and_then(serde_json::Value::as_str),
            Some("Docs Home")
        );
        assert!(output.content_text.contains("# IronRAG Docs"));
        assert!(output.content_text.contains("Ship one canonical ingestion path."));
        assert!(!output.content_text.contains("Top navigation"));
        assert!(!output.content_text.contains("Footer"));
        assert_eq!(
            output
                .source_map
                .get("outboundLinks")
                .and_then(serde_json::Value::as_array)
                .map(std::vec::Vec::len),
            Some(2)
        );
    }

    #[test]
    fn prefers_confluence_main_content_over_outer_container() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head>
                <title>Программные продукты Acme</title>
              </head>
              <body>
                <div id="content">
                  <div class="page-metadata">Created by Alice</div>
                  <div id="main-content" class="wiki-content">
                    <div class="contentLayout2">
                      <div class="columnLayout">
                        <div class="cell">
                          <a href="/pages/viewpage.action?pageId=1">
                            <img src="/download/attachments/1/POS.png" />
                          </a>
                        </div>
                        <div class="cell">
                          <a href="/x/2">
                            <img src="/download/attachments/1/hybrid_pos.png" />
                          </a>
                        </div>
                      </div>
                    </div>
                  </div>
                  <div id="labels-section">No labels</div>
                </div>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("confluence html");

        assert!(output.content_text.contains("# Программные продукты Acme"));
        assert!(output.content_text.contains("POS"));
        assert!(output.content_text.contains("Hybrid Pos"));
        assert!(!output.content_text.contains("Created by Alice"));
        assert!(!output.content_text.contains("No labels"));
    }

    #[test]
    fn extracts_html_canonical_url_against_base_url() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head>
                <link rel="canonical" href="/pages/viewpage.action?pageId=44597523" />
              </head>
              <body>
                <main>Docs home</main>
              </body>
            </html>
        "#;

        let canonical = extract_html_canonical_url(
            html.as_bytes(),
            Some("text/html"),
            "https://docs.example.test/",
        );

        assert_eq!(
            canonical.as_deref(),
            Some("https://docs.example.test/pages/viewpage.action?pageId=44597523")
        );
    }

    #[test]
    fn prefers_dense_confluence_main_content_even_when_outer_container_has_more_chrome() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head>
                <title>Acme Control Center</title>
              </head>
              <body>
                <div id="content">
                  <div class="page-metadata">Created by Alice</div>
                  <div id="page-metadata-banner">3 attachments</div>
                  <div id="breadcrumbs">Docs / Products / Control Center</div>
                  <div id="main-content" class="wiki-content">
                    <h1>Acme Control Center</h1>
                    <p>Control Center is used to manage distributed retail operations.</p>
                    <p>It centralizes settings, notifications, and remote administration flows.</p>
                  </div>
                </div>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("confluence html");

        assert!(output.content_text.contains("Control Center is used to manage"));
        assert!(!output.content_text.contains("Created by Alice"));
        assert!(!output.content_text.contains("3 attachments"));
        assert!(!output.content_text.contains("Docs / Products / Control Center"));
    }
}
