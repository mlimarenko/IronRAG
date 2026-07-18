#[derive(Debug, Clone, PartialEq)]
pub struct TextQualityAssessment {
    pub score: f32,
    pub low_confidence: bool,
    pub unstable_token_ratio: f32,
    pub unstable_token_count: usize,
    pub token_count: usize,
    pub reasons: Vec<&'static str>,
}

struct TextQualityMetrics {
    token_count: usize,
    unstable_token_count: usize,
    unstable_token_ratio: f32,
    digit_or_script_noise_count: usize,
    digit_or_script_noise_ratio: f32,
    code_like_context: bool,
    replacement_ratio: f32,
    control_ratio: f32,
}

struct TextQualitySignals {
    dense_unstable_tokens: bool,
    widespread_unstable_tokens: bool,
    dense_digit_or_script_noise: bool,
    dominant_structural_noise: bool,
}

#[must_use]
pub fn assess_text_quality(text: &str) -> TextQualityAssessment {
    let trimmed = text.trim();
    if trimmed.chars().count() < 24 {
        return high_confidence_short_text();
    }

    let metrics = text_quality_metrics(trimmed);
    let signals = text_quality_signals(&metrics);
    let reasons = text_quality_reasons(&metrics, &signals);
    let mut score = text_quality_score_from_metrics(&metrics, &signals);
    let low_confidence = text_quality_is_low_confidence(&metrics, &signals, score);
    if low_confidence {
        score = score.min(0.30);
    }

    TextQualityAssessment {
        score,
        low_confidence,
        unstable_token_ratio: metrics.unstable_token_ratio,
        unstable_token_count: metrics.unstable_token_count,
        token_count: metrics.token_count,
        reasons,
    }
}

fn high_confidence_short_text() -> TextQualityAssessment {
    TextQualityAssessment {
        score: 1.0,
        low_confidence: false,
        unstable_token_ratio: 0.0,
        unstable_token_count: 0,
        token_count: 0,
        reasons: Vec::new(),
    }
}

fn text_quality_metrics(text: &str) -> TextQualityMetrics {
    let char_count = text.chars().count().max(1);
    let replacement_count = text.chars().filter(|&ch| ch == '\u{FFFD}').count();
    let control_count =
        text.chars().filter(|&ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t')).count();
    let raw_tokens = text.split_whitespace().collect::<Vec<_>>();
    let tokens = raw_tokens
        .iter()
        .map(|token| clean_token(token))
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>();
    let token_count = tokens.len();
    let unstable_token_count =
        tokens.iter().filter(|token| token_is_structurally_unstable(token)).count();
    let digit_or_script_noise_count =
        tokens.iter().filter(|token| token_has_digit_or_script_noise(token)).count();

    TextQualityMetrics {
        token_count,
        unstable_token_count,
        unstable_token_ratio: ratio(unstable_token_count, token_count),
        digit_or_script_noise_count,
        digit_or_script_noise_ratio: ratio(digit_or_script_noise_count, token_count),
        code_like_context: has_code_like_context(&raw_tokens, token_count),
        replacement_ratio: replacement_count as f32 / char_count as f32,
        control_ratio: control_count as f32 / char_count as f32,
    }
}

fn ratio(numerator: usize, denominator: usize) -> f32 {
    if denominator == 0 { 0.0 } else { numerator as f32 / denominator as f32 }
}

fn text_quality_signals(metrics: &TextQualityMetrics) -> TextQualitySignals {
    let dense_unstable_tokens = !metrics.code_like_context
        && metrics.unstable_token_count >= 4
        && metrics.unstable_token_ratio >= 0.18;
    let widespread_unstable_tokens = !metrics.code_like_context
        && metrics.unstable_token_count >= 8
        && metrics.unstable_token_ratio >= 0.12;
    let dense_digit_or_script_noise = !metrics.code_like_context
        && metrics.digit_or_script_noise_count >= 3
        && metrics.digit_or_script_noise_ratio >= 0.08;
    let dominant_structural_noise = metrics.unstable_token_count >= 12
        && metrics.unstable_token_ratio >= 0.35
        && (metrics.digit_or_script_noise_count >= 8
            || metrics.digit_or_script_noise_ratio >= 0.12);

    TextQualitySignals {
        dense_unstable_tokens,
        widespread_unstable_tokens,
        dense_digit_or_script_noise,
        dominant_structural_noise,
    }
}

fn text_quality_reasons(
    metrics: &TextQualityMetrics,
    signals: &TextQualitySignals,
) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if metrics.replacement_ratio >= 0.01 {
        reasons.push("replacement_chars");
    }
    if metrics.control_ratio >= 0.01 {
        reasons.push("control_chars");
    }
    if signals.dense_unstable_tokens {
        reasons.push("dense_unstable_tokens");
    } else if signals.widespread_unstable_tokens {
        reasons.push("widespread_unstable_tokens");
    }
    if signals.dense_digit_or_script_noise {
        reasons.push("dense_digit_or_script_noise");
    }
    if signals.dominant_structural_noise {
        reasons.push("dominant_structural_noise");
    }
    reasons
}

