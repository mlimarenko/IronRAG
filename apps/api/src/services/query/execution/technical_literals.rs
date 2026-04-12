#![allow(dead_code, unused_imports, unused_variables, unused_mut)]
use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use super::answer::concise_document_subject_label;
use super::retrieve::{focused_excerpt_for, score_value};
use super::types::RuntimeMatchedChunk;

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct TechnicalLiteralIntent {
    pub(crate) wants_urls: bool,
    pub(crate) wants_prefixes: bool,
    pub(crate) wants_paths: bool,
    pub(crate) wants_methods: bool,
    pub(crate) wants_parameters: bool,
}

#[derive(Debug, Clone, Default)]
pub(super) struct TechnicalLiteralDocumentGroup {
    pub(super) document_label: String,
    pub(super) matched_excerpt: Option<String>,
    pub(super) urls: Vec<String>,
    pub(super) url_seen: HashSet<String>,
    pub(super) prefixes: Vec<String>,
    pub(super) prefix_seen: HashSet<String>,
    pub(super) paths: Vec<String>,
    pub(super) path_seen: HashSet<String>,
    pub(super) methods: Vec<String>,
    pub(super) method_seen: HashSet<String>,
    pub(super) parameters: Vec<String>,
    pub(super) parameter_seen: HashSet<String>,
}

impl TechnicalLiteralDocumentGroup {
    fn new(document_label: String) -> Self {
        Self { document_label, ..Self::default() }
    }

    pub(super) fn has_any(&self) -> bool {
        self.matched_excerpt.is_some()
            || !self.urls.is_empty()
            || !self.prefixes.is_empty()
            || !self.paths.is_empty()
            || !self.methods.is_empty()
            || !self.parameters.is_empty()
    }
}

impl TechnicalLiteralIntent {
    pub(super) fn any(self) -> bool {
        self.wants_urls
            || self.wants_prefixes
            || self.wants_paths
            || self.wants_methods
            || self.wants_parameters
    }
}

pub(super) fn technical_literal_candidate_limit(
    intent: TechnicalLiteralIntent,
    top_k: usize,
) -> usize {
    if !intent.any() {
        return top_k;
    }

    let multiplier =
        if intent.wants_paths || intent.wants_urls || intent.wants_methods { 4 } else { 3 };
    top_k.saturating_mul(multiplier).clamp(top_k, 64)
}

pub(super) fn detect_technical_literal_intent(question: &str) -> TechnicalLiteralIntent {
    let lowered = question.to_lowercase();
    let wants_urls =
        ["url", "wsdl", "адрес", "ссылка", "endpoint", "эндпоинт", "префикс", "базовый url"]
            .iter()
            .any(|needle| lowered.contains(needle));
    let wants_prefixes =
        ["префикс", "base url", "базовый url"].iter().any(|needle| lowered.contains(needle));
    let wants_paths = wants_urls
        || ["path", "путь", "маршрут", "endpoint", "эндпоинт"]
            .iter()
            .any(|needle| lowered.contains(needle));
    let wants_methods = wants_urls
        || ["метод http", "http method", "get ", "post ", "put ", "patch ", "delete "]
            .iter()
            .any(|needle| lowered.contains(needle));
    let wants_parameters = ["параметр", "аргумент", "пейджинац", "query parameter"]
        .iter()
        .any(|needle| lowered.contains(needle));

    TechnicalLiteralIntent {
        wants_urls,
        wants_prefixes,
        wants_paths,
        wants_methods,
        wants_parameters,
    }
}

pub(super) fn trim_literal_token(token: &str) -> &str {
    token.trim_matches(|ch: char| {
        ch.is_whitespace()
            || matches!(ch, ',' | ';' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'')
    })
}

