//! Request and response validation for the SystemOne decision protocol.
//!
//! All rules are pure functions over the decoded wire types so the client's
//! async path stays a thin transport wrapper and every rule is testable
//! without a socket.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::integrations::retry::ProviderCallError;

use super::protocol::{
    SystemOneAnswer, SystemOneDecisionRequest, SystemOneDecisionResponse, SystemOneQuestion,
    SystemOneUsage,
};

/// Maximum number of questions accepted in one decision request.
///
/// This is a local client-side bound, not a protocol limit: the API reference
/// (<https://docs.typesafe.ai/api.md>) documents no cap on the `questions` map,
/// so this leaf refuses an unbounded request of its own accord.
pub const MAX_QUESTIONS: usize = 64;
/// Maximum number of characters accepted in one question id.
///
/// Local bound for the same reason as [`MAX_QUESTIONS`]; the API reference
/// documents no id length.
pub const MAX_QUESTION_ID_CHARS: usize = 64;
/// Decimal quantum of every number SystemOne publishes.
///
/// The Score examples (<https://docs.typesafe.ai/primitives/score.md>) and live
/// responses report the score, every probability and the confidence rounded to
/// two decimal places. Each value is rounded independently, so a published
/// distribution need not sum to exactly one and a published score need not
/// equal the expectation recomputed from published probabilities. Every
/// tolerance below is derived from this quantum, never chosen ad hoc.
pub const PUBLISHED_QUANTUM: f64 = 0.01;
/// Slack for float summation error on top of the rounding bounds.
const FLOAT_SUMMATION_SLACK: f64 = 1e-9;

/// Tolerance for a published distribution over `count` options to sum to one.
///
/// Each probability carries at most half a quantum of rounding error, so the
/// sum of `count` of them drifts by at most `count * quantum / 2`.
#[must_use]
pub fn probability_sum_tolerance(count: usize) -> f64 {
    count as f64 * PUBLISHED_QUANTUM / 2.0 + FLOAT_SUMMATION_SLACK
}

/// Tolerance for a published `score` against `Σ index * probabilities[index]`
/// over `level_count` levels.
///
/// The score itself carries half a quantum of rounding error, and the
/// recomputed expectation carries half a quantum per unit of index weight,
/// `Σ index = level_count * (level_count - 1) / 2`.
#[must_use]
pub fn score_expectation_tolerance(level_count: usize) -> f64 {
    let index_weight = level_count.saturating_mul(level_count.saturating_sub(1)) / 2;
    PUBLISHED_QUANTUM / 2.0 * (1.0 + index_weight as f64) + FLOAT_SUMMATION_SLACK
}
/// Maximum number of options accepted in one `choice` question. Documented by
/// the protocol: "You can have a maximum of 255 options per Choice"
/// (<https://docs.typesafe.ai/api.md>).
const MAX_CHOICE_OPTIONS: usize = 255;
/// Minimum number of ordered levels accepted in one `score` question. Documented
/// by the protocol: "A Score should have at least two levels"
/// (<https://docs.typesafe.ai/api.md>).
pub const MIN_SCORE_LEVELS: usize = 2;
/// Maximum number of ordered levels accepted in one `score` question. Documented
/// by the protocol: "the API accepts up to 10"
/// (<https://docs.typesafe.ai/api.md>).
pub const MAX_SCORE_LEVELS: usize = 10;