fn text_quality_score_from_metrics(
    metrics: &TextQualityMetrics,
    signals: &TextQualitySignals,
) -> f32 {
    let instability_penalty = if signals.dense_unstable_tokens
        || signals.widespread_unstable_tokens
        || signals.dominant_structural_noise
    {
        metrics.unstable_token_ratio * 1.35
    } else {
        metrics.unstable_token_ratio * 0.85
    };
    (1.0 - instability_penalty - (metrics.replacement_ratio * 8.0) - (metrics.control_ratio * 8.0))
        .clamp(0.0, 1.0)
}

fn text_quality_is_low_confidence(
    metrics: &TextQualityMetrics,
    signals: &TextQualitySignals,
    score: f32,
) -> bool {
    metrics.replacement_ratio >= 0.01
        || metrics.control_ratio >= 0.01
        || signals.dense_unstable_tokens
        || signals.dominant_structural_noise
        || signals.dense_digit_or_script_noise
        || (signals.widespread_unstable_tokens && score < 0.75)
}

#[must_use]
pub fn text_quality_score(text: &str) -> f32 {
    assess_text_quality(text).score
}

#[must_use]
pub fn is_low_confidence_text(text: &str) -> bool {
    assess_text_quality(text).low_confidence
}

#[must_use]
pub fn is_graph_extraction_text_eligible(text: &str) -> bool {
    !assess_text_quality(text).low_confidence
}

#[must_use]
pub fn is_structurally_unstable_fragment(text: &str) -> bool {
    let tokens = text
        .split_whitespace()
        .map(clean_token)
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return false;
    }

    let unstable_token_count =
        tokens.iter().filter(|token| token_is_structurally_unstable(token)).count();
    let short_fragment_noise_count =
        tokens.iter().filter(|token| token_is_short_fragment_noise(token)).count();
    let digit_or_script_noise_count =
        tokens.iter().filter(|token| token_has_digit_or_script_noise(token)).count();
    if short_fragment_noise_count > 0 {
        return true;
    }
    if digit_or_script_noise_count > 0 {
        return true;
    }
    if tokens.len() <= 3 {
        return tokens.len() >= 2 && unstable_token_count == tokens.len();
    }

    unstable_token_count >= 4 && unstable_token_count as f32 / tokens.len() as f32 >= 0.30
}

fn clean_token(token: &str) -> &str {
    token.trim_matches(|ch: char| !ch.is_alphanumeric() && !matches!(ch, '_' | '-' | '/' | '.'))
}

fn token_is_structurally_unstable(token: &str) -> bool {
    let token = clean_token(token);
    let chars = token.chars().collect::<Vec<_>>();
    if chars.len() < 4 || token_has_stable_structure(token) {
        return false;
    }

    let profile = TokenStructureProfile::from_chars(&chars);
    if profile.letters < 4 {
        return false;
    }
    token_structure_is_unstable(&chars, profile)
}

#[derive(Clone, Copy)]
struct TokenStructureProfile {
    letters: usize,
    digits: usize,
    uppercase: usize,
    lowercase: usize,
    case_transitions: usize,
}

impl TokenStructureProfile {
    fn from_chars(chars: &[char]) -> Self {
        Self {
            letters: chars.iter().filter(|ch| ch.is_alphabetic()).count(),
            digits: chars.iter().filter(|ch| ch.is_ascii_digit()).count(),
            uppercase: chars.iter().filter(|ch| ch.is_uppercase()).count(),
            lowercase: chars.iter().filter(|ch| ch.is_lowercase()).count(),
            case_transitions: case_transition_count(chars),
        }
    }

