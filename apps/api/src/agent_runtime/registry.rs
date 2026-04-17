use std::{collections::BTreeMap, sync::Arc};

use crate::{
    agent_runtime::task::{RuntimeTask, RuntimeTaskSpec},
    domains::agent_runtime::{RuntimeExecutionOwnerKind, RuntimeTaskKind},
};

#[derive(Debug, Clone)]
pub struct RuntimeTaskCatalogEntry {
    pub spec: RuntimeTaskSpec,
    pub contract_name: &'static str,
    pub contract_version: &'static str,
}

#[derive(Clone, Default)]
pub struct RuntimeTaskRegistry {
    entries: Arc<BTreeMap<RuntimeTaskKind, RuntimeTaskCatalogEntry>>,
}

impl RuntimeTaskRegistry {
    #[must_use]
    pub fn with_entries(entries: BTreeMap<RuntimeTaskKind, RuntimeTaskCatalogEntry>) -> Self {
        Self { entries: Arc::new(entries) }
    }

    #[must_use]
    pub fn register_task<TTask: RuntimeTask>(&self) -> Self {
        let mut entries = self.entries.as_ref().clone();
        let spec = TTask::spec();
        entries.insert(
            spec.task_kind,
            RuntimeTaskCatalogEntry {
                spec,
                contract_name: TTask::CONTRACT_NAME,
                contract_version: TTask::CONTRACT_VERSION,
            },
        );
        Self::with_entries(entries)
    }

    #[must_use]
    pub fn contains_task_kind(&self, task_kind: RuntimeTaskKind) -> bool {
        self.entries.contains_key(&task_kind)
    }

    #[must_use]
    pub fn spec(&self, task_kind: RuntimeTaskKind) -> Option<&RuntimeTaskSpec> {
        self.entries.get(&task_kind).map(|entry| &entry.spec)
    }

    #[must_use]
    pub fn contract_name(&self, task_kind: RuntimeTaskKind) -> Option<&'static str> {
        self.entries.get(&task_kind).map(|entry| entry.contract_name)
    }

    #[must_use]
    pub fn contract_version(&self, task_kind: RuntimeTaskKind) -> Option<&'static str> {
        self.entries.get(&task_kind).map(|entry| entry.contract_version)
    }

    #[must_use]
    pub fn entries(
        &self,
    ) -> impl ExactSizeIterator<Item = (&RuntimeTaskKind, &RuntimeTaskCatalogEntry)> {
        self.entries.iter()
    }

    #[must_use]
    pub fn dispatch_entry(&self, task_kind: RuntimeTaskKind) -> Option<&RuntimeTaskCatalogEntry> {
        self.entries.get(&task_kind)
    }

    #[must_use]
    #[allow(clippy::missing_const_for_fn)]
    pub fn validate_owner_kind(
        &self,
        task_kind: RuntimeTaskKind,
        owner_kind: RuntimeExecutionOwnerKind,
    ) -> bool {
        match task_kind {
            RuntimeTaskKind::QueryCompile
            | RuntimeTaskKind::QueryPlan
            | RuntimeTaskKind::QueryRerank
            | RuntimeTaskKind::QueryAnswer
            | RuntimeTaskKind::QueryVerify => {
                matches!(owner_kind, RuntimeExecutionOwnerKind::QueryExecution)
            }
            RuntimeTaskKind::GraphExtract => {
                matches!(owner_kind, RuntimeExecutionOwnerKind::GraphExtractionAttempt)
            }
            RuntimeTaskKind::StructuredPrepare => {
                matches!(owner_kind, RuntimeExecutionOwnerKind::StructuredPreparation)
            }
            RuntimeTaskKind::TechnicalFactExtract => {
                matches!(owner_kind, RuntimeExecutionOwnerKind::TechnicalFactExtraction)
            }
        }
    }
}