pub(super) fn technical_literal_focus_keywords(question: &str) -> Vec<String> {
    let ignored_keywords = [
        "если",
        "агенту",
        "ему",
        "какой",
        "какие",
        "какая",
        "какого",
        "какому",
        "endpoint",
        "url",
        "port",
        "порт",
        "path",
        "путь",
        "пути",
        "метод",
        "method",
        "использует",
        "используют",
        "used",
        "uses",
        "возвращает",
        "получить",
        "нужно",
        "нужен",
        "нужны",
        "отдельно",
    ]
    .into_iter()
    .collect::<HashSet<_>>();
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();
    for token in question
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '/')
        .map(str::trim)
        .filter(|token| token.chars().count() >= 4)
        .map(str::to_lowercase)
    {
        if ignored_keywords.contains(token.as_str()) {
            continue;
        }
        if seen.insert(token.clone()) {
            keywords.push(token.clone());
        }
    }
    keywords
}

fn technical_keyword_stem(keyword: &str) -> Option<String> {
    let stem = keyword.chars().take(5).collect::<String>();
    (stem.chars().count() >= 4).then_some(stem)
}

pub(super) fn technical_keyword_present(lowered_text: &str, keyword: &str) -> bool {
    lowered_text.contains(keyword)
        || technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str()))
}

pub(super) fn technical_keyword_weight(lowered_text: &str, keyword: &str) -> usize {
    if lowered_text.contains(keyword) {
        return keyword.chars().count().min(24);
    }
    if technical_keyword_stem(keyword).is_some_and(|stem| lowered_text.contains(stem.as_str())) {
        return 4;
    }
    0
}

pub(super) fn question_mentions_pagination(question: &str) -> bool {
    let lowered = question.to_lowercase();
    ["bypage", "page", "pagesize", "pagenumber", "пейдж", "постранич", "страниц", "пагинац"]
        .iter()
        .any(|marker| lowered.contains(marker))
}

pub(super) fn question_mentions_protocol(question: &str) -> bool {
    let lowered = question.to_lowercase();
    lowered.contains("protocol") || lowered.contains("протокол")
}

pub(super) fn technical_literal_focus_segments_text(question: &str) -> Vec<String> {
    question
        .to_lowercase()
        .replace(" и отдельно ", " | ")
        .replace(" отдельно ", " | ")
        .replace(" and then ", " | ")
        .replace(" then ", " | ")
        .replace(" and ", " | ")
        .replace([';', ','], "|")
        .split('|')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>()
}

pub(super) fn technical_literal_focus_keyword_segments(question: &str) -> Vec<Vec<String>> {
    let segments = technical_literal_focus_segments_text(question)
        .into_iter()
        .map(|segment| technical_literal_focus_keywords(&segment))
        .filter(|keywords| !keywords.is_empty())
        .collect::<Vec<_>>();
    if segments.is_empty() {
        let fallback = technical_literal_focus_keywords(question);
        if fallback.is_empty() { Vec::new() } else { vec![fallback] }
    } else {
        segments
    }
}

pub(super) fn document_local_focus_keywords(
    question: &str,
    chunks: &[&RuntimeMatchedChunk],
    question_keywords: &[String],
) -> Vec<String> {
    if question_keywords.is_empty() {
        return Vec::new();
    }

    let document_text = chunks
        .iter()
        .map(|chunk| format!("{} {}", chunk.excerpt, chunk.source_text))
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    let best_segment = technical_literal_focus_keyword_segments(question)
        .into_iter()
        .map(|segment_keywords| {
            let score = segment_keywords
                .iter()
                .map(|keyword| technical_keyword_weight(&document_text, keyword))
                .sum::<usize>();
            (score, segment_keywords)
        })
        .max_by_key(|(score, _)| *score)
        .filter(|(score, _)| *score > 0)
        .map(|(_, segment_keywords)| segment_keywords);
    if let Some(segment_keywords) = best_segment {
        let local_segment_keywords = segment_keywords
            .iter()
            .filter(|keyword| technical_keyword_present(&document_text, keyword))
            .cloned()
            .collect::<Vec<_>>();
        if !local_segment_keywords.is_empty() {
            return local_segment_keywords;
        }
        return segment_keywords;
    }
    let local_keywords = question_keywords
        .iter()
        .filter(|keyword| technical_keyword_present(&document_text, keyword))
        .cloned()
        .collect::<Vec<_>>();
    if local_keywords.is_empty() { question_keywords.to_vec() } else { local_keywords }
}

