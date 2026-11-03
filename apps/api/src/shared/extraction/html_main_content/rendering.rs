use super::media::element_or_ancestor_hidden;
use scraper::{ElementRef, Html, Selector};

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

/// Product root/visibility policy only; Xberg owns conversion to searchable text.
pub(super) fn selected_html(root: ElementRef<'_>) -> String {
    fn escape(value: &str) -> String {
        value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }
    fn visit(element: ElementRef<'_>, output: &mut String) {
        let tag = element.value().name();
        if is_ignored_tag(tag)
            || element_or_ancestor_hidden(element)
            || matches!(tag, "img" | "picture")
        {
            return;
        }
        output.push('<');
        output.push_str(tag);
        for (name, value) in element.value().attrs() {
            output.push(' ');
            output.push_str(name);
            output.push_str("=\"");
            output.push_str(&escape(value));
            output.push('"');
        }
        output.push('>');
        for child in element.children() {
            if let Some(text) = child.value().as_text() {
                if is_template_literal_container(tag) || !is_template_dominated_text(text) {
                    output.push_str(&escape(text));
                }
            } else if let Some(child) = ElementRef::wrap(child) {
                visit(child, output);
            }
        }
        output.push_str("</");
        output.push_str(tag);
        output.push('>');
    }
    let mut output = String::new();
    visit(root, &mut output);
    output
}

pub(super) fn media_blocks(root: ElementRef<'_>) -> Vec<String> {
    fn visit(element: ElementRef<'_>, blocks: &mut Vec<String>) {
        if is_ignored_tag(element.value().name()) || element_or_ancestor_hidden(element) {
            return;
        }
        super::media::push_background_image_block(blocks, element);
        match element.value().name() {
            "picture" => {
                super::media::push_picture_block(blocks, element);
                return;
            }
            "img" => {
                super::media::push_image_block(blocks, element, None);
                return;
            }
            "figure" => {
                let caption = super::media::figure_caption_text(element);
                if let Some(selector) = parse_selector("img") {
                    for image in element.select(&selector) {
                        super::media::push_image_block(blocks, image, caption.as_deref());
                    }
                }
                return;
            }
            _ => {}
        }
        for child in direct_child_elements(element) {
            visit(child, blocks);
        }
    }
    let mut blocks = Vec::new();
    visit(root, &mut blocks);
    blocks
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
