//! Canonical, strictly-typed client for the TypeSafe SystemOne decision
//! protocol (<https://docs.typesafe.ai/api.md>).
//!
//! The client owns only the SystemOne wire contract and its validation. It
//! reuses the shared provider machinery — transport, retry, redaction and
//! budget — instead of opening a second HTTP path, and it never guesses a
//! provider kind or a model name: both are caller-supplied.

mod client;
mod protocol;
mod validation;

pub use client::{
    SystemOneBudget, SystemOneClient, SystemOneClientBuildError, SystemOneClientConfig,
    SystemOneTransportParams,
};
pub use protocol::{
    SystemOneAnswer, SystemOneDecisionRequest, SystemOneDecisionResponse, SystemOneNoulCriteria,
    SystemOneQuestion, SystemOneUsage,
};
pub use validation::{
    MAX_QUESTION_ID_CHARS, MAX_QUESTIONS, MAX_SCORE_LEVELS, MIN_SCORE_LEVELS, PUBLISHED_QUANTUM,
    SystemOneConfigError, SystemOneError, SystemOneRequestError, SystemOneResponseError,
    probability_sum_tolerance, score_expectation_tolerance,
};

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test fixtures require descriptive failures"
)]
mod client_tests;
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "test assertions require descriptive failures"
)]
mod validation_tests;
#[cfg(test)]
pub(crate) use test_server::{MockResponse, MockServer};
#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    reason = "the mock server panics on a failed bind, clock or request-log lock"
)]
mod test_server;