pub(super) fn technical_chunk_selection_score(
    text: &str,
    keywords: &[String],
    pagination_requested: bool,
) -> isize {
    let lowered = text.to_lowercase();
    let keyword_count = keywords.len();
    let mut score = keywords
        .iter()
        .enumerate()
        .map(|(index, keyword)| {
            let priority = keyword_count.saturating_sub(index).max(1) as isize;
            (technical_keyword_weight(&lowered, keyword) as isize) * priority
        })
        .sum::<isize>();
    let has_pagination_marker = ["bypage", "pagesize", "pagenumber", "number_starting"]
        .iter()
        .any(|marker| lowered.contains(marker));
    if has_pagination_marker {
        score += if pagination_requested { 12 } else { -40 };
    }
    score
}

pub(super) fn select_document_balanced_chunks<'a>(
    question: &str,
    chunks: &'a [RuntimeMatchedChunk],
    keywords: &[String],
    pagination_requested: bool,
    max_total_chunks: usize,
    max_chunks_per_document: usize,
) -> Vec<&'a RuntimeMatchedChunk> {
    let mut ordered_document_ids = Vec::<Uuid>::new();
    let mut per_document_chunks = HashMap::<Uuid, Vec<&RuntimeMatchedChunk>>::new();

    for chunk in chunks {
        if !per_document_chunks.contains_key(&chunk.document_id) {
            ordered_document_ids.push(chunk.document_id);
        }
        per_document_chunks.entry(chunk.document_id).or_default().push(chunk);
    }

    for document_chunks in per_document_chunks.values_mut() {
        let local_keywords = document_local_focus_keywords(question, document_chunks, keywords);
        document_chunks.sort_by(|left, right| {
            let left_match = technical_chunk_selection_score(
                &format!("{} {}", left.excerpt, left.source_text),
                &local_keywords,
                pagination_requested,
            );
            let right_match = technical_chunk_selection_score(
                &format!("{} {}", right.excerpt, right.source_text),
                &local_keywords,
                pagination_requested,
            );
            right_match
                .cmp(&left_match)
                .then_with(|| score_value(right.score).total_cmp(&score_value(left.score)))
        });
    }

    let mut selected = Vec::new();
    for target_document_slot in 0..max_chunks_per_document {
        for document_id in &ordered_document_ids {
            if selected.len() >= max_total_chunks {
                return selected;
            }
            if let Some(chunk) = per_document_chunks
                .get(document_id)
                .and_then(|document_chunks| document_chunks.get(target_document_slot))
            {
                selected.push(*chunk);
            }
        }
    }

    selected
}

fn push_unique_limited(
    target: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: String,
    limit: usize,
) {
    if value.is_empty() || target.len() >= limit {
        return;
    }
    if seen.insert(value.clone()) {
        target.push(value);
    }
}

pub(super) fn extract_url_literals(text: &str, limit: usize) -> Vec<String> {
    let mut urls = Vec::new();
    let mut seen = HashSet::new();
    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        let trailing_open_placeholder = cleaned.rfind('<').is_some_and(|left_index| {
            cleaned.rfind('>').is_none_or(|right_index| left_index > right_index)
        });
        let has_unbalanced_angle_brackets = (cleaned.contains('<') && !cleaned.contains('>'))
            || (cleaned.contains('>') && !cleaned.contains('<'));
        if cleaned.starts_with("http://") || cleaned.starts_with("https://") {
            if !has_unbalanced_angle_brackets && !trailing_open_placeholder {
                push_unique_limited(&mut urls, &mut seen, cleaned.to_string(), limit);
            }
        }
    }
    urls
}

