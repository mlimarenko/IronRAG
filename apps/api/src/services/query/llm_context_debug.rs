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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmIterationDebug {
    pub iteration: usize,
    pub provider_kind: String,
    pub model_name: String,
    pub request_messages: Vec<ChatMessage>,
    pub response_text: Option<String>,
    pub response_tool_calls: Vec<ResponseToolCallDebug>,
    pub usage: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseToolCallDebug {
    pub id: String,
    pub name: String,
    pub arguments_json: String,
    pub result_text: Option<String>,
    pub is_error: bool,
}

/// Full debug snapshot for one assistant turn — one execution_id.
/// The UI can render `iterations` as a stacked timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    use super::{LlmContextDebugStore, LlmContextSnapshot};
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
}
