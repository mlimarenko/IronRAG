//! Wire types for the TypeSafe SystemOne decision protocol.
//!
//! The definitions follow the authoritative protocol document
//! (<https://docs.typesafe.ai/api.md>) with one deliberate, documented
//! narrowing: every field the protocol admits as `string | object | array` —
//! `state`, `instructions`, and the `criteria` descriptions — is modelled as a
//! `String` here, because no caller in this leaf sends structured state,
//! instructions or criteria. The narrowing lives in the type, so a structured
//! value fails to decode instead of being silently discarded.
//!
//! Request decoding is strict (`deny_unknown_fields`) so an unknown field in a
//! caller payload is a decode error rather than a dropped decision input.
//! Response decoding is deliberately tolerant of additive fields: the server
//! may add response metadata without breaking a deployed client, and the
//! validated shape is enforced by [`super::validation::validate_response`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Canonical SystemOne decision request body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemOneDecisionRequest {
    /// The state to reason about. TEXT-ONLY in this leaf: the protocol also
    /// admits objects and arrays, but no caller in this story needs them.
    pub state: String,
    /// Caller-supplied model identifier. The server echoes it and may resolve
    /// an alias to a different concrete version, so it is never validated
    /// against the response.
    pub model: String,
    /// Questions keyed by caller-chosen id. The response must answer exactly
    /// this key set.
    pub questions: BTreeMap<String, SystemOneQuestion>,
}

/// One question in a SystemOne decision request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum SystemOneQuestion {
    /// A boolean decision with optional per-outcome criteria.
    Noul {
        /// Instructions for the model: a prose string, or a free-form JSON
        /// object or array.
        instructions: String,
        /// Descriptions of the `true` / `false` outcomes. Both properties are
        /// individually optional.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<SystemOneNoulCriteria>,
    },
    /// A single-choice decision over 1..=255 options.
    Choice {
        instructions: String,
        /// Option name to criterion. `None` serialises as JSON `null`.
        criteria: BTreeMap<String, Option<String>>,
    },
    /// A graded decision over 2..=10 ordered levels.
    Score {
        instructions: String,
        /// Ordered level descriptions from lowest to highest.
        criteria: Vec<String>,
    },
}

/// Per-outcome criteria of a `noul` question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemOneNoulCriteria {
    /// Criterion the `true` outcome must satisfy.
    #[serde(rename = "true", default, skip_serializing_if = "Option::is_none")]
    pub r#true: Option<String>,
    /// Criterion the `false` outcome must satisfy.
    #[serde(rename = "false", default, skip_serializing_if = "Option::is_none")]
    pub r#false: Option<String>,
}

/// Top-level SystemOne decision response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SystemOneDecisionResponse {
    /// Model the server reports, which may be the resolved alias target. Never
    /// compared with the requested model.
    pub model: String,
    /// Answers keyed by the same ids that were sent as questions.
    pub answers: BTreeMap<String, SystemOneAnswer>,
    pub usage: SystemOneUsage,
}

/// One answer in a SystemOne decision response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemOneAnswer {
    Noul {
        /// Probability of the `true` outcome, in `[0, 1]`.
        noul: f64,
    },
    Choice {
        /// The selected option; must be one of the question's criteria keys.
        choice: String,
        /// Option to probability, keyed exactly like the question criteria.
        probabilities: BTreeMap<String, f64>,
        /// Confidence in the selection, in `[0, 1]`.
        confidence: f64,
    },
    Score {
        /// The probability-weighted position on the level number line,
        /// `Σ index * probabilities[index]`. Reported rounded to the published
        /// two-decimal precision, so it is compared within a bounded epsilon
        /// rather than for exact float equality.
        score: f64,
        /// Ordinal level index (`"0"`..`"N-1"`) to human-readable legend text.
        legend: BTreeMap<String, String>,
        /// Ordinal level index to probability, keyed exactly like `legend`.
        probabilities: BTreeMap<String, f64>,
        /// Confidence in the selection, in `[0, 1]`.
        confidence: f64,
    },
}

/// Token accounting reported by the SystemOne endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemOneUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}
