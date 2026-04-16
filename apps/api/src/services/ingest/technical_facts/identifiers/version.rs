use std::collections::BTreeSet;

use super::super::{
    FactCandidate, StructuredBlockData, TechnicalFactKind, build_candidate, matches_any_substring,
    technical_tokens, trim_technical_token,
};

pub(crate) fn extract_version_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let lower = line.to_ascii_lowercase();
    let has_version_context = matches_any_substring(&lower, &["version", "release", " v.", " v "]);

    let mut versions = BTreeSet::<String>::new();
    let tokens = technical_tokens(line);

    for token in &tokens {
        let candidate = trim_technical_token(token);

        if let Some(rest) = candidate.strip_prefix('v').or_else(|| candidate.strip_prefix('V'))
            && is_semver_like(rest)
        {
            versions.insert(candidate.to_string());
        }

        if has_version_context && is_semver_like(candidate) {
            versions.insert(candidate.to_string());
        }

        if has_version_context && is_date_version(candidate) {
            versions.insert(candidate.to_string());
        }
    }

    versions
        .into_iter()
        .filter_map(|version| {
            build_candidate(
                block,
                TechnicalFactKind::VersionNumber,
                &version,
                Vec::new(),
                line,
                "version_number",
            )
        })
        .collect()
}

fn is_semver_like(candidate: &str) -> bool {
    if semver::Version::parse(candidate).is_ok() {
        return true;
    }
    let core = candidate.split(['-', '+']).next().unwrap_or(candidate);
    let segs: Vec<&str> = core.split('.').collect();
    segs.len() == 2
        && segs.iter().all(|seg| !seg.is_empty() && seg.chars().all(|ch| ch.is_ascii_digit()))
}

fn is_date_version(candidate: &str) -> bool {
    let segments: Vec<&str> = candidate.split('.').collect();
    if segments.len() < 2 || segments.len() > 3 {
        return false;
    }
    let Some(year) = segments[0].parse::<u32>().ok() else {
        return false;
    };
    if !(2000..=2099).contains(&year) {
        return false;
    }
    segments[1..]
        .iter()
        .all(|seg| !seg.is_empty() && seg.len() <= 2 && seg.chars().all(|ch| ch.is_ascii_digit()))
}
