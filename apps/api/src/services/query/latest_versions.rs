const LATEST_VERSION_DEFAULT_COUNT: usize = 5;
const LATEST_VERSION_MAX_COUNT: usize = 10;
pub(crate) const LATEST_VERSION_CHUNKS_PER_DOCUMENT: usize = 4;

pub(crate) fn question_requests_latest_versions(question: &str) -> bool {
    let lower = question.to_lowercase();
    let asks_latest =
        lower.contains("послед") || lower.contains("latest") || lower.contains("recent");
    let asks_version = lower.contains("верс")
        || lower.contains("релиз")
        || lower.contains("version")
        || lower.contains("release");
    asks_latest && asks_version
}

pub(crate) fn requested_latest_version_count(question: &str) -> usize {
    let tokens = lexical_tokens(question);
    for (index, token) in tokens.iter().enumerate() {
        if !token.chars().all(|ch| ch.is_ascii_digit()) {
            continue;
        }
        let Ok(value) = token.parse::<usize>() else {
            continue;
        };
        if value == 0 || (1900..=2100).contains(&value) {
            continue;
        }
        let start = index.saturating_sub(2);
        let end = (index + 3).min(tokens.len());
        let nearby = &tokens[start..end];
        if nearby.iter().any(|item| is_latest_word(item))
            && nearby.iter().any(|item| is_version_release_word(item))
        {
            return value.clamp(1, LATEST_VERSION_MAX_COUNT);
        }
    }
    LATEST_VERSION_DEFAULT_COUNT
}

pub(crate) fn latest_version_context_top_k(question: &str, base_limit: usize) -> usize {
    if !question_requests_latest_versions(question) {
        return base_limit;
    }
    base_limit.max(
        requested_latest_version_count(question).saturating_mul(LATEST_VERSION_CHUNKS_PER_DOCUMENT),
    )
}

pub(crate) fn latest_version_chunk_score(
    score_floor: f32,
    requested_count: usize,
    document_rank: usize,
    chunk_rank: usize,
) -> f32 {
    let band = LATEST_VERSION_CHUNKS_PER_DOCUMENT.saturating_sub(chunk_rank).max(1);
    let offset = band.saturating_mul(requested_count).saturating_sub(document_rank);
    score_floor + offset as f32
}

pub(crate) fn latest_version_scope_terms(question: &str) -> Vec<String> {
    lexical_tokens(question)
        .into_iter()
        .filter(|token| token.chars().count() >= 3)
        .filter(|token| !token.chars().any(|ch| ch.is_ascii_digit()))
        .filter(|token| !is_latest_version_generic_word(token))
        .collect()
}

pub(crate) fn latest_version_family_key(text: &str) -> String {
    let lower = text.to_lowercase();
    let chars = lower.chars().collect::<Vec<_>>();
    let mut index = 0;
    let mut out = String::with_capacity(lower.len());
    while index < chars.len() {
        let ch = chars[index];
        if ch.is_ascii_digit() {
            let start = index;
            let mut end = index + 1;
            let mut has_dot = false;
            while end < chars.len() && (chars[end].is_ascii_digit() || chars[end] == '.') {
                if chars[end] == '.' {
                    has_dot = true;
                }
                end += 1;
            }
            if has_dot {
                out.push_str("{version}");
                index = end;
                continue;
            }
            out.extend(chars[start..end].iter());
            index = end;
            continue;
        }
        out.push(ch);
        index += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn text_has_release_version_marker(text: &str) -> bool {
    lexical_tokens(text)
        .iter()
        .any(|token| is_version_release_word(token) || is_change_log_word(token))
}

fn lexical_tokens(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|ch: char| !(ch.is_alphanumeric() || ch == '.'))
        .map(|token| token.trim_matches('.'))
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn is_latest_word(token: &str) -> bool {
    token.contains("послед") || matches!(token, "latest" | "recent" | "last")
}

fn is_version_release_word(token: &str) -> bool {
    token.contains("верс")
        || token.contains("релиз")
        || matches!(token, "version" | "versions" | "release" | "releases")
}

fn is_change_log_word(token: &str) -> bool {
    token.contains("измен")
        || matches!(token, "changelog" | "changes" | "change" | "whatsnew" | "whatnew")
}

fn is_latest_version_generic_word(token: &str) -> bool {
    is_latest_word(token)
        || is_version_release_word(token)
        || is_change_log_word(token)
        || token.contains("кажд")
        || token.contains("спис")
        || token.contains("нов")
        || matches!(
            token,
            "что" | "нового" | "new" | "what" | "whats" | "what's" | "per" | "each" | "list"
        )
}

pub(crate) fn extract_semver_like_version(text: &str) -> Option<Vec<u32>> {
    let chars = text.char_indices().collect::<Vec<_>>();
    for (index, &(start, ch)) in chars.iter().enumerate() {
        if !ch.is_ascii_digit() {
            continue;
        }
        let mut end = start + ch.len_utf8();
        for &(_, next) in chars.iter().skip(index + 1) {
            if next.is_ascii_digit() || next == '.' {
                end += next.len_utf8();
            } else {
                break;
            }
        }
        let candidate = text[start..end].trim_matches('.');
        let parts = candidate
            .split('.')
            .filter(|part| !part.is_empty())
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        if parts.len() >= 2 {
            return Some(parts);
        }
    }
    None
}

pub(crate) fn compare_version_desc(left: &[u32], right: &[u32]) -> std::cmp::Ordering {
    let len = left.len().max(right.len());
    for index in 0..len {
        let left_part = left.get(index).copied().unwrap_or(0);
        let right_part = right.get(index).copied().unwrap_or(0);
        match right_part.cmp(&left_part) {
            std::cmp::Ordering::Equal => continue,
            ordering => return ordering,
        }
    }
    std::cmp::Ordering::Equal
}