    fn is_mixed_case(self) -> bool {
        self.uppercase > 0 && self.lowercase > 0
    }
}

fn token_structure_is_unstable(chars: &[char], profile: TokenStructureProfile) -> bool {
    turbulent_token_case(chars, profile)
        || compact_token_case_noise(chars, profile)
        || embedded_digit_word(chars, profile)
        || script_switch_count(chars) >= 2
        || uppercase_digit_noise(profile)
}

fn turbulent_token_case(chars: &[char], profile: TokenStructureProfile) -> bool {
    profile.is_mixed_case()
        && profile.case_transitions >= 3
        && (max_internal_uppercase_run(chars) >= 2 || profile.digits > 0)
}

fn compact_token_case_noise(chars: &[char], profile: TokenStructureProfile) -> bool {
    chars.len() <= 5
        && profile.is_mixed_case()
        && profile.uppercase >= 2
        && profile.lowercase >= 2
        && profile.case_transitions >= 3
}

fn embedded_digit_word(chars: &[char], profile: TokenStructureProfile) -> bool {
    profile.digits > 0
        && profile.letters >= 4
        && (digit_letter_switch_count(chars) >= 1
            || (profile.uppercase >= 2 && profile.lowercase >= 2))
}

fn uppercase_digit_noise(profile: TokenStructureProfile) -> bool {
    profile.digits > 0 && profile.lowercase == 0 && profile.uppercase >= 5
}

fn token_has_stable_structure(token: &str) -> bool {
    token.contains("://") || token.matches('/').count() >= 2 || token.matches('_').count() >= 2
}

fn case_transition_count(chars: &[char]) -> usize {
    chars
        .windows(2)
        .filter(|pair| {
            let left = pair[0];
            let right = pair[1];
            (left.is_lowercase() && right.is_uppercase())
                || (left.is_uppercase() && right.is_lowercase())
        })
        .count()
}

fn digit_letter_switch_count(chars: &[char]) -> usize {
    chars
        .windows(2)
        .filter(|pair| {
            let left_is_digit = pair[0].is_ascii_digit();
            let right_is_digit = pair[1].is_ascii_digit();
            left_is_digit != right_is_digit && (pair[0].is_alphabetic() || pair[1].is_alphabetic())
        })
        .count()
}

fn script_switch_count(chars: &[char]) -> usize {
    chars
        .windows(2)
        .filter_map(|pair| Some((letter_script_class(pair[0])?, letter_script_class(pair[1])?)))
        .filter(|(left, right)| left != right)
        .count()
}

fn token_is_short_fragment_noise(token: &str) -> bool {
    let token = clean_token(token);
    let chars = token.chars().collect::<Vec<_>>();
    if chars.len() < 2 || chars.len() > 8 || token_has_strong_code_context_marker(token) {
        return false;
    }

    let letters = chars.iter().filter(|ch| ch.is_alphabetic()).count();
    if letters < 2 {
        return false;
    }

    let uppercase = chars.iter().filter(|ch| ch.is_uppercase()).count();
    let lowercase = chars.iter().filter(|ch| ch.is_lowercase()).count();
    let case_transitions = chars
        .windows(2)
        .filter(|pair| {
            let left = pair[0];
            let right = pair[1];
            (left.is_lowercase() && right.is_uppercase())
                || (left.is_uppercase() && right.is_lowercase())
        })
        .count();
    let script_switches = chars
        .windows(2)
        .filter_map(|pair| Some((letter_script_class(pair[0])?, letter_script_class(pair[1])?)))
        .filter(|(left, right)| left != right)
        .count();

    script_switches >= 1 || (uppercase >= 2 && lowercase >= 2 && case_transitions >= 3)
}

