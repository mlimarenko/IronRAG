use std::collections::HashSet;

use crate::services::query::latest_versions::prefixed_semver_like_literal_at;

const MAX_FORMAL_EXACT_LITERAL_BYTES: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FormalExactClaimKind {
    Numeric,
    IsoDate,
    PrefixedVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FormalExactClaim {
    literal: String,
    kind: FormalExactClaimKind,
}

impl FormalExactClaim {
    pub(super) fn new(literal: impl Into<String>, kind: FormalExactClaimKind) -> Self {
        Self { literal: literal.into(), kind }
    }

    pub(super) fn literal(&self) -> &str {
        &self.literal
    }

    pub(super) fn kind(&self) -> FormalExactClaimKind {
        self.kind
    }
}

pub(super) fn extract_answer_literals(answer: &str) -> (Vec<String>, Vec<String>) {
    let mut literals = AnswerLiterals::default();
    let bytes = answer.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        match answer_literal_at(answer, cursor) {
            AnswerLiteralAt::Inline(next_cursor, literal) => {
                literals.push_inline(literal);
                cursor = next_cursor;
            }
            AnswerLiteralAt::Fenced(next_cursor, lines) => {
                literals.extend_fenced(lines);
                cursor = next_cursor;
            }
            AnswerLiteralAt::Unterminated => break,
            AnswerLiteralAt::None => cursor += utf8_char_len(bytes[cursor]),
        }
    }

    literals.into_parts()
}

#[derive(Default)]
struct AnswerLiterals {
    inline: Vec<String>,
    fenced_lines: Vec<String>,
    seen_inline: HashSet<String>,
    seen_fenced: HashSet<String>,
}

impl AnswerLiterals {
    fn push_inline(&mut self, literal: String) {
        if !literal.is_empty() && self.seen_inline.insert(literal.clone()) {
            self.inline.push(literal);
        }
    }

    fn extend_fenced(&mut self, lines: Vec<String>) {
        for line in lines {
            if self.seen_fenced.insert(line.clone()) {
                self.fenced_lines.push(line);
            }
        }
    }

    fn into_parts(self) -> (Vec<String>, Vec<String>) {
        (self.inline, self.fenced_lines)
    }
}

enum AnswerLiteralAt {
    Inline(usize, String),
    Fenced(usize, Vec<String>),
    Unterminated,
    None,
}

fn answer_literal_at(answer: &str, cursor: usize) -> AnswerLiteralAt {
    let bytes = answer.as_bytes();
    if bytes[cursor..].starts_with(b"```") {
        return fenced_answer_literal_at(answer, cursor);
    }
    if bytes[cursor] == b'`' {
        return inline_answer_literal_at(answer, cursor);
    }
    AnswerLiteralAt::None
}

fn fenced_answer_literal_at(answer: &str, cursor: usize) -> AnswerLiteralAt {
    let body_start = cursor + 3;
    let Some(relative) = find_subslice(&answer.as_bytes()[body_start..], b"```") else {
        return AnswerLiteralAt::Unterminated;
    };
    let body_end = body_start + relative;
    AnswerLiteralAt::Fenced(body_end + 3, fenced_block_content_lines(&answer[body_start..body_end]))
}

fn inline_answer_literal_at(answer: &str, cursor: usize) -> AnswerLiteralAt {
    let literal_start = cursor + 1;
    let Some(relative) = find_byte(&answer.as_bytes()[literal_start..], b'`') else {
        return AnswerLiteralAt::Unterminated;
    };
    let literal_end = literal_start + relative;
    AnswerLiteralAt::Inline(literal_end + 1, answer[literal_start..literal_end].trim().to_string())
}

pub(super) fn extract_formal_exact_claims(answer: &str) -> Vec<FormalExactClaim> {
    let mut claims = Vec::new();
    let mut seen = HashSet::new();
    for line in answer.lines() {
        for token in strip_markdown_ordered_list_marker(line).split_whitespace() {
            for claim in formal_exact_claim_candidates(token) {
                if seen.insert(normalize_verification_literal(claim.literal())) {
                    claims.push(claim);
                }
            }
        }
    }
    claims
}

