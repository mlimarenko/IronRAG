use std::collections::BTreeSet;

use scraper::ElementRef;

use super::metadata::is_ignored_href;
use super::rendering::{
    direct_child_elements, is_ignored_tag, media_label_from_path, normalize_whitespace,
    normalized_text_from_element, parse_selector, push_block,
};
use super::{HTML_RESOURCE_LIMIT, MIN_CRAWLABLE_IMAGE_DIMENSION};

pub(super) fn push_image_block(
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

pub(super) fn push_picture_block(blocks: &mut Vec<String>, element: ElementRef<'_>) -> bool {
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

pub(super) fn push_background_image_block(
    blocks: &mut Vec<String>,
    element: ElementRef<'_>,
) -> bool {
    let Some(source) = preferred_background_image_source(element) else {
        return false;
    };
    let Some(label) = background_image_label(element) else {
        return false;
    };
    push_block(blocks, format!("![{}]({source})", markdown_image_label(&label)));
    true
}

pub(super) fn collect_outbound_resources(root: ElementRef<'_>) -> Vec<String> {
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
    lazy_sources.or(src)
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
    let extension = path.rsplit('.').next();
    if matches!(extension, Some("svg" | "ico")) {
        return false;
    }
    matches!(extension, Some("png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff"))
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

pub(super) fn figure_caption_text(element: ElementRef<'_>) -> Option<String> {
    let selector = parse_selector("figcaption")?;
    element.select(&selector).map(normalized_text_from_element).find(|value| !value.is_empty())
}

pub(super) fn element_or_ancestor_hidden(element: ElementRef<'_>) -> bool {
    is_hidden_element(element)
        || element.ancestors().filter_map(ElementRef::wrap).any(is_hidden_element)
}

fn is_hidden_element(element: ElementRef<'_>) -> bool {
    element.value().attr("hidden").is_some()
        || element
            .value()
            .attr("aria-hidden")
            .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || element.value().attr("style").is_some_and(style_hides_element)
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
            .map_or_else(|| value.trim(), str::trim);
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
    let digits = value.trim().chars().take_while(char::is_ascii_digit).collect::<String>();
    digits.parse().ok()
}

fn markdown_image_label(value: &str) -> String {
    value.replace(['[', ']'], " ").split_whitespace().collect::<Vec<_>>().join(" ")
}