pub(super) fn derive_path_literals_from_url(url: &str) -> Vec<String> {
    let Some(scheme_index) = url.find("://") else {
        return Vec::new();
    };
    let remainder = &url[(scheme_index + 3)..];
    let Some(path_index) = remainder.find('/') else {
        return Vec::new();
    };
    let path = &remainder[path_index..];
    if path.is_empty() {
        return Vec::new();
    }

    let mut paths = vec![path.to_string()];
    let segments =
        path.trim_matches('/').split('/').filter(|segment| !segment.is_empty()).collect::<Vec<_>>();
    if segments.len() >= 2 {
        paths.push(format!("/{}/{}/", segments[0], segments[1]));
    }
    if segments.len() >= 3 && !segments[2].contains('.') {
        paths.push(format!("/{}/{}/{}/", segments[0], segments[1], segments[2]));
    }
    paths
}

pub(super) fn extract_explicit_path_literals(text: &str, limit: usize) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        if cleaned.starts_with('/') && cleaned.matches('/').count() >= 1 {
            push_unique_limited(&mut paths, &mut seen, cleaned.to_string(), limit);
        }
    }

    if paths.is_empty() {
        for url in extract_url_literals(text, limit.saturating_mul(2).max(4)) {
            if let Some(full_path) = derive_path_literals_from_url(&url).into_iter().next() {
                push_unique_limited(&mut paths, &mut seen, full_path, limit);
            }
        }
    }

    paths
}

pub(super) fn extract_prefix_literals(text: &str, limit: usize) -> Vec<String> {
    let mut prefixes = Vec::new();
    let mut seen = HashSet::new();

    for url in extract_url_literals(text, limit.saturating_mul(2).max(4)) {
        for candidate in derive_path_literals_from_url(&url) {
            if candidate.ends_with('/') {
                push_unique_limited(&mut prefixes, &mut seen, candidate, limit);
            }
        }
    }

    prefixes
}

pub(super) fn extract_protocol_literals(text: &str, limit: usize) -> Vec<String> {
    let mut protocols = Vec::new();
    let mut seen = HashSet::new();
    let lowered = text.to_lowercase();

    if lowered.contains("graphql") {
        push_unique_limited(&mut protocols, &mut seen, "GraphQL".to_string(), limit);
    }
    if lowered.contains("soap") {
        push_unique_limited(&mut protocols, &mut seen, "SOAP".to_string(), limit);
    }
    if lowered.contains("rest")
        || lowered.contains("restful api")
        || lowered.contains("rest-интерфейс")
        || lowered.contains("rest interface")
    {
        push_unique_limited(&mut protocols, &mut seen, "REST".to_string(), limit);
    }

    protocols
}

pub(super) fn extract_http_methods(text: &str, limit: usize) -> Vec<String> {
    let mut methods = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        if matches!(cleaned, "GET" | "POST" | "PUT" | "PATCH" | "DELETE") {
            push_unique_limited(&mut methods, &mut seen, cleaned.to_string(), limit);
        }
    }

    methods
}

fn looks_like_parameter_identifier(token: &str) -> bool {
    if token.len() < 3 || token.len() > 64 || !token.is_ascii() {
        return false;
    }
    let Some(first) = token.chars().next() else {
        return false;
    };
    if !first.is_ascii_lowercase() {
        return false;
    }
    if !token.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
        return false;
    }

    token.contains('_')
        || token.starts_with("page")
        || token.starts_with("with")
        || token.chars().skip(1).any(|ch| ch.is_ascii_uppercase())
}

pub(super) fn extract_parameter_literals(text: &str, limit: usize) -> Vec<String> {
    let mut parameters = Vec::new();
    let mut seen = HashSet::new();

    for token in text.split_whitespace() {
        let cleaned =
            trim_literal_token(token).trim_end_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
        if looks_like_parameter_identifier(cleaned) {
            push_unique_limited(&mut parameters, &mut seen, cleaned.to_string(), limit);
        }
    }

    parameters
}

