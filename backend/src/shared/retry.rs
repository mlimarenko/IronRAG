#[must_use]
pub fn is_retryable_ingestion_state(state: &str) -> bool {
    matches!(state, "partial" | "retryable_failed")
}
