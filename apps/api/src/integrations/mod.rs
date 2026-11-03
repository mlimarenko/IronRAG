use std::time::Duration;

pub mod extraction_protocol;
pub mod llm;
pub mod provider_budget;
pub mod retry;
pub mod typesafe;
pub mod xberg;

/// Milliseconds for a tracing field, saturating instead of wrapping.
pub(crate) fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