pub(super) fn collect_technical_literal_groups(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Vec<TechnicalLiteralDocumentGroup> {
    let intent = detect_technical_literal_intent(question);
    if !intent.any() {
        return Vec::new();
    }

    let mut groups: Vec<TechnicalLiteralDocumentGroup> = Vec::new();
    let literal_focus_keywords = technical_literal_focus_keywords(question);
    let pagination_requested = question_mentions_pagination(question);

    for chunk in select_document_balanced_chunks(
        question,
        chunks,
        &literal_focus_keywords,
        pagination_requested,
        8,
        1,
    ) {
        let group_index = groups
            .iter()
            .position(|group| group.document_label == chunk.document_label)
            .unwrap_or_else(|| {
                groups.push(TechnicalLiteralDocumentGroup::new(chunk.document_label.clone()));
                groups.len() - 1
            });
        let group = &mut groups[group_index];
        if group.matched_excerpt.is_none() && !chunk.excerpt.trim().is_empty() {
            group.matched_excerpt = Some(chunk.excerpt.trim().to_string());
        }
        let focused_source_text =
            focused_excerpt_for(&chunk.source_text, &literal_focus_keywords, 900);
        let literal_source_text = if focused_source_text.trim().is_empty() {
            chunk.source_text.as_str()
        } else {
            focused_source_text.as_str()
        };

        if intent.wants_urls {
            for value in extract_url_literals(literal_source_text, 6) {
                push_unique_limited(&mut group.urls, &mut group.url_seen, value, 6);
            }
        }
        if intent.wants_prefixes {
            for value in extract_prefix_literals(literal_source_text, 6) {
                push_unique_limited(&mut group.prefixes, &mut group.prefix_seen, value, 6);
            }
        }
        if intent.wants_paths {
            for value in extract_explicit_path_literals(literal_source_text, 10) {
                push_unique_limited(&mut group.paths, &mut group.path_seen, value, 10);
            }
        }
        if intent.wants_methods {
            for value in extract_http_methods(literal_source_text, 5) {
                push_unique_limited(&mut group.methods, &mut group.method_seen, value, 5);
            }
        }
        if intent.wants_parameters {
            for value in extract_parameter_literals(literal_source_text, 8) {
                push_unique_limited(&mut group.parameters, &mut group.parameter_seen, value, 8);
            }
        }
    }

    groups.into_iter().filter(|group| group.has_any()).collect()
}

pub(super) fn render_exact_technical_literals_section(
    groups: &[TechnicalLiteralDocumentGroup],
) -> Option<String> {
    if groups.is_empty() {
        return None;
    }

    let mut lines = Vec::new();
    for group in groups.iter().filter(|group| group.has_any()) {
        lines.push(format!("- Document: `{}`", group.document_label));
        if let Some(excerpt) = &group.matched_excerpt {
            lines.push(format!("  Matched excerpt: {excerpt}"));
        }
        if !group.urls.is_empty() {
            lines.push(format!(
                "  URLs: {}",
                group.urls.iter().map(|value| format!("`{value}`")).collect::<Vec<_>>().join(", ")
            ));
        }
        if !group.prefixes.is_empty() {
            lines.push(format!(
                "  Prefixes: {}",
                group
                    .prefixes
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.paths.is_empty() {
            lines.push(format!(
                "  Paths: {}",
                group.paths.iter().map(|value| format!("`{value}`")).collect::<Vec<_>>().join(", ")
            ));
        }
        if !group.methods.is_empty() {
            lines.push(format!(
                "  HTTP methods: {}",
                group
                    .methods
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !group.parameters.is_empty() {
            lines.push(format!(
                "  Parameters: {}",
                group
                    .parameters
                    .iter()
                    .map(|value| format!("`{value}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }

    if lines.is_empty() {
        return None;
    }

    Some(format!("Exact technical literals\n{}", lines.join("\n")))
}

#[cfg(test)]
pub(super) fn build_exact_technical_literals_section(
    question: &str,
    chunks: &[RuntimeMatchedChunk],
) -> Option<String> {
    let groups = collect_technical_literal_groups(question, chunks);
    render_exact_technical_literals_section(&groups)
}

pub(super) fn infer_endpoint_subject_label(group: &TechnicalLiteralDocumentGroup) -> String {
    concise_document_subject_label(&group.document_label)
}
