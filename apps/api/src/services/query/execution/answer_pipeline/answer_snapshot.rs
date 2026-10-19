//! The debug-inspector snapshot row for one answer execution.
//!
//! `query_llm_context_snapshot` is keyed by `execution_id` and each write
//! replaces the whole row, so anything only one write knows is lost by the
//! next. Several answer paths legitimately write the row more than once — a
//! candidate answer followed by the verified one — which makes "carry it in a
//! place the second write can no longer read" a defect, not a style choice.
//!
//! [`AnswerSnapshotContext`] owns the per-execution facts that do not change
//! between those writes (question, compiled IR, retrieval spans) and hands out
//! a complete [`LlmContextSnapshot`] through `&self`. Every write of the row
//! therefore carries the same durable fields, however many times it runs.

use super::{PreparedAnswerQueryResult, Uuid};
use crate::services::query::llm_context_debug::{LlmContextSnapshot, LlmIterationDebug};
use crate::services::query::turn_spans::TurnSpan;

pub(super) struct AnswerSnapshotContext {
    execution_id: Uuid,
    library_id: Uuid,
    question: String,
    query_ir: serde_json::Value,
    spans: Vec<TurnSpan>,
}

impl AnswerSnapshotContext {
    pub(super) fn new(
        execution_id: Uuid,
        library_id: Uuid,
        question: &str,
        query_ir: serde_json::Value,
        prepared: &PreparedAnswerQueryResult,
    ) -> Self {
        Self {
            execution_id,
            library_id,
            question: question.to_string(),
            query_ir,
            spans: prepared.retrieval_spans.clone(),
        }
    }

    pub(super) fn execution_id(&self) -> Uuid {
        self.execution_id
    }

    pub(super) fn library_id(&self) -> Uuid {
        self.library_id
    }

    /// Snapshot for an answer produced without provider iterations
    /// (deterministic, clarification, preflight override).
    pub(super) fn snapshot(&self, final_answer: &str) -> LlmContextSnapshot {
        self.snapshot_with_iterations(final_answer, Vec::new(), 0)
    }

    /// Snapshot for a provider-backed answer, carrying the exact wire payloads
    /// of the iterations that produced it.
    pub(super) fn snapshot_with_iterations(
        &self,
        final_answer: &str,
        iterations: Vec<LlmIterationDebug>,
        total_iterations: usize,
    ) -> LlmContextSnapshot {
        LlmContextSnapshot {
            execution_id: self.execution_id,
            library_id: self.library_id,
            question: self.question.clone(),
            iterations,
            total_iterations,
            final_answer: Some(final_answer.to_string()),
            captured_at: chrono::Utc::now(),
            query_ir: Some(self.query_ir.clone()),
            agent_loop: None,
            // Read by reference: a later write of this same row repeats the
            // spans instead of replacing them with an empty list.
            spans: self.spans.clone(),
        }
    }
}
