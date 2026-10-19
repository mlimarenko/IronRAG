use super::{Deserialize, HashSet, Result, anyhow};

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ProviderSemanticScore {
    pub(crate) index: usize,
    pub(crate) score: f64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderSemanticResponse {
    scores: Vec<ProviderSemanticScore>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ValidatedSemanticRanking {
    ordered_indices: Vec<usize>,
}

impl ValidatedSemanticRanking {
    #[must_use]
    pub(crate) fn ordered_indices(&self) -> &[usize] {
        &self.ordered_indices
    }
}

/// Decodes the strict `{"scores":[{"index":..,"score":..}]}` chat envelope into
/// its raw score list, without validating cardinality or range.
///
/// The provider call must be accounted for before a malformed ranking degrades
/// the turn, so the caller parses the envelope first and validates the list
/// with [`validate_semantic_scores`] afterwards. Both the chat and the typed
/// decision branch end in that one validator.
pub(crate) fn parse_semantic_score_list(output_text: &str) -> Result<Vec<ProviderSemanticScore>> {
    let response: ProviderSemanticResponse = serde_json::from_str(output_text)
        .map_err(|error| anyhow!("semantic rerank response is not valid JSON: {error}"))?;
    Ok(response.scores)
}

pub(crate) fn parse_semantic_scores(
    output_text: &str,
    expected_count: usize,
) -> Result<ValidatedSemanticRanking> {
    validate_semantic_scores(parse_semantic_score_list(output_text)?, expected_count)
}

pub(crate) fn validate_semantic_scores(
    scores: Vec<ProviderSemanticScore>,
    expected_count: usize,
) -> Result<ValidatedSemanticRanking> {
    if expected_count == 0 || scores.len() != expected_count {
        return Err(anyhow!(
            "semantic rerank returned {} scores for {expected_count} candidates",
            scores.len()
        ));
    }
    let mut seen = HashSet::with_capacity(expected_count);
    for score in &scores {
        if score.index >= expected_count {
            return Err(anyhow!("semantic rerank index {} is out of range", score.index));
        }
        if !seen.insert(score.index) {
            return Err(anyhow!("semantic rerank index {} is duplicated", score.index));
        }
        if !score.score.is_finite() || !(0.0..=1.0).contains(&score.score) {
            return Err(anyhow!("semantic rerank score for index {} is invalid", score.index));
        }
    }
    let mut ranked = scores;
    ranked.sort_by(|left, right| {
        right.score.total_cmp(&left.score).then_with(|| left.index.cmp(&right.index))
    });
    Ok(ValidatedSemanticRanking {
        ordered_indices: ranked.into_iter().map(|score| score.index).collect(),
    })
}
