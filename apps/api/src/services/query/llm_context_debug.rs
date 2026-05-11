//! In-memory capture of the exact LLM request sent on each answer
//! iteration, used by the assistant's debug panel to show the user
//! what actually reached the provider.
//!
//! The grounded-answer path hands a Vec<ChatMessage> to the LLM for
//! the initial fixed-evidence answer and, when needed, a literal-
//! fidelity revision over the same evidence. This module lets the
//! operator inspect those exact wire payloads after the fact.
//!
//! Storage is a bounded FIFO cache keyed by `execution_id`. Volatile
//! on purpose: debug snapshots are large (kilobytes to a few hundred
//! kilobytes per turn) and have zero value past the current debug
//! session. A worker restart clears the cache; there is no schema
//! migration and no disk footprint.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
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
    /// Runtime execution IDs spawned by tool calls in this iteration.
    /// Populated when a tool call (e.g. `grounded_answer`) recursed into
    /// `execute_turn` and produced its own `LlmContextSnapshot`. Empty
    /// for all single-shot grounded-answer iterations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub child_runtime_execution_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResponseToolCallDebug {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
    pub result_text: Option<String>,
    pub is_error: bool,
}

/// Metadata describing the agent tool-loop that produced this snapshot.
/// Present only on turns driven by the MCP-agent loop; absent on
/// single-shot grounded-answer turns.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AgentLoopMetadata {
    pub iteration_cap: usize,
    pub deadline_ms: u64,
    pub stopped_reason: AgentStopReason,
    pub tool_call_count: usize,
}

/// Reason the agent tool-loop stopped iterating.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentStopReason {
    FinalAnswer,
    IterationCap,
    Deadline,
    ToolError,
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
    /// Agent tool-loop metadata. `None` for single-shot grounded-answer
    /// turns; `Some` when an MCP-agent loop drove this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_loop: Option<AgentLoopMetadata>,
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

/// Bounded FIFO cache of recent snapshots, keyed by `execution_id`.
///
/// Capacity is a hard upper bound; oldest entries are dropped when
/// inserting past the limit. 100 turns × up to ~200 kB per snapshot =
/// ≤ 20 MB peak — fine for an always-on operator tool, never persisted.
#[derive(Clone)]
pub struct LlmContextDebugStore {
    inner: Arc<Mutex<VecDeque<(Uuid, LlmContextSnapshot)>>>,
    capacity: usize,
}

impl Default for LlmContextDebugStore {
    fn default() -> Self {
        Self::with_capacity(100)
    }
}

impl LlmContextDebugStore {
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity.max(1)))),
            capacity: capacity.max(1),
        }
    }

    pub fn insert(&self, snapshot: LlmContextSnapshot) {
        let mut guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        // Evict any prior record for the same execution so repeated
        // writes (streamed vs blocking path) don't duplicate and so
        // the FIFO stays tight.
        guard.retain(|(id, _)| *id != snapshot.execution_id);
        if guard.len() >= self.capacity {
            guard.pop_front();
        }
        guard.push_back((snapshot.execution_id, snapshot));
    }

    #[must_use]
    pub fn get(&self, execution_id: Uuid) -> Option<LlmContextSnapshot> {
        let guard = match self.inner.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.iter().find(|(id, _)| *id == execution_id).map(|(_, snapshot)| snapshot.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentLoopMetadata, AgentStopReason, LlmContextDebugStore, LlmContextSnapshot,
        LlmIterationDebug,
    };
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
            child_runtime_execution_ids: Vec::new(),
        }
    }

    #[test]
    fn insert_and_get_roundtrips() {
        let store = LlmContextDebugStore::with_capacity(4);
        let id = Uuid::now_v7();
        store.insert(fake_snapshot(id));
        assert!(store.get(id).is_some());
    }

    #[test]
    fn capacity_evicts_oldest_fifo() {
        let store = LlmContextDebugStore::with_capacity(2);
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let c = Uuid::now_v7();
        store.insert(fake_snapshot(a));
        store.insert(fake_snapshot(b));
        store.insert(fake_snapshot(c));
        assert!(store.get(a).is_none());
        assert!(store.get(b).is_some());
        assert!(store.get(c).is_some());
    }

    #[test]
    fn reinsert_same_execution_replaces_prior_record() {
        let store = LlmContextDebugStore::with_capacity(4);
        let id = Uuid::now_v7();
        let mut first = fake_snapshot(id);
        first.question = "first".into();
        store.insert(first);
        let mut second = fake_snapshot(id);
        second.question = "second".into();
        store.insert(second);
        let fetched = store.get(id);
        assert_eq!(fetched.map(|s| s.question), Some("second".to_string()));
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