fn token_has_digit_or_script_noise(token: &str) -> bool {
    let token = clean_token(token);
    let chars = token.chars().collect::<Vec<_>>();
    if chars.len() < 4 {
        return false;
    }
    let letters = chars.iter().filter(|ch| ch.is_alphabetic()).count();
    if letters < 3 {
        return false;
    }
    let digits = chars.iter().filter(|ch| ch.is_ascii_digit()).count();
    let digit_letter_switches = chars
        .windows(2)
        .filter(|pair| {
            let left_is_digit = pair[0].is_ascii_digit();
            let right_is_digit = pair[1].is_ascii_digit();
            left_is_digit != right_is_digit && (pair[0].is_alphabetic() || pair[1].is_alphabetic())
        })
        .count();
    let script_switches = chars
        .windows(2)
        .filter_map(|pair| Some((letter_script_class(pair[0])?, letter_script_class(pair[1])?)))
        .filter(|(left, right)| left != right)
        .count();

    (digits > 0 && digit_letter_switches > 0) || script_switches >= 2
}

fn has_code_like_context(raw_tokens: &[&str], token_count: usize) -> bool {
    if token_count == 0 {
        return false;
    }
    let marker_count =
        raw_tokens.iter().filter(|token| token_has_code_context_marker(token)).count();
    let strong_marker_count =
        raw_tokens.iter().filter(|token| token_has_strong_code_context_marker(token)).count();
    let marker_ratio = marker_count as f32 / token_count as f32;

    (strong_marker_count >= 2 && marker_ratio >= 0.10)
        || (strong_marker_count >= 1 && marker_count >= 5 && marker_ratio >= 0.18)
        || (marker_count >= 8 && marker_ratio >= 0.25)
}

fn token_has_code_context_marker(token: &str) -> bool {
    let token = trim_code_context_punctuation(token);
    token_has_strong_code_context_marker(token)
        || token.contains('<')
        || token.contains('>')
        || token_has_internal_dot(token)
}

fn token_has_strong_code_context_marker(token: &str) -> bool {
    let token = trim_code_context_punctuation(token);
    token.contains("://")
        || token.matches('/').count() >= 1
        || token.matches('_').count() >= 1
        || token.contains("::")
        || token.contains("->")
        || token.contains("=>")
        || token.contains('=')
        || token.contains('(')
        || token.contains(')')
        || token.contains('{')
        || token.contains('}')
        || token.contains('[')
        || token.contains(']')
}

fn trim_code_context_punctuation(token: &str) -> &str {
    token.trim_matches(|ch: char| matches!(ch, ',' | ';' | ':' | '.' | '?' | '!'))
}

fn token_has_internal_dot(token: &str) -> bool {
    let chars = token.chars().collect::<Vec<_>>();
    chars.windows(3).any(|window| {
        window[1] == '.'
            && (window[0].is_alphanumeric() || window[0] == '_')
            && (window[2].is_alphanumeric() || window[2] == '_')
    })
}

fn max_internal_uppercase_run(chars: &[char]) -> usize {
    let mut max_run = 0_usize;
    let mut current_run = 0_usize;
    let mut run_start = 0_usize;

    for (index, ch) in chars.iter().enumerate() {
        if ch.is_uppercase() {
            if current_run == 0 {
                run_start = index;
            }
            current_run += 1;
            continue;
        }

        if current_run > 0 && run_start > 0 {
            max_run = max_run.max(current_run);
        }
        current_run = 0;
    }

    if current_run > 0 && run_start > 0 {
        max_run = max_run.max(current_run);
    }

    max_run
}

