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
        warnings.push("outbound link collection reached the canonical limit".to_string());
    }
    if outbound_resources.len() == HTML_RESOURCE_LIMIT {
        warnings.push("outbound resource collection reached the canonical limit".to_string());
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
    const HIGH_CONFIDENCE_SELECTORS: [&str; 5] = [
        "#main-content.wiki-content",
        "#main-content",
        ".wiki-content",
        "#mw-content-text",
        ".mw-parser-output",
    ];
    const CANDIDATE_SELECTORS: [&str; 13] = [
        "#main-content.wiki-content",
        "#main-content",
        ".wiki-content",
        "#mw-content-text",
        ".mw-parser-output",
        "main",
        "article",
        "[role='main']",
        "#content",
        "#main",
        ".content",
        ".main-content",
        ".article-content",
    ];

    let mut high_confidence_best: Option<(usize, ElementRef<'_>)> = None;
    for (priority, query) in HIGH_CONFIDENCE_SELECTORS.iter().enumerate() {
        let Some(selector) = parse_selector(query) else {
            continue;
        };
        for element in document.select(&selector) {
            if !content_root_has_readable_content(element) {
                continue;
            }
            let score = content_root_score(element, priority, HIGH_CONFIDENCE_SELECTORS.len())
                .max(HIGH_CONFIDENCE_SELECTORS.len().saturating_sub(priority));
            match high_confidence_best {
                Some((best_score, _)) if best_score >= score => {}
                _ => high_confidence_best = Some((score, element)),
            }
        }
    }
    if let Some((_, element)) = high_confidence_best {
        return Some(element);
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
    let mut content_blocks = Vec::<String>::new();
    render_element(root, &mut content_blocks);
    if content_blocks.is_empty() {
        return String::new();
    }

    let mut blocks = Vec::<String>::new();
    if let Some(title) = title {
        push_block(&mut blocks, format!("# {}", normalize_whitespace(title)));
    }
    blocks.extend(content_blocks);
    blocks.join("\n\n")
}

fn render_element(element: ElementRef<'_>, blocks: &mut Vec<String>) {
    let tag_name = element.value().name();
    if is_ignored_tag(tag_name) || element_or_ancestor_hidden(element) {
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
        "a" => {
            push_link_block(blocks, element);
            push_nested_image_blocks(blocks, element, None);
        }
        "p" => push_text_with_nested_images(blocks, element),
        "pre" => push_preformatted_block(blocks, element),
        "blockquote" => push_blockquote(blocks, element),
        "figure" => push_figure_block(blocks, element),
        "img" => {
            let _ = push_image_block(blocks, element, None);
        }
        "picture" => {
            let _ = push_picture_block(blocks, element);
        }
        "ul" => push_list_block(blocks, element, false),
        "ol" => push_list_block(blocks, element, true),
        "table" => push_table_block(blocks, element),
        _ => {
            let child_elements = direct_child_elements(element);
            if child_elements.is_empty() {
                push_background_image_block(blocks, element);
                push_text_block(blocks, normalized_text_from_element(element));
            } else if child_elements
                .iter()
                .any(|child| is_block_container_or_leaf(child.value().name()))
            {
                push_background_image_block(blocks, element);
                for child in child_elements {
                    render_element(child, blocks);
                }
            } else if element_contains_image(element) {
                push_text_with_nested_images(blocks, element);
            } else {
                push_background_image_block(blocks, element);
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
            | "picture"
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

fn push_figure_block(blocks: &mut Vec<String>, element: ElementRef<'_>) {
    let caption = figure_caption_text(element);
    let Some(selector) = parse_selector("img") else {
        if let Some(caption) = caption {
            push_text_block(blocks, caption);
        }
        return;
    };

    let mut pushed = false;
    push_background_image_block(blocks, element);
    for image in element.select(&selector) {
        pushed |= push_image_block(blocks, image, caption.as_deref());
    }
    if !pushed && let Some(caption) = caption {
        push_text_block(blocks, caption);
    }
}

fn push_text_with_nested_images(blocks: &mut Vec<String>, element: ElementRef<'_>) {
    push_text_block(blocks, normalized_text_from_element(element));
    push_nested_image_blocks(blocks, element, None);
}

fn push_nested_image_blocks(
    blocks: &mut Vec<String>,
    element: ElementRef<'_>,
    figure_caption: Option<&str>,
) -> bool {
    let Some(selector) = parse_selector("img") else {
        return false;
    };
    let mut pushed = false;
    pushed |= push_background_image_block(blocks, element);
    for image in element.select(&selector) {
        pushed |= push_image_block(blocks, image, figure_caption);
    }
    pushed
}

fn element_contains_image(element: ElementRef<'_>) -> bool {
    parse_selector("img").is_some_and(|selector| element.select(&selector).next().is_some())
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

fn content_root_has_readable_content(element: ElementRef<'_>) -> bool {
    let text_len = normalized_text_from_element(element).chars().count();
    text_len >= 20
        || count_descendants(element, "h1, h2, h3") > 0
        || count_descendants(element, "a[href]") > 0
        || count_descendants(element, "img") > 0
}

fn count_descendants(element: ElementRef<'_>, query: &str) -> usize {
    let Some(selector) = parse_selector(query) else {
        return 0;
    };
    element.select(&selector).count()
}

fn normalized_text_from_element(element: ElementRef<'_>) -> String {
    let mut parts = Vec::new();
    collect_visible_text(element, &mut parts, false);
    let joined = parts.join(" ");
    normalize_whitespace(&joined)
}

fn collect_visible_text(
    element: ElementRef<'_>,
    parts: &mut Vec<String>,
    preserve_template_syntax: bool,
) {
    if is_ignored_tag(element.value().name()) || element_or_ancestor_hidden(element) {
        return;
    }
    let preserve_template_syntax =
        preserve_template_syntax || is_template_literal_container(element.value().name());
    for child in element.children() {
        if let Some(text) = child.value().as_text() {
            if let Some(value) = visible_text_node_value(text, preserve_template_syntax) {
                parts.push(value);
            }
            continue;
        }
        if let Some(child_element) = ElementRef::wrap(child) {
            collect_visible_text(child_element, parts, preserve_template_syntax);
        }
    }
}

fn visible_text_node_value(value: &str, preserve_template_syntax: bool) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || (!preserve_template_syntax && is_template_dominated_text(value)) {
        return None;
    }
    Some(value.to_string())
}

fn is_template_literal_container(tag_name: &str) -> bool {
    matches!(tag_name, "code" | "kbd" | "pre" | "samp")
}

fn is_template_dominated_text(value: &str) -> bool {
    let spans = double_brace_template_spans(value);
    if spans.is_empty() {
        return false;
    }

    let mut outside_non_ws = 0usize;
    let mut outside_alnum = 0usize;
    let mut template_non_ws = 0usize;
    let mut previous_end = 0usize;
    for (start, end) in spans.iter().copied() {
        count_text_signal(&value[previous_end..start], &mut outside_non_ws, &mut outside_alnum);
        template_non_ws += value[start..end].chars().filter(|ch| !ch.is_whitespace()).count();
        previous_end = end;
    }
    count_text_signal(&value[previous_end..], &mut outside_non_ws, &mut outside_alnum);

    (outside_alnum == 0 && outside_non_ws <= 4)
        || (spans.len() >= 2
            && outside_alnum < 12
            && template_non_ws > outside_non_ws.saturating_mul(3).max(12))
}

fn double_brace_template_spans(value: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut search_from = 0usize;
    while let Some(start_offset) = value[search_from..].find("{{") {
        let start = search_from + start_offset;
        let after_open = start + 2;
        let Some(end_offset) = value[after_open..].find("}}") else {
            break;
        };
        let end = after_open + end_offset + 2;
        spans.push((start, end));
        search_from = end;
    }
    spans
}

fn count_text_signal(value: &str, non_ws_count: &mut usize, alnum_count: &mut usize) {
    for ch in value.chars() {
        if !ch.is_whitespace() {
            *non_ws_count += 1;
        }
        if ch.is_alphanumeric() {
            *alnum_count += 1;
        }
    }
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

fn push_image_block(
    blocks: &mut Vec<String>,
    element: ElementRef<'_>,
    figure_caption: Option<&str>,
) -> bool {
    let Some(source) = preferred_image_source(element) else {
        return false;
    };
    if !is_ingestable_image_resource(element, &source) {
        return false;
    }
    let Some(label) = image_label(element, figure_caption) else {
        return false;
    };
    push_block(blocks, format!("![{}]({source})", markdown_image_label(&label)));
    true
}

fn push_picture_block(blocks: &mut Vec<String>, element: ElementRef<'_>) -> bool {
    let Some(source) = preferred_picture_source(element) else {
        return false;
    };
    let label = picture_label(element).unwrap_or_else(|| media_label_from_path(&source));
    if label.is_empty() {
        return false;
    }
    push_block(blocks, format!("![{}]({source})", markdown_image_label(&label)));
    true
}

fn push_background_image_block(blocks: &mut Vec<String>, element: ElementRef<'_>) -> bool {
    let Some(source) = preferred_background_image_source(element) else {
        return false;
    };
    let Some(label) = background_image_label(element) else {
        return false;
    };
    push_block(blocks, format!("![{}]({source})", markdown_image_label(&label)));
    true
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

fn collect_outbound_resources(root: ElementRef<'_>) -> Vec<String> {
    let mut resources = BTreeSet::<String>::new();
    collect_outbound_resources_from_element(root, &mut resources);
    resources.into_iter().collect()
}

fn collect_outbound_resources_from_element(
    element: ElementRef<'_>,
    resources: &mut BTreeSet<String>,
) {
    if resources.len() >= HTML_RESOURCE_LIMIT
        || is_ignored_tag(element.value().name())
        || element_or_ancestor_hidden(element)
    {
        return;
    }
    if element.value().name() == "img"
        && let Some(source) = preferred_image_source(element)
        && is_ingestable_image_resource(element, &source)
    {
        resources.insert(source);
        if resources.len() >= HTML_RESOURCE_LIMIT {
            return;
        }
    }
    if element.value().name() == "picture"
        && let Some(source) = preferred_picture_source(element)
    {
        resources.insert(source);
        return;
    }
    if let Some(source) = preferred_background_image_source(element)
        && background_image_label(element).is_some()
    {
        resources.insert(source);
        if resources.len() >= HTML_RESOURCE_LIMIT {
            return;
        }
    }
    for child in direct_child_elements(element) {
        collect_outbound_resources_from_element(child, resources);
        if resources.len() >= HTML_RESOURCE_LIMIT {
            break;
        }
    }
}

fn preferred_image_source(element: ElementRef<'_>) -> Option<String> {
    let lazy_sources = [
        best_srcset_candidate(element.value().attr("srcset")),
        best_srcset_candidate(element.value().attr("data-srcset")),
        image_url_attribute(element, "data-original"),
        image_url_attribute(element, "data-lazy-src"),
        image_url_attribute(element, "data-src"),
    ]
    .into_iter()
    .flatten()
    .find(|source| is_candidate_image_resource(source));
    let src =
        image_url_attribute(element, "src").filter(|source| is_candidate_image_resource(source));
    match (lazy_sources, src) {
        (Some(lazy), Some(src)) if is_placeholder_image_source(&src) => Some(lazy),
        (Some(lazy), _) => Some(lazy),
        (None, src) => src,
    }
}

fn best_srcset_candidate(srcset: Option<&str>) -> Option<String> {
    let srcset = srcset?;
    srcset
        .split(',')
        .filter_map(parse_srcset_candidate)
        .filter(|candidate| is_candidate_image_resource(&candidate.url))
        .max_by_key(|candidate| candidate.score)
        .map(|candidate| candidate.url)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SrcsetCandidate {
    url: String,
    score: u32,
}

fn parse_srcset_candidate(candidate: &str) -> Option<SrcsetCandidate> {
    let mut parts = candidate.split_whitespace();
    let url = parts.next()?.trim();
    if url.is_empty() {
        return None;
    }
    let score = parts.filter_map(srcset_descriptor_score).max().unwrap_or(1);
    Some(SrcsetCandidate { url: url.to_string(), score })
}

fn srcset_descriptor_score(value: &str) -> Option<u32> {
    let trimmed = value.trim();
    if let Some(width) = trimmed.strip_suffix('w') {
        return width.parse::<u32>().ok();
    }
    if let Some(scale) = trimmed.strip_suffix('x') {
        return parse_decimal_scale(scale);
    }
    None
}

fn parse_decimal_scale(value: &str) -> Option<u32> {
    let (whole, fractional) = value.split_once('.').unwrap_or((value, ""));
    let whole = whole.parse::<u32>().ok()?;
    let fractional_tenths =
        fractional.chars().next().and_then(|ch| ch.to_digit(10)).unwrap_or_default();
    Some(whole.saturating_mul(1000).saturating_add(fractional_tenths.saturating_mul(100)))
}

fn image_url_attribute(element: ElementRef<'_>, attribute: &str) -> Option<String> {
    element
        .value()
        .attr(attribute)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn preferred_picture_source(element: ElementRef<'_>) -> Option<String> {
    let source_selector = parse_selector("source[srcset]")?;
    let from_sources = element
        .select(&source_selector)
        .filter_map(|source| best_srcset_candidate(source.value().attr("srcset")))
        .find(|source| is_candidate_image_resource(source));
    if from_sources.is_some() {
        return from_sources;
    }
    let image_selector = parse_selector("img")?;
    element.select(&image_selector).find_map(preferred_image_source)
}

fn preferred_background_image_source(element: ElementRef<'_>) -> Option<String> {
    let style = element.value().attr("style")?;
    extract_css_url_values(style).into_iter().find(|source| is_candidate_image_resource(source))
}

fn extract_css_url_values(style: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut remainder = style;
    while let Some(start) = remainder.find("url(") {
        let after_open = &remainder[start + "url(".len()..];
        let Some(end) = after_open.find(')') else {
            break;
        };
        let raw = after_open[..end].trim().trim_matches('"').trim_matches('\'').trim();
        if !raw.is_empty() {
            values.push(raw.to_string());
        }
        remainder = &after_open[end + 1..];
    }
    values
}

fn is_placeholder_image_source(source: &str) -> bool {
    let normalized = source.to_ascii_lowercase();
    normalized.contains("/-/empty/")
        || normalized.contains("/empty/")
        || normalized.contains("placeholder")
        || normalized.contains("transparent")
        || normalized.contains("blank.")
        || normalized.ends_with("/spacer.gif")
}

fn is_ingestable_image_resource(element: ElementRef<'_>, source: &str) -> bool {
    if element_or_ancestor_hidden(element) {
        return false;
    }
    if !is_candidate_image_resource(source) {
        return false;
    }
    if let Some((width, height)) = declared_image_dimensions(element) {
        return width >= MIN_CRAWLABLE_IMAGE_DIMENSION
            && height >= MIN_CRAWLABLE_IMAGE_DIMENSION
            && image_label(element, None).is_some();
    }
    image_label(element, None).is_some()
}

fn is_candidate_image_resource(source: &str) -> bool {
    let trimmed = source.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('#')
        || trimmed.starts_with("data:")
        || is_ignored_href(trimmed)
    {
        return false;
    }
    let path = trimmed
        .split(['?', '#'])
        .next()
        .unwrap_or(trimmed)
        .trim_end_matches('/')
        .to_ascii_lowercase();
    if path.ends_with(".svg") || path.ends_with(".ico") {
        return false;
    }
    matches!(
        path.rsplit('.').next(),
        Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff")
    )
}

fn image_label(element: ElementRef<'_>, figure_caption: Option<&str>) -> Option<String> {
    let candidates = [
        figure_caption,
        element.value().attr("alt"),
        element.value().attr("aria-label"),
        element.value().attr("title"),
    ];
    candidates
        .into_iter()
        .flatten()
        .map(normalize_whitespace)
        .find(|value| !value.is_empty())
        .or_else(|| preferred_image_source(element).map(|source| media_label_from_path(&source)))
        .map(|value| value.chars().take(240).collect::<String>())
        .map(|value| normalize_whitespace(&value))
        .filter(|value| !value.is_empty())
}

fn picture_label(element: ElementRef<'_>) -> Option<String> {
    let image_selector = parse_selector("img")?;
    element.select(&image_selector).find_map(|image| image_label(image, None))
}

fn background_image_label(element: ElementRef<'_>) -> Option<String> {
    for attribute in ["aria-label", "title"] {
        if let Some(value) = element.value().attr(attribute) {
            let normalized = normalize_whitespace(value);
            if !normalized.is_empty() {
                return Some(normalized.chars().take(240).collect());
            }
        }
    }
    let text = normalized_text_from_element(element);
    if text.chars().count() >= 5 {
        return Some(text.chars().take(240).collect());
    }
    None
}

fn figure_caption_text(element: ElementRef<'_>) -> Option<String> {
    let selector = parse_selector("figcaption")?;
    element.select(&selector).map(normalized_text_from_element).find(|value| !value.is_empty())
}

fn element_or_ancestor_hidden(element: ElementRef<'_>) -> bool {
    is_hidden_element(element)
        || element.ancestors().filter_map(ElementRef::wrap).any(is_hidden_element)
}

fn is_hidden_element(element: ElementRef<'_>) -> bool {
    element.value().attr("hidden").is_some()
        || element
            .value()
            .attr("aria-hidden")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || element.value().attr("class").is_some_and(class_hides_element)
        || element.value().attr("style").is_some_and(style_hides_element)
}

fn class_hides_element(class_value: &str) -> bool {
    class_value.split_ascii_whitespace().map(|token| token.trim().to_ascii_lowercase()).any(
        |token| {
            matches!(
                token.as_str(),
                "hidden"
                    | "is-hidden"
                    | "u-hidden"
                    | "d-none"
                    | "sr-only"
                    | "visually-hidden"
                    | "screen-reader-text"
            )
        },
    )
}

fn style_hides_element(style: &str) -> bool {
    style.split(';').map(str::trim).any(|declaration| {
        let Some((property, value)) = declaration.split_once(':') else {
            return false;
        };
        let property = property.trim();
        let value = value
            .trim()
            .trim_end_matches(|ch: char| ch.is_ascii_whitespace())
            .strip_suffix("!important")
            .map(str::trim)
            .unwrap_or_else(|| value.trim());
        property.eq_ignore_ascii_case("display") && value.eq_ignore_ascii_case("none")
            || property.eq_ignore_ascii_case("visibility")
                && (value.eq_ignore_ascii_case("hidden") || value.eq_ignore_ascii_case("collapse"))
    })
}

fn declared_image_dimensions(element: ElementRef<'_>) -> Option<(u32, u32)> {
    let width = parse_dimension_attribute(element.value().attr("width")?)?;
    let height = parse_dimension_attribute(element.value().attr("height")?)?;
    Some((width, height))
}

fn parse_dimension_attribute(value: &str) -> Option<u32> {
    let digits = value.trim().chars().take_while(|ch| ch.is_ascii_digit()).collect::<String>();
    digits.parse().ok()
}

fn markdown_image_label(value: &str) -> String {
    value.replace(['[', ']'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
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

fn collect_outbound_links(root: ElementRef<'_>) -> Vec<String> {
    let Some(selector) = parse_selector("a[href]") else {
        return Vec::new();
    };
    let mut links = BTreeSet::<String>::new();
    for href in root.select(&selector).filter_map(|element| {
        (!element_or_ancestor_hidden(element)).then(|| element.value().attr("href")).flatten()
    }) {
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
        return !matches!(url.scheme(), "http" | "https")
            || url.query().is_some_and(query_has_non_content_action);
    }
    if matches!(href.split_once(':'), Some((scheme, _)) if !matches!(scheme, "http" | "https")) {
        return true;
    }
    href.split_once('?').map(|(_, query)| query_has_non_content_action(query)).unwrap_or(false)
}

fn query_has_non_content_action(query: &str) -> bool {
    query.split('&').any(|part| {
        let Some((name, value)) = part.split_once('=') else {
            return false;
        };
        matches!(name, "action" | "veaction") && value.eq_ignore_ascii_case("edit")
    })
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
    fn title_alone_does_not_create_readable_html_content() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Sign In</title></head>
              <body>
                <form>
                  <input name="email" />
                  <input name="password" />
                </form>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.trim().is_empty());
        assert!(
            output
                .warnings
                .iter()
                .any(|warning| warning == "html page did not yield readable main content")
        );
    }

    #[test]
    fn short_visible_body_content_keeps_page_title_context() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Status Page</title></head>
              <body>
                <main>
                  <p>Version 42 is available.</p>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("# Status Page"));
        assert!(output.content_text.contains("Version 42 is available."));
    }

    #[test]
    fn extracts_meaningful_image_surrogates_and_resources_from_main_content() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Widget Guide</title></head>
              <body>
                <header>
                  <img src="/assets/header-badge.png" width="16" height="16" alt="" />
                </header>
                <main>
                  <h1>Widget Guide</h1>
                  <figure>
                    <img
                      src="/docs/widget-flow.png"
                      width="960"
                      height="540"
                      alt="Widget configuration flow"
                    />
                    <figcaption>Configuration flow from upload to approval.</figcaption>
                  </figure>
                  <img src="data:image/png;base64,AA==" alt="inline pixel" />
                  <img src="/docs/tiny.png" width="8" height="8" />
                  <img src="/docs/vector.svg" width="640" height="480" alt="Vector diagram" />
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("# Widget Guide"));
        assert!(
            output
                .content_text
                .contains("![Configuration flow from upload to approval.](/docs/widget-flow.png)")
        );
        assert!(!output.content_text.contains("header-badge"));
        assert!(!output.content_text.contains("inline pixel"));
        assert!(!output.content_text.contains("tiny.png"));
        assert!(!output.content_text.contains("vector.svg"));

        let resources = output
            .source_map
            .get("outboundResources")
            .and_then(serde_json::Value::as_array)
            .expect("outbound resources");
        assert_eq!(resources, &vec![serde_json::json!("/docs/widget-flow.png")]);
    }

    #[test]
    fn ignores_image_resources_inside_chrome_when_body_is_content_root() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Body Fallback</title></head>
              <body>
                <header>
                  <img src="/chrome/company-logo.png" width="300" height="120" alt="Company logo" />
                </header>
                <article>
                  <h1>Body Fallback</h1>
                  <p>Documentation page without an explicit main element.</p>
                  <img src="/docs/architecture.png" width="960" height="540" alt="Architecture diagram" />
                </article>
                <footer>
                  <img src="/chrome/footer-badge.png" width="300" height="120" alt="Footer badge" />
                </footer>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("![Architecture diagram](/docs/architecture.png)"));
        assert!(!output.content_text.contains("company-logo"));
        assert!(!output.content_text.contains("footer-badge"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["/docs/architecture.png"]))
        );
    }

    #[test]
    fn preserves_text_around_inline_images() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Inline Figure</title></head>
              <body>
                <main>
                  <div>
                    Introductory context
                    <img src="/docs/inline-flow.png" width="640" height="360" alt="Inline flow" />
                    Closing context
                  </div>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("Introductory context Closing context"));
        assert!(output.content_text.contains("![Inline flow](/docs/inline-flow.png)"));
    }

    #[test]
    fn extracts_best_srcset_candidate_when_image_has_no_src() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Diagram Page</title></head>
              <body>
                <main>
                  <img
                    srcset="/images/diagram-small.webp 1x, /images/diagram-large.webp 2x"
                    width="400"
                    height="300"
                    alt="Deployment diagram"
                  />
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("![Deployment diagram](/images/diagram-large.webp)"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["/images/diagram-large.webp"]))
        );
    }

    #[test]
    fn prefers_higher_quality_srcset_candidate_over_src_thumbnail() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Screenshot Page</title></head>
              <body>
                <main>
                  <img
                    src="/images/screenshot-250.png"
                    srcset="/images/screenshot-250.png 250w, /images/screenshot-750.png 750w"
                    width="250"
                    height="160"
                    alt="Settings screenshot"
                  />
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("![Settings screenshot](/images/screenshot-750.png)"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["/images/screenshot-750.png"]))
        );
    }

    #[test]
    fn accepts_protocol_relative_srcset_images_with_structural_size_signal() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Protocol Relative</title></head>
              <body>
                <main>
                  <img
                    src="//cdn.example.test/thumbs/diagram.png"
                    srcset="//cdn.example.test/thumbs/diagram-large.png 2x"
                    width="250"
                    height="160"
                  />
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(
            output
                .content_text
                .contains("![Diagram Large](//cdn.example.test/thumbs/diagram-large.png)")
        );
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["//cdn.example.test/thumbs/diagram-large.png"]))
        );
    }

    #[test]
    fn uses_lazy_image_source_when_src_is_placeholder() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Lazy Image</title></head>
              <body>
                <main>
                  <img
                    src="data:image/gif;base64,R0lGODlhAQABAAAAACw="
                    data-src="/screenshots/checkout-settings.png"
                    width="900"
                    height="520"
                    alt="Checkout settings"
                  />
                  <img
                    data-srcset="/screenshots/audit-small.webp 320w, /screenshots/audit-large.webp 960w"
                    width="480"
                    height="280"
                    alt="Audit flow"
                  />
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(
            output
                .content_text
                .contains("![Checkout settings](/screenshots/checkout-settings.png)")
        );
        assert!(output.content_text.contains("![Audit flow](/screenshots/audit-large.webp)"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!([
                "/screenshots/audit-large.webp",
                "/screenshots/checkout-settings.png"
            ]))
        );
    }

    #[test]
    fn prefers_lazy_original_over_empty_placeholder_src() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Lazy Original</title></head>
              <body>
                <main>
                  <img
                    src="https://cdn.example.test/assets/-/empty/screenshot.png"
                    data-original="https://cdn.example.test/assets/screenshot.png"
                    alt="Builder screenshot"
                  />
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(
            output
                .content_text
                .contains("![Builder screenshot](https://cdn.example.test/assets/screenshot.png)")
        );
        assert!(!output.content_text.contains("/-/empty/screenshot.png"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["https://cdn.example.test/assets/screenshot.png"]))
        );
    }

    #[test]
    fn extracts_raster_background_images_without_svg_chrome() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Visual Section</title></head>
              <body>
                <main>
                  <section style="background-image:url('https://cdn.example.test/screens/app.jpg')">
                    App launch screen
                  </section>
                  <section style="background-image:url('https://cdn.example.test/chrome/wave.svg')">
                    Decorative wave
                  </section>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(
            output
                .content_text
                .contains("![App launch screen](https://cdn.example.test/screens/app.jpg)")
        );
        assert!(!output.content_text.contains("wave.svg"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["https://cdn.example.test/screens/app.jpg"]))
        );
    }

    #[test]
    fn ignores_unlabeled_background_images_as_decorative() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Decorative Background</title></head>
              <body>
                <main>
                  <section style="background-image:url('https://cdn.example.test/patterns/dots.png')"></section>
                  <p>Readable article body.</p>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("Readable article body."));
        assert!(!output.content_text.contains("dots.png"));
        assert_eq!(output.source_map.get("outboundResources"), Some(&serde_json::json!([])));
    }

    #[test]
    fn ignores_hidden_images_and_keeps_visible_pair() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Product Grid</title></head>
              <body>
                <main>
                  <img src="/cards/register.png" alt="Register card" />
                  <img src="/cards/register-hover.png" style="display: none;" alt="Register hover" />
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("![Register card](/cards/register.png)"));
        assert!(!output.content_text.contains("register-hover"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["/cards/register.png"]))
        );
    }

    #[test]
    fn extracts_picture_source_with_nested_image_label() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Responsive Media</title></head>
              <body>
                <main>
                  <picture>
                    <source srcset="/media/report-small.webp 320w, /media/report-large.webp 960w" />
                    <img src="/media/report-fallback.png" alt="Report dashboard" />
                  </picture>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("![Report dashboard](/media/report-large.webp)"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["/media/report-large.webp"]))
        );
    }

    #[test]
    fn scopes_outbound_links_to_selected_main_content() {
        let html = r##"
            <!DOCTYPE html>
            <html>
              <head><title>Article Page</title></head>
              <body>
                <nav><a href="/account/login">Login</a></nav>
                <div id="mw-content-text">
                  <div class="mw-parser-output">
                    <p>Article body.</p>
                    <a href="/docs/topic">Topic</a>
                    <a href="/docs/reference">Reference</a>
                    <a href="#local">Local</a>
                  </div>
                </div>
                <footer><a href="/privacy">Privacy</a></footer>
              </body>
            </html>
        "##;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert_eq!(
            output.source_map.get("outboundLinks"),
            Some(&serde_json::json!(["/docs/reference", "/docs/topic"]))
        );
        assert!(!output.content_text.contains("Login"));
        assert!(!output.content_text.contains("Privacy"));
    }

    #[test]
    fn ignores_edit_action_links_as_page_chrome() {
        let html = r##"
            <!DOCTYPE html>
            <html>
              <head><title>Article Page</title></head>
              <body>
                <main>
                  <p>Article body.</p>
                  <a href="/w/index.php?title=Article&action=edit&section=1">edit</a>
                  <a href="https://editor.example.test/page?veaction=edit">visual edit</a>
                  <a href="/docs/topic?mode=read">Topic</a>
                </main>
              </body>
            </html>
        "##;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(!output.content_text.contains("edit]("));
        assert_eq!(
            output.source_map.get("outboundLinks"),
            Some(&serde_json::json!(["/docs/topic?mode=read"]))
        );
    }

    #[test]
    fn chooses_stronger_high_confidence_content_root() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Article Page</title></head>
              <body>
                <div id="main-content"><a href="/account/login">Login</a></div>
                <div class="mw-parser-output">
                  <p>This article body has enough readable content to outrank a short account link block inside another high-confidence container.</p>
                  <a href="/docs/topic">Topic</a>
                </div>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("This article body has enough readable content"));
        assert!(!output.content_text.contains("Login"));
        assert_eq!(
            output.source_map.get("outboundLinks"),
            Some(&serde_json::json!(["/docs/topic"]))
        );
    }

    #[test]
    fn excludes_script_style_and_hidden_descendant_text_from_blocks() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Clean Article</title></head>
              <body>
                <main>
                  <article>
                    <style>.card { color: red; }</style>
                    <script>window.app = {"debug": true};</script>
                    <p>Readable article body.</p>
                    <span hidden>Invisible template copy</span>
                  </article>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("Readable article body."));
        assert!(!output.content_text.contains(".card"));
        assert!(!output.content_text.contains("window.app"));
        assert!(!output.content_text.contains("Invisible template copy"));
    }

    #[test]
    fn excludes_common_hidden_class_and_important_style_text() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Visibility</title></head>
              <body>
                <main>
                  <p class="visually-hidden">Screen reader duplicate heading</p>
                  <p style="display: none !important">Collapsed duplicate text</p>
                  <p style="visibility: collapse">Collapsed table text</p>
                  <p>Visible body text.</p>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("Visible body text."));
        assert!(!output.content_text.contains("Screen reader duplicate heading"));
        assert!(!output.content_text.contains("Collapsed duplicate text"));
        assert!(!output.content_text.contains("Collapsed table text"));
    }

    #[test]
    fn drops_template_dominated_text_but_preserves_template_prose() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Template Text</title></head>
              <body>
                <main>
                  <p>{{ locale ? title.localized : title.default }}</p>
                  <p>- {{ firstOption }} - {{ secondOption }}</p>
                  <p>Use {{ value }} when documenting template examples in prose.</p>
                  <p>Pass <code>{{ inline_code_sample }}</code> as the parameter value.</p>
                  <pre>{{ preserved_code_sample }}</pre>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(!output.content_text.contains("title.localized"));
        assert!(!output.content_text.contains("firstOption"));
        assert!(
            output
                .content_text
                .contains("Use {{ value }} when documenting template examples in prose.")
        );
        assert!(output.content_text.contains("{{ inline_code_sample }}"));
        assert!(output.content_text.contains("{{ preserved_code_sample }}"));
    }

    #[test]
    fn drops_template_dominated_text_with_small_separator_labels() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Template Chrome</title></head>
              <body>
                <main>
                  <p>or {{ firstOption }} / {{ secondOption }}</p>
                  <p>Use {{ value }} when documenting template examples in prose.</p>
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(!output.content_text.contains("firstOption"));
        assert!(
            output
                .content_text
                .contains("Use {{ value }} when documenting template examples in prose.")
        );
    }

    #[test]
    fn ignores_unsupported_srcset_candidate_without_discarding_supported_images() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head><title>Mixed Srcset</title></head>
              <body>
                <main>
                  <img
                    src="/screenshots/fallback.png"
                    srcset="/screenshots/readable.png 640w, /screenshots/readable.avif 1920w"
                    width="640"
                    height="360"
                    alt="Readable screenshot"
                  />
                </main>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("html extraction");

        assert!(output.content_text.contains("![Readable screenshot](/screenshots/readable.png)"));
        assert_eq!(
            output.source_map.get("outboundResources"),
            Some(&serde_json::json!(["/screenshots/readable.png"]))
        );
    }

    #[test]
    fn prefers_confluence_main_content_over_outer_container() {
        let html = r#"
            <!DOCTYPE html>
            <html>
              <head>
                <title>Acme Software Products</title>
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

        assert!(output.content_text.contains("# Acme Software Products"));
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
                <title>Acme Operations Console</title>
              </head>
              <body>
                <div id="content">
                  <div class="page-metadata">Created by Alice</div>
                  <div id="page-metadata-banner">3 attachments</div>
                  <div id="breadcrumbs">Docs / Products / Operations Console</div>
                  <div id="main-content" class="wiki-content">
                    <h1>Acme Operations Console</h1>
                    <p>Operations Console is used to manage distributed workflows.</p>
                    <p>It centralizes settings, notifications, and remote administration flows.</p>
                  </div>
                </div>
              </body>
            </html>
        "#;

        let output =
            extract_html_main_content(html.as_bytes(), Some("text/html")).expect("confluence html");

        assert!(output.content_text.contains("Operations Console is used to manage"));
        assert!(!output.content_text.contains("Created by Alice"));
        assert!(!output.content_text.contains("3 attachments"));
        assert!(!output.content_text.contains("Docs / Products / Operations Console"));
    }
}
