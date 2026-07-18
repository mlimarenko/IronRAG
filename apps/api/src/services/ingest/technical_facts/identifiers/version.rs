use std::collections::BTreeSet;

use super::super::{
    FactCandidate, StructuredBlockData, TechnicalFactKind, build_candidate,
    is_prefixed_semver_literal, technical_tokens, trim_technical_token,
};

pub(crate) fn extract_version_candidates(
    block: &StructuredBlockData,
    line: &str,
) -> Vec<FactCandidate> {
    let mut versions = BTreeSet::<String>::new();
    for token in technical_tokens(line) {
        let candidate = trim_technical_token(&token);
        if is_prefixed_semver_literal(candidate) {
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