/// Terminal validation failure of a request or response contract.
#[derive(Debug, Error)]
pub enum SystemOneError {
    #[error("typesafe systemone request is invalid: {0}")]
    Request(#[from] SystemOneRequestError),
    #[error("typesafe systemone response is invalid: {source}")]
    Response {
        #[source]
        source: SystemOneResponseError,
        /// Usage the server still reported. A decodable 200 response that fails
        /// decision validation was paid for, so a consumer settles it before
        /// degrading; `None` means the body did not decode and no usage exists.
        usage: Option<SystemOneUsage>,
    },
    #[error("typesafe systemone provider call failed: {source}")]
    Provider {
        #[source]
        source: ProviderCallError,
    },
    #[error("typesafe systemone client configuration is invalid: {0}")]
    Config(#[from] SystemOneConfigError),
}

impl SystemOneError {
    /// Value-free classification for operator-facing redacted storage. Provider
    /// failures keep the shared redaction; local validation failures collapse
    /// to a bounded static label so no raw body or id is ever interpolated.
    #[must_use]
    pub fn redacted_detail(&self) -> String {
        match self {
            Self::Request(_) => "request_validation".to_string(),
            Self::Response { .. } => "response_validation".to_string(),
            Self::Config(_) => "config_validation".to_string(),
            Self::Provider { source } => source.redacted_detail(),
        }
    }
}

impl From<ProviderCallError> for SystemOneError {
    fn from(source: ProviderCallError) -> Self {
        Self::Provider { source }
    }
}

impl From<serde_json::Error> for SystemOneResponseError {
    fn from(error: serde_json::Error) -> Self {
        Self::Decode(error.to_string())
    }
}

/// A client construction input that cannot be used safely.
///
/// The variant carries no value: a rejected credential must never be echoed into
/// an error, log or redacted diagnostics column.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SystemOneConfigError {
    #[error("typesafe systemone api key must not be empty or whitespace")]
    EmptyApiKey,
}

/// A request that cannot be sent as a valid SystemOne decision request.
#[derive(Debug, Error, PartialEq)]
pub enum SystemOneRequestError {
    #[error("systemone request model must not be empty")]
    EmptyModel,
    #[error("systemone request state must not be empty")]
    EmptyState,
    #[error("systemone request must carry at least one question")]
    EmptyQuestions,
    #[error("systemone request carries {count} questions, above the {max} limit")]
    TooManyQuestions { count: usize, max: usize },
    #[error("systemone question id {id:?} is empty or longer than the allowed limit")]
    QuestionIdInvalid { id: String },
    #[error("systemone choice question {id:?} must carry at least one option")]
    ChoiceCriteriaEmpty { id: String },
    #[error("systemone choice question {id:?} carries {count} options, above the 255 limit")]
    TooManyChoiceOptions { id: String, count: usize },
    #[error("systemone score question {id:?} carries {count} levels, outside the 2..=10 range")]
    ScoreCriteriaOutOfRange { id: String, count: usize },
}

/// A response that does not satisfy the SystemOne decision contract.
#[derive(Debug, Error, PartialEq)]
pub enum SystemOneResponseError {
    #[error("systemone response body could not be decoded: {0}")]
    Decode(String),
    #[error("systemone response is missing an answer for question {id:?}")]
    MissingAnswer { id: String },
    #[error("systemone response carries an answer for unknown question {id:?}")]
    SurplusAnswer { id: String },
    #[error("systemone answer {id:?} has type {actual:?}, expected {expected:?}")]
    AnswerTypeMismatch { id: String, expected: &'static str, actual: &'static str },
    #[error("systemone noul answer {id:?} is outside [0, 1]")]
    NoulOutOfRange { id: String, value: f64 },
    #[error("systemone noul answer {id:?} is not finite")]
    NoulNotFinite { id: String },
    #[error("systemone confidence for answer {id:?} is outside [0, 1]")]
    ConfidenceOutOfRange { id: String, value: f64 },
    #[error("systemone confidence for answer {id:?} is not finite")]
    ConfidenceNotFinite { id: String },
    #[error("systemone choice probabilities for answer {id:?} do not match option {option:?}")]
    ChoiceProbabilityOptionMismatch { id: String, option: String, surplus: bool },
    #[error("systemone choice probability for answer {id:?} option {option:?} is not finite")]
    ChoiceProbabilityNotFinite { id: String, option: String },
    #[error("systemone choice probability for answer {id:?} option {option:?} is outside [0, 1]")]
    ChoiceProbabilityOutOfRange { id: String, option: String, value: f64 },
    #[error("systemone choice probabilities for answer {id:?} sum to {sum}, not one")]
    ChoiceProbabilitySumOutOfRange { id: String, sum: f64 },
    #[error(
        "systemone choice answer {id:?} selected {choice:?}, which is not a maximum-probability option"
    )]
    ChoiceNotMaxProbability { id: String, choice: String },
    #[error("systemone score probabilities for answer {id:?} do not match legend level {level:?}")]
    ScoreLevelMismatch { id: String, level: String, surplus: bool },
    #[error(
        "systemone score answer {id:?} legend level {level:?} is not the ordinal key of the requested scale"
    )]
    ScoreLegendOrdinalMismatch { id: String, level: String, surplus: bool },
    #[error("systemone score answer {id:?} carries no legend levels")]
    EmptyScoreLevels { id: String },
    #[error("systemone score answer {id:?} carries an empty legend level key")]
    EmptyLegendKey { id: String },
    #[error("systemone score probability for answer {id:?} level {level:?} is not finite")]
    ScoreProbabilityNotFinite { id: String, level: String },
    #[error("systemone score probability for answer {id:?} level {level:?} is outside [0, 1]")]
    ScoreProbabilityOutOfRange { id: String, level: String, value: f64 },
    #[error("systemone score probabilities for answer {id:?} sum to {sum}, not one")]
    ScoreProbabilitySumOutOfRange { id: String, sum: f64 },
    #[error("systemone score answer {id:?} value {value} is outside [0, {max_level}]")]
    ScoreOutOfRange { id: String, value: f64, max_level: usize },
    #[error("systemone score answer {id:?} is not finite")]
    ScoreNotFinite { id: String },
    #[error(
        "systemone score answer {id:?} score {score} does not match the probability-weighted expectation {expected}"
    )]
    ScoreExpectationMismatch { id: String, score: f64, expected: f64 },
}

