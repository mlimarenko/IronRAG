//! Durable capture of the exact LLM request sent on each answer
//! iteration, used by the assistant's debug panel to show the user
//! what actually reached the provider.
//!
//! The grounded-answer path hands a `Vec<ChatMessage>` to the LLM for
//! the initial fixed-evidence answer and, when needed, a literal-
//! fidelity revision over the same evidence. This module lets the
//! operator inspect those exact wire payloads after the fact.
//!
//! Storage is authoritative Postgres state keyed by `execution_id`, so
//! completed turns, cached answer replays, and backend restarts all
//! expose the same provider payload to the UI.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::integrations::llm::ChatMessage;

/// Snapshot of a single answer iteration: the messages vector handed
/// to the provider, the provider's raw response text, optional raw
/// tool-call metadata for external compatibility, and the usage block
/// if the provider returned one.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LlmIterationDebug {
    pub iteration: usize,
    pub provider_kind: String,
    pub model_name: String,
    pub request_messages: Vec<ChatMessage>,
    pub response_text: Option<String>,
    pub response_tool_calls: Vec<ResponseToolCallDebug>,
    pub usage: serde_json::Value,
    /// Wall-clock the model spent on this iteration — request sent to
    /// response received ("model think time"). `None` on records written
    /// before timing capture existed.
    pub duration_ms: Option<u64>,
    /// Runtime execution IDs spawned by tool calls in this iteration.
    /// Populated on tool-use turns when a tool such as
    /// `grounded_answer` creates its own query execution and debug
    /// snapshot. Empty on iterations whose tools do not spawn child
    /// query executions and on single-shot grounded-answer turns.
    pub child_runtime_execution_ids: Vec<Uuid>,
    /// Query-execution IDs of the child executions spawned by this
    /// iteration's tool calls. These are the IDs the execution-detail /
    /// llm-context endpoints accept, so the debug inspector can drill
    /// into a child `grounded_answer` execution. (The runtime IDs above
    /// key a different table and are not directly fetchable.)
    pub child_query_execution_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResponseToolCallDebug {
    pub id: String,
    pub name: String,
    /// Effective arguments executed after server-side defaults and
    /// scope guards were applied.
    pub arguments_json: String,
    /// Raw arguments emitted by the model when server-side defaults or
    /// guards changed the executable payload.
    pub requested_arguments_json: Option<String>,
    pub result_text: Option<String>,
    pub result_json: Option<serde_json::Value>,
    pub is_error: bool,
    /// Wall-clock the tool ran ("tool wait time" the model blocked on).
    /// `None` on records written before timing capture existed.
    pub duration_ms: Option<u64>,
}

/// Metadata describing the tool-use loop that produced this snapshot.
/// Present only on turns driven by tool-use execution; absent on
/// single-shot grounded-answer turns.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentLoopMetadata {
    pub iteration_cap: usize,
    pub deadline_ms: u64,
    pub stopped_reason: AgentStopReason,
    pub tool_call_count: usize,
    #[serde(default)]
    pub provider_429_count: u64,
    #[serde(default)]
    pub provider_retry_count: u64,
    #[serde(default)]
    pub degraded_completion_count: u64,
}

/// Reason the tool-use loop stopped iterating.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStopReason {
    FinalAnswer,
    IterationCap,
    Deadline,
    /// The loop forced a final answer because several consecutive iterations
    /// produced no successful *new* tool result (every call errored, was
    /// suppressed as a duplicate, or replayed an earlier result) and no final
    /// answer was emitted. Distinct from `Deadline`/`IterationCap` so the
    /// no-progress early-stop is observable in the turn debug payload.
    NoProgress,
    ToolError,
    ProviderError,
    /// A value written by a newer or retired binary. Historical diagnostics
    /// remain readable without assigning the value live control semantics.
    #[serde(other)]
    Unknown,
}

