use std::collections::BTreeSet;

use reqwest::Url;
use scraper::{ElementRef, Html};

use super::HTML_LINK_LIMIT;
use super::media::element_or_ancestor_hidden;
use super::rendering::{normalize_whitespace, normalized_text_from_element, parse_selector};

pub(super) fn extract_title(document: &Html) -> Option<String> {
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

pub(super) fn collect_outbound_links(root: ElementRef<'_>) -> Vec<String> {
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

pub(super) fn is_ignored_href(href: &str) -> bool {
    if let Ok(url) = Url::parse(href) {
        return !matches!(url.scheme(), "http" | "https");
    }
    matches!(href.split_once(':'), Some((scheme, _)) if !matches!(scheme, "http" | "https"))
}