/// Validates a decision request before it is serialised and sent.
///
/// # Errors
/// Returns the first structural violation of the SystemOne request contract.
pub(super) fn validate_request(
    request: &SystemOneDecisionRequest,
) -> Result<(), SystemOneRequestError> {
    if request.model.is_empty() {
        return Err(SystemOneRequestError::EmptyModel);
    }
    if request.state.is_empty() {
        return Err(SystemOneRequestError::EmptyState);
    }
    if request.questions.is_empty() {
        return Err(SystemOneRequestError::EmptyQuestions);
    }
    if request.questions.len() > MAX_QUESTIONS {
        return Err(SystemOneRequestError::TooManyQuestions {
            count: request.questions.len(),
            max: MAX_QUESTIONS,
        });
    }
    for (id, question) in &request.questions {
        validate_question(id, question)?;
    }
    Ok(())
}

fn validate_question(id: &str, question: &SystemOneQuestion) -> Result<(), SystemOneRequestError> {
    if id.is_empty() || id.chars().count() > MAX_QUESTION_ID_CHARS {
        return Err(SystemOneRequestError::QuestionIdInvalid { id: id.to_string() });
    }
    match question {
        SystemOneQuestion::Noul { .. } => Ok(()),
        SystemOneQuestion::Choice { criteria, .. } if criteria.is_empty() => {
            Err(SystemOneRequestError::ChoiceCriteriaEmpty { id: id.to_string() })
        }
        SystemOneQuestion::Choice { criteria, .. } if criteria.len() > MAX_CHOICE_OPTIONS => {
            Err(SystemOneRequestError::TooManyChoiceOptions {
                id: id.to_string(),
                count: criteria.len(),
            })
        }
        SystemOneQuestion::Score { criteria, .. }
            if !(MIN_SCORE_LEVELS..=MAX_SCORE_LEVELS).contains(&criteria.len()) =>
        {
            Err(SystemOneRequestError::ScoreCriteriaOutOfRange {
                id: id.to_string(),
                count: criteria.len(),
            })
        }
        SystemOneQuestion::Choice { .. } | SystemOneQuestion::Score { .. } => Ok(()),
    }
}

/// Validates a decoded response against the request it answers.
///
/// # Errors
/// Returns the first violation of the SystemOne response contract.
pub(super) fn validate_response(
    request: &SystemOneDecisionRequest,
    response: &SystemOneDecisionResponse,
) -> Result<(), SystemOneResponseError> {
    for id in response.answers.keys() {
        if !request.questions.contains_key(id) {
            return Err(SystemOneResponseError::SurplusAnswer { id: id.clone() });
        }
    }
    for (id, question) in &request.questions {
        let Some(answer) = response.answers.get(id) else {
            return Err(SystemOneResponseError::MissingAnswer { id: id.clone() });
        };
        validate_answer(id, question, answer)?;
    }
    Ok(())
}

fn validate_answer(
    id: &str,
    question: &SystemOneQuestion,
    answer: &SystemOneAnswer,
) -> Result<(), SystemOneResponseError> {
    match (question, answer) {
        (SystemOneQuestion::Noul { .. }, SystemOneAnswer::Noul { noul }) => {
            if !noul.is_finite() {
                return Err(SystemOneResponseError::NoulNotFinite { id: id.to_string() });
            }
            if !(0.0..=1.0).contains(noul) {
                return Err(SystemOneResponseError::NoulOutOfRange {
                    id: id.to_string(),
                    value: *noul,
                });
            }
        }
        (
            SystemOneQuestion::Choice { criteria, .. },
            SystemOneAnswer::Choice { choice, probabilities, confidence },
        ) => validate_choice(id, criteria, choice, probabilities, *confidence)?,
        (
            SystemOneQuestion::Score { criteria, .. },
            SystemOneAnswer::Score { score, legend, probabilities, confidence },
        ) => validate_score(id, criteria.len(), *score, legend, probabilities, *confidence)?,
        _ => {
            return Err(SystemOneResponseError::AnswerTypeMismatch {
                id: id.to_string(),
                expected: question_kind(question),
                actual: answer_kind(answer),
            });
        }
    }
    Ok(())
}