/// Full debug snapshot for one assistant turn — one execution_id.
/// The UI can render `iterations` as a stacked timeline.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LlmContextSnapshot {
    pub execution_id: Uuid,
    pub library_id: Uuid,
    pub question: String,
    pub iterations: Vec<LlmIterationDebug>,
    pub total_iterations: usize,
    pub final_answer: Option<String>,
    pub captured_at: chrono::DateTime<chrono::Utc>,
    /// Compiled `QueryIR` produced by `QueryCompilerService` before the
    /// answer stage ran. Surfaced to the debug panel as a JSON tree so
    /// operators see act / scope / target_types / literal_constraints /
    /// confidence that actually drove routing and verification. `None`
    /// on records written by older code paths.
    pub query_ir: Option<serde_json::Value>,
    /// Tool-use loop metadata. `None` for single-shot grounded-answer turns;
    /// `Some` when tool-use execution drove this turn.
    pub agent_loop: Option<AgentLoopMetadata>,
    /// Fine-grained timed sub-operations recorded during this execution
    /// (individual DB queries, retrieval lanes, …) so the inspector can
    /// surface where time went and operators can catch heavy sections. Empty
    /// on records written before span capture existed.
    pub spans: Vec<crate::services::query::turn_spans::TurnSpan>,
}

impl LlmContextSnapshot {
    /// Push an iteration onto this snapshot.
    ///
    /// If `iteration.iteration` is `0` the field is auto-set to
    /// `current length + 1` (1-based). A non-zero value is preserved
    /// as-is, letting callers that already track the counter pass it
    /// through without double-incrementing.
    pub fn append_iteration(&mut self, mut iteration: LlmIterationDebug) {
        if iteration.iteration == 0 {
            iteration.iteration = self.iterations.len() + 1;
        }
        self.iterations.push(iteration);
    }
}

pub async fn upsert_snapshot(
    postgres: &PgPool,
    snapshot: &LlmContextSnapshot,
) -> Result<(), sqlx::Error> {
    // The row is replaced whole, so the caller owns every field it carries —
    // including `spans`. Answer paths build the snapshot from
    // `AnswerSnapshotContext`, which reads the retrieval spans by reference and
    // therefore repeats them on every write of the same execution's row.
    let snapshot_json = serde_json::to_value(snapshot).map_err(|error| {
        sqlx::Error::Protocol(format!("failed to serialize LLM context snapshot: {error}"))
    })?;
    sqlx::query(
        "insert into query_llm_context_snapshot (
            execution_id, snapshot_json, captured_at
         ) values ($1, $2, $3)
         on conflict (execution_id) do update
         set snapshot_json = excluded.snapshot_json,
             captured_at = excluded.captured_at",
    )
    .bind(snapshot.execution_id)
    .bind(snapshot_json)
    .bind(snapshot.captured_at)
    .execute(postgres)
    .await?;
    Ok(())
}