fn letter_script_class(ch: char) -> Option<u8> {
    if !ch.is_alphabetic() {
        return None;
    }
    let code = ch as u32;
    Some(match code {
        0x0000..=0x024F => 1,
        0x0370..=0x03FF | 0x1F00..=0x1FFF => 2,
        0x0400..=0x052F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => 3,
        0x0590..=0x05FF => 4,
        0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF => 5,
        0x0900..=0x097F => 6,
        0x0E00..=0x0E7F => 7,
        0x3040..=0x30FF => 8,
        0x3400..=0x9FFF | 0xF900..=0xFAFF => 9,
        0xAC00..=0xD7AF | 0x1100..=0x11FF => 10,
        _ => 255,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        assess_text_quality, is_graph_extraction_text_eligible, is_low_confidence_text,
        is_structurally_unstable_fragment,
    };

    #[test]
    fn detects_structurally_unstable_ocr_tokens() {
        let text = "aBcD3eFgH qWeR7tYuI zXcV9bNmP lMnO4pQrS tUvW6xYzA";

        let assessment = assess_text_quality(text);

        assert!(assessment.low_confidence);
        assert!(assessment.score <= 0.30);
        assert!(assessment.unstable_token_count >= 4);
    }

    #[test]
    fn treats_dense_structural_noise_as_low_confidence_even_with_context() {
        let text = concat!(
            "overview status project service alpha beta gamma delta ",
            "abCDEfGHij klMNOqRStu uvWXYzABcd efGHIjKLmn",
        );

        let assessment = assess_text_quality(text);

        assert!(assessment.low_confidence);
        assert!(assessment.score <= 0.30);
    }

    #[test]
    fn detects_ocr_like_case_digit_noise_in_regular_text_context() {
        let text = concat!(
            "summary topic alpha beta gamma ",
            "abCD4efGH hiJKlmNO pQrST uvWXyZab ",
            "cdEFGh3Ij klMNOprs tuVWxyZq mnOPqRst",
        );

        let assessment = assess_text_quality(text);

        assert!(assessment.low_confidence);
        assert!(assessment.score <= 0.30);
        assert!(assessment.unstable_token_count >= 6);
    }

    #[test]
    fn punctuation_and_markup_do_not_hide_ocr_noise_from_graph_policy() {
        let text = concat!(
            "overview status alpha beta gamma. section summary. ",
            "<!-- formula-not-decoded --> ",
            "aBcD3eFgH qWeR7tYuI zXcV9bNmP lMnO4pQrS tUvW6xYzA ",
            "abCD4efGH hiJK5lmNO pQrST6uv wxYZ7abC."
        );

        let assessment = assess_text_quality(text);

        assert!(assessment.low_confidence);
        assert!(!is_graph_extraction_text_eligible(text));
    }

    #[test]
    fn dominant_noise_overrides_pseudo_code_markers_for_graph_policy() {
        let text = concat!(
            "GET /v1/items status_code config.value key=value item[index] ",
            "GET /v1/items status_code config.value key=value item[index] ",
            "aBcD3eFgH qWeR7tYuI zXcV9bNmP lMnO4pQrS tUvW6xYzA ",
            "abCD4efGH hiJK5lmNO pQrST6uv wxYZ7abC klM8nOPq ",
            "qrST9uvW xyZA1bcD efGH2ijK lmNO3pQr stUV4wXy"
        );

        let assessment = assess_text_quality(text);

        assert!(assessment.low_confidence);
        assert!(assessment.reasons.contains(&"dominant_structural_noise"));
        assert!(!is_graph_extraction_text_eligible(text));
    }

    #[test]
    fn accepts_ordinary_prose_across_writing_systems() {
        assert!(!is_low_confidence_text(
            "Alpha service stores project settings and renders a concise operational summary."
        ));
        assert!(!is_low_confidence_text(
            "El servicio almacena la configuración del proyecto y genera un resumen operativo."
        ));
    }

    #[test]
    fn accepts_common_code_and_config_identifiers() {
        let text = concat!(
            "POST /api/v1/projects getUserById setProjectOwner renderHTMLNode ",
            "parseHTTPResponse AUTH_TOKEN_TIMEOUT_MS status_code",
        );

        assert!(!is_low_confidence_text(text));
    }

    #[test]
    fn fragment_check_rejects_ocr_like_tokens_without_rejecting_camel_case() {
        assert!(is_structurally_unstable_fragment("qWeR7tYuI"));
        assert!(is_structurally_unstable_fragment("abCDEfGHij klMNOqRStu"));
        assert!(is_structurally_unstable_fragment("7.3abCDefGH"));
        assert!(is_structurally_unstable_fragment("CTpoKe"));
        assert!(is_structurally_unstable_fragment("Enμα"));
        assert!(is_structurally_unstable_fragment("∑nμα"));
        assert!(is_structurally_unstable_fragment("μe"));
        assert!(!is_structurally_unstable_fragment("getUserById"));
        assert!(!is_structurally_unstable_fragment("renderHTMLNode"));
        assert!(!is_structurally_unstable_fragment("parseHTTPResponse"));
        assert!(!is_structurally_unstable_fragment("NODE_ALPHA-42"));
        assert!(!is_structurally_unstable_fragment("ALPHA_TIMEOUT_MS=4500"));
    }
}
