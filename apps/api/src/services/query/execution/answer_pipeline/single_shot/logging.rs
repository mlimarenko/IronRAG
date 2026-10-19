use uuid::Uuid;

use super::super::coverage::EvidenceCoverage;
pub(super) fn log_single_shot_done(execution_id: Uuid, answer_len: usize, elapsed_ms: u128) {
    tracing::info!(
        stage = "answer.single_shot_done",
        %execution_id,
        answer_len,
        elapsed_ms,
        "single-shot grounded-answer fast path done"
    );
}

pub(super) fn log_single_shot_accepted(
    execution_id: Uuid,
    verify_elapsed_ms: u128,
    total_elapsed_ms: u128,
) {
    tracing::info!(
        stage = "answer.single_shot_accepted",
        %execution_id,
        verify_elapsed_ms,
        total_elapsed_ms,
        "single-shot grounded-answer accepted"
    );
}

pub(super) fn log_single_shot_rejected(execution_id: Uuid) {
    tracing::info!(
        stage = "answer.single_shot_rejected",
        %execution_id,
        "single-shot answer unacceptable — escalating to evidence preflight over the same retrieved evidence"
    );
}

pub(super) fn log_preflight_single_shot_canonical_attempt(
    execution_id: Uuid,
    coverage: &EvidenceCoverage,
) {
    tracing::info!(
        stage = "answer.preflight_single_shot_\u{63}anonical_attempt",
        %execution_id,
        ?coverage,
        "evidence preflight answer will stay on fixed retrieved evidence despite incomplete structural coverage"
    );
}

pub(super) fn log_preflight_single_shot_start(
    execution_id: Uuid,
    library_id: Uuid,
    primary_chunks: usize,
    prompt_context_chars: usize,
) {
    tracing::info!(
        stage = "answer.preflight_single_shot_start",
        %execution_id,
        %library_id,
        primary_chunks,
        prompt_context_chars,
        "evidence preflight single-shot answer start"
    );
}

pub(super) fn log_preflight_single_shot_error<E: std::fmt::Debug>(execution_id: Uuid, error: E) {
    tracing::warn!(
        stage = "answer.preflight_single_shot_error",
        %execution_id,
        ?error,
        "evidence preflight single-shot answer failed"
    );
}

pub(super) fn log_preflight_single_shot_done(
    execution_id: Uuid,
    answer_len: usize,
    elapsed_ms: u128,
) {
    tracing::info!(
        stage = "answer.preflight_single_shot_done",
        %execution_id,
        answer_len,
        elapsed_ms,
        "evidence preflight single-shot answer done"
    );
}

pub(super) fn log_preflight_single_shot_accepted(
    execution_id: Uuid,
    verify_state: crate::domains::query::QueryVerificationState,
    total_elapsed_ms: u128,
) {
    tracing::info!(
        stage = "answer.preflight_single_shot_accepted",
        %execution_id,
        verify_state = ?verify_state,
        total_elapsed_ms,
        "evidence preflight single-shot answer accepted"
    );
}

pub(super) fn log_preflight_single_shot_rejected(
    execution_id: Uuid,
    verify_state: crate::domains::query::QueryVerificationState,
    warning_count: usize,
) {
    tracing::info!(
        stage = "answer.preflight_single_shot_rejected",
        %execution_id,
        verify_state = ?verify_state,
        warning_count,
        "evidence preflight single-shot answer has verifier warnings — returning fixed-evidence result without re-retrieval"
    );
}
