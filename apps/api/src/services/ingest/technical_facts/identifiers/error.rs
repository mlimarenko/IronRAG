use std::collections::BTreeSet;

use super::super::{
    FactCandidate, StructuredBlockData, TechnicalFactKind, build_candidate, matches_any_substring,
    technical_tokens, trim_technical_token,
};

pub(crate) fn extract_error_code_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let lower = line.to_ascii_lowercase();
    if !matches_any_substring(&lower, &["error", "code", "exception", "ошибк"]) {
        return Vec::new();
    }

    let mut codes = BTreeSet::<String>::new();
    for token in technical_tokens(line) {
        let candidate = trim_technical_token(&token);

        if candidate.starts_with('E')
            && candidate.len() >= 4
            && candidate.len() <= 6
            && candidate[1..].chars().all(|ch| ch.is_ascii_digit())
        {
            codes.insert(candidate.to_string());
            continue;
        }

        if (candidate.starts_with("ERR_") || candidate.starts_with("ERROR_"))
            && candidate.len() > 4
            && candidate
                .chars()
                .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit() || ch == '_')
        {
            codes.insert(candidate.to_string());
        }
    }

    codes
        .into_iter()
        .filter_map(|code| {
            build_candidate(
                block,
                TechnicalFactKind::ErrorCode,
                &code,
                Vec::new(),
                line,
                "error_code",
            )
        })
        .collect()
}
