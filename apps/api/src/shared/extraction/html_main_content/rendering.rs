use reqwest::Url;
use scraper::{ElementRef, Html, Selector};

use super::media::{
    element_or_ancestor_hidden, figure_caption_text, push_background_image_block, push_image_block,
    push_picture_block,
};
use super::metadata::is_ignored_href;

pub(super) fn select_content_root(document: &Html) -> Option<ElementRef<'_>> {
    const CONTENT_ROOT_SELECTORS: [&str; 3] = ["main", "article", "[role='main']"];

    let mut best: Option<(usize, ElementRef<'_>)> = None;
    for query in CONTENT_ROOT_SELECTORS {
        let Some(selector) = parse_selector(query) else {
            continue;
        };
        for element in document.select(&selector) {
            let score = content_root_score(element);
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

pub(super) fn parse_selector(query: &str) -> Option<Selector> {
    Selector::parse(query).ok()
}

pub(super) fn render_markdownish_text(root: ElementRef<'_>, title: Option<&str>) -> String {
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

pub(super) fn direct_child_elements(element: ElementRef<'_>) -> Vec<ElementRef<'_>> {
    element.children().filter_map(ElementRef::wrap).collect()
}

pub(super) fn is_ignored_tag(tag_name: &str) -> bool {
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

fn content_root_score(element: ElementRef<'_>) -> usize {
    let text_len = normalized_text_from_element(element).chars().count();
    let heading_count = count_descendants(element, "h1, h2, h3");
    let link_count = count_descendants(element, "a[href]");
    let image_count = count_descendants(element, "img");

    if text_len == 0 && heading_count == 0 && link_count == 0 && image_count == 0 {
        return 0;
    }

    text_len + heading_count * 120 + link_count * 18 + image_count * 24
}

fn count_descendants(element: ElementRef<'_>, query: &str) -> usize {
    let Some(selector) = parse_selector(query) else {
        return 0;
    };
    element.select(&selector).count()
}

pub(super) fn normalized_text_from_element(element: ElementRef<'_>) -> String {
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

pub(super) fn normalize_whitespace(value: &str) -> String {
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
    link_label_from_attributes(element)
        .or_else(|| link_label_from_images(element))
        .unwrap_or_else(|| fallback_link_label_from_href(href))
}

fn link_label_from_attributes(element: ElementRef<'_>) -> Option<String> {
    ["aria-label", "title"].into_iter().find_map(|attribute| {
        element.value().attr(attribute).map(normalize_whitespace).filter(|value| !value.is_empty())
    })
}

fn link_label_from_images(element: ElementRef<'_>) -> Option<String> {
    let image_selector = parse_selector("img")?;
    element.select(&image_selector).find_map(link_label_from_image)
}

fn link_label_from_image(image: ElementRef<'_>) -> Option<String> {
    image.value().attr("alt").map(normalize_whitespace).filter(|value| !value.is_empty()).or_else(
        || image.value().attr("src").map(media_label_from_path).filter(|value| !value.is_empty()),
    )
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

pub(super) fn media_label_from_path(path: &str) -> String {
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

pub(super) fn push_block(blocks: &mut Vec<String>, value: String) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }
    if blocks.last().is_some_and(|existing| existing == trimmed) {
        return;
    }
    blocks.push(trimmed.to_string());
}