/// Validates one `choice` answer: option coverage, finite probabilities in
/// `[0, 1]` summing to one, and that the selected option is a maximum-probability
/// one.
fn validate_choice(
    id: &str,
    criteria: &BTreeMap<String, Option<String>>,
    choice: &str,
    probabilities: &BTreeMap<String, f64>,
    confidence: f64,
) -> Result<(), SystemOneResponseError> {
    validate_confidence(id, confidence)?;
    if let Some((option, surplus)) = first_key_set_mismatch(criteria, probabilities) {
        return Err(SystemOneResponseError::ChoiceProbabilityOptionMismatch {
            id: id.to_string(),
            option: option.clone(),
            surplus,
        });
    }
    let mut sum = 0.0;
    for (option, probability) in probabilities {
        match check_probability(*probability) {
            Ok(()) => {}
            Err(ProbabilityViolation::NotFinite) => {
                return Err(SystemOneResponseError::ChoiceProbabilityNotFinite {
                    id: id.to_string(),
                    option: option.clone(),
                });
            }
            Err(ProbabilityViolation::OutOfRange(value)) => {
                return Err(SystemOneResponseError::ChoiceProbabilityOutOfRange {
                    id: id.to_string(),
                    option: option.clone(),
                    value,
                });
            }
        }
        sum += probability;
    }
    if (sum - 1.0).abs() > probability_sum_tolerance(probabilities.len()) {
        return Err(SystemOneResponseError::ChoiceProbabilitySumOutOfRange {
            id: id.to_string(),
            sum,
        });
    }
    let maximum = probabilities.values().copied().fold(f64::NEG_INFINITY, f64::max);
    match probabilities.get(choice) {
        Some(probability) if *probability >= maximum => Ok(()),
        _ => Err(SystemOneResponseError::ChoiceNotMaxProbability {
            id: id.to_string(),
            choice: choice.to_string(),
        }),
    }
}

/// Validates one `score` answer: a legend whose keys are exactly the ordinal
/// strings `"0"`..`level_count - 1`, a finite probability in `[0, 1]` for every
/// level summing to one, a finite confidence, a finite score inside
/// `[0, level_count - 1]`, and a score equal to the probability-weighted
/// expectation within the published rounding tolerance.
///
/// The protocol defines the score as a probability-weighted position on the
/// level number line (<https://docs.typesafe.ai/primitives/score.md>), so the
/// range check alone is insufficient: the score must agree with
/// `Σ index * probabilities[index]`, allowing for the published rounding, and
/// the probabilities must sum to one exactly as the Choice distribution does.
fn validate_score(
    id: &str,
    level_count: usize,
    score: f64,
    legend: &BTreeMap<String, String>,
    probabilities: &BTreeMap<String, f64>,
    confidence: f64,
) -> Result<(), SystemOneResponseError> {
    validate_score_legend(id, level_count, legend, probabilities)?;
    let mut sum = 0.0;
    let mut expectation = 0.0;
    for (level, probability) in probabilities {
        match check_probability(*probability) {
            Ok(()) => {}
            Err(ProbabilityViolation::NotFinite) => {
                return Err(SystemOneResponseError::ScoreProbabilityNotFinite {
                    id: id.to_string(),
                    level: level.clone(),
                });
            }
            Err(ProbabilityViolation::OutOfRange(value)) => {
                return Err(SystemOneResponseError::ScoreProbabilityOutOfRange {
                    id: id.to_string(),
                    level: level.clone(),
                    value,
                });
            }
        }
        // The legend check establishes canonical ordinal keys for every
        // probability; retain an error here instead of unwrapping the parse.
        let Ok(index) = level.parse::<usize>() else {
            return Err(SystemOneResponseError::ScoreLegendOrdinalMismatch {
                id: id.to_string(),
                level: level.clone(),
                surplus: true,
            });
        };
        sum += probability;
        expectation += index as f64 * probability;
    }
    if (sum - 1.0).abs() > probability_sum_tolerance(level_count) {
        return Err(SystemOneResponseError::ScoreProbabilitySumOutOfRange {
            id: id.to_string(),
            sum,
        });
    }
    validate_confidence(id, confidence)?;
    if !score.is_finite() {
        return Err(SystemOneResponseError::ScoreNotFinite { id: id.to_string() });
    }
    let max_level = level_count.saturating_sub(1);
    if !(0.0..=max_level as f64).contains(&score) {
        return Err(SystemOneResponseError::ScoreOutOfRange {
            id: id.to_string(),
            value: score,
            max_level,
        });
    }
    if (score - expectation).abs() > score_expectation_tolerance(level_count) {
        return Err(SystemOneResponseError::ScoreExpectationMismatch {
            id: id.to_string(),
            score,
            expected: expectation,
        });
    }
    Ok(())
}