fn strip_markdown_ordered_list_marker(line: &str) -> &str {
    let trimmed = line.trim_start();
    let digit_bytes = trimmed.bytes().take_while(u8::is_ascii_digit).count();
    if digit_bytes == 0 {
        return line;
    }
    let remainder = &trimmed[digit_bytes..];
    let Some(body) = remainder.strip_prefix('.').or_else(|| remainder.strip_prefix(')')) else {
        return line;
    };
    if body.is_empty() {
        return body;
    }
    if !body.chars().next().is_some_and(char::is_whitespace) {
        return line;
    }
    body.trim_start()
}

fn formal_exact_claim_candidates(token: &str) -> Vec<FormalExactClaim> {
    let mut candidates = Vec::new();
    let bytes = token.as_bytes();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        let Some(ch) = token[cursor..].chars().next() else {
            break;
        };
        if let Some((next_cursor, claim)) = prefixed_version_claim_at(token, cursor, ch) {
            candidates.push(claim);
            cursor = next_cursor;
            continue;
        }
        if let Some((next_cursor, claim)) = numeric_claim_at(token, bytes, cursor, ch) {
            candidates.push(claim);
            cursor = next_cursor;
            continue;
        }
        cursor += ch.len_utf8();
    }
    candidates
}

fn prefixed_version_claim_at(
    token: &str,
    cursor: usize,
    ch: char,
) -> Option<(usize, FormalExactClaim)> {
    if !matches!(ch, 'v' | 'V') {
        return None;
    }
    let literal = prefixed_semver_like_literal_at(token, cursor)?;
    (literal.len() <= MAX_FORMAL_EXACT_LITERAL_BYTES).then(|| {
        (
            cursor + literal.len(),
            FormalExactClaim::new(literal, FormalExactClaimKind::PrefixedVersion),
        )
    })
}

fn numeric_claim_at(
    token: &str,
    bytes: &[u8],
    cursor: usize,
    ch: char,
) -> Option<(usize, FormalExactClaim)> {
    if !numeric_literal_starts_at(token, bytes, cursor, ch) {
        return None;
    }

    let end = numeric_literal_end(bytes, cursor + ch.len_utf8());
    let candidate = numeric_claim_candidate(token, cursor, end)?;
    let kind = numeric_claim_kind(candidate)?;
    Some((end, FormalExactClaim::new(candidate, kind)))
}

fn numeric_literal_end(bytes: &[u8], mut cursor: usize) -> usize {
    while cursor < bytes.len()
        && (bytes[cursor].is_ascii_digit()
            || matches!(bytes[cursor], b'.' | b':' | b'/' | b'-' | b'+' | b'%'))
    {
        cursor += 1;
    }
    cursor
}

fn numeric_claim_candidate(token: &str, start: usize, end: usize) -> Option<&str> {
    (!token[end..].chars().next().is_some_and(is_claim_identifier_char))
        .then(|| token[start..end].trim_end_matches(['.', ':', '/', '-', '+']))
}

fn numeric_claim_kind(candidate: &str) -> Option<FormalExactClaimKind> {
    if !is_exact_numeric_literal(candidate) {
        return None;
    }
    Some(if is_exact_iso_date(candidate) {
        FormalExactClaimKind::IsoDate
    } else {
        FormalExactClaimKind::Numeric
    })
}

fn numeric_literal_starts_at(token: &str, bytes: &[u8], start: usize, ch: char) -> bool {
    let before = token[..start].chars().next_back();
    if matches!(ch, '+' | '-') {
        return bytes.get(start + 1).is_some_and(u8::is_ascii_digit)
            && before.is_none_or(|candidate| !is_claim_identifier_char(candidate));
    }
    if !ch.is_ascii_digit() {
        return false;
    }
    match before {
        None => true,
        Some(candidate) if is_claim_identifier_char(candidate) => false,
        Some('.' | '/' | '%' | '+' | '-') => false,
        Some(':') => !token[..start - 1].chars().next_back().is_some_and(|candidate| {
            candidate.is_ascii_digit() || matches!(candidate, '.' | ':' | '/' | '-' | '+')
        }),
        Some(_) => true,
    }
}