pub async fn load_snapshot(
    postgres: &PgPool,
    execution_id: Uuid,
) -> Result<Option<LlmContextSnapshot>, sqlx::Error> {
    let snapshot_json = sqlx::query_scalar::<_, serde_json::Value>(
        "select snapshot_json
         from query_llm_context_snapshot
         where execution_id = $1",
    )
    .bind(execution_id)
    .fetch_optional(postgres)
    .await?;
    snapshot_json
        .map(|value| {
            serde_json::from_value(value).map_err(|error| {
                sqlx::Error::Protocol(format!(
                    "failed to deserialize LLM context snapshot {execution_id}: {error}"
                ))
            })
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::{AgentLoopMetadata, AgentStopReason, LlmContextSnapshot, LlmIterationDebug};
    use chrono::Utc;
    use uuid::Uuid;

    fn fake_snapshot(execution_id: Uuid) -> LlmContextSnapshot {
        LlmContextSnapshot {
            execution_id,
            library_id: Uuid::now_v7(),
            question: "test".into(),
            iterations: Vec::new(),
            total_iterations: 0,
            final_answer: None,
            captured_at: Utc::now(),
            query_ir: None,
            agent_loop: None,
            spans: Vec::new(),
        }
    }

    fn fake_iteration(iteration: usize) -> LlmIterationDebug {
        LlmIterationDebug {
            iteration,
            provider_kind: "test_provider".into(),
            model_name: "test_model".into(),
            request_messages: Vec::new(),
            response_text: None,
            response_tool_calls: Vec::new(),
            usage: serde_json::Value::Null,
            duration_ms: None,
            child_runtime_execution_ids: Vec::new(),
            child_query_execution_ids: Vec::new(),
        }
    }

    #[test]
    fn append_iteration_increments_index() {
        let mut snap = fake_snapshot(Uuid::now_v7());
        snap.append_iteration(fake_iteration(0));
        snap.append_iteration(fake_iteration(0));
        assert_eq!(snap.iterations[0].iteration, 1);
        assert_eq!(snap.iterations[1].iteration, 2);
    }

    #[test]
    fn agent_loop_metadata_roundtrips_serde() {
        let mut snap = fake_snapshot(Uuid::now_v7());
        snap.agent_loop = Some(AgentLoopMetadata {
            iteration_cap: 10,
            deadline_ms: 30_000,
            stopped_reason: AgentStopReason::FinalAnswer,
            tool_call_count: 3,
            provider_429_count: 4,
            provider_retry_count: 4,
            degraded_completion_count: 0,
        });
        let json = serde_json::to_string(&snap).expect("serialize");
        let restored: LlmContextSnapshot = serde_json::from_str(&json).expect("deserialize");
        let meta = restored.agent_loop.expect("agent_loop present");
        assert_eq!(meta.iteration_cap, 10);
        assert_eq!(meta.deadline_ms, 30_000);
        assert_eq!(meta.stopped_reason, AgentStopReason::FinalAnswer);
        assert_eq!(meta.tool_call_count, 3);
        assert_eq!(meta.provider_429_count, 4);
        assert_eq!(meta.provider_retry_count, 4);
        assert_eq!(meta.degraded_completion_count, 0);
    }

    #[test]
    fn agent_stop_reason_serializes_provider_error() {
        assert_eq!(
            serde_json::to_value(AgentStopReason::ProviderError).expect("serialize"),
            serde_json::json!("provider_error")
        );
    }

    #[test]
    fn persisted_snapshot_tolerates_retired_usage_and_stop_reason_values() {
        let mut snapshot = fake_snapshot(Uuid::now_v7());
        let mut iteration = fake_iteration(1);
        iteration.usage = serde_json::json!({
            "deterministic": true,
            "answer_kind": "ordered_source_slice_identity_fallback",
        });
        snapshot.iterations.push(iteration);
        snapshot.total_iterations = 1;
        snapshot.agent_loop = Some(AgentLoopMetadata {
            iteration_cap: 4,
            deadline_ms: 30_000,
            stopped_reason: AgentStopReason::ToolError,
            tool_call_count: 1,
            provider_429_count: 0,
            provider_retry_count: 0,
            degraded_completion_count: 0,
        });
        let mut stored = serde_json::to_value(snapshot).expect("serialize stored snapshot");
        stored["agentLoop"]["stoppedReason"] = serde_json::json!("retired_tool_failure_reason");

        let restored: LlmContextSnapshot =
            serde_json::from_value(stored).expect("load historical snapshot");

        assert_eq!(
            restored.iterations[0].usage["answer_kind"],
            "ordered_source_slice_identity_fallback",
        );
        assert_eq!(
            restored.agent_loop.expect("agent loop metadata").stopped_reason,
            AgentStopReason::Unknown,
        );
    }

    #[test]
    fn snapshot_requires_all_v1_fields_on_deserialization() {
        let json = r#"{
            "executionId": "018f8e4b-0000-7000-8000-000000000001",
            "libraryId":   "018f8e4b-0000-7000-8000-000000000002",
            "question":    "test",
            "iterations":  [{
                "iteration": 1,
                "providerKind": "provider",
                "modelName": "model",
                "requestMessages": [],
                "responseText": null,
                "responseToolCalls": [],
                "usage": null
            }],
            "totalIterations": 1,
            "finalAnswer": null,
            "capturedAt": "2026-05-10T00:00:00Z"
        }"#;

        assert!(serde_json::from_str::<LlmContextSnapshot>(json).is_err());
    }
}