fn validate_score_legend(
    id: &str,
    level_count: usize,
    legend: &BTreeMap<String, String>,
    probabilities: &BTreeMap<String, f64>,
) -> Result<(), SystemOneResponseError> {
    if legend.is_empty() {
        return Err(SystemOneResponseError::EmptyScoreLevels { id: id.to_string() });
    }
    if legend.keys().any(|level| level.is_empty()) {
        return Err(SystemOneResponseError::EmptyLegendKey { id: id.to_string() });
    }
    // The legend must name exactly the requested levels with the canonical
    // ordinal keys; a missing requested key is reported first, an extra or
    // non-canonical key second.
    for index in 0..level_count {
        let level = index.to_string();
        if !legend.contains_key(&level) {
            return Err(SystemOneResponseError::ScoreLegendOrdinalMismatch {
                id: id.to_string(),
                level,
                surplus: false,
            });
        }
    }
    for level in legend.keys() {
        if !is_ordinal_level_key(level, level_count) {
            return Err(SystemOneResponseError::ScoreLegendOrdinalMismatch {
                id: id.to_string(),
                level: level.clone(),
                surplus: true,
            });
        }
    }
    if let Some((level, surplus)) = first_key_set_mismatch(legend, probabilities) {
        return Err(SystemOneResponseError::ScoreLevelMismatch {
            id: id.to_string(),
            level: level.clone(),
            surplus,
        });
    }
    Ok(())
}

/// Returns the first key present in `left` but absent from `right` (with
/// `surplus = false`), otherwise the first key present in `right` but absent
/// from `left` (with `surplus = true`).
///
/// The ordered key-set equality checks for `choice` options and `score` levels
/// differ only in the error variant they construct, so the rule lives here once.
/// Scanning `left` before `right` preserves the original reporting order.
fn first_key_set_mismatch<'a, L, R>(
    left: &'a BTreeMap<String, L>,
    right: &'a BTreeMap<String, R>,
) -> Option<(&'a String, bool)> {
    if let Some(key) = left.keys().find(|key| !right.contains_key(*key)) {
        return Some((key, false));
    }
    right.keys().find(|key| !left.contains_key(*key)).map(|key| (key, true))
}

/// True when `level` is the canonical ordinal key `"0"`..`count - 1`: it parses
/// as an in-range index and its own decimal rendering is that key, so `"00"` or
/// `" 1"` do not pass.
fn is_ordinal_level_key(level: &str, count: usize) -> bool {
    matches!(level.parse::<usize>(), Ok(index) if index < count && index.to_string() == level)
}

/// Why one distribution element is not a probability.
enum ProbabilityViolation {
    NotFinite,
    OutOfRange(f64),
}

/// Shared finite-and-unit-interval check for one probability element.
///
/// `choice` and `score` both model a categorical distribution whose every
/// element must be finite and lie in `[0, 1]`; only the element's label and the
/// concrete error variant differ, so the rule lives here once. A sum of one is
/// not sufficient: `(-0.5, 1.5)` sums to one while neither element is a
/// probability.
fn check_probability(probability: f64) -> Result<(), ProbabilityViolation> {
    if !probability.is_finite() {
        return Err(ProbabilityViolation::NotFinite);
    }
    if !(0.0..=1.0).contains(&probability) {
        return Err(ProbabilityViolation::OutOfRange(probability));
    }
    Ok(())
}

fn validate_confidence(id: &str, confidence: f64) -> Result<(), SystemOneResponseError> {
    if !confidence.is_finite() {
        return Err(SystemOneResponseError::ConfidenceNotFinite { id: id.to_string() });
    }
    if !(0.0..=1.0).contains(&confidence) {
        return Err(SystemOneResponseError::ConfidenceOutOfRange {
            id: id.to_string(),
            value: confidence,
        });
    }
    Ok(())
}

const fn question_kind(question: &SystemOneQuestion) -> &'static str {
    match question {
        SystemOneQuestion::Noul { .. } => "noul",
        SystemOneQuestion::Choice { .. } => "choice",
        SystemOneQuestion::Score { .. } => "score",
    }
}

const fn answer_kind(answer: &SystemOneAnswer) -> &'static str {
    match answer {
        SystemOneAnswer::Noul { .. } => "noul",
        SystemOneAnswer::Choice { .. } => "choice",
        SystemOneAnswer::Score { .. } => "score",
    }
}