fn has_claim_boundary_after(value: &str, end: usize) -> bool {
    value[end..].chars().next().is_none_or(|ch| !is_claim_identifier_char(ch))
}

fn is_claim_identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn valid_iso_date_end(value: &str, bytes: &[u8], start: usize) -> Option<usize> {
    const ISO_DATE_LEN: usize = 10;
    let end = start.checked_add(ISO_DATE_LEN)?;
    let date = bytes.get(start..end)?;
    if date[4] != b'-'
        || date[7] != b'-'
        || !date
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
        || !has_claim_boundary_after(value, end)
    {
        return None;
    }

    let year = decimal_component(&date[0..4]);
    let month = decimal_component(&date[5..7]);
    let day = decimal_component(&date[8..10]);
    is_valid_calendar_date(year, month, day).then_some(end)
}

fn decimal_component(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |value, byte| value * 10 + u32::from(byte - b'0'))
}

fn is_valid_calendar_date(year: u32, month: u32, day: u32) -> bool {
    if !(1..=12).contains(&month) {
        return false;
    }
    let leap_year =
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days_in_month = match month {
        2 if leap_year => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    (1..=days_in_month).contains(&day)
}

fn is_exact_iso_date(candidate: &str) -> bool {
    valid_iso_date_end(candidate, candidate.as_bytes(), 0) == Some(candidate.len())
}

pub(super) fn is_exact_numeric_literal(candidate: &str) -> bool {
    let digit_count = candidate.bytes().filter(u8::is_ascii_digit).count();
    digit_count >= 1
        && candidate.len() <= MAX_FORMAL_EXACT_LITERAL_BYTES
        && candidate.bytes().all(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'.' | b':' | b'/' | b'-' | b'+' | b'%')
        })
}

fn fenced_block_content_lines(body: &str) -> Vec<String> {
    let content = if let Some(content) = body.strip_prefix("\r\n") {
        content
    } else if let Some(content) = body.strip_prefix('\n') {
        content
    } else {
        body.split_once('\n').map(|(_, content)| content).unwrap_or_default()
    };
    content
        .split('\n')
        .map(|line| line.trim_end_matches('\r').trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn find_byte(haystack: &[u8], needle: u8) -> Option<usize> {
    haystack.iter().position(|byte| *byte == needle)
}

fn utf8_char_len(first_byte: u8) -> usize {
    match first_byte {
        b if b < 0x80 => 1,
        b if b & 0xE0 == 0xC0 => 2,
        b if b & 0xF0 == 0xE0 => 3,
        b if b & 0xF8 == 0xF0 => 4,
        _ => 1,
    }
}

pub(super) fn normalize_boundary_verification_text(value: &str) -> String {
    let mut normalized = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&'
            && let Some(decoded) = decode_html_entity(&mut chars)
        {
            normalized.extend(decoded.to_lowercase());
            continue;
        }
        if ch.is_whitespace() {
            if !normalized.ends_with(' ') {
                normalized.push(' ');
            }
            continue;
        }
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
            && is_markdown_escaped_literal_punctuation(next)
            && let Some(escaped) = chars.next()
        {
            normalized.extend(escaped.to_lowercase());
            continue;
        }
        normalized.extend(ch.to_lowercase());
    }
    normalized.trim().to_string()
}

pub(super) fn normalize_verification_literal(value: &str) -> String {
    let mut normalized = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '&'
            && let Some(decoded) = decode_html_entity(&mut chars)
        {
            normalized.extend(decoded.to_lowercase());
            continue;
        }
        if ch.is_whitespace() {
            continue;
        }
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
            && is_markdown_escaped_literal_punctuation(next)
            && let Some(escaped) = chars.next()
        {
            normalized.extend(escaped.to_lowercase());
            continue;
        }
        normalized.extend(ch.to_lowercase());
    }
    normalized
}

