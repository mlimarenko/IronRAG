//! Durable capture of the exact LLM request sent on each answer
//! iteration, used by the assistant's debug panel to show the user
//! what actually reached the provider.
//!
//! The grounded-answer path hands a Vec<ChatMessage> to the LLM for
//! the initial fixed-evidence answer and, when needed, a literal-
//! fidelity revision over the same evidence. This module lets the
//! operator inspect those exact wire payloads after the fact.
//!
//! Storage is canonical Postgres state keyed by `execution_id`, so
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Runtime execution IDs spawned by tool calls in this iteration.
    /// Populated on tool-use turns when a tool such as
    /// `grounded_answer` creates its own query execution and debug
    /// snapshot. Empty on iterations whose tools do not spawn child
    /// query executions and on single-shot grounded-answer turns.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_runtime_execution_ids: Vec<Uuid>,
    /// Query-execution IDs of the child executions spawned by this
    /// iteration's tool calls. These are the IDs the execution-detail /
    /// llm-context endpoints accept, so the debug inspector can drill
    /// into a child `grounded_answer` execution. (The runtime IDs above
    /// key a different table and are not directly fetchable.)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_arguments_json: Option<String>,
    pub result_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_json: Option<serde_json::Value>,
    pub is_error: bool,
    /// Wall-clock the tool ran ("tool wait time" the model blocked on).
    /// `None` on records written before timing capture existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
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
}

/// Reason the tool-use loop stopped iterating.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStopReason {
    FinalAnswer,
    IterationCap,
    Deadline,
    ToolError,
    ProviderError,
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
    /// Canonical `QueryIR` produced by `QueryCompilerService` before the
    /// answer stage ran. Surfaced to the debug panel as a JSON tree so
    /// operators see act / scope / target_types / literal_constraints /
    /// confidence that actually drove routing and verification. `None`
    /// on records written by older code paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_ir: Option<serde_json::Value>,
    /// Tool-use loop metadata. `None` for single-shot grounded-answer turns;
    /// `Some` when tool-use execution drove this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_loop: Option<AgentLoopMetadata>,
    /// Fine-grained timed sub-operations recorded during this execution
    /// (individual DB/Arango queries, retrieval lanes, …) so the inspector can
    /// surface where time went and operators can catch heavy sections. Empty
    /// on records written before span capture existed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
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
    let mut snapshot_json = serde_json::to_value(snapshot).map_err(|error| {
        sqlx::Error::Protocol(format!("failed to serialize LLM context snapshot: {error}"))
    })?;
    // Attach the fine-grained timed spans stashed for this execution (DB
    // queries, retrieval lanes) unless the snapshot already carries them.
    if snapshot.spans.is_empty() {
        let spans = crate::services::query::turn_spans::take_execution_spans(snapshot.execution_id);
        if !spans.is_empty() {
            if let Ok(spans_value) = serde_json::to_value(&spans) {
                snapshot_json["spans"] = spans_value;
            }
        }
    }
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
        });
        let json = serde_json::to_string(&snap).expect("serialize");
        let restored: LlmContextSnapshot = serde_json::from_str(&json).expect("deserialize");
        let meta = restored.agent_loop.expect("agent_loop present");
        assert_eq!(meta.iteration_cap, 10);
        assert_eq!(meta.deadline_ms, 30_000);
        assert_eq!(meta.stopped_reason, AgentStopReason::FinalAnswer);
        assert_eq!(meta.tool_call_count, 3);
    }

    #[test]
    fn agent_stop_reason_serializes_provider_error() {
        assert_eq!(
            serde_json::to_value(AgentStopReason::ProviderError).expect("serialize"),
            serde_json::json!("provider_error")
        );
    }

    #[test]
    fn child_runtime_execution_ids_default_empty() {
        // JSON without childRuntimeExecutionIds must deserialize cleanly.
        let json = r#"{
            "executionId": "018f8e4b-0000-7000-8000-000000000001",
            "libraryId":   "018f8e4b-0000-7000-8000-000000000002",
            "question":    "test",
            "iterations":  [{
                "iteration": 1,
                "providerKind": "openai",
                "modelName": "gpt-4",
                "requestMessages": [],
                "responseText": null,
                "responseToolCalls": [],
                "usage": null
            }],
            "totalIterations": 1,
            "finalAnswer": null,
            "capturedAt": "2026-05-10T00:00:00Z"
        }"#;
        let snap: LlmContextSnapshot = serde_json::from_str(json).expect("deserialize");
        assert_eq!(snap.iterations[0].child_runtime_execution_ids.len(), 0);
        assert!(snap.agent_loop.is_none());
    }
}
