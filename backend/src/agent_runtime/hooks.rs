use async_trait::async_trait;

use crate::domains::agent_runtime::{
    RuntimeActionKind, RuntimeDecisionTargetKind, RuntimeStageKind,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeHookContext {
    pub execution_id: uuid::Uuid,
    pub stage_kind: Option<RuntimeStageKind>,
    pub action_kind: Option<RuntimeActionKind>,
    pub target_kind: RuntimeDecisionTargetKind,
}

#[async_trait]
pub trait RuntimeHooks: Send + Sync {
    async fn before_target(&self, _context: &RuntimeHookContext) {}

    async fn after_target(&self, _context: &RuntimeHookContext) {}

    async fn before_model_request(&self, _context: &RuntimeHookContext) {}

    async fn after_model_request(&self, _context: &RuntimeHookContext) {}

    async fn before_tool_request(&self, _context: &RuntimeHookContext) {}

    async fn after_tool_request(&self, _context: &RuntimeHookContext) {}

    async fn before_tool_result(&self, _context: &RuntimeHookContext) {}

    async fn after_tool_result(&self, _context: &RuntimeHookContext) {}

    async fn before_stage_transition(&self, _context: &RuntimeHookContext) {}

    async fn after_stage_transition(&self, _context: &RuntimeHookContext) {}

    async fn before_final_outcome(&self, _context: &RuntimeHookContext) {}

    async fn after_final_outcome(&self, _context: &RuntimeHookContext) {}
}

pub struct NoopRuntimeHooks;

#[async_trait]
impl RuntimeHooks for NoopRuntimeHooks {}