fn decode_html_entity<I>(chars: &mut std::iter::Peekable<I>) -> Option<char>
where
    I: Iterator<Item = char> + Clone,
{
    let mut entity = String::new();
    let probe = chars.clone();
    let mut saw_semicolon = false;
    for next in probe {
        if next == ';' {
            saw_semicolon = true;
            break;
        }
        if entity.len() >= 16 || next.is_whitespace() {
            return None;
        }
        entity.push(next);
    }
    if !saw_semicolon {
        return None;
    }
    let decoded = match entity.as_str() {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" | "#39" => '\'',
        value if value.starts_with("#x") || value.starts_with("#X") => {
            let codepoint = u32::from_str_radix(&value[2..], 16).ok()?;
            char::from_u32(codepoint)?
        }
        value if value.starts_with('#') => {
            let codepoint = value[1..].parse::<u32>().ok()?;
            char::from_u32(codepoint)?
        }
        _ => return None,
    };
    for _ in 0..entity.chars().count() {
        chars.next();
    }
    chars.next();
    Some(decoded)
}

fn is_markdown_escaped_literal_punctuation(ch: char) -> bool {
    ch.is_ascii_punctuation() && ch != '/' && ch != '\\'
}

#[cfg(test)]
mod tests {
    use super::{
        FormalExactClaim, FormalExactClaimKind, extract_answer_literals,
        extract_formal_exact_claims,
    };

    #[test]
    fn exact_claim_extraction_is_structural_and_typed() {
        assert_eq!(
            extract_formal_exact_claims("value=1 port=9090 date=2029-04-05 build=v2.4.1"),
            [
                FormalExactClaim::new("1", FormalExactClaimKind::Numeric),
                FormalExactClaim::new("9090", FormalExactClaimKind::Numeric),
                FormalExactClaim::new("2029-04-05", FormalExactClaimKind::IsoDate),
                FormalExactClaim::new("v2.4.1", FormalExactClaimKind::PrefixedVersion),
            ]
        );
    }

    #[test]
    fn prefixed_version_accepts_sentence_punctuation_but_not_longer_identifiers() {
        assert_eq!(
            extract_formal_exact_claims("The selected version is v2.4."),
            [FormalExactClaim::new("v2.4", FormalExactClaimKind::PrefixedVersion)]
        );
        assert_eq!(
            extract_formal_exact_claims("build=v2.4-beta metadata=v2.4+build.7"),
            [
                FormalExactClaim::new("v2.4-beta", FormalExactClaimKind::PrefixedVersion),
                FormalExactClaim::new("v2.4+build.7", FormalExactClaimKind::PrefixedVersion),
            ]
        );
        assert!(extract_formal_exact_claims("v2.4.alpha").is_empty());
        assert!(extract_formal_exact_claims("v2.4.-beta").is_empty());
        assert!(extract_formal_exact_claims("v2.4_candidate").is_empty());
        assert!(extract_formal_exact_claims("v2.4.,").is_empty());
    }

    #[test]
    fn exact_claim_extraction_ignores_order_markers_and_identifier_fragments() {
        assert_eq!(
            extract_formal_exact_claims(
                "15. First neutral step.\n16) Second neutral step with value=9090."
            ),
            [FormalExactClaim::new("9090", FormalExactClaimKind::Numeric)]
        );
        assert!(extract_formal_exact_claims("item9090 release2029alpha 2029λ").is_empty());
        assert!(extract_formal_exact_claims("build=v2.4.-beta").is_empty());
    }

    #[test]
    fn exact_claim_extraction_does_not_classify_prose_by_case() {
        assert!(extract_formal_exact_claims("ExampleOwner ALPHA_STATUS BetaNode").is_empty());
    }

    #[test]
    fn unfenced_info_line_is_not_dropped_as_a_language_hint() {
        let (inline, fenced) = extract_answer_literals("```\nNODE_9\n```");

        assert!(inline.is_empty());
        assert_eq!(fenced, ["NODE_9"]);
    }

    #[test]
    fn explicit_fence_info_string_is_not_treated_as_content() {
        let (inline, fenced) = extract_answer_literals("```text\nNODE_9\n```");

        assert!(inline.is_empty());
        assert_eq!(fenced, ["NODE_9"]);
    }
}
